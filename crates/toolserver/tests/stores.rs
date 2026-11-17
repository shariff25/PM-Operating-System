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
    DecideError, Decision, MemoryApprovals, NewItem, Status, Verdict, GRANT_TTL_MINUTES,
};
use pmos_toolserver::audit::{AuditEvent, MemoryAudit, Outcome};
use pmos_toolserver::context::{ContextDoc, MemoryIndex};
use pmos_toolserver::ledger::{LedgerOutcome, LedgerQuery, LedgerRow, Reason};
use pmos_toolserver::members::{
    Created, Decided, MemoryRequests, NewRequest, RequestStatus, MAX_PENDING,
};
use pmos_toolserver::memory::InMemoryStore;
use pmos_toolserver::memory_repo::{MemoryRepo, MemoryRepos};
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
            "context/products/cloud.md",
            "Acme Cloud",
            Sensitivity::Internal,
            "# Acme Cloud\n\nKey management for multi-cloud estates.\n",
        ),
        doc(
            "context/competitors/initech.md",
            "Initech key management",
            Sensitivity::Internal,
            "# Initech\n\nInitech sells key management appliances.\n",
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
                memory_repos: Arc::new(MemoryRepos::default()),
                member_requests: Arc::new(MemoryRequests::default()),
                users: Arc::new(pmos_toolserver::users::MemoryUsers::default()),
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
    session_sensitivity_only_rises,
    bot_sessions_keep_their_bot,
    approval_items_and_audit_events_keep_their_bot,
    grants_match_one_exact_call_once,
    grants_are_taken_once_under_concurrency,
    approvals_list_newest_first_and_filter,
    decisions_follow_the_rules,
    decisions_win_once_under_concurrency,
    completion_records_results_and_failures,
    interrupted_writes_are_found,
    replies_are_drafted_once_per_thread,
    slack_drafts_expire_and_lose_their_text,
    ledger_rows_record_every_outcome,
    ledger_reads_filter_and_order,
    ledger_rows_follow_concurrent_decisions,
    memory_is_per_user,
    notes_keep_their_sensitivity,
    context_search_and_get,
    context_search_applies_the_ceiling_in_the_store,
    reindex_tracks_the_content_repo,
    audit_records_events,
    credentials_are_per_user_and_provider,
    credential_replace_is_conditional,
    vault_round_trips_through_the_store,
    github_credentials_are_stored,
    memory_repos_are_per_user,
    member_requests_follow_the_rules,
    member_requests_are_decided_once_under_concurrency,
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

async fn session_sensitivity_only_rises(b: &Backend) {
    let s = Session::new("ana", "ask", "r1", Duration::minutes(30), now());
    b.stores.sessions.insert(&s, now()).await.unwrap();
    let read = || async {
        b.stores
            .sessions
            .get(&s.token, now())
            .await
            .unwrap()
            .unwrap()
            .sources_sensitivity
    };
    assert_eq!(read().await, Sensitivity::Public);
    for (raise, want) in [
        (Sensitivity::Internal, Sensitivity::Internal),
        (Sensitivity::Public, Sensitivity::Internal),
        (Sensitivity::Restricted, Sensitivity::Restricted),
        (Sensitivity::Internal, Sensitivity::Restricted),
    ] {
        b.stores
            .sessions
            .raise_sensitivity(&s.token, raise)
            .await
            .unwrap();
        assert_eq!(read().await, want, "after {raise}");
    }
    // An unknown token is a no-op.
    b.stores
        .sessions
        .raise_sensitivity("pmos_s_unknown", Sensitivity::Restricted)
        .await
        .unwrap();
}

async fn bot_sessions_keep_their_bot(b: &Backend) {
    let bot = Session::for_bot("ana", "scout", "tsk_1", Duration::minutes(30), now());
    let skill = Session::new("ana", "ask", "r1", Duration::minutes(30), now());
    b.stores.sessions.insert(&bot, now()).await.unwrap();
    b.stores.sessions.insert(&skill, now()).await.unwrap();
    let got = b
        .stores
        .sessions
        .get(&bot.token, now())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(got, bot);
    assert_eq!(got.bot.as_deref(), Some("scout"));
    assert_eq!(got.skill, None);
    let got = b
        .stores
        .sessions
        .get(&skill.token, now())
        .await
        .unwrap()
        .unwrap();
    assert_eq!((got.skill.as_deref(), got.bot), (Some("ask"), None));
    assert!(b.stores.sessions.revoke(&bot.token).await.unwrap());
}

async fn approval_items_and_audit_events_keep_their_bot(b: &Backend) {
    let a = b.stores.approvals.as_ref();
    let args = json!({"project": "PM", "issue_type": "Task", "summary": "x"});
    let item = a
        .create(
            NewItem {
                user: "ana",
                skill: None,
                bot: Some("scout"),
                run_id: "tsk_1",
                tool: "jira.create_issue",
                args: &args,
                content_sha: None,
                model_alias: None,
                sources_sensitivity: Sensitivity::Internal,
            },
            now(),
        )
        .await
        .unwrap();
    assert_eq!(item.bot.as_deref(), Some("scout"));
    assert_eq!(a.get(&item.id).await.unwrap().unwrap(), item);
    assert_eq!(a.list("ana", None).await.unwrap(), vec![item.clone()]);
    let decided = a
        .decide(&item.id, "ana", Decision::Approve, None, now())
        .await
        .unwrap();
    assert_eq!(
        (decided.bot.as_deref(), decided.skill),
        (Some("scout"), None)
    );

    b.stores
        .audit
        .record(AuditEvent {
            skill: None,
            bot: Some("scout".into()),
            ..event(Outcome::Denied)
        })
        .await
        .unwrap();
    if let Some(mem) = &b.memory_audit {
        let e = mem.events().pop().unwrap();
        assert_eq!((e.skill, e.bot.as_deref()), (None, Some("scout")));
    }
    if let Some(pg) = &b.pg {
        let row = sqlx::query("SELECT skill, bot FROM audit_events")
            .fetch_one(pg.pool())
            .await
            .unwrap();
        assert_eq!(row.get::<Option<String>, _>("skill"), None);
        assert_eq!(
            row.get::<Option<String>, _>("bot").as_deref(),
            Some("scout")
        );
    }
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
        skill: Some("prd-draft"),
        bot: None,
        run_id: "r1",
        tool: "confluence.create_draft",
        args,
        content_sha: Some("0123456789abcdef0123456789abcdef01234567"),
        model_alias: Some("pmos/default"),
        sources_sensitivity: Sensitivity::Internal,
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

    // A listing is capped at the newest LIST_LIMIT, in a fixed order.
    let limit = pmos_toolserver::approvals::LIST_LIMIT;
    for i in 0..=limit {
        a.create(new_item("dee", &args), now() + Duration::seconds(i as i64))
            .await
            .unwrap();
    }
    let listed = a.list("dee", None).await.unwrap();
    assert_eq!(listed.len(), limit);
    assert_eq!(
        listed[0].created_at,
        now() + Duration::seconds(limit as i64)
    );
    assert!(listed.windows(2).all(|w| w[0].created_at > w[1].created_at));
    assert_eq!(
        listed.last().unwrap().created_at,
        now() + Duration::seconds(1),
        "the oldest is the one left out"
    );

    // R12: the inbox never hides a pending item behind that cap. With every one of dee's
    // `LIST_LIMIT + 1` items pending, and three of them then decided, the inbox has every
    // pending one, newest first, and the decided ones apart.
    let inbox = a.inbox("dee", 10_000, 2).await.unwrap();
    assert_eq!(inbox.pending.len(), limit + 1);
    assert_eq!(inbox.total_pending, limit + 1);
    assert!(!inbox.truncated());
    let oldest = inbox.pending.last().unwrap().clone();
    assert_eq!(
        oldest.created_at,
        now(),
        "the oldest pending draft is there"
    );
    for item in inbox.pending.iter().take(3) {
        a.decide(&item.id, "dee", Decision::Reject, None, now())
            .await
            .unwrap();
    }
    let inbox = a.inbox("dee", 10_000, 2).await.unwrap();
    assert_eq!(inbox.pending.len(), limit - 2);
    assert!(inbox.pending.iter().all(|i| i.status == Status::Pending));
    assert!(inbox
        .pending
        .windows(2)
        .all(|w| w[0].created_at > w[1].created_at));
    assert_eq!(inbox.decided.len(), 2, "decided items are capped");
    assert!(inbox.decided.iter().all(|i| i.status == Status::Rejected));
    // Past the safety cap: truncated, with the total.
    let inbox = a.inbox("dee", 5, 0).await.unwrap();
    assert_eq!(inbox.pending.len(), 5);
    assert_eq!(inbox.total_pending, limit - 2);
    assert!(inbox.truncated());
    assert!(inbox.decided.is_empty());
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
    // Approved and running: `executing` until the write's outcome is stored.
    assert_eq!(decided.status, Status::Executing);
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
    // Only an executing item takes an outcome: a pending one, or one already completed,
    // is left alone.
    let pending = a.create(new_item("ana", &args), now()).await.unwrap();
    assert!(a
        .complete(&pending.id, Ok(json!({})))
        .await
        .unwrap()
        .is_none());
    assert!(a
        .complete(&ok.id, Err("late".into()))
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        a.get(&ok.id).await.unwrap().unwrap().status,
        Status::Approved
    );
}

async fn interrupted_writes_are_found(b: &Backend) {
    let a = b.stores.approvals.as_ref();
    let args = json!({"title": "PRD"});
    let early = a.create(new_item("ana", &args), now()).await.unwrap();
    a.decide(&early.id, "ana", Decision::Approve, None, now())
        .await
        .unwrap();
    let late = a.create(new_item("ana", &args), now()).await.unwrap();
    a.decide(
        &late.id,
        "ana",
        Decision::Approve,
        None,
        now() + Duration::minutes(20),
    )
    .await
    .unwrap();
    let done = a.create(new_item("ana", &args), now()).await.unwrap();
    a.decide(&done.id, "ana", Decision::Approve, None, now())
        .await
        .unwrap();
    a.complete(&done.id, Ok(json!({}))).await.unwrap();

    let stuck: Vec<_> = a
        .stuck(now() + Duration::minutes(10))
        .await
        .unwrap()
        .into_iter()
        .map(|i| i.id)
        .collect();
    assert_eq!(stuck, vec![early.id]);
}

// ----- Slack drafts (docs/slack-reply-spec.md §3) -----------------------------------------

fn slack_item<'a>(tool: &'a str, args: &'a Value) -> NewItem<'a> {
    NewItem {
        user: "ana",
        skill: None,
        bot: Some("pm-ana"),
        run_id: "tsk_1",
        tool,
        args,
        content_sha: None,
        model_alias: Some("pmos/default"),
        sources_sensitivity: Sensitivity::Restricted,
    }
}

async fn replies_are_drafted_once_per_thread(b: &Backend) {
    use pmos_toolserver::approvals::Drafted;
    let a = b.stores.approvals.as_ref();
    let args = json!({"channel": "C0PMTEAM", "thread_ts": "1731600000.000100", "text": "Yes"});
    let since = now() - Duration::days(7);
    let Drafted::New(first) = a
        .create_reply(slack_item("slack.post_reply", &args), since, now())
        .await
        .unwrap()
    else {
        panic!("the first draft is new");
    };
    assert_eq!(first.sources_sensitivity, Some(Sensitivity::Restricted));
    let other = json!({"channel": "C0PMTEAM", "thread_ts": "1731600000.000100", "text": "No"});
    assert_eq!(
        a.create_reply(slack_item("slack.post_reply", &other), since, now())
            .await
            .unwrap(),
        Drafted::Existing(first.clone())
    );
    // Older than the window: doesn't count.
    let later = now() + Duration::days(8);
    assert!(matches!(
        a.create_reply(
            slack_item("slack.post_reply", &other),
            later - Duration::days(7),
            later
        )
        .await
        .unwrap(),
        Drafted::New(_)
    ));
    // Arguments with no thread are created as usual.
    let no_thread = json!({"channel": "C0PMTEAM", "text": "Hi"});
    for _ in 0..2 {
        assert!(matches!(
            a.create_reply(slack_item("slack.post_reply", &no_thread), since, now())
                .await
                .unwrap(),
            Drafted::New(_)
        ));
    }

    // Concurrent drafts for one thread: one is created.
    let race = json!({"channel": "C0FIELD", "thread_ts": "1731601200.000600", "text": "x"});
    let mut handles = Vec::new();
    for _ in 0..8 {
        let approvals = b.stores.approvals.clone();
        let race = race.clone();
        handles.push(tokio::spawn(async move {
            approvals
                .create_reply(slack_item("slack.post_reply", &race), since, now())
                .await
                .unwrap()
        }));
    }
    let mut created = 0;
    for h in handles {
        if matches!(h.await.unwrap(), Drafted::New(_)) {
            created += 1;
        }
    }
    assert_eq!(created, 1);
}

async fn slack_drafts_expire_and_lose_their_text(b: &Backend) {
    use pmos_toolserver::approvals::{EXPIRED_TEXT, SLACK_DRAFT_TOOLS};
    let a = b.stores.approvals.as_ref();
    let reply = json!({"channel": "C0PMTEAM", "thread_ts": "1731600000.000100", "text": "Yes"});
    let message = json!({"channel": "C0PMTEAM", "text": "Standup moved"});
    let issue = json!({"summary": "SCIM"});
    let old = a
        .create(slack_item("slack.post_reply", &reply), now())
        .await
        .unwrap();
    let sent = a
        .create(slack_item("slack.send_message", &message), now())
        .await
        .unwrap();
    let young = a
        .create(
            slack_item("slack.post_reply", &reply),
            now() + Duration::hours(2),
        )
        .await
        .unwrap();
    let jira = a
        .create(slack_item("jira.create_issue", &issue), now())
        .await
        .unwrap();
    let decided = a
        .create(slack_item("slack.post_reply", &reply), now())
        .await
        .unwrap();
    a.decide(&decided.id, "ana", Decision::Reject, None, now())
        .await
        .unwrap();

    let at = now() + Duration::hours(24);
    let expired = a
        .expire(&SLACK_DRAFT_TOOLS, now() + Duration::hours(1), at)
        .await
        .unwrap();
    // Oldest first; these two were created at the same time, so by id.
    let ids: Vec<&str> = expired.iter().map(|i| i.id.as_str()).collect();
    let mut want = vec![old.id.as_str(), sent.id.as_str()];
    want.sort();
    assert_eq!(ids, want);
    assert!(expired.iter().all(|i| i.status == Status::Expired));
    let stored = a.get(&old.id).await.unwrap().unwrap();
    assert_eq!(stored.status, Status::Expired);
    assert_eq!(
        stored.args,
        json!({"channel": "C0PMTEAM", "thread_ts": "1731600000.000100", "text": EXPIRED_TEXT})
    );
    assert_eq!(stored.args_hash, old.args_hash);
    assert_eq!(stored.redacted_at, Some(at));
    assert_eq!(
        a.get(&sent.id).await.unwrap().unwrap().args,
        json!({"channel": "C0PMTEAM", "text": EXPIRED_TEXT})
    );
    for kept in [&young.id, &jira.id] {
        assert_eq!(a.get(kept).await.unwrap().unwrap().status, Status::Pending);
    }
    assert!(a
        .expire(&SLACK_DRAFT_TOOLS, now() + Duration::hours(1), at)
        .await
        .unwrap()
        .is_empty());

    // An expired item can't be decided, by its user; anyone else is still forbidden.
    assert_eq!(
        a.decide(&old.id, "ana", Decision::Approve, None, at).await,
        Err(DecideError::Expired)
    );
    assert_eq!(
        a.decide(&old.id, "bo", Decision::Approve, None, at).await,
        Err(DecideError::Forbidden)
    );

    // The ledger stores it, with no decision.
    let rows = a
        .ledger(&LedgerQuery::all(now() - Duration::days(30)))
        .await
        .unwrap();
    let row = ledger_row(&rows, &old.id);
    assert_eq!(row.outcome, LedgerOutcome::Expired);
    assert!(row.decided_at.is_none());
    assert_eq!(ledger_row(&rows, &jira.id).outcome, LedgerOutcome::Pending);
    let only = a
        .ledger(&LedgerQuery {
            outcomes: vec![LedgerOutcome::Expired],
            ..LedgerQuery::all(now() - Duration::days(30))
        })
        .await
        .unwrap();
    assert_eq!(only.len(), 2);

    // Decided items lose their text once, after `before`; pending and expired ones aren't
    // touched by this.
    assert_eq!(
        a.redact_decided(&SLACK_DRAFT_TOOLS, now() - Duration::seconds(1), at)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        a.redact_decided(&SLACK_DRAFT_TOOLS, now(), at)
            .await
            .unwrap(),
        1
    );
    let redacted = a.get(&decided.id).await.unwrap().unwrap();
    assert_eq!(redacted.status, Status::Rejected);
    assert_eq!(redacted.args["text"], EXPIRED_TEXT);
    assert_eq!(redacted.redacted_at, Some(at));
    assert_eq!(
        a.redact_decided(&SLACK_DRAFT_TOOLS, at, at).await.unwrap(),
        0,
        "once"
    );
    assert_eq!(a.get(&young.id).await.unwrap().unwrap().args["text"], "Yes");
}

// ----- the feedback ledger ----------------------------------------------------------------

fn bot_item<'a>(bot: &'a str, args: &'a Value) -> NewItem<'a> {
    NewItem {
        user: "ana",
        skill: None,
        bot: Some(bot),
        run_id: "tsk_1",
        tool: "jira.create_issue",
        args,
        content_sha: None,
        model_alias: Some("pmos/extraction"),
        sources_sensitivity: Sensitivity::Internal,
    }
}

fn ledger_row(rows: &[LedgerRow], id: &str) -> LedgerRow {
    rows.iter()
        .find(|r| r.id == id)
        .cloned()
        .unwrap_or_else(|| panic!("no ledger row for {id}"))
}

async fn ledger_rows_record_every_outcome(b: &Backend) {
    let a = b.stores.approvals.as_ref();
    let args = json!({"title": "Release notes", "body": "Fixed the login bug."});
    let decided = now() + Duration::minutes(5);

    let unchanged = a.create(new_item("ana", &args), now()).await.unwrap();
    a.decide(&unchanged.id, "ana", Decision::Approve, None, decided)
        .await
        .unwrap();
    a.complete(&unchanged.id, Ok(json!({"status": "done"})))
        .await
        .unwrap();

    let edited = a.create(new_item("ana", &args), now()).await.unwrap();
    let final_args = json!({"title": "Release notes", "body": "Fixed the logout bug."});
    a.decide(
        &edited.id,
        "ana",
        Decision::Edit,
        Some(final_args.clone()),
        decided,
    )
    .await
    .unwrap();
    a.complete(&edited.id, Ok(json!({"status": "done"})))
        .await
        .unwrap();

    // Edited back to exactly what was drafted: approved unchanged.
    let same = a.create(new_item("ana", &args), now()).await.unwrap();
    a.decide(&same.id, "ana", Decision::Edit, Some(args.clone()), decided)
        .await
        .unwrap();
    a.complete(&same.id, Ok(json!({}))).await.unwrap();

    let rejected = a.create(new_item("ana", &args), now()).await.unwrap();
    a.decide_with(
        &rejected.id,
        "ana",
        Verdict {
            decision: Decision::Reject,
            args: None,
            reason: Some(Reason::WrongFacts),
            confirm_external: false,
            expected_args_hash: None,
        },
        decided,
    )
    .await
    .unwrap();
    let bare_reject = a.create(new_item("ana", &args), now()).await.unwrap();
    a.decide(&bare_reject.id, "ana", Decision::Reject, None, decided)
        .await
        .unwrap();
    // A reason with an approval is not kept (the service refuses it before this).
    let approved_with_reason = a.create(new_item("ana", &args), now()).await.unwrap();
    a.decide_with(
        &approved_with_reason.id,
        "ana",
        Verdict {
            decision: Decision::Approve,
            args: None,
            reason: Some(Reason::Tone),
            confirm_external: false,
            expected_args_hash: None,
        },
        decided,
    )
    .await
    .unwrap();

    let failed = a.create(new_item("ana", &args), now()).await.unwrap();
    a.decide(&failed.id, "ana", Decision::Approve, None, decided)
        .await
        .unwrap();
    a.complete(&failed.id, Err("space not found".into()))
        .await
        .unwrap();

    let old = a
        .create(new_item("ana", &args), now() - Duration::days(8))
        .await
        .unwrap();
    let fresh = a.create(new_item("ana", &args), now()).await.unwrap();
    // A failed decision changes nothing.
    assert_eq!(
        a.decide(&fresh.id, "bo", Decision::Reject, None, decided)
            .await,
        Err(DecideError::Forbidden)
    );

    let rows = a
        .ledger(&LedgerQuery::all(now() - Duration::days(7)))
        .await
        .unwrap();
    assert_eq!(rows.len(), 9, "one row per item");

    let r = ledger_row(&rows, &unchanged.id);
    assert_eq!(r.outcome, LedgerOutcome::Approved);
    assert_eq!(r.edit_distance, Some(0.0));
    assert_eq!(r.reason, None);
    assert_eq!(r.decided_at, Some(decided));
    assert_eq!(r.decided_by.as_deref(), Some("ana"));
    assert_eq!(r.created_at, now());
    assert_eq!(r.user, "ana");
    assert_eq!(r.run_id, "r1");
    assert_eq!(r.skill.as_deref(), Some("prd-draft"));
    assert_eq!(r.bot, None);
    assert_eq!(r.tool, "confluence.create_draft");
    assert_eq!(
        r.content_sha.as_deref(),
        Some("0123456789abcdef0123456789abcdef01234567")
    );
    assert_eq!(r.model_alias.as_deref(), Some("pmos/default"));

    let r = ledger_row(&rows, &edited.id);
    assert_eq!(r.outcome, LedgerOutcome::Edited);
    let expected = pmos_toolserver::ledger::args_edit_distance(&args, &final_args);
    assert!(expected > 0.0 && expected < 0.2, "{expected}");
    assert!((r.edit_distance.unwrap() - expected).abs() < 1e-9);

    let r = ledger_row(&rows, &same.id);
    assert_eq!(r.outcome, LedgerOutcome::Approved);
    assert_eq!(r.edit_distance, Some(0.0));

    let r = ledger_row(&rows, &rejected.id);
    assert_eq!(r.outcome, LedgerOutcome::Rejected);
    assert_eq!(r.reason, Some(Reason::WrongFacts));
    assert_eq!(r.edit_distance, None);
    assert_eq!(r.decided_by.as_deref(), Some("ana"));

    let r = ledger_row(&rows, &bare_reject.id);
    assert_eq!(r.outcome, LedgerOutcome::Rejected);
    assert_eq!(r.reason, None);

    let r = ledger_row(&rows, &approved_with_reason.id);
    assert_eq!(r.outcome, LedgerOutcome::Pending, "still writing");
    assert_eq!(r.reason, None);
    assert_eq!(r.edit_distance, Some(0.0));

    let r = ledger_row(&rows, &failed.id);
    assert_eq!(r.outcome, LedgerOutcome::Failed);
    assert_eq!(r.edit_distance, Some(0.0));

    let r = ledger_row(&rows, &old.id);
    assert_eq!(r.outcome, LedgerOutcome::Expired);
    assert_eq!(r.decided_at, None);
    // Expiry is worked out on read: with a longer window the same row is pending.
    let longer = a
        .ledger(&LedgerQuery::all(now() - Duration::days(30)))
        .await
        .unwrap();
    assert_eq!(ledger_row(&longer, &old.id).outcome, LedgerOutcome::Pending);

    let r = ledger_row(&rows, &fresh.id);
    assert_eq!(r.outcome, LedgerOutcome::Pending);
    assert_eq!(r.decided_at, None);
    assert_eq!(r.decided_by, None);
}

async fn ledger_reads_filter_and_order(b: &Backend) {
    let a = b.stores.approvals.as_ref();
    let args = json!({"summary": "Login fails on Safari", "project": "PM"});
    let t = |m: i64| now() + Duration::minutes(m);

    // docs: rejected at +30, edited heavily at +20, approved at +10, pending since +5.
    let rejected = a.create(bot_item("docs", &args), t(0)).await.unwrap();
    a.decide_with(
        &rejected.id,
        "ana",
        Verdict {
            decision: Decision::Reject,
            args: None,
            reason: Some(Reason::Tone),
            confirm_external: false,
            expected_args_hash: None,
        },
        t(30),
    )
    .await
    .unwrap();
    let heavy = a.create(bot_item("docs", &args), t(1)).await.unwrap();
    a.decide(
        &heavy.id,
        "ana",
        Decision::Edit,
        Some(json!({"summary": "Checkout crashes", "project": "PM"})),
        t(20),
    )
    .await
    .unwrap();
    a.complete(&heavy.id, Ok(json!({}))).await.unwrap();
    let approved = a.create(bot_item("docs", &args), t(2)).await.unwrap();
    a.decide(&approved.id, "ana", Decision::Approve, None, t(10))
        .await
        .unwrap();
    a.complete(&approved.id, Ok(json!({}))).await.unwrap();
    let pending = a.create(bot_item("docs", &args), t(5)).await.unwrap();
    // scout: one pending at +3.
    let scout = a.create(bot_item("scout", &args), t(3)).await.unwrap();

    let all = LedgerQuery::all(now() - Duration::days(7));
    let ids = |rows: Vec<LedgerRow>| rows.into_iter().map(|r| r.id).collect::<Vec<_>>();

    // Newest change first: a decision, or the creation of an undecided item.
    assert_eq!(
        ids(a.ledger(&all).await.unwrap()),
        vec![
            rejected.id.clone(),
            heavy.id.clone(),
            approved.id.clone(),
            pending.id.clone(),
            scout.id.clone(),
        ]
    );
    let q = LedgerQuery {
        bot: Some("scout".into()),
        ..all.clone()
    };
    assert_eq!(ids(a.ledger(&q).await.unwrap()), vec![scout.id.clone()]);
    let q = LedgerQuery {
        since: Some(t(5)),
        until: Some(t(30)),
        ..all.clone()
    };
    assert_eq!(
        ids(a.ledger(&q).await.unwrap()),
        vec![heavy.id.clone(), approved.id.clone(), pending.id.clone()],
        "since is inclusive, until exclusive"
    );
    let q = LedgerQuery {
        outcomes: vec![LedgerOutcome::Rejected, LedgerOutcome::Edited],
        ..all.clone()
    };
    assert_eq!(
        ids(a.ledger(&q).await.unwrap()),
        vec![rejected.id.clone(), heavy.id.clone()]
    );
    // The triage digest's read: rejected, or edited by at least 0.3.
    let q = LedgerQuery {
        outcomes: vec![LedgerOutcome::Rejected, LedgerOutcome::Edited],
        min_edit: Some(0.3),
        ..all.clone()
    };
    assert_eq!(
        ids(a.ledger(&q).await.unwrap()),
        vec![rejected.id.clone(), heavy.id.clone()]
    );
    let q = LedgerQuery {
        min_edit: Some(0.99),
        outcomes: vec![LedgerOutcome::Edited, LedgerOutcome::Approved],
        ..all.clone()
    };
    assert!(a.ledger(&q).await.unwrap().is_empty());
    let q = LedgerQuery {
        limit: Some(2),
        ..all.clone()
    };
    assert_eq!(
        ids(a.ledger(&q).await.unwrap()),
        vec![rejected.id.clone(), heavy.id.clone()]
    );
    // Expired is a reported outcome, and filters like one.
    let later = LedgerQuery {
        outcomes: vec![LedgerOutcome::Expired],
        ..LedgerQuery::all(t(4))
    };
    assert_eq!(ids(a.ledger(&later).await.unwrap()), vec![scout.id.clone()]);
    let q = LedgerQuery {
        outcomes: vec![LedgerOutcome::Pending],
        ..LedgerQuery::all(t(4))
    };
    assert_eq!(ids(a.ledger(&q).await.unwrap()), vec![pending.id.clone()]);
}

async fn ledger_rows_follow_concurrent_decisions(b: &Backend) {
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
        handles.push(tokio::spawn(async move {
            let verdict = if i % 2 == 0 {
                Verdict::new(Decision::Approve, None)
            } else {
                Verdict {
                    decision: Decision::Reject,
                    args: None,
                    reason: Some(Reason::Incomplete),
                    confirm_external: false,
                    expected_args_hash: None,
                }
            };
            approvals
                .decide_with(&id, "ana", verdict, now() + Duration::minutes(i))
                .await
        }));
    }
    let mut winners = Vec::new();
    for h in handles {
        if let Ok(item) = h.await.unwrap() {
            winners.push(item);
        }
    }
    assert_eq!(winners.len(), 1);
    let won = &winners[0];
    let rows = b
        .stores
        .approvals
        .ledger(&LedgerQuery::all(now()))
        .await
        .unwrap();
    let row = ledger_row(&rows, &item.id);
    assert_eq!(row.decided_at, won.decided_at, "the winner's decision");
    match won.status {
        Status::Rejected => {
            assert_eq!(row.outcome, LedgerOutcome::Rejected);
            assert_eq!(row.reason, Some(Reason::Incomplete));
        }
        _ => {
            assert_eq!(row.outcome, LedgerOutcome::Pending);
            assert_eq!(row.edit_distance, Some(0.0));
            assert_eq!(row.reason, None);
        }
    }
}

