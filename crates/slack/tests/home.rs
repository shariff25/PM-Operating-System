//! The App Home, end to end (`docs/slack-mentions-and-home-spec.md` §4 and §6): the fake
//! Slack, a mock bots runtime and a mock tool server (`support`).
//!
//! - Opening it publishes the Inbox for a member, and the note for a non-member.
//! - The section buttons switch sections, and the section survives a republish.
//! - Ticked drafts are decided one at a time, and the notice counts each outcome.
//! - Skills: grouped by track, only the role's Slack skills.
//! - Accounts: Connect for an unlinked account, Disconnect (to the tool server) for a linked
//!   one.
//! - A `needs_approval` notification republishes an open Home.

mod common;
mod support;

use std::time::Duration;

use chrono::Utc;
use serde_json::{json, Value};
use support::{approval, home_click, home_opened, tab_of, view_text, Stack};

fn hours_ago(h: i64) -> String {
    (Utc::now() - chrono::Duration::hours(h)).to_rfc3339()
}

fn reply(id: &str, user: &str, text: &str, h: i64) -> Value {
    approval(
        id,
        user,
        "slack.post_reply",
        json!({"channel": "C0LAUNCH", "thread_ts": "1731500000.000100", "text": text}),
        &hours_ago(h),
    )
}

fn jira(id: &str, user: &str, h: i64) -> Value {
    approval(
        id,
        user,
        "jira.create_issue",
        json!({"project": "PM", "issue_type": "Task", "summary": "Fix <login>"}),
        &hours_ago(h),
    )
}

