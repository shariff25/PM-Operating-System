//! The user database's read API (`docs/user-database-spec.md` §2, §7; slice U2):
//! `GET /internal/directory`, `GET /internal/users/resolve` and
//! `POST /internal/users/{handle}/identities/entra`.
//!
//! Who may call each route (every route × method × token), what each token sees (exactly its
//! fields), the ETag and `304`, the miss-only rate limit, every first-link condition, and two
//! links racing for one identity: through the API on both stores, and in Postgres with raw SQL
//! and no application lock, where the unique key alone decides.

mod common;

use std::collections::BTreeSet;
use std::sync::Arc;

use chrono::Utc;
use common::{Fixture, TestPg};
use pmos_assertion::{unix_now, Claims, Key, KeyRing, RequestBinding};
use pmos_toolserver::app::directory::MISSES_PER_MINUTE;
use pmos_toolserver::app::BotAccess;
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

    async fn link(&self, handle: &str, name: &str, oid: &str) -> (u16, Value) {
        let (s, _, text) = self
            .send(
                Method::POST,
                &format!("/internal/users/{handle}/identities/entra"),
                Some(WEB_TOKEN),
                &[],
                Some(json!({ "tid": TID, "oid": oid, "sign_in_name": name })),
            )
            .await;
        (s, serde_json::from_str(&text).unwrap_or(Value::Null))
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

/// Both stores: memory always, Postgres when a test database is set.
async fn both() -> Vec<Server> {
    let mut v = vec![start(false).await.unwrap()];
    if let Some(s) = start(true).await {
        v.push(s);
    }
    v
}

fn sign(sub: &str, method: &str, target: &str) -> String {
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
            vec![("GET", "resolve"), ("POST", "link")],
        ),
        (
            "web, with an assertion",
            Some(WEB_TOKEN),
            true,
            vec![("GET", "resolve"), ("POST", "link")],
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
                let a = signed.then(|| sign("ana", method, target));
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
                assert_eq!(
                    keys(&one),
                    set(&[
                        "display_name",
                        "dual_role",
                        "entra",
                        "handle",
                        "roles",
                        "sign_in_name_confirmed",
                        "status",
                    ])
                );
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
        // Malformed.
        for body in [
            json!({ "tid": TID, "oid": "nope", "sign_in_name": "ana@contoso.com" }),
            json!({ "tid": TID, "oid": OID_NEW }),
            json!({ "tid": TID, "oid": OID_NEW, "sign_in_name": "ana@contoso.com", "email": "x" }),
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
