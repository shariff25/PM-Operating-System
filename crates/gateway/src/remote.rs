//! **Testing only:** a remote OpenAI-compatible API behind the gateway, written
//! `openai+https://<host>/<base path>` and accepted only with `PMOS_GATEWAY_CLOUD_TEST=1`.
//! Prompts sent to it leave the machine.
//!
//! Requests go out in the OpenAI format as the caller sent them, with the role's model and
//! `PMOS_GATEWAY_REMOTE_KEY` as the bearer token. Every field the harness sends was accepted by
//! the first such API (Gemini's OpenAI endpoint), so nothing is removed. One thing is added
//! back, and only for these upstreams:
//!
//! **Tool-call `extra_content`.** Such an API may attach `extra_content` to each tool call it
//! returns (Gemini puts its `thought_signature` there) and refuse a later request whose history
//! holds that tool call without it ("Function call is missing a thought_signature", 400). The
//! harness's client (LangChain `ChatOpenAI`) drops the field when it sends the history back, so
//! the gateway remembers the `extra_content` of every tool call in an answer, by tool-call id,
//! and puts it back on the same tool call in later requests. Callers' requests are otherwise
//! unchanged; local (vLLM) upstreams never go through this.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use serde_json::Value;

/// How many tool calls' `extra_content` are kept. A worker's task needs only its own few; the
/// oldest are forgotten first.
const KEEP: usize = 10_000;

/// The `extra_content` a remote upstream attached to its tool calls, by upstream and id.
#[derive(Default)]
pub struct ToolCallExtras {
    inner: Mutex<Extras>,
}

#[derive(Default)]
struct Extras {
    by_id: HashMap<(String, String), Value>,
    order: VecDeque<(String, String)>,
}

impl ToolCallExtras {
    fn remember(&self, upstream: &str, id: &str, extra: &Value) {
        if id.is_empty() || !extra.is_object() {
            return;
        }
        let key = (upstream.to_string(), id.to_string());
        let mut x = self.inner.lock().expect("extras lock");
        if x.by_id.insert(key.clone(), extra.clone()).is_none() {
            x.order.push_back(key);
        }
        while x.order.len() > KEEP {
            if let Some(old) = x.order.pop_front() {
                x.by_id.remove(&old);
            }
        }
    }

    /// Remember the tool calls in a whole `chat.completion`.
    pub fn remember_answer(&self, upstream: &str, answer: &Value) {
        for choice in answer["choices"].as_array().into_iter().flatten() {
            for call in choice["message"]["tool_calls"]
                .as_array()
                .into_iter()
                .flatten()
            {
                self.remember(
                    upstream,
                    call["id"].as_str().unwrap_or_default(),
                    &call["extra_content"],
                );
            }
        }
    }

    /// Put remembered `extra_content` back on assistant tool calls that lack it. Returns how
    /// many were restored.
    pub fn restore(&self, upstream: &str, req: &mut Value) -> usize {
        let x = self.inner.lock().expect("extras lock");
        if x.by_id.is_empty() {
            return 0;
        }
        let mut restored = 0;
        for message in req["messages"].as_array_mut().into_iter().flatten() {
            if message["role"] != "assistant" {
                continue;
            }
            for call in message["tool_calls"].as_array_mut().into_iter().flatten() {
                if !call["extra_content"].is_null() {
                    continue;
                }
                let Some(id) = call["id"].as_str() else {
                    continue;
                };
                if let Some(extra) = x.by_id.get(&(upstream.to_string(), id.to_string())) {
                    call["extra_content"] = extra.clone();
                    restored += 1;
                }
            }
        }
        restored
    }
}

/// Reads a streamed answer's server-sent events as they pass through, and remembers the
/// `extra_content` of each tool call in it. The bytes themselves are passed on untouched.
pub struct StreamScanner {
    extras: Arc<ToolCallExtras>,
    upstream: String,
    pending: Vec<u8>,
    /// Tool-call ids by (choice, tool-call index): later deltas of a call carry only its index.
    ids: HashMap<(u64, u64), String>,
}

