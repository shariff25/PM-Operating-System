//! What an approval item is shown with: `GET /internal/approvals/{id}/sources` (what the
//! drafting run read, from the audit log, metadata only) and the item's additive fields
//! (`expires_at`, `target_url`, `external`).

mod common;

use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use common::Fixture;
use pmos_toolserver::app::BotAccess;
use pmos_toolserver::audit::{AuditEvent, Client, JsonlAudit, Outcome};
use pmos_toolserver::catalog::Catalog;
use pmos_toolserver::connectors::fake::{FakeConnector, SLACK_ALL_TEAM, SLACK_CONNECT, SLACK_PM};
use pmos_toolserver::connectors::Connector;
use pmos_toolserver::sources::MAX_SOURCES;
use pmos_toolserver::vault::{Provider, SealedCredential};
use pmos_toolserver::{router, AppConfig, Stores, ToolService};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

const SERVICE_TOKEN: &str = "test-service-token-0123456789abcdef";
const BOT_TOKEN: &str = "test-bot-token-0123456789abcdef-0123";
const SITE: &str = "https://acme.atlassian.net";

struct Server {
    base: String,
    cancel: CancellationToken,
    f: Fixture,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

async fn serve(f: Fixture) -> Server {
    let cancel = CancellationToken::new();
    let app = router(
        Arc::clone(&f.svc),
        AppConfig {
            service_token: SERVICE_TOKEN.into(),
            bot: Some(BotAccess {
                token: BOT_TOKEN.into(),
                skills: vec!["docs".into()],
            }),
            console_token: None,
            content_root: f.root.clone(),
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
    Server { base, cancel, f }
}

async fn start() -> Server {
    let (f, _) = common::reply_fixture(false).await.unwrap();
    serve(f).await
}

/// Link an Atlassian site for `user`: only the site fields matter here; the "token" is a
/// placeholder no test unseals.
async fn link_site(f: &Fixture, user: &str) {
    f.stores
        .credentials
        .put(&SealedCredential {
            user: user.into(),
            provider: Provider::Atlassian,
            key_id: "k1".into(),
            access_token: "sealed-placeholder".into(),
            refresh_token: None,
            expires_at: None,
            scopes: "read:jira-work".into(),
            cloud_id: Some("cloud-1".into()),
            site_url: Some(format!("{SITE}/")),
            external_id: None,
            updated_at: Utc::now(),
        })
        .await
        .unwrap();
}

impl Server {
    async fn get(&self, path: &str, token: Option<&str>) -> (u16, Value, String) {
        let mut req = reqwest::Client::new().get(format!("{}{path}", self.base));
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        let res = req.send().await.unwrap();
        let status = res.status().as_u16();
        let text = res.text().await.unwrap();
        (
            status,
            serde_json::from_str(&text).unwrap_or(Value::Null),
            text,
        )
    }

    async fn decide(&self, id: &str, body: Value) -> (u16, Value) {
        let res = reqwest::Client::new()
            .post(format!("{}/internal/approvals/{id}/decision", self.base))
            .bearer_auth(SERVICE_TOKEN)
            .json(&body)
            .send()
            .await
            .unwrap();
        let status = res.status().as_u16();
        (status, res.json().await.unwrap_or(Value::Null))
    }
}

async fn call(
    f: &Fixture,
    s: &pmos_toolserver::session::Session,
    tool: &str,
    args: Value,
    at: DateTime<Utc>,
) -> Value {
    f.svc.call(s, tool, args, at).await.unwrap()
}

#[tokio::test]
async fn lists_what_the_run_read_newest_first_once_each_without_text() {
    let srv = start().await;
    let f = &srv.f;
    link_site(f, SLACK_PM).await;
    let t0 = Utc::now() - Duration::minutes(30);
    let s = f
        .svc
        .create_session(
            SLACK_PM,
            "reply-test",
            "tsk_src1",
            Some(Duration::hours(2)),
            t0,
        )
        .await
        .unwrap();
    call(
        f,
        &s,
        "context_read",
        json!({"path": "context/partners/globex-nda.md"}),
        t0,
    )
    .await;
    call(
        f,
        &s,
        "jira_read",
        json!({"key": "PM-101"}),
        t0 + Duration::seconds(1),
    )
    .await;
    call(
        f,
        &s,
        "slack_read_thread",
        json!({"channel": "C0FIELD", "ts": "1731500000.000100"}),
        t0 + Duration::seconds(2),
    )
    .await;
    // The same issue again: listed once, at its latest read.
    call(
        f,
        &s,
        "jira_read",
        json!({"key": "PM-101"}),
        t0 + Duration::seconds(3),
    )
    .await;
    call(
        f,
        &s,
        "docs_search",
        json!({"query": "vault"}),
        t0 + Duration::seconds(4),
    )
    .await;
    // A read that found nothing, and stats (no sources), change nothing.
    call(
        f,
        &s,
        "jira_search",
        json!({"jql": "zzzz-nothing"}),
        t0 + Duration::seconds(5),
    )
    .await;
    let drafted = call(
        f,
        &s,
        "slack_post_reply",
        json!({"channel": SLACK_ALL_TEAM, "thread_ts": "1731600000.000100", "text": "Yes: SSO ships in 4.2."}),
        t0 + Duration::seconds(6),
    )
    .await;
    let id = drafted["approval_id"].as_str().unwrap().to_string();
    // After the draft, and in another run: not what this draft was built from.
    call(
        f,
        &s,
        "jira_read",
        json!({"key": "PM-102"}),
        t0 + Duration::seconds(7),
    )
    .await;
    let other = f
        .svc
        .create_session(
            SLACK_PM,
            "reply-test",
            "tsk_src2",
            Some(Duration::hours(2)),
            t0,
        )
        .await
        .unwrap();
    call(
        f,
        &other,
        "jira_read",
        json!({"key": "PM-103"}),
        t0 + Duration::seconds(1),
    )
    .await;

    let (status, body, text) = srv
        .get(
            &format!("/internal/approvals/{id}/sources?user={SLACK_PM}"),
            Some(SERVICE_TOKEN),
        )
        .await;
    assert_eq!(status, 200, "{text}");
    assert_eq!(body["available"], true);
    assert_eq!(body["truncated"], false);
    let sources = body["sources"].as_array().unwrap();
    let refs: Vec<(&str, &str)> = sources
        .iter()
        .map(|s| (s["system"].as_str().unwrap(), s["ref"].as_str().unwrap()))
        .collect();
    assert_eq!(
        refs,
        [
            (
                "docs",
                "https://docs.example.com/docs/vault-key-rotation.md"
            ),
            ("docs", "https://docs.example.com/docs/vault-saml-sso.md"),
            ("jira", "PM-101"),
            ("slack", "C0FIELD:1731500000.000100"),
            ("context", "context/partners/globex-nda.md"),
        ]
    );
    // Titles recorded at the read: the issue's summary after its key, the docs page's title.
    assert_eq!(
        sources[2],
        json!({"system": "jira", "title": "PM-101: Epic: SSO for the Acme Vault console",
               "url": format!("{SITE}/browse/PM-101"),
               "ref": "PM-101", "tool": "jira.read",
               "read_at": serde_json::to_value(t0 + Duration::seconds(3)).unwrap()})
    );
    // Slack stays channel and message ids.
    assert_eq!(sources[3]["title"], "Slack thread in C0FIELD");
    assert_eq!(
        sources[3]["url"],
        "https://slack.com/archives/C0FIELD/p1731500000000100"
    );
    assert_eq!(sources[1]["title"], "Configure SAML SSO for Acme Vault");
    assert_eq!(
        sources[1]["url"],
        "https://docs.example.com/docs/vault-saml-sso.md"
    );
    assert_eq!(sources[4]["url"], Value::Null, "a context file has no link");
    assert!(!sources[4]["title"].as_str().unwrap().is_empty());

    // Metadata only: titles, but no message text, issue description, page snippet or token.
    for text_seen in [
        "Does Acme Vault support SAML SSO",
        "Customers ask for SAML SSO",
        "Key rotation creates",
        "single sign-on",
        "SSO ships in 4.2",
        &s.token,
        "sealed-placeholder",
        "snippet",
        "excerpt",
    ] {
        assert!(!text.contains(text_seen), "{text_seen} in {text}");
    }

    // What the audit log recorded: a title per Jira, docs and context item, cleaned and
    // capped, and nothing at all for the Slack thread (its text may be kept only 24 hours).
    let reads: Vec<AuditEvent> = f
        .audit
        .events()
        .into_iter()
        .filter(|e| e.run_id == "tsk_src1" && e.outcome == Outcome::Ok)
        .collect();
    let titled = |tool: &str| {
        reads
            .iter()
            .find(|e| e.tool == tool)
            .unwrap()
            .source_titles
            .clone()
    };
    assert_eq!(
        titled("jira.read").get("jira:PM-101").map(String::as_str),
        Some("Epic: SSO for the Acme Vault console")
    );
    assert!(!titled("context.read").is_empty());
    assert_eq!(
        titled("docs.search").len(),
        2,
        "{:?}",
        titled("docs.search")
    );
    assert!(
        titled("slack.read_thread").is_empty(),
        "no Slack title is recorded"
    );
    for e in &reads {
        for t in e.source_titles.values() {
            assert!(t.chars().count() <= pmos_toolserver::audit::MAX_SOURCE_TITLE);
            assert!(!t.chars().any(char::is_control));
        }
        assert!(e.source_titles.keys().all(|k| e.sources.contains(k)));
        assert!(!e.source_titles.keys().any(|k| k.starts_with("slack:")));
    }
}

#[tokio::test]
async fn only_the_items_own_user_and_the_service_token_may_read_its_sources() {
    let srv = start().await;
    let f = &srv.f;
    let at = Utc::now() - Duration::minutes(5);
    let s = f
        .svc
        .create_session(
            SLACK_PM,
            "reply-test",
            "tsk_src3",
            Some(Duration::hours(2)),
            at,
        )
        .await
        .unwrap();
    call(f, &s, "jira_read", json!({"key": "PM-101"}), at).await;
    let drafted = call(
        f,
        &s,
        "slack_post_reply",
        json!({"channel": SLACK_ALL_TEAM, "thread_ts": "1731600000.000200", "text": "x"}),
        at + Duration::seconds(1),
    )
    .await;
    let id = drafted["approval_id"].as_str().unwrap();
    let path = |user: &str| format!("/internal/approvals/{id}/sources?user={user}");

    // Someone else's item reads as missing.
    let (status, body, _) = srv.get(&path("ben"), Some(SERVICE_TOKEN)).await;
    assert_eq!(status, 404, "{body}");
    assert_eq!(body["error"], "no such approval item");
    let (status, _, _) = srv
        .get(
            &format!("/internal/approvals/apr_missing/sources?user={SLACK_PM}"),
            Some(SERVICE_TOKEN),
        )
        .await;
    assert_eq!(status, 404);
    let (status, _, _) = srv
        .get(
            &format!("/internal/approvals/{id}/sources"),
            Some(SERVICE_TOKEN),
        )
        .await;
    assert_eq!(status, 400, "`user` is required");
    // Not the Slack bot's, and not without a token.
    assert_eq!(srv.get(&path(SLACK_PM), Some(BOT_TOKEN)).await.0, 403);
    assert_eq!(srv.get(&path(SLACK_PM), None).await.0, 401);
    assert_eq!(srv.get(&path(SLACK_PM), Some("wrong")).await.0, 401);
    // The owner, with the service token. No site linked: the Jira source has no link.
    let (status, body, _) = srv.get(&path(SLACK_PM), Some(SERVICE_TOKEN)).await;
    assert_eq!(status, 200);
    assert_eq!(body["sources"][0]["ref"], "PM-101");
    assert_eq!(body["sources"][0]["url"], Value::Null);
}

#[tokio::test]
async fn caps_the_list_and_skips_what_it_cannot_identify() {
    let srv = start().await;
    let f = &srv.f;
    let at = Utc::now() - Duration::minutes(10);
    let s = f
        .svc
        .create_session(
            SLACK_PM,
            "reply-test",
            "tsk_src4",
            Some(Duration::hours(2)),
            at,
        )
        .await
        .unwrap();
    let event = |n: i64, tool: &str, sources: Vec<String>, outcome| AuditEvent {
        at: at + Duration::seconds(n),
        client: Client::Mcp,
        run_id: "tsk_src4".into(),
        user: SLACK_PM.into(),
        skill: Some("reply-test".into()),
        bot: None,
        tool: tool.into(),
        outcome,
        sources,
        source_titles: Default::default(),
        args_hash: None,
        output_ref: None,
        detail: None,
    };
    for n in 0..30 {
        f.stores
            .audit
            .record(event(
                n,
                "jira.read",
                vec![format!("jira:PM-{}", 200 + n)],
                Outcome::Ok,
            ))
            .await
            .unwrap();
    }
    for (n, tool, source, outcome) in [
        // Shapes it can't identify, a refused call and a write: none are sources.
        (31, "jira.read", "jira:<script>", Outcome::Ok),
        (32, "slack.search", "slack:not a ref", Outcome::Ok),
        (33, "jira.read", "jira:PM-999", Outcome::Denied),
        (34, "jira.create_issue", "jira:PM-998", Outcome::Ok),
        (35, "context.read", "context/../../etc/passwd", Outcome::Ok),
    ] {
        f.stores
            .audit
            .record(event(n, tool, vec![source.into()], outcome))
            .await
            .unwrap();
    }
    let drafted = call(
        f,
        &s,
        "slack_post_reply",
        json!({"channel": SLACK_ALL_TEAM, "thread_ts": "1731600000.000300", "text": "x"}),
        at + Duration::seconds(40),
    )
    .await;
    let item = f
        .svc
        .approval(drafted["approval_id"].as_str().unwrap())
        .await
        .unwrap()
        .unwrap();
    let report = f.svc.approval_sources(&item).await.unwrap();
    assert!(report.available);
    assert!(report.truncated);
    assert_eq!(report.sources.len(), MAX_SOURCES);
    let refs: Vec<&str> = report
        .sources
        .iter()
        .map(|s| s.reference.as_str())
        .collect();
    assert_eq!(refs[0], "PM-229", "newest first");
    assert_eq!(
        report.sources[0].title, "PM-229",
        "an event with no recorded title (as before titles were kept) reads as its id"
    );
    assert_eq!(refs[MAX_SOURCES - 1], "PM-210");
    assert!(!refs
        .iter()
        .any(|r| r.contains("999") || r.contains('<') || r.contains("..")));
}

#[tokio::test]
async fn says_so_when_the_audit_log_cannot_be_read_back() {
    let (f, _) = common::reply_fixture(false).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    // The same content and stores, but audited only to a JSON-lines file.
    let mut stores: Stores = f.stores.clone();
    stores.audit = Arc::new(JsonlAudit::open(&dir.path().join("audit.jsonl")).unwrap());
    let catalog = Catalog::load(&f.root, common::now().date_naive()).unwrap();
    let connectors: Vec<Arc<dyn Connector>> = vec![
        Arc::new(FakeConnector::jira()),
        Arc::new(FakeConnector::confluence()),
        Arc::new(FakeConnector::slack()),
        Arc::new(FakeConnector::docs()),
    ];
    let svc = ToolService::new(catalog, stores, connectors).unwrap();
    let at = common::now();
    let s = svc
        .create_session(SLACK_PM, "reply-test", "tsk_src5", None, at)
        .await
        .unwrap();
    svc.call(&s, "jira_read", json!({"key": "PM-101"}), at)
        .await
        .unwrap();
    let drafted = svc
        .call(
            &s,
            "slack_post_reply",
            json!({"channel": SLACK_ALL_TEAM, "thread_ts": "1731600000.000400", "text": "x"}),
            at,
        )
        .await
        .unwrap();
    let item = svc
        .approval(drafted["approval_id"].as_str().unwrap())
        .await
        .unwrap()
        .unwrap();
    let report = svc.approval_sources(&item).await.unwrap();
    assert!(!report.available);
    assert!(report.sources.is_empty());
}

#[tokio::test]
async fn items_carry_their_expiry_target_link_and_audience() {
    let srv = start().await;
    let f = &srv.f;
    link_site(f, SLACK_PM).await;
    let at = Utc::now() - Duration::minutes(5);
    let s = f
        .svc
        .create_session(
            SLACK_PM,
            "reply-test",
            "tsk_src6",
            Some(Duration::hours(2)),
            at,
        )
        .await
        .unwrap();
    call(f, &s, "jira_read", json!({"key": "PM-101"}), at).await;
    let ext = call(
        f,
        &s,
        "slack_post_reply",
        json!({"channel": SLACK_CONNECT, "thread_ts": "1731601500.000700", "text": "x"}),
        at,
    )
    .await["approval_id"]
        .as_str()
        .unwrap()
        .to_string();
    let issue = call(
        f,
        &s,
        "jira_create_issue",
        json!({"project": "PM", "issue_type": "Story", "summary": "SSO", "epic": "PM-101"}),
        at,
    )
    .await["approval_id"]
        .as_str()
        .unwrap()
        .to_string();

    let (status, item, _) = srv
        .get(&format!("/internal/approvals/{ext}"), Some(SERVICE_TOKEN))
        .await;
    assert_eq!(status, 200);
    let created: DateTime<Utc> = item["created_at"].as_str().unwrap().parse().unwrap();
    let expires: DateTime<Utc> = item["expires_at"].as_str().unwrap().parse().unwrap();
    assert_eq!(expires - created, Duration::hours(24));
    assert_eq!(
        item["target_url"],
        format!("https://slack.com/archives/{SLACK_CONNECT}/p1731601500000700")
    );
    assert!(item.get("external").is_none(), "not checked yet");

    // The audience check runs on approve: the channel is shared outside the company.
    let (status, body) = srv
        .decide(&ext, json!({"decided_by": SLACK_PM, "decision": "approve"}))
        .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (409, Some("needs_external_confirm"))
    );
    let (_, item, _) = srv
        .get(&format!("/internal/approvals/{ext}"), Some(SERVICE_TOKEN))
        .await;
    assert_eq!(item["external"], true);
    assert_eq!(item["status"], "pending");
    // The listings carry the same fields.
    let (_, listed, _) = srv
        .get(
            &format!("/internal/approvals?user={SLACK_PM}"),
            Some(SERVICE_TOKEN),
        )
        .await;
    let listed_ext = listed["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == json!(ext))
        .unwrap();
    assert_eq!(listed_ext["external"], true);
    let (_, home, _) = srv
        .get(
            &format!("/internal/approvals?user={SLACK_PM}"),
            Some(BOT_TOKEN),
        )
        .await;
    let home_ext = home["approvals"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == json!(ext))
        .unwrap();
    assert_eq!(home_ext["expires_at"], item["expires_at"]);

    // A Jira draft links to its epic on the PM's site, and doesn't expire.
    let (_, item, _) = srv
        .get(&format!("/internal/approvals/{issue}"), Some(SERVICE_TOKEN))
        .await;
    assert_eq!(item["target_url"], format!("{SITE}/browse/PM-101"));
    assert!(item.get("expires_at").is_none());
    assert!(item.get("external").is_none());
}
