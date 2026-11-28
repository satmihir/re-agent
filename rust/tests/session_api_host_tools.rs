use reagent::session_api::*;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::{Arc, Mutex};

fn call(id: &str, name: &str, arguments: Value) -> Value {
    json!({"blocks":[{"kind":"tool_call","call":{"call_id":id,"name":name,"arguments":arguments.to_string()}}]})
}

fn text(text: &str) -> Value {
    json!({"blocks":[{"kind":"text","text":text}]})
}

fn tools() -> Vec<HostTool> {
    let schema =
        json!({"type":"object","properties":{"name":{"type":"string"}},"required":["name"]});
    vec![
        HostTool {
            name: "session_status".into(),
            description: "Say how a coding Session is doing.".into(),
            input_schema: schema.clone(),
            read_only: true,
        },
        HostTool {
            name: "session_stop".into(),
            description: "Stop a coding Session.".into(),
            input_schema: schema,
            read_only: false,
        },
    ]
}

fn open(root: &Path, responses: Vec<Value>, plan: bool) -> SessionApi {
    let script = root.join("script.json");
    std::fs::write(&script, json!(responses).to_string()).unwrap();
    open_session(SessionOptions {
        workspace: root.into(),
        scripted: Some(script),
        checkpoint: false,
        agents: false,
        plan,
        host_tools: tools(),
        ..Default::default()
    })
    .unwrap()
}

/// Outcome runs one message and returns the outcome of its single tool call.
fn outcome(api: &SessionApi) -> ToolOutcomeView {
    let run = api
        .send_message("m1", b"check on it", PlanMode::Keep)
        .unwrap()
        .run
        .unwrap();
    assert_eq!(
        api.wait_run(&run).unwrap().result.unwrap().status,
        "completed"
    );
    let page = api.read_entries(None, 100).unwrap();
    let outcomes: Vec<_> = page
        .entries
        .into_iter()
        .filter_map(|entry| match entry.content {
            EntryContent::Tool { outcome, .. } => Some(outcome),
            _ => None,
        })
        .collect();
    assert_eq!(outcomes.len(), 1);
    outcomes.into_iter().next().unwrap()
}

fn bytes(content: &ContentBytes) -> String {
    String::from_utf8_lossy(&content.bytes).into_owned()
}

#[test]
fn host_answer_returns_to_the_model_as_the_calls_outcome() {
    let root = tempfile::tempdir().unwrap();
    let responses = vec![
        call("c1", "session_status", json!({"name":"fix-auth"})),
        text("It is working."),
    ];
    let api = open(root.path(), responses, false);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let calls = seen.clone();
    api.set_host_tools(move |call| {
        calls
            .lock()
            .unwrap()
            .push((call.name.clone(), call.arguments.clone()));
        HostToolOutcome {
            ok: true,
            message: String::new(),
            data: json!({"state":"working"}),
        }
    });
    let outcome = outcome(&api);
    assert!(outcome.ok);
    assert_eq!(bytes(&outcome.effect), "none");
    assert_eq!(
        serde_json::from_slice::<Value>(&outcome.data.bytes).unwrap(),
        json!({"state":"working"})
    );
    assert_eq!(
        *seen.lock().unwrap(),
        [("session_status".to_owned(), json!({"name":"fix-auth"}))]
    );
    api.close().unwrap();
}

#[test]
fn a_failed_host_answer_and_a_writing_tool_report_their_effects() {
    let root = tempfile::tempdir().unwrap();
    for (ok, code, effect) in [(true, "", "applied"), (false, "host_tool_failed", "none")] {
        let responses = vec![
            call("c1", "session_stop", json!({"name":"fix-auth"})),
            text("Done."),
        ];
        let api = open(root.path(), responses, false);
        api.set_host_tools(move |_| HostToolOutcome {
            ok,
            message: if ok { "" } else { "no such session" }.into(),
            data: Value::Null,
        });
        let outcome = outcome(&api);
        assert_eq!(outcome.ok, ok);
        assert_eq!(bytes(&outcome.code), code);
        assert_eq!(bytes(&outcome.effect), effect);
        api.close().unwrap();
    }
}

#[test]
fn a_host_tool_call_with_no_host_fails_at_once() {
    let root = tempfile::tempdir().unwrap();
    let responses = vec![
        call("c1", "session_status", json!({"name":"fix-auth"})),
        text("The application is not connected."),
    ];
    let api = open(root.path(), responses, false);
    let outcome = outcome(&api);
    assert!(!outcome.ok);
    assert_eq!(bytes(&outcome.code), "host_unavailable");
    api.close().unwrap();
}

#[test]
fn plan_mode_refuses_a_writing_host_tool_without_asking_the_host() {
    let root = tempfile::tempdir().unwrap();
    let responses = vec![
        call("c1", "session_stop", json!({"name":"fix-auth"})),
        text("Refused."),
    ];
    let api = open(root.path(), responses, true);
    let asked = Arc::new(Mutex::new(false));
    let flag = asked.clone();
    api.set_host_tools(move |_| {
        *flag.lock().unwrap() = true;
        HostToolOutcome {
            ok: true,
            message: String::new(),
            data: Value::Null,
        }
    });
    let outcome = outcome(&api);
    assert!(!outcome.ok);
    assert_eq!(bytes(&outcome.code), "plan_mode");
    assert!(!*asked.lock().unwrap());
    api.close().unwrap();
}

#[test]
fn host_tools_that_cannot_be_offered_are_refused_at_open() {
    let root = tempfile::tempdir().unwrap();
    let tool = |name: &str, description: &str, schema: Value| HostTool {
        name: name.into(),
        description: description.into(),
        input_schema: schema,
        read_only: true,
    };
    let object = json!({"type":"object"});
    let cases = [
        (
            vec![tool("read_file", "Shadows a file tool.", object.clone())],
            false,
        ),
        (
            vec![tool("spawn", "Shadows an agent tool.", object.clone())],
            false,
        ),
        (
            vec![tool("Session-List", "Bad name.", object.clone())],
            false,
        ),
        (vec![tool("session_list", " ", object.clone())], false),
        (
            vec![tool("session_list", "No schema.", json!("object"))],
            false,
        ),
        (
            vec![
                tool("session_list", "Twice.", object.clone()),
                tool("session_list", "Twice.", object.clone()),
            ],
            false,
        ),
        (vec![tool("session_list", "Fine.", object.clone())], true),
    ];
    for (tools, tool_free) in cases {
        let error = open_session(SessionOptions {
            workspace: root.path().into(),
            scripted: Some(root.path().join("unused.json")),
            checkpoint: false,
            tool_free,
            host_tools: tools.clone(),
            ..Default::default()
        })
        .err()
        .unwrap_or_else(|| panic!("{tools:?} was accepted"));
        assert_eq!(error.code, "invalid_argument", "{tools:?}");
    }
}
