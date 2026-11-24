//! Slice M8, Confluence (`docs/mcp-servers-spec.md` §6): Confluence behind the switch, on its
//! approved record, served by the fake MCP server from the same fixtures as the in-process fake.
//! The same Confluence calls run through the whole tool service on the built-in fake and on the
//! MCP backend and must answer and audit the same; every write passes the approval gate with
//! the exact arguments, and what is sent is a pure function of them: Confluence storage,
//! HTML-escaped by PM OS, with the "Drafted with PM OS" footer; a page update names the
//! version PM OS read, checked before anything is sent. No model, no network beyond loopback.

mod common;

use std::sync::Arc;

use async_trait::async_trait;
use common::{fixture, memory_users, now, Fixture};
use pmos_contract::mcp::ServerRecord;
use pmos_toolserver::approvals::Decision;
use pmos_toolserver::catalog::Catalog;
use pmos_toolserver::connectors::atlassian::storage_body;
use pmos_toolserver::connectors::fake::FakeConnector;
use pmos_toolserver::connectors::{Caller, Connector, ConnectorError, PAGE_CHANGED};
use pmos_toolserver::mcp_client::fake::{self, FakeMcp, FAKE_HOST};
use pmos_toolserver::mcp_client::transport::TestRoutes;
use pmos_toolserver::mcp_client::{McpConnector, McpServer, ServerAuth, UserTokens};
use pmos_toolserver::vault::Secret;
use pmos_toolserver::ToolService;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

const TOKEN: &str = "fake-mcp-token-for-confluence-0123456789ab";

/// `fake-confluence` as an Admin of the fixture approved it.
fn approved(addr: std::net::SocketAddr) -> ServerRecord {
    let mut record = fake::fake_records(addr)
        .into_iter()
        .find(|r| r.id == "fake-confluence")
        .unwrap();
    record.approved_by = common::FIXTURE_ADMIN.0.into();
    for t in &mut record.tools {
        t.approved_by = common::FIXTURE_ADMIN.0.into();
    }
    record
}

/// The fixture with Confluence served by `content/mcp/fake-confluence.yaml`.
async fn on_mcp() -> (Fixture, FakeMcp, CancellationToken) {
    let cancel = CancellationToken::new();
    let fake = FakeMcp::new();
    let addr = fake::serve(
        fake.clone(),
        vec![FAKE_HOST.into()],
        Some(TOKEN.into()),
        cancel.clone(),
    )
    .await
    .unwrap();
    let mut f = fixture();
    common::write_content(
        &f.root,
        "mcp/fake-confluence.yaml",
        &serde_norway::to_string(&approved(addr)).unwrap(),
    );
    // The content lint accepts it, and the tool server loads it.
    let report = pmos_contract::lint(
        &f.root,
        &pmos_contract::LintOptions::new(now().date_naive()),
    );
    assert!(
        !report.has_errors(),
        "{:?}",
        report.errors().collect::<Vec<_>>()
    );
    let registry = pmos_contract::ToolRegistry::from_yaml(
        &std::fs::read_to_string(f.root.join("tools.yaml")).unwrap(),
    )
    .unwrap();
    let records = pmos_contract::mcp::load(&f.root, &registry).unwrap();
    let users = memory_users(vec![], &f.stores).await;
    let dir = users.snapshot(now()).await.unwrap();
    pmos_toolserver::mcp_client::check_approvals(&records, &dir).unwrap();
    // Served as `connectors_for` serves it, in the fake-mcp mode: the fake server answers a
    // write with the in-process fake's own id (`CONFLUENCE-W1`), which only that mode accepts.
    let served = pmos_toolserver::mcp_client::connectors_for(
        &records,
        &["confluence".to_string()],
        |k| (k == "PMOS_MCP_TOKEN_FAKE").then(|| TOKEN.to_string()),
        None,
        Some(f.stores.audit.clone()),
        &TestRoutes::default(),
    )
    .unwrap();
    assert!(served.reads_only.is_empty());
    let server = Arc::new(
        McpServer::new(
            records[0].clone(),
            ServerAuth::Token(Secret::new(TOKEN)),
            Some(f.stores.audit.clone()),
            &TestRoutes::default(),
        )
        .unwrap(),
    );
    let confluence: Arc<dyn Connector> =
        Arc::new(McpConnector::new("confluence", server, "fake-mcp").unwrap());
    let connectors: Vec<Arc<dyn Connector>> = vec![
        f.jira.clone(),
        confluence,
        f.slack.clone(),
        Arc::new(FakeConnector::docs()),
    ];
    let catalog = Catalog::load(&f.root, now().date_naive()).unwrap();
    f.svc = Arc::new(
        ToolService::new(catalog, f.stores.clone(), connectors)
            .unwrap()
            .with_open_directory(),
    );
    (f, fake, cancel)
}

