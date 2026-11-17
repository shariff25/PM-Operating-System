//! The read-only admin and auditor routes (`GET /internal/admin/*`): who may call them (the
//! orchestrator and `pmos-web` only), whose view they serve (the `user=` viewer's role in
//! `team.yaml`, checked here), the metadata-only projection of audit events, pagination and
//! filters, and that `PMOS_WEB_TOKEN` still reaches nothing else.

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use common::{Fixture, TestPg};
use pmos_assertion::{unix_now, Claims, Key, KeyRing, RequestBinding};
use pmos_contract::model::Role;
use pmos_toolserver::app::BotAccess;
use pmos_toolserver::assertion::{parse_keys, Assertions, Mode, AUDIENCE, ISSUERS};
use pmos_toolserver::audit::{AuditEvent, Client, Outcome};
use pmos_toolserver::{router_full, AppConfig, PersonClients};
use reqwest::Method;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

const SERVICE_TOKEN: &str = "test-service-token-0123456789abcdef";
const BOT_TOKEN: &str = "test-bot-token-0123456789abcdef-0123";
const WEB_TOKEN: &str = "test-web-token-0123456789abcdef-01234";
const CONSOLE_TOKEN: &str = "test-console-token-0123456789abcdef-0";
// Test key only: fixed bytes, never used anywhere real.
const WEB_KEY: &str = "d2ViLXRvb2xzZXJ2ZXItdGVzdC1rZXktMDEyMzQ1Njc=";

const ROUTES: [&str; 4] = ["audit", "accounts", "connectors", "overview"];

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
}

