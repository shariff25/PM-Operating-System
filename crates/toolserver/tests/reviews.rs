//! Slice U6a of the user database (`docs/user-database-spec.md` §8): a Reviewer's inbox, read
//! and recommendation (`/internal/reviews…`), the owner's view of the recommendations, and the
//! decision's audit detail. Every rule the slice names, on the in-memory store and, with
//! `PMOS_TEST_DATABASE_URL`, Postgres: a recommendation never changes an approval's status or
//! calls a connector; only an active, assigned reviewer (`404` for an unassigned one); never
//! on one's own draft; restricted drafts (a W6 follow-up included) are metadata only;
//! `409 draft_changed`; "on an earlier version" after an owner's edit; the note goes with the
//! Slack text; the signed assertion and the web token; every call audited without the note.

mod common;

use std::collections::BTreeSet;
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use common::{Fixture, TestPg};
use pmos_assertion::{unix_now, Claims, Key, KeyRing, RequestBinding};
use pmos_contract::model::Role;
use pmos_contract::Sensitivity;
use pmos_toolserver::app::BotAccess;
use pmos_toolserver::approvals::{Decision, Status, Verdict};
use pmos_toolserver::assertion::{parse_keys, Assertions, Mode, AUDIENCE, ISSUERS};
use pmos_toolserver::audit::{AuditEvent, AuditQuery, Client, Outcome};
use pmos_toolserver::connectors::fake::SLACK_ALL_TEAM;
use pmos_toolserver::reviews::RecVerdict;
use pmos_toolserver::service::{About, ReviewError};
use pmos_toolserver::users::{Actor, Op, UserRole};
use pmos_toolserver::{router_full, AppConfig, PersonClients};
use reqwest::Method;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

const SERVICE_TOKEN: &str = "test-service-token-0123456789abcdef";
const BOT_TOKEN: &str = "test-bot-token-0123456789abcdef-0123";
const WEB_TOKEN: &str = "test-web-token-0123456789abcdef-01234";
const CONSOLE_TOKEN: &str = "test-console-token-0123456789abcdef-0";
// Test key only: fixed bytes, never used anywhere real.
const WEB_KEY: &str = "d2ViLXRvb2xzZXJ2ZXItdGVzdC1rZXktMDEyMzQ1Njc=";
/// Fake text only: a note that must never reach a log or the audit.
const NOTE: &str =
    "Fake note: the rollout date looks right <!channel> <img src=x onerror=alert(1)>";

enum Assert {
    Require,
    /// `PMOS_ASSERTION_MODE=off` with `PMOS_ASSERTION_INSECURE_OFF=1`, but the web key set.
    InsecureOff,
}

struct Server {
    base: String,
    http: reqwest::Client,
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
}

/// `ada` admin; `ana`, `bo` PMs; `rev` (assigned `ana` and `bo`) and `rex` (assigned no one)
/// reviewers; `aud` auditor.
async fn start(assert: Assert, postgres: bool) -> Option<Server> {
    let (f, pg) = common::roles_fixture(
        &[
            ("ada", Role::Admin),
            ("ana", Role::Pm),
            ("bo", Role::Pm),
            ("rev", Role::Reviewer),
            ("rex", Role::Reviewer),
            ("aud", Role::Auditor),
        ],
        postgres,
    )
    .await?;
    f.svc
        .people()
        .apply(
            &Actor::web("ada"),
            Op::AssignReviewer {
                reviewer: "rev".into(),
                pms: BTreeSet::from(["ana".to_string(), "bo".to_string()]),
            },
            Utc::now(),
        )
        .await
        .expect("assign rev");
    let keys = format!("web:toolserver:w1:{WEB_KEY}");
    let keys = parse_keys(&keys, "the tool server", |iss, aud| {
        aud == AUDIENCE && ISSUERS.contains(&iss)
    })
    .unwrap();
    let assertions = match assert {
        Assert::Require => Assertions::new(Mode::Require, keys).unwrap(),
        Assert::InsecureOff => {
            let lookup = move |k: &str| match k {
                "PMOS_ASSERTION_MODE" => Some("off".to_string()),
                "PMOS_ASSERTION_INSECURE_OFF" => Some("1".to_string()),
                "PMOS_ASSERTION_KEYS" => Some(format!("web:toolserver:w1:{WEB_KEY}")),
                _ => None,
            };
            let a = Assertions::from_lookup(lookup, &[("web", "PMOS_WEB_TOKEN")]).unwrap();
            assert!(a.insecure_warning().is_some());
            a
        }
    };
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
            assertions: Arc::new(assertions),
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
        http: reqwest::Client::new(),
        cancel,
        f,
        pg,
    })
}

