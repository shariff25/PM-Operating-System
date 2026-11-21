//! The bets register end to end (`docs/staff-principal-spec.md` §5), on the in-memory and
//! the Postgres stores (`tests/github.rs` runs it on GitHub memory): `bets.record` is an
//! approval, nothing is saved before it, the server merges and saves the note on approval,
//! edits are checked like a call, a team bot is refused, and everything is audited.

mod common;

use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use common::{now, write_content, Fixture};
use pmos_contract::model::Sensitivity;
use pmos_toolserver::approvals::{Decision, Status};
use pmos_toolserver::audit::Outcome;
use pmos_toolserver::catalog::Catalog;
use pmos_toolserver::connectors::fake::FakeConnector;
use pmos_toolserver::connectors::Connector;
use pmos_toolserver::service::{DecisionError, ToolError};
use pmos_toolserver::session::Session;
use pmos_toolserver::ToolService;
use serde_json::{json, Value};

/// A skill with the bets tools, so the test doesn't depend on `bets-register`'s text.
pub const BETS_SKILL: &str = "---\nname: bets-test\ndescription: Test skill that keeps the bets register.\nmetadata:\n  owner: \"@pm\"\n  version: \"0.1.0\"\n  model: \"reasoning\"\n  data-class: \"internal\"\n  tools: \"context.search bets.list bets.read bets.record memory.read memory.update\"\n---\n\n# /bets-test\n\nTest.\n";

/// Add the test skill, and a team bot `planner` whose role has it, then rebuild the
/// service on the fixture's stores.
fn with_bets(mut f: Fixture) -> Fixture {
    write_content(&f.root, "skills/bets-test/SKILL.md", BETS_SKILL);
    write_content(
        &f.root,
        "roles/bets-team/ROLE.md",
        "---\nname: bets-team\ndescription: Test team role with the bets tools.\nowner: \"@pm\"\nversion: \"0.1.0\"\nskills: \"bets-test\"\ndata-class: \"internal\"\nmodel: \"default\"\nactions:\n  bets.record: approve\n---\n\n# Bets team\n\nTest.\n",
    );
    write_content(
        &f.root,
        "bots/planner/BOT.md",
        "---\nname: planner\nrole: bets-team\nscope: team\nowner: \"@ana\"\ndescription: Test team bot with the bets tools.\n---\n\n# Planner\n\nTest.\n",
    );
    let catalog = Catalog::load(&f.root, now().date_naive()).expect("content lints");
    let connectors: Vec<Arc<dyn Connector>> = vec![
        f.jira.clone(),
        f.confluence.clone(),
        f.slack.clone(),
        Arc::new(FakeConnector::docs()),
    ];
    f.svc = Arc::new(
        ToolService::new(catalog, f.stores.clone(), connectors)
            .unwrap()
            .with_open_directory(),
    );
    f
}

fn new_bet() -> Value {
    json!({
        "key": "sso-enterprise",
        "title": "SSO wins the enterprise tier",
        "bet": "SAML SSO in the Acme Vault console moves enterprise trials to paid.",
        "metric": "Enterprise trial-to-paid conversion",
        "riskiest_assumption": "SSO is the blocker, not price",
        "test": "Offer SSO to 20 trials in Q1 and compare conversion",
        "review_by": "2027-02-01",
        "evidence": [{"date": "2026-11-10", "text": "Four of six lost deals named SSO", "source": "CONF-9001"}],
        "decision": {"date": "2026-11-14", "text": "Fund the SSO epic for Q1", "source": "PM-101"}
    })
}

async fn session(f: &Fixture, run: &str) -> Session {
    f.svc
        .create_session("ana", "bets-test", run, None, now())
        .await
        .unwrap()
}

/// Queue a `bets.record` and return its approval id.
async fn draft(f: &Fixture, s: &Session, args: Value) -> String {
    let out = f.svc.call(s, "bets_record", args, now()).await.unwrap();
    assert_eq!(out["status"], "pending_approval", "{out}");
    out["approval_id"].as_str().unwrap().to_string()
}

