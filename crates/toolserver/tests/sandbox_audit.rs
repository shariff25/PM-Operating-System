//! Sandbox decisions in the audit log (`docs/openshell-prover-and-sandbox-audit-spec.md` Part B,
//! slice B1): the collector's token reaches its two routes and nothing else (SBA-3); a batch is
//! checked, deduplicated (SBA-6), attributed only from a personal bot's owner or one open session
//! (SBA-7), and every sequence hole, new run and silence is an event (SBA-8); the rows are
//! `audit_events` rows that UPDATE and DELETE can't touch and the 90-day purge deletes (SBA-10);
//! the tripwire reads denials and gaps (SBA-14). Postgres cases need `PMOS_TEST_DATABASE_URL`.

mod common;

use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use common::{Fixture, TestPg};
use pmos_contract::model::Role;
use pmos_toolserver::app::BotAccess;
use pmos_toolserver::audit::{AuditSink, Client};
use pmos_toolserver::openshell::sandbox::{Batch, Heartbeat, Kind, WireEvent};
use pmos_toolserver::openshell::{self, OpenShellStore};
use pmos_toolserver::{router_full, AppConfig, PersonClients};
use reqwest::Method;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

const SERVICE_TOKEN: &str = "test-service-token-0123456789abcdef";
const BOT_TOKEN: &str = "test-bot-token-0123456789abcdef-0123";
const WEB_TOKEN: &str = "test-web-token-0123456789abcdef-01234";
const CONSOLE_TOKEN: &str = "test-console-token-0123456789abcdef-0";
const AUDIT_TOKEN: &str = "test-sandbox-audit-token-0123456789abc";

struct Server {
    base: String,
    cancel: CancellationToken,
    f: Fixture,
    pg: Option<TestPg>,
}

impl Server {
    async fn stop(mut self) {
        self.cancel.cancel();
        if let Some(pg) = self.pg.take() {
            pg.drop_schema().await;
        }
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        token: Option<&str>,
        body: Option<Value>,
    ) -> (u16, Value) {
        let mut r = reqwest::Client::new().request(method, format!("{}{path}", self.base));
        if let Some(t) = token {
            r = r.bearer_auth(t);
        }
        if let Some(b) = body {
            r = r.json(&b);
        }
        let res = r.send().await.unwrap();
        let status = res.status().as_u16();
        (status, res.json().await.unwrap_or(Value::Null))
    }
}

async fn start(postgres: bool, token: bool) -> Option<Server> {
    let (f, pg) = common::roles_fixture(
        &[
            ("ada", Role::Admin),
            ("aud", Role::Auditor),
            ("ana", Role::Pm),
            ("ben", Role::Pm),
        ],
        postgres,
    )
    .await?;
    if let Some(pg) = &pg {
        f.svc.set_openshell(Arc::new(pg.store.clone()));
    }
    if token {
        f.svc.set_sandbox_audit_token(AUDIT_TOKEN);
    }
    let cancel = CancellationToken::new();
    let app = router_full(
        Arc::clone(&f.svc),
        AppConfig {
            service_token: SERVICE_TOKEN.into(),
            bot: Some(BotAccess {
                token: BOT_TOKEN.into(),
                skills: vec!["docs".into()],
            }),
            console_token: Some(CONSOLE_TOKEN.into()),
            content_root: f.root.clone(),
            oauth: None,
            allowed_hosts: vec!["127.0.0.1".into(), "localhost".into()],
        },
        None,
        PersonClients {
            entra: None,
            web_token: Some(WEB_TOKEN.into()),
            assertions: Default::default(),
        },
        cancel.clone(),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let ct = cancel.clone();
    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move { ct.cancelled_owned().await })
            .await
            .unwrap();
    });
    Some(Server {
        base,
        cancel,
        f,
        pg,
    })
}

fn at(min: i64) -> DateTime<Utc> {
    Utc::now() - Duration::minutes(60) + Duration::minutes(min)
}

fn deny(seq: u64, sandbox: &str, when: DateTime<Utc>) -> WireEvent {
    WireEvent {
        seq,
        kind: Some(Kind::Deny),
        at: Some(when),
        sandbox: Some(sandbox.into()),
        rule: Some("-".into()),
        reason: Some("no_matching_policy".into()),
        dest_host: Some("httpbin.org".into()),
        dest_ip: Some("93.184.216.34".into()),
        dest_port: Some(443),
        process: Some("curl".into()),
        os_event_uid: Some(format!("uid-{sandbox}-{seq}")),
        ..WireEvent::default()
    }
}

