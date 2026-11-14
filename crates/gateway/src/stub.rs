//! A stub model for CI and local development (`PMOS_GATEWAY_STUB=1`): deterministic answers
//! in the OpenAI shapes, so the harness and the bot runtime can be tested without a GPU.

use serde_json::{json, Value};

fn last_user_text(body: &Value) -> String {
    body["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .rev()
        .find(|m| m["role"] == "user")
        .and_then(|m| match &m["content"] {
            Value::String(s) => Some(s.clone()),
            Value::Array(parts) => Some(
                parts
                    .iter()
                    .filter_map(|p| p["text"].as_str())
                    .collect::<Vec<_>>()
                    .join(" "),
            ),
            _ => None,
        })
        .unwrap_or_default()
}

fn reply_text(body: &Value) -> String {
    let asked: String = last_user_text(body).chars().take(200).collect();
    format!("Stub reply to: {asked}")
}

pub fn chat(body: &Value, model: &str) -> Value {
    let text = reply_text(body);
    json!({
        "id": "chatcmpl-stub",
        "object": "chat.completion",
        "created": 0,
        "model": model,
        "choices": [{"index": 0, "finish_reason": "stop",
                     "message": {"role": "assistant", "content": text}}],
        "usage": {"prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0},
    })
}

/// The same answer as server-sent events: one chunk per word, then `[DONE]`.
pub fn chat_stream(body: &Value, model: &str) -> String {
    let text = reply_text(body);
    let mut out = String::new();
    let chunk = |delta: Value, finish: Value| {
        json!({"id": "chatcmpl-stub", "object": "chat.completion.chunk", "created": 0,
               "model": model, "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
    };
    out.push_str(&format!(
        "data: {}\n\n",
        chunk(json!({"role": "assistant", "content": ""}), Value::Null)
    ));
    for (i, word) in text.split(' ').enumerate() {
        let piece = if i == 0 {
            word.to_string()
        } else {
            format!(" {word}")
        };
        out.push_str(&format!(
            "data: {}\n\n",
            chunk(json!({"content": piece}), Value::Null)
        ));
    }
    out.push_str(&format!("data: {}\n\n", chunk(json!({}), json!("stop"))));
    out.push_str("data: [DONE]\n\n");
    out
}

pub fn embeddings(body: &Value, model: &str) -> Value {
    let inputs: Vec<String> = match &body["input"] {
        Value::String(s) => vec![s.clone()],
        Value::Array(items) => items
            .iter()
            .map(|i| i.as_str().unwrap_or_default().to_string())
            .collect(),
        _ => Vec::new(),
    };
    let data: Vec<Value> = inputs
        .iter()
        .enumerate()
        .map(|(i, text)| {
            // Eight numbers from the text's bytes: the same text always gets the same vector.
            let mut v = [0f64; 8];
            for (j, b) in text.bytes().enumerate() {
                v[j % 8] += f64::from(b) / 255.0;
            }
            let norm = v.iter().map(|x| x * x).sum::<f64>().sqrt().max(1e-9);
            json!({"object": "embedding", "index": i,
                   "embedding": v.iter().map(|x| x / norm).collect::<Vec<_>>()})
        })
        .collect();
    json!({"object": "list", "model": model, "data": data,
           "usage": {"prompt_tokens": 0, "total_tokens": 0}})
}