fn shape(f: &Fixture, tool: &str) -> Vec<(String, pmos_toolserver::audit::Outcome, Vec<String>)> {
    f.audit
        .events()
        .into_iter()
        .filter(|e| e.tool == tool)
        .map(|e| (e.tool, e.outcome, e.sources))
        .collect()
}

async fn reads(f: &Fixture) -> Vec<Value> {
    let mut out = Vec::new();
    let s = f
        .svc
        .create_session("ana", "field-watch", "r1", None, now())
        .await
        .unwrap();
    let b = f
        .svc
        .create_bot_session("ana", "scout", "tsk_1", None, now())
        .await
        .unwrap();
    let mut calls = Vec::new();
    for cql in ["sso", "PRD", "customer call", "nothing-matches"] {
        calls.push((&s, "confluence_search", json!({"cql": cql})));
        calls.push((&s, "confluence_search", json!({"cql": cql, "limit": 1})));
        calls.push((&b, "confluence_search", json!({"cql": cql})));
    }
    for page in ["9001", "9002", "1234"] {
        calls.push((&s, "confluence_read", json!({"page_id": page})));
        calls.push((&b, "confluence_read", json!({"page_id": page})));
    }
    for (session, tool, args) in calls {
        out.push(match f.svc.call(session, tool, args, now()).await {
            Ok(v) => v,
            Err(e) => json!({"error_kind": format!("{e:?}").split('(').next().unwrap()}),
        });
    }
    out
}

#[tokio::test]
async fn confluence_on_its_record_answers_and_audits_as_the_built_in_connector() {
    let builtin = fixture();
    let (mcp, fake, _cancel) = on_mcp().await;
    let (a, b) = (reads(&mcp).await, reads(&builtin).await);
    assert_eq!(a.len(), 18);
    assert_eq!(a, b);
    for tool in ["confluence.search", "confluence.read"] {
        assert_eq!(shape(&mcp, tool), shape(&builtin, tool), "{tool}");
    }
    assert!(fake
        .calls()
        .iter()
        .any(|c| c == "tools/call:confluence_get_page"));
    assert_eq!(mcp.svc.served_by("confluence.read"), "fake-confluence");
    assert_eq!(
        mcp.svc.served_by("confluence.update_page"),
        "fake-confluence"
    );
    assert_eq!(builtin.svc.served_by("confluence.read"), "fake");
    // The audit names the server and the approved digest; never a body.
    let e = mcp
        .audit
        .events()
        .into_iter()
        .find(|e| e.tool == "confluence.read")
        .unwrap();
    let detail = e.detail.unwrap();
    assert!(detail.contains("via mcp server fake-confluence, tool sha256:"));
    assert!(!detail.contains("Two customers"));
}

