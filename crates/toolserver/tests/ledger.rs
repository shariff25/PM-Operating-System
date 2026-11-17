//! The feedback ledger and the bounded retry for invalid drafts
//! (`docs/improvement-loop-spec.md` §1–2), through `ToolService` and over HTTP.
//!
//! The store contract (every outcome on both backends, filters, concurrency) is in
//! `tests/stores.rs`; these cases check what the service adds: the content commit and model
//! alias on each row, reasons on rejection, the summary, who may read the ledger, and the
//! retry cap.

mod common;

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use chrono::Duration;
use common::{fixture, now};
use pmos_toolserver::app::BotAccess;
use pmos_toolserver::approvals::{Decision, Status, Verdict};
use pmos_toolserver::audit::{Client, Outcome};
use pmos_toolserver::ledger::{LedgerOutcome, LedgerQuery, LedgerRow, Reason};
use pmos_toolserver::service::{DecisionError, ToolError, LAST_ATTEMPT};
use pmos_toolserver::session::Session;
use pmos_toolserver::{router, AppConfig};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

const SERVICE_TOKEN: &str = "test-service-token-0123456789abcdef";
const BOT_TOKEN: &str = "test-bot-token-0123456789abcdef-0123";

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

async fn skill_session(f: &common::Fixture, run: &str) -> Session {
    f.svc
        .create_session("ana", "meeting-notes", run, None, now())
        .await
        .unwrap()
}

fn issue(summary: &str) -> Value {
    json!({"project": "PM", "issue_type": "Story", "summary": summary})
}

/// Draft one Jira issue in `session`; returns the approval id.
async fn draft(f: &common::Fixture, session: &Session, summary: &str) -> String {
    let res = f
        .svc
        .call(session, "jira_create_issue", issue(summary), now())
        .await
        .unwrap();
    res["approval_id"].as_str().unwrap().to_string()
}

async fn rows(f: &common::Fixture) -> Vec<LedgerRow> {
    f.svc.ledger(LedgerQuery::all(now()), now()).await.unwrap()
}

fn row(rows: &[LedgerRow], id: &str) -> LedgerRow {
    rows.iter().find(|r| r.id == id).cloned().unwrap()
}

// ----- rows -------------------------------------------------------------------------------

#[tokio::test]
async fn every_draft_gets_a_row_with_its_model_and_content_commit() {
    let f = fixture();
    // The fixture's content isn't a git checkout: no commit to record.
    assert_eq!(f.svc.content_sha(), None);
    let s = skill_session(&f, "r1").await;
    let before = draft(&f, &s, "SCIM").await;
    let r = row(&rows(&f).await, &before);
    assert_eq!(r.content_sha, None);
    // meeting-notes drafts with the extraction model.
    assert_eq!(r.model_alias.as_deref(), Some("pmos/extraction"));
    assert_eq!(r.skill.as_deref(), Some("meeting-notes"));
    assert_eq!(r.tool, "jira.create_issue");
    assert_eq!(r.outcome, LedgerOutcome::Pending);

    // Once the content is a checkout, reindexing reads its HEAD.
    git(&f.root, &["init", "-q"]);
    git(&f.root, &["add", "-A"]);
    git(&f.root, &["commit", "-q", "-m", "content"]);
    let head = git(&f.root, &["rev-parse", "HEAD"]);
    f.svc.reindex(&f.root, now()).await.unwrap();
    assert_eq!(f.svc.content_sha().as_deref(), Some(head.as_str()));

    // A bot session's row names the bot, and its role's model.
    let bot = f
        .svc
        .create_bot_session("ana", "scout", "tsk_1", None, now())
        .await
        .unwrap();
    let after = draft(&f, &bot, "SSO").await;
    let all = rows(&f).await;
    let r = row(&all, &after);
    assert_eq!(r.content_sha.as_deref(), Some(head.as_str()));
    assert_eq!(r.model_alias.as_deref(), Some("pmos/default"));
    assert_eq!(r.bot.as_deref(), Some("scout"));
    assert_eq!(r.skill, None);
    assert_eq!(r.run_id, "tsk_1");
    assert_eq!(r.user, "ana");
    // The row made before keeps what it was drafted from.
    assert_eq!(row(&all, &before).content_sha, None);

    // A write approved in the harness has no approval item, so no row.
    f.svc
        .grant(&s, "jira_create_issue", &issue("Granted"), now())
        .await
        .unwrap();
    f.svc
        .call(&s, "jira_create_issue", issue("Granted"), now())
        .await
        .unwrap();
    assert_eq!(rows(&f).await.len(), 2);
}

