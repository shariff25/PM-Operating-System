//! **Testing only:** a Claude model behind the gateway, for trying PM OS before the Spark's
//! local models are up (decided 2026-11-14).
//!
//! It is off unless `PMOS_GATEWAY_CLOUD_TEST=1` and a role's upstream is written
//! `anthropic+https://api.anthropic.com`. With it on, prompts leave the machine, so it is for
//! test workspaces with test data only, never with real PM tokens.
//!
//! Callers keep speaking the OpenAI chat shape. This module turns that request into a Claude
//! Messages API request (`POST /v1/messages`, raw HTTP: there is no official Rust SDK) and the
//! answer back into an OpenAI `chat.completion`:
//! - system messages become `system`; tool calls become `tool_use` blocks and tool results
//!   `tool_result` blocks, with consecutive results in one user message;
//! - sampling parameters (`temperature`, `top_p`) are dropped: the API version refuses them;
//! - forced tool choice (`required`, a named function) is not accepted by current models, so
//!   it becomes `auto` plus an instruction;
//! - `response_format` becomes an instruction to reply with JSON only;
//! - thinking blocks in the answer are not returned (the OpenAI shape has no place for them);
//! - a refusal is `finish_reason: "content_filter"`. Server-side refusal fallbacks are on.

use serde_json::{json, Map, Value};

/// The Messages API version sent in `anthropic-version`.
pub const API_VERSION: &str = "2026-10-21";
/// Beta header for server-side refusal fallbacks (`fallbacks: "default"`).
pub const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
/// `max_tokens` when the caller sets none, and the most a non-streamed call asks for.
const DEFAULT_MAX_TOKENS: u64 = 16_000;
const MAX_TOKENS_CAP: u64 = 32_000;