#[tokio::test]
async fn a_draft_goes_through_the_gate_and_is_sent_as_escaped_storage_with_its_footer() {
    let (f, fake, _cancel) = on_mcp().await;
    let s = f
        .svc
        .create_session("ana", "page-update", "r1", None, now())
        .await
        .unwrap();
    let args = json!({"space": "PM", "title": "Launch notes", "body": "# Plan\n<script>alert(1)</script> & co"});
    // Without a grant it waits in the inbox: nothing is sent.
    let res = f
        .svc
        .call(&s, "confluence_create_draft", args.clone(), now())
        .await
        .unwrap();
    assert_eq!(res["status"], "pending_approval");
    assert!(fake.confluence_writes().is_empty());
    assert!(!fake.calls().iter().any(|c| c.contains("create_page")));
    let id = res["approval_id"].as_str().unwrap().to_string();
    // An edit changes what it says, never where it goes; the edited text is what is sent.
    let edited = json!({"space": "PM", "title": "Launch notes", "body": "Edited <b>body</b>"});
    let done = f
        .svc
        .decide(&id, "ana", Decision::Edit, Some(edited.clone()), now())
        .await
        .unwrap();
    assert_eq!(done.result.as_ref().unwrap()["id"], "CONFLUENCE-W1");
    let sent = fake
        .received()
        .into_iter()
        .find(|(t, _)| t == "confluence_create_page")
        .unwrap()
        .1;
    assert_eq!(
        sent,
        json!({"space_key": "PM", "title": "Launch notes", "body": storage_body("Edited <b>body</b>", "r1")})
    );
    let body = sent["body"].as_str().unwrap();
    assert!(body.contains("Edited &lt;b&gt;body&lt;/b&gt;"), "{body}");
    assert!(body.ends_with("<p><em>Drafted with PM OS · run r1</em></p>"));
    // What left PM OS is kept as a hash, on the item and in the audit's `started` event.
    let hash = pmos_toolserver::approvals::args_hash(&sent);
    assert_eq!(done.upstream_args_hash.as_deref(), Some(hash.as_str()));
    let started = f
        .audit
        .events()
        .into_iter()
        .find(|e| {
            e.tool == "confluence.create_draft"
                && e.outcome == pmos_toolserver::audit::Outcome::Started
        })
        .unwrap();
    assert!(started
        .detail
        .unwrap()
        .starts_with(&format!("approved in the inbox; writing; upstream args sha256:{hash} (via mcp server fake-confluence")));
    // A grant for exactly these arguments runs once, as the PM.
    f.svc
        .grant(&s, "confluence_create_draft", &args, now())
        .await
        .unwrap();
    let done = f
        .svc
        .call(&s, "confluence_create_draft", args.clone(), now())
        .await
        .unwrap();
    assert_eq!(done["status"], "done");
    assert_eq!(done["id"], "CONFLUENCE-W2");
    let sent = fake.received().last().unwrap().1.clone();
    assert!(sent["body"]
        .as_str()
        .unwrap()
        .contains("&lt;script&gt;alert(1)&lt;/script&gt; &amp; co"));
    // The audit and the ledger as for a connector write.
    let events: Vec<_> = f
        .audit
        .events()
        .into_iter()
        .filter(|e| e.tool == "confluence.create_draft")
        .collect();
    assert!(events.iter().all(|e| e
        .detail
        .as_deref()
        .unwrap_or_default()
        .contains("fake-confluence")));
    assert!(events.iter().any(|e| e
        .output_ref
        .as_deref()
        .unwrap_or_default()
        .contains("CONFLUENCE-W2")));
    let rows = f
        .svc
        .ledger(pmos_toolserver::ledger::LedgerQuery::all(now()), now())
        .await
        .unwrap();
    let row = rows.iter().find(|r| r.id == id).unwrap();
    assert_eq!(row.tool, "confluence.create_draft");
    assert_eq!(row.served_by.as_deref(), Some("fake-confluence"));
}

#[tokio::test]
async fn postgres_keeps_served_by_and_the_upstream_hash() {
    let Some((f, pg)) = common::pg_fixture().await else {
        return;
    };
    let s = f
        .svc
        .create_session("ana", "page-update", "r1", None, now())
        .await
        .unwrap();
    let res = f
        .svc
        .call(
            &s,
            "confluence_create_draft",
            json!({"space": "PM", "title": "T", "body": "B"}),
            now(),
        )
        .await
        .unwrap();
    let id = res["approval_id"].as_str().unwrap().to_string();
    let rows = f
        .stores
        .approvals
        .ledger(&pmos_toolserver::ledger::LedgerQuery::all(now()))
        .await
        .unwrap();
    assert_eq!(
        rows.iter()
            .find(|r| r.id == id)
            .unwrap()
            .served_by
            .as_deref(),
        Some("fake")
    );
    let hash = "ab".repeat(32);
    f.stores.approvals.note_upstream(&id, &hash).await.unwrap();
    let item = f.stores.approvals.get(&id).await.unwrap().unwrap();
    assert_eq!(item.upstream_args_hash.as_deref(), Some(hash.as_str()));
    // Only a hash is accepted.
    assert!(f
        .stores
        .approvals
        .note_upstream(&id, "not a hash")
        .await
        .is_err());
    drop(f);
    pg.drop_schema().await;
}