#[tokio::test]
async fn decisions_through_the_service_label_their_rows() {
    let f = fixture();
    let s = skill_session(&f, "r1").await;
    let approved = draft(&f, &s, "SCIM").await;
    let edited = draft(&f, &s, "SCIM user provisioning").await;
    let rejected = draft(&f, &s, "Wrong").await;
    let failed = draft(&f, &s, "Doomed").await;
    let waiting = draft(&f, &s, "Later").await;

    f.svc
        .decide(&approved, "ana", Decision::Approve, None, now())
        .await
        .unwrap();
    f.svc
        .decide(
            &edited,
            "ana",
            Decision::Edit,
            Some(issue("SCIM user deprovisioning")),
            now(),
        )
        .await
        .unwrap();
    f.svc
        .decide_verdict(
            Client::Bot,
            &rejected,
            "ana",
            Verdict {
                decision: Decision::Reject,
                args: None,
                reason: Some(Reason::WrongTarget),
                confirm_external: false,
                expected_args_hash: None,
            },
            now(),
        )
        .await
        .unwrap();
    // Approved, then the process died mid-write: the reconciler marks it failed.
    f.stores
        .approvals
        .decide(&failed, "ana", Decision::Approve, None, now())
        .await
        .unwrap();
    f.svc.reconcile(Duration::zero(), now()).await.unwrap();

    let all = rows(&f).await;
    assert_eq!(row(&all, &approved).outcome, LedgerOutcome::Approved);
    assert_eq!(row(&all, &approved).edit_distance, Some(0.0));
    let e = row(&all, &edited);
    assert_eq!(e.outcome, LedgerOutcome::Edited);
    let d = e.edit_distance.unwrap();
    assert!(d > 0.0 && d < 0.2, "{d}");
    let r = row(&all, &rejected);
    assert_eq!(r.outcome, LedgerOutcome::Rejected);
    assert_eq!(r.reason, Some(Reason::WrongTarget));
    assert_eq!(r.edit_distance, None);
    assert_eq!(row(&all, &failed).outcome, LedgerOutcome::Failed);
    assert_eq!(row(&all, &waiting).outcome, LedgerOutcome::Pending);

    // A week on, the undecided one is reported as expired.
    let later = now() + Duration::days(8);
    let expired = f
        .svc
        .ledger(
            LedgerQuery {
                outcomes: vec![LedgerOutcome::Expired],
                ..LedgerQuery::all(later)
            },
            later,
        )
        .await
        .unwrap();
    assert_eq!(
        expired.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
        vec![waiting.as_str()]
    );

    // The rejection's audit event records the reason.
    let reject_event = f
        .audit
        .events()
        .into_iter()
        .find(|e| e.outcome == Outcome::Rejected)
        .unwrap();
    assert_eq!(reject_event.detail.as_deref(), Some("reason: wrong_target"));
    assert_eq!(reject_event.client, Client::Bot);
}

#[tokio::test]
async fn a_reason_is_only_accepted_with_reject() {
    let f = fixture();
    let s = skill_session(&f, "r1").await;
    let id = draft(&f, &s, "SCIM").await;
    for (decision, args) in [
        (Decision::Approve, None),
        (Decision::Edit, Some(issue("Edited"))),
    ] {
        let err = f
            .svc
            .decide_verdict(
                Client::Orchestrator,
                &id,
                "ana",
                Verdict {
                    decision,
                    args,
                    reason: Some(Reason::Tone),
                    confirm_external: false,
                    expected_args_hash: None,
                },
                now(),
            )
            .await
            .unwrap_err();
        assert_eq!(err, DecisionError::ReasonNotAllowed);
    }
    let item = f.svc.approval(&id).await.unwrap().unwrap();
    assert_eq!(item.status, Status::Pending, "nothing changed");
    assert!(f.jira.writes().is_empty());
    assert_eq!(row(&rows(&f).await, &id).decided_at, None);
}