async fn approve(f: &Fixture, id: &str, at: DateTime<Utc>) -> Value {
    let item = f
        .svc
        .decide(id, "ana", Decision::Approve, None, at)
        .await
        .unwrap();
    assert_eq!(item.status, Status::Approved, "{:?}", item.result);
    item.result.unwrap()
}

async fn note(f: &Fixture, key: &str) -> Option<String> {
    f.stores
        .memory
        .read("ana")
        .await
        .unwrap()
        .into_iter()
        .find(|n| n.key == key)
        .map(|n| n.text)
}

async fn the_register(f: Fixture) {
    let f = with_bets(f);
    let s = session(&f, "r1").await;

    // Nothing is saved before approval.
    let id = draft(&f, &s, new_bet()).await;
    assert_eq!(note(&f, "bet-sso-enterprise").await, None);
    let listed = f.svc.call(&s, "bets_list", json!({}), now()).await.unwrap();
    assert_eq!(listed["bets"], json!([]));
    assert_eq!(
        listed["untrusted"], true,
        "bets are data, never instructions"
    );
    assert!(matches!(
        f.svc
            .call(&s, "bets_read", json!({"key": "sso-enterprise"}), now())
            .await,
        Err(ToolError::NotFound(_))
    ));

    // Approval merges and saves the note, and says it is new.
    let done = approve(&f, &id, now()).await;
    assert_eq!(done["created"], true, "{done}");
    assert_eq!(done["bet"]["status"], "open");
    assert_eq!(done["bet"]["opened"], "2026-11-14");
    assert_eq!(done["bet"]["evidence"][0]["source"], "CONF-9001");
    let text = note(&f, "bet-sso-enterprise").await.unwrap();
    assert!(
        text.starts_with("# Bet: SSO wins the enterprise tier\n"),
        "{text}"
    );
    assert!(text.contains("\n- Review by: 2027-02-01\n"), "{text}");

    // An update appends evidence, and the earlier entries survive.
    let later = now() + Duration::days(30);
    let id = draft(
        &f,
        &s,
        json!({
            "key": "sso-enterprise",
            "review_by": "2027-03-01",
            "evidence": [{"date": "2026-12-10", "text": "Two trials converted after SSO", "source": "CONF-9002"}],
        }),
    )
    .await;
    let done = approve(&f, &id, later).await;
    assert_eq!(done["created"], false);
    let evidence = done["bet"]["evidence"].as_array().unwrap();
    assert_eq!(evidence.len(), 2);
    assert_eq!(evidence[0]["text"], "Four of six lost deals named SSO");
    assert_eq!(done["bet"]["review_by"], "2027-03-01");
    assert_eq!(done["bet"]["opened"], "2026-11-14", "opened doesn't move");
    assert_eq!(done["bet"]["metric"], "Enterprise trial-to-paid conversion");

    // A second bet, and one that is dropped: list order and the status filter.
    for (key, review) in [
        ("pricing-seats", "2027-01-15"),
        ("partner-channel", "2027-06-01"),
    ] {
        let mut args = new_bet();
        args["key"] = json!(key);
        args["title"] = json!(format!("Bet {key}"));
        args["review_by"] = json!(review);
        let id = draft(&f, &s, args).await;
        approve(&f, &id, now()).await;
    }
    let id = draft(
        &f,
        &s,
        json!({"key": "partner-channel", "status": "dropped",
               "decision": {"date": "2026-11-14", "text": "No partner budget this year"}}),
    )
    .await;
    approve(&f, &id, now()).await;
    // A note that isn't a bet stays out of the register.
    f.stores
        .memory
        .update(
            "ana",
            "areas",
            "Acme Vault",
            "t",
            Sensitivity::Internal,
            now(),
        )
        .await
        .unwrap();

    let fresh = session(&f, "r2").await;
    let listed = f
        .svc
        .call(&fresh, "bets_list", json!({}), now())
        .await
        .unwrap();
    let keys: Vec<&str> = listed["bets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["key"].as_str().unwrap())
        .collect();
    assert_eq!(
        keys,
        vec!["pricing-seats", "sso-enterprise", "partner-channel"]
    );
    let first = &listed["bets"][0];
    assert_eq!(
        first.as_object().unwrap().keys().collect::<Vec<_>>(),
        vec!["key", "review_by", "status", "title", "updated_at"]
    );
    let open = f
        .svc
        .call(&fresh, "bets_list", json!({"status": "open"}), now())
        .await
        .unwrap();
    assert_eq!(open["bets"].as_array().unwrap().len(), 2);
    let dropped = f
        .svc
        .call(&fresh, "bets_list", json!({"status": "dropped"}), now())
        .await
        .unwrap();
    assert_eq!(dropped["bets"][0]["key"], "partner-channel");
    // Reading the register raises the session to internal.
    let stored = f
        .stores
        .sessions
        .get(&fresh.token, now())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.sources_sensitivity, Sensitivity::Internal);

    let read = f
        .svc
        .call(
            &fresh,
            "bets_read",
            json!({"key": "partner-channel"}),
            now(),
        )
        .await
        .unwrap();
    assert_eq!(read["bet"]["status"], "dropped");
    assert_eq!(read["bet"]["decisions"].as_array().unwrap().len(), 2);
    assert!(read.get("results").is_none(), "not a connector result list");
    assert_eq!(read["untrusted"], true, "but marked as data, like one");

    // The audit trail: queued, started, approved; reads as ok.
    let trail: Vec<(String, Outcome)> = f
        .audit
        .events()
        .into_iter()
        .filter(|e| e.tool.starts_with("bets."))
        .map(|e| (e.tool, e.outcome))
        .collect();
    let first_five: Vec<(&str, Outcome)> = trail
        .iter()
        .take(6)
        .map(|(t, o)| (t.as_str(), *o))
        .collect();
    assert_eq!(
        first_five,
        vec![
            ("bets.record", Outcome::Pending),
            ("bets.list", Outcome::Ok),
            ("bets.read", Outcome::Error),
            ("bets.record", Outcome::Started),
            ("bets.record", Outcome::Approved),
            ("bets.record", Outcome::Pending),
        ]
    );
    let approved = f
        .audit
        .events()
        .into_iter()
        .find(|e| e.tool == "bets.record" && e.outcome == Outcome::Approved)
        .unwrap();
    assert_eq!(
        approved.output_ref.as_deref(),
        Some("memory:bet-sso-enterprise")
    );
}