fn sign(sub: &str, method: &str, target: &str, body: &[u8]) -> String {
    let ring = KeyRing::new(Key::from_base64("w1", WEB_KEY).unwrap());
    let claims = Claims::new("web", "toolserver", sub, &[Role::Reviewer], "s-test").unwrap();
    let req = RequestBinding::new(method, target, body).unwrap();
    ring.sign(&claims, unix_now(), 60, Some(&req)).unwrap()
}

/// A Jira draft by `user`, from a session that starts at `floor` (a follow-up carries its
/// conversation's level, W6) and reads nothing itself.
async fn jira_draft(f: &Fixture, user: &str, run: &str, floor: Sensitivity) -> String {
    let at = Utc::now();
    let s = f
        .svc
        .create_session_from(user, "internal-brief", run, None, floor, at)
        .await
        .unwrap();
    let out = f
        .svc
        .call(
            &s,
            "jira_create_issue",
            json!({"project": "PM", "issue_type": "Story", "summary": "Fake: SSO grace period", "description": "Fake description."}),
            at,
        )
        .await
        .unwrap();
    out["approval_id"].as_str().unwrap().to_string()
}

/// A Slack reply draft by `user`, made at `at`.
async fn slack_draft(f: &Fixture, user: &str, run: &str, at: DateTime<Utc>) -> String {
    let s = f
        .svc
        .create_session(user, "field-watch", run, Some(Duration::hours(1)), at)
        .await
        .unwrap();
    let out = f
        .svc
        .call(
            &s,
            "slack_post_reply",
            json!({"channel": SLACK_ALL_TEAM, "thread_ts": "1731600000.000100", "text": "Fake reply."}),
            at,
        )
        .await
        .unwrap();
    out["approval_id"].as_str().unwrap().to_string()
}

