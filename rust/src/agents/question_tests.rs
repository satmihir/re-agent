use super::*;

const QUESTION: &str = "Which region should I use?";

fn response(id: &str, name: &str, args: Value) -> Value {
    json!({"blocks":[{"kind":"tool_call","call":{"call_id":id,"name":name,"arguments":args.to_string()}}]})
}

fn done(reply: &str) -> Value {
    json!({"blocks":[{"kind":"text","text":reply}]})
}

fn script(dir: &tempfile::TempDir, name: &str, steps: Value) -> std::path::PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, steps.to_string()).unwrap();
    path
}

fn session(dir: &tempfile::TempDir, steps: Value) -> Session {
    Session::new(json!({"provider":"scripted","model":"scripted","scripted":script(dir,"script.json",steps),"workspace":dir.path(),"trace_dir":dir.path(),"agents":true,"agent_questions":true})).unwrap()
}

fn tool(s: &mut Session, name: &str, args: Value) -> Value {
    execute(s, name, &args.to_string()).unwrap().unwrap()
}

fn ok(s: &mut Session, name: &str, args: Value) -> Value {
    let result = tool(s, name, args);
    assert_eq!(result["ok"], true, "{name}: {result}");
    result["data"].clone()
}

fn pending(s: &Session, id: &str) -> Value {
    let shared = tree(s);
    let t = shared.tree.lock().unwrap();
    let (t, _) = shared
        .changed
        .wait_timeout_while(t, Duration::from_secs(5), |t| {
            let n = &t.nodes[id];
            n.working && n.record["pending_question"].is_null()
        })
        .unwrap();
    let n = &t.nodes[id];
    let question = n.record["pending_question"].clone();
    let working = n.working;
    drop(t);
    assert!(working, "child completed instead of blocking on ask_parent");
    assert_eq!(question["question"], QUESTION);
    assert!(!text(&question["question_id"]).is_empty());
    question["question_id"].clone()
}

#[test]
fn parent_question_interrupts_unrelated_wait_and_resumes_the_original_run() {
    let dir = tempfile::tempdir().unwrap();
    let mut root = session(
        &dir,
        json!([
            response("choice", "ask_parent", json!({"question":QUESTION})),
            done("used the answer"),
            done("finished follow-up")
        ]),
    );
    let worker = ok(
        &mut root,
        "spawn",
        json!({"task":"choose region","context":{}}),
    );
    let id = &worker["id"];
    let question_id = pending(&root, text(id));

    // Keep the unrelated target running until wait returns, without a clock or network gate.
    let publication = root.tools.workspace.publish_mu.clone();
    let gate = publication.lock().unwrap();
    root.config["scripted"] = json!(script(
        &dir,
        "writer.json",
        json!([
            response(
                "write",
                "write_file",
                json!({"path":"marker","content":"ready"})
            ),
            done("written")
        ])
    ));
    let writer = ok(
        &mut root,
        "spawn",
        json!({"task":"write marker","context":{},"authority":"write"}),
    );
    let waited = ok(&mut root, "wait", json!({"ids":[writer["id"]],"timeout":2}));
    assert_eq!(waited["timed_out"], false, "{waited}");
    assert_eq!(waited["reason"], "question_pending", "{waited}");
    assert_eq!(waited["ids"], json!([id]));

    assert!(deliver(&mut root));
    let question_entries: Vec<_> = root
        .history
        .iter()
        .filter(|e| text(&e["user"]["text"]).contains(QUESTION))
        .collect();
    assert_eq!(question_entries.len(), 1);
    assert!(text(&question_entries[0]["user"]["text"]).contains(text(&question_id)));
    assert!(!deliver(&mut root));
    drop(gate);

    ok(
        &mut root,
        "send",
        json!({"id":id,"message":"follow-up task"}),
    );
    assert_eq!(
        tool(
            &mut root,
            "send",
            json!({"id":id,"reply_to":"obsolete","message":"wrong"})
        )["ok"],
        false
    );
    assert_eq!(pending(&root, text(id)), question_id);
    let reply = json!({"id":id,"reply_to":question_id,"message":"west"});
    ok(&mut root, "send", reply.clone());
    assert_eq!(tool(&mut root, "send", reply)["ok"], false);
    let waited = ok(
        &mut root,
        "wait",
        json!({"ids":[id,writer["id"]],"timeout":5}),
    );
    assert_eq!(waited["timed_out"], false, "{waited}");

    let events: Vec<Value> = std::fs::read_to_string(text(&worker["trace_path"]))
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    let requests: Vec<_> = events
        .iter()
        .filter(|e| e["type"] == "model.requested")
        .collect();
    assert_eq!(requests.len(), 3);
    assert!(requests[0]["run_id"].is_string());
    assert_eq!(requests[0]["run_id"], requests[1]["run_id"]);
    assert_ne!(requests[1]["run_id"], requests[2]["run_id"]);
    let entry = requests[1]["data"]["history"]
        .as_array()
        .unwrap()
        .last()
        .unwrap();
    assert_eq!(entry["kind"], "tool");
    assert_eq!(entry["tool"]["call_id"], "choice");
    assert_eq!(entry["tool"]["outcome"]["ok"], true);
    assert_eq!(entry["tool"]["outcome"]["data"]["answer"], "west");
    assert!(
        requests[2]["data"]["history"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()["user"]["text"]
            == "follow-up task"
    );
    stop(&root, true);
}

#[test]
fn grandchild_question_is_private_to_its_parent_and_cancel_invalidates_the_reply() {
    let dir = tempfile::tempdir().unwrap();
    let mut root = session(&dir, json!([done("ready")]));
    let middle = ok(
        &mut root,
        "spawn",
        json!({"task":"prepare","context":{},"kind":"teammate","name":"middle"}),
    );
    assert_eq!(
        ok(&mut root, "wait", json!({"ids":[middle["id"]],"timeout":5}))["timed_out"],
        false
    );
    assert!(deliver(&mut root));
    let shared = tree(&root);
    let mut child = shared
        .tree
        .lock()
        .unwrap()
        .nodes
        .get_mut(text(&middle["id"]))
        .unwrap()
        .session
        .take()
        .unwrap();
    child.config["scripted"] = json!(script(
        &dir,
        "leaf.json",
        json!([
            response("choice", "ask_parent", json!({"question":QUESTION})),
            done("unexpected")
        ])
    ));
    let leaf = ok(&mut child, "spawn", json!({"task":"choose","context":{}}));
    let question_id = pending(&child, text(&leaf["id"]));
    let reply = json!({"id":leaf["id"],"reply_to":question_id,"message":"east"});
    assert!(!deliver(&mut root));
    assert_eq!(tool(&mut root, "send", reply.clone())["ok"], false);
    assert!(deliver(&mut child));
    assert!(!deliver(&mut child));

    // Bound the test even if cancellation accidentally leaves ask_parent asleep.
    let (tx, rx) = std::sync::mpsc::channel();
    let target = json!({"id":leaf["id"]});
    std::thread::spawn(move || {
        let cancelled = tool(&mut child, "cancel", target);
        tx.send((child, cancelled)).unwrap();
    });
    let (mut child, cancelled) = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("cancel never released ask_parent");
    assert_eq!(cancelled["ok"], true, "{cancelled}");
    assert_eq!(tool(&mut child, "send", reply)["ok"], false);
    let t = shared.tree.lock().unwrap();
    let leaf = &t.nodes[text(&leaf["id"])];
    assert!(!leaf.working);
    assert!(leaf.record["pending_question"].is_null());
    assert_eq!(leaf.results[0]["result"]["status"], "cancelled");
    drop(t);
    stop(&root, true);
}