/// Effort levels the Messages API accepts in `output_config.effort`.
pub const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// The Messages API request for an OpenAI chat request. `model` is the Claude model id and
/// `effort` the role's effort, if set.
pub fn to_messages(req: &Value, model: &str, effort: Option<&str>) -> Result<Value, String> {
    let mut system: Vec<String> = Vec::new();
    let mut messages: Vec<Value> = Vec::new();
    for m in req["messages"]
        .as_array()
        .ok_or("`messages` must be a list")?
    {
        match m["role"].as_str().unwrap_or_default() {
            "system" | "developer" => {
                let text = text_of(&m["content"]);
                if !text.is_empty() {
                    system.push(text);
                }
            }
            "user" => push(&mut messages, "user", user_blocks(&m["content"])),
            "assistant" => {
                let mut blocks = Vec::new();
                let text = text_of(&m["content"]);
                if !text.trim().is_empty() {
                    blocks.push(json!({"type": "text", "text": text}));
                }
                for call in m["tool_calls"].as_array().into_iter().flatten() {
                    let args = call["function"]["arguments"].as_str().unwrap_or("{}");
                    let input: Value = serde_json::from_str(args)
                        .ok()
                        .filter(Value::is_object)
                        .unwrap_or_else(|| json!({}));
                    blocks.push(json!({
                        "type": "tool_use",
                        "id": call["id"].as_str().unwrap_or_default(),
                        "name": call["function"]["name"].as_str().unwrap_or_default(),
                        "input": input,
                    }));
                }
                if !blocks.is_empty() {
                    push(&mut messages, "assistant", blocks);
                }
            }
            "tool" => push(
                &mut messages,
                "user",
                vec![json!({
                    "type": "tool_result",
                    "tool_use_id": m["tool_call_id"].as_str().unwrap_or_default(),
                    "content": text_of(&m["content"]),
                })],
            ),
            other => return Err(format!("unsupported message role `{other}`")),
        }
    }
    if messages
        .first()
        .map(|m| m["role"] != "user")
        .unwrap_or(true)
    {
        messages.insert(
            0,
            json!({"role": "user", "content": [{"type": "text", "text": "(begin)"}]}),
        );
    }

    let mut out = Map::new();
    out.insert("model".into(), json!(model));
    let max = req["max_completion_tokens"]
        .as_u64()
        .or_else(|| req["max_tokens"].as_u64())
        .unwrap_or(DEFAULT_MAX_TOKENS)
        .clamp(1, MAX_TOKENS_CAP);
    out.insert("max_tokens".into(), json!(max));

    // Tools and tool choice.
    let tools: Vec<Value> = req["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|t| t["type"] == "function")
        .map(|t| {
            let f = &t["function"];
            let mut tool = json!({
                "name": f["name"].as_str().unwrap_or_default(),
                "input_schema": if f["parameters"].is_object() {
                    f["parameters"].clone()
                } else {
                    json!({"type": "object", "properties": {}})
                },
            });
            if let Some(d) = f["description"].as_str() {
                tool["description"] = json!(d);
            }
            tool
        })
        .collect();
    if !tools.is_empty() {
        let mut choice = json!({"type": "auto"});
        match &req["tool_choice"] {
            Value::String(s) if s == "none" => choice = json!({"type": "none"}),
            Value::String(s) if s == "required" => {
                system.push("Call one of your tools in this reply.".into());
            }
            Value::Object(o) if o.get("type").and_then(Value::as_str) == Some("function") => {
                if let Some(name) = o
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(Value::as_str)
                {
                    system.push(format!("Call the `{name}` tool in this reply."));
                }
            }
            _ => {}
        }
        if req["parallel_tool_calls"] == json!(false) && choice["type"] == "auto" {
            choice["disable_parallel_tool_use"] = json!(true);
        }
        out.insert("tools".into(), Value::Array(tools));
        out.insert("tool_choice".into(), choice);
    }

    if let Some(kind) = req["response_format"]["type"].as_str() {
        if kind == "json_object" || kind == "json_schema" {
            let mut rule =
                String::from("Reply with only one JSON object: no prose and no code fences.");
            if let Some(schema) = req["response_format"]["json_schema"].get("schema") {
                rule.push_str(&format!(" It must match this JSON Schema: {schema}"));
            }
            system.push(rule);
        }
    }
    match &req["stop"] {
        Value::String(s) => {
            out.insert("stop_sequences".into(), json!([s]));
        }
        Value::Array(a) if !a.is_empty() => {
            out.insert("stop_sequences".into(), Value::Array(a.clone()));
        }
        _ => {}
    }
    if !system.is_empty() {
        out.insert("system".into(), json!(system.join("\n\n")));
    }
    if let Some(e) = effort {
        out.insert("output_config".into(), json!({"effort": e}));
    }
    out.insert("fallbacks".into(), json!("default"));
    out.insert("messages".into(), Value::Array(messages));
    Ok(Value::Object(out))
}

/// The OpenAI `chat.completion` for a Messages API answer. `model` is reported as the role's
/// model id.
pub fn from_messages(res: &Value, model: &str) -> Value {
    let mut text = String::new();
    let mut calls = Vec::new();
    for block in res["content"].as_array().into_iter().flatten() {
        match block["type"].as_str() {
            Some("text") => text.push_str(block["text"].as_str().unwrap_or_default()),
            Some("tool_use") => calls.push(json!({
                "id": block["id"],
                "type": "function",
                "function": {
                    "name": block["name"],
                    "arguments": block["input"].to_string(),
                },
            })),
            // Thinking, fallback markers and anything newer have no OpenAI equivalent.
            _ => {}
        }
    }
    let finish = match res["stop_reason"].as_str() {
        Some("tool_use") => "tool_calls",
        Some("max_tokens") => "length",
        Some("refusal") => "content_filter",
        _ => "stop",
    };
    if finish == "content_filter" && text.is_empty() {
        text = "I can't help with that request.".into();
    }
    let mut message = json!({"role": "assistant", "content": if text.is_empty() && !calls.is_empty() { Value::Null } else { json!(text) }});
    if !calls.is_empty() {
        message["tool_calls"] = Value::Array(calls);
    }
    let usage = &res["usage"];
    let prompt = usage["input_tokens"].as_u64().unwrap_or(0)
        + usage["cache_read_input_tokens"].as_u64().unwrap_or(0)
        + usage["cache_creation_input_tokens"].as_u64().unwrap_or(0);
    let completion = usage["output_tokens"].as_u64().unwrap_or(0);
    json!({
        "id": format!("chatcmpl-{}", res["id"].as_str().unwrap_or("claude")),
        "object": "chat.completion",
        "created": chrono::Utc::now().timestamp(),
        "model": model,
        "choices": [{"index": 0, "message": message, "finish_reason": finish}],
        "usage": {
            "prompt_tokens": prompt,
            "completion_tokens": completion,
            "total_tokens": prompt + completion,
        },
    })
}

/// An OpenAI error body for a Messages API error answer.
pub fn error_body(res: &Value, status: u16) -> Value {
    let message = res["error"]["message"]
        .as_str()
        .map(String::from)
        .unwrap_or_else(|| format!("the Claude test upstream answered {status}"));
    json!({"error": {
        "message": message,
        "type": res["error"]["type"].as_str().unwrap_or("upstream_error"),
    }})
}

/// A finished `chat.completion` as an OpenAI server-sent-event stream, for callers that asked
/// for `stream: true`: the whole answer in one delta, then the finish reason and `[DONE]`.
/// Also used for a streamed call to a role whose workarounds need the whole answer (`app.rs`).
pub fn as_stream(answer: &Value) -> String {
    let base = |delta: Value, finish: Value| {
        json!({
            "id": answer["id"],
            "object": "chat.completion.chunk",
            "created": answer["created"],
            "model": answer["model"],
            "choices": [{"index": 0, "delta": delta, "finish_reason": finish}],
        })
    };
    let msg = &answer["choices"][0]["message"];
    let mut delta = json!({"role": "assistant", "content": msg["content"]});
    // A workaround's output on a whole answer (`strip-think`'s reasoning) goes with it.
    if let Some(r) = msg["reasoning_content"].as_str() {
        delta["reasoning_content"] = json!(r);
    }
    if let Some(calls) = msg["tool_calls"].as_array() {
        let indexed: Vec<Value> = calls
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let mut c = c.clone();
                c["index"] = json!(i);
                c
            })
            .collect();
        delta["tool_calls"] = Value::Array(indexed);
    }
    let mut last = base(json!({}), answer["choices"][0]["finish_reason"].clone());
    last["usage"] = answer["usage"].clone();
    format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        base(delta, Value::Null),
        last
    )
}

