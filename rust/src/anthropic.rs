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
    let tools:Vec<Value>=array(req,"tools").iter().map(|t|json!({"name":s(t,"name"),"description":s(t,"description"),"input_schema":t["input_schema"]})).collect();
    if !tools.is_empty() {
        body["tools"] = json!(tools);
    }
    body["tool_choice"] = json!({"type":"auto","disable_parallel_tool_use":false});
    if !s(req, "reasoning_effort").is_empty() {
        body["output_config"] = json!({"effort":s(req,"reasoning_effort")});
    }
    crate::openai::check_size(&body)?;
    Ok(body)
}

pub fn normalize(raw: &[u8]) -> Result<Value, ModelError> {
    let body = crate::provider::decode_json(raw)
        .map_err(|_| error("protocol_error", "response is not valid JSON"))?;
    if !crate::provider::wire_fields(
        &body,
        &["id", "model", "stop_reason"],
        &[],
        &["content"],
        &["usage", "stop_details"],
    ) || !crate::provider::wire_fields(
        &body["usage"],
        &[],
        &[
            "input_tokens",
            "output_tokens",
            "cache_creation_input_tokens",
            "cache_read_input_tokens",
        ],
        &[],
        &[],
    ) {
        return Err(error("protocol_error", "response is not valid JSON"));
    }
    let u = &body["usage"];
    let mut usage = usage_zero(!u.is_null());
    usage["input_tokens"] = json!(
        u["input_tokens"].as_i64().unwrap_or(0)
            + u["cache_creation_input_tokens"].as_i64().unwrap_or(0)
            + u["cache_read_input_tokens"].as_i64().unwrap_or(0)
    );
    usage["cached_input_tokens"] = json!(u["cache_read_input_tokens"].as_i64().unwrap_or(0));
    usage["output_tokens"] = json!(u["output_tokens"].as_i64().unwrap_or(0));
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
    for (i, b) in array(&body, "content").iter().enumerate() {
        if !crate::provider::wire_fields(b, &["type", "text", "id", "name"], &[], &[], &[]) {
            return Err(error("protocol_error", "content block is not valid JSON"));
        }
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
                return Err(error(
                    "protocol_error",
                    &format!("unsupported content block type {}", other),
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

#[cfg(test)]
mod tests {
    use super::*;
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
    fn raw_arguments_and_usage() {
        let r=normalize(br#"{"stop_reason":"tool_use","content":[{"type":"tool_use","id":"x","name":"echo","input":{ "a" : 1 }}],"usage":{"input_tokens":3,"cache_read_input_tokens":7,"cache_creation_input_tokens":2,"output_tokens":1}}"#).unwrap();
        assert_eq!(r["blocks"][0]["call"]["arguments"], "{ \"a\" : 1 }");
        assert_eq!(r["usage"]["input_tokens"], 12);
    }
}