async fn calls_are_checked(f: Fixture) {
    let f = with_bets(f);
    let s = session(&f, "r1").await;

    // A new bet without its required fields is refused before anything is queued.
    let err = f
        .svc
        .call(
            &s,
            "bets_record",
            json!({"key": "half", "title": "Half a bet"}),
            now(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, ToolError::InvalidArgs(ref m) if m.contains("`bet`, `metric`, `riskiest_assumption`, `review_by`")),
        "{err:?}"
    );
    // So is a bad date, a bad key, or too much evidence (by the schema).
    for args in [
        json!({"key": "sso", "review_by": "next month"}),
        json!({"key": "SSO!", "title": "x"}),
        json!({"key": "sso", "evidence": vec![json!({"date": "2026-11-01", "text": "x"}); 6]}),
    ] {
        let fresh = session(&f, "r-schema").await;
        assert!(
            matches!(
                f.svc.call(&fresh, "bets_record", args.clone(), now()).await,
                Err(ToolError::InvalidArgs(_))
            ),
            "{args}"
        );
    }
    assert!(f.svc.approvals("ana", None).await.unwrap().is_empty());

    // An edit at approval is checked with the same rules; a bad one leaves it pending.
    let s = session(&f, "r2").await;
    let id = draft(&f, &s, new_bet()).await;
    let mut bad = new_bet();
    bad["review_by"] = json!("2027-02-30");
    let err = f
        .svc
        .decide(&id, "ana", Decision::Edit, Some(bad), now())
        .await
        .unwrap_err();
    assert!(
        matches!(err, DecisionError::InvalidArgs(ref m) if m.contains("review_by")),
        "{err:?}"
    );
    let mut bad = new_bet();
    bad.as_object_mut().unwrap().remove("metric");
    let err = f
        .svc
        .decide(&id, "ana", Decision::Edit, Some(bad), now())
        .await
        .unwrap_err();
    assert!(
        matches!(err, DecisionError::InvalidArgs(ref m) if m.contains("metric")),
        "{err:?}"
    );
    assert_eq!(
        f.svc.approval(&id).await.unwrap().unwrap().status,
        Status::Pending
    );
    assert_eq!(note(&f, "bet-sso-enterprise").await, None);

    // A good edit is saved as edited.
    let mut edited = new_bet();
    edited["title"] = json!("SSO decides the enterprise tier");
    let item = f
        .svc
        .decide(&id, "ana", Decision::Edit, Some(edited), now())
        .await
        .unwrap();
    assert_eq!(item.status, Status::Approved);
    assert_eq!(
        item.result.as_ref().unwrap()["bet"]["title"],
        "SSO decides the enterprise tier"
    );
    assert!(note(&f, "bet-sso-enterprise")
        .await
        .unwrap()
        .starts_with("# Bet: SSO decides"));

    // Two drafts of the same new bet: the second, approved after the first, is an update.
    let a = draft(&f, &s, {
        let mut b = new_bet();
        b["key"] = json!("twice");
        b
    })
    .await;
    let b = draft(&f, &s, {
        let mut b = new_bet();
        b["key"] = json!("twice");
        b
    })
    .await;
    assert_eq!(approve(&f, &a, now()).await["created"], true);
    assert_eq!(approve(&f, &b, now()).await["created"], false);
    let twice = note(&f, "bet-twice").await.unwrap();
    assert_eq!(
        twice.matches("Four of six lost deals").count(),
        2,
        "evidence appends"
    );
}