/// Message text from an OpenAI `content`: a string, or a list of parts whose text is joined.
fn text_of(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// A user message's blocks: text, plus images given as URLs or data URLs.
fn user_blocks(content: &Value) -> Vec<Value> {
    let Value::Array(parts) = content else {
        return vec![json!({"type": "text", "text": text_of(content)})];
    };
    let mut blocks = Vec::new();
    for p in parts {
        match p["type"].as_str() {
            Some("text") => blocks.push(json!({"type": "text", "text": p["text"]})),
            Some("image_url") => {
                let url = p["image_url"]["url"].as_str().unwrap_or_default();
                if let Some(rest) = url.strip_prefix("data:") {
                    if let Some((media, data)) = rest.split_once(";base64,") {
                        blocks.push(json!({"type": "image", "source": {
                            "type": "base64", "media_type": media, "data": data}}));
                    }
                } else if url.starts_with("https://") {
                    blocks.push(json!({"type": "image", "source": {"type": "url", "url": url}}));
                }
            }
            _ => {}
        }
    }
    if blocks.is_empty() {
        blocks.push(json!({"type": "text", "text": ""}));
    }
    blocks
}

/// Append blocks as a message, merging into the previous one when it has the same role, so
/// parallel tool results go back in one user message.
fn push(messages: &mut Vec<Value>, role: &str, blocks: Vec<Value>) {
    if let Some(last) = messages.last_mut() {
        if last["role"] == role {
            if let Some(content) = last["content"].as_array_mut() {
                content.extend(blocks);
                return;
            }
        }
    }
    messages.push(json!({"role": role, "content": blocks}));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tool_loop_translates_both_ways() {
        let req = json!({
            "model": "pmos/default",
            "temperature": 0,
            "messages": [
                {"role": "system", "content": "You are the Docs bot."},
                {"role": "user", "content": "What ports does Acme Vault need?"},
                {"role": "assistant", "content": "", "tool_calls": [
                    {"id": "c1", "type": "function", "function": {"name": "docs_search", "arguments": "{\"query\":\"ports\"}"}},
                    {"id": "c2", "type": "function", "function": {"name": "docs_read", "arguments": "not json"}}
                ]},
                {"role": "tool", "tool_call_id": "c1", "content": "443 and 8443"},
                {"role": "tool", "tool_call_id": "c2", "content": [{"type": "text", "text": "page"}]}
            ],
            "tools": [{"type": "function", "function": {"name": "docs_search", "description": "Search", "parameters": {"type": "object", "properties": {"query": {"type": "string"}}}}}],
            "tool_choice": "required",
            "parallel_tool_calls": false
        });
        let out = to_messages(&req, "claude-opus-5-5", Some("medium")).unwrap();
        assert_eq!(out["model"], "claude-opus-5-5");
        assert!(
            out.get("temperature").is_none(),
            "sampling parameters are dropped"
        );
        assert_eq!(out["max_tokens"], 16_000);
        assert_eq!(out["output_config"]["effort"], "medium");
        assert_eq!(out["fallbacks"], "default");
        assert!(out["system"]
            .as_str()
            .unwrap()
            .starts_with("You are the Docs bot."));
        assert!(out["system"]
            .as_str()
            .unwrap()
            .contains("Call one of your tools"));
        assert_eq!(
            out["tool_choice"],
            json!({"type": "auto", "disable_parallel_tool_use": true})
        );
        assert_eq!(
            out["tools"][0]["input_schema"]["properties"]["query"]["type"],
            "string"
        );
        let msgs = out["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 3, "{msgs:?}");
        assert_eq!(msgs[1]["content"][0]["input"]["query"], "ports");
        assert_eq!(
            msgs[1]["content"][1]["input"],
            json!({}),
            "bad arguments become {{}}"
        );
        // Both tool results in one user message.
        assert_eq!(msgs[2]["role"], "user");
        assert_eq!(msgs[2]["content"][0]["tool_use_id"], "c1");
        assert_eq!(msgs[2]["content"][1]["content"], "page");

        let answer = from_messages(
            &json!({
                "id": "msg_1",
                "content": [
                    {"type": "thinking", "thinking": ""},
                    {"type": "text", "text": "Let me check."},
                    {"type": "tool_use", "id": "t1", "name": "docs_search", "input": {"query": "x"}}
                ],
                "stop_reason": "tool_use",
                "usage": {"input_tokens": 10, "cache_read_input_tokens": 5, "output_tokens": 7}
            }),
            "claude-opus-5-5",
        );
        let choice = &answer["choices"][0];
        assert_eq!(choice["finish_reason"], "tool_calls");
        assert_eq!(choice["message"]["content"], "Let me check.");
        assert_eq!(
            choice["message"]["tool_calls"][0]["function"]["arguments"],
            "{\"query\":\"x\"}"
        );
        assert_eq!(answer["usage"]["prompt_tokens"], 15);
        assert_eq!(answer["usage"]["completion_tokens"], 7);
    }

    #[test]
    fn json_mode_refusals_and_streams() {
        let req = json!({
            "messages": [{"role": "user", "content": "parse"}],
            "response_format": {"type": "json_object"},
            "max_tokens": 999_999,
            "stop": "END"
        });
        let out = to_messages(&req, "m", None).unwrap();
        assert!(out["system"]
            .as_str()
            .unwrap()
            .contains("only one JSON object"));
        assert_eq!(out["max_tokens"], MAX_TOKENS_CAP);
        assert_eq!(out["stop_sequences"], json!(["END"]));
        assert!(out.get("output_config").is_none());
        assert!(out.get("tools").is_none());

        let refused = from_messages(&json!({"content": [], "stop_reason": "refusal"}), "m");
        assert_eq!(refused["choices"][0]["finish_reason"], "content_filter");
        assert!(refused["choices"][0]["message"]["content"]
            .as_str()
            .unwrap()
            .contains("can't help"));

        let sse = as_stream(&from_messages(
            &json!({"content": [{"type": "text", "text": "hi"}], "stop_reason": "end_turn"}),
            "m",
        ));
        assert!(
            sse.contains("\"content\":\"hi\"") && sse.ends_with("data: [DONE]\n\n"),
            "{sse}"
        );
    }

    #[test]
    fn a_leading_assistant_message_gets_a_user_turn_first() {
        let out = to_messages(
            &json!({"messages": [{"role": "assistant", "content": "Hello"}]}),
            "m",
            None,
        )
        .unwrap();
        assert_eq!(out["messages"][0]["role"], "user");
        assert_eq!(out["messages"][1]["role"], "assistant");
        assert!(to_messages(&json!({"messages": [{"role": "bot"}]}), "m", None).is_err());
    }

    #[test]
    fn errors_keep_the_message() {
        let e = error_body(
            &json!({"type": "error", "error": {"type": "invalid_request_error", "message": "bad"}}),
            400,
        );
        assert_eq!(e["error"]["message"], "bad");
        assert_eq!(e["error"]["type"], "invalid_request_error");
    }
}
