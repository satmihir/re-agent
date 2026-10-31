use base64::{engine::general_purpose::STANDARD, Engine};
use serde::Deserialize;
use serde_json::{json, value::RawValue, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::fd::{AsRawFd, IntoRawFd};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

mod legacy;

static LOCKS: OnceLock<Mutex<HashMap<String, File>>> = OnceLock::new();

#[cfg(test)]
thread_local! {
    static CLOSE_ERROR: std::cell::Cell<Option<i32>> = const { std::cell::Cell::new(None) };
}

/// Directory returns the private cache location of a validated session ID.
pub fn directory(id: &str) -> Result<PathBuf, String> {
    if id.len() != 16
        || !id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(format!(
            "invalid session ID {id:?}: expected 16 lowercase hex digits"
        ));
    }
    let root = crate::cache::directory().ok_or("user cache directory is unavailable")?;
    Ok(root.join("reagent/sessions").join(id))
}

fn private(path: &std::path::Path, create: bool) -> Result<(), String> {
    if create {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
            .map_err(|e| format!("create session directory: {}", crate::errors::stable_io(&e)))?;
    }
    let info = fs::symlink_metadata(path).map_err(|e| {
        format!(
            "inspect session directory: {}",
            crate::errors::stable_io(&e)
        )
    })?;
    if !info.is_dir() || info.permissions().mode() & 0o777 != 0o700 {
        return Err(format!(
            "session directory {} must be private (0700) and not a symlink",
            path.display()
        ));
    }
    Ok(())
}

fn open(id: &str, create: bool) -> Result<PathBuf, String> {
    let dir = directory(id)?;
    let mut locks = LOCKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap();
    if locks.contains_key(id) {
        return Ok(dir);
    }
    private(dir.parent().unwrap(), create).map_err(|e| format!("open session store: {e}"))?;
    if create {
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .map_err(|e| format!("create session {id}: {}", crate::errors::stable_io(&e)))?;
    }
    private(&dir, false).map_err(|e| format!("open session {id}: {e}"))?;
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(dir.join("lock"))
        .map_err(|e| format!("open session lock: {}", crate::errors::stable_io(&e)))?;
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(format!("session {id} is already open or cannot be locked"));
    }
    locks.insert(id.into(), lock);
    Ok(dir)
}

/// Close releases the process's lease on a session checkpoint.
pub fn close(id: &str) {
    if let Some(locks) = LOCKS.get() {
        locks.lock().unwrap().remove(id);
    }
}

fn ordered(value: &Value, fields: &[&str]) -> Value {
    let mut map = Map::new();
    for key in fields {
        if let Some(v) = value.get(*key) {
            map.insert((*key).into(), v.clone());
        }
    }
    Value::Object(map)
}

fn sort_map(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let sorted: BTreeMap<_, _> =
                map.iter().map(|(k, v)| (k.clone(), sort_map(v))).collect();
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(a) => Value::Array(a.iter().map(sort_map).collect()),
        _ => value.clone(),
    }
}

fn usage(v: &Value) -> Value {
    let mut result = json!({"known":v["known"].as_bool().unwrap_or(false),"input_tokens":v["input_tokens"].as_i64().unwrap_or(0),"cached_input_tokens":v["cached_input_tokens"].as_i64().unwrap_or(0),"output_tokens":v["output_tokens"].as_i64().unwrap_or(0),"reasoning_tokens":v["reasoning_tokens"].as_i64().unwrap_or(0)});
    if v["unreported_attempts"].as_i64().unwrap_or(0) != 0 {
        result["unreported_attempts"] = v["unreported_attempts"].clone();
    }
    result
}

fn raw(value: &mut Value, decode: bool) -> Result<(), String> {
    if value.is_null() {
        return Ok(());
    }
    if decode {
        let bytes = value
            .as_str()
            .and_then(|s| STANDARD.decode(s).ok())
            .ok_or("checkpoint raw data must be a base64 string or null")?;
        *value = crate::raw::new(&bytes);
    } else {
        let bytes = crate::raw::bytes(value).unwrap_or_else(|| crate::types::json_bytes(value));
        *value = Value::String(STANDARD.encode(bytes));
    }
    Ok(())
}

fn user(value: &mut Value, decode: bool) -> Result<(), String> {
    for key in ["workspace", "roster"] {
        if let Some(v) = value.get_mut(key) {
            raw(v, decode)?;
        }
    }
    if !decode {
        *value = ordered(value, &["text", "workspace", "roster", "source", "plan"]);
    }
    Ok(())
}

fn outcome(value: &mut Value, decode: bool) -> Result<(), String> {
    if let Some(v) = value.get_mut("data") {
        raw(v, decode)?;
    }
    if !decode {
        *value = ordered(
            value,
            &[
                "workspace",
                "ok",
                "code",
                "message",
                "data",
                "truncated",
                "effect",
            ],
        );
    }
    Ok(())
}

fn convert_raw(cp: &mut Value, decode: bool) -> Result<(), String> {
    if let Some(entries) = cp["history"].as_array_mut() {
        for e in entries {
            if let Some(u) = e.get_mut("user").filter(|u| !u.is_null()) {
                user(u, decode)?;
            }
            if let Some(a) = e.get_mut("assistant").filter(|a| !a.is_null()) {
                if let Some(items) = a
                    .get_mut("native")
                    .and_then(|n| n.get_mut("items"))
                    .and_then(Value::as_array_mut)
                {
                    for i in items {
                        raw(i, decode)?;
                    }
                }
                if !decode {
                    a["native"] = ordered(&a["native"], &["provider", "items"]);
                    a["usage"] = usage(&a["usage"]);
                    if let Some(blocks) = a["blocks"].as_array_mut() {
                        for b in blocks {
                            if let Some(c) = b.get_mut("call") {
                                *c = ordered(c, &["call_id", "name", "arguments"]);
                            }
                            *b = ordered(b, &["kind", "text", "call"]);
                        }
                    }
                    *a = ordered(a, &["response_id", "model", "blocks", "native", "usage"]);
                }
            }
            if let Some(t) = e.get_mut("tool").filter(|t| !t.is_null()) {
                outcome(&mut t["outcome"], decode)?;
                if !decode {
                    *t = ordered(t, &["call_id", "name", "outcome"]);
                }
            }
            if !decode {
                *e = ordered(
                    e,
                    &["kind", "user", "assistant", "tool", "shell", "summary"],
                );
            }
        }
    }
    if let Some(u) = cp.get_mut("pending_submission").filter(|u| !u.is_null()) {
        user(u, decode)?;
    }
    if let Some(map) = cp
        .get_mut("gc_state")
        .and_then(|g| g.get_mut("inherited"))
        .and_then(Value::as_object_mut)
    {
        for (_, v) in map {
            outcome(v, decode)?;
        }
    }
    Ok(())
}

