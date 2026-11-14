//! One contract, two backends: every case runs against the in-memory stores and against
//! Postgres.
//!
//! Postgres cases need `PMOS_TEST_DATABASE_URL` (a database the tests may create schemas
//! in). Without it they are skipped locally and **fail in CI** (`CI` is set), so a missing
//! database can't pass silently. Each Postgres case gets its own schema, dropped at the end.

mod common;

use std::sync::Arc;

use chrono::Duration;
use pmos_contract::Sensitivity;
use pmos_toolserver::approvals::{
    DecideError, Decision, MemoryApprovals, NewItem, Status, GRANT_TTL_MINUTES,
};
use pmos_toolserver::audit::{AuditEvent, MemoryAudit, Outcome};
use pmos_toolserver::context::{ContextDoc, MemoryIndex};
use pmos_toolserver::memory::InMemoryStore;
use pmos_toolserver::pg::PgStore;
use pmos_toolserver::session::{token_hash, MemorySessions, Session};
use pmos_toolserver::vault::{
    Credential, KeyRing, MemoryCredentials, Provider, SealedCredential, Secret, Vault,
};
use pmos_toolserver::Stores;
use serde_json::{json, Value};
use sqlx::Row;

use common::now;

fn doc(path: &str, title: &str, sensitivity: Sensitivity, body: &str) -> ContextDoc {
    ContextDoc {
        path: path.to_string(),
        domain: path.split('/').nth(1).unwrap().to_string(),
        title: title.to_string(),
        owner: "@pm".to_string(),
        last_reviewed: "2026-11-01".parse().unwrap(),
        sensitivity,
        body: body.to_string(),
    }
}

fn library() -> Vec<ContextDoc> {
    vec![
        doc(
            "context/products/ccm.md",
            "CCM",
            Sensitivity::Internal,
            "# CCM\n\nKey management for multi-cloud estates.\n",
        ),
        doc(
            "context/competitors/acme.md",
            "Acme key management",
            Sensitivity::Internal,
            "# Acme\n\nAcme sells key management appliances.\n",
        ),
        doc(
            "context/partners/nda.md",
            "Partner roadmap",
            Sensitivity::Restricted,
            "# Partner\n\nJoint roadmap under NDA.\n",
        ),
    ]
}

/// A backend under test, plus the pieces some cases inspect directly.
struct Backend {
    stores: Stores,
    memory_audit: Option<Arc<MemoryAudit>>,
    pg: Option<PgStore>,
    test_pg: Option<common::TestPg>,
}

impl Backend {
    fn memory(docs: Vec<ContextDoc>) -> Self {
        let audit = Arc::new(MemoryAudit::default());
        Self {
            stores: Stores {
                context: Arc::new(MemoryIndex::from_docs(docs)),
                memory: Arc::new(InMemoryStore::default()),
                sessions: Arc::new(MemorySessions::default()),
                approvals: Arc::new(MemoryApprovals::default()),
                audit: audit.clone(),
                credentials: Arc::new(MemoryCredentials::default()),
            },
            memory_audit: Some(audit),
            pg: None,
            test_pg: None,
        }
    }

    async fn postgres(docs: Vec<ContextDoc>) -> Option<Self> {
        let pg = common::TestPg::new().await?;
        pg.store.reindex(&docs).await.expect("reindex");
        Some(Self {
            stores: pg.store.stores(),
            memory_audit: None,
            pg: Some(pg.store.clone()),
            test_pg: Some(pg),
        })
    }

    async fn finish(self) {
        if let Some(pg) = self.test_pg {
            pg.drop_schema().await;
        }
    }
}

/// Generate a `memory` and a `postgres` test for each case.
macro_rules! contract {
    ($($case:ident),* $(,)?) => {
        mod memory {
            $(
                #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
                async fn $case() {
                    let b = super::Backend::memory(super::library());
                    super::$case(&b).await;
                    b.finish().await;
                }
            )*
        }
        mod postgres {
            $(
                #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
                async fn $case() {
                    if let Some(b) = super::Backend::postgres(super::library()).await {
                        super::$case(&b).await;
                        b.finish().await;
                    }
                }
            )*
        }
    };
}

