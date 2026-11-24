//! Slice U3 of the user database (`docs/user-database-spec.md` §12): the tool server decides
//! from the database. Deactivation (the cap, the grace, admin revoked, sessions ended,
//! credentials suspended, drafts held), the audience check, the dual-role marker, and the
//! refusal codes, against both stores where a store does the work.

mod common;

use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use common::Fixture;
use pmos_contract::model::Role;
use pmos_toolserver::approvals::{Decision, Status, Verdict, SYSTEM_DECIDER};
use pmos_toolserver::audit::{Client, Outcome};
use pmos_toolserver::connectors::fake::{SLACK_ALL_TEAM, SLACK_PM};
use pmos_toolserver::ledger::{LedgerQuery, Reason};
use pmos_toolserver::service::{AudienceSummary, DecisionError, SessionError};
use pmos_toolserver::users::{
    Actor, ChangeKind, HostUser, NewUser, Op, Refusal, UserError, UserRole,
};
use pmos_toolserver::vault::{KeyRing, Provider, SealedCredential, Vault};
use pmos_toolserver::{router, AppConfig};
use reqwest::Method;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

const SERVICE_TOKEN: &str = "test-service-token-0123456789abcdef";
const BOT_TOKEN: &str = "test-bot-token-0123456789abcdef-0123";

fn host() -> Actor {
    Actor::host(HostUser::parse("ops").unwrap())
}

/// `ada` admin, `aud` auditor, `p1`…`p6` PMs.
async fn fixture(postgres: bool) -> Option<(Fixture, Option<common::TestPg>)> {
    common::roles_fixture(
        &[
            ("ada", Role::Admin),
            ("aud", Role::Auditor),
            ("p1", Role::Pm),
            ("p2", Role::Pm),
            ("p3", Role::Pm),
            ("p4", Role::Pm),
            ("p5", Role::Pm),
            ("p6", Role::Pm),
        ],
        postgres,
    )
    .await
}

