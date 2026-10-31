use std::path::PathBuf;

/// directory locates the platform cache root without creating it.
pub(crate) fn directory() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        return std::env::var_os("HOME")
            .filter(|home| !home.is_empty())
            .map(|home| PathBuf::from(home).join("Library/Caches"));
    }
    std::env::var_os("XDG_CACHE_HOME")
        .filter(|cache| !cache.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|home| !home.is_empty())
                .map(|home| PathBuf::from(home).join(".cache"))
        })
}

#[cfg(test)]
mod tests {
    use crate::{checkpoint, trace};
    use std::path::Path;

    #[test]
    fn trace_and_checkpoint_preserve_platform_cache_paths_and_errors() {
        const CHILD: &str = "REAGENT_TEST_CACHE_ROOT";
        const ID: &str = "0123456789abcdef";
        if let Ok(root) = std::env::var(CHILD) {
            if root.is_empty() {
                assert_eq!(
                    trace::default_trace_path("run").unwrap_err(),
                    "locate trace cache: operation failed"
                );
                assert_eq!(
                    checkpoint::directory(ID).unwrap_err(),
                    "user cache directory is unavailable"
                );
            } else {
                assert_eq!(
                    trace::default_trace_path("run").unwrap(),
                    Path::new(&root)
                        .join("reagent/runs/run/events.jsonl")
                        .to_string_lossy()
                );
                assert_eq!(
                    checkpoint::directory(ID).unwrap(),
                    Path::new(&root).join("reagent/sessions").join(ID)
                );
            }
            assert_eq!(
                checkpoint::directory("invalid").unwrap_err(),
                "invalid session ID \"invalid\": expected 16 lowercase hex digits"
            );
            return;
        }
        for (home, xdg, unix_root, mac_root) in [
            (
                Some("/home/test"),
                Some("/xdg/test"),
                "/xdg/test",
                "/home/test/Library/Caches",
            ),
            (
                Some("/home/test"),
                None,
                "/home/test/.cache",
                "/home/test/Library/Caches",
            ),
            (
                Some("/home/test"),
                Some(""),
                "/home/test/.cache",
                "/home/test/Library/Caches",
            ),
            (None, Some("/xdg/test"), "/xdg/test", ""),
            (Some(""), Some("relative-cache"), "relative-cache", ""),
            (None, None, "", ""),
            (Some(""), Some(""), "", ""),
        ] {
            // Isolate environment changes from tests running on other threads.
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "cache::tests::trace_and_checkpoint_preserve_platform_cache_paths_and_errors",
                ])
                .env(
                    CHILD,
                    if cfg!(target_os = "macos") {
                        mac_root
                    } else {
                        unix_root
                    },
                )
                .env_remove("HOME")
                .env_remove("XDG_CACHE_HOME");
            if let Some(home) = home {
                command.env("HOME", home);
            }
            if let Some(xdg) = xdg {
                command.env("XDG_CACHE_HOME", xdg);
            }
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "HOME={home:?}, XDG_CACHE_HOME={xdg:?}:\n{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}