contract!(
    sessions_expire_and_revoke,
    grants_match_one_exact_call_once,
    grants_are_taken_once_under_concurrency,
    approvals_list_newest_first_and_filter,
    decisions_follow_the_rules,
    decisions_win_once_under_concurrency,
    completion_records_results_and_failures,
    memory_is_per_user,
    context_search_and_get,
    reindex_tracks_the_content_repo,
    audit_records_events,
    credentials_are_per_user_and_provider,
    credential_replace_is_conditional,
    vault_round_trips_through_the_store,
);

// ----- sessions ---------------------------------------------------------------------------

async fn sessions_expire_and_revoke(b: &Backend) {
    let s = Session::new("ana", "ask", "r1", Duration::minutes(30), now());
    b.stores.sessions.insert(&s, now()).await.unwrap();
    let live = b
        .stores
        .sessions
        .get(&s.token, now() + Duration::minutes(29))
        .await
        .unwrap();
    assert_eq!(live.as_ref().map(|x| x.user.as_str()), Some("ana"));
    assert_eq!(live.unwrap().token, s.token);
    assert!(b
        .stores
        .sessions
        .get(&s.token, now() + Duration::minutes(30))
        .await
        .unwrap()
        .is_none());
    assert!(b
        .stores
        .sessions
        .get("pmos_s_unknown", now())
        .await
        .unwrap()
        .is_none());

    assert!(b.stores.sessions.revoke(&s.token).await.unwrap());
    assert!(!b.stores.sessions.revoke(&s.token).await.unwrap());
    assert!(b
        .stores
        .sessions
        .get(&s.token, now())
        .await
        .unwrap()
        .is_none());
}

// ----- grants -----------------------------------------------------------------------------

async fn grants_match_one_exact_call_once(b: &Backend) {
    let a = b.stores.approvals.as_ref();
    let args = json!({"summary": "Add SSO", "project": "PM"});
    a.add_grant("key-1", "jira.create_issue", &args, now())
        .await
        .unwrap();

    let other = json!({"summary": "Other", "project": "PM"});
    assert!(!a
        .take_grant("key-1", "jira.create_issue", &other, now())
        .await
        .unwrap());
    assert!(!a
        .take_grant("key-2", "jira.create_issue", &args, now())
        .await
        .unwrap());
    assert!(!a
        .take_grant("key-1", "slack.send_message", &args, now())
        .await
        .unwrap());
    let reordered = json!({"project": "PM", "summary": "Add SSO"});
    assert!(a
        .take_grant("key-1", "jira.create_issue", &reordered, now())
        .await
        .unwrap());
    assert!(
        !a.take_grant("key-1", "jira.create_issue", &args, now())
            .await
            .unwrap(),
        "single use"
    );

    a.add_grant("key-1", "jira.create_issue", &args, now())
        .await
        .unwrap();
    let expired = now() + Duration::minutes(GRANT_TTL_MINUTES);
    assert!(!a
        .take_grant("key-1", "jira.create_issue", &args, expired)
        .await
        .unwrap());
}

async fn grants_are_taken_once_under_concurrency(b: &Backend) {
    let args = json!({"title": "PRD"});
    b.stores
        .approvals
        .add_grant("key-c", "confluence.create_draft", &args, now())
        .await
        .unwrap();
    let mut handles = Vec::new();
    for _ in 0..10 {
        let approvals = b.stores.approvals.clone();
        let args = args.clone();
        handles.push(tokio::spawn(async move {
            approvals
                .take_grant("key-c", "confluence.create_draft", &args, now())
                .await
                .unwrap()
        }));
    }
    let mut won = 0;
    for h in handles {
        if h.await.unwrap() {
            won += 1;
        }
    }
    assert_eq!(won, 1, "exactly one concurrent call may use a grant");
}

// ----- approvals --------------------------------------------------------------------------

fn new_item<'a>(user: &'a str, args: &'a Value) -> NewItem<'a> {
    NewItem {
        user,
        skill: "prd-draft",
        run_id: "r1",
        tool: "confluence.create_draft",
        args,
    }
}

