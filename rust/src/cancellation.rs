use serde_json::Value;
use std::{
    collections::HashMap,
    ptr,
    sync::{
        atomic::{AtomicBool, AtomicPtr, Ordering},
        Arc, Mutex, MutexGuard, OnceLock,
    },
};

// Entries remain allocated because a signal handler may still hold an earlier pointer.
static TOKENS: OnceLock<Mutex<HashMap<(String, String), Arc<AtomicBool>>>> = OnceLock::new();
static ACTIVE: AtomicPtr<AtomicBool> = AtomicPtr::new(ptr::null_mut());
static SIGNAL_OWNER: Mutex<()> = Mutex::new(());

/// token returns the cancellation flag shared by one session and agent.
pub(crate) fn token(config: &Value) -> Arc<AtomicBool> {
    let key = (
        config["session_id"].as_str().unwrap_or("").to_owned(),
        config["agent_id"].as_str().unwrap_or("root").to_owned(),
    );
    TOKENS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap()
        .entry(key)
        .or_insert_with(|| Arc::new(AtomicBool::new(false)))
        .clone()
}

/// begin resets cancellation only when the caller starts an intended new run.
pub(crate) fn begin(config: &Value) {
    token(config).store(false, Ordering::Release);
}

/// cancelled reports whether this session and agent have been cancelled.
pub(crate) fn cancelled(config: &Value) -> bool {
    token(config).load(Ordering::Acquire)
}

/// cancel requests cancellation of this session and agent without affecting siblings.
#[cfg(test)]
pub(crate) fn cancel(config: &Value) {
    token(config).store(true, Ordering::Release);
}

extern "C" fn interrupt(_: libc::c_int) {
    let active = ACTIVE.load(Ordering::Acquire);
    if !active.is_null() {
        // The registry owns this allocation for the remainder of the process.
        unsafe {
            (*active).store(true, Ordering::Release);
        }
    }
}

/// Guard restores the prior SIGINT action when a root submission ends.
pub(crate) struct Guard {
    previous: libc::sigaction,
    _owner: MutexGuard<'static, ()>,
}

/// install routes SIGINT to the active root token without resetting it.
pub(crate) fn install(config: &Value) -> Result<Guard, String> {
    if config["agent_id"].as_str().unwrap_or("root") != "root" {
        return Err("only root submissions own SIGINT".into());
    }
    let owner = SIGNAL_OWNER
        .lock()
        .map_err(|_| "install SIGINT cancellation: signal ownership is unavailable".to_owned())?;
    let flag = token(config);
    let active = Arc::as_ptr(&flag).cast_mut();
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = interrupt as *const () as libc::sighandler_t;
    action.sa_flags = libc::SA_RESTART;
    unsafe {
        libc::sigemptyset(&mut action.sa_mask);
    }
    let mut previous = unsafe { std::mem::zeroed() };
    ACTIVE.store(active, Ordering::Release);
    let status = unsafe { libc::sigaction(libc::SIGINT, &action, &mut previous) };
    if status != 0 {
        let error = std::io::Error::last_os_error();
        ACTIVE.store(ptr::null_mut(), Ordering::Release);
        return Err(format!(
            "install SIGINT cancellation: {}",
            crate::errors::stable_io(&error)
        ));
    }
    Ok(Guard {
        previous,
        _owner: owner,
    })
}

impl Drop for Guard {
    fn drop(&mut self) {
        // Restore the action first; an already-entered handler's pointer remains valid.
        unsafe {
            libc::sigaction(libc::SIGINT, &self.previous, ptr::null_mut());
        }
        ACTIVE.store(ptr::null_mut(), Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{os::unix::process::ExitStatusExt, process::Command};

    #[test]
    fn tokens_are_targeted_and_only_begin_resets() {
        let root = json!({"session_id":"targeted"});
        let child = json!({"session_id":"targeted","agent_id":"t1"});
        let other = json!({"session_id":"other","agent_id":"t1"});
        let flag = token(&child);
        cancel(&child);
        assert!(cancelled(&child));
        assert!(!cancelled(&root));
        assert!(!cancelled(&other));
        assert!(Arc::ptr_eq(&flag, &token(&child.clone())));
        begin(&child);
        assert!(!flag.load(Ordering::Acquire));
    }

    #[test]
    fn child_cannot_install_a_root_signal_guard() {
        let child = json!({"session_id":"invalid-signal-owner","agent_id":"t1"});
        assert_eq!(
            install(&child).err().as_deref(),
            Some("only root submissions own SIGINT")
        );
        assert!(!cancelled(&child));
    }

    #[test]
    fn sigint_is_scoped_to_the_guard_in_a_subprocess() {
        for mode in ["restore", "default"] {
            let test = format!(
                "{}::signal_child",
                module_path!().split_once("::").unwrap().1
            );
            let result = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", &test, "--nocapture"])
                .env("REAGENT_SIGNAL_TEST", mode)
                .output()
                .unwrap();
            if mode == "default" {
                assert_eq!(result.status.signal(), Some(libc::SIGINT));
            } else {
                assert!(
                    result.status.success(),
                    "{}",
                    String::from_utf8_lossy(&result.stderr)
                );
            }
        }
    }

    static PREVIOUS_RAN: AtomicBool = AtomicBool::new(false);
    extern "C" fn previous_handler(_: libc::c_int) {
        PREVIOUS_RAN.store(true, Ordering::Release);
    }

    #[test]
    fn signal_child() {
        let Ok(mode) = std::env::var("REAGENT_SIGNAL_TEST") else {
            return;
        };
        let root = json!({"session_id":"signals"});
        let child = json!({"session_id":"signals","agent_id":"t1"});
        let mut previous: libc::sigaction = unsafe { std::mem::zeroed() };
        previous.sa_sigaction = if mode == "restore" {
            previous_handler as *const () as libc::sighandler_t
        } else {
            libc::SIG_DFL
        };
        unsafe {
            libc::sigemptyset(&mut previous.sa_mask);
            assert_eq!(libc::sigaction(libc::SIGINT, &previous, ptr::null_mut()), 0);
        }
        cancel(&root);
        let guard = install(&root).unwrap();
        assert!(
            cancelled(&root),
            "install must not reset an existing cancellation"
        );
        begin(&root);
        unsafe {
            libc::raise(libc::SIGINT);
        }
        assert!(cancelled(&root));
        assert!(!cancelled(&child));
        assert!(!PREVIOUS_RAN.load(Ordering::Acquire));
        drop(guard);
        unsafe {
            libc::raise(libc::SIGINT);
        }
        assert!(PREVIOUS_RAN.load(Ordering::Acquire));
        begin(&root);
        let guard = install(&root).unwrap();
        unsafe {
            libc::raise(libc::SIGINT);
        }
        assert!(cancelled(&root));
        drop(guard);
    }
}
