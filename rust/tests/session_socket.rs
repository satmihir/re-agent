use reagent::session_api::*;
use reagent::session_socket::{call, socket_path, Request};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const ROOT: &str = "REAGENT_SESSION_SOCKET_TEST_ROOT";

fn read_request(stream: &mut TcpStream) -> String {
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
    String::from_utf8(body).unwrap()
}

/// Answer plays the model for one request: a body naming HOLD_ME is held open until the
/// client goes away, after writing `held` in the root; one whose latest words name ASK_ME is
/// a question; anything else gets "Done.".
fn answer(mut stream: TcpStream, root: &Path, stop: &AtomicBool) {
    let body = read_request(&mut stream);
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let text = if body.contains("ASK_ME") {
        "Still working on it."
    } else if body.contains("HOLD_ME") && !body.contains("FOLLOW_UP") {
        std::fs::write(root.join("held"), b"").unwrap();
        while !stop.load(Ordering::Acquire) && stream.write_all(b": held\n\n").is_ok() {
            std::thread::sleep(Duration::from_millis(10));
        }
        return;
    } else {
        "Done."
    };
    let output = json!([{"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]}]);
    let response = json!({"id":"fixture","status":"completed","model":"gpt-6.1-sol","output":output,"usage":{"input_tokens":11,"output_tokens":3}});
    let event = json!({"type":"response.completed","response":response});
    let _ = write!(stream, "event: response.completed\ndata: {event}\n\n");
}

/// Isolated runs the calling test in a child process with its own HOME, against a loopback
/// server playing the model. The root is under /tmp so a socket path stays short on macOS.
fn isolated(name: &str) -> Option<PathBuf> {
    if let Some(root) = std::env::var_os(ROOT) {
        return Some(root.into());
    }
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let stop = Arc::new(AtomicBool::new(false));
    let server = {
        let (stop, root) = (stop.clone(), root.path().to_path_buf());
        std::thread::spawn(move || {
            let mut answers = vec![];
            while !stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        let (stop, root) = (stop.clone(), root.clone());
                        answers.push(std::thread::spawn(move || answer(stream, &root, &stop)));
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(2)),
                }
            }
            for answer in answers {
                answer.join().unwrap();
            }
        })
    };
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", root.path())
        .env("XDG_CACHE_HOME", root.path().join("cache"))
        .env("OPENAI_API_KEY", "offline-socket-fixture")
        .env("REAGENT_OPENAI_BASE_URL", &endpoint)
        .env(ROOT, root.path())
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

fn serve(args: &[&str]) -> Child {
    Command::new(env!("CARGO_BIN_EXE_reagent"))
        .arg("serve")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

fn ready(child: &mut Child) -> Value {
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    serde_json::from_str(&line).unwrap_or_else(|_| panic!("not a ready line: {line:?}"))
}

fn eventually(what: &str, mut done: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < end, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn send(id: &str, request_id: &str, text: &str) -> RunRef {
    let request = Request::SendMessage {
        request_id: request_id.into(),
        text: text.into(),
        note: None,
    };
    call::<CommandInfo>(id, &request).unwrap().run.unwrap()
}

fn finish(id: &str, run: &RunRef) -> RunResult {
    let run = Request::WaitRun { run: run.clone() };
    call::<RunInfo>(id, &run).unwrap().result.unwrap()
}

fn saved(id: &str) -> SavedSession {
    saved_sessions()
        .unwrap()
        .into_iter()
        .find(|s| s.session_id == id)
        .unwrap()
}

#[test]
fn a_served_session_answers_messages_questions_and_stops_over_its_socket() {
    let Some(root) =
        isolated("a_served_session_answers_messages_questions_and_stops_over_its_socket")
    else {
        return;
    };
    let work = root.join("work");
    std::fs::create_dir(&work).unwrap();
    let mut server = serve(&[
        "--workspace",
        work.to_str().unwrap(),
        "--model",
        "gpt-6.1-sol",
        "--agents=false",
        "--idle-exit",
        "2s",
    ]);
    let ready = ready(&mut server);
    let id = ready["session_id"].as_str().unwrap().to_owned();
    assert_eq!(ready["socket"], json!(socket_path(&id).unwrap()));

    let second = serve(&["--resume", &id]).wait_with_output().unwrap();
    assert!(!second.status.success());
    assert!(String::from_utf8_lossy(&second.stderr).contains("already"));

    let held = send(&id, "m1", "HOLD_ME and fix the test");
    eventually("the model request", || root.join("held").exists());
    let saved_mid_run = saved(&id);
    assert!(saved_mid_run.open);
    assert_eq!(saved_mid_run.phase, "model");

    let question = Request::Ask {
        request_id: "q1".into(),
        question: "ASK_ME what is left?".into(),
    };
    let asked = call::<CommandInfo>(&id, &question).unwrap().run.unwrap();
    assert_eq!(finish(&id, &asked).reply, b"Still working on it.");

    let cancel = Request::CancelRun { run: held.clone() };
    call::<RunInfo>(&id, &cancel).unwrap();
    assert_eq!(finish(&id, &held).status, "cancelled");
    let stopped = saved(&id);
    assert_eq!(
        (stopped.phase.as_str(), stopped.last_status.as_str()),
        ("terminal", "cancelled")
    );

    let next = send(&id, "m2", "FOLLOW_UP: carry on");
    assert_eq!(finish(&id, &next).reply, b"Done.");
    let page = read_saved_entries(&id, None, 100).unwrap();
    let said = |entry: &EntryView, words: &str| match &entry.content {
        EntryContent::User { text, .. } => text.bytes.starts_with(words.as_bytes()),
        _ => false,
    };
    assert!(page.entries.iter().any(|e| said(e, "HOLD_ME")));
    assert!(page.entries.iter().any(|e| said(e, "FOLLOW_UP")));
    assert_eq!(page.start + page.entries.len() as u64, page.total);
    let back = read_saved_entries(&id, Some(page.total - 1), 1).unwrap();
    assert_eq!((back.start, back.entries.len()), (page.total - 2, 1));

    let unknown = call::<Value>(
        &id,
        &Request::GetRun {
            run: RunRef {
                session: held.session.clone(),
                run_id: "missing".into(),
            },
        },
    );
    assert_eq!(unknown.unwrap_err().code, "not_found");

    let status = server.wait().unwrap();
    assert!(status.success());
    assert!(!socket_path(&id).unwrap().exists());
    let closed = saved(&id);
    assert!(!closed.open);
    assert_eq!(closed.last_status, "completed");
    assert_eq!(
        call::<SessionSnapshot>(&id, &Request::GetSnapshot)
            .unwrap_err()
            .code,
        "unavailable"
    );
}
