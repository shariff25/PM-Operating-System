//! Slice H4 (`docs/openshell-any-host-spec.md` §3.6, "Keeping every caller on one source" and
//! "hosted-test, and fake-data mode"): the runtime claims nothing and parses no routine while
//! the gateway serves another source than its own; a task keeps the source of its first claim;
//! every model call names the source and an answer from another is dropped; the
//! `held_real_data` marker; the content check at a reload in fake-data mode.

#[macro_use]
mod common;

use chrono::Duration as Span;
use common::{at, Env};
use pmos_bots::model::TaskStatus;
use pmos_bots::store::{check_held_real_data, BotsStore};
use serde_json::json;

const GOOD_REPLY: &str = r#"{"schedule": {"cron": "0 8 * * 1-5", "tz": "Europe/London"},
  "prompt": "Summarize overnight #launch", "output": {"kind": "dm"}, "approval": "approve"}"#;

fn set_source(env: &Env, source: &str) {
    *env.gw.source.lock().unwrap() = source.into();
}

/// Dispatch waits while the gateway serves another source (or none can be read), and the
/// claim names the gateway's source for the worker; the runtime isn't ready meanwhile.
async fn nothing_is_claimed_while_the_gateway_serves_another_source(env: &Env) {
    let tid = env.ask("What changed overnight?").await["id"]
        .as_str()
        .unwrap()
        .to_string();
    for other in ["hosted-test", "hosted:owner-hosted", "not-a-source"] {
        set_source(env, other);
        let (status, body) = env.claim().await;
        assert_eq!(status, 204, "{other}: {body}");
        let (status, ready) = env.get("/readyz", "none", None).await;
        assert_eq!(status, 503);
        assert_eq!(ready["checks"]["model_source"], "down");
    }
    set_source(env, "local");
    *env.gw.readyz_broken.lock().unwrap() = true;
    assert_eq!(
        env.claim().await.0,
        204,
        "an unreadable source claims nothing"
    );
    *env.gw.readyz_broken.lock().unwrap() = false;
    assert_eq!(env.rt.task(&tid).await.unwrap().status, TaskStatus::Queued);
    assert_eq!(env.ts.session_count(), 0, "no session was opened");
    // Its own source, or the stub, is claimed, and the claim names it.
    let (status, claim) = env.claim().await;
    assert_eq!(status, 200, "{claim}");
    assert_eq!(claim["gateway"]["headers"]["X-Pmos-Model-Source"], "local");
    assert_eq!(claim["task"]["attempts"], 1);
}

/// A task keeps the source of its first claim: after its lease lapses it isn't run again
/// under another source (here the stub), and is once the gateway serves the first again.
async fn a_retried_task_waits_for_the_source_it_started_under(env: &Env) {
    let tid = env.ask("What changed overnight?").await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let t0 = at(2026, 11, 23, 10, 0);
    let claim = env.rt.claim_now("w1", None, t0).await.unwrap().unwrap();
    assert_eq!(claim.gateway.headers["X-Pmos-Model-Source"], json!("local"));
    let t1 = t0 + Span::seconds(121);
    env.rt.recover_leases(t1).await.unwrap();
    set_source(env, "stub");
    assert!(env.rt.claim_now("w2", None, t1).await.unwrap().is_none());
    let task = env.rt.task(&tid).await.unwrap();
    assert_eq!(task.status, TaskStatus::Queued);
    assert_eq!(task.attempts, 1, "the refused claim isn't counted");
    set_source(env, "local");
    let again = env.rt.claim_now("w2", None, t1).await.unwrap().unwrap();
    assert_eq!(again.task.id, tid);
    assert_eq!(again.task.attempts, 2);
}

both_envs!(
    nothing_is_claimed_while_the_gateway_serves_another_source,
    a_retried_task_waits_for_the_source_it_started_under,
);

/// Routine parsing (`extract_json`) is refused `503 model_source_changed` while the gateway
/// serves another source, names the source on its call (X4), and drops an answer that says
/// another source.
#[tokio::test]
async fn routine_parsing_names_the_source_and_refuses_another() {
    let env = Env::memory().await;
    *env.gw.reply.lock().unwrap() = GOOD_REPLY.into();
    let ask = json!({"bot": "pm-ana", "owner": "ana", "tz": "Europe/London",
        "text": "every weekday at 8am summarize overnight #launch"});
    let (status, body) = env
        .post("/v1/routines/parse", "slack", Some("ana"), ask.clone())
        .await;
    assert_eq!(status, 200, "{body}");
    let seen = env.gw.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].0["x-pmos-model-source"], "local");

    set_source(&env, "hosted-test");
    let (status, body) = env
        .post("/v1/routines/parse", "slack", Some("ana"), ask.clone())
        .await;
    assert_eq!(status, 503, "{body}");
    assert_eq!(body["code"], "model_source_changed");
    assert_eq!(env.gw.seen.lock().unwrap().len(), 1, "nothing was sent");

    // The gateway changes between the check and the answer: the answer is dropped.
    set_source(&env, "local");
    *env.gw.answer_source.lock().unwrap() = Some("hosted-test".into());
    let (status, body) = env
        .post("/v1/routines/parse", "slack", Some("ana"), ask.clone())
        .await;
    assert_eq!(status, 503, "{body}");
    assert_eq!(body["code"], "model_source_changed");
    assert!(body.get("draft").is_none());
    *env.gw.answer_source.lock().unwrap() = None;
    let (status, _) = env
        .post("/v1/routines/parse", "slack", Some("ana"), ask)
        .await;
    assert_eq!(status, 200);
    env.finish().await;
}

