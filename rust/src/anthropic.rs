use crate::provider::{array, error, raw_tokens, s};
use crate::types::{json_string, usage_zero, ModelError};
use serde_json::{json, Value};

fn mark(messages: &mut [Value]) {
    if let Some(last) = messages.last_mut() {
        if s(last, "role") == "user" {
            if let Some(block) = last["content"].as_array_mut().and_then(|a| a.last_mut()) {
                block["cache_control"] = json!({"type":"ephemeral"});
            }
        }
    }
}
fn flush(messages: &mut Vec<Value>, pending: &mut Vec<Value>) {
    if !pending.is_empty() {
        messages.push(json!({"role":"user","content":std::mem::take(pending)}));
    }
}

pub fn encode_request(req: &Value) -> Result<Value, String> {
    let mut messages = Vec::new();
    let mut pending = Vec::new();
    for (i, entry) in array(req, "history").iter().enumerate() {
        match s(entry, "kind") {
            "user" | "summary" | "shell" => {
                flush(&mut messages, &mut pending);
                for text in crate::openai::entry_texts(entry) {
                    let block = json!({"type":"text","text":text});
                    if messages.last().is_some_and(|m| s(m, "role") == "user") {
                        messages.last_mut().unwrap()["content"]
                            .as_array_mut()
                            .unwrap()
                            .push(block);
                    } else {
                        messages.push(json!({"role":"user","content":[block]}));
                    }
                }
            }
            "assistant" => {
                flush(&mut messages, &mut pending);
                let native = &entry["assistant"]["native"];
                if s(native, "provider") != "anthropic.messages" {
                    return Err(format!("assistant turn carries {:?} items, which this adapter cannot continue from",s(native,"provider")));
                }
                if array(native, "items").is_empty() {
                    return Err("assistant turn carries no provider items to continue from".into());
                }
                messages.push(json!({"role":"assistant","content":native["items"]}));
            }
            "tool" => {
                let tool = &entry["tool"];
                let mut block = json!({"type":"tool_result","tool_use_id":s(tool,"call_id"),"content":json_string(&tool["outcome"])});
                if !tool["outcome"]["ok"].as_bool().unwrap_or(false) {
                    block["is_error"] = json!(true);
                }
                pending.push(block);
            }
            _ => {}
        }
        if array(req, "cache_entries")
            .iter()
            .any(|n| n.as_u64() == Some((i + 1) as u64))
        {
            flush(&mut messages, &mut pending);
            mark(&mut messages);
        }
    }
    flush(&mut messages, &mut pending);
    if array(req, "cache_entries").len() < 3 {
        mark(&mut messages);
    }
    let max = req["max_output_tokens"]
        .as_i64()
        .filter(|n| *n > 0)
        .unwrap_or(16000);
    let mut body = json!({"model":s(req,"model"),"max_tokens":max,"system":[{"type":"text","text":s(req,"instructions"),"cache_control":{"type":"ephemeral"}}],"messages":if messages.is_empty(){Value::Null}else{json!(messages)}});
    if req["fast"] == true {
        body["speed"] = json!("fast");
    }
    let tools:Vec<Value>=array(req,"tools").iter().map(|t|json!({"name":s(t,"name"),"description":s(t,"description"),"input_schema":t["input_schema"]})).collect();
    if !tools.is_empty() {
        body["tools"] = json!(tools);
    }
    body["tool_choice"] = json!({"type":"auto","disable_parallel_tool_use":false});
    if !s(req, "reasoning_effort").is_empty() {
        body["output_config"] = json!({"effort":s(req,"reasoning_effort")});
    }
    // GC invalidates Opus 5.5's conversation-bound thinking signatures. Other
    // models require a thinking mode with this option; keep their defaults.
    if s(req, "model") == "claude-opus-5-5"
        && array(req, "history").iter().any(|entry| {
            s(entry, "kind") == "tool" && s(&entry["tool"]["outcome"], "code") == "context_gc"
        })
    {
        body["thinking"] = json!({"block_binding":{"prefix_mismatch_behavior":"drop_block"}});
    }
    crate::openai::check_size(&body)?;
    Ok(body)
}

