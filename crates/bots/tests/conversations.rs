//! Conversations (spec §3): a follow-up in the same thread is claimed with the earlier turns
//! of that thread, and only those: never another PM's, another bot's, another thread's, an
//! expired or a redacted one. Every case runs on both stores.

#[macro_use]
mod common;

use chrono::Duration;
use common::Env;
use pmos_bots::model::{self, SlackRef, Source, Task, TaskStatus, REDACTED_MARKER};
use pmos_bots::runtime::{HISTORY_TURNS, TRUNCATED, TURN_FIELD_BYTES};
use serde_json::{json, Value};

/// A task asked from Slack in `thread`.
async fn ask(env: &Env, user: &str, bot: &str, thread: &str, text: &str) -> String {
    let (status, task) = env
        .post(
            "/v1/tasks",
            "slack",
            Some(user),
            json!({"bot": bot, "requested_by": user, "text": text, "source": "slack",
                   "slack": {"channel": format!("D0{user}"), "thread_ts": thread}}),
        )
        .await;
    assert_eq!(status, 201, "{task}");
    task["id"].as_str().unwrap().to_string()
}

/// Claim the next task as `w1` and finish it with `result`; returns the claim.
async fn run(env: &Env, result: &str) -> Value {
    let (status, claim) = env.claim().await;
    assert_eq!(status, 200, "{claim}");
    let tid = claim["task"]["id"].as_str().unwrap();
    let (status, body) = env
        .post(
            &format!("/v1/worker/tasks/{tid}/complete"),
            "worker",
            None,
            json!({"result": result, "approvals": []}),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    claim
}

async fn a_follow_up_carries_the_earlier_turns(env: &Env) {
    ask(env, "ana", "docs", "1731.1", "Draft a PRD for SSO").await;
    let first = run(env, "Who is the user of this feature?").await;
    assert_eq!(
        first["history"],
        json!([]),
        "a first message has no history"
    );

    ask(env, "ana", "docs", "1731.1", "customer").await;
    let second = run(env, "Here is the PRD for customers.").await;
    let history = second["history"].as_array().unwrap();
    assert_eq!(history.len(), 1, "{history:?}");
    assert_eq!(history[0]["text"], "Draft a PRD for SSO");
    assert_eq!(history[0]["result"], "Who is the user of this feature?");
    assert_eq!(history[0]["status"], "done");
    assert!(history[0]["at"].is_string());
    assert!(history[0].get("error").is_none());

    // A failed turn comes with its reason; turns stay in order, oldest first.
    ask(env, "ana", "docs", "1731.1", "generate the content").await;
    let (_, claim) = env.claim().await;
    let tid = claim["task"]["id"].as_str().unwrap();
    env.post(
        &format!("/v1/worker/tasks/{tid}/fail"),
        "worker",
        None,
        json!({"reason": "The model is busy right now."}),
    )
    .await;
    ask(env, "ana", "docs", "1731.1", "try again").await;
    let fourth = run(env, "ok").await;
    let texts: Vec<&str> = fourth["history"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["text"].as_str().unwrap())
        .collect();
    assert_eq!(
        texts,
        ["Draft a PRD for SSO", "customer", "generate the content"]
    );
    assert_eq!(fourth["history"][2]["status"], "failed");
    assert_eq!(
        fourth["history"][2]["error"],
        "The model is busy right now."
    );
}

async fn other_threads_people_and_bots_get_nothing(env: &Env) {
    ask(env, "ana", "docs", "1731.1", "Ana's secret question").await;
    run(env, "Ana's answer").await;

    ask(env, "ana", "docs", "1731.2", "another thread").await;
    assert_eq!(run(env, "x").await["history"], json!([]), "another thread");

    ask(env, "ana", "pm-ana", "1731.1", "same thread, Ana's own bot").await;
    assert_eq!(run(env, "x").await["history"], json!([]), "another bot");

    // Ben in a thread with the same ts (and the same channel, made up) sees nothing of Ana's.
    let (status, task) = env
        .post(
            "/v1/tasks",
            "slack",
            Some("ben"),
            json!({"bot": "docs", "requested_by": "ben", "text": "Ben asks", "source": "slack",
                   "slack": {"channel": "D0ana", "thread_ts": "1731.1"}}),
        )
        .await;
    assert_eq!(status, 201, "{task}");
    assert_eq!(run(env, "x").await["history"], json!([]), "another PM");
}

async fn web_conversations_are_named_by_the_client(env: &Env) {
    let web = |text: &str, conversation: Option<&str>| {
        let mut body = json!({"bot": "docs", "requested_by": "ana", "text": text, "source": "web"});
        if let Some(c) = conversation {
            body["conversation"] = json!(c);
        }
        body
    };
    let (status, task) = env
        .post(
            "/v1/tasks",
            "web",
            Some("ana"),
            web("first", Some("cnv_42")),
        )
        .await;
    assert_eq!(status, 201, "{task}");
    assert_eq!(task["conversation"], "cnv_42");
    run(env, "a question back").await;

    env.post("/v1/tasks", "web", Some("ana"), web("unrelated", None))
        .await;
    assert_eq!(
        run(env, "x").await["history"],
        json!([]),
        "no conversation, no history"
    );

    env.post(
        "/v1/tasks",
        "web",
        Some("ana"),
        web("reply", Some("cnv_42")),
    )
    .await;
    let claim = run(env, "x").await;
    assert_eq!(claim["history"].as_array().unwrap().len(), 1);
    assert_eq!(claim["history"][0]["text"], "first");

    // The evals client may name one too, for a benchmark thread (docs/evals.md), as the
    // benchmark member only (`PMOS_EVALS_AS`, here ben); the id must be well formed whoever
    // sends it.
    let (status, _) = env
        .post(
            "/v1/tasks",
            "evals",
            None,
            json!({"bot": "docs", "requested_by": "ben", "text": "x", "source": "eval",
                   "conversation": "cnv_eval_1"}),
        )
        .await;
    assert_eq!(status, 201);
    // Naming a conversation doesn't lift the evals client's other limits: a PM's handle is
    // still `403`, and a Slack target `400`.
    let (status, _) = env
        .post(
            "/v1/tasks",
            "evals",
            None,
            json!({"bot": "docs", "requested_by": "ana", "text": "x", "source": "eval",
                   "conversation": "cnv_42"}),
        )
        .await;
    assert_eq!(status, 403);
    let (status, _) = env
        .post(
            "/v1/tasks",
            "evals",
            None,
            json!({"bot": "docs", "requested_by": "ben", "text": "x", "source": "eval",
                   "conversation": "cnv_eval_2",
                   "slack": {"channel": "C01", "thread_ts": "1.2"}}),
        )
        .await;
    assert_eq!(status, 400);
    for bad in ["", "has space", &"x".repeat(129)] {
        let (status, body) = env
            .post("/v1/tasks", "web", Some("ana"), web("x", Some(bad)))
            .await;
        assert_eq!(status, 400, "{bad:?}: {body}");
    }
}

async fn expired_and_redacted_turns_are_left_out(env: &Env) {
    // Asked "yesterday": stored directly, more than 24 hours before the follow-up's claim.
    let now = model::now();
    let slack = SlackRef {
        channel: "D0ana".into(),
        thread_ts: "1731.1".into(),
    };
    let old = |minutes_ago: i64, text: &str, result: &str| {
        let mut t = Task::queued(
            "docs",
            "ana",
            Source::Slack,
            text,
            Some(slack.clone()),
            None,
            now - Duration::minutes(minutes_ago),
        );
        t.status = TaskStatus::Done;
        t.result = Some(result.into());
        t
    };
    let expired = old(25 * 60, "a day ago", "old answer");
    let redacted = old(30, &format!("{{\"redacted\": \"{REDACTED_MARKER}\"}}"), "x");
    let kept = old(20, "recent", "recent answer");
    for t in [&expired, &redacted, &kept] {
        env.store.create_task(t, "queued").await.unwrap();
    }
    ask(env, "ana", "docs", "1731.1", "follow-up").await;
    let claim = run(env, "x").await;
    let texts: Vec<&str> = claim["history"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["text"].as_str().unwrap())
        .collect();
    assert_eq!(texts, ["recent"]);

    // A day later, even the recent turn has aged out.
    ask(env, "ana", "docs", "1731.1", "much later").await;
    let late = env
        .rt
        .claim_now("w1", None, now + Duration::hours(25))
        .await
        .unwrap()
        .unwrap();
    assert!(late.history.is_empty(), "{:?}", late.history);
}

async fn history_is_bounded_and_long_results_cut(env: &Env) {
    let long = "a".repeat(TURN_FIELD_BYTES * 2);
    for i in 0..HISTORY_TURNS + 2 {
        ask(env, "ana", "docs", "1731.1", &format!("turn {i}")).await;
        run(
            env,
            if i == HISTORY_TURNS + 1 {
                &long
            } else {
                "short"
            },
        )
        .await;
    }
    ask(env, "ana", "docs", "1731.1", "last").await;
    let claim = run(env, "x").await;
    let history = claim["history"].as_array().unwrap();
    assert_eq!(history.len(), HISTORY_TURNS);
    assert_eq!(history[0]["text"], "turn 2", "the oldest are dropped");
    let cut = history[HISTORY_TURNS - 1]["result"].as_str().unwrap();
    assert!(cut.ends_with(TRUNCATED));
    assert_eq!(cut.len(), TURN_FIELD_BYTES + TRUNCATED.len());
}

both_envs!(
    a_follow_up_carries_the_earlier_turns,
    other_threads_people_and_bots_get_nothing,
    web_conversations_are_named_by_the_client,
    expired_and_redacted_turns_are_left_out,
    history_is_bounded_and_long_results_cut,
);