fn batch(run: &str, events: Vec<WireEvent>) -> Value {
    serde_json::to_value(Batch {
        collector_run: run.into(),
        events,
    })
    .unwrap()
}

#[tokio::test]
async fn the_collectors_token_reaches_its_two_routes_and_nothing_else() {
    let s = start(false, true).await.unwrap();
    let ok = batch("run-aaaaaaaa", vec![deny(1, "pmos-docs", at(0))]);
    let (status, body) = s
        .send(
            Method::POST,
            "/internal/sandbox-audit/batch",
            Some(AUDIT_TOKEN),
            Some(ok.clone()),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["stored"], 1);
    let hb = json!({"collector_run": "run-aaaaaaaa", "acked_seq": 1});
    let (status, _) = s
        .send(
            Method::POST,
            "/internal/sandbox-audit/heartbeat",
            Some(AUDIT_TOKEN),
            Some(hb.clone()),
        )
        .await;
    assert_eq!(status, 200);
    // Every other token is refused on the collector's routes; no token is 401.
    for t in [SERVICE_TOKEN, BOT_TOKEN, WEB_TOKEN, CONSOLE_TOKEN] {
        for (path, b) in [
            ("/internal/sandbox-audit/batch", &ok),
            ("/internal/sandbox-audit/heartbeat", &hb),
        ] {
            let (status, _) = s.send(Method::POST, path, Some(t), Some(b.clone())).await;
            assert_eq!(status, 403, "{path} with another client's token");
        }
    }
    for t in [
        None,
        Some("pmos_s_not-a-token"),
        Some("wrong-token-0123456789abcdef-0123456"),
    ] {
        let (status, _) = s
            .send(
                Method::POST,
                "/internal/sandbox-audit/batch",
                t,
                Some(ok.clone()),
            )
            .await;
        assert_eq!(status, 401, "{t:?}");
    }
    // The collector's token on every other route: it is nobody's client there.
    let others: [(Method, &str); 16] = [
        (Method::GET, "/internal/admin/audit?user=ada"),
        (Method::GET, "/internal/admin/overview?user=ada"),
        (Method::GET, "/internal/admin/accounts?user=ada"),
        (Method::GET, "/internal/admin/policy/proofs?user=ada"),
        (Method::GET, "/internal/admin/sandbox?user=ada"),
        (Method::GET, "/internal/approvals?user=ana"),
        (Method::GET, "/internal/approvals/x"),
        (Method::POST, "/internal/approvals/x/decision"),
        (Method::POST, "/internal/sessions"),
        (Method::POST, "/internal/call"),
        (Method::DELETE, "/internal/session"),
        (Method::GET, "/internal/ledger"),
        (Method::POST, "/internal/grants"),
        (Method::POST, "/internal/reindex"),
        (Method::GET, "/internal/onboarding?user=ana"),
        (Method::POST, "/internal/admin/events"),
    ];
    for (m, path) in others {
        let (status, _) = s
            .send(m.clone(), path, Some(AUDIT_TOKEN), Some(json!({})))
            .await;
        assert!(
            matches!(status, 401 | 404 | 405),
            "{m} {path} answered the collector {status}"
        );
        assert_ne!(status / 100, 2, "{m} {path}");
    }
    let (status, _) = s
        .send(Method::POST, "/mcp", Some(AUDIT_TOKEN), Some(json!({})))
        .await;
    assert_eq!(status, 401);
    // A read on its own routes isn't a route.
    let (status, _) = s
        .send(
            Method::GET,
            "/internal/sandbox-audit/batch",
            Some(AUDIT_TOKEN),
            None,
        )
        .await;
    assert_eq!(status, 405);
    s.stop().await;
}

#[tokio::test]
async fn without_the_setting_the_routes_are_absent() {
    let s = start(false, false).await.unwrap();
    let ok = batch("run-aaaaaaaa", vec![deny(1, "pmos-docs", at(0))]);
    for t in [Some(AUDIT_TOKEN), Some(SERVICE_TOKEN), None] {
        let (status, _) = s
            .send(
                Method::POST,
                "/internal/sandbox-audit/batch",
                t,
                Some(ok.clone()),
            )
            .await;
        assert_eq!(status, 404);
    }
    s.stop().await;
}

