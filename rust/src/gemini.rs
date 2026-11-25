use crate::provider::{array, error, raw_tokens, s};
use crate::types::{json_bytes, json_string, usage_zero, ModelError};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

/// Encode the full local history as a stateless Gemini Interactions request.
pub fn encode_request(req: &Value) -> Result<Value, String> {
    if req["web_search"] == true {
        return Err("Gemini web search is not supported".into());
    }
    let mut input = Vec::new();
    for entry in array(req, "history") {
        // Model-switch handoffs and external image tools can retain preview metadata.
        let omitted = (!crate::image_context::entry_references(entry).is_empty()).then(|| {
            json!({"type":"text","text":"Image previews are metadata only; pixels are not supplied to Gemini."})
        });
        match s(entry, "kind") {
            "user" | "summary" | "shell" => {
                let content: Vec<Value> = crate::openai::entry_texts(entry)
                    .into_iter()
                    .map(|text| json!({"type":"text","text":text}))
                    .chain(omitted)
                    .collect();
                if !content.is_empty() {
                    input.push(json!({"type":"user_input","content":content}));
                }
            }
            "assistant" => {
                let native = &entry["assistant"]["native"];
                if s(native, "provider") != "gemini.interactions" {
                    return Err(format!("assistant turn carries {:?} items, which this adapter cannot continue from", s(native, "provider")));
                }
                if array(native, "items").is_empty() {
                    return Err("assistant turn carries no provider items to continue from".into());
                }
                input.extend_from_slice(array(native, "items"));
            }
            "tool" => {
                let tool = &entry["tool"];
                let mut result = vec![json!({"type":"text","text":json_string(&tool["outcome"])})];
                result.extend(omitted);
                input.push(json!({"type":"function_result","call_id":s(tool,"call_id"),"name":s(tool,"name"),"result":result,"is_error":tool["outcome"]["ok"] != true}));
            }
            _ => {}
        }
    }
    let mut body = json!({"model":s(req,"model"),"system_instruction":s(req,"instructions"),"input":input,"store":false});
    let tools: Vec<Value> = array(req, "tools").iter().map(|tool| json!({"type":"function","name":s(tool,"name"),"description":s(tool,"description"),"parameters":tool["input_schema"]})).collect();
    if !tools.is_empty() {
        body["tools"] = json!(tools);
    }
    let mut generation = json!({});
    if !s(req, "reasoning_effort").is_empty() {
        generation["thinking_level"] = req["reasoning_effort"].clone();
    }
    if req["max_output_tokens"].as_u64().is_some_and(|n| n > 0) {
        generation["max_output_tokens"] = req["max_output_tokens"].clone();
    }
    if generation.as_object().is_some_and(|v| !v.is_empty()) {
        body["generation_config"] = generation;
    }
    crate::openai::check_size(&body)?;
    Ok(body)
}

