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
    time::{Duration, Instant},
};

const ROOT: &str = "REAGENT_PREVIEW_TEST_ROOT";
/// KEY starts with a letter replies rarely end on, since a forming prefix is held back.
const KEY: &str = "zq-preview-fixture";

/// Part is one piece of a fake provider stream.
#[derive(Clone)]
enum Part {
    Text(&'static str),
    Reasoning(&'static str),
    /// Wait holds the stream open until the named file exists in the test root.
    Wait(&'static str),
    /// Done completes the response with these output items.
    Done(Value),
}

fn message(text: &str) -> Value {
    json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]})
}

fn read_request(stream: &mut TcpStream) {
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
}

fn send(stream: &mut TcpStream, event: Value) -> bool {
    let kind = event["type"].as_str().unwrap().to_owned();
    write!(stream, "event: {kind}\ndata: {event}\n\n").is_ok() && stream.flush().is_ok()
}

/// Isolated runs the calling test in a child process against a fake Responses endpoint that
/// streams replies[N] for request N.
fn isolated(name: &str, replies: Vec<Vec<Part>>) -> Option<PathBuf> {
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
            read_request(&mut stream);
            let parts = replies
                .get(index)
                .expect("unexpected model request")
                .clone();
            index += 1;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
            for part in parts {
                let open = match part {
                    Part::Text(delta) => send(
                        &mut stream,
                        json!({"type":"response.output_text.delta","output_index":0,"content_index":0,"delta":delta}),
                    ),
                    Part::Reasoning(delta) => send(
                        &mut stream,
                        json!({"type":"response.reasoning_text.delta","output_index":0,"content_index":0,"delta":delta}),
                    ),
                    Part::Wait(file) => {
                        let mut open = true;
                        while open
                            && !directory.join(file).exists()
                            && !stopped.load(Ordering::Acquire)
                        {
                            open = stream.write_all(b": held\n\n").is_ok();
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        open
                    }
                    Part::Done(output) => {
                        let response = json!({"id":"fixture","status":"completed","model":"gpt-6.1-sol","output":output,"usage":{"input_tokens":11,"output_tokens":3}});
                        send(
                            &mut stream,
                            json!({"type":"response.completed","response":response}),
                        )
                    }
                };
                if !open {
                    break;
                }
            }
        }
    });
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", root.path())
        .env("XDG_CACHE_HOME", root.path().join("cache"))
        .env("OPENAI_API_KEY", KEY)
        .env("REAGENT_OPENAI_BASE_URL", &endpoint)
        .env("REAGENT_RESPONSE_HEADER_TIMEOUT", "5")
        .env("REAGENT_RESPONSE_IDLE_TIMEOUT", "5")
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

fn open(root: &Path, tool_free: bool) -> SessionApi {
    open_session(SessionOptions {
        workspace: root.into(),
        model: Some("gpt-6.1-sol".into()),
        reasoning_effort: Some("low".into()),
        agents: false,
        read_only: true,
        checkpoint: false,
        tool_free,
        ..Default::default()
    })
    .unwrap()
}

fn start(api: &SessionApi, text: &str) -> RunRef {
    api.send_message("message", text.as_bytes(), PlanMode::Keep)
        .unwrap()
        .run
        .unwrap()
}

/// Reader follows one Run's preview the way a client would, keeping every text it showed.
struct Reader {
    cursor: Option<PreviewCursor>,
    text: String,
    shown: Vec<String>,
    restarts: u32,
}

impl Reader {
    fn new() -> Self {
        Self {
            cursor: None,
            text: String::new(),
            shown: vec![],
            restarts: 0,
        }
    }

    /// Until reads pages until done(text) holds, returning false if the Run finished first.
    fn until(&mut self, api: &SessionApi, run: &RunRef, done: impl Fn(&str) -> bool) -> bool {
        let end = Instant::now() + Duration::from_secs(10);
        while !done(&self.text) {
            assert!(Instant::now() < end, "preview stalled at {:?}", self.text);
            let page = api.read_preview(run, self.cursor, 1000, None).unwrap();
            if page.finished {
                return false;
            }
            if page.restarted {
                self.restarts += 1;
                self.text.clear();
            }
            if page.restarted || !page.text.is_empty() {
                self.text.push_str(&page.text);
                self.shown.push(self.text.clone());
            }
            self.cursor = Some(page.next);
        }
        true
    }
}