#[tokio::test]
async fn the_built_in_route_keeps_no_upstream_hash_and_the_ledger_says_fake() {
    let f = fixture();
    let s = f
        .svc
        .create_session("ana", "page-update", "r1", None, now())
        .await
        .unwrap();
    let res = f
        .svc
        .call(
            &s,
            "confluence_create_draft",
            json!({"space": "PM", "title": "T", "body": "B"}),
            now(),
        )
        .await
        .unwrap();
    let id = res["approval_id"].as_str().unwrap().to_string();
    let done = f
        .svc
        .decide(&id, "ana", Decision::Approve, None, now())
        .await
        .unwrap();
    assert_eq!(done.upstream_args_hash, None);
    assert!(serde_json::to_value(&done)
        .unwrap()
        .get("upstream_args_hash")
        .is_none());
    let started = f
        .audit
        .events()
        .into_iter()
        .find(|e| e.outcome == pmos_toolserver::audit::Outcome::Started)
        .unwrap();
    assert_eq!(
        started.detail.as_deref(),
        Some("approved in the inbox; writing")
    );
    let rows = f
        .svc
        .ledger(pmos_toolserver::ledger::LedgerQuery::all(now()), now())
        .await
        .unwrap();
    assert_eq!(
        rows.iter()
            .find(|r| r.id == id)
            .unwrap()
            .served_by
            .as_deref(),
        Some("fake")
    );
}

#[tokio::test]
async fn an_update_names_the_version_it_read_checked_before_anything_is_sent() {
    let (f, fake, _cancel) = on_mcp().await;
    let s = f
        .svc
        .create_session("ana", "page-update", "r1", None, now())
        .await
        .unwrap();
    let read = f
        .svc
        .call(&s, "confluence_read", json!({"page_id": "9002"}), now())
        .await
        .unwrap();
    assert_eq!(read["results"][0]["content"]["version"], 1);
    let update = |v: u64| json!({"page_id": "9002", "version": v, "body": "New body"});
    // A version it didn't read: refused, and the update tool is never called.
    f.svc
        .grant(&s, "confluence_update_page", &update(2), now())
        .await
        .unwrap();
    let stale = f
        .svc
        .call(&s, "confluence_update_page", update(2), now())
        .await;
    assert!(format!("{stale:?}").contains(PAGE_CHANGED), "{stale:?}");
    assert!(!fake
        .calls()
        .iter()
        .any(|c| c == "tools/call:confluence_update_page"));
    // The version read: updated once, to version 2.
    f.svc
        .grant(&s, "confluence_update_page", &update(1), now())
        .await
        .unwrap();
    let done = f
        .svc
        .call(&s, "confluence_update_page", update(1), now())
        .await
        .unwrap();
    assert_eq!(done["status"], "done");
    let read = f
        .svc
        .call(&s, "confluence_read", json!({"page_id": "9002"}), now())
        .await
        .unwrap();
    assert_eq!(read["results"][0]["content"]["version"], 2);
    // Now stale.
    f.svc
        .grant(&s, "confluence_update_page", &update(1), now())
        .await
        .unwrap();
    let again = f
        .svc
        .call(&s, "confluence_update_page", update(1), now())
        .await;
    assert!(format!("{again:?}").contains(PAGE_CHANGED), "{again:?}");
    assert_eq!(fake.confluence_writes().len(), 1);
}