#[tokio::test]
async fn the_summary_counts_outcomes_coverage_edits_and_reasons() {
    let f = fixture();
    let s = skill_session(&f, "r1").await;
    let scout = f
        .svc
        .create_bot_session("ana", "scout", "tsk_9", None, now())
        .await
        .unwrap();
    let a = draft(&f, &s, "One").await;
    let b = draft(&f, &s, "Two").await;
    let c = draft(&f, &s, "Three").await;
    draft(&f, &s, "Four").await;
    let bot_item = draft(&f, &scout, "Five").await;
    f.svc
        .decide(&a, "ana", Decision::Approve, None, now())
        .await
        .unwrap();
    f.svc
        .decide(&b, "ana", Decision::Edit, Some(issue("Twelve")), now())
        .await
        .unwrap();
    f.svc
        .decide_verdict(
            Client::Orchestrator,
            &c,
            "ana",
            Verdict {
                decision: Decision::Reject,
                args: None,
                reason: Some(Reason::Incomplete),
                confirm_external: false,
                expected_args_hash: None,
            },
            now(),
        )
        .await
        .unwrap();
    f.svc
        .decide(&bot_item, "ana", Decision::Reject, None, now())
        .await
        .unwrap();

    let summary = f.svc.ledger_summary(None, None, now()).await.unwrap();
    assert_eq!(summary.rows, 5);
    assert!((summary.label_coverage - 0.8).abs() < 1e-9);
    assert_eq!(summary.groups.len(), 2);
    let skill = summary
        .groups
        .iter()
        .find(|g| g.skill.as_deref() == Some("meeting-notes"))
        .unwrap();
    assert_eq!(skill.tool, "jira.create_issue");
    assert_eq!(skill.rows, 4);
    assert_eq!(skill.counts["approved"], 1);
    assert_eq!(skill.counts["edited"], 1);
    assert_eq!(skill.counts["rejected"], 1);
    assert_eq!(skill.counts["pending"], 1);
    assert!((skill.label_coverage - 0.75).abs() < 1e-9);
    let edit = row(&rows(&f).await, &b).edit_distance.unwrap();
    // Median of 0.0 (approved) and the edit.
    assert!((skill.median_edit_distance.unwrap() - edit / 2.0).abs() < 1e-9);
    assert_eq!(skill.reasons["incomplete"], 1);

    let only_scout = f
        .svc
        .ledger_summary(None, Some("scout".into()), now())
        .await
        .unwrap();
    assert_eq!(only_scout.groups.len(), 1);
    let g = &only_scout.groups[0];
    assert_eq!(g.bot.as_deref(), Some("scout"));
    assert_eq!(g.counts["rejected"], 1);
    assert_eq!(g.reasons["none"], 1);
    assert_eq!(g.label_coverage, 1.0);
    assert_eq!(g.median_edit_distance, None);

    // Nothing since tomorrow.
    let empty = f
        .svc
        .ledger_summary(Some(now() + Duration::days(1)), None, now())
        .await
        .unwrap();
    assert_eq!(empty.rows, 0);
    assert!(empty.groups.is_empty());
}

// ----- the bounded retry (§2) ---------------------------------------------------------------

/// Missing `summary`: refused by the schema check.
fn invalid() -> Value {
    json!({"project": "PM", "issue_type": "Story"})
}