const FIELDS: &[&str] = &[
    "version",
    "id",
    "provider",
    "model",
    "effort",
    "launch",
    "active",
    "read_only",
    "plan",
    "no_project_instructions",
    "project_instructions",
    "launch_instructions",
    "max_steps",
    "max_tool_calls",
    "model_retry_window",
    "in_run_compact",
    "context_window_tokens",
    "context_gc_age_steps",
    "context_gc_consolidate",
    "context_gc",
    "gc_state",
    "gc_requests",
    "agents",
    "agent_roster",
    "agent_subtree",
    "report_friction",
    "auto",
    "auto_enabled",
    "auto_fallback_model",
    "auto_fallback_effort",
    "auto_usage",
    "auto_attempts",
    "auto_compact_off",
    "history",
    "pending_submission",
    "seen",
    "handoff",
    "compacted_plan",
    "blocked",
    "last_trace",
    "last_request",
    "tokens_per_byte",
    "usage",
    "phase",
    "in_flight",
    "prompt",
];

fn state(session: &crate::runtime::Session, phase: &str, in_flight: &str) -> Value {
    let c = &session.config;
    let s = &session.state;
    let mut cp = Map::new();
    for key in FIELDS {
        let default = match *key {
            "version" => json!(2),
            "id" => c.get("session_id").unwrap_or(&s["session_id"]).clone(),
            "effort" => c["reasoning_effort"].clone(),
            "launch" => s
                .get("launch")
                .cloned()
                .unwrap_or_else(|| c["workspace"].clone()),
            "active" => c["workspace"].clone(),
            "launch_instructions" => c["project_instructions"].clone(),
            "history" => {
                let restored_array = s["id"] == s["session_id"] && s["history"].is_array();
                if session.history.is_empty() && !restored_array {
                    Value::Null
                } else {
                    json!(session.history)
                }
            }
            "phase" => json!(phase),
            "in_flight" => json!(in_flight),
            "gc_state" => s
                .get("gc")
                .cloned()
                .unwrap_or_else(|| json!({"last_pass":0})),
            "seen" => json!({}),
            "auto_usage" | "last_request" => usage(&Value::Null),
            "usage" => usage(&json!({"known":true})),
            "tokens_per_byte" => json!(0),
            "compacted_plan" | "blocked" | "last_trace" => json!(""),
            "max_steps" | "max_tool_calls" | "model_retry_window" | "auto_attempts" => json!(0),
            "read_only"
            | "plan"
            | "no_project_instructions"
            | "in_run_compact"
            | "context_gc"
            | "agents"
            | "report_friction"
            | "auto"
            | "auto_enabled"
            | "auto_compact_off" => json!(false),
            _ => Value::Null,
        };
        let v = if [
            "version",
            "id",
            "history",
            "phase",
            "in_flight",
            "effort",
            "active",
        ]
        .contains(key)
        {
            default
        } else {
            let config_owned = [
                "provider",
                "model",
                "read_only",
                "no_project_instructions",
                "project_instructions",
                "max_steps",
                "max_tool_calls",
                "model_retry_window",
                "in_run_compact",
                "context_window_tokens",
                "context_gc_age_steps",
                "context_gc_consolidate",
                "context_gc",
                "agents",
                "report_friction",
                "prompt",
            ]
            .contains(key);
            if config_owned {
                c.get(*key).or_else(|| s.get(*key))
            } else {
                s.get(*key).or_else(|| c.get(*key))
            }
            .cloned()
            .unwrap_or(default)
        };
        let optional = [
            "project_instructions",
            "launch_instructions",
            "context_window_tokens",
            "context_gc_age_steps",
            "context_gc_consolidate",
            "gc_requests",
            "agent_roster",
            "agent_subtree",
            "auto_fallback_model",
            "auto_fallback_effort",
            "pending_submission",
            "handoff",
            "in_flight",
            "prompt",
        ]
        .contains(key);
        let empty = v.is_null()
            || v == json!("")
            || v == json!(0)
            || v == json!(false)
            || v.as_array().is_some_and(|a| a.is_empty());
        if !optional
            || !empty
            || (["project_instructions", "launch_instructions"].contains(key) && v.is_string())
        {
            cp.insert((*key).into(), v);
        }
    }
    let mut cp = Value::Object(cp);
    let (roster, subtree) = crate::agents::snapshot(session);
    if !roster.is_null() {
        if roster.as_array().is_some_and(|a| !a.is_empty()) {
            cp["agent_roster"] = roster;
        }
        cp["agent_subtree"] = subtree;
    }
    cp = ordered(&cp, FIELDS);
    cp["seen"] = sort_map(&cp["seen"]);
    if let Some(seen) = s.get("seen_calls") {
        cp["seen"] = sort_map(seen);
    }
    for key in ["auto_usage", "usage", "last_request"] {
        cp[key] = usage(&cp[key]);
    }
    let gc = &mut cp["gc_state"];
    for key in ["dropped", "paths", "inherited"] {
        if let Some(v) = gc.get_mut(key) {
            *v = sort_map(v);
        }
    }
    if let Some(dropped) = gc.get_mut("dropped").and_then(Value::as_object_mut) {
        for value in dropped.values_mut() {
            *value = ordered(value, &["reason", "step", "placeholder"]);
        }
    }
    *gc = ordered(gc, &["dropped", "paths", "last_pass", "inherited"]);
    cp
}

fn canonical(v: &Value) -> Vec<u8> {
    crate::types::canonical_json(v).into_bytes()
}

fn envelope(cp: &Value) -> Result<Vec<u8>, String> {
    let mut persisted = cp.clone();
    convert_raw(&mut persisted, false)?;
    if let Some(roster) = persisted
        .get_mut("agent_roster")
        .and_then(Value::as_array_mut)
    {
        for record in roster {
            *record = crate::agents::thread_record(record);
            record.as_object_mut().unwrap().remove("depth");
        }
    }
    if let Some(subtree) = persisted.get_mut("agent_subtree").filter(|v| v.is_object()) {
        if let Some(records) = subtree.get_mut("threads").and_then(Value::as_array_mut) {
            for record in records {
                *record = crate::agents::thread_record(record);
            }
        }
        if let Some(cost) = subtree.get_mut("total_cost") {
            *cost = usage(cost);
        }
        *subtree = ordered(
            subtree,
            &[
                "threads",
                "total_cost",
                "total_agents",
                "next_offset",
                "accounting_complete",
            ],
        );
    }
    let bytes = canonical(&persisted);
    let digest = format!("{:x}", Sha256::digest(&bytes));
    let state = String::from_utf8(bytes).unwrap();
    Ok(format!(
        "{{\"version\":2,\"id\":{},\"state\":{},\"sha256\":{}}}",
        serde_json::to_string(&cp["id"]).unwrap(),
        state,
        serde_json::to_string(&digest).unwrap()
    )
    .into_bytes())
}

