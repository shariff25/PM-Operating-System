//! Slice MP7 (`docs/model-providers-spec.md` §4.9, §4.10): an eval task's model calls are
//! pinned to each role's primary; a worker reports which labelled models answered a task and
//! whether a fallback did; the runtime keeps it on the task, and an Auditor's view shows it.

#[macro_use]
mod common;

use common::Env;
use pmos_bots::model::Source;
use pmos_bots::runtime::{fallback_off, FallbackMode};
use serde_json::{json, Value};

fn id(v: &Value) -> String {
    v["id"].as_str().unwrap().to_string()
}

/// Claims until `tid` is the claimed task.
async fn claim_it(env: &Env, tid: &str) -> Value {
    for _ in 0..4 {
        let (status, c) = env.claim().await;
        assert_eq!(status, 200, "{c}");
        if c["task"]["id"] == tid {
            return c;
        }
    }
    panic!("{tid} was never claimed");
}

async fn an_eval_tasks_calls_keep_to_the_primary_and_a_pms_may_fall_back(env: &Env) {
    let slack = env.ask("a PM's question").await;
    let c = claim_it(env, &id(&slack)).await;
    assert!(
        c["gateway"]["headers"].get("X-Pmos-Fallback").is_none(),
        "{c}"
    );
    let new = json!({"bot": "docs", "requested_by": "ben", "text": "Q", "source": "eval"});
    let (_, task) = env.post("/v1/tasks", "evals", None, new).await;
    let c = claim_it(env, &id(&task)).await;
    assert_eq!(c["gateway"]["headers"]["X-Pmos-Fallback"], "off", "{c}");
}

async fn the_worker_reports_which_models_answered_and_the_task_keeps_it(env: &Env) {
    let task = env.ask("What changed?").await;
    let tid = id(&task);
    claim_it(env, &tid).await;
    let sha = "a".repeat(64);
    // A label that isn't one, or a hash that isn't hex, is refused.
    for bad in [
        json!([{"label": "has space", "calls": 1}]),
        json!([{"label": "hosted-model-a", "model_sha256": "xyz", "calls": 1}]),
    ] {
        let (status, body) = env
            .post(
                &format!("/v1/worker/tasks/{tid}/complete"),
                "worker",
                None,
                json!({"result": "Done.", "models_used": bad, "fallback_used": true}),
            )
            .await;
        assert_eq!(status, 400, "{body}");
    }
    let (status, done) = env
        .post(
            &format!("/v1/worker/tasks/{tid}/complete"),
            "worker",
            None,
            json!({"result": "Done.", "fallback_used": true, "models_used": [
                {"label": "hosted-model-a", "model_sha256": sha, "calls": 2},
                {"label": "hosted-model-b", "model_sha256": sha, "calls": 1}]}),
        )
        .await;
    assert_eq!(status, 200, "{done}");
    let (_, t) = env
        .get(&format!("/v1/tasks/{tid}"), "slack", Some("ana"))
        .await;
    assert_eq!(t["fallback_used"], true, "{t}");
    assert_eq!(t["models_used"][1]["label"], "hosted-model-b");
    assert_eq!(t["models_used"][0]["calls"], 2);
    // The Slack feed's item says so, for the mark on the reply and each draft card.
    let done = env
        .feed()
        .await
        .into_iter()
        .rfind(|n| n["task_id"] == tid.as_str())
        .unwrap();
    assert_eq!(done["fallback_used"], true, "{done}");
    // A worker that reports nothing (an older harness) leaves the task as before.
    let other = env.ask("Another?").await;
    let oid = id(&other);
    claim_it(env, &oid).await;
    let (status, _) = env
        .post(
            &format!("/v1/worker/tasks/{oid}/complete"),
            "worker",
            None,
            json!({"result": "Done."}),
        )
        .await;
    assert_eq!(status, 200);
    let (_, t) = env
        .get(&format!("/v1/tasks/{oid}"), "slack", Some("ana"))
        .await;
    assert!(
        t.get("fallback_used").is_none() && t.get("models_used").is_none(),
        "{t}"
    );
}

both_envs!(
    an_eval_tasks_calls_keep_to_the_primary_and_a_pms_may_fall_back,
    the_worker_reports_which_models_answered_and_the_task_keeps_it,
);

#[test]
fn refuse_pins_only_a_benchmarked_bot_and_mark_pins_none() {
    let persona = |always_on: bool, preview: bool, benchmarked: bool| {
        let mut p = common::persona_catalog()
            .persona("chief")
            .cloned()
            .expect("the test catalog has chief");
        p.always_on = always_on;
        p.preview = preview;
        p.benchmarked = benchmarked;
        p
    };
    let benchmarked = persona(false, false, true);
    let preview = persona(false, true, false);
    let always_on = persona(true, false, true);
    use FallbackMode::*;
    assert!(fallback_off(Source::Slack, Some(&benchmarked), Refuse));
    assert!(!fallback_off(Source::Slack, Some(&benchmarked), Mark));
    assert!(!fallback_off(Source::Slack, Some(&preview), Refuse));
    assert!(!fallback_off(Source::Slack, Some(&always_on), Refuse));
    assert!(
        !fallback_off(Source::Slack, None, Refuse),
        "Nudge is always marked"
    );
    assert!(
        fallback_off(Source::Eval, None, Mark),
        "an eval task is always pinned"
    );
    assert_eq!(FallbackMode::parse(None), Ok(Mark));
    assert_eq!(FallbackMode::parse(Some("refuse")), Ok(Refuse));
    assert!(FallbackMode::parse(Some("sometimes")).is_err());
}