/// The `held_real_data` marker: written at a start with live connectors whatever the source,
/// refusing a later fake-data start, kept across a restart; and Postgres refuses to delete it.
async fn the_marker_is_written_with_live_connectors_and_refuses_fake_data(
    store: std::sync::Arc<dyn BotsStore>,
) {
    // A fresh database starts in fake-data mode.
    check_held_real_data(store.as_ref(), false, true)
        .await
        .unwrap();
    assert!(!store.held_real_data().await.unwrap());
    // `local` with live connectors writes it.
    check_held_real_data(store.as_ref(), true, false)
        .await
        .unwrap();
    assert!(store.held_real_data().await.unwrap());
    // A later start with fake connectors under `hosted-test` is refused; under `local`, not.
    let err = check_held_real_data(store.as_ref(), false, true)
        .await
        .unwrap_err();
    assert!(err.contains("this database has held real data"), "{err}");
    assert!(err.contains("PMOS_MODEL_SOURCE=hosted-test"), "{err}");
    check_held_real_data(store.as_ref(), false, false)
        .await
        .unwrap();
    // Writing it again is harmless.
    store.mark_held_real_data().await.unwrap();
    // Live connectors under hosted-test: written, then refused.
    assert!(check_held_real_data(store.as_ref(), true, true)
        .await
        .is_err());
}

#[tokio::test]
async fn the_marker_on_both_stores() {
    the_marker_is_written_with_live_connectors_and_refuses_fake_data(std::sync::Arc::new(
        pmos_bots::store::MemoryStore::default(),
    ))
    .await;
    if let Some(pg) = common::TestPg::new().await {
        the_marker_is_written_with_live_connectors_and_refuses_fake_data(std::sync::Arc::new(
            pg.store.clone(),
        ))
        .await;
        pg.drop_schema().await;
    }
}

#[tokio::test]
async fn postgres_keeps_the_marker_for_good() {
    let Some(pg) = common::TestPg::new().await else {
        return;
    };
    pg.store.mark_held_real_data().await.unwrap();
    for sql in [
        "DELETE FROM bots_held_real_data",
        "UPDATE bots_held_real_data SET first_at = now()",
        "TRUNCATE bots_held_real_data",
    ] {
        let err = sqlx::query(sql).execute(pg.store.pool()).await.unwrap_err();
        assert!(err.to_string().contains("permanent"), "{sql}: {err}");
    }
    // A restarted process reads it.
    let again = pg.reconnect().await;
    assert!(again.held_real_data().await.unwrap());
    pg.drop_schema().await;
}

/// In fake-data mode a changed checkout is checked before it is loaded: without the marker, or
/// with a restricted file, nothing is claimed and the runtime isn't ready, until it passes.
#[tokio::test]
async fn a_reload_in_fake_data_mode_checks_the_content() {
    let env = Env::memory().await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    std::fs::write(root.join("x.md"), "---\nsensitivity: internal\n---\nx").unwrap();
    let mut reloader = pmos_bots::catalog::Reloader::new(&root).with_test_content_check();
    let today = chrono::Utc::now().date_naive();
    std::fs::write(root.join("y.md"), "---\nsensitivity: internal\n---\ny").unwrap();
    let outcome = reloader.check(&env.rt, today).await;
    assert!(
        matches!(&outcome, pmos_bots::catalog::Reload::Kept(why) if why.contains("TEST-CONTENT")),
        "{outcome:?}"
    );
    assert!(env.rt.content_refused().is_some());
    env.ask("What changed overnight?").await;
    assert_eq!(env.claim().await.0, 204);
    let (status, ready) = env.get("/readyz", "none", None).await;
    assert_eq!(status, 503);
    assert_eq!(ready["checks"]["content"], "down");
    // The marker added: the check passes (the catalog itself is another matter: this tree
    // has no bots, so the lint keeps the old one), and claims go on.
    std::fs::write(root.join("TEST-CONTENT"), "test data only\n").unwrap();
    let _ = reloader.check(&env.rt, today).await;
    assert!(env.rt.content_refused().is_none());
    assert_eq!(env.claim().await.0, 200);
    // A restricted file stops it again.
    std::fs::write(root.join("z.md"), "---\nsensitivity: restricted\n---\nz").unwrap();
    let outcome = reloader.check(&env.rt, today).await;
    assert!(
        matches!(&outcome, pmos_bots::catalog::Reload::Kept(why) if why.contains("z.md is marked restricted")),
        "{outcome:?}"
    );
    env.finish().await;
}
