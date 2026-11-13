use crate::{
    cancellation, image_generation, images,
    tools::{fail, outcome},
    workspace::{os_outcome, workspace_outcome, Workspace},
};
use serde_json::{json, Value};
use std::{
    ffi::CString,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            ffi::OsStrExt,
            fs::{MetadataExt, OpenOptionsExt},
        },
    },
    path::{Path, PathBuf},
};

/// execute views local images or performs one separately authenticated generation/edit.
pub fn execute(
    ws: &mut Workspace,
    config: &Value,
    name: &str,
    args: &Value,
) -> Result<Value, Value> {
    let fields = match name {
        "view_image" => &["path"][..],
        "generate_image" => &["prompt", "output_path", "reference_image_paths"][..],
        _ => return Err(fail("tool_unavailable", "Unknown image tool")),
    };
    if config["tool_free"] == true {
        return Err(fail("permission_denied", "BTW Runs have no tools"));
    }
    if !crate::catalog::supports_images(config) {
        return Err(fail(
            "tool_unavailable",
            "Image tools require a supported OpenAI vision model",
        ));
    }
    if name == "generate_image" {
        if config["read_only"] == true {
            return Err(fail(
                "permission_denied",
                "Image generation is unavailable with read-only authority",
            ));
        }
        if config["plan"] == true {
            return Err(fail(
                "plan_mode",
                "Image generation is unavailable in plan mode",
            ));
        }
    }
    let object = args
        .as_object()
        .ok_or_else(|| fail("invalid_arguments", "Arguments must be an object"))?;
    if object.keys().any(|key| !fields.contains(&key.as_str())) {
        return Err(fail("invalid_arguments", "Unknown image tool argument"));
    }
    if name == "view_image" {
        let path = string(args, "path")?;
        let prepared = snapshot(ws, path)?;
        let data = observation(&prepared, path)?;
        return Ok(workspace_outcome(ws, data, "none"));
    }
    let prompt = string(args, "prompt")?;
    let path = string(args, "output_path")?;
    if prompt.chars().count() > image_generation::MAX_PROMPT_CHARS {
        return Err(fail(
            "invalid_arguments",
            "prompt must not exceed 16,000 characters",
        ));
    }
    let references = match args.get("reference_image_paths") {
        None => vec![],
        Some(value) => value
            .as_array()
            .filter(|paths| (1..=4).contains(&paths.len()))
            .ok_or_else(|| {
                fail(
                    "invalid_arguments",
                    "reference_image_paths must contain one to four paths",
                )
            })?
            .iter()
            .map(|path| {
                path.as_str()
                    .filter(|path| !path.trim().is_empty())
                    .ok_or_else(|| {
                        fail(
                            "invalid_arguments",
                            "Reference paths must be nonempty strings",
                        )
                    })
            })
            .collect::<Result<Vec<_>, _>>()?,
    };
    let key = image_generation::credentials()?;
    let preflight = destination(ws, path)?;
    let mut snapshots = Vec::new();
    let mut total = 0;
    for path in references {
        let prepared = snapshot(ws, path)?;
        total += prepared.original.len();
        if total > image_generation::MAX_REFERENCE_BYTES {
            return Err(fail(
                "image_references_too_large",
                "Reference originals exceed the aggregate 64 MiB request limit",
            ));
        }
        snapshots.push(prepared);
    }
    let generated = image_generation::generate(config, &key, prompt, &snapshots)?;
    let mut metadata = generated.metadata;
    metadata["output_path"] = json!(path);
    let prepared = images::prepare(&generated.bytes).map_err(|_| save_error("invalid_image_response", "Generation completed but the returned image is invalid or exceeds local limits. No artifact was saved; do not regenerate automatically.", &metadata))?;
    if prepared.metadata["mime_type"] != "image/png" {
        return Err(save_error("invalid_image_response", "Generation completed but the result is not PNG. No artifact was saved; do not regenerate automatically.", &metadata));
    }
    let mut data = observation(&prepared, path).map_err(|_| save_error("image_preview_failed", "Generation completed but its immutable preview could not be stored. No artifact was saved; do not regenerate automatically.", &metadata))?;
    if cancellation::cancelled(config) {
        return Err(save_error("cancelled", "Generation completed, but publication was cancelled. No artifact was saved; do not regenerate automatically.", &metadata));
    }
    // Keep the network outside this lock; bind publication to the preflight parent.
    let publication = ws.publish_mu.clone();
    let _guard = publication.lock().map_err(|_| save_error("image_publication_failed", "Generation completed but workspace publication is unavailable. No artifact was saved; do not regenerate automatically.", &metadata))?;
    let current = destination(ws, path).map_err(|error| save_error(error["code"].as_str().unwrap_or("image_publication_failed"), "Generation completed but the destination is no longer safe or available. No artifact was saved; do not regenerate automatically.", &metadata))?;
    let parent_identity = identity(&current.directory).and_then(|current| identity(&preflight.directory).map(|previous| (current, previous)))
        .map_err(|_| save_error("image_publication_failed", "Generation completed but the destination identity could not be checked. No artifact was saved; do not regenerate automatically.", &metadata))?;
    if parent_identity.0 != parent_identity.1 {
        return Err(save_error("invalid_path", "Generation completed but the destination directory changed. No artifact was saved; do not regenerate automatically.", &metadata));
    }
    if cancellation::cancelled(config) {
        return Err(save_error("cancelled", "Generation completed, but publication was cancelled. No artifact was saved; do not regenerate automatically.", &metadata));
    }
    publish(&current, &prepared.original).map_err(|_| save_error("image_publication_failed", "Generation completed but create-only artifact publication failed. No artifact was saved; do not regenerate automatically.", &metadata))?;
    let confirmed = (|| {
        let absolute = resolve(ws, path)?;
        let parent = directory(ws, absolute.parent().unwrap())?;
        if identity(&parent)? != identity(&current.directory)? {
            return Err(fail(
                "invalid_path",
                "Artifact directory moved during publication",
            ));
        }
        let observed = snapshot(ws, path)?;
        if observed.metadata["sha256"] != prepared.metadata["sha256"] {
            return Err(fail("stale_file", "Artifact changed during publication"));
        }
        ws.check_root()
    })();
    if confirmed.is_err() {
        metadata["artifact_published"] = Value::Null;
        metadata["publication_uncertain"] = json!(true);
        return Err(outcome(false, "image_publication_uncertain", "Generation completed and publication was attempted, but the artifact location or contents changed before confirmation. Inspect the filesystem; do not regenerate automatically.", metadata, "unknown"));
    }
    metadata["artifact_published"] = json!(true);
    data["operation"] = json!("create");
    for (key, value) in metadata.as_object().unwrap() {
        data[key] = value.clone();
    }
    Ok(workspace_outcome(ws, data, "applied"))
}

