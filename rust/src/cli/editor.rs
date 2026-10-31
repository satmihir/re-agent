use crate::errors;
use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io;
use std::os::fd::IntoRawFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::OpenOptionsExt;
use std::process::{Command, Stdio};

fn fields(value: &OsStr) -> Vec<OsString> {
    let bytes = value.as_bytes();
    let mut words = Vec::new();
    let mut start = 0;
    let mut offset = 0;
    // Invalid UTF-8 is not whitespace, and must survive in executable names and arguments.
    for chunk in bytes.utf8_chunks() {
        for (i, ch) in chunk.valid().char_indices() {
            if ch.is_whitespace() {
                let end = offset + i;
                if start < end {
                    words.push(OsString::from_vec(bytes[start..end].to_vec()));
                }
                start = end + ch.len_utf8();
            }
        }
        offset += chunk.valid().len() + chunk.invalid().len();
    }
    if start < bytes.len() {
        words.push(OsString::from_vec(bytes[start..].to_vec()));
    }
    words
}

fn editor_command() -> Vec<OsString> {
    for value in [
        std::env::var_os("VISUAL").unwrap_or_default(),
        std::env::var_os("EDITOR").unwrap_or_default(),
        OsString::from("vi"),
    ] {
        let words = fields(&value);
        if !words.is_empty() {
            return words;
        }
    }
    unreachable!("vi is not whitespace")
}

/// Compose runs the configured editor on a private Markdown file and returns its raw bytes.
pub(crate) fn compose() -> Result<Vec<u8>, String> {
    let editor = editor_command();
    let dir = std::env::var_os("TMPDIR")
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "/tmp".into());
    let (path, file) = loop {
        let path = dir.join(format!("reagent-{}.md", uuid::Uuid::new_v4()));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(file) => break (path, file),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("create editor file: {}", errors::stable_io(&e))),
        }
    };
    let result = (|| {
        // Drop cannot report close failures; transfer ownership and never retry close.
        if unsafe { libc::close(file.into_raw_fd()) } != 0 {
            return Err(format!(
                "close editor file: {}",
                errors::stable_io(&io::Error::last_os_error())
            ));
        }
        let status = crate::process::spawn(
            Command::new(&editor[0])
                .args(&editor[1..])
                .arg(&path)
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit()),
        )
        .and_then(|mut child| child.wait())
        .map_err(|e| {
            let cause =
                if e.kind() == io::ErrorKind::NotFound && !editor[0].as_bytes().contains(&b'/') {
                    "executable not found"
                } else {
                    errors::stable_io(&e)
                };
            format!("run editor: {cause}")
        })?;
        if !status.success() {
            return Err(format!(
                "run editor: command exited with status {}",
                status.code().unwrap_or(-1)
            ));
        }
        fs::read(&path).map_err(|e| format!("read editor file: {}", errors::stable_io(&e)))
    })();
    if fs::remove_file(&path).is_err() {
        let _ = fs::remove_dir(&path);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    #[test]
    fn fields_preserve_go_whitespace_quotes_and_invalid_bytes() {
        assert_eq!(
            fields(OsStr::from_bytes(
                b" \t\r\n\x0b\x0c\xc2\x85\xc2\xa0\xe2\x80\x83editor\xe3\x80\x80\"two words\" \xff "
            )),
            [
                OsString::from("editor"),
                OsString::from("\"two"),
                OsString::from("words\""),
                OsString::from_vec(vec![0xff]),
            ]
        );
    }

    // Only this subprocess reads configured editor variables; parent tests never mutate them.
    #[test]
    fn compose_subprocess() {
        let Some(destination) = std::env::var_os("REAGENT_EDITOR_TEST_RESULT") else {
            return;
        };
        let result = match compose() {
            Ok(bytes) => [b"ok\0".as_slice(), &bytes].concat(),
            Err(error) => [b"err\0".as_slice(), error.as_bytes()].concat(),
        };
        fs::write(destination, result).unwrap();
    }

    fn run(dir: &Path, visual: &OsStr, editor: &OsStr, mode: &str) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let result = dir.join("result");
        let module = module_path!().split_once("::").unwrap().1;
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                &format!("{module}::compose_subprocess"),
                "--nocapture",
            ])
            .env("REAGENT_EDITOR_TEST_RESULT", &result)
            .env("VISUAL", visual)
            .env("EDITOR", editor)
            .env(
                "TMPDIR",
                dir.join(if mode == "create" { "missing" } else { "tmp" }),
            )
            .env("PATH", format!("{}:/usr/bin:/bin", dir.display()))
            .env("REPORT", dir.join("argv"))
            .env("MODE", mode)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"terminal input\n")
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "{:?}", output);
        assert_eq!(fs::read_dir(dir.join("tmp")).unwrap().count(), 0);
        (fs::read(result).unwrap(), output.stdout, output.stderr)
    }

    #[test]
    fn compose_uses_editor_fields_private_file_and_real_streams_then_cleans_up() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("tmp")).unwrap();
        let editor = dir.path().join("editor");
        fs::write(
            &editor,
            br#"#!/bin/sh
set -eu
printf '%s\n' "$@" > "$REPORT"
for file do :; done
test -f "$file"
test ! -s "$file"
case "$(uname -s)" in
  Darwin) mode=$(stat -f %Lp "$file");;
  *) mode=$(stat -c %a "$file");;
