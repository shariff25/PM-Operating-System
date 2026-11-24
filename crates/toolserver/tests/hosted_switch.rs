//! Slice H12 (`docs/openshell-any-host-spec.md` §3.6): the tool server under
//! `PMOS_MODEL_SOURCE=hosted`. The switch acknowledgement keyed on the database's history (r7,
//! X3), its routes, `session.open` and `system.model_source` in the audit log (§3.9), the
//! test-only list as fake-data mode, and the data-class ceiling on tool results (r6, V11).
//! Fake data only: the gateway is a stand-in that reports what each test sets.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::now;
use pmos_contract::model_source::ModelSource;
use pmos_toolserver::audit::Outcome;
use pmos_toolserver::model_source::{History, SourceGuard, ACKNOWLEDGEMENT_REQUIRED};
use pmos_toolserver::service::{SessionError, ToolError};
use serde_json::{json, Value};

const TO: &str = "hosted:owner-hosted";

/// A stand-in gateway whose `/readyz` body can change.
async fn gateway(body: Arc<Mutex<Value>>) -> String {
    let app = axum::Router::new().route(
        "/readyz",
        axum::routing::get(move || {
            let body = body.lock().unwrap().clone();
            async move { axum::Json(body) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    format!("{base}/v1")
}

fn readyz(source: &str, ceiling: &str) -> Value {
    json!({"ready": true, "model_source": source, "approved_list": "release",
           "approved_list_sha256": "a".repeat(64), "commit": "c0ffee", "max_data_class": ceiling})
}

struct Stack {
    f: common::Fixture,
    body: Arc<Mutex<Value>>,
    guard: Arc<SourceGuard>,
}

async fn stack(history: History, held: bool, fake_eligible: bool, source: &str) -> Stack {
    let body = Arc::new(Mutex::new(readyz(source, "restricted")));
    let url = gateway(body.clone()).await;
    let guard = Arc::new(
        SourceGuard::with_ttl(ModelSource::Hosted, &url, Duration::ZERO)
            .with_history(history)
            .with_held_real_data(held)
            .with_fake_eligible(fake_eligible),
    );
    Stack {
        f: common::fixture_with_model_source(guard.clone()),
        body,
        guard,
    }
}

async fn open(s: &Stack) -> Result<pmos_toolserver::session::Session, SessionError> {
    s.f.svc
        .create_session("ana", "ask", "run-h", None, now())
        .await
}

fn refused_for_acknowledgement(r: Result<pmos_toolserver::session::Session, SessionError>) {
    match r {
        Err(SessionError::ModelSourceChanged(d)) => {
            assert!(d.starts_with(ACKNOWLEDGEMENT_REQUIRED), "{d}")
        }
        other => panic!("expected the acknowledgement refusal, got {other:?}"),
    }
}

/// A fresh database goes straight to the approved entry: no acknowledgement; the session's
/// opening is audited with the source, the list's digest and the commit.
#[tokio::test]
async fn a_fresh_database_needs_no_acknowledgement_and_opening_is_audited() {
    let s = stack(History::default(), false, false, TO).await;
    let session = open(&s).await.unwrap();
    assert!(!session.token.is_empty());
    let events = s.f.audit.events();
    let opened = events.iter().find(|e| e.tool == "session.open").unwrap();
    let detail = opened.detail.as_deref().unwrap();
    assert!(detail.contains(&format!("model_source: {TO}")), "{detail}");
    assert!(
        detail.contains(&"a".repeat(64)) && detail.contains("c0ffee"),
        "{detail}"
    );
    assert_eq!(opened.run_id, "run-h");
    let status = s.guard.status().await;
    assert!(status.ok && !status.acknowledgement_needed);
    assert_eq!(status.hosted_name.as_deref(), Some("owner-hosted"));
}

/// X3: a database that served `local` (with test sources in between) needs the
/// acknowledgement; a wrong `to` is refused; once recorded the session opens, the change is
/// audited, and a later `local` voids it.
#[tokio::test]
async fn a_switch_from_local_waits_for_its_acknowledgement() {
    let history = History::default();
    let t0 = now() - chrono::Duration::hours(3);
    history.served("local", t0).await.unwrap();
    history
        .served("stub", t0 + chrono::Duration::minutes(5))
        .await
        .unwrap();
    history
        .served("hosted-test", t0 + chrono::Duration::minutes(6))
        .await
        .unwrap();
    let s = stack(history.clone(), false, false, TO).await;
    refused_for_acknowledgement(open(&s).await);
    let status = s.guard.status().await;
    assert!(status.acknowledgement_needed && !status.ok);
    // Bot sessions are gated the same way.
    assert!(s
        .f
        .svc
        .create_bot_session("ana", "docs", "run-b", None, now())
        .await
        .is_err());
    // Only what the gateway serves now can be acknowledged.
    assert!(s
        .guard
        .acknowledge("hosted:another", "op", "why", now())
        .await
        .is_err());
    let acked_at = now() - chrono::Duration::hours(1);
    let h = s
        .guard
        .acknowledge(TO, "op", "owner approved the provider", acked_at)
        .await
        .unwrap();
    assert!(h.iter().any(|r| r.source == "local" && r.real));
    open(&s).await.unwrap();
    let events = s.f.audit.events();
    let change = events
        .iter()
        .find(|e| e.tool == "system.model_source")
        .unwrap();
    assert!(change
        .detail
        .as_deref()
        .unwrap()
        .contains(&format!("to: {TO}")));
    // The database serves `local` again: the acknowledgement no longer holds.
    history.served("local", now()).await.unwrap();
    refused_for_acknowledgement(open(&s).await);
}

/// The `held_real_data` marker with no history of this entry needs one too.
#[tokio::test]
async fn held_real_data_without_this_entry_needs_an_acknowledgement() {
    let s = stack(History::default(), true, false, TO).await;
    refused_for_acknowledgement(open(&s).await);
}

/// The routes: the status the runtime reads (`503` while refused), and the acknowledgement
/// `pmos-admin hosted acknowledge-switch` posts, which writes its audit event.
#[tokio::test]
async fn the_status_and_acknowledgement_routes() {
    const SERVICE_TOKEN: &str = "service-token-0123456789abcdef0123456789";
    let history = History::default();
    history
        .served("local", now() - chrono::Duration::hours(2))
        .await
        .unwrap();
    let s = stack(history, false, false, TO).await;
    let cancel = tokio_util::sync::CancellationToken::new();
    let app = pmos_toolserver::router(
        s.f.svc.clone(),
        pmos_toolserver::AppConfig {
            service_token: SERVICE_TOKEN.into(),
            bot: None,
            console_token: None,
            content_root: s.f.root.clone(),
            oauth: None,
            allowed_hosts: vec!["127.0.0.1".into(), "localhost".into()],
        },
        cancel.clone(),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    let http = reqwest::Client::new();
    let status = || {
        http.get(format!("{base}/internal/system/model-source-status"))
            .bearer_auth(SERVICE_TOKEN)
            .send()
    };
    let res = status().await.unwrap();
    assert_eq!(res.status(), 503);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["acknowledgement_needed"], true);
    assert_eq!(body["ok"], false);
    // No token, no status.
    let res = http
        .get(format!("{base}/internal/system/model-source-status"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 401);
    let ack = |body: Value| {
        http.post(format!(
            "{base}/internal/system/model-source-acknowledgement"
        ))
        .bearer_auth(SERVICE_TOKEN)
        .json(&body)
        .send()
    };
    // No reason, a line break, another source: refused.
    for bad in [
        json!({"to": TO, "by": "op", "reason": ""}),
        json!({"to": TO, "by": "op", "reason": "a\nb"}),
        json!({"to": "hosted:another", "by": "op", "reason": "x"}),
    ] {
        let res = ack(bad.clone()).await.unwrap();
        assert!(res.status().is_client_error(), "{bad}: {}", res.status());
    }
    let res = ack(json!({"to": TO, "by": "op", "reason": "approved, H13"}))
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let res = status().await.unwrap();
    assert_eq!(res.status(), 200);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["ok"], true);
    let events = s.f.audit.events();
    let acked = events
        .iter()
        .find(|e| e.tool == "system.model_source_acknowledged")
        .unwrap();
    let d = acked.detail.as_deref().unwrap();
    assert!(
        d.contains(&format!("to: {TO}")) && d.contains("by: op") && d.contains("local"),
        "{d}"
    );
    cancel.cancel();
}

/// A gateway with the test-only list is taken under `hosted` only by a stack that passed every
/// fake-data check; nothing else takes it.
#[tokio::test]
async fn the_test_only_list_is_fake_data_mode() {
    let s = stack(History::default(), false, false, "hosted:stack-test").await;
    assert!(matches!(
        open(&s).await,
        Err(SessionError::ModelSourceChanged(_))
    ));
    let s = stack(History::default(), false, true, "hosted:stack-test").await;
    open(&s).await.unwrap();
    let status = s.guard.status().await;
    assert!(status.ok && status.fake_data);
    // It never counts as a real source, so a later approved entry needs no acknowledgement.
    *s.body.lock().unwrap() = readyz(TO, "restricted");
    open(&s).await.unwrap();
}

/// V11: under `hosted`, a tool result above the gateway's effective ceiling is refused,
/// naming the class and not the content, and audited as denied; `restricted` allows all.
#[tokio::test]
async fn a_result_above_the_ceiling_is_refused_and_audited() {
    let s = stack(History::default(), false, false, TO).await;
    let session = open(&s).await.unwrap();
    let search = json!({"query": "key management"});
    assert!(s
        .f
        .svc
        .call(&session, "context_search", search.clone(), now())
        .await
        .is_ok());
    *s.body.lock().unwrap() = readyz(TO, "public");
    let err =
        s.f.svc
            .call(&session, "context_search", search.clone(), now())
            .await
            .unwrap_err();
    let ToolError::Forbidden(m) = &err else {
        panic!("{err:?}")
    };
    assert!(m.contains("ceiling (`public`)"), "{m}");
    assert!(!m.contains("Key management"), "{m}");
    let denied =
        s.f.audit
            .events()
            .into_iter()
            .rfind(|e| e.tool.contains("context") && e.outcome == Outcome::Denied);
    assert!(denied.is_some(), "the refused read is audited");
    // A gateway that reports no ceiling under `hosted` is read as the lowest (fail closed).
    *s.body.lock().unwrap() = json!({"ready": true, "model_source": TO});
    assert!(s
        .f
        .svc
        .call(&session, "context_search", search, now())
        .await
        .is_err());
}

/// The history in Postgres: recorded, read back and permanent.
#[tokio::test]
async fn the_history_in_the_database_is_permanent() {
    let Some(pg) = common::TestPg::new().await else {
        return;
    };
    let h = History::Pg(pg.store.clone());
    let t = now();
    assert_eq!(h.served("local", t).await.unwrap(), None);
    assert_eq!(
        h.served(TO, t + chrono::Duration::minutes(1))
            .await
            .unwrap(),
        Some("local".into())
    );
    h.served("stub", t + chrono::Duration::minutes(2))
        .await
        .unwrap();
    let records = h.records().await.unwrap();
    assert_eq!(records.len(), 3);
    assert!(records.iter().find(|r| r.source == "local").unwrap().real);
    assert!(!records.iter().find(|r| r.source == "stub").unwrap().real);
    h.acknowledge(pmos_toolserver::model_source::Acknowledgement {
        to: TO.into(),
        by: "op".into(),
        reason: "test".into(),
        at: t,
    })
    .await
    .unwrap();
    assert_eq!(h.acknowledgements().await.unwrap().len(), 1);
    for sql in [
        "DELETE FROM model_source_history",
        "TRUNCATE model_source_history",
        "UPDATE model_source_history SET real = false",
        "DELETE FROM model_source_acknowledgements",
        "UPDATE model_source_acknowledgements SET reason = 'x'",
        "TRUNCATE model_source_acknowledgements",
    ] {
        assert!(
            sqlx::query(sql).execute(pg.store.pool()).await.is_err(),
            "{sql} must be refused"
        );
    }
}
