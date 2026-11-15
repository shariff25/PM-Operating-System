//! A PM's level and the monthly bets review, from a DM (`docs/staff-principal-spec.md` §1 and
//! §5), against the fake Slack (`common`) and the mock bots runtime (`support`).
//!
//! - `level` shows the level; `level staff` sets it and says what it means; an unknown level
//!   lists the three; a runtime that doesn't answer changes nothing.
//! - `bets review on`, `off` and status, with the spec's cron, tag and prompt, for members
//!   with and without a personal bot.

mod common;
mod support;

use pmos_slack::bets_review::PROMPT;
use serde_json::{json, Value};
use support::{call_text, dm, Stack};

/// The `n`th `chat.postMessage` (from 1): its mrkdwn text, and the thread it went to.
async fn answer(stack: &Stack, n: usize) -> (String, Value) {
    let call = stack.calls("chat.postMessage", n).await[n - 1].clone();
    let text = call.body["blocks"][0]["text"]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("a section: {}", call_text(&call)))
        .to_string();
    (text, call.body["thread_ts"].clone())
}

#[tokio::test]
async fn level_shows_sets_and_explains() {
    let stack = Stack::new().await;
    stack.send(dm("e1", "U0ANA", "500.1", "level")).await;
    let (text, thread) = answer(&stack, 1).await;
    assert_eq!(thread, "500.1");
    assert_eq!(
        text,
        "You're set to the *Senior* level. Change it with `level senior`, `level staff` or `level principal`."
    );

    stack.send(dm("e2", "U0ANA", "500.2", "Level Staff")).await;
    let (text, _) = answer(&stack, 2).await;
    assert_eq!(
        text,
        "Done. I'll work at the Staff level: several teams or a product line, 4 to 6 quarters ahead, written for directors and peer leads."
    );
    assert_eq!(
        *stack.runtime.settings_writes.lock().unwrap(),
        [(
            Some("ana".to_string()),
            "ana".to_string(),
            json!({"level": "staff"})
        )],
        "the PM's own handle, as them"
    );
    stack.send(dm("e3", "U0ANA", "500.3", "level")).await;
    let (text, _) = answer(&stack, 3).await;
    assert!(
        text.starts_with("You're set to the *Staff* level."),
        "{text}"
    );

    stack
        .send(dm("e4", "U0ANA", "500.4", "level principal"))
        .await;
    assert_eq!(
        answer(&stack, 4).await.0,
        "Done. I'll work at the Principal level: the company and its market, 2 to 3 years ahead, written for executives and the board."
    );
    stack.send(dm("e5", "U0ANA", "500.5", "level senior")).await;
    assert_eq!(
        answer(&stack, 5).await.0,
        "Done. I'll work at the Senior level: one product area, this quarter and next, written for the team and its stakeholders."
    );

    // An unknown level lists the three and changes nothing.
    stack
        .send(dm("e6", "U0ANA", "500.6", "level director"))
        .await;
    let (text, _) = answer(&stack, 6).await;
    assert_eq!(
        text,
        "`director` isn't a level. The levels are `senior`, `staff` and `principal`: say `level senior`, `level staff` or `level principal`."
    );
    assert_eq!(stack.runtime.settings_writes.lock().unwrap().len(), 3);

    // Bob (no personal bot) has a level too.
    stack.send(dm("e7", "U0BOB", "500.7", "level staff")).await;
    assert!(answer(&stack, 7).await.0.starts_with("Done."));
    assert_eq!(
        stack
            .runtime
            .levels
            .lock()
            .unwrap()
            .get("bob")
            .map(String::as_str),
        Some("staff")
    );
    assert!(
        stack.runtime.tasks.lock().unwrap().is_empty(),
        "a command, not a task"
    );
    stack.stop().await;
}

#[tokio::test]
async fn level_says_when_the_runtime_fails_and_changes_nothing() {
    let stack = Stack::new().await;
    *stack.runtime.settings_down.lock().unwrap() = true;
    stack.send(dm("e1", "U0ANA", "501.1", "level staff")).await;
    assert_eq!(
        answer(&stack, 1).await.0,
        "PM OS couldn't reach its bots right now, so your level wasn't changed. Try again in a minute."
    );
    stack.send(dm("e2", "U0ANA", "501.2", "level")).await;
    assert_eq!(
        answer(&stack, 2).await.0,
        "PM OS couldn't reach its bots right now. Try again in a minute."
    );
    assert!(stack.runtime.levels.lock().unwrap().is_empty());
    stack.stop().await;
}