/// Normalize a completed Interaction while retaining its native steps for replay.
pub fn normalize(raw: &[u8]) -> Result<Value, ModelError> {
    let body = crate::provider::decode_json(raw)
        .map_err(|_| error("protocol_error", "Interaction is not valid JSON"))?;
    let totals = usage(&body);
    let fail = |status: &str, message: &str| ModelError {
        status: status.into(),
        message: message.into(),
        usage: totals.clone(),
    };
    match s(&body, "status") {
        "completed" | "requires_action" => {}
        "incomplete" | "requires_continuation" => {
            return Err(fail(
                "incomplete_response",
                "provider returned an incomplete Interaction",
            ));
        }
        "failed" | "cancelled" => {
            return Err(fail(
                "provider_error",
                &format!(
                    "Interaction {}: {}",
                    s(&body, "status"),
                    s(&body["error"], "message")
                ),
            ));
        }
        _ => {
            return Err(fail(
                "protocol_error",
                "Interaction has no supported terminal status",
            ))
        }
    }
    let steps = body["steps"]
        .as_array()
        .ok_or_else(|| fail("protocol_error", "Interaction has no steps array"))?;
    let raw_steps = raw_tokens::array(raw_tokens::field(raw, "steps").unwrap_or(b"[]"));
    let mut blocks = Vec::new();
    let mut calls = BTreeSet::new();
    for (index, step) in steps.iter().enumerate() {
        match s(step, "type") {
            "model_output" => {
                let content = step["content"]
                    .as_array()
                    .ok_or_else(|| fail("protocol_error", "model_output has no content array"))?;
                let mut text = String::new();
                for part in content {
                    if s(part, "type") != "text" || !part["text"].is_string() {
                        return Err(fail("protocol_error", "unsupported model_output content"));
                    }
                    text.push_str(s(part, "text"));
                }
                if !text.is_empty() {
                    blocks.push(crate::openai::text_block("text", &text));
                }
            }
            "thought" => {
                if step.get("signature").is_some_and(|v| !v.is_string()) {
                    return Err(fail("protocol_error", "thought signature is not a string"));
                }
            }
            "function_call" => {
                let id = s(step, "id");
                if id.is_empty()
                    || s(step, "name").is_empty()
                    || !step["arguments"].is_object()
                    || !calls.insert(id)
                {
                    return Err(fail("protocol_error", "invalid or duplicate function_call"));
                }
                let arguments = raw_steps
                    .get(index)
                    .and_then(|step| raw_tokens::field(step, "arguments"))
                    .ok_or_else(|| fail("protocol_error", "function_call has no arguments"))?;
                let arguments = String::from_utf8_lossy(arguments).into_owned();
                blocks.push(json!({"kind":"tool_call","call":{"call_id":id,"name":s(step,"name"),"arguments":arguments}}));
            }
            _ => return Err(fail("protocol_error", "unsupported Interaction step type")),
        }
    }
    if (s(&body, "status") == "requires_action") != !calls.is_empty() {
        return Err(fail(
            "protocol_error",
            "Interaction status does not match its function calls",
        ));
    }
    if blocks.is_empty() {
        return Err(fail(
            "protocol_error",
            "Interaction contains no visible output or function calls",
        ));
    }
    let mut response = json!({"blocks":blocks,"native":{"provider":"gemini.interactions","items":raw_steps.into_iter().map(crate::raw::new).collect::<Vec<_>>()},"usage":totals});
    for (source, target) in [("id", "response_id"), ("model", "model")] {
        if !s(&body, source).is_empty() {
            response[target] = body[source].clone();
        }
    }
    Ok(response)
}

/// Account for cached prompt tokens and separately reported thought tokens.
pub(crate) fn usage(body: &Value) -> Value {
    let u = &body["usage"];
    let count = |key: &str| u[key].as_u64().unwrap_or(0);
    let mut totals = usage_zero(
        u["total_input_tokens"].as_u64().is_some() && u["total_output_tokens"].as_u64().is_some(),
    );
    totals["input_tokens"] = json!(count("total_input_tokens"));
    totals["cached_input_tokens"] = json!(count("total_cached_tokens"));
    totals["output_tokens"] =
        json!(count("total_output_tokens").saturating_add(count("total_thought_tokens")));
    totals["reasoning_tokens"] = json!(count("total_thought_tokens"));
    totals
}