#[tokio::test]
async fn a_write_refused_twice_for_invalid_arguments_is_refused_for_the_session() {
    let f = fixture();
    let s = skill_session(&f, "r1").await;

    let first = f
        .svc
        .call(&s, "jira_create_issue", invalid(), now())
        .await
        .unwrap_err();
    let ToolError::InvalidArgs(message) = &first else {
        panic!("{first:?}")
    };
    assert!(message.contains("summary"), "{message}");
    assert!(!message.contains(LAST_ATTEMPT));

    let second = f
        .svc
        .call(&s, "jira_create_issue", invalid(), now())
        .await
        .unwrap_err();
    let ToolError::InvalidArgs(message) = &second else {
        panic!("{second:?}")
    };
    assert!(message.contains("summary"), "{message}");
    assert!(message.ends_with(LAST_ATTEMPT), "{message}");
    assert!(second.to_string().ends_with(
        "This is the last attempt for this action. Tell the PM what was wrong instead of retrying."
    ));

    // Even a valid call is refused now, without a draft or a connector call; a grant
    // doesn't get past it either.
    f.svc
        .grant(&s, "jira_create_issue", &issue("Fixed"), now())
        .await
        .unwrap();
    let third = f
        .svc
        .call(&s, "jira_create_issue", issue("Fixed"), now())
        .await
        .unwrap_err();
    let ToolError::Forbidden(message) = &third else {
        panic!("{third:?}")
    };
    assert!(message.ends_with(LAST_ATTEMPT), "{message}");
    assert!(f.jira.writes().is_empty());
    assert!(rows(&f).await.is_empty(), "no approval item was made");

    let events: Vec<_> = f
        .audit
        .events()
        .into_iter()
        .filter(|e| e.tool == "jira.create_issue" && e.outcome != Outcome::Granted)
        .collect();
    assert_eq!(events.len(), 3);
    assert!(events.iter().all(|e| e.outcome == Outcome::Denied));
    assert_eq!(events[0].detail.as_deref(), Some("invalid draft (1 of 2)"));
    assert_eq!(events[1].detail.as_deref(), Some("invalid draft (2 of 2)"));
    assert!(events[2]
        .detail
        .as_deref()
        .unwrap()
        .starts_with("invalid draft (2 of 2)"));

    // Other tools, and the same tool in another session, are unaffected.
    assert!(f
        .svc
        .call(&s, "jira_search", json!({"jql": "project = PM"}), now())
        .await
        .is_ok());
    let other = skill_session(&f, "r2").await;
    let res = f
        .svc
        .call(&other, "jira_create_issue", issue("Fresh"), now())
        .await
        .unwrap();
    assert_eq!(res["status"], "pending_approval");
}

#[tokio::test]
async fn one_correction_is_allowed() {
    let f = fixture();
    let s = skill_session(&f, "r1").await;
    f.svc
        .call(&s, "jira_create_issue", invalid(), now())
        .await
        .unwrap_err();
    // Corrected: drafted as usual. A valid call doesn't reset the count.
    let res = f
        .svc
        .call(&s, "jira_create_issue", issue("SCIM"), now())
        .await
        .unwrap();
    assert_eq!(res["status"], "pending_approval");
    let again = f
        .svc
        .call(&s, "jira_create_issue", invalid(), now())
        .await
        .unwrap_err();
    assert!(again.to_string().ends_with(LAST_ATTEMPT), "{again}");
    // Reads refused for invalid arguments are not counted.
    for _ in 0..3 {
        let err = f
            .svc
            .call(&s, "jira_search", json!({}), now())
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)), "{err:?}");
        assert!(!err.to_string().contains(LAST_ATTEMPT));
    }
}

// ----- over HTTP ----------------------------------------------------------------------------