#[tokio::test]
async fn bets_review_on_off_and_status() {
    let stack = Stack::new().await;
    stack
        .slack
        .tz
        .lock()
        .unwrap()
        .insert("U0ANA".into(), "Europe/London".into());
    // Ana's reply assistant is on; the bets review never touches it.
    stack.runtime.routines.lock().unwrap().push(json!({
        "id": "rtn_0", "bot": "pm-ana", "owner": "ana", "enabled": true,
        "schedule": {"cron": "*/30 8-17 * * 1-5", "tz": "Europe/London"},
        "prompt": pmos_slack::reply::PROMPT, "next_run_at": "2026-11-16T08:00:00Z",
    }));

    stack.send(dm("e1", "U0ANA", "600.1", "bets review")).await;
    let (text, thread) = answer(&stack, 1).await;
    assert_eq!(thread, "600.1");
    assert_eq!(
        text,
        "*The bets review is off.* Say `bets review on` and PM Ana reviews your bets register at 09:00 on the 1st of each month, and proposes updates for you to approve."
    );

    stack
        .send(dm("e2", "U0ANA", "600.2", "Bets review on"))
        .await;
    let (text, _) = answer(&stack, 2).await;
    assert_eq!(
        text,
        "*The bets review is on.* PM Ana reviews your bets register at 09:00 on the 1st of each month (Europe/London time): for each open bet due for review, it gathers new evidence and proposes an update here for you to approve. Nothing changes in the register without your tap.\nNext run: Tue 1 Dec 09:00 GMT."
    );
    let writes = stack.runtime.routine_writes.lock().unwrap().clone();
    assert_eq!(writes.len(), 1);
    assert_eq!(
        (writes[0].0.as_str(), writes[0].2.as_deref()),
        ("POST", Some("ana"))
    );
    assert_eq!(
        writes[0].3,
        json!({"draft": {
            "bot": "pm-ana", "owner": "ana",
            "schedule": {"cron": "0 9 1 * *", "tz": "Europe/London"},
            "prompt": "[bets-review] Review my bets register with the bets-register skill: find the open bets due for review, gather new evidence, and propose updates.",
            "output": {"kind": "dm"},
            "approval": "approve",
        }}),
        "exactly the spec's draft"
    );
    assert!(PROMPT.starts_with("[bets-review] "));

    stack
        .send(dm("e3", "U0ANA", "600.3", "bets review on"))
        .await;
    assert_eq!(
        answer(&stack, 3).await.0,
        "*The bets review is already on.* Next run: Tue 1 Dec 09:00 GMT. Say `bets review off` to stop it."
    );
    assert_eq!(
        stack.runtime.routine_writes.lock().unwrap().len(),
        1,
        "not created twice"
    );

    stack
        .send(dm("e4", "U0ANA", "600.4", "bets review status"))
        .await;
    assert_eq!(
        answer(&stack, 4).await.0,
        "*The bets review is on:* at 09:00 on the 1st of each month, Europe/London time.\nNext run: Tue 1 Dec 09:00 GMT. Say `bets review off` to stop it."
    );

    // Paused (from the console): status says so, and `on` resumes it.
    stack.runtime.routines.lock().unwrap()[1]["enabled"] = json!(false);
    stack.send(dm("e5", "U0ANA", "600.5", "bets review?")).await;
    assert_eq!(
        answer(&stack, 5).await.0,
        "*The bets review is paused.* Say `bets review on` to resume it, or `bets review off` to remove it."
    );
    stack
        .send(dm("e6", "U0ANA", "600.6", "bets review on"))
        .await;
    assert_eq!(
        answer(&stack, 6).await.0,
        "*The bets review was paused; it's back on.* Next run: Tue 1 Dec 09:00 GMT."
    );
    let writes = stack.runtime.routine_writes.lock().unwrap().clone();
    assert_eq!(
        (writes[1].0.as_str(), writes[1].1.as_str(), &writes[1].3),
        ("PATCH", "rtn_2", &json!({"enabled": true}))
    );

    stack
        .send(dm("e7", "U0ANA", "600.7", "bets review off"))
        .await;
    assert_eq!(
        answer(&stack, 7).await.0,
        "*The bets review is off.* Your bets register is unchanged, and updates already waiting stay until you decide them or they expire."
    );
    let left = stack.runtime.routines.lock().unwrap().clone();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0]["id"], "rtn_0", "the reply assistant stays");
    stack
        .send(dm("e8", "U0ANA", "600.8", "bets review off"))
        .await;
    assert_eq!(
        answer(&stack, 8).await.0,
        "The bets review is already off. Say `bets review on` to start it."
    );

    // Bob has no personal bot: told so, nothing changes.
    stack
        .send(dm("e9", "U0BOB", "600.9", "bets review on"))
        .await;
    assert_eq!(
        answer(&stack, 9).await.0,
        "The bets review runs on your personal bot, `pm-bob`, and you don't have one yet, so nothing was changed. Say `setup` to see where it stands."
    );
    // Cat has no time zone in Slack: PMOS_DEFAULT_TZ.
    stack
        .send(dm("e10", "U0CAT", "600.10", "bets review on"))
        .await;
    answer(&stack, 10).await;
    let writes = stack.runtime.routine_writes.lock().unwrap().clone();
    let last = &writes.last().unwrap().3["draft"];
    assert_eq!(
        (last["bot"].as_str(), last["schedule"]["tz"].as_str()),
        (Some("pm-cat"), Some("UTC"))
    );
    assert_eq!(
        writes.len(),
        4,
        "POST, PATCH, DELETE for Ana; POST for Cat; none for Bob"
    );
    stack.stop().await;
}