async fn a_full_note_drops_its_oldest_evidence(f: Fixture) {
    let f = with_bets(f);
    let s = session(&f, "r1").await;
    let id = draft(&f, &s, new_bet()).await;
    approve(&f, &id, now()).await;
    let mut last = Value::Null;
    for round in 0..10 {
        let evidence: Vec<Value> = (0..5)
            .map(|i| {
                json!({"date": "2026-12-01", "text": format!("Readout {round}.{i}: {}", "trial notes ".repeat(8)), "source": "CONF-9002"})
            })
            .collect();
        let s = session(&f, &format!("r-{round}")).await;
        let id = draft(
            &f,
            &s,
            json!({"key": "sso-enterprise", "evidence": evidence}),
        )
        .await;
        last = approve(&f, &id, now()).await;
    }
    let text = note(&f, "bet-sso-enterprise").await.unwrap();
    assert!(text.chars().count() <= 4000, "{}", text.len());
    let dropped = last["bet"]["evidence_dropped"].as_u64().unwrap();
    assert!(dropped > 0, "{last}");
    assert!(
        text.contains(&format!("- Earlier evidence dropped: {dropped}\n")),
        "{text}"
    );
    assert!(
        !text.contains("Four of six lost deals"),
        "the oldest went first"
    );
    assert!(text.contains("Readout 9.4"), "the newest stays");
    // The decision is never dropped.
    assert!(text.contains("Fund the SSO epic for Q1"));
}

async fn a_team_bot_is_refused(f: Fixture) {
    let f = with_bets(f);
    let planner = f
        .svc
        .create_bot_session("ana", "planner", "tsk_1", None, now())
        .await
        .unwrap();
    // The memory tools too: memory is the PM's own (S12).
    for (tool, args) in [
        ("bets_list", json!({})),
        ("bets_read", json!({"key": "sso-enterprise"})),
        ("bets_record", new_bet()),
        ("memory_read", json!({})),
        (
            "memory_update",
            json!({"key": "areas", "text": "Acme Vault"}),
        ),
    ] {
        let err = f.svc.call(&planner, tool, args, now()).await.unwrap_err();
        assert!(
            matches!(err, ToolError::Forbidden(ref m) if m.contains("never a team bot")),
            "{tool}: {err:?}"
        );
        let e = f.audit.events().last().cloned().unwrap();
        assert_eq!(
            (e.outcome, e.bot.as_deref()),
            (Outcome::Denied, Some("planner"))
        );
    }
    assert!(f.svc.approvals("ana", None).await.unwrap().is_empty());
    assert_eq!(note(&f, "bet-sso-enterprise").await, None);
    assert_eq!(note(&f, "areas").await, None);
}

