use crate::session_api::{
    ApiError, EntryCursor, EventCursor, PlanMode, PreviewCursor, RunRef, SessionApi,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// MAX_LINE bounds one request; messages fit the Session API's own 10 MiB limit.
const MAX_LINE: u64 = 11 << 20;

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn stop(_: libc::c_int) {
    STOP.store(true, Ordering::Release);
}

/// Request is one line a client sends to a served Session: a Session API call by name.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum Request {
    GetSnapshot,
    /// SendMessage admits a message; `note` is the host's word to the model about it.
    SendMessage {
        request_id: String,
        text: String,
        note: Option<String>,
    },
    Ask {
        request_id: String,
        question: String,
    },
    Steer {
        request_id: String,
        run: RunRef,
        text: String,
    },
    GetRun {
        run: RunRef,
    },
    WaitRun {
        run: RunRef,
    },
    CancelRun {
        run: RunRef,
    },
    ReadEntries {
        cursor: Option<EntryCursor>,
        limit: usize,
    },
    ReadEvents {
        cursor: EventCursor,
        limit: usize,
        wait_ms: u64,
    },
    ReadPreview {
        run: RunRef,
        cursor: Option<PreviewCursor>,
        wait_ms: u64,
    },
}

/// Socket_path is where a served Session listens: `socket` in its private folder.
pub fn socket_path(session_id: &str) -> Result<PathBuf, ApiError> {
    crate::checkpoint::directory(session_id)
        .map(|dir| dir.join("socket"))
        .map_err(|e| api_error("invalid_argument", e))
}

/// Call sends one request to a served Session and returns its result. Each call has its own
/// connection, so a call that waits never holds up another.
pub fn call<T: DeserializeOwned>(session_id: &str, request: &Request) -> Result<T, ApiError> {
    let path = socket_path(session_id)?;
    let unavailable = |what: &str, e: std::io::Error| {
        api_error(
            "unavailable",
            format!(
                "{what} Session {session_id}'s socket: {}",
                crate::errors::stable_io(&e)
            ),
        )
    };
    let mut stream = UnixStream::connect(&path).map_err(|e| unavailable("connect to", e))?;
    let mut line = serde_json::to_vec(request).unwrap();
    line.push(b'\n');
    stream
        .write_all(&line)
        .map_err(|e| unavailable("write to", e))?;
    let mut reply = String::new();
    BufReader::new(stream)
        .read_line(&mut reply)
        .map_err(|e| unavailable("read from", e))?;
    let reply: Value = serde_json::from_str(&reply).map_err(|_| {
        api_error(
            "unavailable",
            format!("Session {session_id} closed its socket without answering"),
        )
    })?;
    if let Some(error) = reply.get("error") {
        return Err(api_error(
            code(error["code"].as_str().unwrap_or("")),
            error["message"].as_str().unwrap_or("").to_owned(),
        ));
    }
    serde_json::from_value(reply["ok"].clone()).map_err(|e| {
        api_error(
            "unavailable",
            format!("Session {session_id} answered in an unexpected shape: {e}"),
        )
    })
}

/// Serve answers requests for `api` on its socket until the Session has had no Run, control or
/// request for `idle`, or SIGINT or SIGTERM arrives; then it cancels what is live and closes.
/// It prints one JSON line naming the Session and its socket once it is listening.
pub fn serve(api: SessionApi, idle: Duration) -> Result<(), String> {
    let id = api
        .get_snapshot()
        .map_err(|e| e.message)?
        .session
        .session_id;
    let path = socket_path(&id).map_err(|e| e.message)?;
    // This process holds the Session's lease, so a socket file left here is a dead owner's.
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).map_err(|e| {
        format!(
            "listen on {}: {}",
            path.display(),
            crate::errors::stable_io(&e)
        )
    })?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("protect the socket: {}", crate::errors::stable_io(&e)))?;
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("listen: {}", crate::errors::stable_io(&e)))?;
    unsafe {
        libc::signal(libc::SIGINT, stop as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, stop as *const () as libc::sighandler_t);
    }
    let ready = json!({"session_id": id, "socket": path});
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{ready}").and_then(|_| stdout.flush());
    drop(stdout);
    let busy = Arc::new(AtomicUsize::new(0));
    let last = Arc::new(Mutex::new(Instant::now()));
    while !STOP.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, _)) => {
                let (api, busy, last) = (api.clone(), busy.clone(), last.clone());
                std::thread::spawn(move || answer(stream, &api, &busy, &last));
                continue;
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(format!("accept: {}", crate::errors::stable_io(&e))),
        }
        std::thread::sleep(Duration::from_millis(50));
        let snapshot = api.get_snapshot().map_err(|e| e.message)?;
        if !snapshot.live_runs.is_empty() || snapshot.active_control_request_id.is_some() {
            *last.lock().unwrap() = Instant::now();
        }
        if busy.load(Ordering::Acquire) == 0 && last.lock().unwrap().elapsed() >= idle {
            break;
        }
    }
    let _ = std::fs::remove_file(&path);
    let snapshot = api.get_snapshot().map_err(|e| e.message)?;
    for run in &snapshot.live_runs {
        let _ = api.cancel_run(&run.run);
    }
    for run in &snapshot.live_runs {
        let _ = api.wait_run(&run.run);
    }
    api.close().map(|_| ()).map_err(|e| e.message)
}