impl StreamScanner {
    pub fn new(extras: Arc<ToolCallExtras>, upstream: &str) -> Self {
        Self {
            extras,
            upstream: upstream.to_string(),
            pending: Vec::new(),
            ids: HashMap::new(),
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.pending.extend_from_slice(bytes);
        while let Some(end) = self.pending.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=end).collect();
            let Some(data) = line.strip_prefix(b"data:") else {
                continue;
            };
            let Ok(chunk) = serde_json::from_slice::<Value>(data.trim_ascii()) else {
                continue;
            };
            self.chunk(&chunk);
        }
    }

    fn chunk(&mut self, chunk: &Value) {
        for (n, choice) in chunk["choices"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
        {
            let c = choice["index"].as_u64().unwrap_or(n as u64);
            for (m, call) in choice["delta"]["tool_calls"]
                .as_array()
                .into_iter()
                .flatten()
                .enumerate()
            {
                let i = call["index"].as_u64().unwrap_or(m as u64);
                if let Some(id) = call["id"].as_str().filter(|id| !id.is_empty()) {
                    self.ids.insert((c, i), id.to_string());
                }
                if let Some(id) = self.ids.get(&(c, i)) {
                    self.extras
                        .remember(&self.upstream, id, &call["extra_content"]);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sig(s: &str) -> Value {
        json!({"google": {"thought_signature": s}})
    }

    fn history() -> Value {
        json!({"messages": [
            {"role": "user", "content": "weather?"},
            {"role": "assistant", "content": "", "tool_calls": [
                {"id": "call_1", "type": "function", "function": {"name": "w", "arguments": "{}"}},
                {"id": "call_2", "type": "function", "function": {"name": "w", "arguments": "{}"}}
            ]},
            {"role": "tool", "tool_call_id": "call_1", "content": "sunny"}
        ]})
    }

    #[test]
    fn extra_content_from_an_answer_goes_back_on_the_same_tool_call() {
        let x = ToolCallExtras::default();
        let mut req = history();
        assert_eq!(x.restore("up", &mut req), 0);
        x.remember_answer(
            "up",
            &json!({"choices": [{"message": {"tool_calls": [
                {"id": "call_1", "extra_content": sig("abc")},
                {"id": "call_2"}
            ]}}]}),
        );
        assert_eq!(x.restore("other-upstream", &mut req), 0);
        assert_eq!(x.restore("up", &mut req), 1);
        let calls = &req["messages"][1]["tool_calls"];
        assert_eq!(calls[0]["extra_content"], sig("abc"));
        assert!(calls[1].get("extra_content").is_none());
        // One the caller kept is left as it is.
        let mut kept = history();
        kept["messages"][1]["tool_calls"][0]["extra_content"] = sig("mine");
        assert_eq!(x.restore("up", &mut kept), 0);
        assert_eq!(
            kept["messages"][1]["tool_calls"][0]["extra_content"],
            sig("mine")
        );
    }

    #[test]
    fn a_stream_is_read_across_chunk_boundaries() {
        let x = Arc::new(ToolCallExtras::default());
        let events = format!(
            "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
            json!({"choices": [{"index": 0, "delta": {"tool_calls": [
                {"index": 0, "id": "call_1", "function": {"name": "w"}}]}}]}),
            json!({"choices": [{"index": 0, "delta": {"tool_calls": [
                {"index": 0, "extra_content": sig("streamed"), "function": {"arguments": "{}"}}]}}]}),
        );
        let mut s = StreamScanner::new(x.clone(), "up");
        for piece in events.as_bytes().chunks(7) {
            s.feed(piece);
        }
        let mut req = history();
        assert_eq!(x.restore("up", &mut req), 1);
        assert_eq!(
            req["messages"][1]["tool_calls"][0]["extra_content"],
            sig("streamed")
        );
    }

    #[test]
    fn the_oldest_are_forgotten_first() {
        let x = ToolCallExtras::default();
        for i in 0..=KEEP {
            x.remember("up", &format!("call_{i}"), &sig("s"));
        }
        let ask =
            |id: String| json!({"messages": [{"role": "assistant", "tool_calls": [{"id": id}]}]});
        assert_eq!(x.restore("up", &mut ask("call_0".into())), 0);
        assert_eq!(x.restore("up", &mut ask(format!("call_{KEEP}"))), 1);
    }
}