/// A bet's note keeps the sensitivity of what its `bets.record` was drafted from (S6), and
/// reading it raises the reader to it. `memory.update` can't write a bet (S10).
async fn a_bet_carries_its_sources_sensitivity(f: Fixture) {
    let f = with_bets(f);
    // An item drafted by a session that had read restricted sources.
    let args = new_bet();
    let item = f
        .stores
        .approvals
        .create(
            pmos_toolserver::approvals::NewItem {
                user: "ana",
                skill: Some("bets-test"),
                bot: None,
                run_id: "r-restricted",
                tool: "bets.record",
                args: &args,
                content_sha: None,
                model_alias: None,
                sources_sensitivity: Sensitivity::Restricted,
                persona: None,
            },
            now(),
        )
        .await
        .unwrap();
    approve(&f, &item.id, now()).await;
    let stored = |key: &'static str| {
        let f = &f;
        async move {
            f.stores
                .memory
                .read("ana")
                .await
                .unwrap()
                .into_iter()
                .find(|n| n.key == key)
                .map(|n| n.sensitivity)
        }
    };
    assert_eq!(
        stored("bet-sso-enterprise").await,
        Some(Sensitivity::Restricted)
    );

    // A later update from a session that read nothing doesn't lower it: the merge keeps the
    // bet's earlier text.
    let s = session(&f, "r-public").await;
    let id = draft(
        &f,
        &s,
        json!({"key": "sso-enterprise", "review_by": "2027-03-01"}),
    )
    .await;
    approve(&f, &id, now()).await;
    assert_eq!(
        stored("bet-sso-enterprise").await,
        Some(Sensitivity::Restricted)
    );

    let raised = |s: Session| {
        let f = &f;
        async move {
            f.stores
                .sessions
                .get(&s.token, now())
                .await
                .unwrap()
                .unwrap()
                .sources_sensitivity
        }
    };
    let reader = session(&f, "r-read").await;
    f.svc
        .call(
            &reader,
            "bets_read",
            json!({"key": "sso-enterprise"}),
            now(),
        )
        .await
        .unwrap();
    assert_eq!(raised(reader).await, Sensitivity::Restricted);
    let lister = session(&f, "r-list").await;
    f.svc
        .call(&lister, "bets_list", json!({}), now())
        .await
        .unwrap();
    assert_eq!(raised(lister).await, Sensitivity::Restricted);
    // Listing only other bets: nothing listed, nothing raised.
    let other = session(&f, "r-other").await;
    f.svc
        .call(&other, "bets_list", json!({"status": "dropped"}), now())
        .await
        .unwrap();
    assert_eq!(raised(other).await, Sensitivity::Public);

    // memory.update refuses the bets register's keys.
    let err = f
        .svc
        .call(
            &s,
            "memory_update",
            json!({"key": "bet-sso-enterprise", "text": "- Status: confirmed"}),
            now(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, ToolError::InvalidArgs(ref m) if m.contains("bets_record")),
        "{err:?}"
    );
    assert_eq!(
        stored("bet-sso-enterprise").await,
        Some(Sensitivity::Restricted)
    );
}

macro_rules! on_both_backends {
    ($($case:ident),* $(,)?) => {
        mod memory {
            $(
                #[tokio::test]
                async fn $case() {
                    super::$case(super::common::fixture()).await;
                }
            )*
        }
        mod postgres {
            $(
                #[tokio::test]
                async fn $case() {
                    let Some((mut f, pg)) = super::common::pg_fixture().await else { return };
                    // Audited to Postgres, and to memory for the test to read.
                    f.stores.audit = std::sync::Arc::new(pmos_toolserver::audit::TeeAudit(vec![
                        f.stores.audit.clone(),
                        f.audit.clone(),
                    ]));
                    super::$case(f).await;
                    pg.drop_schema().await;
                }
            )*
        }
    };
}

