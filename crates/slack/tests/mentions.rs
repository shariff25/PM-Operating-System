//! Tagging PM OS in a channel, end to end (`docs/slack-mentions-and-home-spec.md` §2 and §6):
//! the fake Slack, a mock bots runtime and a mock tool server (`support`).
//!
//! - A member's tag: one ephemeral in the thread, a root message in their DM, and a task for
//!   `pm-<handle>` answering in that DM thread, its text the marker, the three ids and the ask.
//! - A top-level tag is its own thread.
//! - A non-member, no personal bot, and Slack not linked each get their note and no task.
//! - Repeats, bots' messages and DM-channel mentions do nothing.
//! - A failed task creation tells the asker.

mod common;
mod support;

use std::time::Duration;

use common::Call;
use serde_json::json;
use support::{mention, Stack, APP_USER};

const CHANNEL: &str = "C0LAUNCH";
const THREAD: &str = "1731500000.000100";
const MESSAGE: &str = "1731500000.000200";

fn text_of(call: &Call) -> String {
    format!("{} {}", call.body["text"], call.body["blocks"])
}

#[tokio::test]
async fn a_members_tag_starts_a_task_answered_in_their_dm() {
    let stack = Stack::new().await;
    let ack = stack
        .send(mention(
            "e1",
            "Ev1",
            "U0ANA",
            CHANNEL,
            MESSAGE,
            Some(THREAD),
            &format!("<@{APP_USER}> does Acme Vault support SAML &amp; SSO?"),
        ))
        .await;
    assert!(ack.get("payload").is_none(), "acked at once, no payload");
    let ephemeral = stack.calls("chat.postEphemeral", 1).await;

    // The DM: opened, then one root message under the personal bot's name, saying where.
    let opened = stack.slack.calls("conversations.open");
    assert_eq!(opened.len(), 1);
    assert_eq!(opened[0].body["users"], "U0ANA");
    let permalink = stack.slack.calls("chat.getPermalink");
    assert_eq!(
        permalink[0].body,
        json!({"channel": CHANNEL, "message_ts": MESSAGE})
    );
    let posts = stack.slack.calls("chat.postMessage");
    assert_eq!(posts.len(), 1);
    let root = &posts[0].body;
    assert_eq!(root["channel"], "D0ANA");
    assert!(
        root.get("thread_ts").is_none(),
        "the root message starts the thread"
    );
    assert_eq!(root["username"], "PM Ana");
    let said = text_of(&posts[0]);
    assert!(
        said.contains(
            "You tagged me in <https://acme.slack.com/archives/C0LAUNCH/p1731500000000200|a thread> \
             in <#C0LAUNCH>. I'm drafting a reply here. Nothing posts there until you say so."
        ),
        "{said}"
    );
    assert!(
        !said.contains("SAML"),
        "the DM never quotes the thread or the ask"
    );
    let root_ts = "1700000000.000001";

    // The task: the personal bot, as Ana, answering in the DM thread.
    let tasks = stack.runtime.tasks.lock().unwrap().clone();
    assert_eq!(tasks.len(), 1);
    let (user, task) = &tasks[0];
    assert_eq!(user.as_deref(), Some("ana"));
    assert_eq!(task["bot"], "pm-ana");
    assert_eq!(task["requested_by"], "ana");
    assert_eq!(task["source"], "slack");
    assert_eq!(
        task["slack"],
        json!({"channel": "D0ANA", "thread_ts": root_ts})
    );
    assert_eq!(
        task["text"],
        format!(
            "[tagged in Slack] channel={CHANNEL} thread_ts={THREAD} message_ts={MESSAGE}\n\
             does Acme Vault support SAML & SSO?"
        )
    );
    assert_eq!(
        stack.bots.thread_bot("D0ANA", root_ts).as_deref(),
        Some("pm-ana"),
        "the PM's reply in the DM thread goes to the same bot"
    );
    assert_eq!(*stack.tools.onboarding_reads.lock().unwrap(), ["ana"]);

    // The thread: one ephemeral, to the asker, in the thread.
    assert_eq!(ephemeral.len(), 1);
    let e = &ephemeral[0].body;
    assert_eq!(e["channel"], CHANNEL);
    assert_eq!(e["user"], "U0ANA");
    assert_eq!(e["thread_ts"], THREAD);
    assert_eq!(
        e["text"],
        "On it. The draft will be in <https://slack.com/app_redirect?channel=D0ANA|our DM>. \
         Nothing posts here until you say so."
    );
    stack.stop().await;
}

