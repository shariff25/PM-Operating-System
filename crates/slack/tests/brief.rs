//! The daily brief's DM commands (`docs/daily-brief.md` §1), against the fake Slack (`common`)
//! and the mock bots runtime (`support`). The hook is called directly, as `bots.rs` will.

mod common;
mod support;

use pmos_slack::brief::{self, PROMPT};
use serde_json::{json, Value};
use support::{call_text, Stack};

const ANA: &str = "U0ANA";

/// Send `text` as `user` through the hook; returns whether it took the message.
async fn say(stack: &Stack, user: &str, text: &str) -> bool {
    brief::handle_dm(&stack.bots, "DANA", "700.1", user, text).await
}

/// The `n`th `chat.postMessage` (from 1): its mrkdwn text and thread.
async fn answer(stack: &Stack, n: usize) -> (String, Value) {
    let call = stack.calls("chat.postMessage", n).await[n - 1].clone();
    let text = call.body["blocks"][0]["text"]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("a section: {}", call_text(&call)))
        .to_string();
    (text, call.body["thread_ts"].clone())
}

fn mine(stack: &Stack, owner: &str) -> Vec<Value> {
    stack
        .runtime
        .routines
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r["owner"] == owner && brief::is_brief_routine(r))
        .cloned()
        .collect()
}

fn other_routine(owner: &str, prompt: &str) -> Value {
    json!({"id": format!("rtn_other_{prompt}"), "owner": owner, "enabled": true,
        "prompt": prompt, "schedule": {"cron": "0 9 1 * *", "tz": "UTC"}})
}

#[tokio::test]
async fn each_phrasing_makes_the_right_routine() {
    let cases = [
        ("brief me at 8", "0 8 * * 1-5", "08:00"),
        ("brief me at 8:30", "30 8 * * 1-5", "08:30"),
        ("Brief me at 7am", "0 7 * * 1-5", "07:00"),
        ("daily brief at 09:00", "0 9 * * 1-5", "09:00"),
        ("brief on", "30 8 * * 1-5", "08:30"),
        ("brief me at 6:15 pm", "15 18 * * 1-5", "18:15"),
    ];
    for (text, cron, shown) in cases {
        let stack = Stack::new().await;
        stack
            .slack
            .tz
            .lock()
            .unwrap()
            .insert(ANA.into(), "Europe/London".into());
        assert!(say(&stack, ANA, text).await, "{text}");
        let routines = mine(&stack, "ana");
        assert_eq!(routines.len(), 1, "{text}");
        assert_eq!(
            routines[0]["schedule"],
            json!({"cron": cron, "tz": "Europe/London"}),
            "{text}"
        );
        assert_eq!(routines[0]["bot"], "pm-ana");
        assert_eq!(routines[0]["prompt"], PROMPT);
        assert_eq!(routines[0]["output"], json!({"kind": "dm"}));
        let (reply, thread) = answer(&stack, 1).await;
        assert_eq!(thread, "700.1");
        assert!(
            reply.contains(&format!("weekdays at {shown} (Europe/London)"))
                && reply.contains("`brief off`"),
            "{reply}"
        );
        let writes = stack.runtime.routine_writes.lock().unwrap().clone();
        assert_eq!(writes[0].2.as_deref(), Some("ana"), "as the PM");
        stack.stop().await;
    }
}

#[tokio::test]
async fn the_service_default_zone_is_the_fallback() {
    let stack = Stack::new().await;
    assert!(say(&stack, ANA, "brief me at 8").await);
    assert_eq!(mine(&stack, "ana")[0]["schedule"]["tz"], "UTC");
    assert!(answer(&stack, 1).await.0.contains("(UTC)"));
    stack.stop().await;
}

#[tokio::test]
async fn asking_again_updates_and_never_duplicates() {
    let stack = Stack::new().await;
    assert!(say(&stack, ANA, "brief me at 8").await);
    assert!(say(&stack, ANA, "brief me at 8").await);
    assert_eq!(mine(&stack, "ana").len(), 1);
    assert!(answer(&stack, 2)
        .await
        .0
        .starts_with("Your brief is already set for weekdays at 08:00"));

    assert!(say(&stack, ANA, "brief me at 9:15").await);
    let routines = mine(&stack, "ana");
    assert_eq!(routines.len(), 1, "the old one is gone");
    assert_eq!(routines[0]["schedule"]["cron"], "15 9 * * 1-5");
    assert!(answer(&stack, 3).await.0.contains("09:15"));

    // A paused one at the same time is resumed, not duplicated.
    stack.runtime.routines.lock().unwrap()[0]["enabled"] = json!(false);
    assert!(say(&stack, ANA, "brief me at 9:15").await);
    assert_eq!(mine(&stack, "ana").len(), 1);
    assert_eq!(mine(&stack, "ana")[0]["enabled"], true);
    assert!(answer(&stack, 4).await.0.contains("back on"));
    stack.stop().await;
}