struct Server {
    base: String,
    cancel: CancellationToken,
    fixture: common::Fixture,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

async fn start() -> Server {
    let fixture = fixture();
    let cancel = CancellationToken::new();
    let app = router(
        Arc::clone(&fixture.svc),
        AppConfig {
            service_token: SERVICE_TOKEN.into(),
            bot: Some(BotAccess {
                token: BOT_TOKEN.into(),
                skills: vec!["docs".into()],
            }),
            console_token: None,
            content_root: fixture.root.clone(),
            oauth: None,
            allowed_hosts: vec!["127.0.0.1".into(), "localhost".into()],
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
    Server {
        base,
        cancel,
        fixture,
    }
}

impl Server {
    async fn get(&self, token: &str, path: &str) -> (u16, Value) {
        let res = reqwest::Client::new()
            .get(format!("{}{path}", self.base))
            .bearer_auth(token)
            .send()
            .await
            .unwrap();
        let status = res.status().as_u16();
        (status, res.json().await.unwrap_or(Value::Null))
    }

    async fn post(&self, token: &str, path: &str, body: Value) -> (u16, Value) {
        let res = reqwest::Client::new()
            .post(format!("{}{path}", self.base))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .unwrap();
        let status = res.status().as_u16();
        (status, res.json().await.unwrap_or(Value::Null))
    }
}

#[tokio::test]
async fn reasons_over_http() {
    let s = start().await;
    let session = skill_session(&s.fixture, "r1").await;
    let id = draft(&s.fixture, &session, "SCIM").await;
    let path = format!("/internal/approvals/{id}/decision");

    for body in [
        json!({"decided_by": "ana", "decision": "approve", "reason": "tone"}),
        json!({"decided_by": "ana", "decision": "edit", "args": issue("x"), "reason": "tone"}),
        json!({"decided_by": "ana", "decision": "reject", "reason": "rude"}),
    ] {
        let (status, res) = s.post(SERVICE_TOKEN, &path, body.clone()).await;
        assert_eq!(status, 422, "{body}: {res}");
        assert!(res["error"].as_str().unwrap().contains("reason"), "{res}");
    }
    assert!(s.fixture.jira.writes().is_empty());

    // The Slack bot rejects from an approval card, with a reason.
    let (status, item) = s
        .post(
            BOT_TOKEN,
            &path,
            json!({"decided_by": "ana", "decision": "reject", "reason": "should_not_draft"}),
        )
        .await;
    assert_eq!(status, 200, "{item}");
    assert_eq!(item["status"], "rejected");
    let (_, ledger) = s.get(SERVICE_TOKEN, "/internal/ledger").await;
    assert_eq!(ledger["rows"][0]["id"], id.as_str());
    let (_, ledger) = s.get(BOT_TOKEN, "/internal/ledger").await;
    assert!(ledger["rows"][0].get("id").is_none(), "no ids for the bot");
    assert_eq!(ledger["rows"][0]["run_id"], session.run_id.as_str());
    assert_eq!(ledger["rows"][0]["outcome"], "rejected");
    assert_eq!(ledger["rows"][0]["reason"], "should_not_draft");

    // Without a reason, as before.
    let other = draft(&s.fixture, &session, "Other").await;
    let (status, _) = s
        .post(
            SERVICE_TOKEN,
            &format!("/internal/approvals/{other}/decision"),
            json!({"decided_by": "ana", "decision": "reject"}),
        )
        .await;
    assert_eq!(status, 200);
    let (_, ledger) = s.get(SERVICE_TOKEN, "/internal/ledger").await;
    let row = ledger["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == other.as_str())
        .unwrap()
        .clone();
    assert_eq!(row["outcome"], "rejected");
    assert_eq!(row["reason"], Value::Null);
}

#[tokio::test]
async fn who_may_read_the_ledger() {
    let s = start().await;
    let session = skill_session(&s.fixture, "r1").await;
    draft(&s.fixture, &session, "SCIM").await;

    let (status, body) = s.get(SERVICE_TOKEN, "/internal/ledger").await;
    assert_eq!(status, 200);
    let row = &body["rows"][0];
    // Every field, and no draft text.
    for field in [
        "id",
        "run_id",
        "user",
        "bot",
        "skill",
        "tool",
        "content_sha",
        "model_alias",
        "outcome",
        "edit_distance",
        "reason",
        "created_at",
        "decided_at",
        "decided_by",
    ] {
        assert!(row.get(field).is_some(), "{field} missing from {row}");
    }
    assert!(!body.to_string().contains("SCIM"), "no draft text: {body}");
    assert_eq!(row["outcome"], "pending");
    assert_eq!(row["model_alias"], "pmos/extraction");

    // The Slack bot reads the rows for the triage digest, without approval ids (it uses
    // `run_id`), but not the summary.
    let (status, bot_body) = s.get(BOT_TOKEN, "/internal/ledger").await;
    assert_eq!(status, 200);
    let mut without_ids = body.clone();
    for row in without_ids["rows"].as_array_mut().unwrap() {
        row.as_object_mut().unwrap().remove("id");
    }
    assert_eq!(bot_body, without_ids);
    assert!(bot_body["rows"][0].get("id").is_none());
    assert!(bot_body["rows"][0]["run_id"].is_string());
    assert_eq!(s.get(BOT_TOKEN, "/internal/ledger/summary").await.0, 403);
    let (status, summary) = s.get(SERVICE_TOKEN, "/internal/ledger/summary").await;
    assert_eq!(status, 200);
    assert_eq!(summary["rows"], 1);
    assert_eq!(summary["label_coverage"], 0.0);
    assert_eq!(summary["groups"][0]["counts"]["pending"], 1);

    // A sandbox's session token is not an internal token; nor is nothing.
    assert_eq!(s.get(&session.token, "/internal/ledger").await.0, 401);
    assert_eq!(
        s.get(&session.token, "/internal/ledger/summary").await.0,
        401
    );
    let res = reqwest::Client::new()
        .get(format!("{}/internal/ledger", s.base))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 401);
}

#[tokio::test]
async fn ledger_queries_over_http() {
    let s = start().await;
    let session = skill_session(&s.fixture, "r1").await;
    let rejected = draft(&s.fixture, &session, "One").await;
    let edited = draft(&s.fixture, &session, "Two").await;
    let approved = draft(&s.fixture, &session, "Three").await;
    let svc = &s.fixture.svc;
    svc.decide(&rejected, "ana", Decision::Reject, None, now())
        .await
        .unwrap();
    svc.decide(
        &edited,
        "ana",
        Decision::Edit,
        Some(json!({"project": "PM", "issue_type": "Bug", "summary": "Entirely different"})),
        now(),
    )
    .await
    .unwrap();
    svc.decide(&approved, "ana", Decision::Approve, None, now())
        .await
        .unwrap();

    let ids = |body: &Value| -> Vec<String> {
        let mut ids: Vec<String> = body["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_str().unwrap().to_string())
            .collect();
        ids.sort();
        ids
    };
    let sorted = |mut v: Vec<String>| {
        v.sort();
        v
    };
    let filtered =
        "/internal/ledger?outcome=rejected,edited&min_edit=0.3&since=2026-11-14T00:00:00Z";
    let (status, body) = s.get(SERVICE_TOKEN, filtered).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(ids(&body), sorted(vec![rejected.clone(), edited.clone()]));
    // The bot gets the same rows, without their ids.
    let (status, bot_body) = s.get(BOT_TOKEN, filtered).await;
    assert_eq!(status, 200, "{bot_body}");
    let bot_rows = bot_body["rows"].as_array().unwrap();
    assert_eq!(bot_rows.len(), 2);
    assert!(bot_rows.iter().all(|r| r.get("id").is_none()));
    let (_, body) = s
        .get(SERVICE_TOKEN, "/internal/ledger?outcome=approved&limit=1")
        .await;
    assert_eq!(ids(&body), vec![approved.clone()]);
    let (_, body) = s
        .get(SERVICE_TOKEN, "/internal/ledger?until=2026-11-14T00:00:00Z")
        .await;
    assert!(ids(&body).is_empty());
    let (_, body) = s.get(SERVICE_TOKEN, "/internal/ledger?bot=scout").await;
    assert!(ids(&body).is_empty());

    for bad in [
        "since=yesterday",
        "until=2026-13-01",
        "outcome=executing",
        "min_edit=1.5",
        "min_edit=x",
        "limit=0",
        "limit=1001",
    ] {
        let (status, body) = s
            .get(SERVICE_TOKEN, &format!("/internal/ledger?{bad}"))
            .await;
        assert_eq!(status, 422, "{bad}: {body}");
    }
    assert_eq!(
        s.get(SERVICE_TOKEN, "/internal/ledger/summary?since=soon")
            .await
            .0,
        422
    );
}

#[tokio::test]
async fn the_retry_cap_over_http() {
    let s = start().await;
    // The HTTP layer checks sessions against the real clock.
    let session = s
        .fixture
        .svc
        .create_session("ana", "meeting-notes", "r1", None, chrono::Utc::now())
        .await
        .unwrap();
    let call = |args: Value| {
        let base = s.base.clone();
        let token = session.token.clone();
        async move {
            let res = reqwest::Client::new()
                .post(format!("{base}/internal/call"))
                .bearer_auth(SERVICE_TOKEN)
                .header("x-pmos-session", token)
                .json(&json!({"tool": "jira_create_issue", "args": args}))
                .send()
                .await
                .unwrap();
            let status = res.status().as_u16();
            (status, res.json::<Value>().await.unwrap())
        }
    };
    let (status, body) = call(invalid()).await;
    assert_eq!(status, 400);
    assert!(!body["error"].as_str().unwrap().contains(LAST_ATTEMPT));
    let (status, body) = call(invalid()).await;
    assert_eq!(status, 400);
    assert!(body["error"].as_str().unwrap().ends_with(LAST_ATTEMPT));
    let (status, body) = call(issue("Fixed")).await;
    assert_eq!(status, 403);
    assert!(body["error"].as_str().unwrap().ends_with(LAST_ATTEMPT));
    assert!(s.fixture.jira.writes().is_empty());
}
