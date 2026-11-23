//! Slice H4 (`docs/openshell-any-host-spec.md` §3.6): the tool server's cross-check of the
//! model gateway. A session opens only while the gateway serves this stack's source (or the
//! stub); every tool call checks that the source is still the one the session opened under,
//! and on a change the call is refused, the session closed and the change audited. Also the
//! `held_real_data` marker and the fake-data content sync.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::response::IntoResponse;
use common::now;
use pmos_contract::model_source::ModelSource;
use pmos_toolserver::audit::{Client, Outcome};
use pmos_toolserver::model_source::SourceGuard;
use pmos_toolserver::service::{SessionError, ToolError};
use serde_json::json;

/// A stand-in gateway whose source can change: `None` answers `500` (unreadable).
async fn gateway(source: Arc<Mutex<Option<String>>>) -> String {
    let app = axum::Router::new().route(
        "/readyz",
        axum::routing::get(move || {
            let source = source.lock().unwrap().clone();
            async move {
                match source {
                    Some(s) => {
                        axum::Json(json!({"ready": true, "model_source": s})).into_response()
                    }
                    None => axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response(),
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    format!("{base}/v1")
}

struct Stack {
    f: common::Fixture,
    source: Arc<Mutex<Option<String>>>,
}

impl Stack {
    async fn new(own: ModelSource, ttl: Duration) -> Self {
        let source = Arc::new(Mutex::new(Some(own.as_str().to_string())));
        let url = gateway(source.clone()).await;
        let guard = Arc::new(SourceGuard::with_ttl(own, &url, ttl));
        Self {
            f: common::fixture_with_model_source(guard),
            source,
        }
    }

    fn serve(&self, s: Option<&str>) {
        *self.source.lock().unwrap() = s.map(String::from);
    }
}

#[tokio::test]
async fn a_session_opens_only_while_the_gateway_serves_this_stacks_source() {
    let s = Stack::new(ModelSource::Local, Duration::ZERO).await;
    for refused in [
        Some("hosted-test"),
        Some("hosted:owner-hosted"),
        Some("bogus"),
        None,
    ] {
        s.serve(refused);
        let err =
            s.f.svc
                .create_session("ana", "ask", "r1", None, now())
                .await
                .unwrap_err();
        assert!(
            matches!(err, SessionError::ModelSourceChanged(_)),
            "{refused:?}: {err}"
        );
        let err =
            s.f.svc
                .create_bot_session("ana", "docs", "r1", None, now())
                .await
                .unwrap_err();
        assert!(matches!(err, SessionError::ModelSourceChanged(_)), "{err}");
    }
    for ok in ["local", "stub"] {
        s.serve(Some(ok));
        assert!(s
            .f
            .svc
            .create_session("ana", "ask", "r1", None, now())
            .await
            .is_ok());
    }
    // A fake-data stack opens under hosted-test, not under local.
    let t = Stack::new(ModelSource::HostedTest, Duration::ZERO).await;
    assert!(t
        .f
        .svc
        .create_session("ana", "ask", "r1", None, now())
        .await
        .is_ok());
    t.serve(Some("local"));
    assert!(t
        .f
        .svc
        .create_session("ana", "ask", "r1", None, now())
        .await
        .is_err());
}

#[tokio::test]
async fn a_tool_call_after_the_source_changed_closes_the_session() {
    let s = Stack::new(ModelSource::Local, Duration::ZERO).await;
    let svc = &s.f.svc;
    let session = svc
        .create_session("ana", "ask", "run-1", None, now())
        .await
        .unwrap();
    let other = svc
        .create_session("ana", "ask", "run-2", None, now())
        .await
        .unwrap();
    let search = json!({"query": "key management"});
    assert!(svc
        .call(&session, "context_search", search.clone(), now())
        .await
        .is_ok());
    // The gateway restarts on the stub: another source than the session opened under.
    s.serve(Some("stub"));
    let err = svc
        .call(&session, "context_search", search.clone(), now())
        .await
        .unwrap_err();
    assert_eq!(err, ToolError::ModelSourceChanged);
    assert!(err.to_string().starts_with("model_source_changed"));
    assert!(
        svc.session(&session.token, now()).await.unwrap().is_none(),
        "the session is closed"
    );
    let err = svc
        .call(&other, "context_search", search.clone(), now())
        .await
        .unwrap_err();
    assert_eq!(err, ToolError::ModelSourceChanged);
    let events = s.f.audit.events();
    let changes: Vec<_> = events
        .iter()
        .filter(|e| e.tool == "system.model_source")
        .collect();
    assert_eq!(changes.len(), 1, "written once per change");
    assert_eq!(changes[0].client, Client::System);
    assert!(changes[0].detail.as_deref().unwrap().contains("`stub`"));
    let closed: Vec<_> = events
        .iter()
        .filter(|e| e.tool == "session.closed")
        .collect();
    assert_eq!(closed.len(), 2);
    assert_eq!(closed[0].run_id, "run-1");
    assert_eq!(closed[0].outcome, Outcome::Denied);
    assert!(closed[0]
        .detail
        .as_deref()
        .unwrap()
        .starts_with("reason: model_source_changed"));
    // A new session opens under the stub, which this stack accepts, and works.
    let fresh = svc
        .create_session("ana", "ask", "run-3", None, now())
        .await
        .unwrap();
    assert!(svc
        .call(&fresh, "context_search", search.clone(), now())
        .await
        .is_ok());
    // Unreadable: refused and closed too.
    s.serve(None);
    assert_eq!(
        svc.call(&fresh, "context_search", search, now())
            .await
            .unwrap_err(),
        ToolError::ModelSourceChanged
    );
    assert!(svc.session(&fresh.token, now()).await.unwrap().is_none());
}

/// The reading is reused for up to 10 seconds: within that window a call is still accepted
/// (the stated window, RT-21), and once it runs out the change is seen.
#[tokio::test]
async fn the_check_uses_a_reading_at_most_ten_seconds_old() {
    let s = Stack::new(ModelSource::Local, Duration::from_millis(300)).await;
    let svc = &s.f.svc;
    let session = svc
        .create_session("ana", "ask", "run-1", None, now())
        .await
        .unwrap();
    let search = json!({"query": "key management"});
    s.serve(Some("hosted-test"));
    assert!(svc
        .call(&session, "context_search", search.clone(), now())
        .await
        .is_ok());
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        svc.call(&session, "context_search", search, now())
            .await
            .unwrap_err(),
        ToolError::ModelSourceChanged
    );
}

/// The HTTP answers: a refused session is `503 model_source_changed` (the runtime puts the
/// task back in the queue), a refused call `409 model_source_changed`.
#[tokio::test]
async fn the_routes_say_model_source_changed() {
    const SERVICE_TOKEN: &str = "service-token-0123456789abcdef0123456789";
    let s = Stack::new(ModelSource::Local, Duration::ZERO).await;
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
    let open = || {
        http.post(format!("{base}/internal/sessions"))
            .bearer_auth(SERVICE_TOKEN)
            .json(&json!({"user": "ana", "skill": "ask", "run_id": "r1"}))
            .send()
    };
    let res = open().await.unwrap();
    assert_eq!(res.status(), 201);
    let token = res.json::<serde_json::Value>().await.unwrap()["token"]
        .as_str()
        .unwrap()
        .to_string();
    s.serve(Some("hosted-test"));
    let res = open().await.unwrap();
    assert_eq!(res.status(), 503);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["code"], "model_source_changed");
    let res = http
        .post(format!("{base}/internal/call"))
        .bearer_auth(SERVICE_TOKEN)
        .header("x-pmos-session", &token)
        .json(&json!({"tool": "context_search", "args": {"query": "x"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 409);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["code"], "model_source_changed");
    cancel.cancel();
}

/// The marker: written at a start with live connectors or GitHub memory, whatever the source;
/// a later fake-data start is refused; Postgres refuses to delete it.
#[tokio::test]
async fn the_held_real_data_marker_is_permanent() {
    let Some(pg) = common::TestPg::new().await else {
        return;
    };
    pg.store.check_held_real_data(false, true).await.unwrap();
    pg.store.check_held_real_data(true, false).await.unwrap();
    assert!(pg.store.held_real_data().await.unwrap());
    // After a restart with fake connectors under `local`: still there, and accepted.
    pg.store.check_held_real_data(false, false).await.unwrap();
    let err = pg
        .store
        .check_held_real_data(false, true)
        .await
        .unwrap_err();
    assert!(err.contains("this database has held real data"), "{err}");
    for sql in [
        "DELETE FROM held_real_data",
        "UPDATE held_real_data SET first_at = now()",
        "TRUNCATE held_real_data",
    ] {
        let err = sqlx::query(sql).execute(pg.store.pool()).await.unwrap_err();
        assert!(err.to_string().contains("permanent"), "{sql}: {err}");
    }
    assert!(pg.store.held_real_data().await.unwrap());
}