/// Assemble a complete Interactions stream without accepting partial tool calls.
pub fn assemble(raw: &[u8]) -> Result<Value, ModelError> {
    let mut interaction = Value::Null;
    let mut steps = Vec::new();
    let mut open = BTreeSet::new();
    let mut arguments = BTreeMap::<usize, String>::new();
    let mut completed = false;
    let malformed = || error("protocol_error", "invalid Interactions stream event");
    let result = (|| {
        for data in crate::openai::stream_data(raw) {
            if data == b"[DONE]" {
                if !completed {
                    return Err(malformed());
                }
                break;
            }
            let event = crate::provider::decode_json(&data).map_err(|_| malformed())?;
            if completed {
                return Err(malformed());
            }
            match s(&event, "event_type") {
                "interaction.created" => {
                    if !interaction.is_null() || !event["interaction"].is_object() {
                        return Err(malformed());
                    }
                    interaction = event["interaction"].clone();
                }
                "interaction.status_update" => {
                    if interaction.is_null() || !event["status"].is_string() {
                        return Err(malformed());
                    }
                    interaction["status"] = event["status"].clone();
                }
                "step.start" => {
                    let index = event["index"]
                        .as_u64()
                        .and_then(|n| usize::try_from(n).ok())
                        .ok_or_else(malformed)?;
                    if interaction.is_null() || index != steps.len() || !event["step"].is_object() {
                        return Err(malformed());
                    }
                    open.insert(index);
                    steps.push(event["step"].clone());
                }
                "step.delta" => {
                    let index = event["index"]
                        .as_u64()
                        .and_then(|n| usize::try_from(n).ok())
                        .ok_or_else(malformed)?;
                    if !open.contains(&index) {
                        return Err(malformed());
                    }
                    let step = steps.get_mut(index).ok_or_else(malformed)?;
                    let delta = &event["delta"];
                    match (s(step, "type"), s(delta, "type")) {
                        ("function_call", "arguments_delta") => {
                            arguments
                                .entry(index)
                                .or_default()
                                .push_str(delta["arguments"].as_str().ok_or_else(malformed)?);
                        }
                        ("model_output", "text") => {
                            append_text(
                                step,
                                "content",
                                delta["text"].as_str().ok_or_else(malformed)?,
                            )?;
                        }
                        ("thought", "thought_signature") => {
                            let signature = delta["signature"].as_str().ok_or_else(malformed)?;
                            let previous = step
                                .get("signature")
                                .map(|v| v.as_str().ok_or_else(malformed))
                                .transpose()?
                                .unwrap_or("");
                            step["signature"] = json!(format!("{previous}{signature}"));
                        }
                        ("thought", "thought_summary") => {
                            if s(&delta["content"], "type") != "text" {
                                return Err(malformed());
                            }
                            append_text(
                                step,
                                "summary",
                                delta["content"]["text"].as_str().ok_or_else(malformed)?,
                            )?;
                        }
                        _ => return Err(malformed()),
                    }
                }
                "step.stop" => {
                    let index = event["index"]
                        .as_u64()
                        .and_then(|n| usize::try_from(n).ok())
                        .ok_or_else(malformed)?;
                    if !open.remove(&index) {
                        return Err(malformed());
                    }
                    if event["usage"].is_object() {
                        interaction["usage"] = event["usage"].clone();
                    }
                }
                "interaction.completed" => {
                    if interaction.is_null()
                        || !open.is_empty()
                        || !event["interaction"].is_object()
                    {
                        return Err(malformed());
                    }
                    for (key, value) in event["interaction"].as_object().unwrap() {
                        interaction[key] = value.clone();
                    }
                    completed = true;
                }
                "error" => {
                    return Err(error(
                        "provider_error",
                        &format!(
                            "provider reported a stream error: {}",
                            s(&event["error"], "message")
                        ),
                    ))
                }
                _ => return Err(malformed()),
            }
        }
        if !completed {
            return Err(error(
                "provider_error",
                "stream ended before the Interaction completed",
            ));
        }
        for (index, text) in arguments {
            let parsed = crate::provider::decode_json(text.as_bytes()).map_err(|_| malformed())?;
            if !parsed.is_object() {
                return Err(malformed());
            }
            steps[index]["arguments"] = crate::raw::new(text.as_bytes());
        }
        interaction["steps"] = json!(steps);
        normalize(&json_bytes(&interaction))?;
        Ok(())
    })();
    result.map_err(|mut failure| {
        failure.usage = usage(&interaction);
        if !completed {
            failure.usage["known"] = json!(false);
        }
        failure
    })?;
    Ok(interaction)
}

