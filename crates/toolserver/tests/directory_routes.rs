//! The user database's read API (`docs/user-database-spec.md` §2, §7; slice U2):
//! `GET /internal/directory`, `GET /internal/users/resolve` and
//! `POST /internal/users/{handle}/identities/entra`.
//!
//! Who may call each route (every route × method × token), what each token sees (exactly its
//! fields), the ETag and `304`, the miss-only rate limit, every first-link condition, and two
//! links racing for one identity: through the API on both stores, and in Postgres with raw SQL
//! and no application lock, where the unique key alone decides.
//!
//! The first link takes the raw ID token (review of `b469d5b`, U1): the server here checks
//! tokens against a fake tenant (`pmos_entra::testing::FakeIdp`) with locally generated keys.
//! Forged, unsigned, wrong audience, issuer or tenant, expired, guest and replayed tokens are
//! refused; the takeover from the review (the web token alone posting someone else's sign-in
//! name) fails end to end; key rotation refetches; the tenant's keys down fail closed; no
//! settings is `501`.

mod common;

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use chrono::Utc;
use common::{Fixture, TestPg};
use pmos_assertion::{unix_now, Claims, Key, KeyRing, RequestBinding};
use pmos_entra::testing::{jwk, keys, member_claims, sign, unsigned, FakeIdp};
use pmos_toolserver::app::directory::MISSES_PER_MINUTE;
use pmos_toolserver::app::{BotAccess, EntraLinkCheck};
use pmos_toolserver::assertion::{parse_keys, Assertions, Mode, AUDIENCE, ISSUERS};
use pmos_toolserver::audit::{AuditEvent, AuditQuery, Client, Outcome};
use pmos_toolserver::users::{
    Actor, ChangeKind, EntraKey, HostUser, IdentityKind, NewUser, Op, UserRole, UserStore,
};
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

const TID: &str = "11111111-2222-3333-4444-555555555555";
const OID_ADA: &str = "aaaaaaaa-0000-0000-0000-00000000000a";
const OID_CY: &str = "aaaaaaaa-0000-0000-0000-00000000000c";
const OID_NEW: &str = "bbbbbbbb-0000-0000-0000-000000000001";
const OID_NEW2: &str = "bbbbbbbb-0000-0000-0000-000000000002";
/// The PM OS application (client) id the tool server checks `aud` against.
const CLIENT: &str = "0b7f0c6e-1111-4222-8333-444455556666";
const NONCE: &str = "the-sign-in-nonce-0123456789";

struct Server {
    base: String,
    cancel: CancellationToken,
    f: Fixture,
    pg: Option<TestPg>,
    /// The fake tenant the server validates first-link tokens against (`None`: not
    /// configured).
    idp: Option<FakeIdp>,
    /// Each token gets its own `iat`, so two tokens are never one (a replay).
    issued: AtomicI64,
}

impl Server {
    async fn stop(mut self) {
        self.cancel.cancel();
        if let Some(pg) = self.pg.take() {
            pg.drop_schema().await;
        }
    }

    fn users(&self) -> &Arc<dyn UserStore> {
        &self.f.stores.users
    }

    async fn send(
        &self,
        method: Method,
        target: &str,
        token: Option<&str>,
        headers: &[(&str, &str)],
        body: Option<Value>,
    ) -> (u16, reqwest::header::HeaderMap, String) {
        let mut r = reqwest::Client::new().request(method, format!("{}{target}", self.base));
        if let Some(t) = token {
            r = r.bearer_auth(t);
        }
        for (k, v) in headers {
            r = r.header(*k, *v);
        }
        if let Some(b) = body {
            r = r.json(&b);
        }
        let res = r.send().await.unwrap();
        let status = res.status().as_u16();
        let headers = res.headers().clone();
        (status, headers, res.text().await.unwrap())
    }