on_both_backends!(
    the_register,
    calls_are_checked,
    a_full_note_drops_its_oldest_evidence,
    a_team_bot_is_refused,
    a_bet_carries_its_sources_sensitivity,
);

/// The PM's personal bot has the register (the role's `bets-register` skill), and its
/// `bets.record` waits for approval like any write.
#[tokio::test]
async fn a_personal_bot_keeps_its_pms_register() {
    let f = with_bets(common::fixture());
    let mine = f
        .svc
        .create_bot_session("ana", "pm-ana", "tsk_1", None, now())
        .await
        .unwrap();
    let out = f
        .svc
        .call(&mine, "bets_record", new_bet(), now())
        .await
        .unwrap();
    assert_eq!(out["status"], "pending_approval");
    let id = out["approval_id"].as_str().unwrap();
    let done = approve(&f, id, now()).await;
    assert_eq!(done["created"], true);
    let listed = f
        .svc
        .call(&mine, "bets_list", json!({}), now())
        .await
        .unwrap();
    assert_eq!(listed["bets"][0]["key"], "sso-enterprise");
}

/// Memory that fails every call with a detail a caller mustn't see.
struct BrokenMemory;

#[async_trait::async_trait]
impl pmos_toolserver::memory::MemoryStore for BrokenMemory {
    async fn read(
        &self,
        _user: &str,
    ) -> pmos_toolserver::memory::MemoryResult<Vec<pmos_toolserver::memory::Note>> {
        Err(pmos_toolserver::memory::MemoryError::Unavailable(
            "memory host db-7.internal:5432 refused".into(),
        ))
    }

    async fn update(
        &self,
        _user: &str,
        _key: &str,
        _text: &str,
        _source: &str,
        _sensitivity: Sensitivity,
        _now: DateTime<Utc>,
    ) -> pmos_toolserver::memory::MemoryResult<pmos_toolserver::memory::Note> {
        Err(pmos_toolserver::memory::MemoryError::Unavailable(
            "memory host db-7.internal:5432 refused".into(),
        ))
    }
}

/// An internal failure of an approved write answers a generic message; the detail goes to
/// the log only (S24).
#[tokio::test]
async fn an_internal_failure_answers_a_generic_message() {
    let mut f = with_bets(common::fixture());
    f.stores.memory = Arc::new(BrokenMemory);
    let catalog = Catalog::load(&f.root, now().date_naive()).expect("content lints");
    let connectors: Vec<Arc<dyn Connector>> = vec![
        f.jira.clone(),
        f.confluence.clone(),
        f.slack.clone(),
        Arc::new(FakeConnector::docs()),
    ];
    let svc = Arc::new(
        ToolService::new(catalog, f.stores.clone(), connectors)
            .unwrap()
            .with_open_directory(),
    );
    let args = new_bet();
    let item = f
        .stores
        .approvals
        .create(
            pmos_toolserver::approvals::NewItem {
                user: "ana",
                skill: Some("bets-test"),
                bot: None,
                run_id: "r-broken",
                tool: "bets.record",
                args: &args,
                content_sha: None,
                model_alias: None,
                sources_sensitivity: Sensitivity::Internal,
                persona: None,
            },
            now(),
        )
        .await
        .unwrap();

    let cancel = tokio_util::sync::CancellationToken::new();
    let app = pmos_toolserver::router(
        svc,
        pmos_toolserver::AppConfig {
            service_token: "test-service-token-0123456789abcdef".into(),
            bot: None,
            console_token: None,
            content_root: f.root.clone(),
            oauth: None,
            allowed_hosts: vec!["127.0.0.1".into()],
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
    let res = reqwest::Client::new()
        .post(format!("{base}/internal/approvals/{}/decision", item.id))
        .bearer_auth("test-service-token-0123456789abcdef")
        .json(&json!({"decided_by": "ana", "decision": "approve"}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 500);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["error"], pmos_toolserver::app::INTERNAL_FAILURE);
    assert!(!body.to_string().contains("db-7"), "{body}");
    cancel.cancel();
}