fn string<'a>(args: &'a Value, key: &str) -> Result<&'a str, Value> {
    args[key]
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            fail(
                "invalid_arguments",
                &format!("{key} must be a nonempty string"),
            )
        })
}

fn resolve(ws: &Workspace, path: &str) -> Result<PathBuf, Value> {
    if path.is_empty() || Path::new(path).is_absolute() {
        return Err(fail(
            "invalid_path",
            "Image paths must be nonempty and workspace-relative",
        ));
    }
    ws.check_root()?;
    ws.resolve(path)
}

fn directory(ws: &Workspace, path: &Path) -> Result<File, Value> {
    ws.check_root()?;
    let mut directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY | libc::O_NONBLOCK)
        .open(&ws.root)
        .map_err(os_outcome)?;
    let root = fs::symlink_metadata(&ws.root).map_err(os_outcome)?;
    if identity(&directory)? != (root.dev(), root.ino()) {
        return Err(fail(
            "invalid_path",
            "Workspace root changed while opening the image path",
        ));
    }
    for component in path
        .strip_prefix(&ws.root)
        .map_err(|_| {
            fail(
                "invalid_path",
                "Image directory must be inside the workspace",
            )
        })?
        .components()
    {
        directory = open_at(
            &directory,
            component.as_os_str(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_DIRECTORY | libc::O_NONBLOCK,
        )?;
    }
    ws.check_root()?;
    Ok(directory)
}

