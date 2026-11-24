//! Workarounds for model weaknesses (D11). Each one is listed here with what it does and the
//! test that says it can be deleted. A role switches one on in `config/gateway.yaml`, and
//! `PMOS_GATEWAY_WORKAROUNDS=off` switches them all off for the benchmark run after a model
//! swap: if the benchmark passes without one, it is deleted.

use serde::Serialize;
use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Workaround {
    pub id: &'static str,
    pub what: &'static str,
    pub delete_when: &'static str,
}

pub const STRIP_THINK: Workaround = Workaround {
    id: "strip-think",
    what: "Moves a <think>…</think> block out of the answer into `reasoning_content`, so \
           reasoning never reaches a PM's draft. Works on whole answers: a streamed call to a \
           role with it on is asked of the model server whole and replayed as a stream.",
    delete_when: "vLLM serves the model with a reasoning parser (`--reasoning-parser`), so \
                  answers arrive without <think> blocks, and the benchmark passes with \
                  PMOS_GATEWAY_WORKAROUNDS=off.",
};

pub const REGISTRY: &[Workaround] = &[STRIP_THINK];

pub fn known(id: &str) -> bool {
    REGISTRY.iter().any(|w| w.id == id)
}

/// Whether a role's workarounds need the whole answer before they can apply (every one in the
/// registry does). A streamed call to such a role is then asked of the model server as a whole
/// answer and replayed to the caller as an event stream, so the workaround still applies.
pub fn needs_whole_answer(enabled: &[String], all_on: bool) -> bool {
    all_on && enabled.iter().any(|w| known(w))
}

/// Apply a role's workarounds to a chat completion. Returns the ids that changed something.
pub fn apply(enabled: &[String], all_on: bool, response: &mut Value) -> Vec<&'static str> {
    let mut applied = Vec::new();
    if !all_on {
        return applied;
    }
    if enabled.iter().any(|w| w == STRIP_THINK.id) && strip_think(response) {
        applied.push(STRIP_THINK.id);
    }
    applied
}

fn strip_think(response: &mut Value) -> bool {
    let mut changed = false;
    let Some(choices) = response["choices"].as_array_mut() else {
        return false;
    };
    for choice in choices {
        let message = &mut choice["message"];
        let Some(content) = message["content"].as_str() else {
            continue;
        };
        // Some templates open the block in the prompt, so only the closing tag appears.
        let Some(end) = content.rfind("</think>") else {
            continue;
        };
        let reasoning = content[..end]
            .trim()
            .trim_start_matches("<think>")
            .trim()
            .to_string();
        let answer = content[end + "</think>".len()..].trim_start().to_string();
        message["content"] = json!(answer);
        if message["reasoning_content"].is_null() {
            message["reasoning_content"] = json!(reasoning);
        }
        changed = true;
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(content: &str) -> Value {
        json!({"choices": [{"index": 0, "message": {"role": "assistant", "content": content}}]})
    }

    #[test]
    fn strips_reasoning_from_the_answer() {
        let mut r =
            reply("<think>\nThe user wants SAML.\n</think>\n\nAcme Vault supports SAML 2.0.");
        let on = vec!["strip-think".to_string()];
        assert_eq!(apply(&on, true, &mut r), vec!["strip-think"]);
        assert_eq!(
            r["choices"][0]["message"]["content"],
            "Acme Vault supports SAML 2.0."
        );
        assert_eq!(
            r["choices"][0]["message"]["reasoning_content"],
            "The user wants SAML."
        );

        // Only the closing tag.
        let mut r = reply("thinking…</think>Answer.");
        apply(&on, true, &mut r);
        assert_eq!(r["choices"][0]["message"]["content"], "Answer.");

        // Nothing to strip, switched off for the role, or all off: unchanged.
        let mut r = reply("Plain answer.");
        assert!(apply(&on, true, &mut r).is_empty());
        let mut r = reply("<think>x</think>y");
        assert!(apply(&[], true, &mut r).is_empty());
        assert!(apply(&on, false, &mut r).is_empty());
        assert_eq!(r["choices"][0]["message"]["content"], "<think>x</think>y");
    }

    #[test]
    fn a_role_with_a_workaround_on_needs_whole_answers() {
        let on = vec!["strip-think".to_string()];
        assert!(needs_whole_answer(&on, true));
        assert!(!needs_whole_answer(&on, false));
        assert!(!needs_whole_answer(&[], true));
    }

    #[test]
    fn every_workaround_says_when_to_delete_it() {
        for w in REGISTRY {
            assert!(w.delete_when.contains("benchmark"), "{}", w.id);
            assert!(known(w.id));
        }
        assert!(!known("magic"));
    }
}