// ----- memory -----------------------------------------------------------------------------

async fn memory_is_per_user(b: &Backend) {
    let m = b.stores.memory.as_ref();
    m.update(
        "ana",
        "areas",
        "Acme Cloud",
        "ask, task r1",
        Sensitivity::Internal,
        now(),
    )
    .await
    .unwrap();
    m.update(
        "ana",
        "areas",
        "Acme Cloud, Acme Vault",
        "ask, task r2",
        Sensitivity::Internal,
        now() + Duration::minutes(1),
    )
    .await
    .unwrap();
    m.update(
        "ana",
        "style",
        "short",
        "ask, task r1",
        Sensitivity::Internal,
        now(),
    )
    .await
    .unwrap();
    m.update(
        "bo",
        "areas",
        "Acme Vault",
        "ask, task r3",
        Sensitivity::Internal,
        now(),
    )
    .await
    .unwrap();
    let ana = m.read("ana").await.unwrap();
    let keys: Vec<_> = ana.iter().map(|n| n.key.as_str()).collect();
    assert_eq!(keys, vec!["areas", "style"]);
    assert_eq!(ana[0].text, "Acme Cloud, Acme Vault");
    assert_eq!(ana[0].updated_at, now() + Duration::minutes(1));
    assert!(m.read("cy").await.unwrap().is_empty());
}

