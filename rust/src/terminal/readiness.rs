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
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
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