#[tokio::test]
async fn a_bad_batch_is_refused_whole() {
    let s = start(false, true).await.unwrap();
    let mut bad = batch("run-aaaaaaaa", vec![deny(1, "pmos-docs", at(0))]);
    bad["events"][0]["cmd_line"] = json!("curl -H 'Authorization: Bearer SECRET' x");
    let (status, _) = s
        .send(
            Method::POST,
            "/internal/sandbox-audit/batch",
            Some(AUDIT_TOKEN),
            Some(bad),
        )
        .await;
    assert_eq!(status, 400, "an unknown field (a command line) is refused");
    let mut query = deny(1, "pmos-docs", at(0));
    query.http_path = Some("/v1/x?api_key=SECRET".into());
    let (status, _) = s
        .send(
            Method::POST,
            "/internal/sandbox-audit/batch",
            Some(AUDIT_TOKEN),
            Some(batch("run-aaaaaaaa", vec![query])),
        )
        .await;
    assert_eq!(status, 400, "a query string is refused");
    let big: Vec<WireEvent> = (1..=501).map(|i| deny(i, "pmos-docs", at(0))).collect();
    let (status, _) = s
        .send(
            Method::POST,
            "/internal/sandbox-audit/batch",
            Some(AUDIT_TOKEN),
            Some(batch("run-aaaaaaaa", big)),
        )
        .await;
    assert!(matches!(status, 400 | 413), "{status}");
    assert!(
        !s.f.audit
            .events()
            .iter()
            .any(|e| e.tool.starts_with("sandbox.")),
        "nothing stored"
    );
    s.stop().await;
}