async fn approvals_list_newest_first_and_filter(b: &Backend) {
    let a = b.stores.approvals.as_ref();
    let args = json!({"title": "PRD"});
    let first = a.create(new_item("ana", &args), now()).await.unwrap();
    let second = a
        .create(new_item("ana", &args), now() + Duration::minutes(1))
        .await
        .unwrap();
    a.create(new_item("bo", &args), now()).await.unwrap();

    let got = a.get(&first.id).await.unwrap().unwrap();
    assert_eq!(got, first);
    assert!(a.get("apr_missing").await.unwrap().is_none());

    let ids: Vec<_> = a
        .list("ana", None)
        .await
        .unwrap()
        .into_iter()
        .map(|i| i.id)
        .collect();
    assert_eq!(ids, vec![second.id.clone(), first.id.clone()]);

    a.decide(&first.id, "ana", Decision::Reject, None, now())
        .await
        .unwrap();
    let pending = a.list("ana", Some(Status::Pending)).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, second.id);
    assert!(a.list("cy", None).await.unwrap().is_empty());
}

async fn decisions_follow_the_rules(b: &Backend) {
    let a = b.stores.approvals.as_ref();
    let args = json!({"title": "PRD"});
    let item = a.create(new_item("ana", &args), now()).await.unwrap();

    assert_eq!(
        a.decide("apr_missing", "ana", Decision::Approve, None, now())
            .await,
        Err(DecideError::NotFound)
    );
    assert_eq!(
        a.decide(&item.id, "bo", Decision::Approve, None, now())
            .await,
        Err(DecideError::Forbidden)
    );
    assert_eq!(
        a.decide(&item.id, "ana", Decision::Edit, None, now()).await,
        Err(DecideError::MissingArgs)
    );
    assert_eq!(
        a.get(&item.id).await.unwrap().unwrap().status,
        Status::Pending
    );

    let edited = json!({"title": "PRD v2"});
    let later = now() + Duration::minutes(3);
    let decided = a
        .decide(&item.id, "ana", Decision::Edit, Some(edited.clone()), later)
        .await
        .unwrap();
    assert_eq!(decided.status, Status::Approved);
    assert_eq!(decided.args, edited);
    assert_eq!(
        decided.args_hash,
        pmos_toolserver::approvals::args_hash(&edited)
    );
    assert_eq!(decided.decided_by.as_deref(), Some("ana"));
    assert_eq!(decided.decided_at, Some(later));

    assert_eq!(
        a.decide(&item.id, "ana", Decision::Approve, None, now())
            .await,
        Err(DecideError::NotPending)
    );
    assert_eq!(
        a.decide(&item.id, "bo", Decision::Approve, None, now())
            .await,
        Err(DecideError::Forbidden)
    );
}

async fn decisions_win_once_under_concurrency(b: &Backend) {
    let args = json!({"title": "PRD"});
    let item = b
        .stores
        .approvals
        .create(new_item("ana", &args), now())
        .await
        .unwrap();
    let mut handles = Vec::new();
    for i in 0..10 {
        let approvals = b.stores.approvals.clone();
        let id = item.id.clone();
        let decision = if i % 2 == 0 {
            Decision::Approve
        } else {
            Decision::Reject
        };
        handles.push(tokio::spawn(async move {
            approvals.decide(&id, "ana", decision, None, now()).await
        }));
    }
    let mut won = 0;
    for h in handles {
        match h.await.unwrap() {
            Ok(_) => won += 1,
            Err(e) => assert_eq!(e, DecideError::NotPending),
        }
    }
    assert_eq!(won, 1, "exactly one concurrent decision may win");
}