#[tokio::test]
async fn off_deletes_only_the_brief_routine() {
    let stack = Stack::new().await;
    {
        let mut r = stack.runtime.routines.lock().unwrap();
        r.push(other_routine("ana", "[reply-assistant] x"));
        r.push(other_routine("ana", "[bets-review] y"));
        r.push(other_routine("ana", "Summarise Jira every Friday"));
        r.push(json!({"id": "rtn_cat", "owner": "cat", "enabled": true,
            "prompt": PROMPT, "schedule": {"cron": "0 8 * * 1-5", "tz": "UTC"}}));
    }
    assert!(say(&stack, ANA, "brief off").await);
    assert!(answer(&stack, 1)
        .await
        .0
        .starts_with("Your daily brief is already off."));
    assert_eq!(stack.runtime.routines.lock().unwrap().len(), 4);

    assert!(say(&stack, ANA, "brief on").await);
    assert_eq!(stack.runtime.routines.lock().unwrap().len(), 5);
    assert!(say(&stack, ANA, "brief off").await);
    assert_eq!(
        answer(&stack, 3).await.0,
        "Your daily brief is off. Say `brief me at 8` to turn it back on."
    );
    let left: Vec<String> = stack
        .runtime
        .routines
        .lock()
        .unwrap()
        .iter()
        .map(|r| r["prompt"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(left.len(), 4);
    assert!(left.iter().any(|p| p.starts_with("[reply-assistant]")));
    assert!(left.iter().any(|p| p.starts_with("[bets-review]")));
    assert!(left.iter().any(|p| p.starts_with("Summarise Jira")));
    assert_eq!(
        stack
            .runtime
            .routines
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r["owner"] == "cat")
            .count(),
        1,
        "another PM's brief is untouched"
    );
    stack.stop().await;
}

#[tokio::test]
async fn status_says_on_or_off_and_when() {
    let stack = Stack::new().await;
    assert!(say(&stack, ANA, "brief").await);
    assert_eq!(
        answer(&stack, 1).await.0,
        "Your daily brief is off. Say `brief me at 8` to get it on weekday mornings."
    );
    assert!(say(&stack, ANA, "brief me at 8:30").await);
    assert!(say(&stack, ANA, "brief status").await);
    assert_eq!(
        answer(&stack, 3).await.0,
        "Your brief is on: weekdays at 08:30 (UTC). Next run: Tue 1 Dec 09:00 UTC. Say `brief off` to stop it."
    );
    stack.runtime.routines.lock().unwrap()[0]["enabled"] = json!(false);
    assert!(say(&stack, ANA, "brief").await);
    assert!(answer(&stack, 4)
        .await
        .0
        .starts_with("Your brief is paused"));
    stack.stop().await;
}

#[tokio::test]
async fn bad_times_get_one_helpful_line_and_change_nothing() {
    let stack = Stack::new().await;
    for (i, text) in [
        "brief me at 25",
        "brief me at 8:75",
        "brief me at noonish",
        "brief me at 24:00",
    ]
    .into_iter()
    .enumerate()
    {
        assert!(say(&stack, ANA, text).await, "{text}");
        let (reply, _) = answer(&stack, i + 1).await;
        assert!(
            reply.starts_with("I couldn't read that time. Use 00:00 to 23:59"),
            "{reply}"
        );
    }
    assert!(stack.runtime.routine_writes.lock().unwrap().is_empty());
    assert!(stack.runtime.routines.lock().unwrap().is_empty());
    stack.stop().await;
}

#[tokio::test]
async fn brief_now_starts_a_task_in_the_thread() {
    let stack = Stack::new().await;
    assert!(say(&stack, ANA, "brief now").await);
    let tasks = stack.runtime.tasks.lock().unwrap().clone();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].0.as_deref(), Some("ana"));
    assert_eq!(tasks[0].1["bot"], "pm-ana");
    assert_eq!(tasks[0].1["text"], PROMPT);
    assert_eq!(
        tasks[0].1["slack"],
        json!({"channel": "DANA", "thread_ts": "700.1"})
    );
    assert!(
        stack.runtime.routines.lock().unwrap().is_empty(),
        "the routine is unchanged"
    );
    assert_eq!(
        stack.bots.thread_bot("DANA", "700.1").as_deref(),
        Some("pm-ana")
    );
    stack.stop().await;
}

#[tokio::test]
async fn non_members_and_members_without_a_bot_change_nothing() {
    let stack = Stack::new().await;
    assert!(say(&stack, "U0NOBODY", "brief me at 8").await);
    assert_eq!(
        answer(&stack, 1).await.0,
        "The daily brief is for PM OS team members."
    );
    // Bob is a member with no personal bot.
    assert!(say(&stack, "U0BOB", "brief me at 8").await);
    assert!(answer(&stack, 2).await.0.contains("you don't have one yet"));
    assert!(stack.runtime.routine_writes.lock().unwrap().is_empty());
    assert!(stack.runtime.tasks.lock().unwrap().is_empty());
    stack.stop().await;
}

#[tokio::test]
async fn unrelated_messages_are_left_alone() {
    let stack = Stack::new().await;
    for text in [
        "write a brief for the board",
        "brief the team on pricing",
        "what's in my inbox",
        "briefing notes please",
        "",
    ] {
        assert!(!say(&stack, ANA, text).await, "{text}");
        // Not even a non-member is answered for something unrelated.
        assert!(!say(&stack, "U0NOBODY", text).await, "{text}");
    }
    assert!(stack.runtime.routine_writes.lock().unwrap().is_empty());
    assert!(stack.runtime.tasks.lock().unwrap().is_empty());
    assert!(stack.slack.calls("chat.postMessage").is_empty());
    stack.stop().await;
}
