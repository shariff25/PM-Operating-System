//! Marketplace bots in Slack, end to end (`docs/bot-marketplace-spec.md` §9 and §13, S1): the
//! fake Slack, a mock bots runtime with a persona catalog, and a mock tool server (`support`).
//! Every name, message and id here is made up.
//!
//! - `Chief: x` and `chief, x` ask Chief on the PM's own bot; `Chiefly speaking` doesn't; a
//!   withdrawn persona's name doesn't match; `Nudge:` names Nudge.
//! - A reply in a thread names no persona (the runtime inherits the thread's); another bot's
//!   prefix names that bot.
//! - A bot not on the team: the task is sent, and the claim's sentence comes back in the
//!   thread, said by that bot.
//! - Messages start with the bot's name; a Preview bot's first message in a thread says
//!   "Preview: not yet benchmarked", and only that one; Nudge's messages are unchanged.
//! - The approval card says "Drafted by Chief"; a held draft offers only **Reject**.
//! - What the Slack bot can tell before sending: no personal bot, nothing asked, a routine, a
//!   refusal from the runtime; a tag names a bot the same way.
//! - Hostile catalog text is shown inert.

mod common;
mod support;

use std::time::Duration;

use common::until;
use serde_json::{json, Value};
use support::{approval, call_text, dm, mention, Stack, APP_USER};

const DM: &str = "D0ANA";

fn catalog() -> Value {
    json!({"personas": [
        {"key": "nudge", "display_name": "Nudge", "title": "Your PM assistant", "role": "pm-assistant",
         "mascot": "nudge", "always_on": true, "preview": false},
        {"key": "chief", "display_name": "Chief", "title": "Chief of Staff", "role": "chief-of-staff",
         "mascot": "chief", "always_on": false, "preview": true},
        {"key": "sprout", "display_name": "Sprout", "title": "Growth PM", "role": "growth-pm",
         "mascot": "sprout", "always_on": false, "preview": true},
        {"key": "steady", "display_name": "Steady", "title": "Benchmarked helper", "role": "steady-pm",
         "mascot": "nudge", "always_on": false, "preview": false},
    ]})
}

async fn stack() -> Stack {
    let stack = Stack::new().await;
    *stack.runtime.personas.lock().unwrap() = Some(catalog());
    stack
}

/// A reply in a DM thread.
fn reply(envelope_id: &str, user: &str, ts: &str, thread_ts: &str, text: &str) -> Value {
    let mut env = dm(envelope_id, user, ts, text);
    env["payload"]["event"]["thread_ts"] = json!(thread_ts);
    env
}

async fn tasks(stack: &Stack, n: usize) -> Vec<Value> {
    until(&format!("{n} tasks"), || {
        let tasks = stack.runtime.tasks.lock().unwrap().clone();
        (tasks.len() >= n).then(|| tasks.into_iter().map(|(_, t)| t).collect::<Vec<_>>())
    })
    .await
}

