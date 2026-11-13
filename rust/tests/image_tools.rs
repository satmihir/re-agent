use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use std::io::{Cursor, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

fn png(color: [u8; 4]) -> Vec<u8> {
    let pixels = image::RgbaImage::from_pixel(16, 12, image::Rgba(color));
    let mut bytes = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(pixels)
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    bytes.into_inner()
}

fn request(listener: &TcpListener) -> (TcpStream, String, Vec<u8>) {
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => panic!("accept image fixture: {error}"),
        }
        assert!(Instant::now() < deadline, "image request was not received");
        std::thread::sleep(Duration::from_millis(5));
    };
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut headers = Vec::new();
    while !headers.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        stream.read_exact(&mut byte).unwrap();
        headers.push(byte[0]);
    }
    let headers = String::from_utf8(headers).unwrap().to_lowercase();
    let length: usize = headers
        .lines()
        .find_map(|line| line.strip_prefix("content-length: "))
        .unwrap()
        .parse()
        .unwrap();
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes).unwrap();
    (stream, headers, bytes)
}

fn respond(stream: &mut TcpStream, status: &str, body: &str) {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
}

fn model_reply(stream: &mut TcpStream, call: Option<(&str, &str, Value)>) {
    let output = match call {
        Some((id, name, args)) => {
            json!([{"type":"function_call", "id":id, "call_id":id, "name":name, "arguments":args.to_string()}])
        }
        None => {
            json!([{"type":"message", "role":"assistant", "content":[{"type":"output_text", "text":"image workflow complete"}]}])
        }
    };
    let event = json!({"type":"response.completed", "response":{"id":"response-local", "status":"completed", "output":output}});
    respond(stream, "200 OK", &format!("data: {event}\n\n"));
}

fn command(root: &Path, endpoint: &str, key: bool) -> Command {
    let config = root.join(if cfg!(target_os = "macos") {
        "Library/Application Support"
    } else {
        "config"
    });
    let credentials = config.join("reagent");
    std::fs::create_dir_all(&credentials).unwrap();
    std::fs::set_permissions(&credentials, std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = credentials.join("chatgpt.json");
    std::fs::write(&path, json!({"client_id":"local-client", "access_token":"local-oauth", "refresh_token":"local-refresh", "scope":"chatgpt.tokens.use.direct", "expires_at":chrono::Utc::now().timestamp()+3600}).to_string()).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_reagent"));
    command
        .env_clear()
        .env("HOME", root)
        .env("XDG_CONFIG_HOME", config)
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("REAGENT_OPENAI_BASE_URL", endpoint)
        .env("REAGENT_OPENAI_AUTH_ISSUER", endpoint)
        .env("REAGENT_ANTHROPIC_BASE_URL", endpoint)
        .env("REAGENT_JEV_BASE_URL", endpoint)
        .env("REAGENT_RESPONSE_HEADER_TIMEOUT", "2")
        .env("REAGENT_RESPONSE_IDLE_TIMEOUT", "2")
        .current_dir(root)
        .args([
            "run",
            "--model",
            "gpt-6.1-sol",
            "--no-project-instructions",
            "--trace-file",
        ])
        .arg(root.join("events.jsonl"))
        .arg("Create an image, revise it, and inspect the result.");
    if key {
        command.env("OPENAI_API_KEY", "local-image-key");
    }
    command
}

#[test]
fn generated_and_edited_artifacts_are_pixels_in_the_next_chatgpt_request() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let original = png([0, 0, 255, 255]);
    let edited = png([255, 0, 0, 255]);
    let generated_reply = original.clone();
    let edited_reply = edited.clone();
    let server = std::thread::spawn(move || {
        for index in 0..6 {
            let (mut stream, headers, bytes) = request(&listener);
            if index == 1 || index == 3 {
                assert!(headers.contains("authorization: bearer local-image-key\r\n"));
                assert!(!headers.contains("local-oauth"));
                if index == 1 {
                    assert!(headers.starts_with("post /images/generations "));
                    let body: Value = serde_json::from_slice(&bytes).unwrap();
                    assert_eq!(body["model"], "gpt-image-2.5-flare");
                    assert_eq!(body["n"], 1);
                    assert_eq!(body["output_format"], "png");
                } else {
                    assert!(headers.starts_with("post /images/edits "));
                    assert!(headers.contains("content-type: multipart/form-data;"));
                    assert!(bytes
                        .windows(generated_reply.len())
                        .any(|part| part == generated_reply));
                }
                let image = if index == 1 {
                    &generated_reply
                } else {
                    &edited_reply
                };
                respond(&mut stream, "200 OK", &json!({"data":[{"b64_json":STANDARD.encode(image)}], "usage":{"total_tokens":42}}).to_string());
                continue;
            }
            assert!(headers.starts_with("post /responses "));
            assert!(headers.contains("authorization: bearer local-oauth\r\n"));
            assert!(!headers.contains("local-image-key"));
            let body: Value = serde_json::from_slice(&bytes).unwrap();
            let outputs: Vec<_> = body["input"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|item| item["type"] == "function_call_output")
                .collect();
            assert_eq!(
                outputs.len(),
                match index {
                    0 => 0,
                    2 => 1,
                    4 => 2,
                    _ => 3,
                }
            );
            for output in outputs {
                let parts = output["output"]
                    .as_array()
                    .expect("image outputs are structured, not JSON strings");
                assert_eq!(parts[0]["type"], "input_text");
                let pixels = parts
                    .iter()
                    .find(|part| part["type"] == "input_image")
                    .unwrap();
                let url = pixels["image_url"].as_str().unwrap();
                let bytes = STANDARD
                    .decode(url.strip_prefix("data:image/png;base64,").unwrap())
                    .unwrap();
                let decoded = image::load_from_memory(&bytes).unwrap();
                assert_eq!((decoded.width(), decoded.height()), (16, 12));
            }
            let call = match index {
                0 => Some((
                    "generate",
                    "generate_image",
                    json!({"prompt":"a blue circle", "output_path":"generated.png"}),
                )),
                2 => Some((
                    "edit",
                    "generate_image",
                    json!({"prompt":"make the circle red", "output_path":"edited.png", "reference_image_paths":["generated.png"]}),
                )),
                4 => Some(("view", "view_image", json!({"path":"edited.png"}))),
                _ => None,
            };
            model_reply(&mut stream, call);
        }
    });
    let output = command(&root, &endpoint, true).output().unwrap();
    server.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"image workflow complete\n");
    assert_eq!(std::fs::read(root.join("generated.png")).unwrap(), original);
    assert_eq!(std::fs::read(root.join("edited.png")).unwrap(), edited);
    let trace = std::fs::read_to_string(root.join("events.jsonl")).unwrap();
    for forbidden in [
        "local-oauth",
        "local-image-key",
        &STANDARD.encode(&original),
        &STANDARD.encode(&edited),
    ] {
        assert!(
            !trace.contains(forbidden),
            "private image/credential data appeared in trace"
        );
    }
}

