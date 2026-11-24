//! Slice H12 (`docs/openshell-any-host-spec.md` §3.6): the runtime under
//! `PMOS_MODEL_SOURCE=hosted`. It claims nothing and parses no routine while the tool server's
//! switch status isn't `ok` (r7, X3: an acknowledgement waiting, or no readable answer); routine
//! parsing names its run (`routine-<id>`); a bot whose role reads data above the gateway's
//! ceiling doesn't run (V11), and the claim carries the ceiling for the harness; the test-only
//! list is taken only in fake-data mode; and `GET /v1/model-source` gives the indicator.

#[macro_use]
mod common;

use common::Env;
use pmos_bots::model::TaskStatus;
use serde_json::json;

const GOOD_REPLY: &str = r#"{"schedule": {"cron": "0 8 * * 1-5", "tz": "Europe/London"},
  "prompt": "Summarize overnight #launch", "output": {"kind": "dm"}, "approval": "approve"}"#;

fn switch(env: &Env, ok: bool, needed: bool, fake: bool) {
    *env.ts.switch.lock().unwrap() =
        Some(json!({"ok": ok, "acknowledgement_needed": needed, "fake_data": fake}));
}

#[tokio::test]
async fn nothing_is_claimed_until_the_switch_is_cleared() {
    let env = Env::hosted().await;
    let tid = env.ask("What changed overnight?").await["id"]
        .as_str()
        .unwrap()
        .to_string();
    switch(&env, false, true, false);
    assert_eq!(env.claim().await.0, 204, "an acknowledgement waits");
    *env.ts.switch.lock().unwrap() = None;
    assert_eq!(
        env.claim().await.0,
        204,
        "no readable status claims nothing"
    );
    assert_eq!(env.ts.session_count(), 0, "no session was opened");
    assert_eq!(env.rt.task(&tid).await.unwrap().status, TaskStatus::Queued);
    switch(&env, true, false, false);
    let (status, claim) = env.claim().await;
    assert_eq!(status, 200, "{claim}");
    assert_eq!(
        claim["gateway"]["headers"]["X-Pmos-Model-Source"],
        "hosted:owner-hosted"
    );
    assert_eq!(claim["max_data_class"], "restricted");
}

#[tokio::test]
async fn routine_parsing_waits_for_the_switch_and_names_its_run() {
    let env = Env::hosted().await;
    *env.gw.reply.lock().unwrap() = GOOD_REPLY.into();
    let ask = json!({"bot": "pm-ana", "owner": "ana", "tz": "Europe/London",
        "text": "every weekday at 8am summarize overnight #launch"});
    switch(&env, false, true, false);
    let (status, body) = env
        .post("/v1/routines/parse", "slack", Some("ana"), ask.clone())
        .await;
    assert_eq!(status, 503, "{body}");
    assert_eq!(body["code"], "model_source_acknowledgement_required");
    assert!(body["error"]
        .as_str()
        .unwrap()
        .contains("An administrator must confirm the model source switch"));
    assert!(env.gw.seen.lock().unwrap().is_empty(), "nothing was sent");
    *env.ts.switch.lock().unwrap() = None;
    let (status, _) = env
        .post("/v1/routines/parse", "slack", Some("ana"), ask.clone())
        .await;
    assert_eq!(status, 503);
    assert!(env.gw.seen.lock().unwrap().is_empty(), "nothing was sent");
    switch(&env, true, false, false);
    let (status, body) = env
        .post("/v1/routines/parse", "slack", Some("ana"), ask)
        .await;
    assert_eq!(status, 200, "{body}");
    let seen = env.gw.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].0["x-pmos-model-source"], "hosted:owner-hosted");
    assert!(
        seen[0].0["x-pmos-run"].starts_with("routine-"),
        "{:?}",
        seen[0].0
    );
}

#[tokio::test]
async fn a_bot_above_the_ceiling_is_not_dispatched() {
    let env = Env::hosted().await;
    *env.gw.ceiling.lock().unwrap() = Some("internal".into());
    // pm-ana's role reads restricted data.
    let (status, task) = env
        .post(
            "/v1/tasks",
            "slack",
            Some("ana"),
            json!({"bot": "pm-ana", "requested_by": "ana", "text": "Draft the launch notes",
                   "source": "slack", "slack": {"channel": "D0ANA", "thread_ts": "1731.0002"}}),
        )
        .await;
    assert_eq!(status, 201, "{task}");
    let tid = task["id"].as_str().unwrap().to_string();
    assert_eq!(env.claim().await.0, 204);
    let t = env.rt.task(&tid).await.unwrap();
    assert_eq!(t.status, TaskStatus::Failed);
    let (_, events) = env
        .get(&format!("/v1/tasks/{tid}/events"), "slack", Some("ana"))
        .await;
    let text = events.to_string();
    assert!(
        text.contains("`restricted`") && text.contains("ceiling (`internal`)"),
        "{text}"
    );
    assert_eq!(env.ts.session_count(), 0, "no session was opened");
    // The docs bot (public) still runs, and its claim says the ceiling.
    env.ask("What changed overnight?").await;
    let (status, claim) = env.claim().await;
    assert_eq!(status, 200, "{claim}");
    assert_eq!(claim["max_data_class"], "internal");
}

#[tokio::test]
async fn the_test_only_list_runs_only_in_fake_data_mode() {
    let env = Env::hosted().await;
    *env.gw.source.lock().unwrap() = "hosted:stack-test".into();
    env.ask("What changed overnight?").await;
    switch(&env, true, false, false);
    assert_eq!(env.claim().await.0, 204, "not fake-data mode: refused");
    switch(&env, true, false, true);
    let (status, claim) = env.claim().await;
    assert_eq!(status, 200, "{claim}");
    assert_eq!(
        claim["gateway"]["headers"]["X-Pmos-Model-Source"],
        "hosted:stack-test"
    );
    // A local runtime never takes it.
    let local = Env::memory().await;
    *local.gw.source.lock().unwrap() = "hosted:stack-test".into();
    local.ask("What changed overnight?").await;
    assert_eq!(local.claim().await.0, 204);
}

#[tokio::test]
async fn the_model_source_view_names_labels_only() {
    let env = Env::hosted().await;
    let (status, view) = env.get("/v1/model-source", "web", Some("ana")).await;
    assert_eq!(status, 200, "{view}");
    assert_eq!(view["source"], "hosted");
    assert_eq!(view["name"], "owner-hosted");
    assert_eq!(view["fake_data"], false);
    assert_eq!(view["switch_pending"], false);
    switch(&env, false, true, false);
    let (_, view) = env.get("/v1/model-source", "web", Some("ana")).await;
    assert_eq!(view["switch_pending"], true);
    let local = Env::memory().await;
    let (_, view) = local.get("/v1/model-source", "slack", Some("ana")).await;
    assert_eq!(
        view,
        json!({"source": "local", "fake_data": false, "switch_pending": false})
    );
    // A worker's token may not read it.
    let (status, _) = env.get("/v1/model-source", "worker", None).await;
    assert_eq!(status, 403);
}
