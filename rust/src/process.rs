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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::fd::AsRawFd;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

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