    async fn get(&self, target: &str, token: &str) -> (u16, Value) {
        let (s, _, text) = self.send(Method::GET, target, Some(token), &[], None).await;
        (s, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    /// A member's claims from the fake tenant for `oid` and `name`, valid now (each with its
    /// own `iat`, a second apart).
    fn claims(&self, oid: &str, name: &str) -> Value {
        let idp = self.idp.as_ref().expect("a server with Entra settings");
        let now = Utc::now().timestamp();
        let mut c = member_claims(&idp.base, TID, CLIENT, oid, name, NONCE, now);
        c["iat"] = json!(self.issued.fetch_add(1, Ordering::SeqCst));
        c
    }

    /// Microsoft's ID token for `oid` with sign-in name `name`, signed with the tenant's key.
    fn token(&self, oid: &str, name: &str) -> String {
        sign(&self.claims(oid, name), keys::KEY_A_DER, "key-a")
    }

    /// The first-link route with `body`, as pmos-web.
    async fn link_body(&self, handle: &str, body: Value) -> (u16, Value) {
        let (s, _, text) = self
            .send(
                Method::POST,
                &format!("/internal/users/{handle}/identities/entra"),
                Some(WEB_TOKEN),
                &[],
                Some(body),
            )
            .await;
        (s, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    /// The first-link route with `token` and the sign-in's nonce.
    async fn link_token(&self, handle: &str, token: &str) -> (u16, Value) {
        self.link_body(handle, json!({ "id_token": token, "nonce": NONCE }))
            .await
    }

    /// A first link for `handle` with a fresh, valid token naming `oid` and `name`.
    async fn link(&self, handle: &str, name: &str, oid: &str) -> (u16, Value) {
        let token = self.token(oid, name);
        self.link_token(handle, &token).await
    }

    async fn events(&self) -> Vec<AuditEvent> {
        let mut found = self
            .f
            .stores
            .audit
            .search(&AuditQuery {
                limit: 1000,
                ..Default::default()
            })
            .await
            .unwrap()
            .unwrap();
        found.sort_by_key(|s| s.id);
        found.into_iter().map(|s| s.event).collect()
    }
}

fn host() -> Actor {
    Actor::host(HostUser::parse("opsuser").unwrap())
}

fn person(handle: &str, roles: &[UserRole]) -> NewUser {
    NewUser {
        handle: handle.into(),
        display_name: format!("{handle} Name"),
        sign_in_name: Some(format!("{handle}@contoso.com")),
        atlassian_email: Some(format!("{handle}.atl@contoso.com")),
        github_login: Some(format!("{handle}-gh")),
        slack_id: Some(format!("U0{}", handle.to_uppercase())),
        roles: roles.iter().copied().collect(),
    }
}

/// The directory every case starts from:
/// - `raj`: the bootstrap admin (`pm`, `admin`), no Entra link;
/// - `ada`: an admin with a host-confirmed link (`OID_ADA`);
/// - `ana`, `ben`: PMs, sign-in names confirmed (created on the host), not linked;
/// - `sam`: a PM who joined through the Slack bot client: sign-in name unconfirmed;
/// - `aud`: an auditor, not linked;
/// - `cy`: a PM already linked to `OID_CY`;
/// - `dee`: a PM, deactivated.
async fn seed(users: &dyn UserStore) {
    let now = Utc::now();
    users
        .apply(
            &host(),
            Op::BootstrapAdmin {
                user: person("raj", &[UserRole::Pm, UserRole::Admin]),
                entra: None,
            },
            now,
        )
        .await
        .unwrap();
    for (h, r) in [
        ("ada", UserRole::Pm),
        ("ana", UserRole::Pm),
        ("ben", UserRole::Pm),
        ("aud", UserRole::Auditor),
        ("cy", UserRole::Pm),
        ("dee", UserRole::Pm),
    ] {
        users
            .apply(
                &host(),
                Op::CreateUser {
                    user: person(h, &[r]),
                },
                now,
            )
            .await
            .unwrap();
    }
    users
        .apply(
            &Actor::bot("raj"),
            Op::CreateUser {
                user: person("sam", &[UserRole::Pm]),
            },
            now,
        )
        .await
        .unwrap();
    users
        .apply(
            &host(),
            Op::GrantAdmin {
                handle: "ada".into(),
                entra: pmos_toolserver::users::AdminEntra::Oid(EntraKey {
                    tid: TID.into(),
                    oid: OID_ADA.into(),
                }),
            },
            now,
        )
        .await
        .unwrap();
    users
        .apply(
            &host(),
            Op::SetEntra {
                handle: "cy".into(),
                key: EntraKey {
                    tid: TID.into(),
                    oid: OID_CY.into(),
                },
            },
            now,
        )
        .await
        .unwrap();
    users
        .apply(
            &host(),
            Op::Deactivate {
                handle: "dee".into(),
            },
            now,
        )
        .await
        .unwrap();
}

/// A server with every client's token, in assertion `require` (as a deployment with a web
/// token runs), and the seeded directory. In memory, or in Postgres (`None` without a test
/// database).
async fn start(postgres: bool) -> Option<Server> {
    start_with(postgres, true).await
}

/// As [`start`], with or without Entra settings (a fake tenant).
async fn start_with(postgres: bool, entra: bool) -> Option<Server> {
    let (f, pg) = if postgres {
        let (f, pg) = common::pg_fixture().await?;
        (f, Some(pg))
    } else {
        (common::fixture(), None)
    };
    seed(f.stores.users.as_ref()).await;
    let keys = parse_keys(
        &format!("web:toolserver:w1:{WEB_KEY}"),
        "the tool server",
        |iss, aud| aud == AUDIENCE && ISSUERS.contains(&iss),
    )
    .unwrap();
    let idp = if entra {
        Some(FakeIdp::start(TID).await)
    } else {
        None
    };
    let check = idp.as_ref().map(|idp| {
        let mut tenant = pmos_entra::Tenant::from_settings(
            [
                "PMOS_ENTRA_TENANT_ID",
                "PMOS_ENTRA_CLIENT_ID",
                "PMOS_ENTRA_AUTHORITY",
            ],
            TID,
            CLIENT,
            Some(&idp.base),
        )
        .unwrap();
        // Short, so the key rotation case needn't wait a minute.
        tenant.jwks_refetch_after = std::time::Duration::from_millis(200);
        Arc::new(EntraLinkCheck::new(tenant).unwrap())
    });
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
            web_token: Some(WEB_TOKEN.into()),
            assertions: Arc::new(Assertions::new(Mode::Require, keys).unwrap()),
            entra: check,
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
        idp,
        issued: AtomicI64::new(Utc::now().timestamp() - 600),
    })
}

/// Both stores: memory always, Postgres when a test database is set.
async fn both() -> Vec<Server> {
    let mut v = vec![start(false).await.unwrap()];
    if let Some(s) = start(true).await {
        v.push(s);
    }
    v
}

fn sign_assertion(sub: &str, method: &str, target: &str) -> String {
    let ring = KeyRing::new(Key::from_base64("w1", WEB_KEY).unwrap());
    let claims = Claims::new("web", "toolserver", sub, &[], "unknown").unwrap();
    let req = RequestBinding::new(method, target, b"").unwrap();
    ring.sign(&claims, unix_now(), 60, Some(&req)).unwrap()
}

fn keys(v: &Value) -> BTreeSet<String> {
    v.as_object().unwrap().keys().cloned().collect()
}

fn set(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|s| s.to_string()).collect()
}

/// Nothing an answer may never carry: addresses, GitHub logins, `tid`/`oid`s, and (unless
/// `slack`) Slack ids.
fn assert_no_secrets(text: &str, slack: bool) {
    for bad in ["@contoso.com", "-gh", TID, OID_ADA, OID_CY, OID_NEW, "atl"] {
        assert!(!text.contains(bad), "{bad} in {text}");
    }
    if !slack {
        assert!(!text.contains("U0"), "a Slack id in {text}");
    }
}

/// Every route × method × client token. The table is the exact set each client gets past the
/// gate on (anything but `401`, `403` or `405`); everything else is refused. A route or method
/// that widens a client fails here.
///
/// - `GET /internal/directory`: the orchestrator and the Slack bot.
/// - `GET /internal/users/resolve`: `pmos-web` and the console, with or without an assertion
///   (there is no signed-in subject yet at sign-in), even under `require`.
/// - `POST /internal/users/{handle}/identities/entra`: `pmos-web` only.
#[tokio::test]
async fn every_directory_route_method_and_token() {
    let srv = start(false).await.unwrap();
    const METHODS: [&str; 5] = ["GET", "POST", "PUT", "PATCH", "DELETE"];
    let routes = [
        ("directory", "/internal/directory"),
        ("resolve", "/internal/users/resolve?by=handle&handle=ana"),
        ("link", "/internal/users/ana/identities/entra"),
        ("entra-link", "/internal/users/entra-link"),
    ];
    type Row<'a> = (&'a str, Option<&'a str>, bool, Vec<(&'a str, &'a str)>);
    let clients: [Row; 9] = [
        (
            "orchestrator",
            Some(SERVICE_TOKEN),
            false,
            vec![("GET", "directory")],
        ),
        (
            "slack bot",
            Some(BOT_TOKEN),
            false,
            vec![("GET", "directory")],
        ),
        (
            "slack bot, with a web assertion",
            Some(BOT_TOKEN),
            true,
            vec![("GET", "directory")],
        ),
        (
            "web",
            Some(WEB_TOKEN),
            false,
            vec![("GET", "resolve"), ("POST", "link"), ("GET", "entra-link")],
        ),
        (
            "web, with an assertion",
            Some(WEB_TOKEN),
            true,
            vec![("GET", "resolve"), ("POST", "link"), ("GET", "entra-link")],
        ),
        (
            "console",
            Some(CONSOLE_TOKEN),
            false,
            vec![("GET", "resolve")],
        ),
        (
            "console, with a web assertion",
            Some(CONSOLE_TOKEN),
            true,
            vec![("GET", "resolve")],
        ),
        ("no token", None, false, Vec::new()),
        (
            "unknown token",
            Some("not-a-token-0123456789abcdef"),
            false,
            Vec::new(),
        ),
    ];
    for (name, token, signed, mut want) in clients {
        let mut reached = Vec::new();
        for (route, target) in routes {
            for method in METHODS {
                let a = signed.then(|| sign_assertion("ana", method, target));
                let headers: Vec<(&str, &str)> = a
                    .as_deref()
                    .map(|a| vec![("x-pmos-assertion", a)])
                    .unwrap_or_default();
                let (s, _, _) = srv
                    .send(
                        Method::from_bytes(method.as_bytes()).unwrap(),
                        target,
                        token,
                        &headers,
                        None,
                    )
                    .await;
                match s {
                    401 | 403 | 405 => {}
                    _ => reached.push((method, route)),
                }
            }
        }
        reached.sort();
        want.sort();
        assert_eq!(reached, want, "{name}");
    }
    // No token is 401, a known token on the wrong route 403.
    let (s, _) = srv.get("/internal/directory", WEB_TOKEN).await;
    assert_eq!(s, 403);
    let (s, _, _) = srv
        .send(Method::GET, "/internal/directory", None, &[], None)
        .await;
    assert_eq!(s, 401);
    srv.stop().await;
}

/// Each token sees exactly its fields: the orchestrator's directory four, the Slack bot's
/// those and `slack_id`, `resolve` seven for web and the console; never an address, a GitHub
/// login, a `tid` or an `oid`; web and the console can't list.
#[tokio::test]
async fn each_token_sees_exactly_its_fields() {
    for srv in both().await {
        let (s, orch) = srv.get("/internal/directory", SERVICE_TOKEN).await;
        assert_eq!(s, 200, "{orch}");
        assert_eq!(keys(&orch), set(&["users", "version"]));
        let users = orch["users"].as_array().unwrap();
        assert_eq!(users.len(), 8);
        for u in users {
            assert_eq!(
                keys(u),
                set(&["display_name", "handle", "roles", "status"]),
                "{u}"
            );
        }
        assert_no_secrets(&orch.to_string(), false);
        let ada = users.iter().find(|u| u["handle"] == "ada").unwrap();
        assert_eq!(ada["roles"], json!(["admin", "pm"]));
        assert_eq!(ada["display_name"], "ada Name");
        let dee = users.iter().find(|u| u["handle"] == "dee").unwrap();
        assert_eq!(dee["status"], "deactivated");

        let (s, bot) = srv.get("/internal/directory", BOT_TOKEN).await;
        assert_eq!(s, 200);
        for u in bot["users"].as_array().unwrap() {
            assert_eq!(
                keys(u),
                set(&["display_name", "handle", "roles", "slack_id", "status"]),
                "{u}"
            );
            let want = format!("U0{}", u["handle"].as_str().unwrap().to_uppercase());
            assert_eq!(u["slack_id"], json!(want));
        }
        assert_no_secrets(&bot.to_string(), true);

        for token in [WEB_TOKEN, CONSOLE_TOKEN] {
            for target in [
                "/internal/users/resolve?by=handle&handle=ada".to_string(),
                "/internal/users/resolve?by=sign_in_name&name=ADA@Contoso.com".to_string(),
                format!("/internal/users/resolve?by=entra&tid={TID}&oid={OID_ADA}"),
            ] {
                let (s, one) = srv.get(&target, token).await;
                assert_eq!(s, 200, "{target}: {one}");
                let mut want = vec![
                    "display_name",
                    "dual_role",
                    "entra",
                    "handle",
                    "roles",
                    "sign_in_name_confirmed",
                    "status",
                ];
                // pmos-web's answer also carries the password credential's epoch (null without
                // one), which ends a password session when it moves (docs/password-sign-in.md).
                if token == WEB_TOKEN {
                    want.push("password_epoch");
                    assert_eq!(one["password_epoch"], Value::Null);
                }
                assert_eq!(keys(&one), set(&want));
                assert_eq!(one["handle"], "ada");
                assert_eq!(one["entra"], "linked");
                assert_eq!(one["sign_in_name_confirmed"], true);
                assert_eq!(one["dual_role"], false);
                assert_no_secrets(&one.to_string(), false);
            }
            // Can't list.
            let (s, _) = srv.get("/internal/directory", token).await;
            assert_eq!(s, 403);
        }
        // The other flags.
        let (_, sam) = srv
            .get("/internal/users/resolve?by=handle&handle=sam", WEB_TOKEN)
            .await;
        assert_eq!(sam["sign_in_name_confirmed"], false);
        assert_eq!(sam["entra"], "none");
        let (_, dee) = srv
            .get("/internal/users/resolve?by=handle&handle=dee", WEB_TOKEN)
            .await;
        assert_eq!(dee["status"], "deactivated");
        // Malformed lookups are 422, not misses.
        for bad in [
            "",
            "?by=email&name=ana@contoso.com",
            "?by=entra&tid=nope&oid=nope",
            "?by=entra&oid=aaaaaaaa-0000-0000-0000-00000000000a",
            "?by=sign_in_name&name=not-an-address",
            "?by=handle&handle=Not%20A%20Handle",
        ] {
            let (s, body) = srv
                .get(&format!("/internal/users/resolve{bad}"), WEB_TOKEN)
                .await;
            assert_eq!(s, 422, "{bad}: {body}");
            assert_eq!(body["code"], "invalid");
        }
        srv.stop().await;
    }
}

/// `ETag` is the directory's version, strong; `If-None-Match` with it is `304` with no body;
/// a write through the store changes it.
#[tokio::test]
async fn the_directory_etag_and_304() {
    for srv in both().await {
        let (s, h, text) = srv
            .send(
                Method::GET,
                "/internal/directory",
                Some(BOT_TOKEN),
                &[],
                None,
            )
            .await;
        assert_eq!(s, 200);
        let tag = h["etag"].to_str().unwrap().to_string();
        let body: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(tag, format!("\"{}\"", body["version"]));
        assert!(!tag.starts_with("W/"));
        let version = srv.users().directory(Utc::now()).await.unwrap().version;
        assert_eq!(tag, format!("\"{version}\""));

        for client in [SERVICE_TOKEN, BOT_TOKEN] {
            for inm in [tag.clone(), format!("W/{tag}"), format!("\"0\", {tag}")] {
                let (s, h, text) = srv
                    .send(
                        Method::GET,
                        "/internal/directory",
                        Some(client),
                        &[("if-none-match", &inm)],
                        None,
                    )
                    .await;
                assert_eq!(s, 304, "{inm}");
                assert!(text.is_empty());
                assert_eq!(h["etag"].to_str().unwrap(), tag);
            }
        }
        // A write through the store moves the version.
        srv.users()
            .apply(
                &host(),
                Op::CreateUser {
                    user: person("eve", &[UserRole::Pm]),
                },
                Utc::now(),
            )
            .await
            .unwrap();
        let (s, h, text) = srv
            .send(
                Method::GET,
                "/internal/directory",
                Some(SERVICE_TOKEN),
                &[("if-none-match", &tag)],
                None,
            )
            .await;
        assert_eq!(s, 200);
        let new_tag = h["etag"].to_str().unwrap();
        assert_ne!(new_tag, tag);
        assert!(text.contains("\"eve\""));
        // So does a first link.
        let (s, _) = srv.link("ana", "ana@contoso.com", OID_NEW).await;
        assert_eq!(s, 200);
        let (_, h2, _) = srv
            .send(
                Method::GET,
                "/internal/directory",
                Some(SERVICE_TOKEN),
                &[],
                None,
            )
            .await;
        assert_ne!(h2["etag"].to_str().unwrap(), new_tag);
        srv.stop().await;
    }
}

/// Each client gets at most 20 misses a minute; the 21st is `429 resolve_misses`, while hits
/// keep answering. Clients have their own windows, and the first-link route's misses count
/// for `pmos-web` too.
#[tokio::test]
async fn the_21st_miss_in_a_minute_is_429_while_hits_keep_answering() {
    let srv = start(false).await.unwrap();
    for i in 0..MISSES_PER_MINUTE {
        let target = match i % 3 {
            0 => format!("/internal/users/resolve?by=handle&handle=ghost{i}"),
            1 => format!("/internal/users/resolve?by=sign_in_name&name=ghost{i}@contoso.com"),
            _ => format!("/internal/users/resolve?by=entra&tid={TID}&oid={OID_NEW}"),
        };
        let (s, body) = srv.get(&target, WEB_TOKEN).await;
        assert_eq!(s, 404, "miss {i}: {body}");
        assert_eq!(body["code"], "not_found");
    }
    let (s, h, text) = srv
        .send(
            Method::GET,
            "/internal/users/resolve?by=handle&handle=ghost21",
            Some(WEB_TOKEN),
            &[],
            None,
        )
        .await;
    assert_eq!(s, 429, "{text}");
    let body: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(body["code"], "resolve_misses");
    let retry: u64 = h["retry-after"].to_str().unwrap().parse().unwrap();
    assert!((1..=60).contains(&retry));
    // Hits keep answering, as many as asked.
    for _ in 0..30 {
        let (s, one) = srv
            .get("/internal/users/resolve?by=handle&handle=ana", WEB_TOKEN)
            .await;
        assert_eq!(s, 200);
        assert_eq!(one["handle"], "ana");
    }
    let (s, _) = srv
        .get(
            &format!("/internal/users/resolve?by=entra&tid={TID}&oid={OID_ADA}"),
            WEB_TOKEN,
        )
        .await;
    assert_eq!(s, 200);
    // A first link that finds nobody is a miss too: limited now.
    let (s, body) = srv.link("ghost", "ghost@contoso.com", OID_NEW).await;
    assert_eq!(s, 429, "{body}");
    // A first link that finds its user is a hit.
    let (s, body) = srv.link("ana", "ana@contoso.com", OID_NEW).await;
    assert_eq!(s, 200, "{body}");
    // The console has its own window.
    let (s, _) = srv
        .get(
            "/internal/users/resolve?by=handle&handle=ghost",
            CONSOLE_TOKEN,
        )
        .await;
    assert_eq!(s, 404);
    // No miss logged or answered names the value; nothing was audited for resolve.
    assert!(srv
        .events()
        .await
        .iter()
        .all(|e| !e.tool.starts_with("users.resolve")));
    srv.stop().await;
}

/// Every first-link condition (§7 step 2), on both stores, and what each records.
#[tokio::test]
async fn first_link_conditions() {
    for srv in both().await {
        let before = srv.users().directory(Utc::now()).await.unwrap().version;
        let cases: [(&str, &str, &str, u16, &str); 8] = [
            // Unconfirmed sign-in name (joined through the Slack bot client).
            (
                "sam",
                "sam@contoso.com",
                OID_NEW,
                403,
                "sign_in_unconfirmed",
            ),
            // An Admin (no link yet) and an Auditor: the host links them.
            ("raj", "raj@contoso.com", OID_NEW, 403, "host_only"),
            ("aud", "aud@contoso.com", OID_NEW, 403, "host_only"),
            // Already linked to another Microsoft account: a conflict.
            ("cy", "cy@contoso.com", OID_NEW, 409, "linked"),
            // Deactivated.
            ("dee", "dee@contoso.com", OID_NEW, 403, "not_active"),
            // Another user's Microsoft account: a conflict.
            ("ana", "ana@contoso.com", OID_CY, 409, "taken"),
            // The sign-in name isn't this user's; no such user.
            ("ana", "ben@contoso.com", OID_NEW, 404, "not_found"),
            ("ghost", "ghost@contoso.com", OID_NEW, 404, "not_found"),
        ];
        for (handle, name, oid, status, code) in cases {
            let (s, body) = srv.link(handle, name, oid).await;
            assert_eq!((s, body["code"].as_str()), (status, Some(code)), "{handle}");
            assert_no_secrets(&body.to_string(), false);
        }
        // Malformed. The old body, a bare `(tid, oid)` and a name on pmos-web's word, is
        // refused outright (U1); so is a token without the nonce, or anything extra.
        let token = srv.token(OID_NEW, "ana@contoso.com");
        for body in [
            json!({ "tid": TID, "oid": OID_NEW, "sign_in_name": "ana@contoso.com" }),
            json!({ "id_token": token }),
            json!({ "id_token": token, "nonce": NONCE, "oid": OID_NEW }),
        ] {
            let (s, _, text) = srv
                .send(
                    Method::POST,
                    "/internal/users/ana/identities/entra",
                    Some(WEB_TOKEN),
                    &[],
                    Some(body),
                )
                .await;
            assert_eq!(s, 422, "{text}");
        }
        // Nothing was written by any refusal.
        let dir = srv.users().directory(Utc::now()).await.unwrap();
        assert_eq!(dir.version, before);
        assert!(dir
            .get("ana")
            .unwrap()
            .identity(IdentityKind::Entra)
            .is_none());

        // A confirmed, active PM with no link: linked, with any case in the request.
        let (s, body) = srv
            .link("ana", "Ana@Contoso.com", &OID_NEW.to_uppercase())
            .await;
        assert_eq!(s, 200, "{body}");
        assert_eq!(body["first_link"], true);
        assert_eq!(body["user"]["handle"], "ana");
        assert_eq!(body["user"]["entra"], "linked");
        assert_no_secrets(&body.to_string(), false);
        let dir = srv.users().directory(Utc::now()).await.unwrap();
        assert_eq!(dir.version, before + 1);
        let e = dir
            .get("ana")
            .unwrap()
            .identity(IdentityKind::Entra)
            .unwrap();
        assert_eq!((e.issuer.as_str(), e.subject.as_str()), (TID, OID_NEW));
        assert!(!e.host_confirmed);
        // Now `resolve` finds her by the key.
        let (s, one) = srv
            .get(
                &format!("/internal/users/resolve?by=entra&tid={TID}&oid={OID_NEW}"),
                CONSOLE_TOKEN,
            )
            .await;
        assert_eq!((s, one["handle"].as_str()), (200, Some("ana")));
        // The same link again writes nothing.
        let (s, body) = srv.link("ana", "ana@contoso.com", OID_NEW).await;
        assert_eq!((s, body["first_link"].as_bool()), (200, Some(false)));
        // Another Microsoft account for her now is a conflict; hers for ben is taken.
        let (s, body) = srv.link("ana", "ana@contoso.com", OID_NEW2).await;
        assert_eq!((s, body["code"].as_str()), (409, Some("linked")));
        let (s, body) = srv.link("ben", "ben@contoso.com", OID_NEW).await;
        assert_eq!((s, body["code"].as_str()), (409, Some("taken")));
        assert_eq!(
            srv.users().directory(Utc::now()).await.unwrap().version,
            before + 1
        );

        // role_changes: one link row, by ana through web.
        let changes = srv.users().role_changes(Some("ana"), 10).await.unwrap();
        let link = changes
            .iter()
            .find(|c| c.change == ChangeKind::Link && c.detail.as_deref() == Some("entra"))
            .unwrap();
        assert_eq!(link.actor, "ana");

        // The audit: fixed details only, never an address or a key.
        let events: Vec<AuditEvent> = srv
            .events()
            .await
            .into_iter()
            .filter(|e| e.tool == "users.identity_link" || e.tool == "users.identity_conflict")
            .collect();
        for e in &events {
            let d = e.detail.as_deref().unwrap();
            assert!(pmos_toolserver::users::detail::is_safe(d), "{d}");
            assert_no_secrets(&format!("{e:?}"), false);
            assert_eq!(e.client, Client::Web);
        }
        let ok: Vec<_> = events.iter().filter(|e| e.outcome == Outcome::Ok).collect();
        assert_eq!(ok.len(), 1);
        assert_eq!(
            (ok[0].user.as_str(), ok[0].output_ref.as_deref()),
            ("ana", Some("ana"))
        );
        let denied = |tool: &str, target: Option<&str>, detail: &str| {
            events.iter().any(|e| {
                e.tool == tool
                    && e.outcome == Outcome::Denied
                    && e.user == "anonymous"
                    && e.output_ref.as_deref() == target
                    && e.detail.as_deref() == Some(detail)
            })
        };
        assert!(denied(
            "users.identity_link",
            Some("sam"),
            "entra; sign_in unconfirmed"
        ));
        assert!(denied(
            "users.identity_link",
            Some("raj"),
            "entra; host_only"
        ));
        assert!(denied(
            "users.identity_link",
            Some("aud"),
            "entra; host_only"
        ));
        assert!(denied(
            "users.identity_link",
            Some("dee"),
            "entra; not_active"
        ));
        assert!(denied("users.identity_link", None, "entra; not_found"));
        assert!(denied("users.identity_link", Some("ana"), "entra; invalid"));
        assert!(denied("users.identity_conflict", Some("cy"), "entra"));
        assert!(denied("users.identity_conflict", Some("ana"), "entra"));
        assert!(denied("users.identity_conflict", Some("ben"), "entra"));
        srv.stop().await;
    }
}

/// Two users racing for one Microsoft account, and two accounts racing for one user, through
/// the API: exactly one wins each time, on both stores.
#[tokio::test]
async fn racing_first_links_exactly_one_wins() {
    for srv in both().await {
        for round in 0..6 {
            let (p, q) = (format!("r{round}p"), format!("r{round}q"));
            for h in [&p, &q] {
                srv.users()
                    .apply(
                        &host(),
                        Op::CreateUser {
                            user: person(h, &[UserRole::Pm]),
                        },
                        Utc::now(),
                    )
                    .await
                    .unwrap();
            }
            let oid = format!("cccccccc-0000-0000-0000-00000000000{round}");
            let other = format!("dddddddd-0000-0000-0000-00000000000{round}");
            let (pn, qn) = (format!("{p}@contoso.com"), format!("{q}@contoso.com"));
            // Even rounds: two people, one key. Odd rounds: one person, two keys.
            let (a, b) = if round % 2 == 0 {
                tokio::join!(srv.link(&p, &pn, &oid), srv.link(&q, &qn, &oid))
            } else {
                tokio::join!(srv.link(&p, &pn, &oid), srv.link(&p, &pn, &other))
            };
            let mut statuses = [a.0, b.0];
            statuses.sort();
            assert_eq!(statuses, [200, 409], "round {round}: {a:?} {b:?}");
            let loser = if a.0 == 409 { &a.1 } else { &b.1 };
            let want = if round % 2 == 0 { "taken" } else { "linked" };
            assert_eq!(loser["code"], want, "round {round}");
            // Never two people with one key, never two keys on one person.
            let dir = srv.users().directory(Utc::now()).await.unwrap();
            let keys: Vec<(String, String)> = dir
                .users
                .values()
                .filter_map(|u| u.identity(IdentityKind::Entra))
                .map(|e| (e.issuer.clone(), e.subject.clone()))
                .collect();
            let unique: BTreeSet<_> = keys.iter().cloned().collect();
            assert_eq!(keys.len(), unique.len(), "round {round}");
            let linked = [&p, &q]
                .iter()
                .filter(|h| dir.get(h).unwrap().identity(IdentityKind::Entra).is_some())
                .count();
            assert_eq!(linked, 1, "round {round}");
        }
        srv.stop().await;
    }
}

/// In Postgres with raw SQL and no application lock or check: two transactions insert the same
/// `(tid, oid)` for two users. The second insert waits on the unique key until the first
/// commits, then fails; exactly one identity exists. Likewise two keys for one person.
#[tokio::test]
async fn trigger_free_race_the_unique_key_decides() {
    let Some(srv) = start(true).await else { return };
    let pg = srv.pg.as_ref().unwrap();
    let insert = |handle: &str, oid: &str| {
        format!(
            "INSERT INTO user_identities (handle, kind, issuer, subject, linked_at, linked_by,
                 linked_client, host_confirmed)
             VALUES ('{handle}', 'entra', '{TID}', '{oid}', now(), '{handle}', 'link', false)"
        )
    };
    for (first, second, constraint) in [
        (
            insert("ana", OID_NEW),
            insert("ben", OID_NEW),
            "user_identities_kind_issuer_subject_key",
        ),
        (
            insert("sam", OID_NEW2),
            insert("sam", "eeeeeeee-0000-0000-0000-000000000001"),
            "user_identities_handle_kind_key",
        ),
    ] {
        let mut a = pg.store.pool().begin().await.unwrap();
        sqlx::query(sqlx::AssertSqlSafe(first))
            .execute(&mut *a)
            .await
            .unwrap();
        let pool = pg.store.pool().clone();
        let loser = tokio::spawn(async move {
            let mut b = pool.begin().await.unwrap();
            // Blocks on the unique index until `a` commits.
            sqlx::query(sqlx::AssertSqlSafe(second))
                .execute(&mut *b)
                .await
                .map(|_| ())
        });
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(
            !loser.is_finished(),
            "the second insert waits for the first"
        );
        a.commit().await.unwrap();
        let err = loser.await.unwrap().unwrap_err();
        match &err {
            sqlx::Error::Database(db) => {
                assert_eq!(db.code().as_deref(), Some("23505"), "{err}");
                assert_eq!(db.constraint(), Some(constraint), "{err}");
            }
            other => panic!("{other}"),
        }
    }
    let n: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT count(*) FROM user_identities WHERE kind = 'entra' AND subject = '{OID_NEW}'"
    )))
    .fetch_one(pg.store.pool())
    .await
    .unwrap();
    assert_eq!(n, 1);
    srv.stop().await;
}