pub fn normalize(raw: &[u8]) -> Result<Value, ModelError> {
    let body = crate::provider::wire::decode(raw, &crate::provider::wire::ANTHROPIC_BODY).map_err(
        |_| {
            let mut error = error("protocol_error", "response is not valid JSON");
            if let Ok(body) = crate::provider::decode_json(raw) {
                error.usage = usage(&body);
            }
            error
        },
    )?;
    let usage = usage(&body);
    let fail = |status: &str, msg: String| ModelError {
        status: status.into(),
        message: msg,
        usage: usage.clone(),
    };
    match s(&body, "stop_reason") {
        "end_turn" | "tool_use" | "stop_sequence" | "refusal" => {}
        "max_tokens" => {
            return Err(fail(
                "incomplete_response",
                "provider returned an incomplete response: max_tokens".into(),
            ))
        }
        "" => return Err(fail("protocol_error", "response has no stop_reason".into())),
        other => {
            return Err(fail(
                "protocol_error",
                format!("unsupported stop_reason {}", other),
            ))
        }
    }
    let raw_content = raw_tokens::array(raw_tokens::field(raw, "content").unwrap_or(b"[]"));
    let mut blocks = Vec::new();
    for (i, raw_block) in raw_content.iter().enumerate() {
        let b = crate::provider::wire::decode(raw_block, &crate::provider::wire::ANTHROPIC_BLOCK)
            .map_err(|_| fail("protocol_error", "content block is not valid JSON".into()))?;
        let b = &b;
        match s(b, "type") {
            "text" => blocks.push(crate::openai::text_block("text", s(b, "text"))),
            "thinking" | "redacted_thinking" => {}
            "tool_use" => {
                let arguments = raw_content
                    .get(i)
                    .and_then(|raw| raw_tokens::field(raw, "input"))
                    .map(|raw| String::from_utf8_lossy(raw).into_owned())
                    .unwrap_or_default();
                blocks.push(json!({"kind":"tool_call","call":{"call_id":s(b,"id"),"name":s(b,"name"),"arguments":arguments}}));
            }
            other => {
                return Err(fail(
                    "protocol_error",
                    format!("unsupported content block type {}", other),
                ))
            }
        }
    }
    if s(&body, "stop_reason") == "refusal" {
        let reason = s(&body["stop_details"], "explanation");
        blocks.push(crate::openai::text_block(
            "refusal",
            if reason.is_empty() {
                "the model declined this request"
            } else {
                reason
            },
        ));
    }
    let mut response = json!({});
    if !s(&body, "id").is_empty() {
        response["response_id"] = body["id"].clone();
    }
    if !s(&body, "model").is_empty() {
        response["model"] = body["model"].clone();
    }
    response["blocks"] = if blocks.is_empty() {
        Value::Null
    } else {
        json!(blocks)
    };
    response["native"] = json!({"provider":"anthropic.messages"});
    if !array(&body, "content").is_empty() {
        response["native"]["items"] = json!(raw_content
            .into_iter()
            .map(crate::raw::new)
            .collect::<Vec<_>>());
    }
    response["usage"] = usage;
    Ok(response)
}

fn usage(body: &Value) -> Value {
    let u = &body["usage"];
    let mut usage = usage_zero(!u.is_null());
    usage["input_tokens"] = json!(u["input_tokens"]
        .as_i64()
        .unwrap_or(0)
        .wrapping_add(u["cache_creation_input_tokens"].as_i64().unwrap_or(0))
        .wrapping_add(u["cache_read_input_tokens"].as_i64().unwrap_or(0)));
    usage["cached_input_tokens"] = json!(u["cache_read_input_tokens"].as_i64().unwrap_or(0));
    usage["output_tokens"] = json!(u["output_tokens"].as_i64().unwrap_or(0));
    usage
}