fn open_at(directory: &File, name: &std::ffi::OsStr, flags: i32) -> Result<File, Value> {
    let name = CString::new(name.as_bytes())
        .map_err(|_| fail("invalid_path", "Image path contains a NUL byte"))?;
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return Err(os_outcome(std::io::Error::last_os_error()));
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn identity(file: &File) -> Result<(u64, u64), Value> {
    let metadata = file.metadata().map_err(os_outcome)?;
    Ok((metadata.dev(), metadata.ino()))
}

fn snapshot(ws: &Workspace, path: &str) -> Result<images::Prepared, Value> {
    let absolute = resolve(ws, path)?;
    let directory = directory(
        ws,
        absolute
            .parent()
            .ok_or_else(|| fail("invalid_path", "Image path must name a file"))?,
    )?;
    let mut file = open_at(
        &directory,
        absolute
            .file_name()
            .ok_or_else(|| fail("invalid_path", "Image path must name a file"))?,
        libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK,
    )?;
    let metadata = file.metadata().map_err(os_outcome)?;
    if !metadata.is_file() {
        return Err(fail("not_file", "Image path must be a regular file"));
    }
    if metadata.len() > image_generation::MAX_IMAGE_BYTES as u64 {
        return Err(fail(
            "image_too_large",
            "Image exceeds the 20 MiB compressed-byte limit",
        ));
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take((image_generation::MAX_IMAGE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(os_outcome)?;
    ws.check_root()?;
    // Binary viewing deliberately does not call Workspace::remember.
    images::prepare(&bytes).map_err(|message| fail("invalid_image", &message))
}

fn observation(prepared: &images::Prepared, path: &str) -> Result<Value, Value> {
    let preview =
        images::store(prepared).map_err(|message| fail("image_preview_failed", &message))?;
    let mut data = prepared.metadata.clone();
    data["path"] = json!(path);
    let pixels = |metadata: &Value| {
        metadata["width"].as_u64().unwrap_or(0) * metadata["height"].as_u64().unwrap_or(0)
    };
    data["preview_resized"] = json!(pixels(&preview) < pixels(&data));
    data["preview"] = preview;
    Ok(data)
}

struct Destination {
    directory: File,
    name: std::ffi::OsString,
}

fn destination(ws: &Workspace, path: &str) -> Result<Destination, Value> {
    let absolute = resolve(ws, path)?;
    if absolute
        .extension()
        .and_then(|value| value.to_str())
        .is_none_or(|value| !value.eq_ignore_ascii_case("png"))
    {
        return Err(fail(
            "invalid_path",
            "output_path must have a .png extension",
        ));
    }
    let directory = directory(
        ws,
        absolute
            .parent()
            .ok_or_else(|| fail("invalid_path", "output_path must name a file"))?,
    )?;
    match fs::symlink_metadata(&absolute) {
        Ok(_) => {
            return Err(fail(
                "already_exists",
                "Image output is create-only; choose a new output_path",
            ))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(os_outcome(error)),
    }
    Ok(Destination {
        directory,
        name: absolute.file_name().unwrap().to_owned(),
    })
}

fn publish(destination: &Destination, bytes: &[u8]) -> Result<(), Value> {
    let temporary = format!(".reagent-image-{}", crate::types::new_id());
    let mut file = open_at(
        &destination.directory,
        std::ffi::OsStr::new(&temporary),
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW,
    )?;
    // Descriptor-relative publication cannot be redirected through a replaced symlink.
    let temporary = CString::new(temporary).unwrap();
    let name = CString::new(destination.name.as_bytes()).unwrap();
    let result = (|| {
        file.write_all(bytes).map_err(os_outcome)?;
        let fd = destination.directory.as_raw_fd();
        if unsafe { libc::linkat(fd, temporary.as_ptr(), fd, name.as_ptr(), 0) } != 0 {
            return Err(os_outcome(std::io::Error::last_os_error()));
        }
        Ok(())
    })();
    unsafe {
        libc::unlinkat(destination.directory.as_raw_fd(), temporary.as_ptr(), 0);
    }
    result
}

fn save_error(code: &str, message: &str, metadata: &Value) -> Value {
    outcome(false, code, message, metadata.clone(), "applied")
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{engine::general_purpose::STANDARD, Engine};
    use std::{
        io::Cursor,
        net::{TcpListener, TcpStream},
        os::unix::fs::symlink,
        process::Command,
        sync::mpsc,
        thread,
        time::Duration,
    };

    fn isolated(case: &str) {
        let home = tempfile::tempdir().unwrap();
        let result = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tool_images::tests::image_tool_fixture",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("REAGENT_IMAGE_TOOL_FIXTURE", case)
            .env("HOME", home.path())
            .env("XDG_CACHE_HOME", home.path().join("cache"))
            .env("OPENAI_API_KEY", "fixture-image-key")
            .env("API_PROXY_URL", "http://127.0.0.1:1/must-not-be-used")
            .env("REAGENT_OPENAI_BASE_URL", "not-a-url")
            .env("REAGENT_RESPONSE_HEADER_TIMEOUT", "3")
            .env("REAGENT_RESPONSE_IDLE_TIMEOUT", "3")
            .env_remove("HTTP_PROXY")
            .env_remove("http_proxy")
            .env_remove("HTTPS_PROXY")
            .env_remove("https_proxy")
            .env_remove("ALL_PROXY")
            .env_remove("all_proxy")
            .env("NO_PROXY", "*")
            .env("no_proxy", "*")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{case}:\n{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
    }

    #[test]
    fn missing_generation_key_does_not_block_viewing_or_publish() {
        isolated("credentials");
    }
    #[test]
    fn generation_view_and_edit_keep_original_bytes_and_separate_auth() {
        isolated("workflow");
    }
    #[test]
    fn image_arguments_paths_and_regular_file_limits_fail_before_submission() {
        isolated("validation");
    }
    #[test]
    fn direct_image_handlers_enforce_capabilities_authority_and_prompt_characters() {
        isolated("authority");
    }
    #[test]
    fn image_http_failures_are_single_attempt_bounded_and_redacted() {
        isolated("failures");
    }
    #[test]
    fn generation_revalidates_root_directory_symlinks_and_collisions() {
        isolated("publication");
    }
    #[test]
    fn cancellation_distinguishes_preflight_from_possible_remote_effects() {
        isolated("cancellation");
    }

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut output = Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(width, height)
            .write_to(&mut output, image::ImageFormat::Png)
            .unwrap();
        output.into_inner()
    }

    fn request(stream: &mut TcpStream) -> Vec<u8> {
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut bytes = Vec::new();
        let header_end = loop {
            let mut buffer = [0; 4096];
            let count = stream.read(&mut buffer).unwrap();
            assert!(count > 0);
            bytes.extend_from_slice(&buffer[..count]);
            if let Some(end) = bytes.windows(4).position(|value| value == b"\r\n\r\n") {
                break end + 4;
            }
            assert!(bytes.len() < 64 << 10);
        };
        let length: usize = String::from_utf8_lossy(&bytes[..header_end])
            .lines()
            .find_map(|line| {
                line.split_once(':')
                    .filter(|(key, _)| key.eq_ignore_ascii_case("content-length"))
                    .map(|(_, value)| value.trim().parse().unwrap())
            })
            .unwrap();
        while bytes.len() < header_end + length {
            let mut buffer = [0; 8192];
            let count = stream.read(&mut buffer).unwrap();
            assert!(count > 0);
            bytes.extend_from_slice(&buffer[..count]);
        }
        bytes
    }

    fn response(status: u16, body: &[u8]) -> Vec<u8> {
        let mut bytes = format!("HTTP/1.1 {status} Fixture\r\nConnection: close\r\nX-Request-ID: req_fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n", body.len()).into_bytes();
        bytes.extend_from_slice(body);
        bytes
    }

    fn generated(bytes: &[u8]) -> Vec<u8> {
        serde_json::to_vec(&json!({"data":[{"b64_json":STANDARD.encode(bytes)}],"usage":{"total_tokens":12,"input_tokens":5,"output_tokens":7,"input_tokens_details":{"image_tokens":3,"text_tokens":2},"ignored":"fixture-image-key"}})).unwrap()
    }

    fn server(action: impl FnOnce(Vec<u8>) -> Vec<u8> + Send + 'static) -> thread::JoinHandle<()> {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        std::env::set_var(
            "REAGENT_OPENAI_BASE_URL",
            format!("http://{}/v1", listener.local_addr().unwrap()),
        );
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let bytes = request(&mut stream);
            let _ = stream.write_all(&action(bytes));
        })
    }

    fn call(ws: &mut Workspace, config: &Value, args: Value) -> Value {
        execute(ws, config, "generate_image", &args).unwrap_or_else(|error| error)
    }

    #[test]
    fn image_tool_fixture() {
        let Ok(case) = std::env::var("REAGENT_IMAGE_TOOL_FIXTURE") else {
            return;
        };
        let root = tempfile::tempdir().unwrap();
        let mut ws = Workspace::open(root.path().to_str().unwrap()).unwrap();
        let config = json!({"session_id":format!("image-{case}"),"provider":"openai","model":"gpt-6.1-sol","auth_mode":"chatgpt","proxied":true,"api_key":"not-an-image-credential"});
        let original = png(3, 2);
        fs::write(root.path().join("reference.png"), &original).unwrap();
        let args = json!({"prompt":"Draw a shape","output_path":"output.png"});
        match case.as_str() {
            "credentials" => {
                std::env::remove_var("OPENAI_API_KEY");
                let listener = TcpListener::bind("127.0.0.1:0").unwrap();
                std::env::set_var(
                    "REAGENT_OPENAI_BASE_URL",
                    format!("http://{}/v1", listener.local_addr().unwrap()),
                );
                let result = call(&mut ws, &config, args);
                assert_eq!(result["code"], "credentials_required");
                assert_eq!(result["effect"], "none");
                assert!(!root.path().join("output.png").exists());
                listener.set_nonblocking(true).unwrap();
                assert_eq!(
                    listener.accept().unwrap_err().kind(),
                    std::io::ErrorKind::WouldBlock
                );
                let viewed = execute(
                    &mut ws,
                    &config,
                    "view_image",
                    &json!({"path":"reference.png"}),
                )
                .unwrap();
                assert_eq!(
                    viewed["data"]["sha256"],
                    crate::workspace::digest(&original)
                );
                assert!(images::load(&viewed["data"]["preview"]).is_ok());
                assert!(ws.seen.is_empty());
                assert!(!ws.returned(
                    &root.path().join("reference.png"),
                    viewed["data"]["sha256"].as_str().unwrap()
                ));
            }
            "workflow" => {
                let bytes = original.clone();
                let serving = server(move |request| {
                    let end = request
                        .windows(4)
                        .position(|value| value == b"\r\n\r\n")
                        .unwrap()
                        + 4;
                    let headers = String::from_utf8_lossy(&request[..end]).to_lowercase();
                    assert!(headers.starts_with("post /v1/images/generations http/1.1"));
                    assert!(headers.contains("authorization: bearer fixture-image-key"));
                    assert!(!headers.contains("not-an-image-credential"));
                    let body: Value = serde_json::from_slice(&request[end..]).unwrap();
                    assert_eq!(
                        body,
                        json!({"model":image_generation::MODEL,"prompt":"Draw a shape","n":1,"quality":"auto","size":"auto","output_format":"png"})
                    );
                    response(200, &generated(&bytes))
                });
                let first = call(&mut ws, &config, args);
                serving.join().unwrap();
                assert_eq!(first["ok"], true, "{first}");
                assert_eq!(first["effect"], "applied");
                assert_eq!(first["data"]["operation"], "create");
                assert_eq!(first["data"]["path"], "output.png");
                assert_eq!(first["data"]["artifact_published"], true);
                assert_eq!(first["data"]["remote_generation"], "completed");
                assert_eq!(first["data"]["request_id"], "req_fixture");
                assert_eq!(first["data"]["image_usage"]["total_tokens"], 12);
                assert_eq!(first["data"]["cost_known"], false);
                assert!(first["data"].get("usage").is_none());
                assert!(!first.to_string().contains("fixture-image-key"));
                assert_eq!(fs::read(root.path().join("output.png")).unwrap(), original);
                let viewed = execute(
                    &mut ws,
                    &config,
                    "view_image",
                    &json!({"path":"output.png"}),
                )
                .unwrap();
                assert_eq!(first["data"]["preview"], viewed["data"]["preview"]);
                let source = png(2300, 1);
                let reference = root.path().join("quoted\"reference.jpg");
                fs::write(&reference, &source).unwrap();
                let bytes = original.clone();
                let serving = server(move |request| {
                    let headers = String::from_utf8_lossy(&request).to_string();
                    assert!(headers.starts_with("POST /v1/images/edits HTTP/1.1"));
                    assert!(headers.contains("multipart/form-data; boundary=reagent-image-"));
                    assert!(headers.contains("name=\"image[]\"; filename=\"reference-0.png\""));
                    assert!(!headers.contains("quoted\"reference"));
                    assert!(request.windows(source.len()).any(|window| window == source));
                    fs::write(reference, b"changed after snapshot").unwrap();
                    response(200, &generated(&bytes))
                });
                let edited = call(
                    &mut ws,
                    &config,
                    json!({"prompt":"Revise the shape","output_path":"edited.PNG","reference_image_paths":["quoted\"reference.jpg"]}),
                );
                serving.join().unwrap();
                assert_eq!(edited["ok"], true, "{edited}");
                assert_eq!(fs::read(root.path().join("edited.PNG")).unwrap(), original);
                execute(
                    &mut ws,
                    &config,
                    "view_image",
                    &json!({"path":"edited.PNG"}),
                )
                .unwrap();
            }
            "authority" => {
                for (key, value, code) in [
                    ("provider", json!("anthropic"), "tool_unavailable"),
                    ("model", json!("unlisted-model"), "tool_unavailable"),
                    ("tool_free", json!(true), "permission_denied"),
                    ("read_only", json!(true), "permission_denied"),
                    ("plan", json!(true), "plan_mode"),
                ] {
                    let mut restricted = config.clone();
                    restricted[key] = value;
                    let result = call(&mut ws, &restricted, args.clone());
                    assert_eq!(result["code"], code, "{key}: {result}");
                    assert_eq!(result["effect"], "none");
                    assert!(!root.path().join("output.png").exists());
                    if matches!(key, "read_only" | "plan") {
                        let result = execute(
                            &mut ws,
                            &restricted,
                            "view_image",
                            &json!({"path":"reference.png"}),
                        )
                        .unwrap();
                        assert!(result["data"].get("operation").is_none());
                    } else {
                        let result = execute(
                            &mut ws,
                            &restricted,
                            "view_image",
                            &json!({"path":"reference.png"}),
                        )
                        .unwrap_err();
                        assert_eq!(result["code"], code);
                    }
                }
                for prompt in ["a".repeat(16_001), "🎨".repeat(16_001)] {
                    let result = call(
                        &mut ws,
                        &config,
                        json!({"prompt":prompt,"output_path":"output.png"}),
                    );
                    assert_eq!(result["code"], "invalid_arguments");
                    assert_eq!(result["effect"], "none");
                }
                let prompt = "🎨".repeat(16_000);
                let expected = prompt.clone();
                let bytes = original.clone();
                let serving = server(move |request| {
                    let end = request
                        .windows(4)
                        .position(|value| value == b"\r\n\r\n")
                        .unwrap()
                        + 4;
                    let body: Value = serde_json::from_slice(&request[end..]).unwrap();
                    assert_eq!(body["prompt"], expected);
                    response(200, &generated(&bytes))
                });
                let result = call(
                    &mut ws,
                    &config,
                    json!({"prompt":prompt,"output_path":"output.png"}),
                );
                serving.join().unwrap();
                assert_eq!(result["ok"], true, "{result}");
            }
            "validation" => {
                fs::create_dir(root.path().join("folder")).unwrap();
                symlink("folder", root.path().join("linked")).unwrap();
                symlink("reference.png", root.path().join("image-link.png")).unwrap();
                let outside = tempfile::tempdir().unwrap();
                symlink(outside.path(), root.path().join("outside")).unwrap();
                let large = File::create(root.path().join("large.png")).unwrap();
                large
                    .set_len((image_generation::MAX_IMAGE_BYTES + 1) as u64)
                    .unwrap();
                let fifo = CString::new(root.path().join("pipe").as_os_str().as_bytes()).unwrap();
                assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
                fs::write(root.path().join("invalid.png"), b"not an image").unwrap();
                for path in [
                    "/absolute.png",
                    "../reference.png",
                    ".env",
                    ".git/file.png",
                    "folder",
                    "image-link.png",
                    "linked/file.png",
                    "outside/file.png",
                    "pipe",
                    "large.png",
                    "invalid.png",
                ] {
                    let error =
                        execute(&mut ws, &config, "view_image", &json!({"path":path})).unwrap_err();
                    assert_eq!(error["effect"], "none", "{path}: {error}");
                }
                for args in [
                    json!({}),
                    json!({"prompt":" ","output_path":"x.png"}),
                    json!({"prompt":"p","output_path":"x.png","extra":true}),
                    json!({"prompt":"p","output_path":"x.png","reference_image_paths":[]}),
                    json!({"prompt":"p","output_path":"x.png","reference_image_paths":[1]}),
                    json!({"prompt":"p","output_path":"x.png","reference_image_paths":["a","b","c","d","e"]}),
                ] {
                    assert_eq!(call(&mut ws, &config, args)["code"], "invalid_arguments");
                }
                for output in [
                    "reference.png",
                    "folder",
                    "missing/out.png",
                    "linked/out.png",
                    "outside/out.png",
                    "image-link.png",
                    ".env.png",
                    "output.jpg",
                ] {
                    let result = call(&mut ws, &config, json!({"prompt":"p","output_path":output}));
                    assert_eq!(result["effect"], "none", "{output}: {result}");
                    assert_ne!(result["code"], "invalid_endpoint", "{output}: {result}");
                }
                // Canonicalization of a symlinked temporary root must not reject it.
                symlink(root.path(), outside.path().join("workspace")).unwrap();
                let mut aliased =
                    Workspace::open(outside.path().join("workspace").to_str().unwrap()).unwrap();
                execute(
                    &mut aliased,
                    &config,
                    "view_image",
                    &json!({"path":"reference.png"}),
                )
                .unwrap();
            }
            "failures" => {
                let mut cases = vec![
                    (401, response(401, b"fixture-image-key"), "none"),
                    (429, response(429, b"fixture-image-key"), "none"),
                    (500, response(500, b"fixture-image-key"), "unknown"),
                    (408, response(408, b"fixture-image-key"), "unknown"),
                    (200, response(200, b"not JSON fixture-image-key"), "unknown"),
                    (200, response(200, b"{\"data\":[]}"), "unknown"),
                    (200, response(200, b"{\"data\":[{},{}]}"), "unknown"),
                    (
                        200,
                        response(200, b"{\"data\":[{\"url\":\"http://127.0.0.1:1/image\"}]}"),
                        "unknown",
                    ),
                    (
                        200,
                        response(200, b"{\"data\":[{\"b64_json\":\"%%%\"}]}"),
                        "unknown",
                    ),
                    (200, response(200, &generated(b"not PNG")), "applied"),
                    (0, Vec::new(), "unknown"),
                ];
                let redirect = b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/never-follow\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec();
                cases.push((302, redirect, "unknown"));
                cases.push((200, response(200, &vec![b' '; (32 << 20) + 1]), "unknown"));
                cases.push((200,response(200,&serde_json::to_vec(&json!({"data":[{"b64_json":"A".repeat(image_generation::MAX_IMAGE_BYTES.div_ceil(3) * 4 + 4)}]})).unwrap()),"applied"));
                for (status, bytes, effect) in cases {
                    let serving = server(move |_| bytes);
                    let result = call(&mut ws, &config, args.clone());
                    serving.join().unwrap();
                    assert_eq!(result["ok"], false, "HTTP {status}: {result}");
                    assert_eq!(result["effect"], effect, "HTTP {status}: {result}");
                    assert_eq!(result["data"]["artifact_published"], false);
                    assert!(!result.to_string().contains("fixture-image-key"));
                    assert!(!root.path().join("output.png").exists());
                }
            }
            "publication" => {
                for change in ["collision", "symlink", "directory", "root"] {
                    let temp = tempfile::tempdir().unwrap();
                    let workspace_root = temp.path().join("workspace");
                    fs::create_dir(&workspace_root).unwrap();
                    let mut workspace = Workspace::open(workspace_root.to_str().unwrap()).unwrap();
                    fs::create_dir(workspace_root.join("images")).unwrap();
                    let bytes = original.clone();
                    let change_root = workspace_root.clone();
                    let serving = server(move |_| {
                        match change {
                            "collision" => {
                                fs::write(change_root.join("images/output.png"), b"collision")
                                    .unwrap()
                            }
                            "symlink" => {
                                fs::rename(
                                    change_root.join("images"),
                                    change_root.join("old-images"),
                                )
                                .unwrap();
                                symlink("old-images", change_root.join("images")).unwrap();
                            }
                            "directory" => {
                                fs::rename(
                                    change_root.join("images"),
                                    change_root.join("old-images"),
                                )
                                .unwrap();
                                fs::create_dir(change_root.join("images")).unwrap();
                            }
                            "root" => {
                                fs::rename(&change_root, change_root.with_file_name("old-root"))
                                    .unwrap();
                                fs::create_dir(&change_root).unwrap();
                                fs::create_dir(change_root.join("images")).unwrap();
                            }
                            _ => unreachable!(),
                        }
                        response(200, &generated(&bytes))
                    });
                    let result = call(
                        &mut workspace,
                        &config,
                        json!({"prompt":"p","output_path":"images/output.png"}),
                    );
                    serving.join().unwrap();
                    assert_eq!(result["ok"], false, "{change}: {result}");
                    assert_eq!(result["effect"], "applied", "{change}: {result}");
                    assert_eq!(result["data"]["remote_generation"], "completed");
                    assert_eq!(result["data"]["artifact_published"], false);
                    assert!(result["data"].get("operation").is_none());
                    assert!(result["data"].get("path").is_none());
                    if change == "collision" {
                        assert_eq!(
                            fs::read(workspace_root.join("images/output.png")).unwrap(),
                            b"collision"
                        );
                    } else {
                        assert!(!workspace_root.join("images/output.png").exists());
                    }
                }
            }
            "cancellation" => {
                let listener = TcpListener::bind("127.0.0.1:0").unwrap();
                std::env::set_var(
                    "REAGENT_OPENAI_BASE_URL",
                    format!("http://{}/v1", listener.local_addr().unwrap()),
                );
                cancellation::cancel(&config);
                let result = call(&mut ws, &config, args.clone());
                assert_eq!(result["code"], "cancelled");
                assert_eq!(result["effect"], "none");
                listener.set_nonblocking(true).unwrap();
                assert_eq!(
                    listener.accept().unwrap_err().kind(),
                    std::io::ErrorKind::WouldBlock
                );
                cancellation::begin(&config);
                let (submitted, observed) = mpsc::channel();
                let (release, wait_release) = mpsc::channel();
                let serving = server(move |_| {
                    submitted.send(()).unwrap();
                    wait_release.recv_timeout(Duration::from_secs(10)).unwrap();
                    Vec::new()
                });
                let cancel_config = config.clone();
                let flag = cancellation::token(&cancel_config);
                let canceller = thread::spawn(move || {
                    observed.recv_timeout(Duration::from_secs(10)).unwrap();
                    flag.store(true, std::sync::atomic::Ordering::Release);
                });
                let result = call(&mut ws, &config, args);
                release.send(()).unwrap();
                canceller.join().unwrap();
                serving.join().unwrap();
                assert_eq!(result["effect"], "unknown", "{result}");
                assert_eq!(result["data"]["remote_generation"], "unknown");
                assert!(!root.path().join("output.png").exists());
            }
            _ => panic!("unknown image fixture"),
        }
    }
}
