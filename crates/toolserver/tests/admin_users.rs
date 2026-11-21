//! Slice U5a of the user database (`docs/user-database-spec.md` §6, §8, §12): the Admin API
//! (`/internal/admin/users…`, `/internal/admin/reviewers/…`) and the console's admin listing
//! (`/internal/console/members`). Who may call, the assertion rules, every refusal the spec
//! names, the cap, the pilot exception's lapse, the auditor's projection, and that every write
//! is audited, against the in-memory store and Postgres.

mod common;

use std::sync::Arc;

use chrono::{Duration, Utc};
use common::{Fixture, TestPg};
use pmos_assertion::{unix_now, Claims, Key, KeyRing, RequestBinding};
use pmos_contract::model::Role;
use pmos_toolserver::app::BotAccess;
use pmos_toolserver::assertion::{parse_keys, Assertions, Mode, AUDIENCE, ISSUERS};
use pmos_toolserver::audit::{AuditQuery, Outcome};
use pmos_toolserver::users::link::EntraLink;
use pmos_toolserver::users::{
    Actor, ChangeKind, EntraKey, HostUser, IdentityKind, NewUser, Op, UserRole,
};
use pmos_toolserver::vault::{Provider, SealedCredential};
use pmos_toolserver::{router_full, AppConfig, PersonClients};
use reqwest::Method;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

const SERVICE_TOKEN: &str = "test-service-token-0123456789abcdef";
const BOT_TOKEN: &str = "test-bot-token-0123456789abcdef-0123";
const WEB_TOKEN: &str = "test-web-token-0123456789abcdef-01234";
const CONSOLE_TOKEN: &str = "test-console-token-0123456789abcdef-0";
// Test keys only: fixed bytes, never used anywhere real.
const WEB_KEY: &str = "d2ViLXRvb2xzZXJ2ZXItdGVzdC1rZXktMDEyMzQ1Njc=";
const CONSOLE_KEY: &str = "Y29uc29sZS10b29sc2VydmVyLXRlc3Qta2V5LTAxMjM=";
const TID: &str = "11111111-2222-3333-4444-555555555555";
const OID: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";

fn host() -> Actor {
    Actor::host(HostUser::parse("ops").unwrap())
}