/// No Ana link, nothing written: the user database's version is unchanged since `before`.
async fn assert_nothing_linked(srv: &Server, before: i64) {
    let dir = srv.users().directory(Utc::now()).await.unwrap();
    assert_eq!(dir.version, before);
    assert!(dir
        .get("ana")
        .unwrap()
        .identity(IdentityKind::Entra)
        .is_none());
}

/// The route checks the ID token itself (U1): forged, unsigned, another audience, issuer or
/// tenant, expired, a guest, no `acct`, another nonce. Each is refused, audited (`entra;
/// <code>`, target Ana), counted as a miss, and writes nothing. On both stores.
#[tokio::test]
async fn the_first_link_validates_the_id_token_itself() {
    for srv in both().await {
        let before = srv.users().directory(Utc::now()).await.unwrap().version;
        let good = srv.claims(OID_NEW, "ana@contoso.com");
        let with = |k: &str, v: Value| {
            let mut c = good.clone();
            c[k] = v;
            sign(&c, keys::KEY_A_DER, "key-a")
        };
        let base = srv.idp.as_ref().unwrap().base.clone();
        let cases: Vec<(&str, String, u16, &str)> = vec![
            (
                "forged: signed with a key the tenant doesn't publish",
                sign(&good, keys::KEY_B_DER, "key-a"),
                422,
                "invalid_token",
            ),
            ("unsigned", unsigned(&good, "key-a"), 422, "invalid_token"),
            (
                "another application's token",
                with("aud", json!("aaaaaaaa-0000-0000-0000-0000000000ff")),
                422,
                "invalid_token",
            ),
            (
                "another issuer",
                with(
                    "iss",
                    json!(format!("{base}/99999999-2222-3333-4444-555555555555/v2.0")),
                ),
                422,
                "invalid_token",
            ),
            (
                "another tenant",
                with("tid", json!("99999999-2222-3333-4444-555555555555")),
                422,
                "invalid_token",
            ),
            (
                "expired",
                with("exp", json!(Utc::now().timestamp() - 3600)),
                422,
                "invalid_token",
            ),
            (
                "another sign-in's nonce",
                with("nonce", json!("not-this-one")),
                422,
                "invalid_token",
            ),
            ("a guest", with("acct", json!(1)), 403, "not_a_member"),
            (
                "another identity provider",
                with("idp", json!("live.com")),
                403,
                "not_a_member",
            ),
            (
                "no acct claim",
                {
                    let mut c = good.clone();
                    c.as_object_mut().unwrap().remove("acct");
                    sign(&c, keys::KEY_A_DER, "key-a")
                },
                422,
                "entra_misconfigured",
            ),
        ];
        for (name, token, status, code) in &cases {
            let (s, body) = srv.link_token("ana", token).await;
            assert_eq!((s, body["code"].as_str()), (*status, Some(*code)), "{name}");
            assert_no_secrets(&body.to_string(), false);
        }
        assert_nothing_linked(&srv, before).await;
        let events = srv.events().await;
        for code in ["invalid_token", "not_a_member", "entra_misconfigured"] {
            let detail = format!("entra; {code}");
            assert!(
                events.iter().any(|e| e.tool == "users.identity_link"
                    && e.outcome == Outcome::Denied
                    && e.user == "anonymous"
                    && e.output_ref.as_deref() == Some("ana")
                    && e.detail.as_deref() == Some(detail.as_str())),
                "{code}"
            );
            assert!(pmos_toolserver::users::detail::is_safe(&detail));
        }
        // Every refusal counted as a miss: the 11th..20th are still answered, then 429.
        for i in cases.len()..MISSES_PER_MINUTE {
            let (s, _) = srv
                .link_token("ana", &sign(&good, keys::KEY_B_DER, "key-a"))
                .await;
            assert_eq!(s, 422, "refusal {i}");
        }
        let (s, body) = srv
            .link_token("ana", &sign(&good, keys::KEY_B_DER, "key-a"))
            .await;
        assert_eq!((s, body["code"].as_str()), (429, Some("resolve_misses")));
        srv.stop().await;
    }
}