#[tokio::test]
async fn a_name_prefix_asks_that_bot_on_the_pms_own_bot() {
    let stack = stack().await;
    stack
        .send(dm(
            "e1",
            "U0ANA",
            "1.000001",
            "Chief: what slipped since Monday?",
        ))
        .await;
    stack
        .send(dm(
            "e2",
            "U0ANA",
            "1.000002",
            "chief, prep me for the staff meeting",
        ))
        .await;
    stack
        .send(dm("e3", "U0ANA", "1.000003", "SPROUT:size the annual plan"))
        .await;
    stack
        .send(dm(
            "e4",
            "U0ANA",
            "1.000004",
            "Chiefly speaking, what slipped?",
        ))
        .await;
    // Echo is not offered in this catalog (withdrawn): its name is ordinary text.
    stack
        .send(dm("e5", "U0ANA", "1.000005", "Echo: draft the launch note"))
        .await;
    stack
        .send(dm("e6", "U0ANA", "1.000006", "Nudge: what is on today?"))
        .await;
    // A team bot's own prefix still comes first.
    stack
        .send(dm(
            "e7",
            "U0ANA",
            "1.000007",
            "docs: what ports does Acme Vault need?",
        ))
        .await;
    let all = tasks(&stack, 7).await;
    let by_ts = |ts: &str| {
        all.iter()
            .find(|t| t["slack"]["thread_ts"] == ts)
            .cloned()
            .unwrap_or_else(|| panic!("no task for {ts}"))
    };
    let t = by_ts("1.000001");
    assert_eq!(
        (t["bot"].as_str(), t["persona"].as_str(), t["text"].as_str()),
        (
            Some("pm-ana"),
            Some("chief"),
            Some("what slipped since Monday?")
        )
    );
    assert_eq!(t["requested_by"], "ana");
    let t = by_ts("1.000002");
    assert_eq!(t["persona"], "chief");
    assert_eq!(t["text"], "prep me for the staff meeting");
    let t = by_ts("1.000003");
    assert_eq!(t["persona"], "sprout");
    assert_eq!(t["text"], "size the annual plan");
    for (ts, text) in [
        ("1.000004", "Chiefly speaking, what slipped?"),
        ("1.000005", "Echo: draft the launch note"),
    ] {
        let t = by_ts(ts);
        assert!(t.get("persona").is_none(), "{ts}: {t}");
        assert_eq!(t["bot"], "pm-ana");
        assert_eq!(t["text"], text);
    }
    let t = by_ts("1.000006");
    assert_eq!(t["persona"], "nudge");
    assert_eq!(t["text"], "what is on today?");
    let t = by_ts("1.000007");
    assert_eq!(t["bot"], "docs");
    assert!(t.get("persona").is_none());
    assert!(*stack.runtime.persona_reads.lock().unwrap() >= 1);
    // Nothing was posted: the answers come from the notification feed.
    assert!(stack.slack.calls("chat.postMessage").is_empty());
    stack.stop().await;
}

#[tokio::test]
async fn thread_replies_inherit_unless_they_name_a_bot() {
    let stack = stack().await;
    let root = "1.000100";
    stack
        .send(dm("e1", "U0ANA", root, "Chief: what slipped since Monday?"))
        .await;
    tasks(&stack, 1).await;
    stack
        .send(reply(
            "e2",
            "U0ANA",
            "1.000101",
            root,
            "and the week before?",
        ))
        .await;
    stack
        .send(reply(
            "e3",
            "U0ANA",
            "1.000102",
            root,
            "sprout, which of these hurt signups?",
        ))
        .await;
    stack
        .send(reply(
            "e4",
            "U0ANA",
            "1.000103",
            root,
            "nudge: thanks, back to you",
        ))
        .await;
    let all = tasks(&stack, 4).await;
    assert!(all
        .iter()
        .all(|t| t["slack"]["thread_ts"] == root && t["bot"] == "pm-ana"));
    assert_eq!(all[0]["persona"], "chief");
    assert!(
        all[1].get("persona").is_none(),
        "no persona: the runtime inherits the thread's ({})",
        all[1]
    );
    assert_eq!(all[1]["text"], "and the week before?");
    assert_eq!(all[2]["persona"], "sprout");
    assert_eq!(
        all[3]["persona"], "nudge",
        "`nudge` switches the thread back"
    );
    stack.stop().await;
}

#[tokio::test]
async fn a_bot_not_on_the_team_is_still_asked_and_its_refusal_is_posted() {
    let stack = stack().await;
    stack
        .send(dm(
            "e1",
            "U0ANA",
            "1.000200",
            "sprout: size the annual plan",
        ))
        .await;
    let sent = tasks(&stack, 1).await;
    assert_eq!(sent[0]["persona"], "sprout", "no silent fallback to Nudge");
    let (stop, notifier) = stack.notifier();
    stack.runtime.feed.lock().unwrap().push(json!({
        "id": "ntf_1", "task_id": "tsk_s1", "bot": "pm-ana", "requested_by": "ana",
        "persona": "sprout", "slack": {"channel": DM, "thread_ts": "1.000200"},
        "kind": "failed",
        "text": "Sprout isn't on your team any more. Add it on the Bots page in PM OS, or ask Nudge.",
    }));
    let posts = stack.calls("chat.postMessage", 1).await;
    let body = &posts[0].body;
    assert_eq!(body["thread_ts"], "1.000200");
    assert_eq!(body["username"], "PM Ana", "posted as the PM's own bot");
    assert!(
        body["text"]
            .as_str()
            .unwrap()
            .starts_with("Sprout: I couldn't finish this: Sprout isn't on your team any more."),
        "{body}"
    );
    let first = body["blocks"][0]["text"]["text"].as_str().unwrap();
    assert!(
        first.starts_with("*Sprout:* I couldn't finish this:"),
        "{first}"
    );
    assert!(call_text(&posts[0]).contains("Preview: not yet benchmarked"));
    stop.cancel();
    let _ = notifier.await;
    stack.stop().await;
}