impl Server {
    async fn send(
        &self,
        method: Method,
        target: &str,
        token: Option<&str>,
        assertion: Option<&str>,
        body: Option<&Value>,
    ) -> (u16, Value) {
        let mut r = self.http.request(method, format!("{}{target}", self.base));
        if let Some(t) = token {
            r = r.bearer_auth(t);
        }
        if let Some(a) = assertion {
            r = r.header("x-pmos-assertion", a);
        }
        if let Some(b) = body {
            r = r
                .header("content-type", "application/json")
                .body(serde_json::to_vec(b).unwrap());
        }
        let res = r.send().await.unwrap();
        let status = res.status().as_u16();
        let text = res.text().await.unwrap();
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    /// `pmos-web` acting for `actor`, signed for them.
    async fn web(
        &self,
        actor: &str,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> (u16, Value) {
        let sep = if path.contains('?') { '&' } else { '?' };
        let target = format!("{path}{sep}user={actor}");
        let bytes = body
            .as_ref()
            .map(|b| serde_json::to_vec(b).unwrap())
            .unwrap_or_default();
        let a = sign(actor, method.as_str(), &target, &bytes);
        self.send(method, &target, Some(WEB_TOKEN), Some(&a), body.as_ref())
            .await
    }

    async fn read(&self, reviewer: &str, id: &str) -> (u16, Value) {
        self.web(
            reviewer,
            Method::GET,
            &format!("/internal/reviews/{id}"),
            None,
        )
        .await
    }

    async fn recommend(&self, reviewer: &str, id: &str, body: Value) -> (u16, Value) {
        self.web(
            reviewer,
            Method::POST,
            &format!("/internal/reviews/{id}/recommendation"),
            Some(body),
        )
        .await
    }

    /// Read `id` as `reviewer`, then recommend `verdict` on what was read.
    async fn read_and_recommend(
        &self,
        reviewer: &str,
        id: &str,
        verdict: &str,
        note: Option<&str>,
    ) -> (u16, Value) {
        let (s, d) = self.read(reviewer, id).await;
        assert_eq!(s, 200, "{d}");
        let mut body = json!({
            "verdict": verdict,
            "draft_revision": d["revision"],
            "args_hash": d["args_hash"],
        });
        if let Some(n) = note {
            body["note"] = json!(n);
        }
        self.recommend(reviewer, id, body).await
    }

    /// The owner's own read through `pmos-web`.
    async fn owner_view(&self, owner: &str, id: &str) -> Value {
        let (s, b) = self
            .web(
                owner,
                Method::GET,
                &format!("/internal/approvals/{id}"),
                None,
            )
            .await;
        assert_eq!(s, 200, "{b}");
        b
    }

    async fn events(&self, action: &str) -> Vec<AuditEvent> {
        let q = AuditQuery {
            before: None,
            limit: 500,
            actor: None,
            action: Some(action.into()),
            outcome: None,
            from: None,
            to: None,
        };
        self.f
            .stores
            .audit
            .search(&q)
            .await
            .unwrap()
            .unwrap()
            .into_iter()
            .map(|e| e.event)
            .collect()
    }

    async fn all_events(&self) -> Vec<AuditEvent> {
        let q = AuditQuery {
            before: None,
            limit: 1000,
            actor: None,
            action: None,
            outcome: None,
            from: None,
            to: None,
        };
        self.f
            .stores
            .audit
            .search(&q)
            .await
            .unwrap()
            .unwrap()
            .into_iter()
            .map(|e| e.event)
            .collect()
    }
}

/// Each case runs on the in-memory store and, with `PMOS_TEST_DATABASE_URL`, Postgres.
macro_rules! both {
    ($name:ident, $assert:expr, |$s:ident| $body:block) => {
        #[tokio::test]
        async fn $name() {
            for postgres in [false, true] {
                let Some(server) = start($assert, postgres).await else {
                    continue;
                };
                {
                    let $s = &server;
                    $body
                }
                server.stop().await;
            }
        }
    };
}

both!(
    a_recommendation_never_decides_or_writes,
    Assert::Require,
    |s| {
        let id = jira_draft(&s.f, "ana", "tsk_a1", Sensitivity::Public).await;
        let before = s.f.stores.approvals.get(&id).await.unwrap().unwrap();
        let (st, b) = s
            .read_and_recommend("rev", &id, "approve", Some(NOTE))
            .await;
        assert_eq!(st, 200, "{b}");
        assert_eq!(b["recommendation"]["verdict"], "approve");
        assert_eq!(b["draft"]["my_recommendation"]["note"], NOTE);
        let (st, b) = s.read_and_recommend("rev", &id, "changes", None).await;
        assert_eq!(st, 200, "{b}");
        // Still pending, with the very same text; no connector was called; the ledger row waits.
        let after = s.f.stores.approvals.get(&id).await.unwrap().unwrap();
        assert_eq!(after.status, Status::Pending);
        assert_eq!(after.args_hash, before.args_hash);
        assert_eq!(after.revision, before.revision);
        assert!(s.f.jira.writes().is_empty());
        // One live recommendation per reviewer: the second replaced the first (and its note).
        let live =
            s.f.stores
                .approvals
                .recommendations(std::slice::from_ref(&id))
                .await
                .unwrap();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].verdict, RecVerdict::Changes);
        assert_eq!(live[0].note, None);
        // A reviewer's decision on someone else's draft is refused, recommendations or not.
        let (st, _) = s
        .web(
            "rev",
            Method::POST,
            &format!("/internal/approvals/{id}/decision"),
            Some(json!({"decided_by": "rev", "decision": "approve", "expected_args_hash": after.args_hash})),
        )
        .await;
        assert_eq!(st, 403, "only the item's own user decides it");
        let err =
            s.f.svc
                .decide_verdict(
                    Client::Web,
                    &id,
                    "rev",
                    Verdict::new(Decision::Approve, None).shown(Some(after.args_hash.clone())),
                    Utc::now(),
                )
                .await
                .unwrap_err();
        assert_eq!(err.to_string(), "only the item's own user can decide it");
        assert_eq!(
            s.f.stores.approvals.get(&id).await.unwrap().unwrap().status,
            Status::Pending
        );
        assert!(s.f.jira.writes().is_empty());
    }
);