async fn completion_records_results_and_failures(b: &Backend) {
    let a = b.stores.approvals.as_ref();
    let args = json!({"title": "PRD"});
    let ok = a.create(new_item("ana", &args), now()).await.unwrap();
    a.decide(&ok.id, "ana", Decision::Approve, None, now())
        .await
        .unwrap();
    let done = a
        .complete(&ok.id, Ok(json!({"status": "done", "url": "https://x/1"})))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(done.status, Status::Approved);
    assert_eq!(done.result.unwrap()["url"], "https://x/1");

    let bad = a.create(new_item("ana", &args), now()).await.unwrap();
    a.decide(&bad.id, "ana", Decision::Approve, None, now())
        .await
        .unwrap();
    let failed = a
        .complete(&bad.id, Err("space not found".into()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(failed.status, Status::Failed);
    assert_eq!(failed.result.unwrap()["error"], "space not found");

    assert!(a
        .complete("apr_missing", Ok(json!({})))
        .await
        .unwrap()
        .is_none());
}

// ----- memory -----------------------------------------------------------------------------

async fn memory_is_per_user(b: &Backend) {
    let m = b.stores.memory.as_ref();
    m.update("ana", "areas", "CCM", now()).await.unwrap();
    m.update("ana", "areas", "CCM, DSM", now() + Duration::minutes(1))
        .await
        .unwrap();
    m.update("ana", "style", "short", now()).await.unwrap();
    m.update("bo", "areas", "DSM", now()).await.unwrap();
    let ana = m.read("ana").await.unwrap();
    let keys: Vec<_> = ana.iter().map(|n| n.key.as_str()).collect();
    assert_eq!(keys, vec!["areas", "style"]);
    assert_eq!(ana[0].text, "CCM, DSM");
    assert_eq!(ana[0].updated_at, now() + Duration::minutes(1));
    assert!(m.read("cy").await.unwrap().is_empty());
}

// ----- context ----------------------------------------------------------------------------

async fn context_search_and_get(b: &Backend) {
    let c = b.stores.context.as_ref();
    let hits = c.search("key management", None, 10).await.unwrap();
    let paths: Vec<_> = hits.iter().map(|h| h.doc.path.as_str()).collect();
    assert_eq!(paths.len(), 2, "{paths:?}");
    assert!(paths.contains(&"context/products/ccm.md"));
    assert!(paths.contains(&"context/competitors/acme.md"));
    assert_eq!(
        paths[0], "context/competitors/acme.md",
        "a title match ranks first"
    );
    assert!(hits.iter().all(|h| h.score > 0.0));
    assert!(hits[0].snippet.to_lowercase().contains("key management"));

    let only = c.search("key", Some("products"), 10).await.unwrap();
    assert_eq!(only.len(), 1);
    assert_eq!(only[0].doc.domain, "products");

    assert_eq!(c.search("key management", None, 1).await.unwrap().len(), 1);
    assert!(c.search("", None, 10).await.unwrap().is_empty());
    assert!(c.search("zebra", None, 10).await.unwrap().is_empty());
    // Query syntax characters are just separators, never operators.
    let odd = c.search("key & | ! ( :* roadmap", None, 10).await.unwrap();
    assert!(!odd.is_empty());

    let nda = c.get("context/partners/nda.md").await.unwrap().unwrap();
    assert_eq!(nda.sensitivity, Sensitivity::Restricted);
    assert!(nda.body.contains("under NDA"));
    assert!(c.get("context/partners/none.md").await.unwrap().is_none());
}

async fn reindex_tracks_the_content_repo(b: &Backend) {
    // One file edited, one deleted, one added.
    let mut docs = library();
    docs.retain(|d| d.path != "context/partners/nda.md");
    docs[0].body = "# CCM\n\nNow with quantum-safe keys.\n".into();
    docs.push(doc(
        "context/glossary/terms.md",
        "Glossary",
        Sensitivity::Public,
        "# Glossary\n\nTEE: trusted execution environment.\n",
    ));
    let report = b.stores.context.reindex(docs).await.unwrap();
    assert_eq!(report.indexed, 3);
    assert_eq!(report.removed, 1);

    let c = b.stores.context.as_ref();
    assert!(c.get("context/partners/nda.md").await.unwrap().is_none());
    let quantum = c.search("quantum", None, 5).await.unwrap();
    assert_eq!(quantum[0].doc.path, "context/products/ccm.md");
    let trusted = c.search("trusted", None, 5).await.unwrap();
    assert_eq!(trusted[0].doc.path, "context/glossary/terms.md");
    assert!(c.search("roadmap", None, 5).await.unwrap().is_empty());
}

// ----- audit ------------------------------------------------------------------------------

fn event(outcome: Outcome) -> AuditEvent {
    AuditEvent {
        client: pmos_toolserver::audit::Client::Mcp,
        at: now(),
        run_id: "run-a".into(),
        user: "ana".into(),
        skill: "prd-draft".into(),
        tool: "confluence.create_draft".into(),
        outcome,
        sources: vec!["context/products/ccm.md".into(), "jira:PM-101".into()],
        args_hash: Some("abc".into()),
        output_ref: Some("apr_1".into()),
        detail: None,
    }
}

async fn audit_records_events(b: &Backend) {
    b.stores
        .audit
        .record(event(Outcome::Pending))
        .await
        .unwrap();
    b.stores
        .audit
        .record(event(Outcome::Approved))
        .await
        .unwrap();
    if let Some(mem) = &b.memory_audit {
        let outcomes: Vec<_> = mem.events().into_iter().map(|e| e.outcome).collect();
        assert_eq!(outcomes, vec![Outcome::Pending, Outcome::Approved]);
    }
    if let Some(pg) = &b.pg {
        let rows = sqlx::query("SELECT outcome, sources FROM audit_events ORDER BY id")
            .fetch_all(pg.pool())
            .await
            .unwrap();
        let outcomes: Vec<String> = rows.iter().map(|r| r.get("outcome")).collect();
        assert_eq!(outcomes, vec!["pending", "approved"]);
        let sources: Vec<String> = rows[0].get("sources");
        assert_eq!(sources, vec!["context/products/ccm.md", "jira:PM-101"]);
    }
}

// ----- credentials ------------------------------------------------------------------------

fn sealed(user: &str, provider: Provider, token: &str) -> SealedCredential {
    SealedCredential {
        user: user.into(),
        provider,
        key_id: "k1".into(),
        access_token: format!("v1.k1.sealed-{token}"),
        refresh_token: Some(format!("v1.k1.sealed-r-{token}")),
        expires_at: Some(now()),
        scopes: "read:jira-work".into(),
        cloud_id: Some("cloud-1".into()),
        site_url: Some("https://example.atlassian.net".into()),
        external_id: None,
        updated_at: now(),
    }
}

async fn credentials_are_per_user_and_provider(b: &Backend) {
    let c = b.stores.credentials.as_ref();
    c.put(&sealed("ana", Provider::Atlassian, "a1"))
        .await
        .unwrap();
    c.put(&sealed("ana", Provider::Slack, "s1")).await.unwrap();
    c.put(&sealed("bo", Provider::Atlassian, "b1"))
        .await
        .unwrap();

    let got = c.get("ana", Provider::Atlassian).await.unwrap().unwrap();
    assert_eq!(got, sealed("ana", Provider::Atlassian, "a1"));
    assert!(c.get("cy", Provider::Atlassian).await.unwrap().is_none());

    // Reconnecting replaces the row.
    c.put(&sealed("ana", Provider::Atlassian, "a2"))
        .await
        .unwrap();
    let got = c.get("ana", Provider::Atlassian).await.unwrap().unwrap();
    assert_eq!(got.access_token, "v1.k1.sealed-a2");

    let providers: Vec<_> = c
        .list("ana")
        .await
        .unwrap()
        .into_iter()
        .map(|x| x.provider)
        .collect();
    assert_eq!(providers, vec![Provider::Atlassian, Provider::Slack]);

    assert!(c.delete("ana", Provider::Slack).await.unwrap());
    assert!(!c.delete("ana", Provider::Slack).await.unwrap());
    assert_eq!(c.list("ana").await.unwrap().len(), 1);
    assert!(c.get("bo", Provider::Atlassian).await.unwrap().is_some());
}

async fn credential_replace_is_conditional(b: &Backend) {
    let c = b.stores.credentials.as_ref();
    let first = sealed("ana", Provider::Atlassian, "a1");
    c.put(&first).await.unwrap();
    let stored = c.get("ana", Provider::Atlassian).await.unwrap().unwrap();

    // Replacing the version that was read works once; the stale second writer loses.
    let newer = SealedCredential {
        access_token: "v1.k1.sealed-a2".into(),
        updated_at: stored.updated_at + Duration::seconds(5),
        ..stored.clone()
    };
    assert!(c.replace(&newer, stored.updated_at).await.unwrap());
    let stale = SealedCredential {
        access_token: "v1.k1.sealed-stale".into(),
        ..stored.clone()
    };
    assert!(!c.replace(&stale, stored.updated_at).await.unwrap());
    let got = c.get("ana", Provider::Atlassian).await.unwrap().unwrap();
    assert_eq!(got.access_token, "v1.k1.sealed-a2");

    // Replace never inserts: an unlinked account stays unlinked.
    assert!(c.delete("ana", Provider::Atlassian).await.unwrap());
    assert!(!c.replace(&newer, newer.updated_at).await.unwrap());
    assert!(c.get("ana", Provider::Atlassian).await.unwrap().is_none());
}

async fn vault_round_trips_through_the_store(b: &Backend) {
    let vault = Vault::new(
        KeyRing::from_spec(&KeyRing::generate_spec("k1")).unwrap(),
        b.stores.credentials.clone(),
    );
    let cred = Credential {
        user: "ana".into(),
        provider: Provider::Slack,
        access_token: Secret::new("xoxp-plaintext-token"),
        refresh_token: None,
        expires_at: None,
        scopes: "chat:write search:read.public".into(),
        cloud_id: None,
        site_url: None,
        external_id: Some("U0ANA".into()),
        updated_at: now(),
    };
    vault.put(&cred).await.unwrap();
    assert_eq!(vault.get("ana", Provider::Slack).await.unwrap(), Some(cred));
    let raw = b
        .stores
        .credentials
        .get("ana", Provider::Slack)
        .await
        .unwrap()
        .unwrap();
    assert!(!raw.access_token.contains("xoxp"));
    if let Some(pg) = &b.pg {
        let hits: i64 =
            sqlx::query_scalar("SELECT count(*) FROM credentials WHERE access_token LIKE '%xoxp%'")
                .fetch_one(pg.pool())
                .await
                .unwrap();
        assert_eq!(hits, 0, "no plaintext token in the database");
    }
}

// ----- Postgres only ----------------------------------------------------------------------

mod postgres_only {
    use super::*;

    #[tokio::test]
    async fn sessions_keep_only_token_hashes() {
        let Some(b) = Backend::postgres(Vec::new()).await else {
            return;
        };
        let s = Session::new("ana", "ask", "r1", Duration::minutes(30), now());
        b.stores.sessions.insert(&s, now()).await.unwrap();
        let pg = b.pg.as_ref().unwrap();
        let stored: Vec<String> = sqlx::query("SELECT token_hash FROM sessions")
            .fetch_all(pg.pool())
            .await
            .unwrap()
            .iter()
            .map(|r| r.get("token_hash"))
            .collect();
        assert_eq!(stored, vec![token_hash(&s.token)]);
        assert!(!stored.contains(&s.token));
        b.finish().await;
    }

    #[tokio::test]
    async fn audit_log_is_append_only() {
        let Some(b) = Backend::postgres(Vec::new()).await else {
            return;
        };
        b.stores.audit.record(event(Outcome::Ok)).await.unwrap();
        let pg = b.pg.as_ref().unwrap();
        let update = sqlx::query("UPDATE audit_events SET outcome = 'denied'")
            .execute(pg.pool())
            .await;
        assert!(update.unwrap_err().to_string().contains("append-only"));
        let delete = sqlx::query("DELETE FROM audit_events")
            .execute(pg.pool())
            .await;
        assert!(delete.unwrap_err().to_string().contains("append-only"));
        b.finish().await;
    }

    #[tokio::test]
    async fn prune_drops_only_expired_sessions_and_grants() {
        let Some(b) = Backend::postgres(Vec::new()).await else {
            return;
        };
        let old = Session::new("ana", "ask", "r1", Duration::minutes(5), now());
        let live = Session::new("ana", "ask", "r2", Duration::minutes(60), now());
        b.stores.sessions.insert(&old, now()).await.unwrap();
        b.stores.sessions.insert(&live, now()).await.unwrap();
        let args = json!({"title": "PRD"});
        b.stores
            .approvals
            .add_grant(
                "k-old",
                "confluence.create_draft",
                &args,
                now() - Duration::minutes(30),
            )
            .await
            .unwrap();
        b.stores
            .approvals
            .add_grant("k-live", "confluence.create_draft", &args, now())
            .await
            .unwrap();

        let pg = b.pg.as_ref().unwrap();
        pg.prune(now() + Duration::minutes(10)).await.unwrap();
        let sessions: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions")
            .fetch_one(pg.pool())
            .await
            .unwrap();
        let grants: Vec<String> = sqlx::query_scalar("SELECT session_key FROM grants")
            .fetch_all(pg.pool())
            .await
            .unwrap();
        assert_eq!(sessions, 1);
        assert_eq!(grants, vec!["k-live"]);
        b.finish().await;
    }

    #[tokio::test]
    async fn migrations_are_idempotent() {
        let Some(b) = Backend::postgres(Vec::new()).await else {
            return;
        };
        b.pg.as_ref().unwrap().migrate().await.unwrap();
        b.finish().await;
    }
}