#[tokio::test]
async fn messages_name_their_bot_and_a_preview_is_labelled_once_per_thread() {
    let stack = stack().await;
    let (stop, notifier) = stack.notifier();
    let note =
        |id: &str, task: &str, persona: Option<&str>, thread: &str, kind: &str, text: &str| {
            let mut n = json!({
                "id": id, "task_id": task, "bot": "pm-ana", "requested_by": "ana",
                "slack": {"channel": DM, "thread_ts": thread}, "kind": kind, "text": text,
            });
            if let Some(p) = persona {
                n["persona"] = json!(p);
            }
            n
        };
    // Chief (a Preview): a milestone, then the answer, in one thread.
    stack.runtime.feed.lock().unwrap().extend([
        note(
            "n1",
            "tsk_c1",
            Some("chief"),
            "1.1",
            "milestone",
            "Reading your Jira",
        ),
        note(
            "n2",
            "tsk_c1",
            Some("chief"),
            "1.1",
            "milestone",
            "Writing it up",
        ),
        note(
            "n3",
            "tsk_c1",
            Some("chief"),
            "1.1",
            "done",
            "Two things slipped.",
        ),
    ]);
    let posts = stack.calls("chat.postMessage", 2).await;
    let progress = call_text(&posts[0]);
    assert!(progress.contains("*Chief:*"), "{progress}");
    assert!(
        progress.contains("Chief of Staff · Preview: not yet benchmarked"),
        "{progress}"
    );
    let updates = stack.calls("chat.update", 2).await;
    for u in &updates {
        let t = format!("{} {}", u.body["text"], u.body["blocks"]);
        assert!(t.contains("*Chief:*"), "{t}");
        assert!(
            t.contains("Preview: not yet benchmarked"),
            "the progress keeps its line: {t}"
        );
    }
    let answer = &posts[1].body;
    assert!(answer["text"]
        .as_str()
        .unwrap()
        .starts_with("Chief: Two things slipped."));
    assert!(!call_text(&posts[1]).contains("Preview"), "once per thread");

    // A second Chief task in the same thread: no label again. Another thread: labelled.
    stack.runtime.feed.lock().unwrap().extend([
        note(
            "n4",
            "tsk_c2",
            Some("chief"),
            "1.1",
            "done",
            "Nothing else slipped.",
        ),
        note(
            "n5",
            "tsk_c3",
            Some("chief"),
            "1.2",
            "done",
            "Here is your prep.",
        ),
        // Steady is offered and not a Preview: named, never labelled.
        note("n6", "tsk_s1", Some("steady"), "1.3", "done", "Done."),
        // Nudge: as before, no name and no label.
        note("n7", "tsk_n1", None, "1.4", "done", "Plain Nudge answer."),
        note("n8", "tsk_n2", Some("nudge"), "1.5", "done", "Also Nudge."),
        // A key the catalog doesn't list: never guessed from the key.
        note("n9", "tsk_g1", Some("ghost"), "1.6", "done", "Old answer."),
    ]);
    let posts = stack.calls("chat.postMessage", 8).await;
    let by_thread = |ts: &str| {
        posts
            .iter()
            .filter(|c| c.body["thread_ts"] == ts)
            .map(call_text)
            .collect::<Vec<_>>()
    };
    let again = by_thread("1.1");
    assert_eq!(again.len(), 3);
    assert!(again[2].contains("Chief: Nothing else slipped.") && !again[2].contains("Preview"));
    let other = &by_thread("1.2")[0];
    assert!(
        other.contains("Chief: Here is your prep.")
            && other.contains("Preview: not yet benchmarked")
    );
    let steady = &by_thread("1.3")[0];
    assert!(
        steady.contains("Steady: Done.") && !steady.contains("Preview"),
        "{steady}"
    );
    for ts in ["1.4", "1.5"] {
        let nudge = &by_thread(ts)[0];
        assert!(!nudge.contains("Nudge:") && !nudge.contains("*"), "{nudge}");
    }
    let ghost = &by_thread("1.6")[0];
    assert!(
        ghost.contains("A bot no longer offered: Old answer."),
        "{ghost}"
    );
    assert!(
        !ghost.contains("ghost") && !ghost.contains("Ghost"),
        "{ghost}"
    );
    stop.cancel();
    let _ = notifier.await;
    stack.stop().await;
}

