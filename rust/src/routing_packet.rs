use crate::{jev, raw, types};
use serde_json::{json, Map, Value};

const PREVIEW_BYTES: usize = 512;

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}

/// Builds the bounded, visible conversation supplied to the router.
pub(crate) fn build(
    config: &Value,
    history: &[Value],
    plan_mode: bool,
    routes: &[Value],
) -> Result<Value, String> {
    let (entries, native) = crate::chat::model_switch::visible(history)?;
    let mut state = json!({
        "workspace":text(config,"workspace"),
        "mode":if config["read_only"].as_bool().unwrap_or(false) {"read only"} else {"read, write, and execute"},
        "plan_mode":plan_mode,
        "current_model":text(config,"model"),
        "current_effort":text(config,"reasoning_effort"),
        "project_instructions":config["project_instructions"],
        "user_requests_and_summaries":[],
        "latest_complete_plan":null,
        "recent_evidence":[],
        "omitted_earlier_entries":0,
        "omitted_earlier_user_requests":0,
        "omitted_native_items":native,
        "previews":[]
    });
    let mut critical = vec![false; entries.len()];
    let mut has_user = false;
    let mut last_assistant = entries.len();
    for (index, entry) in entries.iter().enumerate() {
        let user = text(entry, "kind") == "user" && text(&entry["user"], "source").is_empty();
        if user || text(entry, "kind") == "summary" {
            state["user_requests_and_summaries"]
                .as_array_mut()
                .unwrap()
                .push(entry.clone());
            critical[index] = true;
        }
        if user {
            let bytes = raw::string_bytes(&entry["user"]["text"]).unwrap_or_default();
            has_user |= !String::from_utf8_lossy(&bytes).trim().is_empty();
        }
        if entry["assistant"].is_object() {
            last_assistant = index;
            if complete_plan(&history[index]["assistant"]) {
                state["latest_complete_plan"] = entry.clone();
            }
        }
    }
    if !has_user {
        return Err("routing objective is missing".into());
    }
    if let Some(number) = state["latest_complete_plan"]["entry"].as_u64() {
        critical[number as usize - 1] = true;
    }
    for (index, entry) in entries.iter().enumerate() {
        if critical[index] {
            continue;
        }
        if index < last_assistant {
            increment(&mut state, "omitted_earlier_entries");
            continue;
        }
        let (entry, markers) = preview(entry);
        state["recent_evidence"].as_array_mut().unwrap().push(entry);
        state["previews"].as_array_mut().unwrap().extend(markers);
    }
    let mut encoded = encode(&state, routes);
    let mut latest = state["user_requests_and_summaries"]
        .as_array()
        .unwrap()
        .iter()
        .rposition(|entry| text(entry, "kind") == "user")
        .unwrap();
    let recent_markers = state["previews"].as_array().unwrap().clone();
    // Go retains a nonnil slice when omission removes its final marker.
    let mut previews_non_nil = !recent_markers.is_empty();
    let mut previewed = Vec::new();
    for index in 0..latest {
        if encoded.is_ok() {
            break;
        }
        let entry = &state["user_requests_and_summaries"][index];
        if text(entry, "kind") != "user" {
            continue;
        }
        let (entry, markers) = preview(entry);
        if markers.is_empty() {
            continue;
        }
        state["user_requests_and_summaries"][index] = entry;
        previewed.extend(markers);
        previews_non_nil = true;
        state["previews"] = json!(previewed
            .iter()
            .chain(recent_markers.iter())
            .cloned()
            .collect::<Vec<_>>());
        encoded = encode(&state, routes);
    }
    while encoded.is_err() {
        let oldest = state["user_requests_and_summaries"].as_array().unwrap()[..latest]
            .iter()
            .position(|entry| text(entry, "kind") == "user");
        let Some(oldest) = oldest else { break };
        let number = state["user_requests_and_summaries"][oldest]["entry"].clone();
        state["user_requests_and_summaries"]
            .as_array_mut()
            .unwrap()
            .remove(oldest);
        state["previews"]
            .as_array_mut()
            .unwrap()
            .retain(|marker| marker["entry"] != number);
        increment(&mut state, "omitted_earlier_user_requests");
        latest -= 1;
        encoded = encode(&state, routes);
    }
    let mut encoded = encoded.map_err(|error| {
        format!("critical routing state or latest segment does not fit: {error}")
    })?;
    // Backfill the newest contiguous evidence, stopping at the first non-fit.
    for index in (0..last_assistant).rev() {
        if critical[index] {
            continue;
        }
        let (entry, markers) = preview(&entries[index]);
        let mut candidate = state.clone();
        candidate["recent_evidence"]
            .as_array_mut()
            .unwrap()
            .insert(0, entry);
        candidate["previews"]
            .as_array_mut()
            .unwrap()
            .extend(markers);
        candidate["omitted_earlier_entries"] =
            json!(candidate["omitted_earlier_entries"].as_u64().unwrap() - 1);
        let Ok(candidate_text) = encode(&candidate, routes) else {
            break;
        };
        // Backfill copies markers into a nil slice before appending new ones.
        previews_non_nil = !candidate["previews"].as_array().unwrap().is_empty();
        state = candidate;
        encoded = candidate_text;
    }
    Ok(json!({
        "state":encoded,
        "omitted_earlier":state["omitted_earlier_entries"],
        "omitted_requests":state["omitted_earlier_user_requests"],
        "omitted_native":native,
        "truncations":if previews_non_nil {state["previews"].clone()} else {Value::Null}
    }))
}

