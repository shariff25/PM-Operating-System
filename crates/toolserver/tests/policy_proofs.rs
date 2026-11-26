//! Host proofs (`docs/openshell-prover-and-sandbox-audit-spec.md` §A.5, slice A3; PRV-9):
//! `pmos-toolserver policy-proof record` needs PMOS_HOST_USER and records one row per bot with an
//! audit event; `policy_proofs` is append-only; the newest row per bot is served on
//! `GET /internal/admin/policy/proofs` to admins and auditors only.

mod common;

use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::Arc;

use chrono::{Duration, Utc};
use common::TestPg;
use pmos_contract::model::Role;
use pmos_policy::prove::Status;
use pmos_toolserver::audit::{AuditQuery, AuditSink, Client, Outcome};
use pmos_toolserver::openshell::proofs::{rows_from_report, ProofRow};
use pmos_toolserver::openshell::OpenShellStore;
use pmos_toolserver::users::HostUser;
use pmos_toolserver::{router_full, AppConfig, PersonClients};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

const BIN: &str = env!("CARGO_BIN_EXE_pmos-toolserver");
const SERVICE_TOKEN: &str = "test-service-token-0123456789abcdef";

fn report() -> Value {
    json!({
        "prover": {"version": "0.1.9", "sha256": "a".repeat(64), "pinned_version": "0.1.9"},
        "boundary_sha256": "b".repeat(64),
        "results": [
            {"bot": "docs", "status": "proved", "candidate_sha256": "c".repeat(64),
             "coverage": ["filesystem", "network_l4", "network_rest", "process", "landlock"]},
            {"bot": "pm-ana", "status": "exceeds", "counterexample": "filesystem write /sandbox",
             "candidate_sha256": "d".repeat(64), "coverage": []},
        ],
    })
}

fn record(input: &str, env: &[(&str, &str)]) -> (i32, String, String) {
    let mut cmd = Command::new(BIN);
    cmd.args(["policy-proof", "record"])
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(p) = std::env::var_os("PATH") {
        cmd.env("PATH", p);
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
    // The command may refuse and exit before it reads its input; the write then fails with a
    // broken pipe, which is not what any of these tests is about. Every other error still fails.
    match child.stdin.take().unwrap().write_all(input.as_bytes()) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
        Err(e) => panic!("writing the command's input: {e}"),
    }
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn the_host_command_needs_a_host_user_and_a_database() {
    let unreachable = ("PMOS_DATABASE_URL", "postgresql://nobody@127.0.0.1:1/none");
    let (code, _, err) = record(&report().to_string(), &[unreachable]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("PMOS_HOST_USER is required"), "{err}");
    for bad in ["Root User", "root;rm", "ROOT"] {
        let (code, _, _) = record(
            &report().to_string(),
            &[unreachable, ("PMOS_HOST_USER", bad)],
        );
        assert_eq!(code, 2, "{bad}");
    }
    let (code, _, err) = record(&report().to_string(), &[("PMOS_HOST_USER", "raj")]);
    assert_eq!(code, 2, "{err}");
    // A report that isn't one is refused before the database is tried.
    let (code, _, err) = record("not json", &[unreachable, ("PMOS_HOST_USER", "raj")]);
    assert_eq!(code, 1, "{err}");
    let (code, _, err) = record(
        &json!({"prover": {}, "results": [{"bot": "docs", "status": "passed"}]}).to_string(),
        &[unreachable, ("PMOS_HOST_USER", "raj")],
    );
    assert_eq!(code, 1);
    assert!(err.contains("refused"), "{err}");
}

#[tokio::test]
async fn the_host_command_records_one_row_per_bot_with_its_audit_event() {
    let Some(pg) = TestPg::new().await else {
        return;
    };
    let (code, out, err) = record(
        &report().to_string(),
        &[
            ("PMOS_HOST_USER", "raj"),
            ("PMOS_DATABASE_URL", &pg.schema_url()),
        ],
    );
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("recorded docs: proved") && out.contains("recorded pm-ana: exceeds"));
    let latest = pg.store.latest_proofs().await.unwrap();
    assert_eq!(latest.len(), 2);
    assert!(latest.iter().all(|r| r.host_user == "raj"));
    let q = AuditQuery {
        action: Some("policy.prove".into()),
        limit: 10,
        ..Default::default()
    };
    let events = pg.store.search(&q).await.unwrap().unwrap();
    assert_eq!(events.len(), 2);
    for e in &events {
        assert_eq!(e.event.client, Client::Host);
        assert_eq!(e.event.detail.as_deref(), Some("host_user raj"));
    }
    let by_bot = |b: &str| {
        events
            .iter()
            .find(|e| e.event.bot.as_deref() == Some(b))
            .unwrap()
    };
    assert_eq!(by_bot("docs").event.outcome, Outcome::Ok);
    assert_eq!(by_bot("pm-ana").event.outcome, Outcome::Denied);
    pg.drop_schema().await;
}