/// Assemble validates a completed Messages stream before normalizing its reply.
pub fn assemble(raw: &[u8]) -> Result<Value, ModelError> {
    let mut message = Value::Null;
    let mut blocks = Vec::new();
    let mut arguments = std::collections::BTreeMap::<usize, String>::new();
    let mut stopped = false;
    let mut open = std::collections::BTreeSet::new();
    let malformed = || error("protocol_error", "invalid Messages stream event");
    let mut final_usage = false;
    let result = (|| {
        for data in crate::openai::stream_data(raw) {
            let event: Value = serde_json::from_slice(&data).map_err(|_| malformed())?;
            match s(&event, "type") {
                "message_start" => {
                    if !message.is_null() || !event["message"].is_object() {
                        return Err(malformed());
                    }
                    message = event["message"].clone();
                }
                "content_block_start" => {
                    let index = event["index"]
                        .as_u64()
                        .and_then(|index| usize::try_from(index).ok())
                        .ok_or_else(malformed)?;
                    if index != blocks.len() || !event["content_block"].is_object() {
                        return Err(malformed());
                    }
                    if !message.is_object() {
                        return Err(malformed());
                    }
                    open.insert(index);
                    blocks.push(event["content_block"].clone());
                }
                "content_block_delta" => {
                    let index = event["index"]
                        .as_u64()
                        .and_then(|index| usize::try_from(index).ok())
                        .ok_or_else(malformed)?;
                    let block = blocks.get_mut(index).ok_or_else(malformed)?;
                    if !open.contains(&index) {
                        return Err(malformed());
                    }
                    let delta = &event["delta"];
                    let field = match s(delta, "type") {
                        "text_delta" => "text",
                        "thinking_delta" => "thinking",
                        "signature_delta" => "signature",
                        "input_json_delta" => {
                            if s(block, "type") != "tool_use" {
                                return Err(malformed());
                            }
                            arguments
                                .entry(index)
                                .or_default()
                                .push_str(delta["partial_json"].as_str().ok_or_else(malformed)?);
                            continue;
                        }
                        _ => return Err(malformed()),
                    };
                    let expected = match field {
                        "text" => "text",
                        _ => "thinking",
                    };
                    if s(block, "type") != expected {
                        return Err(malformed());
                    }
                    if block[field].is_null() {
                        block[field] = json!("");
                    }
                    let Value::String(text) = &mut block[field] else {
                        return Err(malformed());
                    };
                    text.push_str(delta[field].as_str().ok_or_else(malformed)?);
                }
                "content_block_stop" => {
                    let index = event["index"]
                        .as_u64()
                        .and_then(|index| usize::try_from(index).ok())
                        .ok_or_else(malformed)?;
                    if !open.remove(&index) {
                        return Err(malformed());
                    }
                }
                "message_delta" => {
                    if !message.is_object() {
                        return Err(malformed());
                    }
                    for (key, value) in event["delta"].as_object().ok_or_else(malformed)? {
                        message[key] = value.clone();
                    }
                    if let Some(usage) = event["usage"].as_object() {
                        final_usage |= usage.get("output_tokens").and_then(Value::as_i64).is_some()
                            && !s(&message, "stop_reason").is_empty();
                        for (key, value) in usage {
                            if !message["usage"].is_object() {
                                message["usage"] = json!({});
                            }
                            message["usage"][key] = value.clone();
                        }
                    }
                }
                "message_stop" => {
                    stopped = true;
                    break;
                }
                "error" => {
                    return Err(error(
                        "provider_error",
                        &format!(
                            "provider reported a stream error: {}",
                            s(&event["error"], "message")
                        ),
                    ));
                }
                "ping" => {}
                _ => return Err(malformed()),
            }
        }
        if s(&message, "stop_reason") == "max_tokens" {
            return Err(error(
                "incomplete_response",
                "provider returned an incomplete response: max_tokens",
            ));
        }
        if !stopped || !message.is_object() || !open.is_empty() {
            return Err(error(
                "provider_error",
                "stream ended before the message completed",
            ));
        }
        for (index, input) in arguments {
            crate::provider::decode_json(input.as_bytes()).map_err(|_| malformed())?;
            blocks[index]["input"] = crate::raw::new(input.as_bytes());
        }
        message["content"] = json!(blocks);
        Ok(())
    })();
    result.map_err(|mut error| {
        error.usage = usage(&message);
        error.usage["known"] =
            json!(final_usage && message["usage"]["input_tokens"].as_i64().is_some());
        error
    })?;
    Ok(message)
}