struct Server {
    base: String,
    http: reqwest::Client,
    cancel: CancellationToken,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

async fn serve(f: &Fixture) -> Server {
    let cancel = CancellationToken::new();
    let app = router(
        Arc::clone(&f.svc),
        AppConfig {
            service_token: SERVICE_TOKEN.into(),
            bot: Some(pmos_toolserver::app::BotAccess {
                token: BOT_TOKEN.into(),
                skills: vec!["docs".into()],
            }),
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
    Server {
        base,
        http: reqwest::Client::new(),
        cancel,
    }
}

impl Server {
    async fn call(
        &self,
        method: Method,
        path: &str,
        token: &str,
        body: Option<Value>,
    ) -> (u16, Value) {
        let mut req = self
            .http
            .request(method, format!("{}{path}", self.base))
            .bearer_auth(token);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let res = req.send().await.unwrap();
        let status = res.status().as_u16();
        (status, res.json().await.unwrap_or(Value::Null))
    }

    /// The Slack bot's `remove <handle>`, decided by `by`.
    async fn remove(&self, handle: &str, by: &str) -> (u16, Value) {
        self.call(
            Method::POST,
            &format!("/internal/members/{handle}/remove"),
            BOT_TOKEN,
            Some(json!({"decided_by": by})),
        )
        .await
    }
}

fn sealed(user: &str, provider: Provider) -> SealedCredential {
    SealedCredential {
        user: user.into(),
        provider,
        key_id: "k1".into(),
        access_token: "v1.k1.placeholder.placeholder".into(),
        refresh_token: None,
        expires_at: None,
        scopes: String::new(),
        cloud_id: None,
        site_url: None,
        external_id: None,
        updated_at: Utc::now(),
        suspended_at: None,
    }
}

/// A pending draft of `user`'s.
async fn draft(f: &Fixture, user: &str, run: &str) -> String {
    let s = f
        .svc
        .create_session(user, "internal-brief", run, None, Utc::now())
        .await
        .unwrap();
    let out = f
        .svc
        .call(
            &s,
            "jira_create_issue",
            json!({"project": "PM", "issue_type": "Story", "summary": "SCIM"}),
            Utc::now(),
        )
        .await
        .unwrap();
    out["approval_id"].as_str().unwrap().to_string()
}

/// The audit log as the fixture's backend stores it, oldest first.
async fn events(f: &Fixture) -> Vec<pmos_toolserver::audit::AuditEvent> {
    let query = pmos_toolserver::audit::AuditQuery {
        limit: pmos_toolserver::audit::MAX_PAGE,
        ..Default::default()
    };
    let mut found: Vec<_> = f
        .stores
        .audit
        .search(&query)
        .await
        .unwrap()
        .expect("searchable")
        .into_iter()
        .map(|s| s.event)
        .collect();
    found.reverse();
    found
}

fn code(e: UserError) -> &'static str {
    e.refusal().expect("a refusal").code()
}

// ----- the cap --------------------------------------------------------------------------------

/// At most 3 deactivations an hour from the web and Slack clients together: the 4th is
/// `429 deactivation_limit`, recorded, and the host command still works. A refused attempt
/// never counts, and a reactivation doesn't reset the window (second review, point 4).
async fn the_cap(postgres: bool) {
    let Some((f, pg)) = fixture(postgres).await else {
        return;
    };
    let srv = serve(&f).await;
    // Two deactivations, then a refused one (an Admin or Auditor from Slack): not counted.
    assert_eq!(srv.remove("p1", "ada").await.0, 200);
    assert_eq!(srv.remove("p2", "ada").await.0, 200);
    let (status, body) = srv.remove("aud", "ada").await;
    assert_eq!(
        (status, body["code"].as_str()),
        (403, Some("protected_not_from_slack")),
        "{body}"
    );
    let (status, body) = srv.remove("p2", "ada").await;
    assert_eq!(
        (status, body["code"].as_str()),
        (409, Some("already_deactivated"))
    );
    // The third still goes through.
    assert_eq!(srv.remove("p3", "ada").await.0, 200);
    // Reactivating one doesn't give the slot back.
    f.stores
        .users
        .apply(
            &host(),
            Op::Reactivate {
                handle: "p1".into(),
            },
            Utc::now(),
        )
        .await
        .unwrap();
    let (status, body) = srv.remove("p4", "ada").await;
    assert_eq!(
        (status, body["code"].as_str()),
        (429, Some("deactivation_limit")),
        "{body}"
    );
    assert!(body["error"]
        .as_str()
        .unwrap()
        .contains("pmos-admin users deactivate"));
    assert!(
        f.svc.people().allows("p4").await.unwrap(),
        "nothing changed"
    );
    // The refused fourth doesn't count either: still three in the window.
    assert_eq!(
        f.stores
            .users
            .deactivations_in_window(Utc::now())
            .await
            .unwrap(),
        3
    );
    let denied = events(&f)
        .await
        .into_iter()
        .rev()
        .find(|e| e.tool == "users.deactivate" && e.outcome == Outcome::Denied)
        .unwrap();
    assert_eq!(denied.detail.as_deref(), Some("deactivation_limit"));
    assert_eq!(denied.output_ref.as_deref(), Some("p4"));
    // The host isn't capped.
    f.stores
        .users
        .apply(
            &host(),
            Op::Deactivate {
                handle: "p4".into(),
            },
            Utc::now(),
        )
        .await
        .unwrap();
    assert!(!f.svc.people().allows("p4").await.unwrap());
    // The overview shows it.
    let (status, ov) = srv
        .call(
            Method::GET,
            "/internal/admin/overview?user=ada",
            SERVICE_TOKEN,
            None,
        )
        .await;
    assert_eq!(status, 200, "{ov}");
    assert_eq!(ov["users"]["deactivations_last_hour"], 3);
    assert_eq!(ov["users"]["deactivation_limit_reached"], true);
    drop(srv);
    if let Some(pg) = pg {
        pg.drop_schema().await;
    }
}

#[tokio::test]
async fn the_cap_in_memory() {
    the_cap(false).await;
}

#[tokio::test]
async fn the_cap_in_postgres() {
    the_cap(true).await;
}

// ----- the grace ------------------------------------------------------------------------------

/// Deactivate, then reactivate within 24 hours: the linked accounts work again and the held
/// draft can be decided. Deactivate again and let 24 hours pass: the sweeper deletes the
/// accounts and closes the draft (`owner_deactivated`), and a reactivation after the grace
/// brings back neither. A former admin comes back without `admin`.
async fn the_grace(postgres: bool) {
    let Some((f, pg)) = fixture(postgres).await else {
        return;
    };
    let t0 = Utc::now();
    let vault = Vault::new(
        KeyRing::from_spec(&KeyRing::generate_spec("k1")).unwrap(),
        f.stores.credentials.clone(),
    );
    // p1: a linked account (sealed for real) and a pending draft.
    vault
        .put(&pmos_toolserver::vault::Credential {
            user: "p1".into(),
            provider: Provider::Atlassian,
            access_token: pmos_toolserver::vault::Secret::new("at-p1"),
            refresh_token: None,
            expires_at: None,
            scopes: String::new(),
            cloud_id: None,
            site_url: None,
            external_id: None,
            updated_at: t0,
        })
        .await
        .unwrap();
    let held = draft(&f, "p1", "run-held").await;
    let session = f
        .svc
        .create_session("p1", "internal-brief", "run-open", None, t0)
        .await
        .unwrap();

    f.svc
        .people()
        .apply(
            &Actor::web("ada"),
            Op::Deactivate {
                handle: "p1".into(),
            },
            t0,
        )
        .await
        .unwrap();
    // At once: no session, the open one gone, the account suspended and never unsealed, the
    // draft held (still pending, but its owner can't decide it).
    assert!(f.svc.session(&session.token, t0).await.unwrap().is_none());
    assert!(matches!(
        f.svc
            .create_session("p1", "internal-brief", "run-x", None, t0)
            .await,
        Err(SessionError::NotAMember(_))
    ));
    let stored = f
        .stores
        .credentials
        .get("p1", Provider::Atlassian)
        .await
        .unwrap()
        .unwrap();
    assert!(stored.suspended_at.is_some());
    assert!(vault
        .get("p1", Provider::Atlassian)
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        f.svc.approval(&held).await.unwrap().unwrap().status,
        Status::Pending
    );
    assert!(matches!(
        f.svc.decide(&held, "p1", Decision::Reject, None, t0).await,
        Err(DecisionError::NotAMember(_))
    ));
    // The sweeper has nothing to do within the grace.
    assert_eq!(
        f.svc
            .sweep_deactivated(t0 + Duration::hours(23))
            .await
            .unwrap(),
        0
    );

    // Reactivated within the grace: the account works, the draft can be decided.
    let t1 = t0 + Duration::hours(23);
    f.svc
        .people()
        .apply(
            &Actor::web("ada"),
            Op::Reactivate {
                handle: "p1".into(),
            },
            t1,
        )
        .await
        .unwrap();
    let back = vault.get("p1", Provider::Atlassian).await.unwrap().unwrap();
    assert_eq!(back.access_token.expose(), "at-p1");
    assert!(f.svc.people().allows("p1").await.unwrap());

    // Deactivated again and the grace passes: the sweeper deletes and closes.
    let t2 = t1 + Duration::minutes(1);
    f.stores
        .users
        .apply(
            &host(),
            Op::Deactivate {
                handle: "p1".into(),
            },
            t2,
        )
        .await
        .unwrap();
    let t3 = t2 + Duration::hours(24);
    assert_eq!(f.svc.sweep_deactivated(t3).await.unwrap(), 1);
    assert_eq!(f.svc.sweep_deactivated(t3).await.unwrap(), 0, "once");
    assert!(f.stores.credentials.list("p1").await.unwrap().is_empty());
    let closed = f.svc.approval(&held).await.unwrap().unwrap();
    assert_eq!(closed.status, Status::Rejected);
    assert_eq!(closed.decided_by.as_deref(), Some(SYSTEM_DECIDER));
    let row = f
        .stores
        .approvals
        .ledger(&LedgerQuery::all(t0 - Duration::days(30)))
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.id == held)
        .unwrap();
    assert_eq!(row.reason, Some(Reason::OwnerDeactivated));
    let changes = f.stores.users.role_changes(Some("p1"), 20).await.unwrap();
    assert!(changes
        .iter()
        .any(|c| c.change == ChangeKind::DeleteCredentials));
    assert!(changes
        .iter()
        .any(|c| c.change == ChangeKind::SuspendCredentials));
    // Reactivated after the grace: active again, but nothing comes back.
    f.stores
        .users
        .apply(
            &host(),
            Op::Reactivate {
                handle: "p1".into(),
            },
            t3 + Duration::hours(1),
        )
        .await
        .unwrap();
    assert!(f.stores.credentials.list("p1").await.unwrap().is_empty());
    assert_eq!(
        f.svc.approval(&held).await.unwrap().unwrap().status,
        Status::Rejected
    );

    // p2: deactivated with a draft and an account, never reactivated; reactivating after
    // the grace before the sweeper ran deletes them all the same.
    f.stores
        .credentials
        .put(&sealed("p2", Provider::Slack))
        .await
        .unwrap();
    let p2_draft = draft(&f, "p2", "run-p2").await;
    f.stores
        .users
        .apply(
            &host(),
            Op::Deactivate {
                handle: "p2".into(),
            },
            t0,
        )
        .await
        .unwrap();
    f.stores
        .users
        .apply(
            &host(),
            Op::Reactivate {
                handle: "p2".into(),
            },
            t0 + Duration::hours(25),
        )
        .await
        .unwrap();
    assert!(f.stores.credentials.list("p2").await.unwrap().is_empty());
    assert_eq!(
        f.svc.approval(&p2_draft).await.unwrap().unwrap().status,
        Status::Rejected
    );

    // A former admin: made admin on the host, deactivated (admin revoked, recorded),
    // reactivated without it.
    f.stores
        .users
        .apply(
            &host(),
            Op::GrantAdmin {
                handle: "p3".into(),
                entra: pmos_toolserver::users::AdminEntra::Oid(pmos_toolserver::users::EntraKey {
                    tid: "11111111-1111-1111-1111-111111111111".into(),
                    oid: "33333333-3333-3333-3333-333333333333".into(),
                }),
            },
            t0,
        )
        .await
        .unwrap();
    assert!(f.svc.people().is_admin("p3").await.unwrap());
    f.svc
        .people()
        .apply(
            &Actor::web("ada"),
            Op::Deactivate {
                handle: "p3".into(),
            },
            t0,
        )
        .await
        .unwrap();
    f.svc
        .people()
        .apply(
            &Actor::web("ada"),
            Op::Reactivate {
                handle: "p3".into(),
            },
            t0 + Duration::hours(1),
        )
        .await
        .unwrap();
    let p3 = f.svc.people().user("p3").await.unwrap().unwrap();
    assert!(p3.active());
    assert!(
        !p3.has(UserRole::Admin),
        "reactivation never restores admin"
    );
    assert!(p3.has(UserRole::Pm));
    assert!(!f.svc.people().is_admin("p3").await.unwrap());
    let revoke = f
        .stores
        .users
        .role_changes(Some("p3"), 20)
        .await
        .unwrap()
        .into_iter()
        .find(|c| c.change == ChangeKind::Revoke)
        .unwrap();
    assert_eq!(revoke.role, Some(UserRole::Admin));
    assert_eq!(revoke.detail.as_deref(), Some("on deactivation"));
    if let Some(pg) = pg {
        pg.drop_schema().await;
    }
}

#[tokio::test]
async fn the_grace_in_memory() {
    the_grace(false).await;
}

#[tokio::test]
async fn the_grace_in_postgres() {
    the_grace(true).await;
}

// ----- refusals from the Slack bot client ------------------------------------------------------

/// Through the unsigned Slack bot client: a join is `pm` only, an Admin or Auditor is never
/// deactivated, and nothing else about users changes.
#[tokio::test]
async fn the_slack_clients_refusal_codes() {
    let Some((f, _)) = fixture(false).await else {
        return;
    };
    let users = &f.stores.users;
    let join = |handle: &str, roles: &[UserRole]| Op::CreateUser {
        user: NewUser {
            handle: handle.into(),
            slack_id: Some("U0NEW001".into()),
            roles: roles.iter().copied().collect(),
            ..Default::default()
        },
    };
    let bot = Actor::bot("ada");
    let now = Utc::now();
    for (roles, want) in [
        (&[UserRole::Admin][..], "admin_not_from_slack"),
        (&[UserRole::Pm, UserRole::Admin][..], "admin_not_from_slack"),
        (
            &[UserRole::Pm, UserRole::Reviewer][..],
            "role_not_from_slack",
        ),
        (&[UserRole::Auditor][..], "role_not_from_slack"),
    ] {
        let e = users
            .apply(&bot, join("nia", roles), now)
            .await
            .unwrap_err();
        assert_eq!(code(e), want, "{roles:?}");
    }
    users
        .apply(&bot, join("nia", &[UserRole::Pm]), now)
        .await
        .unwrap();
    let nia = f.svc.people().user("nia").await.unwrap().unwrap();
    assert_eq!(nia.created_via, pmos_toolserver::users::Via::Bot);
    for target in ["ada", "aud"] {
        let actor = if target == "ada" {
            Actor::bot("fixture-x")
        } else {
            bot.clone()
        };
        let e = users
            .apply(
                &actor,
                Op::Deactivate {
                    handle: target.into(),
                },
                now,
            )
            .await
            .unwrap_err();
        let want = if target == "ada" {
            "forbidden"
        } else {
            "protected_not_from_slack"
        };
        assert_eq!(code(e), want, "{target}");
    }
    // An admin, as the target, from another admin through Slack: protected too.
    users
        .apply(
            &host(),
            Op::GrantAdmin {
                handle: "p6".into(),
                entra: pmos_toolserver::users::AdminEntra::Oid(pmos_toolserver::users::EntraKey {
                    tid: "11111111-1111-1111-1111-111111111111".into(),
                    oid: "66666666-6666-6666-6666-666666666666".into(),
                }),
            },
            now,
        )
        .await
        .unwrap();
    let e = users
        .apply(
            &bot,
            Op::Deactivate {
                handle: "p6".into(),
            },
            now,
        )
        .await
        .unwrap_err();
    assert_eq!(e.refusal(), Some(Refusal::ProtectedNotFromSlack));
    // Each refusal is recorded as a denied event with its code.
    let events = events(&f).await;
    for want in [
        "admin_not_from_slack",
        "role_not_from_slack",
        "protected_not_from_slack",
    ] {
        assert!(
            events
                .iter()
                .any(|e| e.outcome == Outcome::Denied && e.detail.as_deref() == Some(want)),
            "{want}"
        );
    }
}

// ----- MCP sessions ---------------------------------------------------------------------------

/// An open session dies on deactivation (the same transaction ends it), and with any change
/// that means its user may no longer use PM OS (re-checked on each call, ≤ 5 s).
async fn sessions_die(postgres: bool) {
    let Some((f, pg)) = fixture(postgres).await else {
        return;
    };
    let now = Utc::now();
    let s1 = f
        .svc
        .create_session("p1", "internal-brief", "run-1", None, now)
        .await
        .unwrap();
    let s2 = f
        .svc
        .create_session("p2", "internal-brief", "run-2", None, now)
        .await
        .unwrap();
    assert!(f.svc.session(&s1.token, now).await.unwrap().is_some());
    // Deactivated on the host (another process in production: no cache to empty here).
    f.stores
        .users
        .apply(
            &host(),
            Op::Deactivate {
                handle: "p1".into(),
            },
            now,
        )
        .await
        .unwrap();
    assert!(f.svc.session(&s1.token, now).await.unwrap().is_none());
    // p2 becomes auditor-only: the session isn't revoked, but the re-check refuses it once
    // the cached directory is older than 5 seconds (forgotten here).
    f.stores
        .users
        .apply(
            &host(),
            Op::SetRoles {
                handle: "p2".into(),
                roles: [UserRole::Auditor].into(),
            },
            now,
        )
        .await
        .unwrap();
    f.svc.people().invalidate();
    assert!(f.svc.session(&s2.token, now).await.unwrap().is_none());
    let err = f
        .svc
        .create_session("aud", "internal-brief", "run-a", None, now)
        .await
        .unwrap_err();
    assert_eq!(err, SessionError::NotAMember("aud".into()), "auditor-only");
    if let Some(pg) = pg {
        pg.drop_schema().await;
    }
}

#[tokio::test]
async fn sessions_die_in_memory() {
    sessions_die(false).await;
}

#[tokio::test]
async fn sessions_die_in_postgres() {
    sessions_die(true).await;
}

// ----- the audience ---------------------------------------------------------------------------

/// A restricted draft to #pm-team (evals, ben, cy): allowed while all three are active PMs;
/// blocked once cy holds only `auditor`, and once ben is deactivated.
#[tokio::test]
async fn auditor_only_and_deactivated_members_are_outsiders() {
    let (f, _) = common::reply_fixture(false).await.unwrap();
    let now = common::now();
    let restricted = |run: &'static str, ts: &'static str| {
        let f = &f;
        async move {
            let s = f
                .svc
                .create_session(SLACK_PM, "reply-test", run, None, now)
                .await
                .unwrap();
            f.svc
                .call(
                    &s,
                    "context_read",
                    json!({"path": "context/partners/globex-nda.md"}),
                    now,
                )
                .await
                .unwrap();
            let out = f
                .svc
                .call(
                    &s,
                    "slack_post_reply",
                    json!({"channel": SLACK_ALL_TEAM, "thread_ts": ts, "text": "Under NDA"}),
                    now,
                )
                .await
                .unwrap();
            out["approval_id"].as_str().unwrap().to_string()
        }
    };
    let approve = |id: String| {
        let f = &f;
        async move {
            let shown = f.svc.approval(&id).await.unwrap().unwrap().args_hash[..16].to_string();
            let verdict = Verdict {
                expected_args_hash: Some(shown),
                ..Verdict::new(Decision::Approve, None)
            };
            f.svc
                .decide_verdict(Client::Bot, &id, SLACK_PM, verdict, now)
                .await
        }
    };
    let blocked = |r: Result<_, DecisionError>, outsiders: usize| {
        assert_eq!(
            r.unwrap_err(),
            DecisionError::AudienceBlocked(AudienceSummary {
                members: 3,
                outsiders,
                external: false
            })
        );
    };
    // cy holds only `auditor`: an outsider.
    f.stores
        .users
        .apply(
            &host(),
            Op::SetRoles {
                handle: "cy".into(),
                roles: [UserRole::Auditor].into(),
            },
            now,
        )
        .await
        .unwrap();
    blocked(
        approve(restricted("tsk_a1", "1731600000.000100").await).await,
        1,
    );
    // ben deactivated too: two.
    f.stores
        .users
        .apply(
            &host(),
            Op::Deactivate {
                handle: "ben".into(),
            },
            now,
        )
        .await
        .unwrap();
    let id = restricted("tsk_a2", "1731600000.000200").await;
    blocked(approve(id.clone()).await, 2);
    assert_eq!(
        f.svc.approval(&id).await.unwrap().unwrap().status,
        Status::Pending
    );
    assert!(f.slack.writes().is_empty());
    let e = f.audit.events().last().cloned().unwrap();
    assert_eq!(e.outcome, Outcome::Denied);
    assert!(e.detail.unwrap().contains("2 not on the product team"));
    // Both back as PMs: allowed.
    for (h, op) in [
        (
            "ben",
            Op::Reactivate {
                handle: "ben".into(),
            },
        ),
        (
            "cy",
            Op::SetRoles {
                handle: "cy".into(),
                roles: [UserRole::Pm].into(),
            },
        ),
    ] {
        f.stores
            .users
            .apply(&host(), op, now)
            .await
            .unwrap_or_else(|e| panic!("{h}: {e}"));
    }
    let id = restricted("tsk_a3", "1731600000.000300").await;
    approve(id).await.unwrap();
}

// ----- the dual-role marker -------------------------------------------------------------------

/// Admin and Auditor on one person under the pilot exception: their audit-log reads and
/// exports carry `dual_role admin+auditor`; once the exception lapses, `403 role_conflict`.
#[tokio::test]
async fn a_dual_role_audit_read_is_marked() {
    let Some((f, _)) = fixture(false).await else {
        return;
    };
    let srv = serve(&f).await;
    let read = |user: &'static str, export: bool| {
        let srv = &srv;
        async move {
            let q = if export { "&purpose=export" } else { "" };
            srv.call(
                Method::GET,
                &format!("/internal/admin/audit?user={user}{q}"),
                SERVICE_TOKEN,
                None,
            )
            .await
        }
    };
    // A plain admin's read: no marker.
    assert_eq!(read("ada", false).await.0, 200);
    let last = |tool: &str| {
        f.audit
            .events()
            .into_iter()
            .rev()
            .find(|e| e.tool == tool)
            .unwrap()
    };
    assert_eq!(last("admin.audit").detail, None);
    // The pilot exception, then ada gains auditor.
    let until: DateTime<Utc> = Utc::now() + Duration::seconds(3);
    f.stores
        .users
        .apply(&host(), Op::ExceptionOn { until }, Utc::now())
        .await
        .unwrap();
    f.stores
        .users
        .apply(
            &host(),
            Op::SetRoles {
                handle: "ada".into(),
                roles: [UserRole::Admin, UserRole::Auditor].into(),
            },
            Utc::now(),
        )
        .await
        .unwrap();
    for export in [false, true] {
        let (status, body) = read("ada", export).await;
        assert_eq!(status, 200, "{body}");
        let tool = if export {
            "admin.audit_export"
        } else {
            "admin.audit"
        };
        let ev = last(tool);
        assert_eq!((ev.user.as_str(), ev.outcome), ("ada", Outcome::Ok));
        assert_eq!(ev.detail.as_deref(), Some("dual_role admin+auditor"));
    }
    // The marker is served to the reader (a fixed format).
    let (_, body) = read("aud", false).await;
    assert!(body["events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["detail"] == "dual_role admin+auditor"));
    // The overview lists them, with the exception's end.
    let (_, ov) = srv
        .call(
            Method::GET,
            "/internal/admin/overview?user=aud",
            SERVICE_TOKEN,
            None,
        )
        .await;
    assert_eq!(ov["users"]["dual_role"], json!(["ada"]));
    assert_eq!(ov["users"]["pilot_exception"]["lapsed"], false);
    assert_eq!(ov["users"]["pilot_exception"]["ending_soon"], true);
    // Lapsed: refused, recorded; admin powers stay.
    tokio::time::sleep(std::time::Duration::from_millis(3200)).await;
    let (status, body) = read("ada", false).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (403, Some("role_conflict")),
        "{body}"
    );
    assert_eq!(last("admin.audit").outcome, Outcome::Denied);
    let (status, _) = srv
        .call(
            Method::GET,
            "/internal/admin/overview?user=ada",
            SERVICE_TOKEN,
            None,
        )
        .await;
    assert_eq!(status, 200, "admin powers stay");
    let (_, ov) = srv
        .call(
            Method::GET,
            "/internal/admin/overview?user=ada",
            SERVICE_TOKEN,
            None,
        )
        .await;
    assert_eq!(ov["users"]["role_conflicts"], json!(["ada"]));
}

// ----- the admin reads read the database ------------------------------------------------------

/// Viewers are checked in the database: an auditor reads the audit log, a deactivated
/// admin reads nothing, and the accounts screen lists the database's users with status.
#[tokio::test]
async fn the_admin_reads_check_the_database() {
    let Some((f, _)) = fixture(false).await else {
        return;
    };
    let srv = serve(&f).await;
    let get = |path: String| {
        let srv = &srv;
        async move { srv.call(Method::GET, &path, SERVICE_TOKEN, None).await }
    };
    assert_eq!(get("/internal/admin/audit?user=aud".into()).await.0, 200);
    assert_eq!(get("/internal/admin/audit?user=p1".into()).await.0, 403);
    f.stores
        .users
        .apply(
            &host(),
            Op::Deactivate {
                handle: "p1".into(),
            },
            Utc::now(),
        )
        .await
        .unwrap();
    let (status, body) = get("/internal/admin/accounts?user=ada".into()).await;
    assert_eq!(status, 200, "{body}");
    let p1 = body["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["handle"] == "p1")
        .unwrap()
        .clone();
    assert_eq!(p1["status"], "deactivated");
    assert_eq!(p1["roles"], json!(["pm"]));
    // A join through Slack shows on the overview as unconfirmed.
    f.stores
        .users
        .apply(
            &Actor::bot("ada"),
            Op::CreateUser {
                user: NewUser {
                    handle: "nia".into(),
                    sign_in_name: Some("nia@example.com".into()),
                    atlassian_email: Some("nia@example.com".into()),
                    slack_id: Some("U0NIA001".into()),
                    roles: [UserRole::Pm].into(),
                    ..Default::default()
                },
            },
            Utc::now(),
        )
        .await
        .unwrap();
    let (_, ov) = get("/internal/admin/overview?user=ada".into()).await;
    assert_eq!(ov["users"]["sign_in_unconfirmed"], json!(["nia"]));
    assert_eq!(ov["users"]["deactivated"], 1);
    assert!(
        ov.to_string().find("nia@example.com").is_none(),
        "no addresses"
    );
    // An admin deactivated on the host reads nothing.
    f.stores
        .users
        .apply(
            &host(),
            Op::GrantAdmin {
                handle: "p5".into(),
                entra: pmos_toolserver::users::AdminEntra::Oid(pmos_toolserver::users::EntraKey {
                    tid: "11111111-1111-1111-1111-111111111111".into(),
                    oid: "55555555-5555-5555-5555-555555555555".into(),
                }),
            },
            Utc::now(),
        )
        .await
        .unwrap();
    assert_eq!(get("/internal/admin/overview?user=p5".into()).await.0, 200);
    f.stores
        .users
        .apply(
            &host(),
            Op::Deactivate {
                handle: "p5".into(),
            },
            Utc::now(),
        )
        .await
        .unwrap();
    assert_eq!(get("/internal/admin/overview?user=p5".into()).await.0, 403);
}

// ----- the console's single-item read ---------------------------------------------------------

/// A suspended credential is never unsealed, by any path: the vault refuses to read it.
#[tokio::test]
async fn a_suspended_credential_is_never_unsealed() {
    let Some((f, _)) = fixture(false).await else {
        return;
    };
    let vault = Vault::new(
        KeyRing::from_spec(&KeyRing::generate_spec("k1")).unwrap(),
        f.stores.credentials.clone(),
    );
    vault
        .put(&pmos_toolserver::vault::Credential {
            user: "p1".into(),
            provider: Provider::Github,
            access_token: pmos_toolserver::vault::Secret::new("ghu-p1"),
            refresh_token: None,
            expires_at: None,
            scopes: String::new(),
            cloud_id: None,
            site_url: None,
            external_id: None,
            updated_at: Utc::now(),
        })
        .await
        .unwrap();
    assert_eq!(
        f.stores
            .credentials
            .suspend("p1", Utc::now())
            .await
            .unwrap(),
        1
    );
    assert!(vault.get("p1", Provider::Github).await.unwrap().is_none());
    // A later put (a refresh, a re-link) keeps it suspended.
    vault
        .put(&pmos_toolserver::vault::Credential {
            user: "p1".into(),
            provider: Provider::Github,
            access_token: pmos_toolserver::vault::Secret::new("ghu-p1-2"),
            refresh_token: None,
            expires_at: None,
            scopes: String::new(),
            cloud_id: None,
            site_url: None,
            external_id: None,
            updated_at: Utc::now(),
        })
        .await
        .unwrap();
    assert!(vault.get("p1", Provider::Github).await.unwrap().is_none());
    assert_eq!(f.stores.credentials.restore("p1").await.unwrap(), 1);
    assert!(vault.get("p1", Provider::Github).await.unwrap().is_some());
}

// ----- Atlassian and GitHub links -------------------------------------------------------------

/// After the first link the account id decides: a second Atlassian or GitHub account is
/// `different_account`, and one person's account id is never another's.
#[tokio::test]
async fn a_relink_with_another_account_id_is_refused() {
    let Some((f, _)) = fixture(false).await else {
        return;
    };
    let users = &f.stores.users;
    let link = |h: &str, kind, subject: &str| Op::LinkAccount {
        handle: h.into(),
        kind,
        subject: subject.into(),
    };
    let now = Utc::now();
    use pmos_toolserver::users::IdentityKind::{Atlassian, Github};
    users
        .apply(&Actor::link("p1"), link("p1", Atlassian, "acc-1"), now)
        .await
        .unwrap();
    let e = users
        .apply(&Actor::link("p1"), link("p1", Atlassian, "acc-1"), now)
        .await
        .unwrap_err();
    assert_eq!(
        e.refusal(),
        Some(Refusal::Unchanged),
        "the same account again"
    );
    let e = users
        .apply(&Actor::link("p1"), link("p1", Atlassian, "acc-2"), now)
        .await
        .unwrap_err();
    assert_eq!(code(e), "different_account");
    let e = users
        .apply(&Actor::link("p2"), link("p2", Atlassian, "acc-1"), now)
        .await
        .unwrap_err();
    assert_eq!(e.refusal(), Some(Refusal::Taken));
    users
        .apply(&Actor::link("p1"), link("p1", Github, "1001"), now)
        .await
        .unwrap();
    let e = users
        .apply(&Actor::link("p1"), link("p1", Github, "1002"), now)
        .await
        .unwrap_err();
    assert_eq!(code(e), "different_account");
    // Only the person, through OAuth; never for someone deactivated.
    let e = users
        .apply(&Actor::link("p1"), link("p3", Atlassian, "acc-3"), now)
        .await
        .unwrap_err();
    assert_eq!(e.refusal(), Some(Refusal::Invalid));
    users
        .apply(
            &host(),
            Op::Deactivate {
                handle: "p3".into(),
            },
            now,
        )
        .await
        .unwrap();
    let e = users
        .apply(&Actor::link("p3"), link("p3", Atlassian, "acc-3"), now)
        .await
        .unwrap_err();
    assert_eq!(e.refusal(), Some(Refusal::NotActive));
}

/// The sweeper fails closed and never touches an active person or one within the grace;
/// `EndGrace` is the system's alone.
#[tokio::test]
async fn the_grace_clean_up_is_the_systems_alone() {
    let Some((f, _)) = fixture(false).await else {
        return;
    };
    let now = Utc::now();
    f.stores
        .credentials
        .put(&sealed("p1", Provider::Slack))
        .await
        .unwrap();
    let e = f
        .stores
        .users
        .apply(
            &host(),
            Op::EndGrace {
                handle: "p1".into(),
            },
            now,
        )
        .await
        .unwrap_err();
    assert_eq!(e.refusal(), Some(Refusal::Invalid));
    let e = f
        .stores
        .users
        .apply(
            &Actor::system(),
            Op::EndGrace {
                handle: "p1".into(),
            },
            now,
        )
        .await
        .unwrap_err();
    assert_eq!(e.refusal(), Some(Refusal::NotDue), "active");
    assert_eq!(
        f.svc
            .sweep_deactivated(now + Duration::days(30))
            .await
            .unwrap(),
        0
    );
    assert_eq!(f.stores.credentials.list("p1").await.unwrap().len(), 1);
}

// ----- the stacks' seeds ----------------------------------------------------------------------

/// CI smoke's import file names an admin and imports into an empty database as
/// `import --bootstrap` does there.
#[tokio::test]
async fn the_ci_import_file_bootstraps() {
    use pmos_toolserver::users::UserStore;
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/test/ci-users.yaml");
    let req = pmos_toolserver::users::ImportRequest::parse(
        &std::fs::read(path).unwrap(),
        None,
        pmos_toolserver::users::ImportMode::Bootstrap,
    )
    .unwrap();
    let users = pmos_toolserver::users::MemoryUsers::default();
    users
        .apply(
            &Actor::import(Some(HostUser::parse("runner").unwrap())),
            Op::Import(Box::new(req)),
            Utc::now(),
        )
        .await
        .unwrap();
    let dir = users.directory(Utc::now()).await.unwrap();
    let ci = dir.get("ci").unwrap();
    assert!(ci.has(UserRole::Admin) && ci.has(UserRole::Pm));
}
