use reagent::session_api::*;
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

const ROOT: &str = "REAGENT_WEB_SEARCH_TEST_ROOT";

fn read_body(stream: &mut TcpStream) -> Value {
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
    let size = String::from_utf8(header)
        .unwrap()
        .lines()
        .find_map(|line| {
            let (k, v) = line.split_once(':')?;
            k.eq_ignore_ascii_case("content-length")
                .then(|| v.trim().parse::<usize>().unwrap())
        })
        .unwrap();
    let mut body = vec![0; size];
    stream.read_exact(&mut body).unwrap();
    serde_json::from_slice(&body).unwrap()
}

/// Isolated runs the calling test in a child process against a fake Messages endpoint that
/// answers request N with replies[N] and saves the request as request-N.json.
fn isolated(name: &str, replies: Vec<Value>) -> Option<PathBuf> {
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
        let mut index = 0;
        while !stopped.load(Ordering::Acquire) {
            let mut stream = match listener.accept() {
                Ok((stream, _)) => stream,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                }
                Err(e) => panic!("listener: {e}"),
            };
            let request = read_body(&mut stream);
            let path = directory.join(format!("request-{index}.json"));
            let staged = path.with_extension("tmp");
            std::fs::write(&staged, request.to_string()).unwrap();
            std::fs::rename(&staged, &path).unwrap();
            let body = replies
                .get(index)
                .expect("unexpected model request")
                .to_string();
            index += 1;
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", root.path())
        .env("XDG_CACHE_HOME", root.path().join("cache"))
        .env("ANTHROPIC_API_KEY", "offline-web-search-fixture")
        .env("REAGENT_ANTHROPIC_BASE_URL", &endpoint)
        .env("REAGENT_RESPONSE_HEADER_TIMEOUT", "3")
        .env("REAGENT_RESPONSE_IDLE_TIMEOUT", "3")
        .env(ROOT, root.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
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

fn request(root: &Path, index: usize) -> Value {
    serde_json::from_slice(&std::fs::read(root.join(format!("request-{index}.json"))).unwrap())
        .unwrap()
}

fn message(stop: &str, content: Value) -> Value {
    json!({"id":"m","type":"message","role":"assistant","model":"claude-haiku-5-5","stop_reason":stop,
        "content":content,"usage":{"input_tokens":5,"output_tokens":2}})
}

fn search(id: &str) -> Value {
    json!([
        {"type":"server_tool_use","id":id,"name":"web_search","input":{"query":"rust release"}},
        {"type":"web_search_tool_result","tool_use_id":id,"content":[
            {"type":"web_search_result","url":"https://a.example/x","title":"A","encrypted_content":"ENC-A","page_age":null}
        ]}
    ])
}

fn cited_answer() -> Value {
    message(
        "end_turn",
        json!([
            {"type":"text","text":"It is "},
            {"type":"text","text":"1.100","citations":[{"type":"web_search_result_location","url":"https://a.example/x","title":"A","encrypted_index":"IDX","cited_text":"1.100"}]},
            {"type":"text","text":"."}
        ]),
    )
}

fn open(root: &Path, web_search: bool) -> SessionApi {
    open_session(SessionOptions {
        workspace: root.into(),
        model: Some("claude-haiku-5-5".into()),
        agents: false,
        tool_free: true,
        instructions: Some("Answer briefly.".into()),
        web_search,
        ..Default::default()
    })
    .unwrap()
}

fn run(api: &SessionApi, id: &str, text: &str) -> RunResult {
    let run = api
        .send_message(id, text.as_bytes(), PlanMode::Keep)
        .unwrap()
        .run
        .unwrap();
    api.wait_run(&run).unwrap().result.unwrap()
}

#[test]
fn a_paused_search_turn_continues_without_new_input_and_reports_its_sources() {
    let replies = vec![
        message("pause_turn", search("srv1")),
        cited_answer(),
        cited_answer(),
        message("end_turn", json!([{"type":"text","text":"aside"}])),
    ];
    let Some(root) = isolated(
        "a_paused_search_turn_continues_without_new_input_and_reports_its_sources",
        replies,
    ) else {
        return;
    };
    let api = open(&root, true);
    assert!(api.get_snapshot().unwrap().web_search);
    let result = run(&api, "one", "which rust version?");
    assert_eq!(result.status, "completed", "{}", result.reason);
    assert_eq!(result.reply, b"It is 1.100.");
    assert_eq!((result.steps, result.web_searches), (2, 1));
    assert_eq!(
        result.sources,
        [Source {
            url: "https://a.example/x".into(),
            title: "A".into()
        }]
    );
    let first = request(&root, 0);
    assert_eq!(
        first["tools"],
        json!([{"type":"web_search_20250305","name":"web_search","max_uses":8}])
    );
    // The continuation resends the paused assistant turn and adds nothing after it.
    let resumed = request(&root, 1);
    let messages = resumed["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[1]["role"], "assistant");
    assert!(messages[1].to_string().contains("ENC-A"));
    let session = api.get_snapshot().unwrap().session.session_id;
    api.close().unwrap();

    let api = resume_session(&session).unwrap();
    assert!(api.get_snapshot().unwrap().web_search);
    run(&api, "two", "and the date?");
    let later = request(&root, 2).to_string();
    assert!(later.contains("ENC-A") && later.contains("IDX"), "{later}");
    // An isolated question never searches.
    let question = api.ask("aside", b"aside?").unwrap().run.unwrap();
    api.wait_run(&question).unwrap();
    assert!(
        request(&root, 3)["tools"].is_null(),
        "{}",
        request(&root, 3)
    );
    api.close().unwrap();
}

#[test]
fn consecutive_paused_search_turns_stop_at_the_cap() {
    let replies = (0..6)
        .map(|n| message("pause_turn", search(&format!("srv{n}"))))
        .collect();
    let Some(root) = isolated("consecutive_paused_search_turns_stop_at_the_cap", replies) else {
        return;
    };
    let api = open(&root, true);
    let result = run(&api, "one", "search forever");
    assert_eq!(result.status, "incomplete_response");
    assert_eq!((result.steps, result.web_searches), (6, 6));
    api.close().unwrap();
}

#[test]
fn without_web_search_a_tool_free_request_declares_no_tools() {
    let replies = vec![message("end_turn", json!([{"type":"text","text":"hi"}]))];
    let Some(root) = isolated(
        "without_web_search_a_tool_free_request_declares_no_tools",
        replies,
    ) else {
        return;
    };
    let api = open(&root, false);
    assert_eq!(run(&api, "one", "hello").status, "completed");
    assert!(request(&root, 0)["tools"].is_null());
    api.close().unwrap();
}