fn append_text(step: &mut Value, field: &str, text: &str) -> Result<(), ModelError> {
    let malformed = || error("protocol_error", "invalid Interactions text delta");
    if step.get(field).is_none() {
        step[field] = json!([]);
    }
    let content = step[field].as_array_mut().ok_or_else(malformed)?;
    if let Some(last) = content.last_mut().filter(|part| s(part, "type") == "text") {
        let Value::String(previous) = &mut last["text"] else {
            return Err(malformed());
        };
        previous.push_str(text);
    } else {
        content.push(json!({"type":"text","text":text}));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream(events: &[Value]) -> Vec<u8> {
        events
            .iter()
            .map(|event| format!("data: {}\n\n", json_string(event)))
            .collect::<String>()
            .into_bytes()
    }

    fn events() -> Vec<Value> {
        vec![
            json!({"event_type":"interaction.created","interaction":{"id":"i","model":"gemini-4-hydrogen","status":"in_progress"}}),
            json!({"event_type":"step.start","index":0,"step":{"type":"thought"}}),
            json!({"event_type":"step.delta","index":0,"delta":{"type":"thought_signature","signature":"opaque"}}),
            json!({"event_type":"step.delta","index":0,"delta":{"type":"thought_summary","content":{"type":"text","text":"hidden"}}}),
            json!({"event_type":"step.stop","index":0}),
            json!({"event_type":"step.start","index":1,"step":{"type":"function_call","id":"a","name":"read_file","arguments":{}}}),
            json!({"event_type":"step.delta","index":1,"delta":{"type":"arguments_delta","arguments":"{\"path\":"}}),
            json!({"event_type":"step.delta","index":1,"delta":{"type":"arguments_delta","arguments":"\"README.md\"}"}}),
            json!({"event_type":"step.stop","index":1}),
            json!({"event_type":"step.start","index":2,"step":{"type":"function_call","id":"b","name":"list_files","arguments":{}}}),
            json!({"event_type":"step.stop","index":2}),
            json!({"event_type":"interaction.completed","interaction":{"id":"i","status":"requires_action","usage":{"total_input_tokens":100,"total_cached_tokens":30,"total_output_tokens":12,"total_thought_tokens":8}}}),
        ]
    }

    #[test]
    fn streamed_parallel_calls_and_thoughts_replay_with_tool_results() {
        let body = assemble(&stream(&events())).unwrap();
        let reply = normalize(&json_bytes(&body)).unwrap();
        assert_eq!(reply["blocks"].as_array().unwrap().len(), 2);
        assert_eq!(
            crate::provider::decode_json(
                reply["blocks"][0]["call"]["arguments"]
                    .as_str()
                    .unwrap()
                    .as_bytes()
            )
            .unwrap(),
            json!({"path":"README.md"})
        );
        assert_eq!(
            reply["usage"],
            json!({"known":true,"input_tokens":100,"cached_input_tokens":30,"output_tokens":20,"reasoning_tokens":8})
        );
        let request = encode_request(&json!({"model":"gemini-4-hydrogen","instructions":"system","reasoning_effort":"high","max_output_tokens":123,"tools":[{"name":"read_file","description":"read","input_schema":{"type":"object"}}],"history":[{"kind":"user","user":{"text":"hello"}},{"kind":"assistant","assistant":reply},{"kind":"tool","tool":{"call_id":"a","name":"read_file","outcome":{"ok":false,"message":"missing"}}}]})).unwrap();
        let request = crate::provider::decode_json(&json_bytes(&request)).unwrap();
        assert_eq!(request["store"], false);
        assert!(request.get("previous_interaction_id").is_none());
        assert_eq!(
            request["generation_config"],
            json!({"thinking_level":"high","max_output_tokens":123})
        );
        assert_eq!(request["tools"][0]["parameters"], json!({"type":"object"}));
        let input = request["input"].as_array().unwrap();
        assert_eq!(input[0]["content"][0]["text"], "hello");
        assert!(input.iter().any(|step| step["signature"] == "opaque"));
        assert_eq!(input.last().unwrap()["call_id"], "a");
        assert_eq!(input.last().unwrap()["is_error"], true);
    }

    #[test]
    fn native_steps_preserve_opaque_bytes_and_text_does_not_expose_thoughts() {
        let raw = br#"{"status":"completed","steps":[{"type":"thought","signature":"\u0061","extra":1e+02},{"type":"model_output","content":[{"type":"text","text":"answer"}]}]}"#;
        let reply = normalize(raw).unwrap();
        assert_eq!(reply["blocks"][0]["text"], "answer");
        assert_eq!(reply["blocks"].as_array().unwrap().len(), 1);
        assert_eq!(
            crate::raw::bytes(&reply["native"]["items"][0]).unwrap(),
            br#"{"type":"thought","signature":"\u0061","extra":1e+02}"#
        );
        let request =
            encode_request(&json!({"history":[{"kind":"assistant","assistant":reply}]})).unwrap();
        assert!(String::from_utf8(json_bytes(&request))
            .unwrap()
            .contains("1e+02"));
    }

    #[test]
    fn incomplete_or_malformed_interactions_never_release_calls() {
        for body in [
            json!({"status":"incomplete","steps":[]}),
            json!({"status":"in_progress","steps":[]}),
            json!({"status":"requires_action","steps":[]}),
            json!({"status":"completed","steps":[{"type":"function_call","id":"a","name":"x","arguments":{}}]}),
            json!({"status":"requires_action","steps":[{"type":"function_call","id":"a","name":"x","arguments":"{}"}]}),
            json!({"status":"completed","steps":[{"type":"model_output","content":[{"type":"image","data":"pixels"}]}]}),
        ] {
            assert!(normalize(&json_bytes(&body)).is_err(), "{body}");
        }
        let good = events();
        for length in 0..good.len() {
            assert!(assemble(&stream(&good[..length])).is_err());
        }
        for index in [4, 8, 10] {
            let mut missing_stop = good.clone();
            missing_stop.remove(index);
            assert!(assemble(&stream(&missing_stop)).is_err());
        }
        let mut duplicate = good.clone();
        duplicate[9]["step"]["id"] = json!("a");
        assert!(assemble(&stream(&duplicate)).is_err());
        let mut failed = good.clone();
        failed[11] = json!({"event_type":"error","error":{"message":"overloaded"}});
        assert_eq!(
            assemble(&stream(&failed)).unwrap_err().status,
            "provider_error"
        );
        let mut incomplete = good;
        incomplete[11]["interaction"]["status"] = json!("incomplete");
        let failure = assemble(&stream(&incomplete)).unwrap_err();
        assert_eq!(failure.status, "incomplete_response");
        assert_eq!(failure.usage["output_tokens"], 20);
    }

    #[test]
    fn adjacent_model_output_text_parts_join() {
        let body = json!({"status":"completed","steps":[{"type":"model_output","content":[{"type":"text","text":"one "},{"type":"text","text":"paragraph"}]}]});
        let reply = normalize(&json_bytes(&body)).unwrap();
        assert_eq!(reply["blocks"].as_array().unwrap().len(), 1);
        assert_eq!(reply["blocks"][0]["text"], "one paragraph");
    }

    #[test]
    fn documented_terminal_status_rejects_token_limits_and_preserves_refusal_text() {
        let mut events = vec![
            json!({"event_type":"interaction.created","interaction":{"status":"in_progress"}}),
            json!({"event_type":"step.start","index":0,"step":{"type":"model_output"}}),
            json!({"event_type":"step.delta","index":0,"delta":{"type":"text","text":"I cannot help with that request."}}),
            json!({"event_type":"step.stop","index":0}),
            json!({"event_type":"interaction.completed","interaction":{"status":"completed","usage":{"total_input_tokens":5,"total_output_tokens":7}}}),
        ];
        let body = assemble(&stream(&events)).unwrap();
        let reply = normalize(&json_bytes(&body)).unwrap();
        assert_eq!(reply["blocks"][0]["kind"], "text");
        assert_eq!(
            reply["blocks"][0]["text"],
            "I cannot help with that request."
        );
        events[4]["interaction"]["status"] = json!("incomplete");
        let failure = assemble(&stream(&events)).unwrap_err();
        assert_eq!(failure.status, "incomplete_response");
        assert_eq!(failure.usage["output_tokens"], 7);
    }

    #[test]
    fn image_handoffs_and_external_image_results_remain_text_only() {
        let request = encode_request(&json!({"image_generation":true,"history":[
            {"kind":"user","user":{"text":"Earlier image: diagram.png; pixels not supplied","images":[{"path":"nonexistent.png"}]}},
            {"kind":"tool","tool":{"call_id":"image","name":"generate_image","outcome":{"ok":true,"code":"ok","data":{"preview":{"path":"nonexistent.png"}}}}}
        ]})).unwrap();
        assert_eq!(
            request["input"][0]["content"][0]["text"],
            "Earlier image: diagram.png; pixels not supplied"
        );
        for content in [
            &request["input"][0]["content"],
            &request["input"][1]["result"],
        ] {
            let parts = content.as_array().unwrap();
            assert!(parts.iter().all(|part| part["type"] == "text"));
            assert!(s(parts.last().unwrap(), "text").contains("pixels are not supplied"));
        }
    }

    #[test]
    fn streamed_text_is_joined_and_unsupported_requests_are_rejected() {
        let events = vec![
            json!({"event_type":"interaction.created","interaction":{"status":"in_progress"}}),
            json!({"event_type":"step.start","index":0,"step":{"type":"model_output"}}),
            json!({"event_type":"step.delta","index":0,"delta":{"type":"text","text":"hello "}}),
            json!({"event_type":"step.delta","index":0,"delta":{"type":"text","text":"world"}}),
            json!({"event_type":"step.stop","index":0}),
            json!({"event_type":"interaction.completed","interaction":{"status":"completed"}}),
        ];
        let reply = normalize(&json_bytes(&assemble(&stream(&events)).unwrap())).unwrap();
        assert_eq!(reply["blocks"][0]["text"], "hello world");
        for req in [
            json!({"web_search":true}),
            json!({"history":[{"kind":"assistant","assistant":{"native":{"provider":"openai.responses","items":[{}]}}}]}),
            json!({"history":[{"kind":"assistant","assistant":{"native":{"provider":"gemini.interactions","items":[]}}}]}),
        ] {
            assert!(encode_request(&req).is_err());
        }
        assert_eq!(
            usage(&json!({"usage":{"total_output_tokens":2}}))["known"],
            false
        );
    }
}