/// A note keeps the sensitivity it was written with, and a rewrite replaces it (S6). In
/// Postgres, a note from before the column (null) reads as `internal`.
async fn notes_keep_their_sensitivity(b: &Backend) {
    let m = b.stores.memory.as_ref();
    let written = m
        .update(
            "ana",
            "partners",
            "Globex roadmap",
            "ask, task r1",
            Sensitivity::Restricted,
            now(),
        )
        .await
        .unwrap();
    assert_eq!(written.sensitivity, Sensitivity::Restricted);
    m.update(
        "ana",
        "style",
        "short",
        "ask, task r1",
        Sensitivity::Public,
        now(),
    )
    .await
    .unwrap();
    let read = m.read("ana").await.unwrap();
    let by_key: Vec<_> = read
        .iter()
        .map(|n| (n.key.as_str(), n.sensitivity))
        .collect();
    assert_eq!(
        by_key,
        vec![
            ("partners", Sensitivity::Restricted),
            ("style", Sensitivity::Public)
        ]
    );
    m.update(
        "ana",
        "partners",
        "Public summary",
        "ask, task r2",
        Sensitivity::Internal,
        now(),
    )
    .await
    .unwrap();
    assert_eq!(
        m.read("ana").await.unwrap()[0].sensitivity,
        Sensitivity::Internal
    );
    if let Some(pg) = &b.pg {
        sqlx::query(
            "INSERT INTO memory_notes (user_handle, key, text, updated_at) VALUES ('ana', 'old', 'x', now())",
        )
        .execute(pg.pool())
        .await
        .unwrap();
        let old = m
            .read("ana")
            .await
            .unwrap()
            .into_iter()
            .find(|n| n.key == "old")
            .unwrap();
        assert_eq!(old.sensitivity, Sensitivity::Internal);
    }
}