#[tokio::test]
async fn a_changed_confluence_tool_is_off_at_once_and_nothing_is_sent() {
    let (f, fake, _cancel) = on_mcp().await;
    let s = f
        .svc
        .create_session("ana", "page-update", "r1", None, now())
        .await
        .unwrap();
    // A first call connects and lists.
    f.svc
        .call(&s, "confluence_read", json!({"page_id": "9001"}), now())
        .await
        .unwrap();
    fake.tricks.change("confluence_create_page");
    // A tools/list_changed (or the 10-minute re-list) finds it; here, the next connection, on
    // a fresh connector for the same record.
    let args = json!({"space": "PM", "title": "T", "body": "B"});
    let addr_record = f.root.join("mcp/fake-confluence.yaml");
    let record = pmos_contract::mcp::parse(&std::fs::read_to_string(addr_record).unwrap()).unwrap();
    let mcp = connector(record, ServerAuth::Token(Secret::new(TOKEN)));
    let before = fake.calls().len();
    let e = mcp
        .write(Caller::new("ana", "r1"), "create_draft", &args)
        .await
        .unwrap_err();
    assert!(
        matches!(&e, ConnectorError::Forbidden(m) if m.starts_with("tool_changed: fake-confluence.confluence_create_page")),
        "{e:?}"
    );
    assert!(!fake.calls()[before..]
        .iter()
        .any(|c| c.starts_with("tools/call")));
    // Reads on unchanged tools still work.
    assert_eq!(
        mcp.read(
            Caller::new("ana", "r1"),
            "read",
            &json!({"page_id": "9001"})
        )
        .await
        .unwrap()
        .len(),
        1
    );
}

fn connector(record: ServerRecord, auth: ServerAuth) -> McpConnector {
    connector_via(record, auth, &TestRoutes::default())
}

fn connector_via(record: ServerRecord, auth: ServerAuth, routes: &TestRoutes) -> McpConnector {
    let server = Arc::new(McpServer::new(record, auth, None, routes).unwrap());
    McpConnector::new("confluence", server, "fake-mcp").unwrap()
}

#[tokio::test]
async fn an_unclassified_tool_is_a_write_and_never_serves_a_read() {
    let cancel = CancellationToken::new();
    let fake = FakeMcp::new();
    let addr = fake::serve(
        fake.clone(),
        vec![FAKE_HOST.into()],
        Some(TOKEN.into()),
        cancel.clone(),
    )
    .await
    .unwrap();
    let mut record = approved(addr);
    for t in &mut record.tools {
        if t.registry == "confluence.search" {
            t.class = None;
        }
    }
    let registry = pmos_contract::ToolRegistry::from_yaml(
        &std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../content/tools.yaml"
        ))
        .unwrap(),
    )
    .unwrap();
    assert!(
        pmos_contract::mcp::validate(&record, "fake-confluence", &registry)
            .iter()
            .any(|p| p.code == "mcp-class")
    );
    let mcp = connector(record, ServerAuth::Token(Secret::new(TOKEN)));
    assert!(matches!(
        mcp.read(Caller::new("ana", "r1"), "search", &json!({"cql": "sso"}))
            .await,
        Err(ConnectorError::Unsupported(_))
    ));
    assert!(!fake
        .calls()
        .iter()
        .any(|c| c == "tools/call:confluence_search"));
    // A write without the storage adapter is never served: no escaping, no footer.
    let mut bare = approved(addr);
    for t in &mut bare.tools {
        t.adapter = None;
    }
    let server = Arc::new(
        McpServer::new(
            bare,
            ServerAuth::Token(Secret::new(TOKEN)),
            None,
            &TestRoutes::default(),
        )
        .unwrap(),
    );
    assert!(McpConnector::new("confluence", server, "fake-mcp").is_err());
}

/// Per-PM tokens: each PM's own, never another's, never PM OS's.
struct Tokens(Vec<(&'static str, &'static str)>);

#[async_trait]
impl UserTokens for Tokens {
    async fn token(&self, user: &str, _server: &str) -> Result<Secret, ConnectorError> {
        self.0
            .iter()
            .find(|(u, _)| *u == user)
            .map(|(_, t)| Secret::new(*t))
            .ok_or_else(|| ConnectorError::NotConnected("connect Confluence in PM OS first".into()))
    }
}