/// A token links once (U1): presented again, even for the same person, it is `403
/// token_used`, audited; a later sign-in's token (a new `iat`) is its own.
#[tokio::test]
async fn a_replayed_token_is_refused() {
    for srv in both().await {
        let token = srv.token(OID_NEW, "ana@contoso.com");
        let (s, body) = srv.link_token("ana", &token).await;
        assert_eq!(
            (s, body["first_link"].as_bool()),
            (200, Some(true)),
            "{body}"
        );
        for handle in ["ana", "ben"] {
            let (s, body) = srv.link_token(handle, &token).await;
            assert_eq!(
                (s, body["code"].as_str()),
                (403, Some("token_used")),
                "{handle}"
            );
        }
        assert!(srv
            .events()
            .await
            .iter()
            .any(|e| e.outcome == Outcome::Denied
                && e.detail.as_deref() == Some("entra; token_used")));
        // Her next sign-in's token answers as before: already linked, nothing written.
        let (s, body) = srv.link("ana", "ana@contoso.com", OID_NEW).await;
        assert_eq!((s, body["first_link"].as_bool()), (200, Some(false)));
        srv.stop().await;
    }
}

/// The takeover from the review (U1), end to end: someone holding only the web token, and a
/// Microsoft account of their own in the tenant, confirms Ana's sign-in name through
/// `resolve`, then tries to link their own `oid` to her. The old body (their `oid`, her name)
/// is refused; their real token names their own sign-in name, not hers (`not_found`); a token
/// they make up naming her is forged or unsigned. Nothing is linked, and Ana's own first
/// sign-in later links her own account, which the admin overview lists (handle, time,
/// provider; never the `oid`).
#[tokio::test]
async fn the_takeover_from_the_review_fails_end_to_end() {
    for srv in both().await {
        let before = srv.users().directory(Utc::now()).await.unwrap().version;
        const MALLORY_OID: &str = "eeeeeeee-0000-0000-0000-00000000000e";
        let (s, ana) = srv
            .get(
                "/internal/users/resolve?by=sign_in_name&name=ana@contoso.com",
                WEB_TOKEN,
            )
            .await;
        assert_eq!((s, ana["handle"].as_str()), (200, Some("ana")));
        // 1. The request the review described: refused outright.
        let (s, _) = srv
            .link_body(
                "ana",
                json!({ "tid": TID, "oid": MALLORY_OID, "sign_in_name": "ana@contoso.com" }),
            )
            .await;
        assert_eq!(s, 422);
        // 2. Mallory's own valid token: it names her, not Ana.
        let (s, body) = srv.link("ana", "mallory@contoso.com", MALLORY_OID).await;
        assert_eq!((s, body["code"].as_str()), (404, Some("not_found")));
        // 3. A token naming Ana with Mallory's oid: she can't get the tenant to sign it.
        let forged = srv.claims(MALLORY_OID, "ana@contoso.com");
        for token in [
            sign(&forged, keys::KEY_B_DER, "key-a"),
            sign(&forged, keys::KEY_B_DER, "key-b"),
            unsigned(&forged, "key-a"),
        ] {
            let (s, body) = srv.link_token("ana", &token).await;
            assert_eq!((s, body["code"].as_str()), (422, Some("invalid_token")));
        }
        assert_nothing_linked(&srv, before).await;
        // Mallory's oid finds nobody.
        let (s, _) = srv
            .get(
                &format!("/internal/users/resolve?by=entra&tid={TID}&oid={MALLORY_OID}"),
                WEB_TOKEN,
            )
            .await;
        assert_eq!(s, 404);

        // Ana's own first sign-in links her own account.
        let (s, body) = srv.link("ana", "ana@contoso.com", OID_NEW).await;
        assert_eq!((s, body["first_link"].as_bool()), (200, Some(true)));
        let e = srv
            .users()
            .directory(Utc::now())
            .await
            .unwrap()
            .get("ana")
            .unwrap()
            .identity(IdentityKind::Entra)
            .cloned()
            .unwrap();
        assert_eq!(e.subject, OID_NEW);
        // The admin overview lists the first link: who, when, which provider; no oid.
        let target = "/internal/admin/overview?user=raj";
        let a = sign_assertion("raj", "GET", target);
        let (s, _, text) = srv
            .send(
                Method::GET,
                target,
                Some(WEB_TOKEN),
                &[("x-pmos-assertion", &a)],
                None,
            )
            .await;
        assert_eq!(s, 200, "{text}");
        let v: Value = serde_json::from_str(&text).unwrap();
        let links = v["users"]["first_links"].as_array().unwrap();
        assert_eq!(links.len(), 1, "{v}");
        assert_eq!(links[0]["handle"], "ana");
        assert_eq!(links[0]["provider"], "entra");
        assert!(links[0]["at"].is_string());
        assert_no_secrets(&text, false);
        assert!(!text.contains(MALLORY_OID));
        srv.stop().await;
    }
}