fn close_checkpoint(file: File) -> std::io::Result<()> {
    // Close consumes the descriptor even on error; retrying could close a reused FD.
    let error = if unsafe { libc::close(file.into_raw_fd()) } != 0 {
        Some(std::io::Error::last_os_error())
    } else {
        None
    };
    #[cfg(test)]
    let error = CLOSE_ERROR
        .with(|fault| fault.take())
        .map(std::io::Error::from_raw_os_error)
        .or(error);
    match error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// Save persists an execution barrier without replaying any pending work.
pub fn save(session: &crate::runtime::Session, phase: &str, in_flight: &str) -> Result<(), String> {
    if !session.config["checkpoint_enabled"]
        .as_bool()
        .unwrap_or(false)
        || session.config["scripted"]
            .as_str()
            .is_some_and(|x| !x.is_empty())
        || session.config["agent"].as_bool().unwrap_or(false)
    {
        return Ok(());
    }
    let cp = state(session, phase, in_flight);
    let id = cp["id"].as_str().ok_or("session ID is missing")?;
    let dir = open(id, !directory(id)?.exists())?;
    let body = envelope(&cp)?;
    let staged = dir.join(format!(".checkpoint-{}", crate::types::new_id()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&staged)
            .map_err(|e| format!("stage checkpoint: {}", crate::errors::stable_io(&e)))?;
        file.write_all(&body)
            .map_err(|e| format!("write checkpoint: {}", crate::errors::stable_io(&e)))?;
        file.sync_all()
            .map_err(|e| format!("sync checkpoint: {}", crate::errors::stable_io(&e)))?;
        close_checkpoint(file)
            .map_err(|e| format!("close checkpoint: {}", crate::errors::stable_io(&e)))?;
        let destination = dir.join("checkpoint.json");
        // os.Rename rejects a directory destination before its rename syscall.
        if fs::symlink_metadata(&destination).is_ok_and(|metadata| metadata.is_dir()) {
            return Err("replace checkpoint: path already exists".into());
        }
        fs::rename(&staged, destination)
            .map_err(|e| format!("replace checkpoint: {}", crate::errors::stable_io(&e)))?;
        File::open(&dir).and_then(|f| f.sync_all()).map_err(|e| {
            format!(
                "sync checkpoint directory: {}",
                crate::errors::stable_io(&e)
            )
        })
    })();
    let _ = fs::remove_file(staged);
    result.map_err(|e| format!("save session {id}: {e}"))
}

#[derive(Deserialize)]
struct Envelope<'a> {
    version: i64,
    id: String,
    #[serde(borrow)]
    state: &'a RawValue,
    sha256: String,
}

// Match Go's declared JSON types while allowing null zero values and unknown fields.
// RawMessage fields are checked by convert_raw after the containing shapes are valid.
fn valid_checkpoint_type(value: &mut Value, kind: &str) -> bool {
    if value.is_null() {
        return true;
    }
    if let Some(element) = kind.strip_prefix("[]") {
        return value.as_array_mut().is_some_and(|items| {
            items
                .iter_mut()
                .all(|item| valid_checkpoint_type(item, element))
        });
    }
    if let Some(element) = kind.strip_prefix("map:") {
        return value.as_object_mut().is_some_and(|items| {
            items
                .values_mut()
                .all(|item| valid_checkpoint_type(item, element))
        });
    }
    match kind {
        "string" => return value.is_string(),
        "bool" => return value.is_boolean(),
        "int" => return value.as_i64().is_some(),
        "float" => return value.as_f64().is_some_and(f64::is_finite),
        "raw" => return true,
        _ => {}
    }
    let fields = checkpoint_fields(kind);
    let Some(object) = value.as_object_mut() else {
        return false;
    };
    // Go matches tagged struct fields case-insensitively, unlike map keys.
    let mut normalized = Map::new();
    for (name, mut item) in std::mem::take(object) {
        let field = fields.split_whitespace().find_map(|field| {
            let (key, kind) = field.split_once(':').unwrap();
            key.eq_ignore_ascii_case(&name).then_some((key, kind))
        });
        if let Some((key, kind)) = field {
            if !valid_checkpoint_type(&mut item, kind) {
                return false;
            }
            normalized.insert(key.into(), item);
        } else {
            normalized.insert(name, item);
        }
    }
    *object = normalized;
    true
}

fn checkpoint_fields(kind: &str) -> &'static str {
    match kind {
        "checkpoint" => "version:int id:string provider:string model:string effort:string \
            launch:string active:string read_only:bool plan:bool no_project_instructions:bool \
            project_instructions:string launch_instructions:string max_steps:int max_tool_calls:int \
            model_retry_window:int in_run_compact:bool context_window_tokens:int \
            context_gc_age_steps:int context_gc_consolidate:bool context_gc:bool gc_state:gc \
            gc_requests:int agents:bool agent_roster:[]thread agent_subtree:subtree \
            report_friction:bool auto:bool auto_enabled:bool auto_fallback_model:string \
            auto_fallback_effort:string auto_usage:usage auto_attempts:int auto_compact_off:bool \
            history:[]entry pending_submission:user seen:map:bool handoff:handoff \
            compacted_plan:string blocked:string last_trace:string last_request:usage \
            tokens_per_byte:float usage:usage phase:string in_flight:string prompt:prompt",
        "entry" => "kind:string user:user assistant:response tool:tool shell:shell summary:summary",
        "user" => "text:string workspace:raw roster:raw source:string plan:string",
        "response" => "response_id:string model:string blocks:[]block native:native usage:usage",
        "block" => "kind:string text:string call:call",
        "call" => "call_id:string name:string arguments:string",
        "native" => "provider:string items:[]raw",
        "usage" => "known:bool input_tokens:int cached_input_tokens:int output_tokens:int \
            reasoning_tokens:int unreported_attempts:int",
        "tool" => "call_id:string name:string outcome:outcome",
        "outcome" => "workspace:string ok:bool code:string message:string data:raw \
            truncated:bool effect:string",
        "shell" => "workspace:string kind:string command:string exit_code:int signal:string \
            interrupted:bool output:string output_bytes_seen:int output_truncated:bool \
            encoding_replaced:bool duration_ms:int",
        "summary" => "text:string replaced_entries:int model:string",
        "gc" => "dropped:map:drop paths:map:string last_pass:int inherited:map:outcome",
        "drop" => "reason:string step:int placeholder:string",
        "handoff" => "text:string plan:string entries:int native_items:int",
        "prompt" => "text:string project:string",
        "thread" => "id:string parent_id:string name:string state:string task:string steps:int \
            cost:usage trace_path:string authority:string context_mode:string effects_unknown:bool",
        "observation" => "id:string parent_id:string name:string state:string task:string steps:int \
            cost:usage trace_path:string authority:string context_mode:string effects_unknown:bool depth:int",
        "subtree" => "threads:[]observation total_cost:usage total_agents:int next_offset:int accounting_complete:bool",
        _ => unreachable!("unknown checkpoint type"),
    }
}