#[tokio::test]
async fn the_approval_card_says_which_bot_drafted_it_and_a_held_draft_can_only_be_rejected() {
    let stack = stack().await;
    let mut chiefs = approval(
        "apr_c1",
        "ana",
        "jira.create_issue",
        json!({"project": "PAY", "summary": "Fix the retry"}),
        "2026-11-21T09:00:00Z",
    );
    chiefs["persona"] = json!("chief");
    // A tool server's copy of the Slack bot's own field is never used.
    chiefs["_slack_drafted_by"] = json!("<!channel>");
    let mut nudges = approval(
        "apr_n1",
        "ana",
        "jira.create_issue",
        json!({"project": "PAY", "summary": "Nudge's"}),
        "2026-11-21T09:01:00Z",
    );
    nudges["persona"] = Value::Null;
    let mut held = approval(
        "apr_h1",
        "ana",
        "confluence.create_draft",
        json!({"space": "PM", "title": "Battlecard", "body": "Body"}),
        "2026-11-21T09:02:00Z",
    );
    held["persona"] = json!("echo");
    held["held"] = json!(true);
    stack
        .tools
        .items
        .lock()
        .unwrap()
        .extend([chiefs, nudges, held]);
    let (stop, notifier) = stack.notifier();
    stack.runtime.feed.lock().unwrap().push(json!({
        "id": "ntf_1", "task_id": "tsk_1", "bot": "pm-ana", "requested_by": "ana",
        "persona": "chief", "slack": {"channel": DM, "thread_ts": "2.1"},
        "kind": "needs_approval", "text": "", "approvals": ["apr_c1", "apr_n1", "apr_h1"],
    }));
    let posts = stack.calls("chat.postMessage", 3).await;
    let card = |id: &str| {
        posts
            .iter()
            .map(call_text)
            .find(|t| t.contains(id))
            .unwrap_or_else(|| panic!("no card for {id}"))
    };
    let chief = card("apr_c1");
    assert!(chief.contains("for Ana · Drafted by Chief"), "{chief}");
    assert!(!chief.contains("<!channel>"), "{chief}");
    let nudge = card("apr_n1");
    assert!(
        !nudge.contains("Drafted by"),
        "Nudge's cards are as before: {nudge}"
    );
    let held = card("apr_h1");
    assert!(
        held.contains("Drafted by A bot no longer offered"),
        "{held}"
    );
    assert!(held.contains("Held: the bot that drafted this"), "{held}");
    assert!(held.contains("\"Reject\""), "{held}");
    assert!(
        !held.contains("\"Approve\"") && !held.contains("\"Edit\""),
        "{held}"
    );
    stop.cancel();
    let _ = notifier.await;
    stack.stop().await;
}

#[tokio::test]
async fn what_slack_can_tell_before_sending_is_told_at_once() {
    let stack = stack().await;
    // Bob has no personal bot.
    stack
        .send(dm("e1", "U0BOB", "3.000001", "Chief: what slipped?"))
        .await;
    // Nothing asked.
    stack.send(dm("e2", "U0ANA", "3.000002", "chief:")).await;
    // A routine: Slack's routines are Nudge's for now.
    stack
        .send(dm(
            "e3",
            "U0ANA",
            "3.000003",
            "Chief: every weekday at 9am send my brief",
        ))
        .await;
    let posts = stack.calls("chat.postMessage", 3).await;
    let said = |ts: &str| {
        posts
            .iter()
            .find(|c| c.body["thread_ts"] == ts)
            .map(call_text)
            .unwrap_or_else(|| panic!("nothing in {ts}"))
    };
    assert!(
        said("3.000001").contains("Chief works through your own PM OS bot, which isn't set up yet")
    );
    assert!(said("3.000002").contains("Ask Chief something after “Chief:”."));
    assert!(said("3.000003").contains("Routines set up in Slack run as Nudge for now"));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        stack.runtime.tasks.lock().unwrap().is_empty(),
        "no task sent"
    );
    assert!(
        stack.runtime.parses.lock().unwrap().is_empty(),
        "no routine parsed"
    );

    // The runtime refuses the persona (withdrawn since the catalog was read).
    *stack.runtime.persona_refusal.lock().unwrap() = Some("unknown_persona".into());
    stack
        .send(dm("e4", "U0ANA", "3.000004", "Chief: what slipped?"))
        .await;
    let posts = stack.calls("chat.postMessage", 4).await;
    let refusal = call_text(&posts[3]);
    assert!(
        refusal.contains("Chief isn't offered in PM OS right now, so I didn't send that."),
        "{refusal}"
    );
    assert!(
        !refusal.contains("developer"),
        "the runtime's own sentence isn't shown"
    );
    *stack.runtime.persona_refusal.lock().unwrap() = Some("persona_needs_personal_bot".into());
    stack
        .send(dm("e5", "U0ANA", "3.000005", "Chief: what slipped?"))
        .await;
    let posts = stack.calls("chat.postMessage", 5).await;
    assert!(call_text(&posts[4]).contains("Chief works through your own PM OS bot"));
    stack.stop().await;
}