#[test]
fn missing_image_key_is_actionable_without_interrupting_chatgpt_inference() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        for index in 0..2 {
            let (mut stream, headers, bytes) = request(&listener);
            assert!(
                headers.starts_with("post /responses "),
                "missing key must not contact the Images API"
            );
            assert!(headers.contains("authorization: bearer local-oauth\r\n"));
            if index == 1 {
                let body: Value = serde_json::from_slice(&bytes).unwrap();
                let output = body["input"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|item| item["type"] == "function_call_output")
                    .unwrap();
                let outcome: Value =
                    serde_json::from_str(output["output"].as_str().unwrap()).unwrap();
                assert_eq!(outcome["code"], "credentials_required");
                assert_eq!(outcome["effect"], "none");
                assert!(outcome["message"]
                    .as_str()
                    .unwrap()
                    .contains("OPENAI_API_KEY"));
            }
            model_reply(
                &mut stream,
                (index == 0).then(|| {
                    (
                        "missing-key",
                        "generate_image",
                        json!({"prompt":"a circle", "output_path":"missing.png"}),
                    )
                }),
            );
        }
    });
    let output = command(&root, &endpoint, false).output().unwrap();
    server.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!root.join("missing.png").exists());
}

#[test]
fn image_tools_are_provider_scoped_and_generation_obeys_authority() {
    let directory = tempfile::tempdir().unwrap();
    for (provider, model, read_only, tool_free, agent, view, generate) in [
        ("openai", "gpt-6.1-sol", false, false, false, true, true),
        ("openai", "gpt-6.1-sol", true, false, false, true, false),
        ("openai", "gpt-6.1-sol", false, true, false, false, false),
        ("openai", "gpt-6.1-sol", false, false, true, true, true),
        ("openai", "gpt-6.1-sol", true, false, true, true, false),
        (
            "openai",
            "unverified-model",
            false,
            false,
            false,
            false,
            false,
        ),
        (
            "anthropic",
            "claude-haiku-5-5",
            false,
            false,
            false,
            false,
            false,
        ),
    ] {
        let config = json!({"provider":provider,"model":model,"read_only":read_only,"tool_free":tool_free,"agent":agent,"workspace":directory.path()});
        let specs = reagent::tools::specs(&config);
        let names: Vec<_> = specs
            .as_array()
            .unwrap()
            .iter()
            .map(|spec| spec["name"].as_str().unwrap())
            .collect();
        assert_eq!(names.contains(&"view_image"), view, "{config}");
        assert_eq!(names.contains(&"generate_image"), generate, "{config}");
        if !generate {
            let mut session = reagent::runtime::Session::new(config.clone()).unwrap();
            let outcome = reagent::tools::execute(
                &mut session.tools,
                &config,
                "generate_image",
                r#"{"prompt":"circle","output_path":"denied.png"}"#,
            )
            .unwrap();
            assert_eq!(outcome["ok"], false);
            assert_eq!(outcome["effect"], "none");
        }
    }
    let config =
        json!({"provider":"openai","model":"gpt-6.1-sol","plan":true,"workspace":directory.path()});
    let mut session = reagent::runtime::Session::new(config.clone()).unwrap();
    let outcome = reagent::tools::execute(
        &mut session.tools,
        &config,
        "generate_image",
        r#"{"prompt":"circle","output_path":"denied.png"}"#,
    )
    .unwrap();
    assert_eq!(outcome["ok"], false);
    assert_eq!(outcome["effect"], "none");
    assert!(!directory.path().join("denied.png").exists());
}
