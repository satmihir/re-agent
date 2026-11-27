use std::io;
use std::os::fd::RawFd;

// Darwin's /dev/tty reports POLLNVAL to poll even though select and read work.
#[cfg(target_os = "macos")]
pub(crate) fn poll_read(fd: RawFd, timeout_ms: i32) -> io::Result<libc::c_short> {
    if fd < 0 {
        return Err(io::Error::from_raw_os_error(libc::EBADF));
    }
    if fd as usize >= libc::FD_SETSIZE || timeout_ms < 0 {
        return Err(io::Error::from_raw_os_error(libc::EINVAL));
    }
    unsafe {
        let mut read = std::mem::zeroed();
        libc::FD_ZERO(&mut read);
        libc::FD_SET(fd, &mut read);
        let mut timeout = libc::timeval {
            tv_sec: (timeout_ms / 1000).into(),
            tv_usec: ((timeout_ms % 1000) * 1000).into(),
        };
        let ready = libc::select(
            fd + 1,
            &mut read,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut timeout,
        );
        if ready < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(if ready == 0 { 0 } else { libc::POLLIN })
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn poll_read(fd: RawFd, timeout_ms: i32) -> io::Result<libc::c_short> {
    let mut poll = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    if unsafe { libc::poll(&mut poll, 1, timeout_ms) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(poll.revents)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::{Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    fn pty() -> (File, File) {
        let (mut master, mut slave) = (-1, -1);
        let mut size = libc::winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &mut size,
                )
            },
            0,
            "{}",
            io::Error::last_os_error()
        );
        unsafe { (File::from_raw_fd(master), File::from_raw_fd(slave)) }
    }

    #[test]
    fn controlling_tty_readiness_and_read() {
        // Re-exec isolates setsid/TIOCSCTTY from the parallel test harness.
        if std::env::var_os("REAGENT_TEST_CONTROLLING_TTY").is_some() {
            let mut tty = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/tty")
                .unwrap();
            let fd = tty.as_raw_fd();
            unsafe {
                let mut raw = std::mem::zeroed();
                assert_eq!(libc::tcgetattr(fd, &mut raw), 0);
                libc::cfmakeraw(&mut raw);
                assert_eq!(libc::tcsetattr(fd, libc::TCSANOW, &raw), 0);
            }
            let start = Instant::now();
            assert_eq!(poll_read(fd, 25).unwrap(), 0);
            assert!(start.elapsed() >= Duration::from_millis(10));
            tty.write_all(b"ready\n").unwrap();
            assert_ne!(poll_read(fd, 2000).unwrap() & libc::POLLIN, 0);
            let mut bytes = [0; 6];
            tty.read_exact(&mut bytes).unwrap();
            assert_eq!(&bytes, b"hello\r");
            assert_eq!(poll_read(fd, 0).unwrap(), 0);

            // The caller must retain control after EINTR, rather than silently retrying.
            extern "C" fn signal(_: libc::c_int) {}
            unsafe {
                let mut action: libc::sigaction = std::mem::zeroed();
                action.sa_sigaction = signal as *const () as usize;
                libc::sigemptyset(&mut action.sa_mask);
                let mut previous = std::mem::zeroed();
                assert_eq!(libc::sigaction(libc::SIGUSR1, &action, &mut previous), 0);
                let thread = libc::pthread_self() as usize;
                let sender = std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(25));
                    assert_eq!(
                        libc::pthread_kill(thread as libc::pthread_t, libc::SIGUSR1),
                        0
                    );
                });
                let error = poll_read(fd, 2000).unwrap_err();
                sender.join().unwrap();
                assert_eq!(
                    libc::sigaction(libc::SIGUSR1, &previous, std::ptr::null_mut()),
                    0
                );
                assert_eq!(error.kind(), io::ErrorKind::Interrupted);
            }
            return;
        }
        let (mut master, slave) = pty();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "terminal::readiness::tests::controlling_tty_readiness_and_read",
                "--nocapture",
            ])
            .env("REAGENT_TEST_CONTROLLING_TTY", "1")
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave));
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut output = Vec::new();
        let mut sent = false;
        loop {
            if poll_read(master.as_raw_fd(), 25).unwrap() != 0 {
                let mut bytes = [0; 1024];
                match master.read(&mut bytes) {
                    Ok(0) => break,
                    Ok(count) => output.extend_from_slice(&bytes[..count]),
                    Err(error) if error.raw_os_error() == Some(libc::EIO) => break,
                    Err(error) => panic!("read PTY: {error}"),
                }
                if !sent && output.windows(6).any(|bytes| bytes == b"ready\n") {
                    master.write_all(b"hello\r").unwrap();
                    sent = true;
                }
            }
            if child.try_wait().unwrap().is_some() {
                break;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!(
                    "TTY readiness timed out: {}",
                    String::from_utf8_lossy(&output)
                );
            }
        }
        assert!(
            child.wait().unwrap().success(),
            "{}",
            String::from_utf8_lossy(&output)
        );
        assert!(
            sent,
            "child never requested input: {}",
            String::from_utf8_lossy(&output)
        );
    }

    #[test]
    fn idle_wake_preserves_continuation_cursor_and_partial_paste() {
        use crate::terminal::{Input, Region, Submission};
        use std::sync::{atomic::AtomicBool, Arc, Mutex};

        // The fallback editor polls stdin, so give the test its own process and PTY.
        const CHILD: &str = "REAGENT_TEST_IDLE_WAKE";
        if std::env::var_os(CHILD).is_none() {
            let home = tempfile::tempdir().unwrap();
            let output = Command::new(std::env::current_exe().unwrap())
                .env("HOME", home.path())
                .env("XDG_CACHE_HOME", home.path().join("cache"))
                .args([
                    "--exact",
                    "terminal::readiness::tests::idle_wake_preserves_continuation_cursor_and_partial_paste",
                ])
                .env(CHILD, "1")
                .env("TERM", "dumb")
                .stdin(Stdio::null())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stdout)
            );
            return;
        }
        unsafe { libc::alarm(10) }; // Bound a regression that never polls the predicate.
        assert!(matches!(
            Input::new()
                .read_line_until("", || panic!("pipe woke"))
                .unwrap(),
            Submission::Eof
        ));
        for regional in [false, true] {
            let (mut master, slave) = pty();
            assert_eq!(unsafe { libc::dup2(slave.as_raw_fd(), 0) }, 0);
            let mut input = Input::new();
            if regional {
                input.tty = Some(slave);
                input.region = Some(Arc::new(Mutex::new(Region::new())));
                input.next.queued = Some("queued".into());
                assert!(matches!(
                    input.read_line_until("", || panic!("queue lost priority")).unwrap(),
                    Submission::Text(text) if text == "queued"
                ));
            }
            let mut sent = false;
            let submission = input
                .read_line_until("> ", || {
                    if sent {
                        return true;
                    }
                    // Start pasting a UTF-8 character in the middle of a continuation line.
                    master.write_all(b"first\\\rab\x1b[D\x1b[200~\xc3").unwrap();
                    sent = true;
                    false
                })
                .unwrap();
            assert!(matches!(submission, Submission::Wake));
            input
                .poll_turn(&AtomicBool::new(true), &AtomicBool::new(false))
                .unwrap();
            let mut tail = Some(b"\xa9\rX\x1b[201~\r");
            let submission = input
                .read_line_until("> ", || {
                    master
                        .write_all(tail.take().expect("submission did not finish"))
                        .unwrap();
                    false
                })
                .unwrap();
            assert!(matches!(submission, Submission::Text(text) if text == "first\naé\nXb"));

            // A lone ESC belongs to the idle editor, not to the Run that wakes it.
            input.decoder.feed(b"prefix\\\rab\x1b[D\x1b");
            assert!(matches!(
                input.read_line_until("> ", || true).unwrap(),
                Submission::Wake
            ));
            assert_eq!(input.decoder.escape(), None);
            if regional {
                input.decoder.feed(b"X\x03");
                let cancelled = AtomicBool::new(false);
                input
                    .poll_turn(&AtomicBool::new(false), &cancelled)
                    .unwrap();
                assert!(cancelled.into_inner());
                input.decoder.feed(b"!\r");
                assert!(
                    matches!(input.read_line("> ").unwrap(), Submission::Text(text) if text == "prefix\naX!b")
                );

                // The next, ordinary Run still discards its type-ahead on Ctrl-C.
                input.decoder.feed(b"discard me\x03");
                input
                    .poll_turn(&AtomicBool::new(false), &AtomicBool::new(false))
                    .unwrap();
                input.decoder.feed(b"ok\r");
                assert!(
                    matches!(input.read_line("> ").unwrap(), Submission::Text(text) if text == "ok")
                );
                woken_draft_survives_consent(&mut input, &mut master);
            }
        }
    }

    fn woken_draft_survives_consent(input: &mut crate::terminal::Input, master: &mut File) {
        use crate::session_api::{open_session, PlanMode, SessionOptions};
        use crate::terminal::Submission;
        use std::sync::atomic::AtomicBool;

        let root = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let script = root.path().join("script.json");
        let arguments = serde_json::json!({"path":target.path()}).to_string();
        std::fs::write(&script, serde_json::json!([
            {"blocks":[{"kind":"tool_call","call":{"call_id":"consent","name":"request_workspace_access","arguments":arguments}}]},
            {"blocks":[{"kind":"text","text":"done"}]}
        ]).to_string()).unwrap();
        let api = open_session(SessionOptions {
            workspace: root.path().to_owned(),
            scripted: Some(script),
            ..Default::default()
        })
        .unwrap();
        input.bind_workspace_consent(&api).unwrap();
        input.decoder.feed(b"prefix\\\rab\x1b[D");
        assert!(matches!(
            input.read_line_until("> ", || true).unwrap(),
            Submission::Wake
        ));
        let run = api
            .send_message("consent-test", b"ask", PlanMode::Keep)
            .unwrap()
            .run
            .unwrap();
        let guard = &input.consent.as_ref().unwrap().0;
        while guard.get_pending_workspace_access().unwrap().is_none() {
            std::thread::sleep(Duration::from_millis(5));
        }
        // Both the decoder and the PTY contain keys that would select Allow.
        input.decoder.feed(b"\x1b[A\r");
        master.write_all(b"\x1b[A\r").unwrap();
        assert_ne!(poll_read(0, 100).unwrap(), 0);
        // An undersized picker must fail closed without consuming these keys as answers.
        let tiny = libc::winsize {
            ws_row: 2,
            ws_col: 10,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        assert_eq!(unsafe { libc::ioctl(0, libc::TIOCSWINSZ, &tiny) }, 0);
        let cancelled = AtomicBool::new(false);
        let error = input
            .poll_turn(&AtomicBool::new(false), &cancelled)
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("workspace approval could not be displayed"));
        assert!(cancelled.into_inner());
        assert_eq!(poll_read(0, 0).unwrap(), 0);
        input.decoder.feed(b"X\r");
        assert!(
            matches!(input.read_line("> ").unwrap(), Submission::Text(text) if text == "prefix\naXb")
        );
        api.wait_run(&run).unwrap();
        api.close().unwrap();
    }

    #[test]
    fn pipe_readiness_includes_eof() {
        let mut fds = [-1; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        let mut read = unsafe { File::from_raw_fd(fds[0]) };
        let mut write = unsafe { File::from_raw_fd(fds[1]) };
        assert_eq!(poll_read(read.as_raw_fd(), 0).unwrap(), 0);
        write.write_all(b"x").unwrap();
        assert_ne!(poll_read(read.as_raw_fd(), 25).unwrap(), 0);
        let mut bytes = [0];
        read.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"x");
        drop(write);
        assert_ne!(poll_read(read.as_raw_fd(), 25).unwrap(), 0);
        assert_eq!(read.read(&mut bytes).unwrap(), 0);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn select_rejects_invalid_and_out_of_range_descriptors() {
        assert_eq!(
            poll_read(-1, 0).unwrap_err().raw_os_error(),
            Some(libc::EBADF)
        );
        assert_eq!(
            poll_read(libc::FD_SETSIZE as i32, 0)
                .unwrap_err()
                .raw_os_error(),
            Some(libc::EINVAL)
        );
        let (_, slave) = pty();
        let fd = slave.as_raw_fd();
        drop(slave);
        assert_eq!(
            poll_read(fd, 0).unwrap_err().raw_os_error(),
            Some(libc::EBADF)
        );
    }
}