esac
test "$mode" = 600
case "$file" in */reagent-*.md) ;; *) exit 91;; esac
printf 'editor stdout\n'
printf 'editor stderr\n' >&2
case "$MODE" in
  fail) exit 7;;
  signal) kill -TERM $$;;
  read) rm "$file";;
  directory) rm "$file"; mkdir "$file";;
  empty) :;;
  raw) printf '\377\000\376\n' > "$file";;
  *) IFS= read -r line; printf '%s\n' "$line" > "$file";;
esac
"#,
        )
        .unwrap();
        fs::set_permissions(&editor, fs::Permissions::from_mode(0o700)).unwrap();
        fs::copy(&editor, dir.path().join("vi")).unwrap();
        let configured = format!("{} --first \"two words\" '$HOME'", editor.display());
        let (result, stdout, stderr) = run(
            dir.path(),
            OsStr::new(&configured),
            OsStr::new("missing"),
            "write",
        );
        assert_eq!(result, b"ok\0terminal input\n");
        assert!(stdout
            .windows(b"editor stdout\n".len())
            .any(|s| s == b"editor stdout\n"));
        assert_eq!(stderr, b"editor stderr\n");
        let argv = fs::read_to_string(dir.path().join("argv")).unwrap();
        let args: Vec<_> = argv.lines().collect();
        assert_eq!(&args[..4], &["--first", "\"two", "words\"", "'$HOME'"]);
        assert_eq!(args.len(), 5);
        assert!(!Path::new(args[4]).exists());
        for (visual, fallback, mode, want) in [
            (
                " \t\u{85}\u{2003}",
                editor.to_str().unwrap(),
                "write",
                b"ok\0terminal input\n".as_slice(),
            ),
            (
                "\n\u{a0}",
                " \r\t\u{3000}",
                "write",
                b"ok\0terminal input\n".as_slice(),
            ),
            (editor.to_str().unwrap(), "", "empty", b"ok\0".as_slice()),
            (
                editor.to_str().unwrap(),
                "",
                "raw",
                b"ok\0\xff\0\xfe\n".as_slice(),
            ),
            (
                editor.to_str().unwrap(),
                "",
                "fail",
                b"err\0run editor: command exited with status 7".as_slice(),
            ),
            (
                editor.to_str().unwrap(),
                "",
                "signal",
                b"err\0run editor: command exited with status -1".as_slice(),
            ),
            (
                editor.to_str().unwrap(),
                "",
                "read",
                b"err\0read editor file: path does not exist".as_slice(),
            ),
            (
                editor.to_str().unwrap(),
                "",
                "directory",
                b"err\0read editor file: path is a directory".as_slice(),
            ),
            (
                editor.to_str().unwrap(),
                "",
                "create",
                b"err\0create editor file: path does not exist".as_slice(),
            ),
            (
                "/reagent-editor-definitely-missing",
                "",
                "write",
                b"err\0run editor: path does not exist".as_slice(),
            ),
            (
                "reagent-editor-definitely-missing",
                "",
                "write",
                b"err\0run editor: executable not found".as_slice(),
            ),
        ] {
            let (result, _, _) = run(dir.path(), OsStr::new(visual), OsStr::new(fallback), mode);
            assert_eq!(
                result, want,
                "visual={visual:?}, editor={fallback:?}, mode={mode}"
            );
        }
    }
}