fn increment(state: &mut Value, key: &str) {
    state[key] = json!(state[key].as_u64().unwrap() + 1);
}

fn encode(state: &Value, routes: &[Value]) -> Result<String, String> {
    let mut fields = Map::new();
    for (key, value) in state.as_object().unwrap() {
        if matches!(
            key.as_str(),
            "project_instructions" | "latest_complete_plan"
        ) && value.is_null()
        {
            continue;
        }
        if key == "omitted_earlier_user_requests" && value == &json!(0) {
            continue;
        }
        if key == "previews" && value.as_array().unwrap().is_empty() {
            continue;
        }
        let value = if matches!(
            key.as_str(),
            "user_requests_and_summaries" | "recent_evidence"
        ) && value.as_array().unwrap().is_empty()
        {
            Value::Null
        } else {
            value.clone()
        };
        fields.insert(key.clone(), value);
    }
    let value = Value::Object(fields);
    if crate::provider::decode_json(&types::json_bytes(&value)).is_err() {
        return Err("cannot encode routing state: invalid JSON data".into());
    }
    let encoded = types::json_string(&value);
    jev::encode_request(&encoded, routes)?;
    Ok(encoded)
}

fn complete_plan(assistant: &Value) -> bool {
    let parts: Vec<_> = assistant["blocks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|block| text(block, "kind") == "text")
        .filter_map(|block| raw::string_bytes(&block["text"]))
        .filter(|bytes| !String::from_utf8_lossy(bytes).trim().is_empty())
        .collect();
    let reply = parts.join(b"\n".as_slice());
    let mut open = false;
    let mut found = false;
    for line in reply.split(|byte| *byte == b'\n') {
        match line {
            b"<plan>" => {
                open = true;
                found = false;
            }
            b"</plan>" if open => {
                open = false;
                found = true;
            }
            _ => {}
        }
    }
    found && !open
}

fn preview_text(value: &mut Value, entry: &Value, field: &str, markers: &mut Vec<Value>) {
    let Some(bytes) = raw::string_bytes(value) else {
        return;
    };
    if bytes.len() <= PREVIEW_BYTES {
        return;
    }
    markers.push(json!({"entry":entry,"field":field,"original_bytes":bytes.len()}));
    let mut end = PREVIEW_BYTES;
    while end > 0 && bytes[end] & 0xc0 == 0x80 {
        end -= 1;
    }
    *value = raw::string(&bytes[..end]);
}

fn preview(entry: &Value) -> (Value, Vec<Value>) {
    let mut projected = entry.clone();
    let mut markers = Vec::new();
    let number = &entry["entry"];
    if let Some(value) = projected
        .get_mut("user")
        .and_then(|user| user.get_mut("text"))
    {
        preview_text(value, number, "user.text", &mut markers);
    }
    for (index, block) in projected
        .get_mut("assistant")
        .and_then(|assistant| assistant.get_mut("blocks"))
        .and_then(Value::as_array_mut)
        .into_iter()
        .flatten()
        .enumerate()
    {
        if let Some(value) = block.get_mut("text") {
            preview_text(
                value,
                number,
                &format!("assistant.blocks.{index}.text"),
                &mut markers,
            );
        }
        if let Some(value) = block
            .get_mut("call")
            .and_then(|call| call.get_mut("arguments"))
        {
            preview_text(
                value,
                number,
                &format!("assistant.blocks.{index}.arguments"),
                &mut markers,
            );
        }
    }
    if projected["assistant"]["blocks"]
        .as_array()
        .is_some_and(Vec::is_empty)
    {
        // Go's preview copies blocks into a nil slice, preserving null for empty blocks.
        projected["assistant"]["blocks"] = Value::Null;
    }
    if let Some(outcome) = projected
        .get_mut("tool")
        .and_then(|tool| tool.get_mut("outcome"))
    {
        if let Some(value) = outcome.get_mut("message") {
            preview_text(value, number, "tool.message", &mut markers);
        }
        let data =
            raw::bytes(&outcome["data"]).unwrap_or_else(|| types::json_bytes(&outcome["data"]));
        if data.len() > PREVIEW_BYTES {
            let mut value = raw::string(&data);
            preview_text(&mut value, number, "tool.data", &mut markers);
            outcome["data"] = value;
            outcome["truncated"] = json!(true);
        }
    }
    if let Some(shell) = projected.get_mut("shell").filter(|value| value.is_object()) {
        for (key, field) in [("command", "shell.command"), ("output", "shell.output")] {
            if let Some(value) = shell.get_mut(key) {
                preview_text(value, number, field, &mut markers);
            }
        }
        if !markers.is_empty() {
            shell["output_truncated"] = json!(true);
        }
    }
    (projected, markers)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Value {
        json!({"workspace":"/work","model":"gpt-6-sol","reasoning_effort":"medium"})
    }

    fn routes() -> Vec<Value> {
        ["low", "medium"].into_iter().map(|effort| json!({"id":effort,"model":"gpt-6-sol","reasoning_effort":effort,"description":"route"})).collect()
    }

    fn user(text: &str) -> Value {
        json!({"kind":"user","user":{"text":text}})
    }

    fn assistant(text: &str) -> Value {
        json!({"kind":"assistant","assistant":{"blocks":[{"kind":"text","text":text}],"native":{"provider":"openai.responses","items":[{"type":"reasoning","secret":"omit"}]}}})
    }

    fn state(packet: &Value) -> Value {
        serde_json::from_str(packet["state"].as_str().unwrap()).unwrap()
    }

    #[test]
    fn declaration_order_nulls_and_project_presence_match_go() {
        let packet = build(&config(), &[user("task")], false, &routes()).unwrap();
        assert_eq!(packet["state"], "{\"workspace\":\"/work\",\"mode\":\"read, write, and execute\",\"plan_mode\":false,\"current_model\":\"gpt-6-sol\",\"current_effort\":\"medium\",\"user_requests_and_summaries\":[{\"entry\":1,\"kind\":\"user\",\"user\":{\"text\":\"task\"}}],\"recent_evidence\":null,\"omitted_earlier_entries\":0,\"omitted_native_items\":0}");
        assert!(packet["truncations"].is_null());
        let mut config = config();
        config["project_instructions"] = json!("");
        config["read_only"] = json!(true);
        let state = state(&build(&config, &[user("task")], true, &routes()).unwrap());
        assert_eq!(state["project_instructions"], "");
        assert_eq!(state["mode"], "read only");
        assert_eq!(state["plan_mode"], true);
    }

    #[test]
    fn previews_observe_512_byte_and_utf8_boundaries_without_mutating_history() {
        for (source, expected) in [
            ("x".repeat(511), "x".repeat(511)),
            ("x".repeat(512), "x".repeat(512)),
            ("x".repeat(513), "x".repeat(512)),
            (format!("{}雪", "x".repeat(511)), "x".repeat(511)),
        ] {
            let history = vec![user("task"), assistant(&source)];
            let before = history.clone();
            let packet = build(&config(), &history, false, &routes()).unwrap();
            let state = state(&packet);
            assert_eq!(
                state["recent_evidence"][0]["assistant"]["blocks"][0]["text"],
                expected
            );
            assert_eq!(packet["omitted_native"], 1);
            assert!(!packet["state"].as_str().unwrap().contains("secret"));
            if source.len() > 512 {
                assert_eq!(
                    packet["truncations"],
                    json!([{"entry":2,"field":"assistant.blocks.0.text","original_bytes":source.len()}])
                );
            } else {
                assert!(packet["truncations"].is_null());
            }
            assert_eq!(history, before);
        }
    }

    #[test]
    fn latest_completed_plan_remains_whole_and_unclosed_last_block_does_not_replace_it() {
        let plan = format!("<plan>\n{}\n</plan>", "p".repeat(700));
        let history = vec![
            user("task"),
            assistant(&plan),
            assistant("<plan>\nclosed\n</plan>\n<plan>\nunfinished"),
        ];
        let state = state(&build(&config(), &history, true, &routes()).unwrap());
        assert_eq!(state["latest_complete_plan"]["entry"], 2);
        assert_eq!(
            state["latest_complete_plan"]["assistant"]["blocks"][0]["text"],
            plan
        );
        assert_eq!(state["recent_evidence"][0]["entry"], 3);
    }

    #[test]
    fn older_requests_yield_but_latest_request_summaries_and_project_remain_critical() {
        let history = vec![user(&"old".repeat(15000)), user("latest")];
        let packet = build(&config(), &history, false, &routes()).unwrap();
        let state = state(&packet);
        assert_eq!(
            state["user_requests_and_summaries"][0]["user"]["text"]
                .as_str()
                .unwrap()
                .len(),
            512
        );
        assert_eq!(
            state["user_requests_and_summaries"][1]["user"]["text"],
            "latest"
        );
        assert_eq!(packet["omitted_requests"], 0);
        for history in [
            vec![user(&"x".repeat(40000))],
            vec![
                user("task"),
                json!({"kind":"summary","summary":{"text":"s".repeat(40000)}}),
            ],
        ] {
            assert!(build(&config(), &history, false, &routes())
                .unwrap_err()
                .starts_with("critical routing state or latest segment does not fit:"));
        }
        let mut config = config();
        config["project_instructions"] = json!("p".repeat(40000));
        assert!(build(&config, &[user("task")], false, &routes()).is_err());
    }

    #[test]
    fn oldest_requests_are_omitted_after_previews_and_their_markers_are_removed() {
        let mut history: Vec<_> = (0..70)
            .map(|index| user(&format!("old-{index}:{}", "x".repeat(600))))
            .collect();
        history.push(user("latest"));
        let packet = build(&config(), &history, false, &routes()).unwrap();
        let omitted = packet["omitted_requests"].as_u64().unwrap();
        assert!(omitted > 0);
        let state = state(&packet);
        let critical = state["user_requests_and_summaries"].as_array().unwrap();
        assert_eq!(critical[0]["entry"], omitted + 1);
        assert_eq!(critical.last().unwrap()["user"]["text"], "latest");
        assert!(packet["truncations"]
            .as_array()
            .unwrap()
            .iter()
            .all(|marker| marker["entry"].as_u64().unwrap() > omitted));
    }

    #[test]
    fn omitting_all_previewed_requests_retains_empty_markers_until_backfill_copies_them() {
        let mut config = config();
        config["project_instructions"] = json!("");
        let baseline = build(&config, &[user("latest")], false, &routes()).unwrap();
        let body = jev::encode_request(baseline["state"].as_str().unwrap(), &routes()).unwrap();
        config["project_instructions"] =
            json!("p".repeat(32768 - types::json_bytes(&body).len() - 256));
        for (shell_output, expected, recent_count) in [
            (None, json!([]), 0),
            (Some("x".to_owned()), Value::Null, 1),
            (Some("x".repeat(512)), json!([]), 0),
        ] {
            let mut history = Vec::new();
            if let Some(output) = shell_output {
                history.push(json!({"kind":"shell","shell":{"command":"cmd","output":output}}));
            }
            history.extend([user(&"old".repeat(1000)), user("latest")]);
            let packet = build(&config, &history, false, &routes()).unwrap();
            assert_eq!(packet["omitted_requests"], 1);
            assert_eq!(packet["truncations"], expected);
            let state = state(&packet);
            assert!(state.get("previews").is_none());
            assert_eq!(
                state["user_requests_and_summaries"]
                    .as_array()
                    .unwrap()
                    .len(),
                1
            );
            assert_eq!(
                state["recent_evidence"].as_array().map_or(0, Vec::len),
                recent_count
            );
        }
    }

    #[test]
    fn oversized_tool_json_becomes_source_preview_and_shell_markers_are_ordered() {
        let source = format!("{{ \"data\": \"{}\" }}", "v".repeat(600));
        let history = vec![
            user("task"),
            json!({"kind":"assistant","assistant":{"blocks":[{"kind":"tool_call","call":{"call_id":"c","name":"read_file","arguments":"{}"}}],"native":{}}}),
            json!({"kind":"tool","tool":{"call_id":"c","name":"read_file","outcome":{"ok":true,"code":"ok","message":"m".repeat(513),"data":raw::new(source.as_bytes()),"truncated":false,"effect":"none"}}}),
            json!({"kind":"shell","shell":{"command":"c".repeat(513),"output":"o".repeat(514),"output_truncated":false}}),
        ];
        let packet = build(&config(), &history, false, &routes()).unwrap();
        let state = state(&packet);
        let outcome = &state["recent_evidence"][1]["tool"]["outcome"];
        assert_eq!(outcome["data"], &source[..512]);
        assert_eq!(outcome["truncated"], true);
        assert_eq!(
            state["recent_evidence"][2]["shell"]["output_truncated"],
            true
        );
        let fields: Vec<_> = packet["truncations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|marker| text(marker, "field"))
            .collect();
        assert_eq!(
            fields,
            ["tool.message", "tool.data", "shell.command", "shell.output"]
        );
    }

    #[test]
    fn complete_escaped_request_is_the_size_boundary() {
        let mut config = config();
        config["project_instructions"] = json!("");
        let packet = build(&config, &[user("task")], false, &routes()).unwrap();
        let body = jev::encode_request(packet["state"].as_str().unwrap(), &routes()).unwrap();
        let padding = 32768 - types::json_bytes(&body).len();
        config["project_instructions"] = json!("p".repeat(padding));
        assert!(build(&config, &[user("task")], false, &routes()).is_ok());
        config["project_instructions"] = json!("p".repeat(padding + 1));
        assert!(build(&config, &[user("task")], false, &routes()).is_err());
        config["project_instructions"] = json!("<".repeat(padding / 2));
        assert!(build(&config, &[user("task")], false, &routes()).is_err());
    }
}

#[cfg(test)]
mod boundary_tests {
    use super::*;

    fn build_history(history: &[Value]) -> Value {
        let routes: Vec<_> = ["low", "medium"].into_iter().map(|effort| json!({"id":effort,"model":"gpt-6-sol","reasoning_effort":effort,"description":"route"})).collect();
        build(&json!({"model":"gpt-6-sol"}), history, false, &routes).unwrap()
    }

    #[test]
    fn plan_tags_are_exact_lines_joined_across_text_blocks_even_with_invalid_bytes() {
        for (blocks, expected) in [
            (
                vec![json!({"kind":"text","text":" <plan>\nx\n</plan>"})],
                false,
            ),
            (
                vec![json!({"kind":"text","text":"<plan>\r\nx\r\n</plan>\r"})],
                false,
            ),
            (
                vec![json!({"kind":"refusal","text":"<plan>\nx\n</plan>"})],
                false,
            ),
            (
                vec![
                    json!({"kind":"text","text":"<plan>"}),
                    json!({"kind":"text","text":" \t"}),
                    json!({"kind":"text","text":"</plan>"}),
                ],
                true,
            ),
            (
                vec![json!({"kind":"text","text":"<plan>\nx\n</plan>\n<plan>"})],
                false,
            ),
            (
                vec![json!({"kind":"text","text":raw::string(b"<plan>\n\xff\n</plan>")})],
                true,
            ),
        ] {
            let packet = build_history(&[
                json!({"kind":"user","user":{"text":"task"}}),
                json!({"kind":"assistant","assistant":{"blocks":blocks}}),
            ]);
            let state: Value = serde_json::from_str(packet["state"].as_str().unwrap()).unwrap();
            assert_eq!(
                state.get("latest_complete_plan").is_some(),
                expected,
                "{blocks:?}"
            );
        }
    }

    #[test]
    fn empty_assistant_blocks_preview_as_null_and_invalid_bytes_count_before_escaping() {
        let packet = build_history(&[
            json!({"kind":"user","user":{"text":"task"}}),
            json!({"kind":"assistant","assistant":{"blocks":[]}}),
        ]);
        assert!(packet["state"]
            .as_str()
            .unwrap()
            .contains("\"blocks\":null"));
        let mut bytes = vec![b'x'; 511];
        bytes.extend_from_slice(b"\xffyz");
        let packet = build_history(&[
            json!({"kind":"user","user":{"text":"task"}}),
            json!({"kind":"assistant","assistant":{"blocks":[{"kind":"text","text":raw::string(&bytes)}]}}),
        ]);
        assert_eq!(packet["truncations"][0]["original_bytes"], 514);
        assert!(packet["state"].as_str().unwrap().contains("\\ufffd"));
        let state: Value = serde_json::from_str(packet["state"].as_str().unwrap()).unwrap();
        assert_eq!(
            state["recent_evidence"][0]["assistant"]["blocks"][0]["text"],
            format!("{}�", "x".repeat(511))
        );
    }

    #[test]
    fn backfill_keeps_a_contiguous_newest_suffix_and_appends_markers_in_reverse_entry_order() {
        let mut history = vec![json!({"kind":"user","user":{"text":"task"}})];
        history.extend(
            (0..80).map(
                |_| json!({"kind":"shell","shell":{"command":"cmd","output":"x".repeat(800)}}),
            ),
        );
        history.push(
            json!({"kind":"assistant","assistant":{"blocks":[{"kind":"text","text":"latest"}]}}),
        );
        let packet = build_history(&history);
        let state: Value = serde_json::from_str(packet["state"].as_str().unwrap()).unwrap();
        let omitted = packet["omitted_earlier"].as_u64().unwrap();
        assert!(omitted > 0);
        let recent = state["recent_evidence"].as_array().unwrap();
        assert_eq!(recent.len() as u64 + omitted, 81);
        for (index, entry) in recent.iter().enumerate() {
            assert_eq!(entry["entry"], omitted + 2 + index as u64);
        }
        let markers = packet["truncations"].as_array().unwrap();
        assert_eq!(markers.len() + 1, recent.len());
        for (index, marker) in markers.iter().enumerate() {
            assert_eq!(marker["entry"], 81 - index as u64);
        }
    }
}