// ----- context ----------------------------------------------------------------------------

async fn context_search_and_get(b: &Backend) {
    let c = b.stores.context.as_ref();
    let hits = c
        .search("key management", None, Sensitivity::Restricted, 10)
        .await
        .unwrap()
        .hits;
    let paths: Vec<_> = hits.iter().map(|h| h.doc.path.as_str()).collect();
    assert_eq!(paths.len(), 2, "{paths:?}");
    assert!(paths.contains(&"context/products/cloud.md"));
    assert!(paths.contains(&"context/competitors/initech.md"));
    assert_eq!(
        paths[0], "context/competitors/initech.md",
        "a title match ranks first"
    );
    assert!(hits.iter().all(|h| h.score > 0.0));
    assert!(hits[0].snippet.to_lowercase().contains("key management"));

    let only = c
        .search("key", Some("products"), Sensitivity::Restricted, 10)
        .await
        .unwrap()
        .hits;
    assert_eq!(only.len(), 1);
    assert_eq!(only[0].doc.domain, "products");

    assert_eq!(
        c.search("key management", None, Sensitivity::Restricted, 1)
            .await
            .unwrap()
            .hits
            .len(),
        1
    );
    assert!(c
        .search("", None, Sensitivity::Restricted, 10)
        .await
        .unwrap()
        .hits
        .is_empty());
    assert!(c
        .search("zebra", None, Sensitivity::Restricted, 10)
        .await
        .unwrap()
        .hits
        .is_empty());
    // Query syntax characters are just separators, never operators.
    let odd = c
        .search("key & | ! ( :* roadmap", None, Sensitivity::Restricted, 10)
        .await
        .unwrap()
        .hits;
    assert!(!odd.is_empty());

    let nda = c.get("context/partners/nda.md").await.unwrap().unwrap();
    assert_eq!(nda.sensitivity, Sensitivity::Restricted);
    assert!(nda.body.contains("under NDA"));
    assert!(c.get("context/partners/none.md").await.unwrap().is_none());
}