#[tokio::test]
async fn a_top_level_tag_is_its_own_thread_and_works_without_a_permalink() {
    let stack = Stack::new().await;
    stack
        .slack
        .no_permalink
        .lock()
        .unwrap()
        .push(CHANNEL.into());
    stack
        .send(mention(
            "e1",
            "Ev1",
            "U0ANA",
            CHANNEL,
            MESSAGE,
            None,
            &format!("<@{APP_USER}>"),
        ))
        .await;
    let ephemeral = stack.calls("chat.postEphemeral", 1).await;
    assert_eq!(ephemeral[0].body["thread_ts"], MESSAGE);
    let task = stack.runtime.tasks.lock().unwrap()[0].1.clone();
    assert_eq!(
        task["text"],
        format!(
            "[tagged in Slack] channel={CHANNEL} thread_ts={MESSAGE} message_ts={MESSAGE}\n\
             Draft a reply to this thread."
        )
    );
    let root = stack.slack.calls("chat.postMessage");
    assert!(
        text_of(&root[0]).contains("You tagged me in a thread in <#C0LAUNCH>."),
        "no link when Slack has no permalink: {}",
        text_of(&root[0])
    );
    stack.stop().await;
}

#[tokio::test]
async fn non_members_and_unready_members_get_their_note_and_no_task() {
    let stack = Stack::new().await;
    let cases = [
        ("U0STRANGER", "PM OS is for the product team."),
        (
            "U0BOB",
            "Tagging needs your personal bot. DM me `setup` to finish setting up.",
        ),
        (
            "U0CAT",
            "Connect your Slack account first, so I can read this thread as you: DM me `setup`.",
        ),
    ];
    for (i, (user, note)) in cases.iter().enumerate() {
        stack
            .send(mention(
                &format!("e{i}"),
                &format!("Ev{i}"),
                user,
                CHANNEL,
                MESSAGE,
                Some(THREAD),
                &format!("<@{APP_USER}> help"),
            ))
            .await;
        let ephemeral = stack.calls("chat.postEphemeral", i + 1).await;
        let e = &ephemeral[i].body;
        assert_eq!(e["user"], *user);
        assert_eq!(e["channel"], CHANNEL);
        assert_eq!(e["thread_ts"], THREAD);
        assert_eq!(e["text"], *note);
        assert!(e.get("blocks").is_none(), "no button in a channel");
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(stack.runtime.tasks.lock().unwrap().is_empty());
    assert!(stack.slack.calls("conversations.open").is_empty());
    assert!(stack.slack.calls("chat.postMessage").is_empty());
    stack.stop().await;
}

#[tokio::test]
async fn repeats_bots_and_dm_mentions_do_nothing() {
    let stack = Stack::new().await;
    let tag = || {
        mention(
            "e1",
            "Ev1",
            "U0ANA",
            CHANNEL,
            MESSAGE,
            Some(THREAD),
            &format!("<@{APP_USER}> summarize"),
        )
    };
    stack.send(tag()).await;
    stack.calls("chat.postEphemeral", 1).await;
    // Slack retries the same event.
    let mut again = tag();
    again["envelope_id"] = json!("e2");
    stack.send(again).await;
    // A bot's message, a message with a subtype, a mention in a DM, and ids that aren't ids.
    let mut from_bot = tag();
    from_bot["envelope_id"] = json!("e3");
    from_bot["payload"]["event_id"] = json!("Ev3");
    from_bot["payload"]["event"]["bot_id"] = json!("B1");
    stack.send(from_bot).await;
    let mut edited = tag();
    edited["envelope_id"] = json!("e4");
    edited["payload"]["event_id"] = json!("Ev4");
    edited["payload"]["event"]["subtype"] = json!("message_changed");
    stack.send(edited).await;
    stack
        .send(mention(
            "e5",
            "Ev5",
            "U0ANA",
            "D0ANA",
            MESSAGE,
            None,
            &format!("<@{APP_USER}> hi"),
        ))
        .await;
    stack
        .send(mention(
            "e6",
            "Ev6",
            "U0ANA",
            "C0<!here>",
            "1.2",
            None,
            "hi",
        ))
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(stack.runtime.tasks.lock().unwrap().len(), 1);
    assert_eq!(stack.slack.calls("chat.postEphemeral").len(), 1);
    assert_eq!(stack.slack.calls("chat.postMessage").len(), 1);
    for id in ["e2", "e3", "e4", "e5", "e6"] {
        assert!(stack.slack.ack(id).is_some(), "{id} acked");
    }
    stack.stop().await;
}

#[tokio::test]
async fn a_failed_task_tells_the_asker() {
    let stack = Stack::new().await;
    *stack.runtime.fail_tasks.lock().unwrap() = true;
    stack
        .send(mention(
            "e1",
            "Ev1",
            "U0ANA",
            CHANNEL,
            MESSAGE,
            Some(THREAD),
            &format!("<@{APP_USER}> draft it"),
        ))
        .await;
    let ephemeral = stack.calls("chat.postEphemeral", 1).await;
    assert_eq!(
        ephemeral[0].body["text"],
        "I couldn't start on this. Try again in a minute, or DM me."
    );
    assert_eq!(ephemeral[0].body["thread_ts"], THREAD);
    assert_eq!(stack.runtime.tasks.lock().unwrap().len(), 1, "it was tried");
    assert_eq!(
        stack.bots.thread_bot("D0ANA", "1700000000.000001"),
        None,
        "the DM thread isn't kept for a task that doesn't exist"
    );
    stack.stop().await;
}
