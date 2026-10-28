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
    fn raw_arguments_and_usage() {
        let r=normalize(br#"{"stop_reason":"tool_use","content":[{"type":"tool_use","id":"x","name":"echo","input":{ "a" : 1 }}],"usage":{"input_tokens":3,"cache_read_input_tokens":7,"cache_creation_input_tokens":2,"output_tokens":1}}"#).unwrap();
        assert_eq!(r["blocks"][0]["call"]["arguments"], "{ \"a\" : 1 }");
        assert_eq!(r["usage"]["input_tokens"], 12);
    }
}
