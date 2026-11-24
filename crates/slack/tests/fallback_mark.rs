//! Slice MP7 (`docs/model-providers-spec.md` §4.10, MP-18): a task a fallback model answered
//! (in part) is marked "Answered by a fallback model: not benchmarked for this bot" on its
//! reply and on each of its draft cards, on every such message; other tasks are unchanged.
//! Every name, message and id here is made up.

mod common;
mod support;

use serde_json::{json, Value};
use support::{approval, call_text, Stack};

const DM: &str = "D0ANA";
const MARK: &str = "Answered by a fallback model: not benchmarked for this bot";

fn catalog() -> Value {
    json!({"personas": [
        {"key": "nudge", "display_name": "Nudge", "title": "Your PM assistant", "role": "pm-assistant",
         "mascot": "nudge", "always_on": true, "preview": false},
        {"key": "steady", "display_name": "Steady", "title": "Benchmarked helper", "role": "steady-pm",
         "mascot": "nudge", "always_on": false, "preview": false},
    ]})
}

#[tokio::test]
async fn a_fallback_answer_is_marked_on_its_reply_and_each_draft_card() {
    let stack = Stack::new().await;
    *stack.runtime.personas.lock().unwrap() = Some(catalog());
    let mut draft = approval(
        "apr_f1",
        "ana",
        "jira.create_issue",
        json!({"project": "PAY", "summary": "From a fallback"}),
        "2026-11-24T09:00:00Z",
    );
    draft["persona"] = json!("steady");
    stack.tools.items.lock().unwrap().push(draft);
    let (stop, notifier) = stack.notifier();
    let note = |id: &str, task: &str, thread: &str, kind: &str, text: &str, fallback: bool| {
        let mut n = json!({
            "id": id, "task_id": task, "bot": "pm-ana", "requested_by": "ana", "persona": "steady",
            "slack": {"channel": DM, "thread_ts": thread}, "kind": kind, "text": text,
        });
        if fallback {
            n["fallback_used"] = json!(true);
        }
        n
    };
    stack.runtime.feed.lock().unwrap().extend([
        note(
            "n1",
            "tsk_f1",
            "1.1",
            "done",
            "Answered by the fallback.",
            true,
        ),
        note(
            "n2",
            "tsk_p1",
            "1.2",
            "done",
            "Answered by the primary.",
            false,
        ),
        note(
            "n3",
            "tsk_f2",
            "1.3",
            "done",
            "A second fallback answer.",
            true,
        ),
    ]);
    let posts = stack.calls("chat.postMessage", 3).await;
    let in_thread = |ts: &str| {
        posts
            .iter()
            .find(|c| c.body["thread_ts"] == ts)
            .map(call_text)
            .unwrap()
    };
    assert!(in_thread("1.1").contains(MARK), "{}", in_thread("1.1"));
    assert!(!in_thread("1.2").contains(MARK), "{}", in_thread("1.2"));
    // On every such reply, not only the first.
    assert!(in_thread("1.3").contains(MARK));

    let mut approval_note = note("n4", "tsk_f3", "2.1", "needs_approval", "Drafted it.", true);
    approval_note["approvals"] = json!(["apr_f1"]);
    stack.runtime.feed.lock().unwrap().push(approval_note);
    let posts = stack.calls("chat.postMessage", 5).await;
    let card = posts
        .iter()
        .map(call_text)
        .find(|t| t.contains("apr_f1"))
        .expect("the draft's card");
    assert!(card.contains(MARK), "{card}");
    assert!(card.contains("Drafted by Steady"), "{card}");
    let reply = posts
        .iter()
        .map(call_text)
        .find(|t| t.contains("Drafted it."))
        .unwrap();
    assert!(reply.contains(MARK), "{reply}");
    stop.cancel();
    let _ = notifier.await;
}