both!(
    only_an_active_assigned_reviewer_reads_or_recommends,
    Assert::Require,
    |s| {
        let ana = jira_draft(&s.f, "ana", "tsk_a1", Sensitivity::Public).await;
        let bo = jira_draft(&s.f, "bo", "tsk_b1", Sensitivity::Public).await;
        // The assigned reviewer's inbox: both PMs' drafts.
        let (st, b) = s.web("rev", Method::GET, "/internal/reviews", None).await;
        assert_eq!(st, 200, "{b}");
        let ids: BTreeSet<String> = b["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["id"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(ids, BTreeSet::from([ana.clone(), bo.clone()]));
        assert_eq!(b["pms"], json!(["ana", "bo"]));
        assert!(b["items"][0]["owner_name"].as_str().is_some());
        // A reviewer assigned no one: an empty inbox, and `404` on each draft, as for a missing one.
        let (st, b) = s.web("rex", Method::GET, "/internal/reviews", None).await;
        assert_eq!((st, b["items"].as_array().unwrap().len()), (200, 0), "{b}");
        let (st, b) = s.read("rex", &ana).await;
        assert_eq!((st, b["code"].as_str()), (404, Some("not_found")), "{b}");
        let (st, missing) = s.read("rex", "apr_doesnotexist").await;
        assert_eq!((st, &missing["code"]), (404, &b["code"]));
        let body = json!({"verdict": "approve", "draft_revision": 1, "args_hash": "a".repeat(64)});
        let (st, b) = s.recommend("rex", &ana, body.clone()).await;
        assert_eq!((st, b["code"].as_str()), (404, Some("not_found")), "{b}");
        // Without the reviewer role (a PM, an admin, an auditor): `403 forbidden`.
        for who in ["ana", "ada", "aud"] {
            let (st, b) = s.read(who, &bo).await;
            assert_eq!(
                (st, b["code"].as_str()),
                (403, Some("forbidden")),
                "{who}: {b}"
            );
            let (st, _) = s.web(who, Method::GET, "/internal/reviews", None).await;
            assert_eq!(st, 403, "{who}");
        }
        // The role taken away: refused at once.
        s.f.svc
            .people()
            .apply(
                &Actor::web("ada"),
                Op::SetRoles {
                    handle: "rev".into(),
                    roles: BTreeSet::from([UserRole::Pm]),
                },
                Utc::now(),
            )
            .await
            .unwrap();
        let (st, _) = s.read("rev", &ana).await;
        assert_eq!(st, 403);
        s.f.svc
            .people()
            .apply(
                &Actor::web("ada"),
                Op::SetRoles {
                    handle: "rev".into(),
                    roles: BTreeSet::from([UserRole::Reviewer]),
                },
                Utc::now(),
            )
            .await
            .unwrap();
        // A deactivated PM's held drafts are no one's to review.
        s.f.svc
            .people()
            .apply(
                &Actor::web("ada"),
                Op::Deactivate {
                    handle: "bo".into(),
                },
                Utc::now(),
            )
            .await
            .unwrap();
        let (st, b) = s.read("rev", &bo).await;
        assert_eq!(st, 404, "{b}");
        let (_, b) = s.web("rev", Method::GET, "/internal/reviews", None).await;
        assert_eq!(b["pms"], json!(["ana"]));
        // Every refusal is audited, by code.
        let denied: Vec<_> = s
            .events("approvals.review_read")
            .await
            .into_iter()
            .filter(|e| e.outcome == Outcome::Denied)
            .map(|e| (e.user, e.detail.unwrap_or_default()))
            .collect();
        assert!(
            denied.contains(&("rex".into(), "not_found".into())),
            "{denied:?}"
        );
        assert!(
            denied.contains(&("aud".into(), "forbidden".into())),
            "{denied:?}"
        );
        assert!(s
            .events("approvals.recommend")
            .await
            .iter()
            .any(|e| e.user == "rex"
                && e.outcome == Outcome::Denied
                && e.detail.as_deref() == Some("not_found")));
    }
);

both!(never_on_your_own_draft, Assert::Require, |s| {
    // A reviewer drafts too (the role includes a PM's): their own draft is theirs to decide.
    let mine = jira_draft(&s.f, "rev", "tsk_r1", Sensitivity::Public).await;
    let (st, b) = s.read("rev", &mine).await;
    assert_eq!((st, b["code"].as_str()), (403, Some("own_draft")), "{b}");
    let item = s.f.stores.approvals.get(&mine).await.unwrap().unwrap();
    let (st, b) = s
        .recommend(
            "rev",
            &mine,
            json!({"verdict": "approve", "draft_revision": item.revision, "args_hash": item.args_hash}),
        )
        .await;
    assert_eq!((st, b["code"].as_str()), (403, Some("own_draft")), "{b}");
    assert!(s
        .f
        .stores
        .approvals
        .recommendations(&[mine])
        .await
        .unwrap()
        .is_empty());
});

both!(a_restricted_draft_is_metadata_only, Assert::Require, |s| {
    // A follow-up whose conversation read restricted content (W6): the session starts at
    // `restricted` and reads nothing itself, and its draft is still restricted.
    let id = jira_draft(&s.f, "ana", "tsk_follow", Sensitivity::Restricted).await;
    let item = s.f.stores.approvals.get(&id).await.unwrap().unwrap();
    assert_eq!(item.sources_sensitivity, Some(Sensitivity::Restricted));
    let (st, b) = s.read("rev", &id).await;
    assert_eq!(st, 200, "{b}");
    assert_eq!(b["restricted"], true);
    for hidden in [
        "args",
        "args_hash",
        "sources",
        "target_url",
        "target_channel",
        "my_recommendation",
        "edited",
    ] {
        assert!(b.get(hidden).is_none(), "{hidden} in {b}");
    }
    assert!(!b.to_string().contains("Fake: SSO"), "{b}");
    let (_, inbox) = s.web("rev", Method::GET, "/internal/reviews", None).await;
    let listed = &inbox["items"][0];
    assert_eq!(listed["id"], id.as_str());
    assert!(
        listed.get("args").is_none() && !inbox.to_string().contains("Fake: SSO"),
        "{inbox}"
    );
    let (st, b) = s
        .recommend(
            "rev",
            &id,
            json!({"verdict": "approve", "draft_revision": item.revision, "args_hash": item.args_hash}),
        )
        .await;
    assert_eq!(
        (st, b["code"].as_str()),
        (403, Some("restricted_draft")),
        "{b}"
    );
    // An internal one, for contrast, shows its text and sources.
    let open = jira_draft(&s.f, "ana", "tsk_open", Sensitivity::Public).await;
    let (_, b) = s.read("rev", &open).await;
    assert_eq!(b["restricted"], false);
    assert_eq!(b["args"]["summary"], "Fake: SSO grace period");
    assert!(b["sources"].is_array() && b["args_hash"].is_string(), "{b}");
});

both!(
    a_recommendation_is_bound_to_the_version_read,
    Assert::Require,
    |s| {
        let id = jira_draft(&s.f, "ana", "tsk_a1", Sensitivity::Public).await;
        let (_, read) = s.read("rev", &id).await;
        assert_eq!(read["revision"], 1);
        // The owner saves an edit: the revision moves on.
        let mut args = read["args"].clone();
        args["summary"] = json!("Fake: SSO grace period, edited");
        let edited =
            s.f.svc
                .save_draft(Client::Web, &id, "ana", args, None, Utc::now())
                .await
                .unwrap();
        assert_eq!(edited.revision, 2);
        let stale =
            json!({"verdict": "approve", "draft_revision": 1, "args_hash": read["args_hash"]});
        let (st, b) = s.recommend("rev", &id, stale).await;
        assert_eq!(
            (st, b["code"].as_str()),
            (409, Some("draft_changed")),
            "{b}"
        );
        assert_eq!(b["revision"], 2);
        assert_eq!(b["args_hash"], edited.args_hash.as_str());
        // The new revision with the old hash is refused too, and so is the reverse.
        let (st, _) = s
            .recommend(
                "rev",
                &id,
                json!({"verdict": "approve", "draft_revision": 2, "args_hash": read["args_hash"]}),
            )
            .await;
        assert_eq!(st, 409);
        let (st, _) = s
            .recommend(
                "rev",
                &id,
                json!({"verdict": "approve", "draft_revision": 1, "args_hash": edited.args_hash}),
            )
            .await;
        assert_eq!(st, 409);
        assert!(s
            .f
            .stores
            .approvals
            .recommendations(std::slice::from_ref(&id))
            .await
            .unwrap()
            .is_empty());
        // Saving the very same text again changes nothing: the revision stays.
        let again =
            s.f.svc
                .save_draft(
                    Client::Web,
                    &id,
                    "ana",
                    edited.args.clone(),
                    None,
                    Utc::now(),
                )
                .await
                .unwrap();
        assert_eq!(again.revision, 2);
        let (st, b) = s.read_and_recommend("rev", &id, "approve", None).await;
        assert_eq!(st, 200, "{b}");
        assert_eq!(b["recommendation"]["draft_revision"], 2);
        // Malformed bodies: `400`; an over-long note: `422 invalid`. Each audited.
        for bad in [
            json!({"verdict": "reject", "draft_revision": 2, "args_hash": edited.args_hash}),
            json!({"verdict": "approve", "draft_revision": 0, "args_hash": edited.args_hash}),
            json!({"verdict": "approve", "draft_revision": 2, "args_hash": &edited.args_hash[..16]}),
            json!({"verdict": "approve", "draft_revision": 2, "args_hash": edited.args_hash, "extra": 1}),
        ] {
            let (st, b) = s.recommend("rev", &id, bad.clone()).await;
            assert_eq!(
                (st, b["code"].as_str()),
                (400, Some("bad_request")),
                "{bad}"
            );
        }
        let long = json!({"verdict": "approve", "draft_revision": 2, "args_hash": edited.args_hash, "note": "x".repeat(1001)});
        let (st, b) = s.recommend("rev", &id, long).await;
        assert_eq!((st, b["code"].as_str()), (422, Some("invalid")), "{b}");
        let codes: Vec<_> = s
            .events("approvals.recommend")
            .await
            .into_iter()
            .filter(|e| e.outcome == Outcome::Denied)
            .filter_map(|e| e.detail)
            .collect();
        for code in ["draft_changed", "bad_request", "invalid"] {
            assert!(codes.iter().any(|c| c == code), "{code} in {codes:?}");
        }
    }
);

both!(
    an_owner_edit_marks_the_advice_as_on_an_earlier_version,
    Assert::Require,
    |s| {
        let id = jira_draft(&s.f, "ana", "tsk_a1", Sensitivity::Public).await;
        let (st, _) = s
            .read_and_recommend("rev", &id, "approve", Some(NOTE))
            .await;
        assert_eq!(st, 200);
        // The owner sees who, what, when and the note, given on this version.
        let view = s.owner_view("ana", &id).await;
        let rec = &view["recommendations"][0];
        assert_eq!(rec["reviewer"], "rev");
        assert!(rec["reviewer_name"].as_str().is_some());
        assert_eq!(rec["verdict"], "approve");
        assert_eq!(rec["note"], NOTE);
        assert_eq!(rec["stale"], false);
        assert!(rec["created_at"].is_string());
        // The Slack bot's read (the card) and the console's: no note.
        let (st, bot) = s
            .send(
                Method::GET,
                &format!("/internal/approvals/{id}?user=ana"),
                Some(BOT_TOKEN),
                None,
                None,
            )
            .await;
        assert_eq!(st, 200);
        assert_eq!(bot["recommendations"][0]["verdict"], "approve");
        assert!(bot["recommendations"][0].get("note").is_none(), "{bot}");
        // The owner edits: the advice was given on an earlier version.
        let item = s.f.stores.approvals.get(&id).await.unwrap().unwrap();
        let mut args = item.args.clone();
        args["summary"] = json!("Fake: SSO grace period, v2");
        s.f.svc
            .save_draft(Client::Web, &id, "ana", args, None, Utc::now())
            .await
            .unwrap();
        let view = s.owner_view("ana", &id).await;
        assert_eq!(view["recommendations"][0]["stale"], true);
        let (_, bot) = s
            .send(
                Method::GET,
                &format!("/internal/approvals/{id}?user=ana"),
                Some(BOT_TOKEN),
                None,
                None,
            )
            .await;
        assert_eq!(bot["recommendations"][0]["stale"], true);
        // The owner approves: the decision's record says it followed advice on an earlier version.
        let item = s.f.stores.approvals.get(&id).await.unwrap().unwrap();
        let decided =
            s.f.svc
                .decide_verdict(
                    Client::Web,
                    &id,
                    "ana",
                    Verdict::new(Decision::Approve, None).shown(Some(item.args_hash.clone())),
                    Utc::now(),
                )
                .await
                .unwrap();
        assert_eq!(decided.status, Status::Approved);
        let approved: Vec<_> = s
            .events("jira.create_issue")
            .await
            .into_iter()
            .filter(|e| e.outcome == Outcome::Approved)
            .collect();
        assert_eq!(
            approved[0].detail.as_deref(),
            Some(
                "edited before approval; after a recommendation to approve, on an earlier version"
            )
        );
        // Another draft, advice on the version decided: "on this version"; a rejection keeps its
        // reason first.
        let other = jira_draft(&s.f, "bo", "tsk_b1", Sensitivity::Public).await;
        let (st, _) = s.read_and_recommend("rev", &other, "changes", None).await;
        assert_eq!(st, 200);
        let mut v = Verdict::new(Decision::Reject, None);
        v.reason = Some(pmos_toolserver::ledger::Reason::Tone);
        s.f.svc
            .decide_verdict(Client::Web, &other, "bo", v, Utc::now())
            .await
            .unwrap();
        let rejected: Vec<_> = s
            .events("jira.create_issue")
            .await
            .into_iter()
            .filter(|e| {
                e.outcome == Outcome::Rejected && e.output_ref.as_deref() == Some(other.as_str())
            })
            .collect();
        assert_eq!(
            rejected[0].detail.as_deref(),
            Some("reason: tone; after a recommendation to make changes, on this version")
        );
        // Decided: a recommendation now is `409 not_pending`.
        let item = s.f.stores.approvals.get(&other).await.unwrap().unwrap();
        let (st, b) = s
        .recommend("rev", &other, json!({"verdict": "approve", "draft_revision": item.revision, "args_hash": item.args_hash}))
        .await;
        assert_eq!((st, b["code"].as_str()), (409, Some("not_pending")), "{b}");
        let (st, b) = s.read("rev", &other).await;
        assert_eq!((st, b["code"].as_str()), (409, Some("not_pending")), "{b}");
    }
);

both!(a_note_goes_with_the_slack_text, Assert::Require, |s| {
    let at = Utc::now();
    let pending = slack_draft(&s.f, "ana", "tsk_s1", at).await;
    let decided = slack_draft(&s.f, "bo", "tsk_s2", at).await;
    for id in [&pending, &decided] {
        let (st, b) = s.read_and_recommend("rev", id, "approve", Some(NOTE)).await;
        assert_eq!(st, 200, "{b}");
    }
    let mut v = Verdict::new(Decision::Reject, None);
    v.reason = Some(pmos_toolserver::ledger::Reason::Tone);
    s.f.svc
        .decide_verdict(Client::Web, &decided, "bo", v, at)
        .await
        .unwrap();
    // A day on: the undecided draft expires and the decided one loses its text; both notes go.
    let later = at + s.f.svc.slack_draft_ttl() + Duration::minutes(1);
    let swept = s.f.svc.sweep_slack_drafts(later).await.unwrap();
    assert_eq!((swept.expired, swept.redacted), (1, 1));
    let recs =
        s.f.stores
            .approvals
            .recommendations(&[pending.clone(), decided.clone()])
            .await
            .unwrap();
    assert_eq!(recs.len(), 2);
    for r in &recs {
        assert_eq!(r.note, None, "{r:?}");
        assert!(r.note_removed_at.is_some());
    }
    let view = s.owner_view("bo", &decided).await;
    assert_eq!(view["recommendations"][0]["note_removed"], true);
    assert_eq!(view["recommendations"][0]["note"], Value::Null);
    // An expired draft can't be read or recommended on.
    let err =
        s.f.svc
            .review_item("rev", &pending, later)
            .await
            .unwrap_err();
    assert_eq!(err, ReviewError::Expired);
});

both!(
    an_overdue_slack_draft_expires_when_a_reviewer_reaches_it,
    Assert::Require,
    |s| {
        let at = Utc::now() - Duration::hours(30);
        let id = slack_draft(&s.f, "ana", "tsk_old", at).await;
        let (st, b) = s.read("rev", &id).await;
        assert_eq!((st, b["code"].as_str()), (409, Some("expired")), "{b}");
    }
);

both!(
    only_pmos_web_signed_for_the_reviewer,
    Assert::Require,
    |s| {
        let id = jira_draft(&s.f, "ana", "tsk_a1", Sensitivity::Public).await;
        let target = format!("/internal/reviews/{id}?user=rev");
        for token in [SERVICE_TOKEN, BOT_TOKEN, CONSOLE_TOKEN] {
            let (st, _) = s.send(Method::GET, &target, Some(token), None, None).await;
            assert_eq!(st, 403, "{token}");
        }
        let (st, _) = s.send(Method::GET, &target, None, None, None).await;
        assert_eq!(st, 401);
        // No assertion, or one for someone else, or one for another request: `401 bad_assertion`.
        let (st, b) = s
            .send(Method::GET, &target, Some(WEB_TOKEN), None, None)
            .await;
        assert_eq!((st, b["code"].as_str()), (401, Some("bad_assertion")));
        let ana = sign("ana", "GET", &target, b"");
        let (st, _) = s
            .send(Method::GET, &target, Some(WEB_TOKEN), Some(&ana), None)
            .await;
        assert_eq!(st, 401);
        let other = sign("rev", "GET", "/internal/reviews?user=rev", b"");
        let (st, _) = s
            .send(Method::GET, &target, Some(WEB_TOKEN), Some(&other), None)
            .await;
        assert_eq!(st, 401);
        // A body other than the one signed.
        let item = s.f.stores.approvals.get(&id).await.unwrap().unwrap();
        let post = format!("/internal/reviews/{id}/recommendation?user=rev");
        let body = json!({"verdict": "approve", "draft_revision": item.revision, "args_hash": item.args_hash});
        let signed = sign("rev", "POST", &post, br#"{"verdict":"changes"}"#);
        let (st, _) = s
            .send(
                Method::POST,
                &post,
                Some(WEB_TOKEN),
                Some(&signed),
                Some(&body),
            )
            .await;
        assert_eq!(st, 401);
        assert!(s
            .f
            .stores
            .approvals
            .recommendations(&[id])
            .await
            .unwrap()
            .is_empty());
        let anonymous: Vec<_> = s
            .all_events()
            .await
            .into_iter()
            .filter(|e| e.user == "anonymous" && e.outcome == Outcome::Denied)
            .map(|e| e.detail.unwrap_or_default())
            .collect();
        assert!(
            anonymous.contains(&"forbidden".to_string()),
            "{anonymous:?}"
        );
        assert!(
            anonymous.contains(&"bad_assertion".to_string()),
            "{anonymous:?}"
        );
    }
);

both!(
    recommending_needs_assertion_require,
    Assert::InsecureOff,
    |s| {
        let id = jira_draft(&s.f, "ana", "tsk_a1", Sensitivity::Public).await;
        // Reads still need the signed assertion, and work with one.
        let (st, read) = s.read("rev", &id).await;
        assert_eq!(st, 200, "{read}");
        let (st, _) = s
            .send(
                Method::GET,
                &format!("/internal/reviews/{id}?user=rev"),
                Some(WEB_TOKEN),
                None,
                None,
            )
            .await;
        assert_eq!(st, 401);
        let (st, b) = s
        .recommend("rev", &id, json!({"verdict": "approve", "draft_revision": read["revision"], "args_hash": read["args_hash"]}))
        .await;
        assert_eq!(
            (st, b["code"].as_str()),
            (503, Some("assertion_required")),
            "{b}"
        );
        assert!(s
            .f
            .stores
            .approvals
            .recommendations(&[id])
            .await
            .unwrap()
            .is_empty());
    }
);

both!(
    every_call_is_audited_and_never_the_note,
    Assert::Require,
    |s| {
        let id = jira_draft(&s.f, "ana", "tsk_a1", Sensitivity::Public).await;
        let (st, _) = s.web("rev", Method::GET, "/internal/reviews", None).await;
        assert_eq!(st, 200);
        let (st, _) = s
            .read_and_recommend("rev", &id, "approve", Some(NOTE))
            .await;
        assert_eq!(st, 200);
        let item = s.f.stores.approvals.get(&id).await.unwrap().unwrap();
        let rec = s.events("approvals.recommend").await;
        let ok: Vec<_> = rec.iter().filter(|e| e.outcome == Outcome::Ok).collect();
        assert_eq!(ok.len(), 1);
        assert_eq!(ok[0].user, "rev");
        assert_eq!(ok[0].client, Client::Web);
        assert_eq!(ok[0].output_ref.as_deref(), Some(id.as_str()));
        assert_eq!(ok[0].args_hash.as_deref(), Some(item.args_hash.as_str()));
        assert_eq!(ok[0].detail.as_deref(), Some("verdict approve revision 1"));
        assert_eq!(s.events("approvals.review_read").await.len(), 1);
        assert_eq!(s.events("approvals.review_inbox").await.len(), 1);
        for e in s.all_events().await {
            let text = serde_json::to_string(&e).unwrap();
            assert!(!text.contains("rollout date"), "{text}");
        }
    }
);

/// The 90-day rule: a reviewer's audit events are ordinary events, deleted by the purge at 90
/// days like the rest (Postgres: the real delete path).
#[tokio::test]
async fn recommendation_events_purge_at_ninety_days() {
    let Some(s) = start(Assert::Require, true).await else {
        return;
    };
    let now = Utc::now();
    for (days, outcome) in [(91, Outcome::Ok), (89, Outcome::Denied)] {
        s.f.svc
            .record_review(
                "rev",
                "approvals.recommend",
                outcome,
                About::Id("apr_old"),
                Some("verdict approve revision 1".into()),
                now - Duration::days(days),
            )
            .await;
    }
    let ran = s.f.stores.audit.purge(90, 1000, now).await.unwrap();
    assert!(ran.is_some());
    let left: Vec<_> = s.events("approvals.recommend").await;
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].outcome, Outcome::Denied);
    s.stop().await;
}