async fn context_search_applies_the_ceiling_in_the_store(b: &Backend) {
    // Thirty restricted files that match strongly, and one internal file that barely does.
    let mut docs: Vec<ContextDoc> = (0..30)
        .map(|i| {
            doc(
                &format!("context/partners/quasar-{i}.md"),
                "Quasar quasar",
                Sensitivity::Restricted,
                "# Quasar\n\nQuasar quasar quasar.\n",
            )
        })
        .collect();
    docs.push(doc(
        "context/products/plan.md",
        "Plan",
        Sensitivity::Internal,
        "# Plan\n\nOne mention of quasar.\n",
    ));
    b.stores.context.reindex(docs).await.unwrap();
    let c = b.stores.context.as_ref();

    // An internal skill still finds the file it may see, and learns nothing else.
    let found = c
        .search("quasar", None, Sensitivity::Internal, 5)
        .await
        .unwrap();
    let paths: Vec<_> = found.hits.iter().map(|h| h.doc.path.as_str()).collect();
    assert_eq!(paths, vec!["context/products/plan.md"]);
    assert_eq!(found.withheld, 30);

    let all = c
        .search("quasar", None, Sensitivity::Restricted, 5)
        .await
        .unwrap();
    assert_eq!(all.hits.len(), 5);
    assert_eq!(all.withheld, 0);
    assert!(c
        .search("quasar", None, Sensitivity::Public, 5)
        .await
        .unwrap()
        .hits
        .is_empty());
}