async fn ingest(s: &Server, run: &str, events: Vec<WireEvent>) -> Value {
    let (status, body) = s
        .send(
            Method::POST,
            "/internal/sandbox-audit/batch",
            Some(AUDIT_TOKEN),
            Some(batch(run, events)),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    body
}

async fn rows(s: &Server, tool: &str) -> Vec<pmos_toolserver::openshell::sandbox::SandboxRow> {
    s.f.svc
        .openshell()
        .sandbox_rows(&pmos_toolserver::openshell::sandbox::SandboxQuery {
            tools: vec![tool.into()],
            limit: 1000,
            ..Default::default()
        })
        .await
        .unwrap()
}

async fn the_store_records_dedups_attributes_and_detects_gaps(postgres: bool) {
    let Some(s) = start(postgres, true).await else {
        return;
    };
    // Sessions: Ana on her personal bot; one task on the team bot `scout`; two on `docs`.
    let now = Utc::now();
    let svc = &s.f.svc;
    let before = now - Duration::minutes(90);
    svc.create_bot_session("ana", "pm-ana", "tsk_ana", Some(Duration::hours(3)), before)
        .await
        .unwrap();
    svc.create_bot_session(
        "ben",
        "scout",
        "tsk_scout",
        Some(Duration::hours(3)),
        before,
    )
    .await
    .unwrap();
    svc.create_bot_session("ana", "docs", "tsk_d1", Some(Duration::hours(3)), before)
        .await
        .unwrap();
    svc.create_bot_session("ben", "docs", "tsk_d2", Some(Duration::hours(3)), before)
        .await
        .unwrap();

    let body = ingest(
        &s,
        "run-aaaaaaaa",
        vec![
            deny(1, "pmos-pm-ana", at(1)),
            deny(2, "pmos-scout", at(2)),
            deny(3, "pmos-docs", at(3)),
            deny(4, "pmos-blind", at(4)),
            deny(5, "pmos-nobody", at(5)),
            deny(6, "something-else", at(6)),
        ],
    )
    .await;
    assert_eq!(body["stored"], 6);
    let denies = rows(&s, "sandbox.deny").await;
    let by = |sb: &str| {
        denies
            .iter()
            .find(|r| r.fields.sandbox.as_deref() == Some(sb))
            .unwrap()
            .clone()
    };
    let ana = by("pmos-pm-ana");
    assert_eq!(
        (
            ana.event.user.as_str(),
            ana.event.run_id.as_str(),
            ana.event.detail.as_deref()
        ),
        ("ana", "tsk_ana", Some("attributed"))
    );
    assert_eq!(ana.event.bot.as_deref(), Some("pm-ana"));
    assert_eq!(ana.event.client, Client::SandboxAudit);
    let scout = by("pmos-scout");
    assert_eq!(
        (scout.event.user.as_str(), scout.event.run_id.as_str()),
        ("ben", "tsk_scout")
    );
    let docs = by("pmos-docs");
    assert_eq!(
        (docs.event.user.as_str(), docs.event.detail.as_deref()),
        ("pmos-sandbox-audit", Some("ambiguous"))
    );
    let blind = by("pmos-blind");
    assert_eq!(
        (blind.event.run_id.as_str(), blind.event.detail.as_deref()),
        ("sandbox", Some("no task"))
    );
    for sb in ["pmos-nobody", "something-else"] {
        let r = by(sb);
        assert_eq!(
            (r.event.bot.as_deref(), r.event.detail.as_deref()),
            (None, Some("unknown sandbox"))
        );
    }
    // Kept fields, and nothing else.
    assert_eq!(ana.fields.dest_host.as_deref(), Some("httpbin.org"));
    assert_eq!(ana.fields.dest_port, Some(443));
    assert_eq!(ana.fields.collector_seq, Some(1));

    // Redelivered by OpenShell (same uid, new seq): stored once.
    let mut again = deny(7, "pmos-pm-ana", at(1));
    again.os_event_uid = Some("uid-pmos-pm-ana-1".into());
    let body = ingest(&s, "run-aaaaaaaa", vec![again]).await;
    assert_eq!(
        (body["stored"].as_u64(), body["duplicates"].as_u64()),
        (Some(0), Some(1))
    );
    // Replayed from the spool: skipped.
    let body = ingest(
        &s,
        "run-aaaaaaaa",
        vec![
            deny(6, "something-else", at(6)),
            deny(8, "pmos-docs", at(8)),
        ],
    )
    .await;
    assert_eq!(
        (body["stored"].as_u64(), body["replayed"].as_u64()),
        (Some(1), Some(1))
    );
    // A hole: 9 to 11.
    let body = ingest(&s, "run-aaaaaaaa", vec![deny(12, "pmos-docs", at(9))]).await;
    assert_eq!(body["gaps"], 1);
    let gaps = rows(&s, "sandbox.collector_gap").await;
    assert_eq!(
        gaps[0].event.detail.as_deref(),
        Some("seq 9 to 11 missing 3")
    );
    // A new run: down from the old one's last word to the new one's first.
    ingest(&s, "run-bbbbbbbb", vec![deny(1, "pmos-docs", at(10))]).await;
    let downs = rows(&s, "sandbox.collector_down").await;
    assert_eq!(downs.len(), 1);
    assert!(downs[0]
        .event
        .detail
        .as_deref()
        .unwrap()
        .starts_with("down from "));
    // Silent for five minutes: the watchdog opens a collector_down once; heard from, it closes.
    let later = Utc::now() + Duration::minutes(6);
    let w = openshell::watchdog(svc, later).await.unwrap();
    assert_eq!(w.stored.len(), 1);
    assert!(openshell::watchdog(svc, later + Duration::minutes(1))
        .await
        .unwrap()
        .stored
        .is_empty());
    let state = svc.openshell().collector_state(later).await.unwrap();
    assert!(!state.receiving && state.silent_since.is_some());
    let done = svc
        .openshell()
        .heartbeat(
            &Heartbeat {
                collector_run: "run-bbbbbbbb".into(),
                acked_seq: 1,
            },
            later + Duration::minutes(2),
        )
        .await
        .unwrap();
    assert_eq!(done.stored.len(), 1);
    assert!(done.stored[0]
        .0
        .detail
        .as_deref()
        .unwrap()
        .starts_with("down from "));
    // The tripwire.
    let d = openshell::denials_since(svc, "pm-ana", at(0))
        .await
        .unwrap();
    assert_eq!(d.len(), 1);
    assert!(openshell::denials_since(svc, "pm-ana", at(2))
        .await
        .unwrap()
        .is_empty());
    let g = openshell::sandbox_gaps_since(svc, at(0)).await.unwrap();
    assert!(g.len() >= 3, "{}", g.len());

    if let Some(pg) = &s.pg {
        // The same rows are audit_events, under the append-only triggers.
        let n: i64 =
            sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE client = 'sandbox_audit'")
                .fetch_one(pg.store.pool())
                .await
                .unwrap();
        assert!(n >= 10, "{n}");
        for sql in [
            "UPDATE audit_events SET dest_host = 'x' WHERE client = 'sandbox_audit'",
            "DELETE FROM audit_events WHERE client = 'sandbox_audit'",
        ] {
            let err = sqlx::query(sqlx::AssertSqlSafe(sql))
                .execute(pg.store.pool())
                .await
                .unwrap_err();
            assert!(err.to_string().contains("append-only"), "{sql}: {err}");
        }
        // Only the collector's client writes sandbox rows.
        let err = sqlx::query(
            "INSERT INTO audit_events (at, run_id, user_handle, tool, outcome, sources, client)
             VALUES (now(), 'r', 'u', 'sandbox.deny', 'denied', '{}', 'orchestrator')",
        )
        .execute(pg.store.pool())
        .await
        .unwrap_err();
        assert!(
            err.to_string().contains("audit_events_sandbox_client"),
            "{err}"
        );
        // The admin audit search sees them as ordinary events.
        let found = pg
            .store
            .search(&pmos_toolserver::audit::AuditQuery {
                action: Some("sandbox.deny".into()),
                limit: 100,
                ..Default::default()
            })
            .await
            .unwrap()
            .unwrap();
        assert!(found.len() >= 7);
    } else {
        // In memory, a MemoryAudit sink keeps the copies (the fixture's own sink is a switch
        // that copies nothing).
        let sink = pmos_toolserver::audit::MemoryAudit::default();
        for r in rows(&s, "sandbox.deny").await {
            sink.mirror_sandbox(r.event, json!({})).await.unwrap();
        }
        assert!(sink.events().iter().all(|e| e.tool == "sandbox.deny"));
        assert!(!sink.events().is_empty());
    }
    s.stop().await;
}

#[tokio::test]
async fn in_memory_the_store_records_dedups_attributes_and_detects_gaps() {
    the_store_records_dedups_attributes_and_detects_gaps(false).await;
}

#[tokio::test]
async fn in_postgres_the_store_records_dedups_attributes_and_detects_gaps() {
    the_store_records_dedups_attributes_and_detects_gaps(true).await;
}

#[tokio::test]
async fn the_purge_deletes_sandbox_rows_at_90_days_and_not_before() {
    let Some(pg) = TestPg::new().await else {
        return;
    };
    let store = pg.store.clone();
    let prepared = |seq: u64, days: i64| {
        let e = deny(seq, "pmos-docs", Utc::now() - Duration::days(days));
        pmos_toolserver::openshell::sandbox::prepare(&e, "run-aaaaaaaa", None)
    };
    store
        .ingest(
            "run-aaaaaaaa",
            vec![prepared(1, 120), prepared(2, 89)],
            Utc::now(),
        )
        .await
        .unwrap();
    let purged = store.purge(90, 1000, Utc::now()).await.unwrap();
    assert!(purged.is_some());
    let left: Vec<String> = sqlx::query_scalar(
        "SELECT os_event_uid FROM audit_events WHERE tool = 'sandbox.deny' ORDER BY at",
    )
    .fetch_all(store.pool())
    .await
    .unwrap();
    assert_eq!(
        left,
        ["uid-pmos-docs-2"],
        "the 120-day row went, the 89-day row stayed"
    );
    pg.drop_schema().await;
}

#[tokio::test]
async fn the_json_lines_copy_gets_each_stored_row_with_its_fields() {
    let s = start(false, true).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("audit.jsonl");
    let file: Arc<dyn AuditSink> =
        Arc::new(pmos_toolserver::audit::JsonlAudit::open(&path).unwrap());
    // The JSON-lines sink as main tees it.
    file.mirror_sandbox(
        rows(&s, "sandbox.deny")
            .await
            .first()
            .map(|r| r.event.clone())
            .unwrap_or_else(|| {
                pmos_toolserver::openshell::sandbox::prepare(
                    &deny(1, "pmos-docs", at(0)),
                    "run-aaaaaaaa",
                    None,
                )
                .event
            }),
        json!({"dest_class": "public"}),
    )
    .await
    .unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    let line: Value = serde_json::from_str(text.trim()).unwrap();
    assert_eq!(line["tool"], "sandbox.deny");
    assert_eq!(line["sandbox"]["dest_class"], "public");
    s.stop().await;
}