/// The block ids of a view's tick boxes.
fn picks(published: &Value) -> Vec<String> {
    published["view"]["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|b| b["block_id"].as_str())
        .filter(|b| b.starts_with("pick_"))
        .map(String::from)
        .collect()
}

#[tokio::test]
async fn opening_it_shows_the_inbox_or_the_note() {
    let stack = Stack::new().await;
    stack.tools.items.lock().unwrap().extend([
        reply("apr_a1", "ana", "Yes, SSO ships in 5.1 <!here>", 3),
        jira("apr_a2", "ana", 1),
        jira("apr_b1", "bob", 1),
    ]);
    let ack = stack.send(home_opened("e1", "Ev1", "U0ANA", None)).await;
    assert!(ack.get("payload").is_none());
    let published = stack.published(1).await;
    assert_eq!(published["user_id"], "U0ANA");
    assert_eq!(published["view"]["type"], "home");
    assert_eq!(tab_of(&published), "inbox");
    let s = view_text(&published);
    assert!(s.contains("*2 drafts waiting for you*"), "{s}");
    assert!(s.contains("*Reply drafted for <#C0LAUNCH>*\\nby PM Ana · 3 hours ago\\n> Yes, SSO ships in 5.1 &lt;!here&gt;"), "{s}");
    assert!(s.contains("*Create a Jira issue in `PM`*"));
    assert!(s.contains("To edit or snooze a draft, use its card in our DM."));
    assert!(s.contains("Approve the selected drafts? Each one is created or posted as you."));
    for a in ["home_approve", "home_reject", "home_refresh", "home_pick"] {
        assert!(s.contains(a), "{a}");
    }
    assert_eq!(
        picks(&published),
        ["pick_apr_a1", "pick_apr_a2"],
        "oldest first, Ana's only"
    );
    assert_eq!(*stack.tools.lists.lock().unwrap(), ["ana"]);
    // The current section's button is the primary one.
    let tabs = &published["view"]["blocks"][1]["elements"];
    assert_eq!(tabs[0]["action_id"], "home_tab_inbox");
    assert_eq!(tabs[0]["style"], "primary");
    assert!(tabs[1].get("style").is_none());

    stack
        .send(home_opened("e2", "Ev2", "U0STRANGER", None))
        .await;
    let stranger = stack.published(2).await;
    assert_eq!(stranger["user_id"], "U0STRANGER");
    let s = view_text(&stranger);
    assert!(s.contains("PM OS is for the product team. DM me to ask for access."));
    assert!(!s.contains("home_tab_inbox"));
    assert_eq!(
        stack.tools.lists.lock().unwrap().len(),
        1,
        "nothing asked for a stranger"
    );

    // A tick is acked and does nothing.
    stack
        .send(home_click(
            "e3",
            "U0ANA",
            "home_pick",
            &published,
            &["apr_a1"],
        ))
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(stack.slack.calls("views.publish").len(), 2);
    assert!(stack.slack.calls("chat.update").is_empty());
    stack.stop().await;
}

#[tokio::test]
async fn sections_switch_and_survive_a_republish() {
    let stack = Stack::new().await;
    stack
        .runtime
        .role_skills
        .lock()
        .unwrap()
        .insert("pm-ana".into(), vec![]);
    stack.send(home_opened("e1", "Ev1", "U0ANA", None)).await;
    let inbox = stack.published(1).await;
    assert!(view_text(&inbox).contains("Nothing is waiting for you."));

    stack
        .send(home_click("e2", "U0ANA", "home_tab_skills", &inbox, &[]))
        .await;
    let skills = stack.published(2).await;
    assert_eq!(tab_of(&skills), "skills");
    assert_eq!(
        skills["view"]["blocks"][1]["elements"][1]["style"],
        "primary"
    );

    // Refresh keeps the section.
    stack
        .send(home_click("e3", "U0ANA", "home_refresh", &skills, &[]))
        .await;
    assert_eq!(tab_of(&stack.published(3).await), "skills");

    stack
        .send(home_click("e4", "U0ANA", "home_tab_accounts", &skills, &[]))
        .await;
    let accounts = stack.published(4).await;
    assert_eq!(tab_of(&accounts), "accounts");

    // Opening the Home again shows the section it was left on.
    stack
        .send(home_opened("e5", "Ev5", "U0ANA", Some("accounts")))
        .await;
    assert_eq!(tab_of(&stack.published(5).await), "accounts");
    stack.stop().await;
}

#[tokio::test]
async fn ticking_two_and_approving_decides_both() {
    let stack = Stack::new().await;
    stack.tools.items.lock().unwrap().extend([
        reply("apr_a1", "ana", "Yes", 3),
        jira("apr_a2", "ana", 2),
        jira("apr_a3", "ana", 1),
    ]);
    stack.send(home_opened("e1", "Ev1", "U0ANA", None)).await;
    let inbox = stack.published(1).await;
    stack
        .send(home_click(
            "e2",
            "U0ANA",
            "home_approve",
            &inbox,
            &["apr_a1", "apr_a2"],
        ))
        .await;
    let after = stack.published(2).await;
    let decisions = stack.tools.decisions.lock().unwrap().clone();
    assert_eq!(decisions.len(), 2);
    for ((id, body), want) in decisions.iter().zip(["apr_a1", "apr_a2"]) {
        assert_eq!(id, want);
        assert_eq!(body, &json!({"decided_by": "ana", "decision": "approve"}));
    }
    let s = view_text(&after);
    assert!(
        s.contains(":white_check_mark: 2 approved or posted."),
        "{s}"
    );
    assert_eq!(tab_of(&after), "inbox");
    assert_eq!(picks(&after), ["pick_apr_a3"], "what's left");
    assert!(s.contains("*1 draft waiting for you*"));

    // Reject selected.
    stack
        .send(home_click(
            "e3",
            "U0ANA",
            "home_reject",
            &after,
            &["apr_a3"],
        ))
        .await;
    let rejected = stack.published(3).await;
    assert!(view_text(&rejected).contains(":no_entry_sign: 1 rejected."));
    assert_eq!(
        stack.tools.decisions.lock().unwrap()[2].1,
        json!({"decided_by": "ana", "decision": "reject"})
    );
    assert!(
        stack.slack.calls("chat.update").is_empty(),
        "only views.publish"
    );
    stack.stop().await;
}

#[tokio::test]
async fn mixed_outcomes_each_show_their_line() {
    let stack = Stack::new().await;
    stack.tools.items.lock().unwrap().extend([
        reply("apr_ok", "ana", "posted", 5),
        reply("apr_blocked", "ana", "blocked", 4),
        reply("apr_external", "ana", "external", 3),
        reply("apr_expired", "ana", "expired", 2),
        jira("apr_down", "ana", 1),
    ]);
    stack.tools.refusals.lock().unwrap().extend([
        (
            "apr_blocked".to_string(),
            (409, "audience_blocked".to_string()),
        ),
        (
            "apr_external".to_string(),
            (409, "needs_external_confirm".to_string()),
        ),
        ("apr_expired".to_string(), (409, "expired".to_string())),
        ("apr_down".to_string(), (500, String::new())),
    ]);
    stack.send(home_opened("e1", "Ev1", "U0ANA", None)).await;
    let inbox = stack.published(1).await;
    stack
        .send(home_click(
            "e2",
            "U0ANA",
            "home_approve",
            &inbox,
            &[
                "apr_ok",
                "apr_blocked",
                "apr_external",
                "apr_expired",
                "apr_down",
            ],
        ))
        .await;
    let s = view_text(&stack.published(2).await);
    for line in [
        ":white_check_mark: 1 approved or posted.",
        ":no_entry: 1 blocked: built from restricted sources, and people in the channel aren't on the product team. Reject it, or answer in the thread yourself.",
        ":warning: 1 needs your confirmation: it goes to a channel shared outside the company. Confirm on its card in our DM.",
        ":hourglass: 1 expired, or already decided.",
        ":x: 1 failed: PM OS couldn't reach the tool server.",
    ] {
        assert!(s.contains(line), "{line} in {s}");
    }
    assert_eq!(stack.tools.decisions.lock().unwrap().len(), 5);
    stack.stop().await;
}

#[tokio::test]
async fn nothing_ticked_asks_to_tick_first() {
    let stack = Stack::new().await;
    stack
        .tools
        .items
        .lock()
        .unwrap()
        .push(jira("apr_a1", "ana", 1));
    stack.send(home_opened("e1", "Ev1", "U0ANA", None)).await;
    let inbox = stack.published(1).await;
    stack
        .send(home_click("e2", "U0ANA", "home_approve", &inbox, &[]))
        .await;
    let s = view_text(&stack.published(2).await);
    assert!(s.contains("Tick the drafts first."));
    assert!(stack.tools.decisions.lock().unwrap().is_empty());
    // Ids that aren't approval ids are never sent.
    stack
        .send(home_click("e3", "U0ANA", "home_reject", &inbox, &["../x"]))
        .await;
    assert!(view_text(&stack.published(3).await).contains("Tick the drafts first."));
    assert!(stack.tools.decisions.lock().unwrap().is_empty());
    stack.stop().await;
}

#[tokio::test]
async fn skills_group_by_track_and_show_only_the_roles_slack_skills() {
    let stack = Stack::new().await;
    stack.skill(
        "ask",
        "Answer a question from the context library. Use for anything.",
        "slack web",
        Some("Everyday"),
    );
    stack.skill(
        "strategy-doc",
        "Draft a strategy <doc> & its bets.",
        "slack",
        Some("Strategy and decisions"),
    );
    stack.skill("prd-draft", "Draft a PRD.", "slack", None);
    stack.skill("web-only", "Only on the web.", "web", Some("Everyday"));
    stack.skill("not-in-role", "Someone else's.", "slack", Some("Everyday"));
    stack.runtime.role_skills.lock().unwrap().extend([
        (
            "pm-ana".to_string(),
            vec![
                "ask".to_string(),
                "strategy-doc".to_string(),
                "prd-draft".to_string(),
                "web-only".to_string(),
            ],
        ),
        ("docs".to_string(), vec!["ask".to_string()]),
    ]);
    stack
        .send(home_opened("e1", "Ev1", "U0ANA", Some("skills")))
        .await;
    let published = stack.published(1).await;
    let blocks = published["view"]["blocks"].as_array().unwrap();
    let headers: Vec<&str> = blocks
        .iter()
        .skip(1)
        .filter(|b| b["type"] == "header")
        .map(|b| b["text"]["text"].as_str().unwrap())
        .collect();
    assert_eq!(headers, ["Everyday", "Strategy and decisions", "Other"]);
    let s = view_text(&published);
    assert!(s.contains("DM me and ask in your own words, or name a skill: `strategy-doc: our plan for small teams`"));
    assert!(s.contains("*ask* Answer a question from the context library."));
    assert!(!s.contains("Use for anything"), "the first sentence only");
    assert!(s.contains("*strategy-doc* Draft a strategy &lt;doc&gt; &amp; its bets."));
    assert!(s.contains("*prd-draft* Draft a PRD."));
    assert!(!s.contains("web-only") && !s.contains("not-in-role"), "{s}");
    assert_eq!(*stack.runtime.details.lock().unwrap(), ["pm-ana"]);

    // Bob has no personal bot: the Docs bot's skills.
    stack
        .send(home_opened("e2", "Ev2", "U0BOB", Some("skills")))
        .await;
    let s = view_text(&stack.published(2).await);
    assert!(s.contains("*ask*") && !s.contains("strategy-doc*"));
    assert_eq!(stack.runtime.details.lock().unwrap()[1], "docs");
    stack.stop().await;
}

#[tokio::test]
async fn accounts_connect_and_disconnect() {
    let stack = Stack::new().await;
    stack
        .send(home_opened("e1", "Ev1", "U0ANA", Some("accounts")))
        .await;
    let published = stack.published(1).await;
    let s = view_text(&published);
    assert!(s.contains("*Jira and Confluence* · Not connected"), "{s}");
    assert!(s.contains("https://pmos.example.com/oauth/atlassian/start?state=s-ana"));
    assert!(s.contains(":white_check_mark: *Slack* · Connected"));
    assert!(s.contains("home_unlink_slack"));
    assert!(
        s.contains("The reply assistant, tagging and thread reading stop until you connect again.")
    );
    assert!(
        !s.contains("GitHub"),
        "not in the onboarding answer, not shown"
    );
    assert!(s.contains("*Memory repo:* `example-org/pm-os-memory-ana`"));
    assert!(s.contains("home_refresh"));

    stack
        .send(home_click(
            "e2",
            "U0ANA",
            "home_unlink_slack",
            &published,
            &[],
        ))
        .await;
    let after = stack.published(2).await;
    assert_eq!(
        *stack.tools.unlinks.lock().unwrap(),
        [("ana".to_string(), "slack".to_string())]
    );
    let s = view_text(&after);
    assert!(s.contains("Disconnected Slack."), "{s}");
    assert!(s.contains("https://pmos.example.com/oauth/slack/start?state=s-ana"));
    assert_eq!(tab_of(&after), "accounts");

    // GitHub shows when the answer has it; an unknown provider is never sent.
    *stack.tools.github_in_answer.lock().unwrap() = true;
    stack
        .send(home_click("e3", "U0ANA", "home_refresh", &after, &[]))
        .await;
    assert!(view_text(&stack.published(3).await).contains("*GitHub* · Not connected"));
    stack
        .send(home_click("e4", "U0ANA", "home_unlink_bogus", &after, &[]))
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(stack.tools.unlinks.lock().unwrap().len(), 1);
    assert_eq!(stack.slack.calls("views.publish").len(), 3);
    stack.stop().await;
}

#[tokio::test]
async fn a_needs_approval_notification_republishes_an_open_home() {
    let stack = Stack::new().await;
    stack.send(home_opened("e1", "Ev1", "U0ANA", None)).await;
    let first = stack.published(1).await;
    assert!(view_text(&first).contains("Nothing is waiting for you."));
    stack
        .tools
        .items
        .lock()
        .unwrap()
        .extend([reply("apr_a1", "ana", "Yes", 1), jira("apr_b1", "bob", 1)]);
    let (stop, notifier) = stack.notifier();
    // Bob never opened his Home: nothing is published for him.
    stack.runtime.feed.lock().unwrap().extend([
        json!({"id": "ntf_1", "task_id": "tsk_b", "bot": "docs", "requested_by": "bob",
               "slack": {"channel": "D0BOB", "thread_ts": "1.1"}, "kind": "needs_approval",
               "text": "", "approvals": ["apr_b1"]}),
        json!({"id": "ntf_2", "task_id": "tsk_a", "bot": "pm-ana", "requested_by": "ana",
               "slack": {"channel": "D0ANA", "thread_ts": "1.2"}, "kind": "needs_approval",
               "text": "", "approvals": ["apr_a1"]}),
    ]);
    let second = stack.published(2).await;
    assert_eq!(second["user_id"], "U0ANA");
    assert_eq!(picks(&second), ["pick_apr_a1"]);
    // The cards still go to the DM threads.
    stack.calls("chat.postMessage", 2).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(stack.slack.calls("views.publish").len(), 2);
    stop.cancel();
    let _ = notifier.await;
    stack.stop().await;
}