fn row(bot: &str, status: Status, at: chrono::DateTime<Utc>) -> ProofRow {
    let mut r = rows_from_report(
        &json!({"prover": {"version": "0.1.9"}, "results": [{"bot": bot, "status": "error"}]}),
        &HostUser::parse("raj").unwrap(),
        at,
    )
    .unwrap()
    .remove(0);
    r.status = status;
    if status == Status::Proved {
        r.coverage = pmos_policy::prove::DOMAINS
            .iter()
            .map(|s| s.to_string())
            .collect();
        r.coverage.sort();
    }
    r
}

#[tokio::test]
async fn proofs_are_append_only_and_the_newest_per_bot_is_served() {
    let Some(pg) = TestPg::new().await else {
        return;
    };
    let t = Utc::now();
    pg.store
        .record_proofs(&[
            row("docs", Status::Proved, t - Duration::hours(2)),
            row("docs", Status::Exceeds, t - Duration::hours(1)),
            row("pm-ana", Status::NotAvailable, t),
        ])
        .await
        .unwrap();
    let latest = pg.store.latest_proofs().await.unwrap();
    assert_eq!(latest.len(), 2);
    assert_eq!(latest[0].bot, "docs");
    assert_eq!(
        latest[0].status,
        Status::Exceeds,
        "the newest, not the passing one"
    );
    for sql in [
        "UPDATE policy_proofs SET status = 'proved'",
        "DELETE FROM policy_proofs",
        "TRUNCATE policy_proofs",
    ] {
        let err = sqlx::query(sqlx::AssertSqlSafe(sql))
            .execute(pg.store.pool())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("append-only"), "{sql}: {err}");
    }
    // A `proved` row without the five domains can't be stored, whatever wrote it.
    let err = sqlx::query(
        "INSERT INTO policy_proofs (at, bot, status, coverage, host_user)
         VALUES (now(), 'docs', 'proved', ARRAY['filesystem'], 'raj')",
    )
    .execute(pg.store.pool())
    .await
    .unwrap_err();
    assert!(err.to_string().contains("check"), "{err}");
    // A failing row takes the whole call with it.
    let mut bad = row("zz", Status::Error, t);
    bad.host_user = "Not Valid".into();
    assert!(pg
        .store
        .record_proofs(&[row("pm-raj", Status::Proved, t), bad])
        .await
        .is_err());
    assert!(pg
        .store
        .latest_proofs()
        .await
        .unwrap()
        .iter()
        .all(|r| r.bot != "pm-raj"));
    pg.drop_schema().await;
}

#[tokio::test]
async fn only_admins_and_auditors_read_the_proofs() {
    let (f, _) = common::roles_fixture(
        &[
            ("ada", Role::Admin),
            ("aud", Role::Auditor),
            ("ana", Role::Pm),
        ],
        false,
    )
    .await
    .unwrap();
    f.svc
        .openshell()
        .record_proofs(&[row("docs", Status::Proved, Utc::now())])
        .await
        .unwrap();
    let cancel = CancellationToken::new();
    let app = router_full(
        Arc::clone(&f.svc),
        AppConfig {
            service_token: SERVICE_TOKEN.into(),
            bot: None,
            console_token: None,
            content_root: f.root.clone(),
            oauth: None,
            allowed_hosts: vec!["127.0.0.1".into()],
        },
        None,
        PersonClients::default(),
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
    let get = |who: &str| {
        let url = format!("{base}/internal/admin/policy/proofs?user={who}");
        async move {
            let r = reqwest::Client::new()
                .get(url)
                .bearer_auth(SERVICE_TOKEN)
                .send()
                .await
                .unwrap();
            let status = r.status().as_u16();
            (status, r.json::<Value>().await.unwrap_or(Value::Null))
        }
    };
    for who in ["ada", "aud"] {
        let (status, body) = get(who).await;
        assert_eq!(status, 200, "{who}: {body}");
        assert_eq!(body["proofs"][0]["bot"], "docs");
        assert_eq!(body["proofs"][0]["status"], "proved");
        assert_eq!(body["not_proved"].as_array().unwrap().len(), 6);
    }
    let (status, body) = get("ana").await;
    assert_eq!(status, 403, "{body}");
    let refused = f
        .audit
        .events()
        .into_iter()
        .filter(|e| e.tool == "admin.policy_proofs" && e.outcome == Outcome::Denied)
        .count();
    assert_eq!(refused, 1);
    // No token, or the wrong one.
    let r = reqwest::Client::new()
        .get(format!("{base}/internal/admin/policy/proofs?user=ada"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 401);
    cancel.cancel();
}
