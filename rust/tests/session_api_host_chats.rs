use base64::{engine::general_purpose::STANDARD, Engine};
use reagent::session_api::*;
use serde_json::{json, Value};
use std::{
    io::{Cursor, Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

const ROOT: &str = "REAGENT_HOST_CHATS_TEST_ROOT";
const PROMPT: &str = "You are a friendly chat assistant.";

/// Reply is what the fake chat model answers one request with.
#[derive(Clone)]
enum Reply {
    /// Text ends the Run with these words.
    Text(&'static str),
    /// Call asks for a tool by name with these arguments.
    Call(&'static str, Value),
}

fn png() -> Vec<u8> {
    let mut output = Cursor::new(Vec::new());
    image::DynamicImage::new_rgba8(6, 4)
        .write_to(&mut output, image::ImageFormat::Png)
        .unwrap();
    output.into_inner()
}

fn read_request(stream: &mut TcpStream) -> (String, Value) {
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut header = vec![];
    while !header.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        stream.read_exact(&mut byte).unwrap();
        header.push(byte[0]);
    }
    let header = String::from_utf8(header).unwrap();
    let path = header.split_whitespace().nth(1).unwrap().to_owned();
    let size = header
        .lines()
        .find_map(|line| {
            let (k, v) = line.split_once(':')?;
            k.eq_ignore_ascii_case("content-length")
                .then(|| v.trim().parse::<usize>().unwrap())
        })
        .unwrap();
    let mut body = vec![0; size];
    stream.read_exact(&mut body).unwrap();
    (path, serde_json::from_slice(&body).unwrap())
}

fn save(directory: &Path, name: &str, request: &Value) {
    let path = directory.join(name);
    let staged = path.with_extension("tmp");
    std::fs::write(&staged, request.to_string()).unwrap();
    std::fs::rename(&staged, &path).unwrap();
}

/// Isolated runs the calling test in a child process against one loopback server that plays
/// the chat model on both providers' routes and the Images API. Chat request N is answered
/// with replies[N] and saved as chat-N.json; image request N is saved as image-N.json and
/// answered once the file `release` exists in the root, or at once when `held` is false.
fn isolated(name: &str, replies: Vec<Reply>, held: bool, image_key: bool) -> Option<PathBuf> {
    if let Some(root) = std::env::var_os(ROOT) {
        return Some(root.into());
    }
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    let directory = root.path().to_path_buf();
    let server = std::thread::spawn(move || {
        let (mut chats, mut images) = (0, 0);
        while !stopped.load(Ordering::Acquire) {
            let mut stream = match listener.accept() {
                Ok((stream, _)) => stream,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                }
                Err(e) => panic!("listener: {e}"),
            };
            let (path, request) = read_request(&mut stream);
            let body = if path.ends_with("/images/generations") {
                save(&directory, &format!("image-{images}.json"), &request);
                images += 1;
                while held && !directory.join("release").exists() {
                    if stopped.load(Ordering::Acquire) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                json!({"data":[{"b64_json":STANDARD.encode(png())}],
                       "usage":{"input_tokens":7,"output_tokens":11,"total_tokens":18}})
            } else {
                save(&directory, &format!("chat-{chats}.json"), &request);
                let reply = replies.get(chats).expect("unexpected chat request").clone();
                chats += 1;
                if path.ends_with("/messages") {
                    let (stop, content) = match reply {
                        Reply::Text(text) => ("end_turn", json!([{"type":"text","text":text}])),
                        Reply::Call(name, input) => (
                            "tool_use",
                            json!([{"type":"tool_use","id":format!("toolu_{chats}"),"name":name,"input":input}]),
                        ),
                    };
                    json!({"id":"m","type":"message","role":"assistant","model":"claude-haiku-5-5",
                           "stop_reason":stop,"content":content,"usage":{"input_tokens":5,"output_tokens":2}})
                } else {
                    let output = match reply {
                        Reply::Text(text) => json!([{"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]}]),
                        Reply::Call(name, arguments) => json!([{"type":"function_call","id":format!("fc_{chats}"),"call_id":format!("call_{chats}"),"name":name,"arguments":arguments.to_string(),"status":"completed"}]),
                    };
                    let response = json!({"id":"fixture","status":"completed","model":"gpt-6.1-sol","output":output,"usage":{"input_tokens":11,"output_tokens":3}});
                    let event = json!({"type":"response.completed","response":response});
                    let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\nevent: response.completed\ndata: {event}\n\n");
                    continue;
                }
            }
            .to_string();
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    let mut child = Command::new(std::env::current_exe().unwrap());
    child
        .args(["--exact", name, "--nocapture"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", root.path())
        .env("XDG_CACHE_HOME", root.path().join("cache"))
        .env("ANTHROPIC_API_KEY", "offline-chat-images-fixture")
        .env("REAGENT_ANTHROPIC_BASE_URL", &endpoint)
        .env("REAGENT_OPENAI_BASE_URL", &endpoint)
        .env("REAGENT_RESPONSE_HEADER_TIMEOUT", "5")
        .env("REAGENT_RESPONSE_IDLE_TIMEOUT", "5")
        .env(ROOT, root.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if image_key {
        child.env("OPENAI_API_KEY", "offline-chat-images-fixture");
    }
    let output = child.output().unwrap();
    stop.store(true, Ordering::Release);
    server.join().unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    None
}

fn request(root: &Path, name: &str) -> Value {
    let path = root.join(name);
    let end = Instant::now() + Duration::from_secs(8);
    while !path.exists() {
        assert!(Instant::now() < end, "missing {}", path.display());
        std::thread::sleep(Duration::from_millis(2));
    }
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

/// Chat opens a tool-free Session that may make images in `root/images`.
fn chat(root: &Path, model: &str) -> SessionApi {
    let folder = root.join("images");
    std::fs::create_dir_all(&folder).unwrap();
    open_session(SessionOptions {
        workspace: folder,
        model: Some(model.into()),
        agents: false,
        tool_free: true,
        instructions: Some(PROMPT.into()),
        image_generation: true,
        ..Default::default()
    })
    .unwrap()
}

fn send(api: &SessionApi, id: &str, text: &str) -> RunRef {
    api.send_message(id, text.as_bytes(), PlanMode::Keep)
        .unwrap()
        .run
        .unwrap()
}

fn reply(api: &SessionApi, id: &str, text: &str) -> RunResult {
    api.wait_run(&send(api, id, text)).unwrap().result.unwrap()
}

/// Images lists the PNG files in the Session's folder.
fn images(root: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(root.join("images"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

fn draw() -> Reply {
    Reply::Call("generate_image", json!({"prompt":"a small red sailboat"}))
}

#[test]
fn a_chat_makes_an_image_in_its_folder_reports_it_and_continues_to_a_reply() {
    let replies = vec![
        draw(),
        Reply::Text("Here is your boat."),
        Reply::Text("aside"),
        Reply::Text("later"),
    ];
    let Some(root) = isolated(
        "a_chat_makes_an_image_in_its_folder_reports_it_and_continues_to_a_reply",
        replies,
        true,
        true,
    ) else {
        return;
    };
    let api = chat(&root, "gpt-6.1-sol");
    let snapshot = api.get_snapshot().unwrap();
    assert!(snapshot.tool_free && snapshot.image_generation);
    let id = snapshot.session.session_id;
    let run = send(&api, "one", "draw me a boat");

    // The only declared tool takes a prompt and nothing else: no path, key or backend.
    let first = request(&root, "chat-0.json");
    let tools = first["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 1, "{first}");
    assert_eq!(tools[0]["name"], "generate_image");
    let schema = &tools[0]["parameters"];
    assert_eq!(schema["required"], json!(["prompt"]), "{schema}");
    assert_eq!(
        schema["properties"].as_object().unwrap().len(),
        1,
        "{schema}"
    );
    assert_eq!(first["instructions"], PROMPT);

    // While the Images API works, the reply preview says an image is being made.
    assert_eq!(
        request(&root, "image-0.json")["prompt"],
        "a small red sailboat"
    );
    let mut cursor = None;
    let end = Instant::now() + Duration::from_secs(8);
    loop {
        let page = api.read_preview(&run, cursor, 1_000, None).unwrap();
        assert!(!page.finished, "the Run ended while its image was held");
        if page.generating {
            break;
        }
        cursor = Some(page.next);
        assert!(Instant::now() < end, "no preview reported the image");
    }
    assert!(
        images(&root).is_empty(),
        "nothing is saved before it exists"
    );
    std::fs::write(root.join("release"), b"").unwrap();

    let result = api.wait_run(&run).unwrap().result.unwrap();
    assert_eq!(result.status, "completed", "{}", result.reason);
    assert_eq!(result.reply, b"Here is your boat.");
    assert_eq!((result.steps, result.tool_calls), (2, 1));
    let saved = images(&root);
    assert_eq!(saved.len(), 1, "{saved:?}");
    assert!(saved[0].starts_with("image-") && saved[0].ends_with(".png"));
    assert_eq!(
        std::fs::read(root.join("images").join(&saved[0])).unwrap(),
        png()
    );
    let created: Vec<_> = result
        .effects
        .iter()
        .filter(|e| e.tool == "generate_image" && e.operation == "create")
        .map(|e| String::from_utf8(e.path.clone()).unwrap())
        .collect();
    assert_eq!(created, saved);

    // The model gets the result, and an OpenAI model also sees the picture.
    let second = request(&root, "chat-1.json").to_string();
    assert!(second.contains(&saved[0]), "{second}");
    assert!(second.contains("input_image"), "{second}");

    // A question asked beside the chat can make no image.
    let question = api
        .ask("aside", b"what did you draw?")
        .unwrap()
        .run
        .unwrap();
    api.wait_run(&question).unwrap();
    let aside = request(&root, "chat-2.json");
    assert!(
        aside["tools"]
            .as_array()
            .is_none_or(|tools| tools.is_empty()),
        "{aside}"
    );
    api.close().unwrap();

    let api = resume_session(&id).unwrap();
    assert!(api.get_snapshot().unwrap().image_generation);
    assert_eq!(reply(&api, "two", "thanks").reply, b"later");
    let later = request(&root, "chat-3.json");
    assert_eq!(later["tools"][0]["name"], "generate_image");
    api.close().unwrap();
}

#[test]
fn a_chat_that_makes_images_still_refuses_every_other_tool() {
    let Some(root) = isolated(
        "a_chat_that_makes_images_still_refuses_every_other_tool",
        vec![Reply::Call("exec", json!({"argv":["ls"]}))],
        false,
        true,
    ) else {
        return;
    };
    let api = chat(&root, "gpt-6.1-sol");
    let result = reply(&api, "one", "list my files");
    assert_eq!(result.status, "protocol_error");
    assert!(result.reason.contains("tool-free"), "{}", result.reason);
    assert!(images(&root).is_empty());
    api.close().unwrap();
}

#[test]
fn an_anthropic_chat_is_offered_the_tool_and_sees_the_image_it_made() {
    let Some(root) = isolated(
        "an_anthropic_chat_is_offered_the_tool_and_sees_the_image_it_made",
        vec![draw(), Reply::Text("Done.")],
        false,
        true,
    ) else {
        return;
    };
    let api = chat(&root, "claude-haiku-5-5");
    let result = reply(&api, "one", "draw me a boat");
    assert_eq!(result.status, "completed", "{}", result.reason);
    assert_eq!(result.reply, b"Done.");
    let first = request(&root, "chat-0.json");
    let tools = first["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 1, "{first}");
    assert_eq!(tools[0]["name"], "generate_image");
    assert_eq!(tools[0]["input_schema"]["required"], json!(["prompt"]));
    let saved = images(&root);
    assert_eq!(saved.len(), 1);
    let second = request(&root, "chat-1.json");
    let result = second["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|message| message["content"].as_array().into_iter().flatten())
        .find(|block| block["type"] == "tool_result")
        .unwrap_or_else(|| panic!("no tool result: {second}"));
    assert!(result.to_string().contains(&saved[0]), "{result}");
    assert_eq!(image_blocks(&result), 1, "{result}");
    api.close().unwrap();
}

/// Image_blocks counts the Anthropic image blocks carrying pixels in part of a request.
fn image_blocks(part: &Value) -> usize {
    part.to_string().matches("\"type\":\"base64\"").count()
}

/// Tool_result finds the result of one tool call in an Anthropic request.
fn tool_result(request: &Value, call: &str) -> Value {
    request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|message| message["content"].as_array().into_iter().flatten())
        .find(|block| block["type"] == "tool_result" && block["tool_use_id"] == call)
        .unwrap_or_else(|| panic!("no result for {call}: {request}"))
        .clone()
}

/// Forget_previews deletes the stored image previews under the test's home, as a cleared
/// cache would.
fn forget_previews(directory: &Path) {
    for entry in std::fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() && path.ends_with("reagent/images") {
            std::fs::remove_dir_all(&path).unwrap();
        } else if path.is_dir() {
            forget_previews(&path);
        }
    }
}

#[test]
fn without_an_image_key_the_call_fails_in_words_and_no_image_request_is_made() {
    let Some(root) = isolated(
        "without_an_image_key_the_call_fails_in_words_and_no_image_request_is_made",
        vec![draw(), Reply::Text("I can't make images here yet.")],
        false,
        false,
    ) else {
        return;
    };
    let api = chat(&root, "claude-haiku-5-5");
    let result = reply(&api, "one", "draw me a boat");
    assert_eq!(result.status, "completed", "{}", result.reason);
    let second = request(&root, "chat-1.json").to_string();
    assert!(second.contains("credentials_required"), "{second}");
    assert!(!root.join("image-0.json").exists());
    assert!(images(&root).is_empty());
    api.close().unwrap();
}

/// Ninth_image makes nine images in one chat and returns the last request, which must still
/// have gone out with at most eight previews.
fn ninth_image(name: &str, model: &str) -> Option<String> {
    let replies = (0..9)
        .flat_map(|_| [draw(), Reply::Text("Here.")])
        .collect();
    let root = isolated(name, replies, false, true)?;
    let api = chat(&root, model);
    for turn in 0..9 {
        let result = reply(&api, &format!("turn-{turn}"), "another boat");
        assert_eq!(result.status, "completed", "turn {turn}: {}", result.reason);
    }
    assert_eq!(images(&root).len(), 9);
    api.close().unwrap();
    Some(request(&root, "chat-17.json").to_string())
}

#[test]
fn a_ninth_image_in_one_chat_still_sends_a_request_with_at_most_eight_previews() {
    let name = "a_ninth_image_in_one_chat_still_sends_a_request_with_at_most_eight_previews";
    let Some(last) = ninth_image(name, "gpt-6.1-sol") else {
        return;
    };
    let previews = last.matches("input_image").count();
    assert!((1..=8).contains(&previews), "{previews} previews");
}

#[test]
fn a_ninth_image_in_a_claude_chat_also_retires_the_oldest_previews() {
    let name = "a_ninth_image_in_a_claude_chat_also_retires_the_oldest_previews";
    let Some(last) = ninth_image(name, "claude-haiku-5-5") else {
        return;
    };
    let previews = last.matches("\"type\":\"base64\"").count();
    assert!((1..=8).contains(&previews), "{previews} previews");
}

#[test]
fn image_generation_needs_an_existing_folder() {
    let root = tempfile::tempdir().unwrap();
    for tool_free in [true, false] {
        let error = open_session(SessionOptions {
            workspace: root.path().join("missing"),
            tool_free,
            image_generation: true,
            ..Default::default()
        })
        .err()
        .unwrap();
        assert_eq!(error.code, "invalid_argument");
        assert!(
            error.message.contains("existing directory"),
            "{}",
            error.message
        );
    }
}

const NOTE: &str = "The user spoke this message; the reply will be read aloud.";

/// Workspace_chat opens a host's Session with file tools in `root/work`, without commands,
/// that may make images on any model.
fn workspace_chat(root: &Path, model: &str) -> SessionApi {
    let folder = root.join("work");
    std::fs::create_dir_all(&folder).unwrap();
    open_session(SessionOptions {
        workspace: folder,
        model: Some(model.into()),
        agents: false,
        instructions: Some(PROMPT.into()),
        image_generation: true,
        exec: false,
        ..Default::default()
    })
    .unwrap()
}

fn tool_names(request: &Value) -> Vec<String> {
    request["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn a_host_session_with_files_and_no_commands_keeps_its_prompt_note_and_limits_across_resume() {
    let replies = vec![
        Reply::Call(
            "write_file",
            json!({"path":"list.md","content":"- socks\n"}),
        ),
        Reply::Text("Saved."),
        Reply::Call("exec", json!({"argv":["ls"]})),
        Reply::Text("I can't run programs."),
    ];
    let Some(root) = isolated(
        "a_host_session_with_files_and_no_commands_keeps_its_prompt_note_and_limits_across_resume",
        replies,
        false,
        true,
    ) else {
        return;
    };
    let api = workspace_chat(&root, "gpt-6.1-sol");
    let snapshot = api.get_snapshot().unwrap();
    assert!(!snapshot.tool_free && !snapshot.exec && snapshot.image_generation);
    let id = snapshot.session.session_id;
    let run = api
        .send_message_with_note("one", b"make a packing list", NOTE, PlanMode::Keep)
        .unwrap()
        .run
        .unwrap();
    let result = api.wait_run(&run).unwrap().result.unwrap();
    assert_eq!(result.status, "completed", "{}", result.reason);
    assert_eq!(result.reply, b"Saved.");

    // The host's text replaces re:agent's rules; the Runtime block still describes the folder.
    let first = request(&root, "chat-0.json");
    let instructions = first["instructions"].as_str().unwrap();
    assert!(instructions.starts_with(PROMPT), "{instructions}");
    assert!(instructions.contains("# Runtime"), "{instructions}");
    assert!(
        instructions.contains("commands are unavailable"),
        "{instructions}"
    );
    assert!(!instructions.contains("You are re:agent"), "{instructions}");
    let tools = tool_names(&first);
    for name in ["write_file", "edit_file", "read_file", "generate_image"] {
        assert!(tools.iter().any(|tool| tool == name), "{tools:?}");
    }
    for name in ["exec", "git_do"] {
        assert!(!tools.iter().any(|tool| tool == name), "{tools:?}");
    }
    assert_eq!(first["input"].to_string().matches(NOTE).count(), 1);

    assert_eq!(
        std::fs::read_to_string(root.join("work/list.md")).unwrap(),
        "- socks\n"
    );
    let written: Vec<_> = result
        .effects
        .iter()
        .filter(|e| e.tool == "write_file")
        .map(|e| {
            (
                e.operation.clone(),
                String::from_utf8(e.path.clone()).unwrap(),
            )
        })
        .collect();
    assert_eq!(written, [("create".to_owned(), "list.md".to_owned())]);
    api.close().unwrap();

    // Resume keeps commands off, and a call to one is refused without running anything.
    let api = resume_session(&id).unwrap();
    assert!(!api.get_snapshot().unwrap().exec);
    let result = reply(&api, "two", "list the folder with ls");
    assert_eq!(result.status, "completed", "{}", result.reason);
    let tools = tool_names(&request(&root, "chat-2.json"));
    assert!(!tools.iter().any(|tool| tool == "exec" || tool == "git_do"));
    let refusal = request(&root, "chat-3.json").to_string();
    assert!(refusal.contains("cannot run commands"), "{refusal}");
    api.close().unwrap();
}

#[test]
fn an_anthropic_session_with_files_sees_the_images_it_makes_and_views() {
    let Some(root) = isolated(
        "an_anthropic_session_with_files_sees_the_images_it_makes_and_views",
        vec![
            Reply::Call(
                "generate_image",
                json!({"prompt":"a small red sailboat","output_path":"boat.png"}),
            ),
            Reply::Call("view_image", json!({"path":"boat.png"})),
            Reply::Text("Done."),
            Reply::Text("Still here."),
        ],
        false,
        true,
    ) else {
        return;
    };
    let api = workspace_chat(&root, "claude-haiku-5-5");
    let result = reply(&api, "one", "draw me a boat");
    assert_eq!(result.status, "completed", "{}", result.reason);
    assert_eq!(result.reply, b"Done.");
    let tools = tool_names(&request(&root, "chat-0.json"));
    for name in ["generate_image", "view_image"] {
        assert!(tools.iter().any(|tool| tool == name), "{tools:?}");
    }
    assert_eq!(std::fs::read(root.join("work/boat.png")).unwrap(), png());

    // The picture it made, then the one it looked at, arrive as pixels inside each result.
    let made = tool_result(&request(&root, "chat-1.json"), "toolu_1");
    assert!(made.to_string().contains("boat.png"), "{made}");
    assert_eq!(image_blocks(&made), 1, "{made}");
    let third = request(&root, "chat-2.json");
    assert_eq!(image_blocks(&tool_result(&third, "toolu_2")), 1, "{third}");

    // Without its stored preview an image is described in words, and the request still goes.
    forget_previews(&root);
    assert_eq!(reply(&api, "two", "still there?").reply, b"Still here.");
    let later = request(&root, "chat-3.json");
    assert_eq!(image_blocks(&later), 0, "{later}");
    assert!(
        later.to_string().contains("image pixels unavailable"),
        "{later}"
    );
    api.close().unwrap();
}

/// Uncached drops the cache markers, which move between requests and are not history.
fn uncached(value: &Value) -> Value {
    match value {
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .filter(|(key, _)| *key != "cache_control")
                .map(|(key, field)| (key.clone(), uncached(field)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(uncached).collect()),
        _ => value.clone(),
    }
}

#[test]
fn a_resumed_claude_chat_resends_its_image_results_unchanged() {
    let Some(root) = isolated(
        "a_resumed_claude_chat_resends_its_image_results_unchanged",
        vec![
            Reply::Call("view_image", json!({"path":"boat.png"})),
            Reply::Text("A boat."),
            Reply::Text("Still a boat."),
        ],
        false,
        true,
    ) else {
        return;
    };
    std::fs::create_dir_all(root.join("work")).unwrap();
    std::fs::write(root.join("work/boat.png"), png()).unwrap();
    let api = workspace_chat(&root, "claude-haiku-5-5");
    let id = api.get_snapshot().unwrap().session.session_id;
    assert_eq!(reply(&api, "one", "what is this?").reply, b"A boat.");
    api.close().unwrap();

    // Claude binds a thinking block to every byte before it, so a resumed Session must
    // resend the viewed picture exactly as the first process did.
    let api = resume_session(&id).unwrap();
    assert_eq!(reply(&api, "two", "and now?").reply, b"Still a boat.");
    let before = request(&root, "chat-1.json");
    let after = request(&root, "chat-2.json");
    let viewed = tool_result(&after, "toolu_1");
    assert_eq!(image_blocks(&viewed), 1, "{viewed}");
    let earlier = before["messages"].as_array().unwrap();
    assert_eq!(
        uncached(&json!(
            after["messages"].as_array().unwrap()[..earlier.len()]
        )),
        uncached(&before["messages"])
    );
    api.close().unwrap();
}