/// How the server's assertions are set up.
enum Assert {
    Require,
    /// `PMOS_ASSERTION_MODE=off` with `PMOS_ASSERTION_INSECURE_OFF=1`: the development override.
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

/// `ada`, `ben` admins; `aud` auditor; `ana` (owns team bots `scout`, `blind`), `p1`…`p4` PMs;
/// `rev` reviewer.
async fn start(assert: Assert, postgres: bool) -> Option<Server> {
    let (f, pg) = common::roles_fixture(
        &[
            ("ada", Role::Admin),
            ("ben", Role::Admin),
            ("aud", Role::Auditor),
            ("ana", Role::Pm),
            ("p1", Role::Pm),
            ("p2", Role::Pm),
            ("p3", Role::Pm),
            ("p4", Role::Pm),
            ("rev", Role::Reviewer),
        ],
        postgres,
    )
    .await?;
    let keys = format!("web:toolserver:w1:{WEB_KEY},console:toolserver:c1:{CONSOLE_KEY}");
    let assertions = match assert {
        Assert::Require => {
            let keys = parse_keys(&keys, "the tool server", |iss, aud| {
                aud == AUDIENCE && ISSUERS.contains(&iss)
            })
            .unwrap();
            Assertions::new(Mode::Require, keys).unwrap()
        }
        Assert::InsecureOff => {
            let lookup = move |k: &str| match k {
                "PMOS_ASSERTION_MODE" => Some("off".to_string()),
                "PMOS_ASSERTION_INSECURE_OFF" => Some("1".to_string()),
                _ => None,
            };
            let a = Assertions::from_lookup(
                lookup,
                &[
                    ("web", "PMOS_WEB_TOKEN"),
                    ("console", "PMOS_CONSOLE_TS_TOKEN"),
                ],
            )
            .unwrap();
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

/// An assertion from `issuer` (`web` or `console`) for `sub`, with `roles`, bound to the
/// request.
fn sign(
    issuer: &str,
    sub: &str,
    roles: &[Role],
    method: &str,
    target: &str,
    body: &[u8],
) -> String {
    let (id, key) = if issuer == "web" {
        ("w1", WEB_KEY)
    } else {
        ("c1", CONSOLE_KEY)
    };
    let ring = KeyRing::new(Key::from_base64(id, key).unwrap());
    let claims = Claims::new(issuer, "toolserver", sub, roles, "s-test").unwrap();
    let req = RequestBinding::new(method, target, body).unwrap();
    ring.sign(&claims, unix_now(), 60, Some(&req)).unwrap()
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
        let a = sign("web", actor, &[], method.as_str(), &target, &bytes);
        self.send(method, &target, Some(WEB_TOKEN), Some(&a), body.as_ref())
            .await
    }

    async fn list(&self, viewer: &str) -> Value {
        let (s, b) = self
            .web(viewer, Method::GET, "/internal/admin/users", None)
            .await;
        assert_eq!(s, 200, "{b}");
        b
    }

    async fn version(&self) -> i64 {
        self.list("ada").await["version"].as_i64().unwrap()
    }

    fn user<'a>(list: &'a Value, handle: &str) -> &'a Value {
        list["users"]
            .as_array()
            .unwrap()
            .iter()
            .find(|u| u["handle"] == handle)
            .unwrap_or_else(|| panic!("{handle} in {list}"))
    }

    async fn roles(&self, actor: &str, handle: &str, roles: &[&str]) -> (u16, Value) {
        let v = self.version().await;
        self.web(
            actor,
            Method::PUT,
            &format!("/internal/admin/users/{handle}/roles"),
            Some(json!({"roles": roles, "expected_version": v})),
        )
        .await
    }

    async fn fields(&self, actor: &str, handle: &str, mut body: Value) -> (u16, Value) {
        body["expected_version"] = json!(self.version().await);
        self.web(
            actor,
            Method::PUT,
            &format!("/internal/admin/users/{handle}/fields"),
            Some(body),
        )
        .await
    }

    async fn status(&self, actor: &str, handle: &str, what: &str) -> (u16, Value) {
        self.web(
            actor,
            Method::POST,
            &format!("/internal/admin/users/{handle}/{what}"),
            None,
        )
        .await
    }

    /// The audit events recorded for `action`, newest first.
    async fn events(&self, action: &str) -> Vec<(String, Outcome, Option<String>, Option<String>)> {
        let q = AuditQuery {
            before: None,
            limit: 200,
            actor: None,
            action: Some(action.into()),
            outcome: None,
            from: None,
            to: None,
            persona: None,
        };
        self.f
            .stores
            .audit
            .search(&q)
            .await
            .unwrap()
            .unwrap()
            .into_iter()
            .map(|e| {
                (
                    e.event.user,
                    e.event.outcome,
                    e.event.output_ref,
                    e.event.detail,
                )
            })
            .collect()
    }

    async fn has_role(&self, handle: &str, role: UserRole) -> bool {
        self.f
            .stores
            .users
            .directory(Utc::now())
            .await
            .unwrap()
            .get(handle)
            .unwrap()
            .has(role)
    }
}

/// Each case runs on the in-memory store and, with `PMOS_TEST_DATABASE_URL`, Postgres.
macro_rules! both {
    ($name:ident, $assert:expr, |$s:ident, $pg:pat_param| $body:block) => {
        #[tokio::test]
        async fn $name() {
            for postgres in [false, true] {
                let Some(server) = start($assert, postgres).await else {
                    continue;
                };
                {
                    let $s = &server;
                    let $pg = postgres;
                    $body
                }
                server.stop().await;
            }
        }
    };
}

// --- Who may call ----------------------------------------------------------------------------

both!(
    only_the_web_token_reaches_the_admin_api,
    Assert::Require,
    |s, _| {
        for path in [
            "/internal/admin/users?user=ada",
            "/internal/admin/users/ana/deactivate?user=ada",
        ] {
            let method = if path.contains("deactivate") {
                Method::POST
            } else {
                Method::GET
            };
            for token in [SERVICE_TOKEN, CONSOLE_TOKEN, BOT_TOKEN] {
                let (st, _) = s.send(method.clone(), path, Some(token), None, None).await;
                assert_eq!(st, 403, "{path} with another client's token");
            }
            let (st, _) = s.send(method.clone(), path, None, None, None).await;
            assert_eq!(st, 401);
        }
        // The Slack bot keeps only what U3 gave it: its own capped `remove`.
        let (st, _) = s
            .send(
                Method::PUT,
                "/internal/admin/reviewers/rev?user=ada",
                Some(BOT_TOKEN),
                None,
                Some(&json!({"pms": ["ana"]})),
            )
            .await;
        assert_eq!(st, 403);
        assert!(s.has_role("ana", UserRole::Pm).await);
    }
);

both!(
    writes_need_an_assertion_for_the_actor,
    Assert::Require,
    |s, _| {
        let path = "/internal/admin/users/p1/deactivate?user=ada";
        // None.
        let (st, b) = s
            .send(Method::POST, path, Some(WEB_TOKEN), None, None)
            .await;
        assert_eq!((st, b["code"].as_str()), (401, Some("bad_assertion")));
        // For someone else than the `user=` named.
        let other = sign("web", "ben", &[], "POST", path, b"");
        let (st, _) = s
            .send(Method::POST, path, Some(WEB_TOKEN), Some(&other), None)
            .await;
        assert_eq!(st, 401);
        // Bound to another body.
        let body = json!({"expected_version": 1});
        let wrong = sign("web", "ada", &[], "POST", path, b"{}");
        let (st, _) = s
            .send(
                Method::POST,
                path,
                Some(WEB_TOKEN),
                Some(&wrong),
                Some(&body),
            )
            .await;
        assert_eq!(st, 401);
        // From the console's key, with the web token.
        let console = sign("console", "ada", &[], "POST", path, b"");
        let (st, _) = s
            .send(Method::POST, path, Some(WEB_TOKEN), Some(&console), None)
            .await;
        assert_eq!(st, 401);
        let dir = s.f.stores.users.directory(Utc::now()).await.unwrap();
        assert!(dir.get("p1").unwrap().active(), "nothing changed");
        let denied = s.events("users.deactivate").await;
        assert!(
            denied
                .iter()
                .filter(|e| e.1 == Outcome::Denied && e.3.as_deref() == Some("bad_assertion"))
                .count()
                >= 4,
            "{denied:?}"
        );
    }
);

both!(
    writes_refuse_the_development_override,
    Assert::InsecureOff,
    |s, _| {
        // Reads still answer (the override is for development stacks)...
        let (st, b) = s
            .send(
                Method::GET,
                "/internal/admin/users?user=ada",
                Some(WEB_TOKEN),
                None,
                None,
            )
            .await;
        assert_eq!(st, 200, "{b}");
        // ... but no user or role write runs, assertion or not.
        let path = "/internal/admin/users/p1/deactivate?user=ada";
        let a = sign("web", "ada", &[], "POST", path, b"");
        for assertion in [None, Some(a.as_str())] {
            let (st, b) = s
                .send(Method::POST, path, Some(WEB_TOKEN), assertion, None)
                .await;
            assert_eq!(
                (st, b["code"].as_str()),
                (503, Some("assertion_required")),
                "{b}"
            );
        }
        let (st, b) = s
            .send(
                Method::PUT,
                "/internal/admin/users/p1/roles?user=ada",
                Some(WEB_TOKEN),
                None,
                Some(&json!({"roles": ["reviewer"], "expected_version": 1})),
            )
            .await;
        assert_eq!((st, b["code"].as_str()), (503, Some("assertion_required")));
        assert!(s.has_role("p1", UserRole::Pm).await);
        let denied = s.events("users.deactivate").await;
        assert!(denied
            .iter()
            .any(|e| e.1 == Outcome::Denied && e.3.as_deref() == Some("assertion_required")));
    }
);

both!(
    the_actor_is_checked_in_the_database_not_the_assertion,
    Assert::Require,
    |s, _| {
        // `ana` is a PM; an assertion claiming `admin` changes nothing.
        let v = s.version().await;
        let path = "/internal/admin/users/p1/roles?user=ana";
        let body = json!({"roles": ["reviewer"], "expected_version": v});
        let bytes = serde_json::to_vec(&body).unwrap();
        let a = sign("web", "ana", &[Role::Admin], "PUT", path, &bytes);
        let (st, b) = s
            .send(Method::PUT, path, Some(WEB_TOKEN), Some(&a), Some(&body))
            .await;
        assert_eq!((st, b["code"].as_str()), (403, Some("forbidden")), "{b}");
        // An auditor may read the list but write nothing.
        let (st, b) = s.status("aud", "p1", "deactivate").await;
        assert_eq!((st, b["code"].as_str()), (403, Some("forbidden")));
        // A PM may not even read it.
        let (st, _) = s
            .web("ana", Method::GET, "/internal/admin/users", None)
            .await;
        assert_eq!(st, 403);
        assert!(s.has_role("p1", UserRole::Pm).await);
        let denied = s.events("users.roles").await;
        assert!(denied.iter().any(|e| e.0 == "ana"
            && e.1 == Outcome::Denied
            && e.3.as_deref() == Some("forbidden")));
    }
);

// --- The rules -------------------------------------------------------------------------------

both!(
    nobody_changes_their_own_roles_status_or_addresses,
    Assert::Require,
    |s, _| {
        let (st, b) = s.roles("ada", "ada", &["admin", "pm", "reviewer"]).await;
        assert_eq!((st, b["code"].as_str()), (403, Some("self_change")), "{b}");
        let (st, b) = s.status("ada", "ada", "deactivate").await;
        assert_eq!((st, b["code"].as_str()), (403, Some("self_change")));
        let (st, b) = s
            .fields(
                "ada",
                "ada",
                json!({"atlassian_email": "ada.new@example.test"}),
            )
            .await;
        assert_eq!((st, b["code"].as_str()), (403, Some("self_change")));
        let (st, b) = s
            .web(
                "ada",
                Method::DELETE,
                "/internal/admin/users/ada/identities/entra",
                None,
            )
            .await;
        assert_eq!((st, b["code"].as_str()), (403, Some("self_change")));
        // A display name is not an address: an admin may fix their own.
        let (st, b) = s
            .fields("ada", "ada", json!({"display_name": "Ada L."}))
            .await;
        assert_eq!(st, 200, "{b}");
        assert_eq!(b["user"]["display_name"], "Ada L.");
    }
);

both!(
    admin_is_never_granted_from_the_web,
    Assert::Require,
    |s, _| {
        let (st, b) = s.roles("ada", "p1", &["pm", "admin"]).await;
        assert_eq!((st, b["code"].as_str()), (403, Some("host_only")), "{b}");
        let (st, b) = s
        .web(
            "ada",
            Method::POST,
            "/internal/admin/users",
            Some(json!({"handle": "zed", "display_name": "Zed", "sign_in_name": "zed@example.test",
                        "atlassian_email": "zed@example.test", "roles": ["pm", "admin"]})),
        )
        .await;
        assert_eq!((st, b["code"].as_str()), (403, Some("host_only")));
        assert!(!s.has_role("p1", UserRole::Admin).await);
        // Revoking admin from someone else is the web's to do.
        let (st, b) = s.roles("ada", "ben", &["pm"]).await;
        assert_eq!(st, 200, "{b}");
        assert!(!s.has_role("ben", UserRole::Admin).await);
    }
);

both!(the_web_can_never_leave_no_admin, Assert::Require, |s, _| {
    // Two admins demote each other: the first wins; the second is no longer an admin.
    let (st, _) = s.roles("ada", "ben", &["pm"]).await;
    assert_eq!(st, 200);
    let (st, b) = s.roles("ben", "ada", &["pm"]).await;
    assert_eq!((st, b["code"].as_str()), (403, Some("forbidden")), "{b}");
    // The last admin can't act on themselves either.
    let (st, b) = s.status("ada", "ada", "deactivate").await;
    assert_eq!((st, b["code"].as_str()), (403, Some("self_change")));
    assert!(s.has_role("ada", UserRole::Admin).await);
    // The rules refuse it in the store too (the trigger in Postgres): the host break glass
    // can't leave none either.
    let err =
        s.f.stores
            .users
            .apply(
                &host(),
                Op::RevokeAdmin {
                    handle: "ada".into(),
                },
                Utc::now(),
            )
            .await
            .unwrap_err();
    assert_eq!(err.refusal().map(|r| r.code()), Some("last_admin"));
});

both!(
    an_admins_or_auditors_addresses_are_host_only,
    Assert::Require,
    |s, _| {
        for target in ["ben", "aud"] {
            for body in [
                json!({"sign_in_name": "x@example.test"}),
                json!({"atlassian_email": "x@example.test"}),
                json!({"github_login": "x-gh"}),
            ] {
                let (st, b) = s.fields("ada", target, body).await;
                assert_eq!(
                    (st, b["code"].as_str()),
                    (403, Some("host_only")),
                    "{target} {b}"
                );
            }
            let (st, b) = s
                .web(
                    "ada",
                    Method::POST,
                    &format!("/internal/admin/users/{target}/sign-in-name/confirm"),
                    Some(json!({"sign_in_name": "x@example.test"})),
                )
                .await;
            assert_eq!((st, b["code"].as_str()), (403, Some("host_only")));
            // Their display name is not an address.
            let (st, b) = s
                .fields("ada", target, json!({"display_name": "Someone"}))
                .await;
            assert_eq!(st, 200, "{b}");
        }
    }
);

both!(expected_version_must_be_current, Assert::Require, |s, _| {
    let old = s.version().await;
    let (st, _) = s
        .fields("ada", "p1", json!({"display_name": "P One"}))
        .await;
    assert_eq!(st, 200);
    let (st, b) = s
        .web(
            "ada",
            Method::PUT,
            "/internal/admin/users/p2/roles",
            Some(json!({"roles": ["pm", "reviewer"], "expected_version": old})),
        )
        .await;
    assert_eq!((st, b["code"].as_str()), (409, Some("stale")), "{b}");
    assert!(!s.has_role("p2", UserRole::Reviewer).await);
    // Without it: a bad request.
    let (st, b) = s
        .web(
            "ada",
            Method::PUT,
            "/internal/admin/users/p2/roles",
            Some(json!({"roles": ["pm", "reviewer"]})),
        )
        .await;
    assert_eq!((st, b["code"].as_str()), (400, Some("bad_request")));
    // With the current one it goes, and the answer carries the next version.
    let now = s.version().await;
    let (st, b) = s
        .web(
            "ada",
            Method::PUT,
            "/internal/admin/users/p2/roles",
            Some(json!({"roles": ["pm", "reviewer"], "expected_version": now})),
        )
        .await;
    assert_eq!(st, 200, "{b}");
    assert!(b["version"].as_i64().unwrap() > now);
    assert_eq!(b["user"]["roles"], json!(["pm", "reviewer"]));
    // A refusal that doesn't depend on what moved is itself, even with an old version.
    let (st, b) = s
        .web(
            "ada",
            Method::PUT,
            "/internal/admin/users/ada/roles",
            Some(json!({"roles": ["pm"], "expected_version": old})),
        )
        .await;
    assert_eq!((st, b["code"].as_str()), (403, Some("self_change")));
});

both!(
    addresses_change_before_a_link_and_the_account_id_decides_after,
    Assert::Require,
    |s, _| {
        // Before any link: set freely.
        let (st, b) = s
            .fields(
                "ada",
                "p1",
                json!({"atlassian_email": "P1@Example.test", "github_login": "p1-gh"}),
            )
            .await;
        assert_eq!(st, 200, "{b}");
        assert_eq!(b["user"]["atlassian_email"], "p1@example.test");
        assert_eq!(b["user"]["github_login"], "p1-gh");
        // p1 links Atlassian (the OAuth sign-in) and GitHub: now the account id decides.
        for (kind, id) in [
            (IdentityKind::Atlassian, "acc-1"),
            (IdentityKind::Github, "4242"),
        ] {
            s.f.stores
                .users
                .apply(
                    &Actor::link("p1"),
                    Op::LinkAccount {
                        handle: "p1".into(),
                        kind,
                        subject: id.into(),
                    },
                    Utc::now(),
                )
                .await
                .unwrap();
        }
        s.f.stores
            .credentials
            .put(&sealed("p1", Provider::Atlassian))
            .await
            .unwrap();
        let (st, b) = s
            .fields(
                "ada",
                "p1",
                json!({"atlassian_email": "p1.other@example.test"}),
            )
            .await;
        assert_eq!((st, b["code"].as_str()), (409, Some("linked")), "{b}");
        let (st, b) = s
            .fields("ada", "p1", json!({"github_login": "p1-other"}))
            .await;
        assert_eq!((st, b["code"].as_str()), (409, Some("linked")));
        // An admin unlinks first (the linked account goes with it), then changes the field.
        let (st, b) = s
            .web(
                "ada",
                Method::DELETE,
                "/internal/admin/users/p1/identities/atlassian",
                None,
            )
            .await;
        assert_eq!(st, 200, "{b}");
        assert_eq!(b["user"]["atlassian_linked"], false);
        assert!(s
            .f
            .stores
            .credentials
            .get("p1", Provider::Atlassian)
            .await
            .unwrap()
            .is_none());
        let (st, b) = s
            .fields(
                "ada",
                "p1",
                json!({"atlassian_email": "p1.other@example.test"}),
            )
            .await;
        assert_eq!(st, 200, "{b}");
        // Addresses are never reused, active or not.
        let (st, b) = s
            .fields(
                "ada",
                "p2",
                json!({"atlassian_email": "p1.other@example.test"}),
            )
            .await;
        assert_eq!((st, b["code"].as_str()), (409, Some("taken")));
        // Nothing to unlink twice; a Slack key isn't unlinked; junk isn't a kind.
        let (st, b) = s
            .web(
                "ada",
                Method::DELETE,
                "/internal/admin/users/p1/identities/atlassian",
                None,
            )
            .await;
        assert_eq!((st, b["code"].as_str()), (409, Some("unchanged")));
        for kind in ["slack", "okta"] {
            let (st, b) = s
                .web(
                    "ada",
                    Method::DELETE,
                    &format!("/internal/admin/users/p1/identities/{kind}"),
                    None,
                )
                .await;
            assert_eq!((st, b["code"].as_str()), (422, Some("invalid")), "{kind}");
        }
        let changes = s.f.stores.users.role_changes(Some("p1"), 50).await.unwrap();
        assert!(changes
            .iter()
            .any(|c| c.change == ChangeKind::Unlink && c.detail.as_deref() == Some("atlassian")));
    }
);

both!(
    a_confirmed_sign_in_name_lets_the_first_microsoft_link_through,
    Assert::Require,
    |s, _| {
        // A join approved through the Slack bot client: the sign-in name is unconfirmed.
        s.f.stores
            .users
            .apply(
                &Actor::bot("ada"),
                Op::CreateUser {
                    user: NewUser {
                        handle: "sam".into(),
                        display_name: "Sam".into(),
                        sign_in_name: Some("sam@example.test".into()),
                        atlassian_email: Some("sam@example.test".into()),
                        slack_id: Some("U0SAM0001".into()),
                        roles: [UserRole::Pm].into(),
                        ..Default::default()
                    },
                },
                Utc::now(),
            )
            .await
            .unwrap();
        let link = EntraLink {
            handle: "sam".into(),
            sign_in_name: "sam@example.test".into(),
            key: EntraKey {
                tid: TID.into(),
                oid: OID.into(),
            },
        };
        assert!(s
            .f
            .stores
            .users
            .link_entra(&link, Utc::now())
            .await
            .is_err());
        let list = s.list("ada").await;
        assert_eq!(Server::user(&list, "sam")["sign_in_unconfirmed"], true);
        // The admin confirms it as it is.
        let (st, b) = s
            .web(
                "ada",
                Method::POST,
                "/internal/admin/users/sam/sign-in-name/confirm",
                Some(json!({"sign_in_name": "sam@example.test"})),
            )
            .await;
        assert_eq!(st, 200, "{b}");
        assert_eq!(b["user"]["sign_in_name_confirmed"], true);
        assert_eq!(b["user"]["sign_in_unconfirmed"], false);
        let changes =
            s.f.stores
                .users
                .role_changes(Some("sam"), 10)
                .await
                .unwrap();
        assert_eq!(changes[0].change, ChangeKind::ConfirmSignInName);
        let ev = s.events("users.fields").await;
        assert_eq!(ev[0].3.as_deref(), Some("sign_in_name confirmed"));
        // Confirming again is no change.
        let (st, b) = s
            .web(
                "ada",
                Method::POST,
                "/internal/admin/users/sam/sign-in-name/confirm",
                Some(json!({"sign_in_name": "SAM@example.test"})),
            )
            .await;
        assert_eq!((st, b["code"].as_str()), (409, Some("unchanged")));
        // Now the first link works.
        let linked =
            s.f.stores
                .users
                .link_entra(&link, Utc::now())
                .await
                .unwrap();
        assert!(linked.first_link);
        // After the link the name is informational: changing it is allowed and recorded.
        let (st, b) = s
            .fields("ada", "sam", json!({"sign_in_name": "sam2@example.test"}))
            .await;
        assert_eq!(st, 200, "{b}");
        assert_eq!(b["user"]["entra_linked"], true);
        // An admin may unlink a PM's Microsoft account (a reassigned mailbox).
        let (st, b) = s
            .web(
                "ada",
                Method::DELETE,
                "/internal/admin/users/sam/identities/entra",
                None,
            )
            .await;
        assert_eq!(st, 200, "{b}");
        assert_eq!(b["user"]["entra_linked"], false);
    }
);

both!(
    an_admins_microsoft_link_is_unlinked_on_the_host_only,
    Assert::Require,
    |s, _| {
        s.f.stores
            .users
            .apply(
                &host(),
                Op::SetEntra {
                    handle: "ben".into(),
                    key: EntraKey {
                        tid: TID.into(),
                        oid: OID.into(),
                    },
                },
                Utc::now(),
            )
            .await
            .unwrap();
        let (st, b) = s
            .web(
                "ada",
                Method::DELETE,
                "/internal/admin/users/ben/identities/entra",
                None,
            )
            .await;
        assert_eq!((st, b["code"].as_str()), (403, Some("host_only")), "{b}");
        let list = s.list("ada").await;
        assert_eq!(Server::user(&list, "ben")["entra_linked"], true);
    }
);

both!(
    invite_creates_an_active_confirmed_user,
    Assert::Require,
    |s, _| {
        let (st, b) = s
        .web(
            "ada",
            Method::POST,
            "/internal/admin/users",
            Some(json!({"handle": "zed", "display_name": "Zed <b>Z</b>", "sign_in_name": "Zed@Example.test",
                        "atlassian_email": "zed@example.test", "github_login": "zed-gh",
                        "slack_id": "U0ZED0001", "roles": ["pm", "reviewer"]})),
        )
        .await;
        assert_eq!(st, 201, "{b}");
        let u = &b["user"];
        assert_eq!(u["status"], "active");
        assert_eq!(u["roles"], json!(["pm", "reviewer"]));
        assert_eq!(u["display_name"], "Zed bZ/b");
        assert_eq!(u["sign_in_name"], "zed@example.test");
        assert_eq!(u["sign_in_name_confirmed"], true);
        assert_eq!(u["slack_linked"], true);
        assert!(u.get("slack_id").is_none());
        // A handle is never reused; an unknown role is invalid; no role at all is invalid.
        let (st, b) = s
            .web(
                "ada",
                Method::POST,
                "/internal/admin/users",
                Some(json!({"handle": "zed", "sign_in_name": "z2@example.test",
                        "atlassian_email": "z2@example.test", "roles": ["pm"]})),
            )
            .await;
        assert_eq!((st, b["code"].as_str()), (409, Some("exists")));
        for roles in [json!(["owner"]), json!([])] {
            let (st, b) = s
                .web(
                    "ada",
                    Method::POST,
                    "/internal/admin/users",
                    Some(json!({"handle": "yan", "sign_in_name": "yan@example.test",
                            "atlassian_email": "yan@example.test", "roles": roles})),
                )
                .await;
            assert_eq!((st, b["code"].as_str()), (422, Some("invalid")));
        }
        // A field this route doesn't take is a bad request.
        let (st, b) = s
            .web(
                "ada",
                Method::POST,
                "/internal/admin/users",
                Some(json!({"handle": "yan", "sign_in_name": "yan@example.test",
                        "atlassian_email": "yan@example.test", "roles": ["pm"], "oid": OID})),
            )
            .await;
        assert_eq!((st, b["code"].as_str()), (400, Some("bad_request")));
        let ev = s.events("users.create").await;
        assert!(ev
            .iter()
            .any(|e| e.0 == "ada" && e.1 == Outcome::Ok && e.2.as_deref() == Some("zed")));
    }
);

// --- Deactivation ------------------------------------------------------------------------------

both!(
    the_deactivation_cap_counts_role_changes_only,
    Assert::Require,
    |s, _| {
        let (st, b) = s.status("ada", "p1", "deactivate").await;
        assert_eq!(st, 200, "{b}");
        assert_eq!(b["user"]["status"], "deactivated");
        assert!(b["user"]["grace_ends_at"].is_string());
        assert!(b["note"].as_str().unwrap().contains("24h"));
        // A refused attempt doesn't count.
        let (st, _) = s.status("ada", "ada", "deactivate").await;
        assert_eq!(st, 403);
        let (st, _) = s.status("ada", "nobody", "deactivate").await;
        assert_eq!(st, 404);
        // A reactivation doesn't reset the window.
        let (st, _) = s.status("ada", "p1", "reactivate").await;
        assert_eq!(st, 200);
        let (st, _) = s.status("ada", "p1", "deactivate").await;
        assert_eq!(st, 200);
        let list = s.list("ada").await;
        assert_eq!(list["deactivations"]["last_hour"], 2);
        assert_eq!(list["deactivations"]["remaining"], 1);
        assert_eq!(list["deactivations"]["limit"], 3);
        assert_eq!(list["deactivations"]["grace_hours"], 24);
        let (st, _) = s.status("ben", "p2", "deactivate").await;
        assert_eq!(st, 200);
        // The fourth in the hour, from any admin.
        let (st, b) = s.status("ada", "p3", "deactivate").await;
        assert_eq!(
            (st, b["code"].as_str()),
            (429, Some("deactivation_limit")),
            "{b}"
        );
        let ev = s.events("users.deactivate").await;
        assert!(ev
            .iter()
            .any(|e| e.1 == Outcome::Denied && e.3.as_deref() == Some("deactivation_limit")));
        // The host stays uncapped.
        s.f.stores
            .users
            .apply(
                &host(),
                Op::Deactivate {
                    handle: "p3".into(),
                },
                Utc::now(),
            )
            .await
            .unwrap();
        let list = s.list("ada").await;
        assert_eq!(list["deactivations"]["last_hour"], 3);
        assert_eq!(Server::user(&list, "p3")["status"], "deactivated");
    }
);

both!(
    reactivation_never_restores_admin,
    Assert::Require,
    |s, _| {
        let (st, b) = s.status("ada", "ben", "deactivate").await;
        assert_eq!(st, 200, "{b}");
        assert!(!s.has_role("ben", UserRole::Admin).await);
        let (st, b) = s.status("ada", "ben", "reactivate").await;
        assert_eq!(st, 200, "{b}");
        assert_eq!(b["user"]["status"], "active");
        assert!(!b["user"]["roles"]
            .as_array()
            .unwrap()
            .contains(&json!("admin")));
        assert!(!s.has_role("ben", UserRole::Admin).await);
        let (st, b) = s.status("ada", "ben", "reactivate").await;
        assert_eq!((st, b["code"].as_str()), (409, Some("unchanged")));
    }
);

both!(
    a_team_bots_owner_is_not_deactivated,
    Assert::Require,
    |s, _| {
        let (st, b) = s.status("ada", "ana", "deactivate").await;
        assert_eq!((st, b["code"].as_str()), (409, Some("owns_bots")), "{b}");
        assert!(b["error"].as_str().unwrap().contains("scout"));
        let list = s.list("ada").await;
        assert_eq!(
            Server::user(&list, "ana")["team_bots"],
            json!(["blind", "scout"])
        );
        assert_eq!(list["deactivations"]["last_hour"], 0);
        // A PM asking gets the rules' answer, not the bots'.
        let (st, b) = s.status("p1", "ana", "deactivate").await;
        assert_eq!((st, b["code"].as_str()), (403, Some("forbidden")));
    }
);

// --- The pilot exception ---------------------------------------------------------------------

/// `ben` holds Admin and Auditor under a pilot exception that has since lapsed.
async fn lapse_with_ben_dual(s: &Server, postgres: bool) {
    let users = &s.f.stores.users;
    let now = Utc::now();
    if postgres {
        users
            .apply(
                &host(),
                Op::ExceptionOn {
                    until: now + Duration::days(1),
                },
                now,
            )
            .await
            .unwrap();
        users
            .apply(
                &host(),
                Op::SetRoles {
                    handle: "ben".into(),
                    roles: [UserRole::Admin, UserRole::Pm, UserRole::Auditor].into(),
                },
                now,
            )
            .await
            .unwrap();
        // Move the exception into the past, as time would: the trigger is set aside for
        // this one fixture statement.
        let pool = s.pg.as_ref().unwrap().store.pool();
        for sql in [
            "ALTER TABLE pilot_exceptions DISABLE TRIGGER users_invariants",
            "UPDATE pilot_exceptions SET enabled_at = now() - interval '10 days', \
             expires_at = now() - interval '1 day' WHERE ended_at IS NULL",
            "ALTER TABLE pilot_exceptions ENABLE TRIGGER users_invariants",
        ] {
            sqlx::query(sqlx::AssertSqlSafe(sql))
                .execute(pool)
                .await
                .unwrap();
        }
    } else {
        let then = now - Duration::days(10);
        users
            .apply(
                &host(),
                Op::ExceptionOn {
                    until: now - Duration::days(1),
                },
                then,
            )
            .await
            .unwrap();
        users
            .apply(
                &host(),
                Op::SetRoles {
                    handle: "ben".into(),
                    roles: [UserRole::Admin, UserRole::Pm, UserRole::Auditor].into(),
                },
                then,
            )
            .await
            .unwrap();
    }
}

both!(
    a_lapsed_exception_still_allows_what_resolves_it,
    Assert::Require,
    |s, pg| {
        lapse_with_ben_dual(s, pg).await;
        let list = s.list("ada").await;
        assert_eq!(list["pilot_exception"]["lapsed"], true);
        let ben = Server::user(&list, "ben");
        assert_eq!(ben["dual_role"], true);
        assert_eq!(ben["role_conflict"], true);
        let (st, overview) = s
            .web("ada", Method::GET, "/internal/admin/overview", None)
            .await;
        assert_eq!(st, 200, "{overview}");
        assert_eq!(overview["users"]["role_conflicts"], json!(["ben"]));
        // A change that keeps both is refused.
        let (st, b) = s
            .roles("ada", "ben", &["admin", "auditor", "pm", "reviewer"])
            .await;
        assert_eq!((st, b["code"].as_str()), (409, Some("dual_role")), "{b}");
        // Ben's admin powers stay: the stack keeps a working admin while it lapses.
        let (st, b) = s
            .fields("ben", "p1", json!({"display_name": "Set by Ben"}))
            .await;
        assert_eq!(st, 200, "{b}");
        // Revoking either role resolves it.
        let (st, b) = s.roles("ada", "ben", &["admin", "pm"]).await;
        assert_eq!(st, 200, "{b}");
        assert!(s.has_role("ben", UserRole::Admin).await);
        let (st, overview) = s
            .web("ada", Method::GET, "/internal/admin/overview", None)
            .await;
        assert_eq!(st, 200);
        assert_eq!(overview["users"]["role_conflicts"], json!([]));
    }
);

both!(
    a_lapsed_dual_role_admin_can_be_demoted_or_deactivated,
    Assert::Require,
    |s, pg| {
        lapse_with_ben_dual(s, pg).await;
        let (st, b) = s.roles("ada", "ben", &["auditor", "pm"]).await;
        assert_eq!(st, 200, "{b}");
        assert!(!s.has_role("ben", UserRole::Admin).await);
        assert!(
            s.has_role("ada", UserRole::Admin).await,
            "ada is still a working admin"
        );
    }
);

both!(
    a_lapsed_dual_role_admin_can_be_deactivated,
    Assert::Require,
    |s, pg| {
        lapse_with_ben_dual(s, pg).await;
        let (st, b) = s.status("ada", "ben", "deactivate").await;
        assert_eq!(st, 200, "{b}");
        assert!(!s.has_role("ben", UserRole::Admin).await);
        // A deactivated person holds both no more in any way that counts.
        let (st, overview) = s
            .web("ada", Method::GET, "/internal/admin/overview", None)
            .await;
        assert_eq!(st, 200);
        assert_eq!(overview["users"]["role_conflicts"], json!([]));
    }
);

// --- What each viewer sees -------------------------------------------------------------------

both!(
    an_auditor_never_sees_an_address_oid_or_slack_id,
    Assert::Require,
    |s, _| {
        s.f.stores
            .users
            .apply(
                &host(),
                Op::SetField {
                    handle: "p1".into(),
                    field: pmos_toolserver::users::Field::SignInName,
                    value: "p1@example.test".into(),
                },
                Utc::now(),
            )
            .await
            .unwrap();
        s.f.stores
            .users
            .apply(
                &host(),
                Op::SetEntra {
                    handle: "p1".into(),
                    key: EntraKey {
                        tid: TID.into(),
                        oid: OID.into(),
                    },
                },
                Utc::now(),
            )
            .await
            .unwrap();
        let auditor = s.list("aud").await;
        assert_eq!(auditor["view"], "auditor");
        let text = auditor.to_string();
        for secret in ["p1@example.test", OID, TID, "U0TEST"] {
            assert!(!text.contains(secret), "{secret} in the auditor's answer");
        }
        let p1 = Server::user(&auditor, "p1");
        for key in [
            "sign_in_name",
            "atlassian_email",
            "github_login",
            "slack_id",
            "oid",
        ] {
            assert!(p1.get(key).is_none(), "{key}");
        }
        assert_eq!(p1["has_sign_in_name"], true);
        assert_eq!(p1["entra_linked"], true);
        assert_eq!(p1["slack_linked"], true);
        // The admin sees the addresses, never an oid or a Slack id.
        let admin = s.list("ada").await;
        assert_eq!(admin["view"], "admin");
        assert_eq!(
            Server::user(&admin, "p1")["sign_in_name"],
            "p1@example.test"
        );
        let text = admin.to_string();
        for secret in [OID, TID, "U0TEST"] {
            assert!(!text.contains(secret), "{secret} in the admin's answer");
        }
        // Both reads are audited.
        let reads = s.events("admin.users").await;
        assert!(reads.iter().any(|e| e.0 == "aud" && e.1 == Outcome::Ok));
        assert!(reads.iter().any(|e| e.0 == "ada" && e.1 == Outcome::Ok));
    }
);

both!(
    every_write_allowed_or_refused_is_audited,
    Assert::Require,
    |s, _| {
        let (st, _) = s.roles("ada", "p1", &["pm", "reviewer"]).await;
        assert_eq!(st, 200);
        let (st, _) = s.roles("ada", "ada", &["pm"]).await;
        assert_eq!(st, 403);
        let ev = s.events("users.roles").await;
        assert!(ev.iter().any(|e| e.0 == "ada"
            && e.1 == Outcome::Ok
            && e.2.as_deref() == Some("p1")
            && e.3.as_deref() == Some("+reviewer")));
        assert!(ev.iter().any(|e| e.0 == "ada"
            && e.1 == Outcome::Denied
            && e.2.as_deref() == Some("ada")
            && e.3.as_deref() == Some("self_change")));
        let changes = s.f.stores.users.role_changes(Some("p1"), 10).await.unwrap();
        assert_eq!(changes[0].change, ChangeKind::Grant);
        assert_eq!(changes[0].role, Some(UserRole::Reviewer));
        assert_eq!(changes[0].actor, "ada");
        // A refusal writes no role change.
        let ada =
            s.f.stores
                .users
                .role_changes(Some("ada"), 50)
                .await
                .unwrap();
        assert!(ada.iter().all(|c| c.change != ChangeKind::Revoke));
        // The audit list serves the details (fixed formats).
        let (st, page) = s
            .web(
                "ada",
                Method::GET,
                "/internal/admin/audit?action=users.roles",
                None,
            )
            .await;
        assert_eq!(st, 200);
        assert!(page["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["detail"] == "self_change"));
    }
);

// --- Reviewer assignments --------------------------------------------------------------------

both!(reviewers_are_assigned_pms, Assert::Require, |s, _| {
    let before = s.version().await;
    let (st, b) = s
        .web(
            "ada",
            Method::PUT,
            "/internal/admin/reviewers/rev",
            Some(json!({"pms": ["p1", "p2"]})),
        )
        .await;
    assert_eq!(st, 200, "{b}");
    assert_eq!(b["user"]["reviews"], json!(["p1", "p2"]));
    assert!(
        b["version"].as_i64().unwrap() > before,
        "the directory version moved"
    );
    let list = s.list("aud").await;
    assert_eq!(Server::user(&list, "p1")["reviewed_by"], json!(["rev"]));
    // The whole set: p2 goes, p3 comes.
    let v = s.version().await;
    let (st, b) = s
        .web(
            "ada",
            Method::PUT,
            "/internal/admin/reviewers/rev",
            Some(json!({"pms": ["p1", "p3"], "expected_version": v})),
        )
        .await;
    assert_eq!(st, 200, "{b}");
    assert_eq!(b["user"]["reviews"], json!(["p1", "p3"]));
    let (st, b) = s
        .web(
            "ada",
            Method::PUT,
            "/internal/admin/reviewers/rev",
            Some(json!({"pms": ["p1"], "expected_version": v})),
        )
        .await;
    assert_eq!((st, b["code"].as_str()), (409, Some("stale")));
    // Refusals.
    for (reviewer, pms, code) in [
        ("p4", json!(["p1"]), "not_reviewer"),
        ("rev", json!(["aud"]), "not_pm"),
        ("rev", json!(["rev"]), "invalid"),
        ("rev", json!(["nobody"]), "not_found"),
        ("rev", json!(["p1", "p1"]), "invalid"),
    ] {
        let (st, b) = s
            .web(
                "ada",
                Method::PUT,
                &format!("/internal/admin/reviewers/{reviewer}"),
                Some(json!({"pms": pms})),
            )
            .await;
        assert_eq!(b["code"].as_str(), Some(code), "{reviewer} {b}");
        assert!(st >= 400);
    }
    let (st, _) = s.status("ada", "p4", "deactivate").await;
    assert_eq!(st, 200);
    let (st, b) = s
        .web(
            "ada",
            Method::PUT,
            "/internal/admin/reviewers/rev",
            Some(json!({"pms": ["p1", "p3", "p4"]})),
        )
        .await;
    assert_eq!((st, b["code"].as_str()), (409, Some("not_active")));
    // Clearing is always allowed; the same set again is no change.
    let (st, b) = s
        .web(
            "ada",
            Method::PUT,
            "/internal/admin/reviewers/rev",
            Some(json!({"pms": []})),
        )
        .await;
    assert_eq!(st, 200, "{b}");
    assert_eq!(b["user"]["reviews"], json!([]));
    let (st, b) = s
        .web(
            "ada",
            Method::PUT,
            "/internal/admin/reviewers/rev",
            Some(json!({"pms": []})),
        )
        .await;
    assert_eq!((st, b["code"].as_str()), (409, Some("unchanged")));
    let ev = s.events("reviewers.assign").await;
    assert!(ev
        .iter()
        .any(|e| e.1 == Outcome::Ok && e.3.as_deref() == Some("2 pms")));
    let changes =
        s.f.stores
            .users
            .role_changes(Some("rev"), 20)
            .await
            .unwrap();
    assert!(changes
        .iter()
        .any(|c| c.change == ChangeKind::Unassign && c.detail.as_deref() == Some("pm p2")));
});

both!(
    an_admin_never_assigns_themselves_pms_to_review,
    Assert::Require,
    |s, _| {
        s.f.stores
            .users
            .apply(
                &host(),
                Op::SetRoles {
                    handle: "ben".into(),
                    roles: [UserRole::Admin, UserRole::Pm, UserRole::Reviewer].into(),
                },
                Utc::now(),
            )
            .await
            .unwrap();
        let (st, b) = s
            .web(
                "ben",
                Method::PUT,
                "/internal/admin/reviewers/ben",
                Some(json!({"pms": ["p1"]})),
            )
            .await;
        assert_eq!((st, b["code"].as_str()), (403, Some("self_change")), "{b}");
        let (st, b) = s
            .web(
                "ada",
                Method::PUT,
                "/internal/admin/reviewers/ben",
                Some(json!({"pms": ["p1"]})),
            )
            .await;
        assert_eq!(st, 200, "{b}");
    }
);

// --- The overview ----------------------------------------------------------------------------

both!(
    the_overview_flags_what_needs_an_admin,
    Assert::Require,
    |s, _| {
        s.f.stores
            .users
            .apply(
                &Actor::bot("ada"),
                Op::CreateUser {
                    user: NewUser {
                        handle: "sam".into(),
                        sign_in_name: Some("sam@example.test".into()),
                        slack_id: Some("U0SAM0001".into()),
                        roles: [UserRole::Pm].into(),
                        ..Default::default()
                    },
                },
                Utc::now(),
            )
            .await
            .unwrap();
        // A conflicting first sign-in on p1's name (p1 already linked to another account).
        s.f.stores
            .users
            .apply(
                &host(),
                Op::SetField {
                    handle: "p1".into(),
                    field: pmos_toolserver::users::Field::SignInName,
                    value: "p1@example.test".into(),
                },
                Utc::now(),
            )
            .await
            .unwrap();
        s.f.stores
            .users
            .apply(
                &host(),
                Op::SetEntra {
                    handle: "p1".into(),
                    key: EntraKey {
                        tid: TID.into(),
                        oid: OID.into(),
                    },
                },
                Utc::now(),
            )
            .await
            .unwrap();
        let other = EntraLink {
            handle: "p1".into(),
            sign_in_name: "p1@example.test".into(),
            key: EntraKey {
                tid: TID.into(),
                oid: "bbbbbbbb-bbbb-cccc-dddd-eeeeeeeeeeee".into(),
            },
        };
        assert!(s
            .f
            .stores
            .users
            .link_entra(&other, Utc::now())
            .await
            .is_err());
        // An exception ending within 14 days, with one person holding both.
        s.f.stores
            .users
            .apply(
                &host(),
                Op::ExceptionOn {
                    until: Utc::now() + Duration::days(10),
                },
                Utc::now(),
            )
            .await
            .unwrap();
        let (st, b) = s.roles("ada", "ben", &["admin", "auditor", "pm"]).await;
        assert_eq!(st, 200, "{b}");
        let (st, o) = s
            .web("aud", Method::GET, "/internal/admin/overview", None)
            .await;
        assert_eq!(st, 200, "{o}");
        let u = &o["users"];
        assert_eq!(u["sign_in_unconfirmed"], json!(["sam"]));
        assert_eq!(u["dual_role"], json!(["ben"]));
        assert_eq!(u["pilot_exception"]["ending_soon"], true);
        assert_eq!(u["pilot_exception"]["lapsed"], false);
        assert_eq!(u["role_conflicts"], json!([]));
        assert_eq!(u["identity_conflicts"], 1);
        assert_eq!(u["identity_conflict_users"], json!(["p1"]));
        assert_eq!(u["resolve_miss_warning"], json!([]));
        assert_eq!(u["resolve_miss_warning_threshold"], 100);
        assert_eq!(u["deactivation_grace_hours"], 24);
        assert!(!o.to_string().contains("p1@example.test"));
        // The resolve-miss warning: more than 100 misses in an hour from one client.
        for _ in 0..=100 {
            s.f.svc.people().resolve_misses().record("console");
        }
        let (_, o) = s
            .web("ada", Method::GET, "/internal/admin/overview", None)
            .await;
        assert_eq!(o["users"]["resolve_miss_warning"], json!(["console"]));
    }
);

// --- The console's listing -------------------------------------------------------------------

impl Server {
    async fn console(&self, viewer: &str, signed: bool) -> (u16, Value) {
        let target = format!("/internal/console/members?user={viewer}");
        let a = sign("console", viewer, &[], "GET", &target, b"");
        self.send(
            Method::GET,
            &target,
            Some(CONSOLE_TOKEN),
            signed.then_some(a.as_str()),
            None,
        )
        .await
    }
}

both!(
    the_console_lists_handles_for_an_admin_only,
    Assert::Require,
    |s, _| {
        let (st, _) = s.status("ada", "p4", "deactivate").await;
        assert_eq!(st, 200);
        let (st, b) = s.console("ada", true).await;
        assert_eq!(st, 200, "{b}");
        // Active users who may hold drafts: no auditor-only person, nobody deactivated.
        assert_eq!(
            b,
            json!({"handles": ["ada", "ana", "ben", "p1", "p2", "p3", "rev"]})
        );
        // Handles only.
        assert_eq!(b.as_object().unwrap().len(), 1);
        // A PM, or an auditor, may not list.
        for viewer in ["ana", "aud"] {
            let (st, b) = s.console(viewer, true).await;
            assert_eq!(
                (st, b["code"].as_str()),
                (403, Some("forbidden")),
                "{viewer}"
            );
        }
        // In require, the console's assertion for the viewer is needed.
        let (st, _) = s.console("ada", false).await;
        assert_eq!(st, 401);
        // No other token.
        for token in [WEB_TOKEN, SERVICE_TOKEN, BOT_TOKEN] {
            let (st, _) = s
                .send(
                    Method::GET,
                    "/internal/console/members?user=ada",
                    Some(token),
                    None,
                    None,
                )
                .await;
            assert_eq!(st, 403);
        }
        // And the console still reaches no write.
        let (st, _) = s
            .send(
                Method::POST,
                "/internal/admin/users/p1/deactivate?user=ada",
                Some(CONSOLE_TOKEN),
                None,
                None,
            )
            .await;
        assert_eq!(st, 403);
        let ev = s.events("admin.console_members").await;
        assert!(ev.iter().any(|e| e.0 == "ada" && e.1 == Outcome::Ok));
        assert!(ev.iter().any(|e| e.0 == "ana" && e.1 == Outcome::Denied));
    }
);

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
