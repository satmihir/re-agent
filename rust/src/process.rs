use std::io::{self, PipeReader, PipeWriter};
use std::process::{Child, Command};
use std::sync::{Mutex, MutexGuard};

// Darwin has no pipe2: a pipe becomes close-on-exec only after it exists, so a process
// started by another thread in that window inherits it and holds the first command's
// output open. Like Go's syscall.ForkLock, one lock covers pipe creation and every start.
static SPAWN: Mutex<()> = Mutex::new(());

/// SpawnGuard keeps other threads from creating pipes or starting processes.
pub(crate) struct SpawnGuard {
    _lock: MutexGuard<'static, ()>,
}

/// lock waits until no other thread is creating a pipe or starting a process.
pub(crate) fn lock() -> SpawnGuard {
    // The guarded data is empty, so a panic while holding the lock leaves nothing to repair.
    SpawnGuard {
        _lock: SPAWN
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
    }
}

impl SpawnGuard {
    /// pipe returns a close-on-exec pipe for a child started under this guard.
    pub(crate) fn pipe(&self) -> io::Result<(PipeReader, PipeWriter)> {
        io::pipe()
    }

    /// spawn starts a command whose piped standard streams are created under this guard.
    pub(crate) fn spawn(&self, command: &mut Command) -> io::Result<Child> {
        command.spawn()
    }
}

/// spawn starts a command without racing another thread's pipe creation.
pub(crate) fn spawn(command: &mut Command) -> io::Result<Child> {
    lock().spawn(command)
}

/// Streams chooses whether stderr is discarded, separate, or merged into stdout.
#[derive(Clone, Copy)]
pub(crate) enum Streams {
    Stdout,
    Separate,
    Merged,
}

/// CaptureDeadline counts either the whole operation or only time after starting.
pub(crate) enum CaptureDeadline {
    At(std::time::Instant),
    AfterSpawn(std::time::Duration),
}

/// CaptureReadError preserves how callers treat errors other than interrupted or pending reads.
#[derive(Clone, Copy)]
pub(crate) enum CaptureReadError {
    Incomplete,
    Retry,
    Eof,
}

/// CaptureOptions preserves each caller's output and process-lifetime policy.
pub(crate) struct CaptureOptions<'a> {
    pub streams: Streams,
    pub limit: Option<usize>,
    pub head_tail: bool,
    pub read_error: CaptureReadError,
    pub input: Option<&'a [u8]>,
    pub deadline: CaptureDeadline,
    pub cancellation: Option<&'a std::sync::atomic::AtomicBool>,
    pub terminate: Option<fn(i32) -> io::Result<()>>,
    pub deadline_after_exit: bool,
    pub kill_group_on_incomplete: bool,
    pub eof_timeout: std::time::Duration,
}

/// Captured holds a bounded prefix or head and tail, plus the full byte count.
#[derive(Default)]
pub(crate) struct Captured {
    pub head: Vec<u8>,
    pub tail: Vec<u8>,
    pub seen: usize,
}

impl Captured {
    fn push(&mut self, bytes: &[u8], limit: Option<usize>, head_tail: bool) {
        self.seen += bytes.len();
        let Some(limit) = limit else {
            self.head.extend_from_slice(bytes);
            return;
        };
        let head_limit = if head_tail { (limit + 1) / 2 } else { limit };
        let take = head_limit.saturating_sub(self.head.len()).min(bytes.len());
        self.head.extend_from_slice(&bytes[..take]);
        if head_tail {
            self.tail.extend_from_slice(&bytes[take..]);
            let excess = self.tail.len().saturating_sub(limit / 2);
            self.tail.drain(..excess);
        }
    }
}

/// CaptureResult reports process state separately from output completeness.
pub(crate) struct CaptureResult {
    pub output: [Captured; 2],
    pub status: Option<std::process::ExitStatus>,
    pub timed_out: bool,
    pub cancelled: bool,
    pub incomplete: bool,
    pub cleanup_error: Option<io::Error>,
}

/// CaptureError distinguishes a failed start from a failure after effects were possible.
#[derive(Debug)]
pub(crate) enum CaptureError {
    Start(io::Error),
    Wait(io::Error),
}