#[tokio::test]
async fn writes_act_with_the_pms_own_token_and_a_team_bot_never_writes_as_itself() {
    const HOST: &str = "confluence-mcp.example.test";
    let ana = "ana-token-0123456789abcdef0123456789";
    let bot = "bot-token-0123456789abcdef0123456789";
    let cancel = CancellationToken::new();
    let fake = FakeMcp::new()
        .with_account(ana, "ana@example.com")
        .with_account(bot, "scout-bot@example.com");
    let addr = fake::serve(fake.clone(), vec![HOST.into()], None, cancel.clone())
        .await
        .unwrap();
    let mut record = approved(addr);
    record.url = format!("https://{HOST}/mcp");
    record.identity = pmos_contract::mcp::Identity {
        host: Some(HOST.into()),
        ..Default::default()
    };
    record.auth = pmos_contract::mcp::AuthKind::OauthPerUser;
    record.secret_env = None;
    let routes = TestRoutes::default().route(HOST, addr);
    let tokens = Arc::new(Tokens(vec![("ana", ana), ("bots.scout", bot)]));
    let mcp = connector_via(record, ServerAuth::PerUser(tokens), &routes);
    let args = json!({"space": "PM", "title": "T", "body": "B"});
    // Ben never linked: "connect first", and nothing is sent.
    let e = mcp
        .write(Caller::new("ben", "r1"), "create_draft", &args)
        .await
        .unwrap_err();
    assert!(matches!(e, ConnectorError::NotConnected(_)), "{e:?}");
    assert!(fake.received().is_empty());
    // A team bot's session writes as the PM, with the PM's token.
    let reads = pmos_contract::bots::Reads {
        confluence_spaces: vec!["PM".into()],
        ..Default::default()
    };
    let caller = Caller::new("ana", "r1").for_team_bot(pmos_toolserver::connectors::TeamBot {
        name: "scout",
        identity: "bots.scout",
        reads: &reads,
    });
    mcp.write(caller, "create_draft", &args).await.unwrap();
    // Its reads use its own read account.
    mcp.read(caller, "read", &json!({"page_id": "9001"}))
        .await
        .unwrap();
    let callers = fake.callers();
    assert_eq!(
        callers[0],
        (
            "confluence_create_page".to_string(),
            Some("ana@example.com".to_string())
        )
    );
    assert_eq!(
        callers[1],
        (
            "confluence_get_page".to_string(),
            Some("scout-bot@example.com".to_string())
        )
    );
    assert_eq!(fake.confluence_writes()[0].user, "ana@example.com");
    // No PM OS token or session id in what was sent.
    let sent = serde_json::to_string(&fake.received()).unwrap();
    assert!(!sent.contains(ana) && !sent.contains(bot) && !sent.contains("tsk_"));
}

#[tokio::test]
async fn fake_data_mode_refuses_a_confluence_server_that_isnt_a_fake() {
    let record = approved("127.0.0.1:9".parse().unwrap());
    let refused = pmos_toolserver::mcp_client::fake_data_refusals(std::slice::from_ref(&record));
    assert_eq!(refused.len(), 1);
    assert!(refused[0].contains("mcp/fake-confluence.yaml must not be loaded"));
    let mut vendor = record;
    vendor.operator = pmos_contract::mcp::Operator::SystemVendor;
    assert!(pmos_toolserver::mcp_client::real_data(
        &[vendor],
        &["confluence".to_string()]
    ));
}

#[tokio::test]
async fn results_are_untrusted_and_capped() {
    let (f, _fake, _cancel) = on_mcp().await;
    let s = f
        .svc
        .create_session("ana", "field-watch", "r1", None, now())
        .await
        .unwrap();
    let out = f
        .svc
        .call(&s, "confluence_search", json!({"cql": "sso"}), now())
        .await
        .unwrap();
    let first = &out["results"][0];
    assert_eq!(first["untrusted"], true);
    assert_eq!(first["source"]["system"], "confluence");
    assert!(first["ref"].as_str().unwrap().starts_with("src-"));
}