/// The tenant rotates its keys: a token under a `kid` not yet fetched refetches the JWKS
/// (at most once per interval) and links. With the tenant's endpoints down, nothing links
/// (`503 entra_unavailable`): fail closed.
#[tokio::test]
async fn key_rotation_refetches_and_keys_down_fail_closed() {
    let srv = start(false).await.unwrap();
    let idp = srv.idp.as_ref().unwrap();
    let (s, _) = srv.link("ana", "ana@contoso.com", OID_NEW).await;
    assert_eq!(s, 200);
    assert_eq!(idp.jwks_hits(), 1);
    idp.set_keys(vec![
        jwk("key-a", keys::KEY_A_N),
        jwk("key-b", keys::KEY_B_N),
    ]);
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    let rotated = sign(
        &srv.claims(OID_NEW2, "ben@contoso.com"),
        keys::KEY_B_DER,
        "key-b",
    );
    let (s, body) = srv.link_token("ben", &rotated).await;
    assert_eq!(s, 200, "{body}");
    assert_eq!(idp.jwks_hits(), 2);
    srv.stop().await;

    let srv = start(false).await.unwrap();
    srv.idp.as_ref().unwrap().set_down(true);
    let before = srv.users().directory(Utc::now()).await.unwrap().version;
    for _ in 0..3 {
        let (s, body) = srv.link("ana", "ana@contoso.com", OID_NEW).await;
        assert_eq!((s, body["code"].as_str()), (503, Some("entra_unavailable")));
    }
    assert_nothing_linked(&srv, before).await;
    srv.stop().await;
}