/// Transient classifies retryable Messages failures without accepting partial replies.
pub(crate) fn transient(raw: &[u8]) -> (String, Value) {
    let failure = match assemble(raw) {
        Ok(body) => return (String::new(), usage(&body)),
        Err(failure) => failure,
    };
    if failure.status != "provider_error" {
        return (String::new(), failure.usage);
    }
    for data in crate::openai::stream_data(raw) {
        let Ok(event) = serde_json::from_slice::<Value>(&data) else {
            break;
        };
        if s(&event, "type") == "message_stop" || s(&event["delta"], "stop_reason") == "max_tokens"
        {
            return (String::new(), failure.usage);
        }
        if s(&event, "type") == "error" {
            if matches!(
                s(&event["error"], "type"),
                "overloaded_error" | "rate_limit_error" | "api_error"
            ) {
                return ("provider stream overload".into(), failure.usage);
            }
            break;
        }
    }
    (
        if failure.message == "stream ended before the message completed" {
            "stream ended early".into()
        } else {
            String::new()
        },
        failure.usage,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fast_requests_select_speed_without_changing_default_bytes() {
        let mut request = json!({"model":"claude-opus-5-5","instructions":"p","history":[]});
        let plain = encode_request(&request).unwrap();
        assert!(plain.get("speed").is_none());
        for fast in [json!(false), Value::Null, json!("true"), json!(1)] {
            request["fast"] = fast;
            assert_eq!(
                crate::types::json_bytes(&encode_request(&request).unwrap()),
                crate::types::json_bytes(&plain)
            );
        }
        request["fast"] = true.into();
        let mut body = encode_request(&request).unwrap();
        assert_eq!(body["speed"], "fast");
        assert!(body.get("service_tier").is_none());
        assert!(body.get("fast").is_none());
        body.as_object_mut().unwrap().shift_remove("speed");
        assert_eq!(
            crate::types::json_bytes(&body),
            crate::types::json_bytes(&plain)
        );
    }

    #[test]
    fn completed_stream_preserves_thinking_tool_input_and_usage_for_continuation() {
        let events = [
            json!({"type":"message_start","message":{"id":"m","model":"claude","content":[],"usage":{"input_tokens":12,"output_tokens":1,"cache_read_input_tokens":4}}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"private"}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"signed"}}),
            json!({"type":"content_block_stop","index":0}),
            json!({"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}),
            json!({"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"hello\nworld"}}),
            json!({"type":"content_block_stop","index":1}),
            json!({"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"call","name":"read_file","input":{}}}),
            json!({"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"a\",\"n\":1.2300}"}}),
            json!({"type":"content_block_stop","index":2}),
            json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":20}}),
            json!({"type":"message_stop"}),
        ];
        let raw = events
            .iter()
            .map(|event| format!("data: {event}\r\n\r\n"))
            .collect::<String>();
        let body = assemble(raw.as_bytes()).unwrap();
        let response = normalize(&crate::types::json_bytes(&body)).unwrap();
        assert_eq!(response["blocks"][0]["text"], "hello\nworld");
        assert_eq!(response["usage"]["output_tokens"], 20);
        assert_eq!(response["usage"]["input_tokens"], 16);
        let request = encode_request(
            &json!({"model":"claude","history":[{"kind":"assistant","assistant":response}]}),
        )
        .unwrap();
        let native = &request["messages"][0]["content"];
        assert!(crate::types::json_string(native).contains("signed"));
        let encoded = String::from_utf8(crate::types::json_bytes(native)).unwrap();
        assert!(encoded.contains("1.2300"), "{encoded}");
        let cutoff = raw.rfind("data:").unwrap();
        assert_eq!(
            assemble(&raw.as_bytes()[..cutoff]).unwrap_err().status,
            "provider_error"
        );
        let failed = raw[..cutoff].to_owned()
            + "data: {\"type\":\"error\",\"error\":{\"message\":\"overloaded\"}}\n\n";
        assert!(assemble(failed.as_bytes())
            .unwrap_err()
            .message
            .contains("overloaded"));
    }

    #[test]
    fn malformed_and_errored_streams_never_produce_an_accepted_message() {
        let start = json!({"type":"message_start","message":{"content":[]}});
        let block = json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}});
        let stop = json!({"type":"message_stop"});
        for (events, status) in [
            (vec![stop.clone()], "provider_error"),
            (vec![start.clone(), start.clone()], "protocol_error"),
            (vec![block.clone()], "protocol_error"),
            (
                vec![start.clone(), block.clone(), stop.clone()],
                "provider_error",
            ),
            (
                vec![
                    start.clone(),
                    json!({"type":"content_block_start","index":u64::MAX,"content_block":{"type":"text"}}),
                ],
                "protocol_error",
            ),
            (
                vec![
                    start.clone(),
                    block.clone(),
                    json!({"type":"content_block_delta","index":u64::MAX,"delta":{"type":"text_delta","text":"x"}}),
                ],
                "protocol_error",
            ),
            (
                vec![
                    start.clone(),
                    block.clone(),
                    json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":42}}),
                ],
                "protocol_error",
            ),
            (
                vec![
                    start.clone(),
                    block.clone(),
                    json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{}"}}),
                ],
                "protocol_error",
            ),
            (
                vec![
                    start.clone(),
                    block.clone(),
                    json!({"type":"content_block_stop","index":0}),
                    json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"late"}}),
                ],
                "protocol_error",
            ),
            (
                vec![
                    start.clone(),
                    json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"c","name":"f","input":{}}}),
                    json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{"}}),
                    json!({"type":"content_block_stop","index":0}),
                    json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":2}}),
                    stop.clone(),
                ],
                "protocol_error",
            ),
            (
                vec![
                    start.clone(),
                    block.clone(),
                    json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"speculative"}}),
                    json!({"type":"error","error":{"type":"overloaded_error","message":"overloaded"}}),
                ],
                "provider_error",
            ),
        ] {
            let raw = events
                .iter()
                .map(|event| format!("data: {event}\n\n"))
                .collect::<String>();
            assert_eq!(
                assemble(raw.as_bytes()).unwrap_err().status,
                status,
                "{raw}"
            );
        }
        assert_eq!(
            assemble(b"data: {invalid}\n\n").unwrap_err().status,
            "protocol_error"
        );
    }

    #[test]
    fn tool_results_precede_following_user_parts_and_keep_inherited_markers() {
        let native = crate::raw::new(
            br#"{ "type":"tool_use", "id":"call", "name":"echo", "input":{"n":1e+02,"n":2} }"#,
        );
        let user = json!({"text":"task","workspace":{"kind":"workspace_state"},"roster":{"kind":"agent_roster","threads":[]},"plan":"on"});
        let outcome = json!({"ok":false,"code":"failed","message":"failure","data":null,"truncated":false,"effect":"none"});
        let request = json!({"model":"m","cache_entries":[1,3],"history":[
            {"kind":"user","user":user},
            {"kind":"assistant","assistant":{"native":{"provider":"anthropic.messages","items":[native]}}},
            {"kind":"tool","tool":{"call_id":"call","outcome":outcome}},
            {"kind":"user","user":user}
        ]});
        let body = encode_request(&request).unwrap();
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0]["content"].as_array().unwrap().len(), 4);
        assert_eq!(
            messages[0]["content"][3]["cache_control"]["type"],
            "ephemeral"
        );
        assert_eq!(
            crate::raw::bytes(&messages[1]["content"][0]),
            crate::raw::bytes(&native)
        );
        let content = messages[2]["content"].as_array().unwrap();
        assert_eq!(content.len(), 5);
        let expected = json!({"type":"tool_result","tool_use_id":"call","content":json_string(&outcome),"is_error":true,"cache_control":{"type":"ephemeral"}});
        assert_eq!(
            crate::types::json_bytes(&content[0]),
            crate::types::json_bytes(&expected)
        );
        for (block, text) in content[1..].iter().zip(crate::openai::entry_texts(
            &json!({"kind":"user","user":user}),
        )) {
            assert_eq!(block["type"], "text");
            assert_eq!(block["text"], text);
        }
        assert!(content[1]["cache_control"].is_null());
        assert!(content[2]["cache_control"].is_null());
        assert!(content[3]["cache_control"].is_null());
        assert_eq!(content[4]["cache_control"]["type"], "ephemeral");
        assert_eq!(request["history"][0]["user"], user);
    }
    #[test]
    fn gc_allows_prefix_bound_thinking_to_be_dropped_without_rewriting_history() {
        let response = normalize(br#"{"stop_reason":"tool_use","content":[{"type":"thinking","thinking":"private","signature":"bound"},{"type":"redacted_thinking","data":"opaque"},{"type":"tool_use","id":"a","name":"read_file","input":{}},{"type":"tool_use","id":"b","name":"read_file","input":{}}]}"#).unwrap();
        let later = normalize(br#"{"stop_reason":"end_turn","content":[{"type":"thinking","thinking":"later","signature":"later-bound"},{"type":"text","text":"done"}]}"#).unwrap();
        let accepted = vec![
            json!({"kind":"user","user":{"text":"task"}}),
            json!({"kind":"assistant","assistant":response}),
            json!({"kind":"tool","tool":{"call_id":"a","name":"read_file","outcome":{"ok":true,"code":"ok","data":{"text":"original"},"effect":"none"}}}),
            json!({"kind":"tool","tool":{"call_id":"b","name":"read_file","outcome":{"ok":true,"code":"ok","data":{"text":"current"},"effect":"none"}}}),
            json!({"kind":"assistant","assistant":later}),
            json!({"kind":"user","user":{"text":"continue"}}),
        ];
        let snapshot = accepted.clone();
        for gc in [false, true] {
            let dropped = if gc {
                json!({"a":{"placeholder":"[dropped: recall a]"}})
            } else {
                json!({})
            };
            let mut request = json!({"model":"claude-opus-5-5","history":crate::context_gc::history(&accepted, &dropped)});
            for followup in [false, true] {
                if followup {
                    request["history"].as_array_mut().unwrap().extend([
                        json!({"kind":"assistant","assistant":later}),
                        json!({"kind":"user","user":{"text":"continue"}}),
                    ]);
                }
                let body = encode_request(&request).unwrap();
                assert_eq!(body.get("thinking").is_some(), gc);
                if gc {
                    assert_eq!(
                        body["thinking"],
                        json!({"block_binding":{"prefix_mismatch_behavior":"drop_block"}})
                    );
                }
                assert_eq!(body["messages"][1]["content"], response["native"]["items"]);
                assert_eq!(body["messages"][3]["content"], later["native"]["items"]);
                for (i, id) in ["a", "b"].iter().enumerate() {
                    let result = &body["messages"][2]["content"][i];
                    assert_eq!(result["tool_use_id"], *id);
                    let outcome: Value = serde_json::from_str(s(result, "content")).unwrap();
                    assert_eq!(
                        outcome["code"],
                        if gc && i == 0 { "context_gc" } else { "ok" }
                    );
                }
            }
        }
        assert_eq!(accepted, snapshot);
    }

    #[test]
    fn gc_keeps_other_models_thinking_defaults_even_with_native_signatures() {
        let response = normalize(br#"{"stop_reason":"tool_use","content":[{"type":"thinking","thinking":"","signature":"opaque"},{"type":"tool_use","id":"a","name":"read_file","input":{}}]}"#).unwrap();
        let mut request = json!({"model":"claude-haiku-5-5","history":[
            {"kind":"user","user":{"text":"context_gc"}},
            {"kind":"assistant","assistant":response},
            {"kind":"tool","tool":{"call_id":"a","name":"read_file","outcome":{"ok":true,"code":"context_gc","message":"recall a","data":null,"effect":"none"}}}
        ]});
        for model in ["claude-haiku-5-5", "claude-sonnet-5-5", "unknown"] {
            request["model"] = json!(model);
            let body = encode_request(&request).unwrap();
            assert!(body.get("thinking").is_none());
        }
    }

    #[test]
    fn invalid_user_bytes_survive_text_block_and_cache_marker() {
        let text = crate::raw::string(b"raw\xff\xc0\x80\xef\xbf\xbd<&>\r");
        let request = json!({"model":"m","history":[{"kind":"user","user":{"text":text}}]});
        let body = encode_request(&request).unwrap();
        assert_eq!(body["messages"][0]["content"][0]["text"], text);
        assert_eq!(
            body["messages"][0]["content"][0]["cache_control"]["type"],
            "ephemeral"
        );
        let expected = "\"text\":\"raw\\ufffd\\ufffd\\ufffd�\\u003c\\u0026\\u003e\\r\"".as_bytes();
        let wire = crate::types::json_bytes(&body);
        assert!(wire
            .windows(expected.len())
            .any(|window| window == expected));
        assert!(!String::from_utf8(wire)
            .unwrap()
            .contains("__reagent_raw_string"));
    }
    #[test]
    fn usage_overflow_keeps_go_integer_wrapping_without_panicking() {
        let response = normalize(br#"{"stop_reason":"end_turn","content":[{"type":"text","text":"ok"}],"usage":{"input_tokens":9223372036854775807,"cache_creation_input_tokens":1}}"#).unwrap();
        assert_eq!(response["usage"]["input_tokens"], i64::MIN);
    }

    #[test]
    fn raw_arguments_and_usage() {
        let r=normalize(br#"{"stop_reason":"tool_use","content":[{"type":"tool_use","id":"x","name":"echo","input":{ "a" : 1 }}],"usage":{"input_tokens":3,"cache_read_input_tokens":7,"cache_creation_input_tokens":2,"output_tokens":1}}"#).unwrap();
        assert_eq!(r["blocks"][0]["call"]["arguments"], "{ \"a\" : 1 }");
        assert_eq!(r["usage"]["input_tokens"], 12);
    }
}