async fn start(mode: Mode, postgres: bool) -> Option<Server> {
    let (f, pg) = common::roles_fixture(
        &[
            ("ada", Role::Admin),
            ("aud", Role::Auditor),
            ("ana", Role::Pm),
            ("rev", Role::Reviewer),
        ],
        postgres,
    )
    .await?;
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
            assertions: Arc::new(Assertions::new(mode, keys).unwrap()),
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

fn sign(sub: &str, method: &str, target: &str) -> String {
    let ring = KeyRing::new(Key::from_base64("w1", WEB_KEY).unwrap());
    let claims = Claims::new("web", "toolserver", sub, &[], "unknown").unwrap();
    let req = RequestBinding::new(method, target, b"").unwrap();
    ring.sign(&claims, unix_now(), 60, Some(&req)).unwrap()
}

impl Server {
    async fn send(
        &self,
        method: Method,
        target: &str,
        token: Option<&str>,
        assertion: Option<&str>,
    ) -> (u16, Value) {
        let mut r = reqwest::Client::new().request(method, format!("{}{target}", self.base));
        if let Some(t) = token {
            r = r.bearer_auth(t);
        }
        if let Some(a) = assertion {
            r = r.header("x-pmos-assertion", a);
        }
        let res = r.send().await.unwrap();
        let status = res.status().as_u16();
        let text = res.text().await.unwrap();
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    /// A `pmos-web` GET signed for `viewer`.
    async fn web(&self, viewer: &str, target: &str) -> (u16, Value) {
        let a = sign(viewer, "GET", target);
        self.send(Method::GET, target, Some(WEB_TOKEN), Some(&a))
            .await
    }

    async fn search(&self, viewer: &str, query: &str) -> Value {
        let (status, body) = self
            .web(
                viewer,
                &format!("/internal/admin/audit?user={viewer}&{query}"),
            )
            .await;
        assert_eq!(status, 200, "{body}");
        body
    }
}

fn at(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}

fn event(at_: &str, user: &str, tool: &str, outcome: Outcome) -> AuditEvent {
    AuditEvent {
        at: at(at_),
        client: Client::Mcp,
        run_id: "tsk_1".into(),
        user: user.into(),
        skill: None,
        bot: Some("pm-ana".into()),
        tool: tool.into(),
        outcome,
        sources: vec![],
        source_titles: BTreeMap::new(),
        args_hash: Some("hash-abc".into()),
        output_ref: None,
        detail: None,
    }
}

/// Five events on 2026-11-14 at 08:00:00 to 08:00:04, and one sensitive-looking one.
async fn seed(f: &Fixture) {
    let audit = &f.stores.audit;
    for (i, (user, tool, outcome)) in [
        ("ana", "jira.search", Outcome::Ok),
        ("ana", "jira.create_issue", Outcome::Pending),
        ("ben", "slack.post_reply", Outcome::Denied),
        ("ana", "jira.create_issue", Outcome::Approved),
        ("ben", "confluence.read", Outcome::Error),
    ]
    .into_iter()
    .enumerate()
    {
        audit
            .record(event(
                &format!("2026-11-14T08:00:0{i}Z"),
                user,
                tool,
                outcome,
            ))
            .await
            .unwrap();
    }
    let mut secret = event(
        "2026-11-13T09:00:00Z",
        "cy",
        "confluence.create_draft",
        Outcome::Rejected,
    );
    secret.sources = vec!["jira:PAY-212".into(), "slack:C0PAY/1731.0001".into()];
    secret
        .source_titles
        .insert("jira:PAY-212".into(), "SECRET-TITLE Project Falcon".into());
    secret.detail = Some("reason: SECRET-REASON the draft quotes the CFO".into());
    secret.output_ref = Some("apr_secret".into());
    audit.record(secret).await.unwrap();
}

#[tokio::test]
async fn role_matrix_at_the_tool_server() {
    let srv = start(Mode::Require, false).await.unwrap();
    let expect = |viewer: &str, route: &str| match viewer {
        "ada" => 200,
        "aud" if route != "connectors" => 200,
        _ => 403,
    };
    for viewer in ["ada", "aud", "ana", "rev", "nobody"] {
        for route in ROUTES {
            let (status, body) = srv
                .web(viewer, &format!("/internal/admin/{route}?user={viewer}"))
                .await;
            assert_eq!(status, expect(viewer, route), "{viewer} {route}: {body}");
            if status == 403 {
                assert_eq!(body["code"], "forbidden", "{body}");
            }
        }
        let (status, _) = srv
            .web(
                viewer,
                &format!("/internal/admin/audit?user={viewer}&purpose=export"),
            )
            .await;
        assert_eq!(status, expect(viewer, "audit"), "{viewer} export");
    }
    // The orchestrator names a viewer too, and the viewer's role still decides.
    let (status, _) = srv
        .send(
            Method::GET,
            "/internal/admin/audit?user=ada",
            Some(SERVICE_TOKEN),
            None,
        )
        .await;
    assert_eq!(status, 200);
    let (status, _) = srv
        .send(
            Method::GET,
            "/internal/admin/audit?user=ana",
            Some(SERVICE_TOKEN),
            None,
        )
        .await;
    assert_eq!(status, 403);
    let (status, body) = srv
        .send(
            Method::GET,
            "/internal/admin/audit",
            Some(SERVICE_TOKEN),
            None,
        )
        .await;
    assert_eq!(status, 400, "{body}");
    // Every read is audited, refusals too, with no query values.
    let events = srv.f.audit.events();
    let reads: Vec<_> = events.iter().filter(|e| e.run_id == "admin").collect();
    assert!(reads
        .iter()
        .any(|e| e.user == "aud" && e.tool == "admin.audit" && e.outcome == Outcome::Ok));
    assert!(reads.iter().any(|e| e.user == "ana"
        && e.tool == "admin.connectors"
        && e.outcome == Outcome::Denied
        && e.client == Client::Web));
    assert!(reads
        .iter()
        .any(|e| e.user == "aud" && e.tool == "admin.audit_export" && e.outcome == Outcome::Ok));
    assert!(reads
        .iter()
        .all(|e| e.detail.is_none() && e.sources.is_empty()));
    srv.stop().await;
}

#[tokio::test]
async fn the_web_token_reaches_the_admin_reads_and_nothing_more() {
    let srv = start(Mode::Verify, false).await.unwrap();
    // Not sessions, grants, calls, account linking, credentials, the ledger or reindexing.
    for (method, target) in [
        (Method::POST, "/internal/sessions"),
        (Method::DELETE, "/internal/session"),
        (Method::POST, "/internal/grants"),
        (Method::POST, "/internal/call"),
        (Method::GET, "/internal/credentials?user=ada"),
        (Method::DELETE, "/internal/credentials/ada/atlassian"),
        (Method::GET, "/internal/oauth/atlassian/start?user=ada"),
        (Method::GET, "/internal/ledger"),
        (Method::GET, "/internal/ledger/summary"),
        (Method::POST, "/internal/reindex"),
        (Method::GET, "/internal/members/requests"),
        (Method::GET, "/internal/onboarding"),
    ] {
        let (status, body) = srv
            .send(method.clone(), target, Some(WEB_TOKEN), None)
            .await;
        assert_eq!(status, 403, "{method} {target}: {body}");
    }
    // The admin reads only read.
    for route in ROUTES {
        let target = format!("/internal/admin/{route}?user=ada");
        let (status, _) = srv.send(Method::POST, &target, Some(WEB_TOKEN), None).await;
        assert_eq!(status, 405, "POST {route}");
    }
    // The Slack bot and the console may not use them; no token is no entry.
    for token in [BOT_TOKEN, CONSOLE_TOKEN] {
        for route in ROUTES {
            let target = format!("/internal/admin/{route}?user=ada");
            let (status, _) = srv.send(Method::GET, &target, Some(token), None).await;
            assert_eq!(status, 403, "{route}");
        }
    }
    let (status, _) = srv
        .send(Method::GET, "/internal/admin/audit?user=ada", None, None)
        .await;
    assert_eq!(status, 401);
    srv.stop().await;
}

#[tokio::test]
async fn the_viewer_must_be_the_assertions_subject() {
    let srv = start(Mode::Require, false).await.unwrap();
    // No assertion.
    let (status, body) = srv
        .send(
            Method::GET,
            "/internal/admin/audit?user=ada",
            Some(WEB_TOKEN),
            None,
        )
        .await;
    assert_eq!(status, 401, "{body}");
    assert_eq!(body["code"], "bad_assertion");
    // A PM's assertion naming an admin as the viewer.
    let target = "/internal/admin/audit?user=ada";
    let a = sign("ana", "GET", target);
    let (status, body) = srv
        .send(Method::GET, target, Some(WEB_TOKEN), Some(&a))
        .await;
    assert_eq!(status, 401, "{body}");
    // Signed for another query.
    let a = sign("ada", "GET", "/internal/admin/audit?user=ada&limit=5");
    let (status, _) = srv
        .send(Method::GET, target, Some(WEB_TOKEN), Some(&a))
        .await;
    assert_eq!(status, 401);
    srv.stop().await;
}

async fn search_pages_filters_and_hides_content(srv: &Server) {
    seed(&srv.f).await;
    let to = "to=2026-11-15T00:00:00Z";
    // Pages of two, newest first, until `next` is null.
    let mut seen = Vec::new();
    let mut query = format!("{to}&limit=2");
    loop {
        let page = srv.search("aud", &query).await;
        let events = page["events"].as_array().unwrap();
        assert!(events.len() <= 2);
        for e in events {
            seen.push(e["action"].as_str().unwrap().to_string());
        }
        match page["next"].as_str() {
            Some(next) => query = format!("{to}&limit=2&before={next}"),
            None => break,
        }
    }
    assert_eq!(
        seen,
        [
            "confluence.read",
            "jira.create_issue",
            "slack.post_reply",
            "jira.create_issue",
            "jira.search",
            "confluence.create_draft",
        ]
    );
    // Filters.
    let page = srv.search("aud", &format!("{to}&actor=ana")).await;
    assert_eq!(page["events"].as_array().unwrap().len(), 3);
    let page = srv.search("aud", &format!("{to}&outcome=denied")).await;
    assert_eq!(page["events"][0]["actor"], "ben");
    assert_eq!(page["events"].as_array().unwrap().len(), 1);
    let page = srv
        .search("aud", &format!("{to}&action=jira.create_issue&actor=ana"))
        .await;
    assert_eq!(page["events"].as_array().unwrap().len(), 2);
    let page = srv
        .search("aud", "from=2026-11-14T08:00:01Z&to=2026-11-14T08:00:03Z")
        .await;
    let outcomes: Vec<_> = page["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["outcome"].as_str().unwrap())
        .collect();
    assert_eq!(
        outcomes,
        ["denied", "pending"],
        "from inclusive, to exclusive"
    );
    // Metadata only.
    let page = srv.search("aud", &format!("{to}&actor=cy")).await;
    let e = &page["events"][0];
    assert_eq!(e["action"], "confluence.create_draft");
    assert_eq!(e["system"], "confluence");
    assert_eq!(e["target"], "apr_secret");
    assert_eq!(e["outcome"], "rejected");
    assert_eq!(e["sources"], 2);
    assert_eq!(e["titled_sources"], 1);
    assert_eq!(e["detail"], Value::Null);
    assert_eq!(e["client"], "mcp");
    assert_eq!(e["task_id"], "tsk_1");
    let text = page.to_string();
    for absent in [
        "SECRET",
        "PAY-212",
        "C0PAY",
        "hash-abc",
        "args_hash",
        "source_titles",
    ] {
        assert!(!text.contains(absent), "{absent} in {text}");
    }
    // Bad parameters.
    for bad in [
        "limit=0",
        "limit=1001",
        "outcome=maybe",
        "before=not-a-cursor",
        "from=yesterday",
        "from=2026-11-15T00:00:00Z&to=2026-11-14T00:00:00Z",
        "purpose=dump",
    ] {
        let (status, body) = srv
            .web("aud", &format!("/internal/admin/audit?user=aud&{bad}"))
            .await;
        assert_eq!(status, 400, "{bad}: {body}");
        assert_eq!(body["code"], "bad_request");
    }
}

#[tokio::test]
async fn search_in_memory() {
    let srv = start(Mode::Require, false).await.unwrap();
    search_pages_filters_and_hides_content(&srv).await;
    srv.stop().await;
}

/// The same in Postgres, where the admin reads are recorded with `client = 'web'`.
#[tokio::test]
async fn search_in_postgres() {
    let Some(srv) = start(Mode::Require, true).await else {
        return;
    };
    search_pages_filters_and_hides_content(&srv).await;
    let page = srv.search("ada", "actor=aud&action=admin.audit").await;
    let first = &page["events"][0];
    assert_eq!(first["client"], "web", "{page}");
    assert_eq!(first["outcome"], "ok");
    srv.stop().await;
}

#[tokio::test]
async fn accounts_connectors_and_overview() {
    let srv = start(Mode::Require, false).await.unwrap();
    seed(&srv.f).await;
    let (status, body) = srv.web("aud", "/internal/admin/accounts?user=aud").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["linking"], false);
    let ana = body["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["handle"] == "ana")
        .unwrap();
    assert_eq!(
        ana["accounts"],
        serde_json::json!({"atlassian": false, "slack": false, "github": false})
    );

    let (status, body) = srv.web("ada", "/internal/admin/connectors?user=ada").await;
    assert_eq!(status, 200, "{body}");
    let names: Vec<_> = body["connectors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["confluence", "docs", "jira", "slack"]);
    assert_eq!(body["connectors"][0]["mode"], "fake");
    assert_eq!(body["connectors"][0]["health"], "ok");
    assert_eq!(body["connectors"][0]["error_class"], Value::Null);

    let (status, body) = srv.web("aud", "/internal/admin/overview?user=aud").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["pending_drafts"], 0);
    // The seeded events are from 2026-11-13/14; "since" is 7 days before now.
    let since: DateTime<Utc> = body["since"].as_str().unwrap().parse().unwrap();
    assert!((Utc::now() - Duration::days(7) - since).num_seconds().abs() < 60);
    assert!(body["denied"].is_u64());
    srv.stop().await;
}