#[test]
fn reply_text_is_readable_while_the_model_writes_and_reasoning_never_is() {
    let replies = vec![vec![
        Part::Reasoning("hidden plan"),
        Part::Text("Hello"),
        Part::Wait("go"),
        Part::Text(", world"),
        Part::Done(json!([message("Hello, world")])),
    ]];
    let Some(root) = isolated(
        "reply_text_is_readable_while_the_model_writes_and_reasoning_never_is",
        replies,
    ) else {
        return;
    };
    let api = open(&root, true);
    let run = start(&api, "hi");
    let mut reader = Reader::new();
    assert!(reader.until(&api, &run, |text| text == "Hello"));
    std::fs::write(root.join("go"), b"").unwrap();
    reader.until(&api, &run, |_| false);
    assert!(reader.shown.iter().all(|text| !text.contains("hidden")));
    assert_eq!(reader.shown.last().unwrap(), "Hello, world");
    let result = api.wait_run(&run).unwrap().result.unwrap();
    assert_eq!(result.reply, b"Hello, world");
    assert!(
        api.read_preview(&run, reader.cursor, 0, None)
            .unwrap()
            .finished
    );
    api.close().unwrap();
}

#[test]
fn a_new_model_step_restarts_the_preview() {
    let call = json!({"type":"function_call","id":"c1","call_id":"c1","name":"list_files","arguments":"{\"path\":\".\"}"});
    let replies = vec![
        vec![
            Part::Text("Let me look."),
            Part::Wait("go"),
            Part::Done(json!([message("Let me look."), call])),
        ],
        vec![Part::Text("Done."), Part::Done(json!([message("Done.")]))],
    ];
    let Some(root) = isolated("a_new_model_step_restarts_the_preview", replies) else {
        return;
    };
    let api = open(&root, false);
    let run = start(&api, "look around");
    let mut reader = Reader::new();
    assert!(reader.until(&api, &run, |text| text == "Let me look."));
    std::fs::write(root.join("go"), b"").unwrap();
    assert!(reader.until(&api, &run, |text| text == "Done."));
    assert_eq!(reader.restarts, 1);
    assert_eq!(reader.cursor.unwrap().attempt, 2);
    assert_eq!(api.wait_run(&run).unwrap().result.unwrap().reply, b"Done.");
    api.close().unwrap();
}

#[test]
fn a_credential_split_across_deltas_never_reaches_a_reader() {
    let replies = vec![vec![
        Part::Text("key zq-pre"),
        Part::Wait("go"),
        Part::Text("view-fixture done"),
        Part::Done(json!([message("key zq-preview-fixture done")])),
    ]];
    let Some(root) = isolated(
        "a_credential_split_across_deltas_never_reaches_a_reader",
        replies,
    ) else {
        return;
    };
    let api = open(&root, true);
    let run = start(&api, "what is the key?");
    let mut reader = Reader::new();
    assert!(reader.until(&api, &run, |text| text == "key "));
    // The forming prefix stays held back however long the stream pauses.
    assert!(api
        .read_preview(&run, reader.cursor, 100, None)
        .unwrap()
        .text
        .is_empty());
    std::fs::write(root.join("go"), b"").unwrap();
    reader.until(&api, &run, |_| false);
    for text in &reader.shown {
        assert!(!text.contains("zq") && !text.contains("fixture"), "{text}");
    }
    let result = api.wait_run(&run).unwrap().result.unwrap();
    assert_eq!(result.status, "provider_error", "{}", result.reason);
    api.close().unwrap();
}

#[test]
fn cancelling_a_run_finishes_its_preview() {
    let replies = vec![vec![Part::Text("Partial"), Part::Wait("never")]];
    let Some(root) = isolated("cancelling_a_run_finishes_its_preview", replies) else {
        return;
    };
    let api = open(&root, true);
    let run = start(&api, "write a lot");
    let mut reader = Reader::new();
    assert!(reader.until(&api, &run, |text| text == "Partial"));
    api.cancel_run(&run).unwrap();
    assert!(!reader.until(&api, &run, |_| false));
    assert_eq!(
        api.wait_run(&run).unwrap().result.unwrap().status,
        "cancelled"
    );
    api.close().unwrap();
}