fn drain(
    reader: &mut Option<io::PipeReader>,
    captured: &mut Captured,
    options: &CaptureOptions<'_>,
    incomplete: &mut bool,
) -> bool {
    use io::Read;
    let Some(pipe) = reader.as_mut() else {
        return false;
    };
    let mut buffer = [0; 32768];
    // A continuously noisy pipe must not starve cancellation or the other stream.
    for _ in 0..64 {
        match pipe.read(&mut buffer) {
            Ok(0) => {
                *reader = None;
                return false;
            }
            Ok(n) => captured.push(&buffer[..n], options.limit, options.head_tail),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return false,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => {
                match options.read_error {
                    CaptureReadError::Retry => return true,
                    CaptureReadError::Incomplete => *incomplete = true,
                    CaptureReadError::Eof => {}
                }
                *reader = None;
                return false;
            }
        }
    }
    false
}

/// capture starts under the pipe lock and polls all I/O with bounded inherited-pipe waits.
pub(crate) fn capture(
    mut command: Command,
    options: CaptureOptions<'_>,
) -> Result<CaptureResult, CaptureError> {
    use std::io::Write;
    use std::os::fd::AsRawFd;
    use std::process::Stdio;
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    let guard = lock();
    let (stdout, writer) = guard.pipe().map_err(CaptureError::Start)?;
    let stderr = match options.streams {
        Streams::Stdout => {
            command.stderr(Stdio::null());
            None
        }
        Streams::Merged => {
            command.stderr(Stdio::from(
                writer.try_clone().map_err(CaptureError::Start)?,
            ));
            None
        }
        Streams::Separate => {
            let (reader, writer) = guard.pipe().map_err(CaptureError::Start)?;
            command.stderr(Stdio::from(writer));
            Some(reader)
        }
    };
    command
        .stdout(Stdio::from(writer))
        .stdin(if options.input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
    let mut child = guard.spawn(&mut command).map_err(CaptureError::Start)?;
    drop(command);
    drop(guard);
    let pid = child.id() as i32;
    let mut readers = [Some(stdout), stderr];
    let mut stdin = child.stdin.take();
    let mut input = options.input.unwrap_or_default();
    for fd in readers
        .iter()
        .flatten()
        .map(AsRawFd::as_raw_fd)
        .chain(stdin.iter().map(AsRawFd::as_raw_fd))
    {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            let error = io::Error::last_os_error();
            let _ = child.kill();
            let _ = child.wait();
            return Err(CaptureError::Wait(error));
        }
    }
    let mut result = CaptureResult {
        output: [Captured::default(), Captured::default()],
        status: None,
        timed_out: false,
        cancelled: false,
        incomplete: false,
        cleanup_error: None,
    };
    let deadline = match &options.deadline {
        CaptureDeadline::At(deadline) => *deadline,
        CaptureDeadline::AfterSpawn(timeout) => Instant::now() + *timeout,
    };
    let mut exited = None;
    loop {
        let mut read_failed = [false; 2];
        for (index, reader) in readers.iter_mut().enumerate() {
            read_failed[index] = drain(
                reader,
                &mut result.output[index],
                &options,
                &mut result.incomplete,
            );
        }
        if input.is_empty() {
            stdin = None;
        }
        if let Some(pipe) = stdin.as_mut() {
            match pipe.write(input) {
                Ok(n) => input = &input[n..],
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(_) => stdin = None,
            }
        }
        if result.status.is_none() {
            result.status = child.try_wait().map_err(CaptureError::Wait)?;
            if result.status.is_some() {
                exited = Some(Instant::now());
            }
        }
        if result.status.is_some() && readers.iter().all(Option::is_none) && stdin.is_none() {
            break;
        }
        let now = Instant::now();
        if !result.timed_out
            && !result.cancelled
            && (result.status.is_none() || options.deadline_after_exit)
        {
            let cancelled = options
                .cancellation
                .is_some_and(|token| token.load(Ordering::Acquire));
            if cancelled || now >= deadline {
                result.cancelled = cancelled;
                result.timed_out = !cancelled;
                if let Some(terminate) = options.terminate {
                    result.cleanup_error = terminate(pid).err();
                } else {
                    let _ = child.kill();
                    result.status = child.wait().ok();
                    return Ok(result);
                }
            }
        }
        if exited.is_some_and(|time: Instant| time.elapsed() >= options.eof_timeout) {
            result.incomplete = true;
            if options.kill_group_on_incomplete {
                unsafe {
                    libc::kill(-pid, libc::SIGKILL);
                }
            }
            break;
        }
        let mut wait = Duration::from_millis(20);
        if !result.timed_out
            && !result.cancelled
            && (result.status.is_none() || options.deadline_after_exit)
        {
            wait = wait.min(deadline.saturating_duration_since(Instant::now()));
        }
        if let Some(time) = exited {
            wait = wait.min(options.eof_timeout.saturating_sub(time.elapsed()));
        }
        // Back off persistent read errors without treating them as EOF or spinning on POLLERR.
        let mut fds: Vec<_> = readers
            .iter()
            .enumerate()
            .filter(|(index, _)| !read_failed[*index])
            .filter_map(|(_, reader)| reader.as_ref())
            .map(|reader| libc::pollfd {
                fd: reader.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            })
            .collect();
        if let Some(pipe) = &stdin {
            fds.push(libc::pollfd {
                fd: pipe.as_raw_fd(),
                events: libc::POLLOUT,
                revents: 0,
            });
        }
        let millis = wait.as_millis() as i32 + i32::from(wait.subsec_nanos() % 1_000_000 != 0);
        if unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, millis) } < 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                let _ = child.kill();
                let _ = child.wait();
                return Err(CaptureError::Wait(error));
            }
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::fd::AsRawFd;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    fn options<'a>(streams: Streams) -> CaptureOptions<'a> {
        CaptureOptions {
            streams,
            limit: Some(8),
            head_tail: false,
            read_error: CaptureReadError::Incomplete,
            input: None,
            deadline: CaptureDeadline::At(Instant::now() + Duration::from_secs(2)),
            cancellation: None,
            terminate: None,
            deadline_after_exit: true,
            kill_group_on_incomplete: false,
            eof_timeout: Duration::from_millis(100),
        }
    }

    #[test]
    fn capture_read_errors_preserve_retry_incomplete_and_eof_policies() {
        use std::os::fd::OwnedFd;
        let dir = tempfile::tempdir().unwrap();
        for (policy, retry, incomplete, open) in [
            (CaptureReadError::Retry, true, false, true),
            (CaptureReadError::Incomplete, false, true, false),
            (CaptureReadError::Eof, false, false, false),
        ] {
            // A directory descriptor gives a real read error without closing or reusing live pipe FDs.
            let directory = std::fs::File::open(dir.path()).unwrap();
            let mut reader = Some(io::PipeReader::from(OwnedFd::from(directory)));
            let mut captured = Captured {
                head: b"before".to_vec(),
                tail: vec![],
                seen: 6,
            };
            let mut options = options(Streams::Stdout);
            options.read_error = policy;
            let mut output_incomplete = false;
            assert_eq!(
                drain(&mut reader, &mut captured, &options, &mut output_incomplete),
                retry
            );
            assert_eq!(reader.is_some(), open);
            assert_eq!(output_incomplete, incomplete);
            assert_eq!(captured.head, b"before");
            assert_eq!(captured.seen, 6);
            if retry {
                assert!(drain(
                    &mut reader,
                    &mut captured,
                    &options,
                    &mut output_incomplete
                ));
                assert!(
                    reader.is_some(),
                    "read errors must not satisfy the success/EOF check"
                );
                assert!(!output_incomplete);
            }
        }
    }

    #[test]
    fn capture_deadline_after_exit_preserves_timeout_versus_eof_failure() {
        for deadline_after_exit in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let pid_file = dir.path().join("descendant");
            let mut command = Command::new("/bin/sh");
            command.args(["-c", "/bin/sleep 3 & echo $! > \"$1\"; printf done", "sh"]);
            command.arg(&pid_file);
            let mut options = options(Streams::Stdout);
            options.deadline = CaptureDeadline::AfterSpawn(Duration::from_millis(200));
            options.eof_timeout = Duration::from_millis(400);
            options.deadline_after_exit = deadline_after_exit;
            let result = capture(command, options).unwrap();
            let pid: i32 = std::fs::read_to_string(pid_file)
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
            assert!(result.status.unwrap().success());
            assert_eq!(result.timed_out, deadline_after_exit);
            assert_eq!(result.incomplete, !deadline_after_exit);
            assert_eq!(result.output[0].head, b"done");
        }
    }

    #[test]
    fn capture_drains_both_streams_with_prefix_and_head_tail_caps() {
        for (head_tail, limit) in [false, true].into_iter().flat_map(|mode| {
            [0, 1, 7, 8, 16, 17]
                .into_iter()
                .map(move |limit| (mode, limit))
        }) {
            let mut command = Command::new("/bin/sh");
            command.args(["-c", "printf 0123456789abcdef; printf fedcba9876543210 >&2"]);
            let mut options = options(Streams::Separate);
            options.head_tail = head_tail;
            options.limit = Some(limit);
            let result = capture(command, options).unwrap();
            assert!(result.status.unwrap().success());
            assert!(!result.incomplete);
            for (captured, expected) in result.output.iter().zip([
                b"0123456789abcdef".as_slice(),
                b"fedcba9876543210".as_slice(),
            ]) {
                assert_eq!(captured.seen, 16);
                let head = if head_tail { (limit + 1) / 2 } else { limit }.min(expected.len());
                let tail = if head_tail { limit / 2 } else { 0 }.min(expected.len() - head);
                assert_eq!(captured.head, expected[..head]);
                assert_eq!(captured.tail, expected[expected.len() - tail..]);
            }
        }
    }

    #[test]
    fn capture_preserves_merged_order_and_unlimited_recipe_output() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "printf out; printf err >&2; printf end"]);
        let mut options = options(Streams::Merged);
        options.limit = None;
        let result = capture(command, options).unwrap();
        assert_eq!(result.output[0].head, b"outerrend");
        assert_eq!(result.output[0].seen, 9);
        assert_eq!(result.output[1].seen, 0);
    }

    #[test]
    fn capture_polls_input_without_deadlocking_a_full_output_pipe() {
        let input = vec![b'x'; 256 * 1024];
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "dd if=/dev/zero bs=65536 count=4 2>/dev/null; cat"]);
        let mut options = options(Streams::Stdout);
        options.input = Some(&input);
        let result = capture(command, options).unwrap();
        assert!(result.status.unwrap().success());
        assert!(!result.timed_out);
        assert_eq!(result.output[0].seen, 512 * 1024);
    }

    #[test]
    fn capture_bounds_inherited_eof_without_killing_detached_descendants() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("descendant");
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "/bin/sleep 3 & echo $! > \"$1\"; printf done", "sh"]);
        command.arg(&pid_file);
        let started = Instant::now();
        let result = capture(command, options(Streams::Stdout)).unwrap();
        let pid: i32 = std::fs::read_to_string(pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let alive = unsafe { libc::kill(pid, 0) } == 0;
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
        assert!(alive, "stdout-only capture must not kill grandchildren");
        assert!(result.status.unwrap().success());
        assert!(result.incomplete);
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(result.output[0].head, b"done");
        let mut next = Command::new("/bin/echo");
        next.arg("next");
        let result = capture(next, options(Streams::Stdout)).unwrap();
        assert!(!result.incomplete);
        assert_eq!(result.output[0].head, b"next\n");
    }

    #[test]
    fn capture_preserves_timeout_cancellation_and_cleanup_uncertainty() {
        use std::sync::atomic::AtomicBool;

        fn uncertain_cleanup(pid: i32) -> io::Result<()> {
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "cannot enumerate descendants",
            ))
        }

        for cancelled in [false, true] {
            let cancellation = AtomicBool::new(cancelled);
            let mut command = Command::new("/bin/sleep");
            command.arg("3");
            let mut options = options(Streams::Separate);
            options.deadline = CaptureDeadline::At(Instant::now() + Duration::from_millis(40));
            options.cancellation = Some(&cancellation);
            options.terminate = Some(uncertain_cleanup);
            options.deadline_after_exit = false;
            let result = capture(command, options).unwrap();
            assert!(result.status.is_some(), "leader must be reaped");
            assert_eq!(result.cancelled, cancelled);
            assert_eq!(result.timed_out, !cancelled);
            assert_eq!(
                result.cleanup_error.unwrap().kind(),
                io::ErrorKind::PermissionDenied
            );
        }
    }

    #[test]
    fn capture_preserves_absolute_and_after_spawn_deadlines() {
        for after_spawn in [false, true] {
            let mut options = options(Streams::Stdout);
            options.deadline = if after_spawn {
                CaptureDeadline::AfterSpawn(Duration::from_secs(1))
            } else {
                CaptureDeadline::At(Instant::now())
            };
            let worker = std::thread::spawn(move || {
                let mut command = Command::new("/bin/sleep");
                command.arg("0.1");
                capture(command, options).unwrap()
            });
            let result = worker.join().unwrap();
            assert_eq!(result.timed_out, !after_spawn);
            assert_eq!(result.status.unwrap().success(), after_spawn);
        }
    }

    #[test]
    fn capture_start_failure_releases_the_spawn_lock_and_pipes() {
        let dir = tempfile::tempdir().unwrap();
        let error = capture(
            Command::new(dir.path().join("missing")),
            options(Streams::Merged),
        );
        assert!(
            matches!(error, Err(CaptureError::Start(error)) if error.kind() == io::ErrorKind::NotFound)
        );
        let mut next = Command::new("/bin/echo");
        next.arg("next");
        let result = capture(next, options(Streams::Separate)).unwrap();
        assert!(result.status.unwrap().success());
        assert_eq!(result.output[0].head, b"next\n");
    }

    #[test]
    fn capture_eof_cleanup_signals_only_the_requested_process_group() {
        use std::os::unix::process::CommandExt;
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("still-alive");
        let mut command = Command::new("/bin/sh");
        command.args([
            "-c",
            "(/bin/sleep 1; printf alive > \"$1\") & printf done",
            "sh",
        ]);
        command.arg(&marker).process_group(0);
        let mut options = options(Streams::Separate);
        options.kill_group_on_incomplete = true;
        let result = capture(command, options).unwrap();
        assert!(result.status.unwrap().success());
        assert!(result.incomplete);
        assert!(!result.timed_out);
        assert_eq!(result.output[0].head, b"done");
        // If the EOF policy only closes the reader, the descendant will write the marker.
        std::thread::sleep(Duration::from_millis(1100));
        assert!(!marker.exists());
        let mut next = Command::new("/bin/echo");
        next.arg("next");
        let result = capture(next, self::options(Streams::Stdout)).unwrap();
        assert!(result.status.unwrap().success());
        assert_eq!(result.output[0].head, b"next\n");
    }

    fn close_on_exec(fd: i32) -> bool {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        flags >= 0 && flags & libc::FD_CLOEXEC != 0
    }

    #[test]
    fn guarded_pipes_are_close_on_exec() {
        let guard = lock();
        let (reader, writer) = guard.pipe().unwrap();
        assert!(close_on_exec(reader.as_raw_fd()));
        assert!(close_on_exec(writer.as_raw_fd()));
    }

    #[test]
    fn concurrent_starts_do_not_inherit_each_others_output_pipes() {
        // Threads released together start one command that exits at once and several that
        // sleep. An inherited write end holds the quick command's output open until a
        // sleeper exits, which is how a parallel agent's exec ended effect_unknown (#204).
        const SLEEPERS: usize = 7;
        let rounds = 60;
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(SLEEPERS + 1));
        let workers: Vec<_> = (0..SLEEPERS)
            .map(|_| {
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    for _ in 0..rounds {
                        barrier.wait();
                        let mut child = spawn(
                            Command::new("/bin/sleep")
                                .arg("2")
                                .stdin(Stdio::null())
                                .stdout(Stdio::piped())
                                .stderr(Stdio::piped()),
                        )
                        .unwrap();
                        barrier.wait();
                        let _ = child.kill();
                        let _ = child.wait();
                    }
                })
            })
            .collect();
        let mut slowest = Duration::ZERO;
        for _ in 0..rounds {
            barrier.wait();
            let started = Instant::now();
            let mut child = spawn(
                Command::new("/bin/sh")
                    .args(["-c", "exit 0"])
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped()),
            )
            .unwrap();
            let mut output = Vec::new();
            let mut stdout = child.stdout.take().unwrap();
            // Bound the read so a leaked pipe fails the test instead of waiting for the sleeper.
            unsafe {
                libc::fcntl(stdout.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK);
            }
            loop {
                match stdout.read_to_end(&mut output) {
                    Ok(_) => break,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        if started.elapsed() > Duration::from_millis(1000) {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    Err(error) => panic!("read command output: {error}"),
                }
            }
            child.wait().unwrap();
            slowest = slowest.max(started.elapsed());
            barrier.wait();
        }
        for worker in workers {
            worker.join().unwrap();
        }
        assert!(
            slowest < Duration::from_millis(1000),
            "a command's output reached EOF only after {slowest:?}"
        );
    }
}