fn parse(id: &str, bytes: &[u8]) -> Result<Value, String> {
    let corrupt = |why: &str| format!("corrupt session {id}: {why}");
    let env: Envelope =
        serde_json::from_slice(bytes).map_err(|_| corrupt("invalid checkpoint JSON"))?;
    if env.version != 1 && env.version != 2 {
        return Err(format!(
            "session {id} has unsupported checkpoint version {}",
            env.version
        ));
    }
    if env.id != id {
        return Err(corrupt("invalid checkpoint envelope"));
    }
    let legacy_bytes;
    let payload = if env.version == 1 {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Bytes {
            Base64(String),
            Array(Vec<Option<u8>>),
            Null,
        }
        let encoded: Bytes = serde_json::from_str(env.state.get())
            .map_err(|_| corrupt("invalid legacy checkpoint payload"))?;
        legacy_bytes = match encoded {
            Bytes::Base64(encoded) => {
                // Go's standard base64 decoder ignores unused trailing bits.
                let decoder = base64::engine::GeneralPurpose::new(
                    &base64::alphabet::STANDARD,
                    base64::engine::general_purpose::GeneralPurposeConfig::new()
                        .with_decode_allow_trailing_bits(true),
                );
                decoder
                    .decode(encoded.replace(['\r', '\n'], ""))
                    .map_err(|_| corrupt("invalid legacy checkpoint payload"))?
            }
            Bytes::Array(bytes) => bytes.into_iter().map(|b| b.unwrap_or(0)).collect(),
            Bytes::Null => Vec::new(),
        };
        legacy_bytes.as_slice()
    } else {
        env.state.get().as_bytes()
    };
    if env.sha256 != format!("{:x}", Sha256::digest(payload)) {
        return Err(corrupt("checkpoint checksum mismatch"));
    }
    let mut cp: Value = if env.version == 1 {
        legacy::decode(payload).map_err(|_| corrupt("invalid legacy checkpoint state"))?
    } else {
        serde_json::from_slice(payload).map_err(|_| corrupt("invalid checkpoint state"))?
    };
    if !cp.is_object() {
        return Err(corrupt("invalid checkpoint state"));
    }
    // The gob decoder already checks the schema and may retain invalid string bytes
    // in internal wrappers, which are deliberately not accepted as v2 JSON strings.
    if env.version == 2 && !valid_checkpoint_type(&mut cp, "checkpoint") {
        return Err(corrupt("invalid checkpoint state"));
    }
    convert_raw(&mut cp, true).map_err(|e| corrupt(&e))?;
    if cp["version"] != env.version {
        return Err(corrupt("mismatched version"));
    }
    if cp["id"] != id
        || ["launch", "provider", "model"]
            .iter()
            .any(|k| cp[k].as_str().unwrap_or("").is_empty())
        || [
            "max_steps",
            "max_tool_calls",
            "model_retry_window",
            "context_window_tokens",
        ]
        .iter()
        .any(|k| cp[k].as_i64().unwrap_or(0) < 0)
    {
        return Err(corrupt("invalid configuration"));
    }
    let phase = cp["phase"].as_str().unwrap_or("");
    if !["idle", "model", "accepted", "tool", "terminal"].contains(&phase) {
        return Err(corrupt("unknown checkpoint phase"));
    }
    let mut pending: HashMap<String, String> = HashMap::new();
    let mut accepted = HashSet::new();
    for (i, e) in cp["history"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .enumerate()
    {
        let kind = e["kind"].as_str().unwrap_or("");
        let keys: Vec<_> = ["user", "assistant", "tool", "shell", "summary"]
            .into_iter()
            .filter(|k| !e[k].is_null())
            .collect();
        if keys.len() != 1
            || (["user", "assistant", "tool", "shell", "summary"].contains(&kind)
                && keys[0] != kind)
        {
            return Err(corrupt(&format!("entry {}", i + 1)));
        }
        if let Some(blocks) = e["assistant"]["blocks"].as_array() {
            for b in blocks {
                if b["kind"] != "tool_call" {
                    continue;
                }
                let call = &b["call"];
                let cid = call["call_id"].as_str().unwrap_or("");
                let name = call["name"].as_str().unwrap_or("");
                if cid.is_empty()
                    || name.is_empty()
                    || !accepted.insert(cid.to_string())
                    || cp["seen"][cid] != true
                {
                    return Err(corrupt(&format!("invalid call in entry {}", i + 1)));
                }
                pending.insert(cid.into(), name.into());
            }
        }
        if !e["tool"].is_null() {
            let t = &e["tool"];
            let cid = t["call_id"].as_str().unwrap_or("");
            if pending.remove(cid).as_deref() != t["name"].as_str() {
                return Err(corrupt(&format!("unmatched result in entry {}", i + 1)));
            }
        }
    }
    let flight = cp["in_flight"].as_str().unwrap_or("");
    if phase == "tool" && !pending.contains_key(flight) {
        return Err(corrupt("missing in-flight call"));
    }
    if phase != "tool" && !flight.is_empty() {
        return Err(corrupt("unexpected in-flight call"));
    }
    if !pending.is_empty() && phase != "tool" && phase != "accepted" {
        return Err(corrupt("unresolved calls outside an accepted batch"));
    }
    if let Some(n) = cp["handoff"]["entries"].as_i64() {
        if n < 0 || n as usize > cp["history"].as_array().map_or(0, Vec::len) {
            return Err(corrupt("handoff"));
        }
    }
    Ok(cp)
}

/// Load validates a v2 JSON or legacy v1 gob checkpoint and returns normalized runtime state.
pub fn load(id: &str) -> Result<Value, String> {
    let dir = open(id, false)?;
    let path = dir.join("checkpoint.json");
    let info = fs::symlink_metadata(&path)
        .map_err(|e| format!("session {id} checkpoint: {}", crate::errors::stable_io(&e)))?;
    if !info.is_file() || info.permissions().mode() & 0o777 != 0o600 {
        return Err(format!(
            "session {id} checkpoint is not a private regular file"
        ));
    }
    let bytes = fs::read(path)
        .map_err(|e| format!("read session {id}: {}", crate::errors::stable_io(&e)))?;
    let cp = parse(id, &bytes)?;
    Ok(restored(id, cp))
}

fn restored(id: &str, cp: Value) -> Value {
    let mut config = Map::new();
    for key in [
        "provider",
        "model",
        "read_only",
        "plan",
        "no_project_instructions",
        "project_instructions",
        "max_steps",
        "max_tool_calls",
        "model_retry_window",
        "in_run_compact",
        "context_window_tokens",
        "context_gc_age_steps",
        "context_gc_consolidate",
        "context_gc",
        "agents",
        "report_friction",
        "prompt",
    ] {
        if let Some(v) = cp.get(key) {
            config.insert(key.into(), v.clone());
        }
    }
    let project = if cp["active"] == cp["launch"] {
        &cp["project_instructions"]
    } else {
        &cp["launch_instructions"]
    };
    config.insert("project_instructions".into(), project.clone());
    config.insert("session_id".into(), json!(id));
    config.insert("reasoning_effort".into(), cp["effort"].clone());
    config.insert("workspace".into(), cp["launch"].clone());
    config.insert("checkpoint_enabled".into(), json!(true));
    let mut s = cp.clone();
    s["launch_instructions"] = project.clone();
    s["seen_calls"] = cp["seen"].clone();
    s["session_id"] = json!(id);
    s["gc"] = cp["gc_state"].clone();
    s["recovery_workspace"] = cp["active"].clone();
    if cp["context_gc"] == true && cp["gc_requests"].as_u64().unwrap_or(0) == 0 {
        s["gc_requests"] = json!(cp["history"].as_array().map_or(0, |history| history
            .iter()
            .filter(|entry| !entry["assistant"].is_null())
            .count()));
    }
    json!({"config":config,"history":cp["history"],"state":s,"checkpoint":cp})
}

/// Recover records interrupted calls as observations, never restarted actions.
pub fn recover(session: &mut crate::runtime::Session) -> Result<String, String> {
    crate::agents::restore(session);
    let phase = session.state["phase"].as_str().unwrap_or("idle");
    if ["idle", "terminal"].contains(&phase) && session.state["pending_submission"].is_null() {
        return Ok(String::new());
    }
    let mut calls = Vec::new();
    let mut results = HashSet::new();
    for e in &session.history {
        if let Some(blocks) = e["assistant"]["blocks"].as_array() {
            for b in blocks {
                if b["kind"] == "tool_call" {
                    calls.push(b["call"].clone());
                }
            }
        }
        if let Some(id) = e["tool"]["call_id"].as_str() {
            results.insert(id.to_string());
        }
    }
    let workspace = session
        .state
        .get("recovery_workspace")
        .unwrap_or(&session.config["workspace"])
        .clone();
    let mut unknown = false;
    for call in calls {
        if results.contains(call["call_id"].as_str().unwrap_or("")) {
            continue;
        }
        let flight = call["call_id"] == session.state["in_flight"];
        unknown |= flight;
        let (code, message, effect) = if flight {
            (
                "recovery_effect_unknown",
                "recovery: call was in flight; effects are unknown; inspect before continuing",
                "unknown",
            )
        } else {
            ("not_executed", "recovery: call was not executed", "none")
        };
        session.history.push(json!({"kind":"tool","tool":{"call_id":call["call_id"],"name":call["name"],"outcome":{"workspace":workspace,"ok":false,"code":code,"message":message,"data":null,"truncated":false,"effect":effect}}}));
    }
    let pending = !session.state["pending_submission"].is_null();
    if pending {
        session
            .history
            .push(json!({"kind":"user","user":session.state["pending_submission"]}));
        session
            .state
            .as_object_mut()
            .unwrap()
            .remove("pending_submission");
    }
    session.state["blocked"] = json!("");
    session.state["phase"] = json!("terminal");
    session.state["in_flight"] = json!("");
    let mut message = "interrupted session restored; no pending model request or tool was restarted; send a new message".to_string();
    if pending {
        message.push_str(
            "; the message submitted before compaction was retained but not sent to the model",
        );
    }
    if unknown {
        message.push_str(
            "; an in-flight tool has unknown effects; inspect them before further changes",
        );
    }
    save(session, "terminal", "")?;
    Ok(message)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn cp() -> Value {
        json!({"version":2,"id":"0123456789abcdef","provider":"openai","model":"test","launch":"/tmp","max_steps":0,"max_tool_calls":0,"model_retry_window":0,"gc_state":{"last_pass":0},"history":[{"kind":"user","user":{"text":"<hello>","workspace":{"kind":"workspace_state"}}}],"seen":{},"phase":"terminal"})
    }
    fn checked_envelope(state: &Value) -> Vec<u8> {
        let bytes = canonical(state);
        let digest = format!("{:x}", Sha256::digest(&bytes));
        format!(
            "{{\"version\":2,\"id\":\"0123456789abcdef\",\"state\":{},\"sha256\":\"{digest}\"}}",
            String::from_utf8(bytes).unwrap()
        )
        .into_bytes()
    }

    #[test]
    fn restored_project_uses_launch_text_but_keeps_recorded_prompt() {
        for active in ["/tmp", "/other"] {
            for launch in [Value::Null, json!(""), json!("launch project")] {
                let mut original = cp();
                original["active"] = json!(active);
                original["project_instructions"] = json!("active project");
                original["launch_instructions"] = launch.clone();
                original["prompt"] = json!({"text":"recorded prompt","project":"recorded project"});
                let saved = restored("0123456789abcdef", original.clone());
                let expected = if active == "/tmp" {
                    json!("active project")
                } else {
                    launch
                };
                assert_eq!(saved["config"]["project_instructions"], expected);
                assert_eq!(saved["state"]["launch_instructions"], expected);
                assert_eq!(saved["config"]["workspace"], "/tmp");
                assert_eq!(saved["state"]["recovery_workspace"], active);
                assert_eq!(saved["config"]["prompt"], original["prompt"]);
                let request =
                    crate::context::build_context(&saved["config"], json!({}), &[], json!([]));
                assert_eq!(request["instructions"], "recorded prompt");
            }
        }
    }

    #[test]
    fn typed_state_accepts_go_nulls_and_unknown_fields() {
        let mut persisted: Value = serde_json::from_slice(&envelope(&cp()).unwrap()).unwrap();
        let state = &mut persisted["state"];
        state["read_only"] = Value::Null;
        state["history"][0]["user"]["text"] = Value::Null;
        state["history"][0]["tool"] = Value::Null;
        state["history"][0]["assistant"] = Value::Null;
        state["usage"] = json!({"known":null,"input_tokens":null,"future":[]});
        state["future"] = json!({"anything":true});
        parse("0123456789abcdef", &checked_envelope(state)).unwrap();
        state["history"] = Value::Null;
        state.as_object_mut().unwrap().remove("read_only");
        state["READ_ONLY"] = json!(true);
        let restored = parse("0123456789abcdef", &checked_envelope(state)).unwrap();
        assert_eq!(restored["read_only"], true);
        assert!(restored["history"].is_null());
    }

    #[test]
    fn missing_private_directory_uses_owned_filesystem_error() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(
            private(&root.path().join("missing"), false).unwrap_err(),
            "inspect session directory: path does not exist"
        );
    }

    #[test]
    fn canonical_envelope_roundtrips_and_encodes_raw() {
        let original = cp();
        let bytes = envelope(&original).unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();
        assert!(!text.ends_with('\n'));
        assert!(text.contains("<hello>"));
        assert!(text.contains("eyJraW5kIjoid29ya3NwYWNlX3N0YXRlIn0="));
        let restored = parse("0123456789abcdef", &bytes).unwrap();
        assert_eq!(
            crate::raw::parse(&restored["history"][0]["user"]["workspace"]).unwrap(),
            original["history"][0]["user"]["workspace"]
        );
        assert_eq!(envelope(&restored).unwrap(), bytes);
    }
    #[test]
    fn corrupt_checksum_is_not_accepted() {
        let bytes = envelope(&cp()).unwrap();
        let text = String::from_utf8(bytes)
            .unwrap()
            .replace("<hello>", "<other>");
        assert!(parse("0123456789abcdef", text.as_bytes())
            .unwrap_err()
            .ends_with("checkpoint checksum mismatch"));
    }
    #[test]
    fn imported_raw_json_retains_whitespace_and_opaque_bytes() {
        let mut persisted = cp();
        let bytes = b"{ \"kind\" : \"workspace_state\", \"opaque\":\"\xff\" }";
        persisted["history"][0]["user"]["workspace"] = json!(STANDARD.encode(bytes));
        let mut normalized = persisted.clone();
        convert_raw(&mut normalized, true).unwrap();
        let encoded = envelope(&normalized).unwrap();
        let envelope: Value = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(
            envelope["state"]["history"][0]["user"]["workspace"],
            STANDARD.encode(bytes)
        );
    }

    #[test]
    fn opaque_native_item_survives_checkpoint_and_next_provider_request() {
        let native = b"{ \"type\" : \"reasoning\", \"id\":\"\\u0061\", \"n\":1e+02, \"n\":-0.0, \"opaque\":\"\xff\xfe\" }";
        let mut original = cp();
        original["history"].as_array_mut().unwrap().push(json!({"kind":"assistant","assistant":{"blocks":[],"native":{"provider":"openai.responses","items":[crate::raw::new(native)]},"usage":crate::types::usage_zero(true)}}));
        let saved = envelope(&original).unwrap();
        let persisted: Value = serde_json::from_slice(&saved).unwrap();
        assert_eq!(
            persisted["state"]["history"][1]["assistant"]["native"]["items"][0],
            STANDARD.encode(native)
        );
        assert!(!saved
            .windows(b"__reagent_raw_json_v1".len())
            .any(|w| w == b"__reagent_raw_json_v1"));
        let restored = parse("0123456789abcdef", &saved).unwrap();
        assert_eq!(
            crate::raw::bytes(&restored["history"][1]["assistant"]["native"]["items"][0]).unwrap(),
            native
        );
        assert_eq!(envelope(&restored).unwrap(), saved);
        let request = crate::context::build_context(
            &json!({"provider":"openai","model":"gpt-5"}),
            json!({}),
            restored["history"].as_array().unwrap(),
            json!([]),
        );
        let body = crate::openai::encode_request(&request, false).unwrap();
        assert_eq!(crate::raw::bytes(&body["input"][1]).unwrap(), native);
        let wire = crate::types::json_bytes(&body);
        let compact = b"{\"type\":\"reasoning\",\"id\":\"\\u0061\",\"n\":1e+02,\"n\":-0.0,\"opaque\":\"\xff\xfe\"}";
        assert!(wire.windows(compact.len()).any(|w| w == compact));
        assert!(!wire
            .windows(b"__reagent_raw_json_v1".len())
            .any(|w| w == b"__reagent_raw_json_v1"));
    }

    #[test]
    fn all_raw_checkpoint_fields_preserve_original_bytes_without_sidecars() {
        let bytes = b" { \"text\" : \"\\u003c\xff\", \"n\":1.00e+02 } ";
        let wrapped = crate::raw::new(bytes);
        let mut original = cp();
        original["history"][0]["user"]["workspace"] = wrapped.clone();
        original["history"][0]["user"]["roster"] = wrapped.clone();
        original["history"].as_array_mut().unwrap().push(json!({"kind":"tool","tool":{"call_id":"c","name":"read_file","outcome":{"ok":true,"code":"ok","message":"","data":wrapped,"truncated":false,"effect":"none"}}}));
        original["pending_submission"] =
            json!({"text":"pending","workspace":wrapped,"roster":wrapped});
        original["gc_state"]["inherited"] = json!({"c":{"ok":true,"code":"ok","message":"","data":wrapped,"truncated":false,"effect":"none"}});
        let mut persisted = original.clone();
        convert_raw(&mut persisted, false).unwrap();
        for path in [
            "/history/0/user/workspace",
            "/history/0/user/roster",
            "/history/1/tool/outcome/data",
            "/pending_submission/workspace",
            "/pending_submission/roster",
            "/gc_state/inherited/c/data",
        ] {
            assert_eq!(
                persisted.pointer(path).unwrap(),
                &json!(STANDARD.encode(bytes)),
                "{path}"
            );
        }
        convert_raw(&mut persisted, true).unwrap();
        assert_eq!(persisted, original);
    }

    #[test]
    fn restored_settings_redeclare_recall_and_rebuild_legacy_request_count() {
        let mut original = cp();
        original["context_gc"] = json!(true);
        original["read_only"] = json!(true);
        original["agents"] = json!(true);
        original["history"].as_array_mut().unwrap().extend([
            json!({"kind":"assistant","assistant":{"blocks":[]}}),
            json!({"kind":"assistant","assistant":{"blocks":[]}}),
        ]);
        let saved = restored("0123456789abcdef", original);
        assert_eq!(saved["config"]["context_gc"], true);
        assert_eq!(saved["config"]["checkpoint_enabled"], true);
        assert_eq!(saved["state"]["gc_requests"], 2);
        assert!(crate::tools::specs(&saved["config"])
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "recall"));
    }

    #[test]
    fn agent_checkpoint_records_use_declaration_order_and_omit_empty_fields() {
        let mut original = cp();
        let record = json!({"state":"idle","name":"worker","id":"t1","parent_id":"root","task":"inspect","steps":0,"cost":crate::types::usage_zero(true),"authority":"","effects_unknown":false,"depth":1});
        original["agent_roster"] = json!([record]);
        original["agent_subtree"] = json!({"accounting_complete":true,"total_agents":1,"total_cost":crate::types::usage_zero(true),"threads":[record]});
        let persisted: Value = serde_json::from_slice(&envelope(&original).unwrap()).unwrap();
        let roster = &persisted["state"]["agent_roster"][0];
        assert_eq!(
            crate::types::json_string(roster),
            r#"{"id":"t1","parent_id":"root","name":"worker","state":"idle","task":"inspect","steps":0,"cost":{"known":true,"input_tokens":0,"cached_input_tokens":0,"output_tokens":0,"reasoning_tokens":0}}"#
        );
        let observation = &persisted["state"]["agent_subtree"]["threads"][0];
        assert_eq!(
            observation.as_object().unwrap().keys().last().unwrap(),
            "depth"
        );
        assert_eq!(
            persisted["state"]["agent_subtree"]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            [
                "threads",
                "total_cost",
                "total_agents",
                "accounting_complete"
            ]
        );
    }

    #[test]
    fn idle_restore_is_atomically_rewritten_without_another_model_request() {
        use std::os::unix::fs::MetadataExt;
        const CHILD: &str = "REAGENT_CHECKPOINT_REWRITE_TEST";
        if let Some(root) = std::env::var_os(CHILD) {
            let root = std::path::PathBuf::from(root);
            let script = root.join("script.json");
            std::fs::write(&script, r#"[{"blocks":[{"kind":"text","text":"unused"}]}]"#).unwrap();
            let mut session = crate::runtime::Session::new(json!({"session_id":"0123456789abcdef","provider":"scripted","model":"scripted","scripted":script,"workspace":root,"checkpoint_enabled":true,"context_gc":true})).unwrap();
            session.config["provider"] = json!("openai");
            session.config["model"] = json!("gpt-6-sol");
            session.config["scripted"] = json!("");
            save(&session, "idle", "").unwrap();
            let path = directory("0123456789abcdef")
                .unwrap()
                .join("checkpoint.json");
            let mut imported = state(&session, "idle", "");
            imported["history"] = json!([]);
            std::fs::write(&path, envelope(&imported).unwrap()).unwrap();
            let old = std::fs::metadata(&path).unwrap().ino();
            close("0123456789abcdef");
            let saved = load("0123456789abcdef").unwrap();
            session.config = saved["config"].clone();
            session.state = saved["state"].clone();
            session.history = saved["history"].as_array().cloned().unwrap_or_default();
            assert_eq!(recover(&mut session).unwrap(), "");
            save(&session, "idle", "").unwrap();
            assert_ne!(std::fs::metadata(&path).unwrap().ino(), old);
            let rewritten: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            assert_eq!(rewritten["state"]["history"], json!([]));
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            let previous = std::fs::read(&path).unwrap();
            let previous_inode = std::fs::metadata(&path).unwrap().ino();
            CLOSE_ERROR.with(|fault| fault.set(Some(libc::EIO)));
            assert_eq!(
                save(&session, "terminal", "").unwrap_err(),
                format!(
                    "save session 0123456789abcdef: close checkpoint: {}",
                    crate::errors::stable_io(&std::io::Error::from_raw_os_error(libc::EIO))
                )
            );
            assert_eq!(std::fs::read(&path).unwrap(), previous);
            assert_eq!(std::fs::metadata(&path).unwrap().ino(), previous_inode);
            assert!(!std::fs::read_dir(path.parent().unwrap())
                .unwrap()
                .any(|entry| {
                    entry
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".checkpoint-")
                }));
            save(&session, "terminal", "").unwrap();
            assert_eq!(
                load("0123456789abcdef").unwrap()["checkpoint"]["phase"],
                "terminal"
            );
            for (pointer, invalid) in [
                ("/read_only", json!("true")),
                ("/plan", json!(1)),
                ("/max_steps", json!("5")),
                ("/max_tool_calls", json!(1.5)),
                ("/model_retry_window", json!(true)),
                ("/context_window_tokens", json!(9223372036854775808u64)),
                ("/history", json!({})),
                ("/history", json!(false)),
                ("/history", json!([{"kind":"user","user":{"text":7}}])),
                ("/history", json!([{"kind":"assistant","assistant":true}])),
                (
                    "/history",
                    json!([{"kind":"assistant","assistant":{"blocks":{}}}]),
                ),
                (
                    "/history",
                    json!([{"kind":"assistant","assistant":{"blocks":[{"kind":"text","text":true}]}}]),
                ),
                (
                    "/history",
                    json!([{"kind":"assistant","assistant":{"native":{"provider":false}}}]),
                ),
                (
                    "/history",
                    json!([{"kind":"assistant","assistant":{"usage":{"known":"true"}}}]),
                ),
                (
                    "/history",
                    json!([{"kind":"assistant","assistant":{"usage":{"input_tokens":1.5}}}]),
                ),
                ("/usage", json!([])),
                ("/usage", json!({"output_tokens":"1"})),
                ("/seen", json!({"c":"true"})),
                ("/gc_state", json!({"paths":{"c":1}})),
                ("/pending_submission", json!([])),
                ("/prompt", json!({"text":true})),
                ("/agent_roster", json!([{"steps":"1"}])),
            ] {
                let mut malformed = rewritten["state"].clone();
                malformed
                    .as_object_mut()
                    .unwrap()
                    .insert(pointer[1..].into(), invalid);
                std::fs::write(&path, checked_envelope(&malformed)).unwrap();
                assert_eq!(
                    load("0123456789abcdef").unwrap_err(),
                    "corrupt session 0123456789abcdef: invalid checkpoint state",
                    "{pointer}"
                );
            }
            std::fs::write(&path, LEGACY).unwrap();
            let legacy = load("0123456789abcdef").unwrap();
            session.config = legacy["config"].clone();
            session.state = legacy["state"].clone();
            session.history = legacy["history"].as_array().unwrap().clone();
            save(&session, "terminal", "").unwrap();
            let migrated = load("0123456789abcdef").unwrap();
            assert_eq!(migrated["checkpoint"]["version"], 2);
            assert_eq!(migrated["config"]["read_only"], true);
            assert_eq!(
                migrated["config"]["prompt"]["text"],
                "recorded instructions"
            );
            assert_eq!(migrated["history"], legacy["history"]);
            std::fs::remove_file(&path).unwrap();
            std::fs::create_dir(&path).unwrap();
            assert_eq!(
                save(&session, "terminal", "").unwrap_err(),
                "save session 0123456789abcdef: replace checkpoint: path already exists"
            );
            assert!(path.is_dir());
            assert!(!std::fs::read_dir(path.parent().unwrap())
                .unwrap()
                .any(|entry| entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".checkpoint-")));
            close("0123456789abcdef");
            session.reset();
            assert!(state(&session, "idle", "")["history"].is_null());
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let result = std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact", "checkpoint::tests::idle_restore_is_atomically_rewritten_without_another_model_request", "--nocapture"]).env(CHILD, root.path()).env("XDG_CACHE_HOME", root.path().join("cache")).env("HOME", root.path()).output().unwrap();
        assert!(
            result.status.success(),
            "{}{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
    }

    const LEGACY: &[u8] = include_bytes!("../testdata/legacy_checkpoint.json");

    fn legacy_envelope(payload: &[u8]) -> Vec<u8> {
        serde_json::to_vec(&json!({"version":1,"id":"0123456789abcdef",
            "state":STANDARD.encode(payload),"sha256":format!("{:x}", Sha256::digest(payload))}))
        .unwrap()
    }

    #[test]
    fn historical_go_gob_restores_and_migrates_opaque_raw_bytes() {
        let cp = parse("0123456789abcdef", LEGACY).unwrap();
        assert_eq!(cp["version"], 1);
        assert_eq!(cp["read_only"], true);
        assert_eq!(cp["max_tool_calls"], 16);
        assert_eq!(cp["model_retry_window"], 1500000000i64);
        assert_eq!(cp["tokens_per_byte"], 0.375);
        assert_eq!(cp["history"].as_array().unwrap().len(), 5);
        assert_eq!(cp["history"][3]["shell"]["exit_code"], -1);
        assert_eq!(cp["history"][4]["summary"]["replaced_entries"], 3);
        assert_eq!(cp["usage"]["input_tokens"], i64::MAX);
        assert_eq!(cp["gc_state"]["dropped"]["c1"]["step"], 2);
        assert_eq!(cp["agent_subtree"]["threads"][0]["id"], "t1");
        assert_eq!(cp["agent_subtree"]["threads"][0]["depth"], 2);
        assert_eq!(cp["agent_subtree"]["next_offset"], 3);
        assert_eq!(cp["context_window_tokens"], 0);
        assert_eq!(cp["context_gc_consolidate"], false);
        assert!(cp.get("LegacyTime").is_none());
        let saved = restored("0123456789abcdef", cp.clone());
        assert_eq!(saved["config"]["project_instructions"], "launch project");
        assert_eq!(saved["config"]["prompt"]["text"], "recorded instructions");
        let expected = [0xff, 0, 0xfe];
        assert_eq!(
            crate::raw::bytes(&cp["history"][1]["assistant"]["native"]["items"][1]).unwrap(),
            expected
        );
        let mut next = cp.clone();
        next["version"] = json!(2);
        let bytes = envelope(&next).unwrap();
        let migrated = parse("0123456789abcdef", &bytes).unwrap();
        assert_eq!(migrated["version"], 2);
        for field in ["history", "gc_state", "usage", "prompt"] {
            assert_eq!(migrated[field], cp[field], "{field}");
        }
        let mut expected_subtree = cp["agent_subtree"].clone();
        expected_subtree["threads"][0]
            .as_object_mut()
            .unwrap()
            .remove("trace_path");
        assert_eq!(migrated["agent_subtree"], expected_subtree);
        assert_eq!(
            crate::raw::bytes(&migrated["history"][0]["user"]["workspace"]).unwrap(),
            b" { \"kind\" : \"workspace_state\" } "
        );
    }

    #[test]
    fn legacy_numeric_byte_arrays_match_base64_and_reject_malformed_payloads() {
        let mut fixture: Value = serde_json::from_slice(LEGACY).unwrap();
        let payload = STANDARD.decode(fixture["state"].as_str().unwrap()).unwrap();
        fixture["state"] = json!(payload);
        let expected = parse("0123456789abcdef", LEGACY).unwrap();
        assert_eq!(
            parse("0123456789abcdef", &serde_json::to_vec(&fixture).unwrap()).unwrap(),
            expected
        );
        let zero = payload.iter().position(|b| *b == 0).unwrap();
        fixture["state"][zero] = Value::Null; // Go leaves null byte elements zeroed.
        assert_eq!(
            parse("0123456789abcdef", &serde_json::to_vec(&fixture).unwrap()).unwrap(),
            expected
        );
        for malformed in [
            json!([-1]),
            json!([256]),
            json!([1.0]),
            json!(["1"]),
            json!({}),
            json!(true),
        ] {
            fixture["state"] = malformed;
            assert!(
                parse("0123456789abcdef", &serde_json::to_vec(&fixture).unwrap())
                    .unwrap_err()
                    .ends_with("invalid legacy checkpoint payload")
            );
        }
        for empty in [Value::Null, json!([])] {
            fixture["state"] = empty;
            fixture["sha256"] = json!(format!("{:x}", Sha256::digest([])));
            assert!(
                parse("0123456789abcdef", &serde_json::to_vec(&fixture).unwrap())
                    .unwrap_err()
                    .ends_with("invalid legacy checkpoint state")
            );
        }
        let truncated = &payload[..payload.len() - 1];
        fixture["state"] = json!(truncated);
        fixture["sha256"] = json!(format!("{:x}", Sha256::digest(truncated)));
        assert!(
            parse("0123456789abcdef", &serde_json::to_vec(&fixture).unwrap())
                .unwrap_err()
                .ends_with("invalid legacy checkpoint state")
        );
    }

    #[test]
    fn legacy_payload_checksum_and_bounded_gob_failures() {
        let fixture: Value = serde_json::from_slice(LEGACY).unwrap();
        let payload = STANDARD.decode(fixture["state"].as_str().unwrap()).unwrap();
        let mut invalid_text = payload.clone();
        let index = invalid_text
            .windows(6)
            .position(|v| v == b"\x05hello")
            .unwrap();
        invalid_text[index + 4..index + 6].copy_from_slice(&[0xe2, 0x82]);
        let cp = parse("0123456789abcdef", &legacy_envelope(&invalid_text)).unwrap();
        assert_eq!(
            crate::raw::string_bytes(&cp["history"][0]["user"]["text"]).unwrap(),
            b"hel\xe2\x82"
        );
        let history = cp["history"].as_array().unwrap();
        let request = crate::context::build_context(
            &json!({"provider":"openai","model":"gpt-5"}),
            json!({}),
            &history[..1],
            json!([]),
        );
        let body = crate::openai::encode_request(&request, false).unwrap();
        let quoted = br#""hel\ufffd\ufffd""#;
        let wire = crate::types::json_bytes(&body);
        assert!(wire.windows(quoted.len()).any(|w| w == quoted));
        let cache_request = json!({"model":"m","instructions":"i","history":[
            {"kind":"user","user":{"text":cp["history"][0]["user"]["text"]}}
        ]});
        let mut hash = Sha256::new();
        hash.update(b"m\0i\0");
        hash.update(br#"{"kind":"user","user":{"text":"hel\ufffd\ufffd"}}"#);
        assert_eq!(
            crate::openai::cache_key(&cache_request),
            format!("{:x}", hash.finalize())[..32]
        );
        let mut next = cp.clone();
        next["version"] = json!(2);
        let migrated = envelope(&next).unwrap();
        assert!(migrated.windows(quoted.len()).any(|w| w == quoted));
        assert_eq!(
            parse("0123456789abcdef", &migrated).unwrap()["history"][0]["user"]["text"],
            "hel\u{fffd}\u{fffd}"
        );
        let marker = b"__reagent_raw_string_v1";
        assert!(!migrated.windows(marker.len()).any(|w| w == marker));
        assert_eq!(
            crate::raw::bytes(&cp["history"][1]["assistant"]["native"]["items"][1]).unwrap(),
            [0xff, 0, 0xfe]
        );
        let mut trailing_bits = fixture.clone();
        let mut encoded = fixture["state"].as_str().unwrap().as_bytes().to_vec();
        assert!(encoded.ends_with(b"AA=="));
        let last = encoded.len() - 3;
        encoded[last] = b'B';
        trailing_bits["state"] = json!(String::from_utf8(encoded).unwrap());
        parse(
            "0123456789abcdef",
            &serde_json::to_vec(&trailing_bits).unwrap(),
        )
        .unwrap();
        let mut bad = fixture.clone();
        bad["state"] = json!("not base64");
        assert!(
            parse("0123456789abcdef", &serde_json::to_vec(&bad).unwrap())
                .unwrap_err()
                .ends_with("invalid legacy checkpoint payload")
        );
        bad["state"] = json!(STANDARD.encode(b"different"));
        assert!(
            parse("0123456789abcdef", &serde_json::to_vec(&bad).unwrap())
                .unwrap_err()
                .ends_with("checkpoint checksum mismatch")
        );
        for cut in (0..payload.len())
            .step_by(31)
            .chain(payload.len() - 12..payload.len())
        {
            assert!(
                parse("0123456789abcdef", &legacy_envelope(&payload[..cut]))
                    .unwrap_err()
                    .ends_with("invalid legacy checkpoint state"),
                "cut {cut}"
            );
        }
        for malformed in [
            vec![0xff, 0xff],
            vec![1, 2],
            vec![2, 127, 0],
            vec![0xf8; 20],
        ] {
            assert!(parse("0123456789abcdef", &legacy_envelope(&malformed))
                .unwrap_err()
                .ends_with("invalid legacy checkpoint state"));
        }
        let mut wrong_type = payload.clone();
        let name = b"ReadOnly";
        let index = wrong_type
            .windows(name.len())
            .position(|v| v == name)
            .unwrap();
        assert_eq!(
            &wrong_type[index + name.len()..index + name.len() + 2],
            &[1, 2]
        );
        wrong_type[index + name.len() + 1] = 12; // string instead of bool in the type declaration.
        assert!(parse("0123456789abcdef", &legacy_envelope(&wrong_type))
            .unwrap_err()
            .ends_with("invalid legacy checkpoint state"));
        let mut unknown = payload.clone();
        unknown[index] = b'r'; // Gob field names, unlike JSON tags, are case-sensitive.
        let cp = parse("0123456789abcdef", &legacy_envelope(&unknown)).unwrap();
        assert_eq!(cp["read_only"], false);
        let first_length = 3 + usize::from(u16::from_be_bytes([payload[1], payload[2]]));
        let duplicate = [&payload[..first_length], payload.as_slice()].concat();
        assert!(parse("0123456789abcdef", &legacy_envelope(&duplicate))
            .unwrap_err()
            .ends_with("invalid legacy checkpoint state"));
        // Mutation smoke checks also guard the parser's non-panicking failure boundary.
        for index in (0..payload.len()).step_by(17) {
            let mut mutated = payload.clone();
            mutated[index] ^= 0xff;
            let _ = parse("0123456789abcdef", &legacy_envelope(&mutated));
        }
        bad["version"] = json!(3);
        assert!(
            parse("0123456789abcdef", &serde_json::to_vec(&bad).unwrap())
                .unwrap_err()
                .ends_with("unsupported checkpoint version 3")
        );
    }
}