/// Answer serves one connection, a request and its result per line, until the client leaves.
fn answer(stream: UnixStream, api: &SessionApi, busy: &AtomicUsize, last: &Mutex<Instant>) {
    let _ = stream.set_nonblocking(false);
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(stream);
    loop {
        let mut line = String::new();
        match (&mut reader).take(MAX_LINE).read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        busy.fetch_add(1, Ordering::AcqRel);
        let reply = match serde_json::from_str::<Request>(&line) {
            Ok(request) => match dispatch(api, request) {
                Ok(value) => json!({"ok": value}),
                Err(e) => json!({"error": {"code": e.code, "message": e.message}}),
            },
            Err(e) => {
                json!({"error": {"code": "invalid_argument", "message": format!("not a request: {e}")}})
            }
        };
        *last.lock().unwrap() = Instant::now();
        busy.fetch_sub(1, Ordering::AcqRel);
        let mut reply = reply.to_string();
        reply.push('\n');
        if writer.write_all(reply.as_bytes()).is_err() {
            return;
        }
    }
}

fn value<T: Serialize>(result: Result<T, ApiError>) -> Result<Value, ApiError> {
    result.map(|v| serde_json::to_value(v).unwrap())
}

fn dispatch(api: &SessionApi, request: Request) -> Result<Value, ApiError> {
    match request {
        Request::GetSnapshot => value(api.get_snapshot()),
        Request::SendMessage {
            request_id,
            text,
            note,
        } => value(match note {
            Some(note) => {
                api.send_message_with_note(&request_id, text.as_bytes(), &note, PlanMode::Keep)
            }
            None => api.send_message(&request_id, text.as_bytes(), PlanMode::Keep),
        }),
        Request::Ask {
            request_id,
            question,
        } => value(api.ask(&request_id, question.as_bytes())),
        Request::Steer {
            request_id,
            run,
            text,
        } => value(api.steer(&request_id, &run, text.as_bytes())),
        Request::GetRun { run } => value(api.get_run(&run)),
        Request::WaitRun { run } => value(api.wait_run(&run)),
        Request::CancelRun { run } => value(api.cancel_run(&run)),
        Request::ReadEntries { cursor, limit } => value(api.read_entries(cursor, limit)),
        Request::ReadEvents {
            cursor,
            limit,
            wait_ms,
        } => value(api.read_events(cursor, limit, wait_ms, None)),
        Request::ReadPreview {
            run,
            cursor,
            wait_ms,
        } => value(api.read_preview(&run, cursor, wait_ms, None)),
    }
}

fn api_error(code: &'static str, message: impl Into<String>) -> ApiError {
    ApiError {
        code,
        message: message.into(),
    }
}

/// Code restores an error code read off the wire to the API's own static vocabulary.
fn code(name: &str) -> &'static str {
    const CODES: &[&str] = &[
        "invalid_argument",
        "not_found",
        "busy",
        "blocked",
        "conflict",
        "cancelled",
        "closed",
        "expired",
        "not_pending",
        "cursor_expired",
        "context_too_large",
        "unsafe_context",
        "persistence_failure",
        "resource_exhausted",
        "permission_required",
    ];
    CODES
        .iter()
        .find(|code| **code == name)
        .copied()
        .unwrap_or("unavailable")
}