/// Without Entra settings the route answers `501 not_configured` and links nothing, whatever
/// the body; `GET /internal/users/entra-link` says so. With them it names the tenant and
/// client id.
#[tokio::test]
async fn no_entra_settings_is_501_and_says_so() {
    let srv = start_with(false, false).await.unwrap();
    let (s, v) = srv.get("/internal/users/entra-link", WEB_TOKEN).await;
    assert_eq!(s, 200);
    assert_eq!(
        v,
        json!({"configured": false, "tenant_id": null, "client_id": null})
    );
    for body in [
        json!({ "id_token": "x.y.z", "nonce": NONCE }),
        json!({ "tid": TID, "oid": OID_NEW, "sign_in_name": "ana@contoso.com" }),
    ] {
        let (s, v) = srv.link_body("ana", body).await;
        assert_eq!((s, v["code"].as_str()), (501, Some("not_configured")));
    }
    assert!(srv
        .users()
        .directory(Utc::now())
        .await
        .unwrap()
        .get("ana")
        .unwrap()
        .identity(IdentityKind::Entra)
        .is_none());
    srv.stop().await;

    let srv = start(false).await.unwrap();
    let (s, v) = srv.get("/internal/users/entra-link", WEB_TOKEN).await;
    assert_eq!(s, 200);
    assert_eq!(
        v,
        json!({"configured": true, "tenant_id": TID, "client_id": CLIENT})
    );
    let (s, _) = srv.get("/internal/users/entra-link", CONSOLE_TOKEN).await;
    assert_eq!(s, 403);
    srv.stop().await;
}