#[tokio::test]
async fn a_tag_names_a_bot_the_same_way() {
    let stack = stack().await;
    stack
        .send(mention(
            "e1",
            "Ev1",
            "U0ANA",
            "C0LAUNCH",
            "1731500000.000200",
            Some("1731500000.000100"),
            &format!("<@{APP_USER}> Chief: draft a status reply"),
        ))
        .await;
    stack.calls("chat.postEphemeral", 1).await;
    let task = tasks(&stack, 1).await.remove(0);
    assert_eq!(task["persona"], "chief");
    assert_eq!(task["bot"], "pm-ana");
    assert!(
        task["text"]
            .as_str()
            .unwrap()
            .ends_with("\ndraft a status reply"),
        "{task}"
    );
    let root = &stack.slack.calls("chat.postMessage")[0];
    assert!(
        call_text(root).contains("*Chief:* You tagged me in"),
        "{}",
        call_text(root)
    );
    // A plain tag is Nudge's, as before.
    stack
        .send(mention(
            "e2",
            "Ev2",
            "U0ANA",
            "C0LAUNCH",
            "1731500000.000300",
            Some("1731500000.000100"),
            &format!("<@{APP_USER}> draft a reply"),
        ))
        .await;
    let task = tasks(&stack, 2).await.remove(1);
    assert!(task.get("persona").is_none());
    stack.stop().await;
}

#[tokio::test]
async fn hostile_catalog_text_is_shown_inert() {
    let stack = Stack::new().await;
    *stack.runtime.personas.lock().unwrap() = Some(json!({"personas": [
        {"key": "chief",
         "display_name": "<!channel> *Boss* <@U0BOB> <https://evil.example|click> `x`",
         "title": "<!here> _admin_ evil.example/login", "preview": true},
    ]}));
    let (stop, notifier) = stack.notifier();
    stack.runtime.feed.lock().unwrap().push(json!({
        "id": "ntf_1", "task_id": "tsk_1", "bot": "pm-ana", "requested_by": "ana",
        "persona": "chief", "slack": {"channel": DM, "thread_ts": "4.1"},
        "kind": "done", "text": "Done.",
    }));
    let posts = stack.calls("chat.postMessage", 1).await;
    let all = call_text(&posts[0]);
    for bad in [
        "<!channel>",
        "<!here>",
        "<@U0BOB>",
        "<https://evil",
        "*Boss*",
        "`x`",
        "_admin_",
        "evil.example",
    ] {
        assert!(!all.contains(bad), "{bad} in {all}");
    }
    assert!(
        all.contains("Boss") && all.contains("Preview: not yet benchmarked"),
        "{all}"
    );
    stop.cancel();
    let _ = notifier.await;
    // Its display name isn't a prefix (only the key is).
    stack
        .send(dm("e1", "U0ANA", "4.000002", "<!channel> *Boss*: hi"))
        .await;
    let t = tasks(&stack, 1).await.remove(0);
    assert!(t.get("persona").is_none());
    stack.stop().await;
}

#[tokio::test]
async fn a_runtime_without_personas_works_as_before() {
    let stack = Stack::new().await; // `/v1/personas` answers 404
    stack
        .send(dm("e1", "U0ANA", "5.000001", "Chief: what slipped?"))
        .await;
    let t = tasks(&stack, 1).await.remove(0);
    assert!(t.get("persona").is_none());
    assert_eq!(t["bot"], "pm-ana");
    assert_eq!(t["text"], "Chief: what slipped?");
    stack.stop().await;
}