async fn reindex_tracks_the_content_repo(b: &Backend) {
    // One file edited, one deleted, one added.
    let mut docs = library();
    docs.retain(|d| d.path != "context/partners/nda.md");
    docs[0].body = "# Acme Cloud\n\nNow with quantum-safe keys.\n".into();
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
    let quantum = c
        .search("quantum", None, Sensitivity::Restricted, 5)
        .await
        .unwrap()
        .hits;
    assert_eq!(quantum[0].doc.path, "context/products/cloud.md");
    let trusted = c
        .search("trusted", None, Sensitivity::Restricted, 5)
        .await
        .unwrap()
        .hits;
    assert_eq!(trusted[0].doc.path, "context/glossary/terms.md");
    assert!(c
        .search("roadmap", None, Sensitivity::Restricted, 5)
        .await
        .unwrap()
        .hits
        .is_empty());
}

// ----- audit ------------------------------------------------------------------------------

fn event(outcome: Outcome) -> AuditEvent {
    AuditEvent {
        client: pmos_toolserver::audit::Client::Mcp,
        at: now(),
        run_id: "run-a".into(),
        user: "ana".into(),
        skill: Some("prd-draft".into()),
        bot: None,
        tool: "confluence.create_draft".into(),
        outcome,
        sources: vec!["context/products/cloud.md".into(), "jira:PM-101".into()],
        source_titles: [("jira:PM-101".to_string(), "SSO for the console".to_string())]
            .into_iter()
            .collect(),
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
        assert_eq!(sources, vec!["context/products/cloud.md", "jira:PM-101"]);
        // An event recorded before titles were (the column is null) reads with none.
        sqlx::query(
            "INSERT INTO audit_events (at, run_id, user_handle, skill, tool, outcome, sources)
             VALUES ($1, 'run-a', 'ana', 'prd-draft', 'jira.read', 'ok', ARRAY['jira:PM-7'])",
        )
        .bind(now() - Duration::seconds(5))
        .execute(pg.pool())
        .await
        .unwrap();

        // Append-only: no update, delete or truncate, even by the table's owner.
        for sql in [
            "UPDATE audit_events SET outcome = 'ok'",
            "DELETE FROM audit_events",
            "TRUNCATE audit_events",
        ] {
            let err = sqlx::query(sql).execute(pg.pool()).await.unwrap_err();
            assert!(err.to_string().contains("append-only"), "{sql}: {err}");
        }
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_events")
            .fetch_one(pg.pool())
            .await
            .unwrap();
        assert_eq!(count, 3);
    }
    // A read is read back with its titles, in both stores.
    let mut read = event(Outcome::Ok);
    read.tool = "jira.read".into();
    b.stores.audit.record(read).await.unwrap();
    let reads = b
        .stores
        .audit
        .reads("ana", "run-a", now() + Duration::seconds(1), 10)
        .await
        .unwrap()
        .unwrap();
    // Only `ok` events with sources are reads: not the pending and approved ones above.
    assert!(reads.iter().all(|r| r.tool == "jira.read"));
    assert_eq!(
        reads[0].titles.get("jira:PM-101").map(String::as_str),
        Some("SSO for the console")
    );
    if b.pg.is_some() {
        assert_eq!(reads.len(), 2);
        assert!(reads[1].titles.is_empty(), "an older event has no titles");
    } else {
        assert_eq!(reads.len(), 1);
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
        suspended_at: None,
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

async fn github_credentials_are_stored(b: &Backend) {
    // The migration widens the provider check to `github`.
    let c = b.stores.credentials.as_ref();
    c.put(&sealed("ana", Provider::Github, "v1.k1.sealed-g"))
        .await
        .unwrap();
    let got = c.get("ana", Provider::Github).await.unwrap().unwrap();
    assert_eq!(got.provider, Provider::Github);
    assert_eq!(c.list("ana").await.unwrap().len(), 1);
}

// ----- onboarding -------------------------------------------------------------------------

async fn memory_repos_are_per_user(b: &Backend) {
    let r = b.stores.memory_repos.as_ref();
    assert!(r.get("ana").await.unwrap().is_none());
    let record = MemoryRepo {
        user: "ana".into(),
        repo: "example-org/pm-os-memory-ana".into(),
        verified_at: now(),
    };
    r.put(&record).await.unwrap();
    assert_eq!(r.get("ana").await.unwrap(), Some(record.clone()));
    assert!(r.get("bo").await.unwrap().is_none());
    let moved = MemoryRepo {
        repo: "example-org/ana-notes".into(),
        verified_at: now() + Duration::minutes(11),
        ..record
    };
    r.put(&moved).await.unwrap();
    assert_eq!(r.get("ana").await.unwrap(), Some(moved));
    assert!(r.delete("ana").await.unwrap());
    assert!(!r.delete("ana").await.unwrap());
    assert!(r.get("ana").await.unwrap().is_none());
}

fn new_request<'a>(slack_id: &'a str, handle: &'a str) -> NewRequest<'a> {
    NewRequest {
        slack_id,
        email: "new@example.com",
        name: "New Person",
        proposed_handle: handle,
    }
}

fn decided(status: RequestStatus, by: &str) -> Decided {
    Decided {
        status,
        decided_by: by.into(),
        handle: (status == RequestStatus::Approved).then(|| "nia".to_string()),
        commit: (status == RequestStatus::Approved).then(|| "abc123".to_string()),
        error: (status == RequestStatus::Failed).then(|| "it broke".to_string()),
    }
}

async fn member_requests_follow_the_rules(b: &Backend) {
    let s = b.stores.member_requests.as_ref();
    let Created::New(first) = s.create(new_request("U0NIA", "nia"), now()).await.unwrap() else {
        panic!("a new request");
    };
    assert_eq!(first.status, RequestStatus::Pending);
    assert_eq!(first.proposed_handle, "nia");
    // One pending request per Slack id.
    let again = s
        .create(new_request("U0NIA", "nia2"), now() + Duration::minutes(1))
        .await
        .unwrap();
    assert_eq!(again, Created::Existing(first.clone()));
    assert_eq!(s.pending_for("U0NIA").await.unwrap(), Some(first.clone()));
    assert_eq!(s.get(&first.id).await.unwrap(), Some(first.clone()));
    assert!(s.get("no-such-id").await.unwrap().is_none());

    let Created::New(second) = s
        .create(new_request("U0OMI", "omi"), now() + Duration::minutes(2))
        .await
        .unwrap()
    else {
        panic!("a new request");
    };
    let approved = s
        .decide(
            &first.id,
            &decided(RequestStatus::Approved, "raj"),
            now() + Duration::minutes(3),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(approved.status, RequestStatus::Approved);
    assert_eq!(approved.decided_by.as_deref(), Some("raj"));
    assert_eq!(approved.decided_at, Some(now() + Duration::minutes(3)));
    assert_eq!(approved.handle.as_deref(), Some("nia"));
    assert_eq!(approved.commit.as_deref(), Some("abc123"));
    // Decided once.
    assert!(s
        .decide(&first.id, &decided(RequestStatus::Rejected, "kim"), now())
        .await
        .unwrap()
        .is_none());
    assert_eq!(s.get(&first.id).await.unwrap(), Some(approved.clone()));
    let failed = s
        .decide(&second.id, &decided(RequestStatus::Failed, "raj"), now())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(failed.error.as_deref(), Some("it broke"));
    // A decided request doesn't block a new one.
    assert!(s.pending_for("U0NIA").await.unwrap().is_none());
    let Created::New(third) = s
        .create(new_request("U0NIA", "nia"), now() + Duration::minutes(4))
        .await
        .unwrap()
    else {
        panic!("a new request");
    };

    let ids = |v: Vec<pmos_toolserver::members::MemberRequest>| -> Vec<String> {
        v.into_iter().map(|r| r.id).collect()
    };
    assert_eq!(
        ids(s.list(None).await.unwrap()),
        vec![first.id.clone(), second.id.clone(), third.id.clone()],
        "oldest first"
    );
    assert_eq!(
        ids(s.list(Some(RequestStatus::Pending)).await.unwrap()),
        vec![third.id.clone()]
    );
    assert_eq!(
        ids(s.list(Some(RequestStatus::Failed)).await.unwrap()),
        vec![second.id.clone()]
    );

    // At most MAX_PENDING pending.
    for n in 1..MAX_PENDING {
        let slack = format!("U0P{n}");
        assert!(matches!(
            s.create(new_request(&slack, "p"), now()).await.unwrap(),
            Created::New(_)
        ));
    }
    assert_eq!(
        s.create(new_request("U0LAST", "p"), now()).await.unwrap(),
        Created::TooMany
    );
    // Someone with a pending request still gets it back.
    assert!(matches!(
        s.create(new_request("U0NIA", "nia"), now()).await.unwrap(),
        Created::Existing(_)
    ));
}

async fn member_requests_are_decided_once_under_concurrency(b: &Backend) {
    let s = b.stores.member_requests.clone();
    let Created::New(r) = s.create(new_request("U0NIA", "nia"), now()).await.unwrap() else {
        panic!("a new request");
    };
    let tasks: Vec<_> = (0..8)
        .map(|i| {
            let s = s.clone();
            let id = r.id.clone();
            tokio::spawn(async move {
                let status = if i % 2 == 0 {
                    RequestStatus::Approved
                } else {
                    RequestStatus::Rejected
                };
                s.decide(&id, &decided(status, "raj"), now()).await.unwrap()
            })
        })
        .collect();
    let mut won = 0;
    for t in tasks {
        if t.await.unwrap().is_some() {
            won += 1;
        }
    }
    assert_eq!(won, 1);
    // Concurrent requests for one Slack id make one pending request.
    let tasks: Vec<_> = (0..8)
        .map(|_| {
            let s = s.clone();
            tokio::spawn(async move { s.create(new_request("U0OMI", "omi"), now()).await.unwrap() })
        })
        .collect();
    let mut created = 0;
    for t in tasks {
        if matches!(t.await.unwrap(), Created::New(_)) {
            created += 1;
        }
    }
    assert_eq!(created, 1);
    assert_eq!(s.list(Some(RequestStatus::Pending)).await.unwrap().len(), 1);
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
    async fn a_session_or_item_names_a_skill_or_a_bot_never_both() {
        let Some(b) = Backend::postgres(Vec::new()).await else {
            return;
        };
        let pg = b.pg.as_ref().unwrap();
        for (skill, bot) in [(Some("ask"), Some("scout")), (None, None)] {
            let err = sqlx::query(
                "INSERT INTO sessions (token_hash, user_handle, skill, bot, run_id, created_at, expires_at)
                 VALUES ('h', 'ana', $1, $2, 'r', now(), now())",
            )
            .bind(skill)
            .bind(bot)
            .execute(pg.pool())
            .await
            .unwrap_err();
            assert!(err.to_string().contains("sessions_skill_or_bot"), "{err}");
            let err = sqlx::query(
                "INSERT INTO approvals (id, user_handle, skill, bot, run_id, tool, args, args_hash, status, created_at)
                 VALUES ('apr_x', 'ana', $1, $2, 'r', 't', '{}', 'h', 'pending', now())",
            )
            .bind(skill)
            .bind(bot)
            .execute(pg.pool())
            .await
            .unwrap_err();
            assert!(err.to_string().contains("approvals_skill_or_bot"), "{err}");
        }
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

    /// Migration 0007 gives every item made before the ledger a row.
    #[tokio::test]
    async fn the_ledger_migration_backfills_existing_items() {
        let Some(b) = Backend::postgres(Vec::new()).await else {
            return;
        };
        let pg = b.pg.as_ref().unwrap();
        let args = json!({"title": "PRD"});
        let a = b.stores.approvals.as_ref();
        let pending = a.create(new_item("ana", &args), now()).await.unwrap();
        let running = a.create(new_item("ana", &args), now()).await.unwrap();
        a.decide(&running.id, "ana", Decision::Approve, None, now())
            .await
            .unwrap();
        let rejected = a.create(new_item("ana", &args), now()).await.unwrap();
        a.decide(&rejected.id, "ana", Decision::Reject, None, now())
            .await
            .unwrap();
        // Back to how a database looked before 0007, then apply it by hand.
        sqlx::query("DROP TABLE ledger")
            .execute(pg.pool())
            .await
            .unwrap();
        sqlx::raw_sql(sqlx::AssertSqlSafe(include_str!(
            "../migrations/0007_ledger.sql"
        )))
        .execute(pg.pool())
        .await
        .unwrap();
        let rows = a.ledger(&LedgerQuery::all(now())).await.unwrap();
        assert_eq!(rows.len(), 3);
        let outcome = |id: &str| rows.iter().find(|r| r.id == id).unwrap().outcome;
        assert_eq!(outcome(&pending.id), LedgerOutcome::Pending);
        assert_eq!(outcome(&running.id), LedgerOutcome::Pending);
        assert_eq!(outcome(&rejected.id), LedgerOutcome::Rejected);
        assert!(rows.iter().all(|r| r.content_sha.is_none()));
        // The write still running finishes into its backfilled row.
        a.complete(&running.id, Ok(json!({}))).await.unwrap();
        let rows = a.ledger(&LedgerQuery::all(now())).await.unwrap();
        let r = rows.iter().find(|r| r.id == running.id).unwrap();
        assert_eq!(r.outcome, LedgerOutcome::Approved);
        b.finish().await;
    }
}
