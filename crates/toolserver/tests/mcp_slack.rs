//! Slice M8, Slack (`docs/mcp-servers-spec.md` §2.6, §6): Slack on its approved record, the
//! guarantees of M1 to M7 for it, and the split. The audience-check, thread and 24-hour cases
//! run over MCP in `slack_reply.rs` (modules `mcp` and `mcp_reads_only`); the parity of reads,
//! posts and audiences is in `mcp_parity.rs`; the comparison in `mcp_compare.rs`. No model, no
//! network beyond loopback.

mod common;

use std::sync::Arc;

use async_trait::async_trait;
use common::{fixture, memory_users, now};
use pmos_contract::mcp::{CheckRole, ServerRecord, ToolClass};
use pmos_toolserver::connectors::fake::FakeConnector;
use pmos_toolserver::connectors::{Caller, Connector, ConnectorError};
use pmos_toolserver::mcp_client::fake::{self, FakeMcp, FAKE_HOST};
use pmos_toolserver::mcp_client::transport::TestRoutes;
use pmos_toolserver::mcp_client::{McpConnector, McpServer, ServerAuth, UserTokens};
use pmos_toolserver::vault::Secret;
use serde_json::json;
use tokio_util::sync::CancellationToken;

const TOKEN: &str = "fake-mcp-token-for-slack-0123456789abcdef";

/// `fake-slack` as the fixture's Admin approved it: every tool and both checks.
fn approved(addr: std::net::SocketAddr) -> ServerRecord {
    let mut record = fake::fake_records(addr)
        .into_iter()
        .find(|r| r.id == "fake-slack")
        .unwrap();
    record.approved_by = common::FIXTURE_ADMIN.0.into();
    for t in &mut record.tools {
        t.approved_by = common::FIXTURE_ADMIN.0.into();
    }
    for c in &mut record.checks {
        c.approved_by = common::FIXTURE_ADMIN.0.into();
    }
    record
}

async fn serve(fake: FakeMcp) -> (std::net::SocketAddr, CancellationToken) {
    let cancel = CancellationToken::new();
    let addr = fake::serve(
        fake,
        vec![FAKE_HOST.into()],
        Some(TOKEN.into()),
        cancel.clone(),
    )
    .await
    .unwrap();
    (addr, cancel)
}

fn connector(record: ServerRecord) -> McpConnector {
    let server = Arc::new(
        McpServer::new(
            record,
            ServerAuth::Token(Secret::new(TOKEN)),
            None,
            &TestRoutes::default(),
        )
        .unwrap(),
    );
    McpConnector::new("slack", server, "fake-mcp").unwrap()
}

#[tokio::test]
async fn the_record_lints_loads_and_needs_an_admin_for_every_check() {
    let (addr, _cancel) = serve(FakeMcp::new()).await;
    let f = fixture();
    common::write_content(
        &f.root,
        "mcp/fake-slack.yaml",
        &serde_norway::to_string(&approved(addr)).unwrap(),
    );
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
    assert!(records[0].slack_audience());
    let users = memory_users(vec![], &f.stores).await;
    let dir = users.snapshot(now()).await.unwrap();
    pmos_toolserver::mcp_client::check_approvals(&records, &dir).unwrap();
    // A check approved by someone who isn't an active Admin is refused at start.
    let mut by_nobody = records[0].clone();
    by_nobody.checks[1].approved_by = "ana".into();
    let errors = pmos_toolserver::mcp_client::check_approvals(&[by_nobody], &dir).unwrap_err();
    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains("slack_members") && errors[0].contains("not an active Admin"));
}

#[tokio::test]
async fn an_unclassified_check_is_never_called_and_posts_stay_built_in() {
    let (addr, _cancel) = serve(FakeMcp::new()).await;
    let mut record = approved(addr);
    for c in &mut record.checks {
        if c.role == CheckRole::SlackMembers {
            c.class = None;
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
    let codes: Vec<_> = pmos_contract::mcp::validate(&record, "fake-slack", &registry)
        .into_iter()
        .map(|p| p.code)
        .collect();
    assert!(
        codes.contains(&"mcp-check") && codes.contains(&"mcp-slack-audience"),
        "{codes:?}"
    );
    // Even past the lint, the tool server treats it as missing: no audience check over MCP,
    // so posts are not served here.
    assert!(!record.slack_audience());
    let mcp = connector(record);
    assert!(!mcp.posts_on_mcp());
    let caller = Caller::new("evals", "r1");
    assert!(matches!(
        mcp.audience(caller, "C0PMTEAM").await,
        Err(ConnectorError::Unsupported(_))
    ));
    assert!(matches!(
        mcp.write(
            caller,
            "send_message",
            &json!({"channel": "C0PMTEAM", "text": "Hi"})
        )
        .await,
        Err(ConnectorError::Unsupported(_))
    ));
    assert!(mcp.route("send_message").is_none() && mcp.route("search").is_some());
}

#[tokio::test]
async fn a_changed_post_tool_is_off_at_once_and_nothing_is_sent() {
    let fake = FakeMcp::new();
    let (addr, _cancel) = serve(fake.clone()).await;
    fake.tricks.change("slack_reply");
    let mcp = connector(approved(addr));
    let caller = Caller::new("evals", "r1");
    let before = fake.calls().len();
    let e = mcp
        .write(
            caller,
            "post_reply",
            &json!({"channel": "C0PMTEAM", "thread_ts": "1731600000.000100", "text": "Yes"}),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&e, ConnectorError::Forbidden(m) if m.starts_with("tool_changed: fake-slack.slack_reply")),
        "{e:?}"
    );
    assert!(!fake.calls()[before..]
        .iter()
        .any(|c| c == "tools/call:slack_reply"));
    assert!(fake.slack_writes().is_empty());
    // The unchanged reads still work; the unchanged send still works.
    assert!(!mcp
        .read(caller, "search", &json!({"query": "SAML"}))
        .await
        .unwrap()
        .is_empty());
    mcp.write(
        caller,
        "send_message",
        &json!({"channel": "C0PMTEAM", "text": "Hi"}),
    )
    .await
    .unwrap();
    assert_eq!(fake.slack_writes().len(), 1);
}

#[tokio::test]
async fn a_search_answers_at_most_the_limit_asked_whatever_the_server_sends() {
    let fake = FakeMcp::new();
    let (addr, _cancel) = serve(fake.clone()).await;
    // A record that doesn't send the limit: the server answers its own default.
    let mut record = approved(addr);
    for t in &mut record.tools {
        if t.registry == "slack.search" {
            t.args.pass.retain(|a| a != "limit");
            t.args.drop.push("limit".into());
        }
    }
    let mcp = connector(record);
    let caller = Caller::new("evals", "r1");
    let args = json!({"query": "<@U0EVALS>", "limit": 2});
    let sent = fake.received();
    assert!(sent.is_empty());
    let got = mcp.read(caller, "search", &args).await.unwrap();
    assert_eq!(fake.received()[0].1, json!({"query": "<@U0EVALS>"}));
    let builtin = FakeConnector::slack()
        .read(caller, "search", &args)
        .await
        .unwrap();
    assert_eq!(got.len(), 2);
    assert_eq!(
        got.iter().map(|i| &i.source).collect::<Vec<_>>(),
        builtin.iter().map(|i| &i.source).collect::<Vec<_>>()
    );
}

/// Per-PM tokens: each person's own, never another's, never PM OS's.
struct Tokens(Vec<(&'static str, &'static str)>);

#[async_trait]
impl UserTokens for Tokens {
    async fn token(&self, user: &str, _server: &str) -> Result<Secret, ConnectorError> {
        self.0
            .iter()
            .find(|(u, _)| *u == user)
            .map(|(_, t)| Secret::new(*t))
            .ok_or_else(|| ConnectorError::NotConnected("connect Slack in PM OS first".into()))
    }
}

#[tokio::test]
async fn a_team_bots_post_and_audience_act_as_the_pm_its_reads_as_itself() {
    const HOST: &str = "slack-mcp.example.test";
    let pm = "evals-token-0123456789abcdef01234567";
    let bot = "bot-token-0123456789abcdef0123456789";
    let cancel = CancellationToken::new();
    let fake = FakeMcp::new()
        .with_account(pm, "evals")
        .with_account(bot, "bots.scout");
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
    let server = Arc::new(
        McpServer::new(
            record,
            ServerAuth::PerUser(Arc::new(Tokens(vec![("evals", pm), ("bots.scout", bot)]))),
            None,
            &TestRoutes::default().route(HOST, addr),
        )
        .unwrap(),
    );
    let mcp = McpConnector::new("slack", server, "fake-mcp").unwrap();
    let reads = pmos_contract::bots::Reads {
        slack_channels: vec!["C0PMTEAM".into()],
        ..Default::default()
    };
    let caller = Caller::new("evals", "r1").for_team_bot(pmos_toolserver::connectors::TeamBot {
        name: "scout",
        identity: "bots.scout",
        reads: &reads,
    });
    // Ben never linked: "connect first", and nothing is sent.
    assert!(matches!(
        mcp.audience(Caller::new("ben", "r1"), "C0PMTEAM").await,
        Err(ConnectorError::NotConnected(_))
    ));
    assert!(fake.received().is_empty());
    mcp.audience(caller, "C0PMTEAM").await.unwrap();
    mcp.write(
        caller,
        "post_reply",
        &json!({"channel": "C0PMTEAM", "thread_ts": "1731600000.000100", "text": "Yes"}),
    )
    .await
    .unwrap();
    mcp.read(
        caller,
        "read_thread",
        &json!({"channel": "C0PMTEAM", "ts": "1731600000.000100"}),
    )
    .await
    .unwrap();
    // Outside the bot's channels: refused before anything is sent.
    let before = fake.received().len();
    assert!(mcp
        .read(
            caller,
            "read_thread",
            &json!({"channel": "C0FIELD", "ts": "1731500000.000100"}),
        )
        .await
        .is_err());
    assert_eq!(fake.received().len(), before);
    let callers = fake.callers();
    let by = |tool: &str| {
        callers
            .iter()
            .filter(|(t, _)| t == tool)
            .map(|(_, a)| a.clone().unwrap_or_default())
            .collect::<Vec<_>>()
    };
    assert_eq!(by("slack_conversation_info"), vec!["evals"]);
    assert_eq!(by("slack_conversation_members"), vec!["evals"]);
    assert_eq!(by("slack_reply"), vec!["evals"]);
    assert_eq!(by("slack_get_thread"), vec!["bots.scout"]);
    assert_eq!(fake.slack_writes()[0].user, "evals");
    let sent = serde_json::to_string(&fake.received()).unwrap();
    assert!(!sent.contains(pm) && !sent.contains(bot) && !sent.contains("tsk_"));
}

#[tokio::test]
async fn fake_data_mode_refuses_a_slack_server_that_isnt_a_fake() {
    let record = approved("127.0.0.1:9".parse().unwrap());
    let refused = pmos_toolserver::mcp_client::fake_data_refusals(std::slice::from_ref(&record));
    assert_eq!(refused.len(), 1);
    assert!(refused[0].contains("mcp/fake-slack.yaml must not be loaded"));
    let mut vendor = record;
    vendor.operator = pmos_contract::mcp::Operator::SystemVendor;
    assert!(pmos_toolserver::mcp_client::real_data(
        &[vendor],
        &["slack".to_string()]
    ));
}

#[tokio::test]
async fn a_reads_only_record_keeps_its_unmapped_posts_off_the_server() {
    // A record approving reads only (no post tools, no checks): the split serves posts with
    // the built-in connector, and the server never sees a post or a members call.
    let fake = FakeMcp::new();
    let (addr, _cancel) = serve(fake.clone()).await;
    let mut record = approved(addr);
    record.checks.clear();
    record.tools.retain(|t| t.class() == ToolClass::Read);
    let served = pmos_toolserver::mcp_client::connectors_for(
        &[record],
        &["slack".to_string()],
        |_| Some(TOKEN.to_string()),
        None,
        None,
        &TestRoutes::default(),
    )
    .unwrap();
    let builtin = Arc::new(FakeConnector::slack());
    let slack = served
        .over(&[builtin.clone() as Arc<dyn Connector>])
        .unwrap()
        .remove(0);
    let caller = Caller::new("evals", "r1");
    let a = slack.audience(caller, "C0GLOBEXCX").await.unwrap();
    assert_eq!(a, builtin.audience(caller, "C0GLOBEXCX").await.unwrap());
    slack
        .write(
            caller,
            "post_reply",
            &json!({"channel": "C0PMTEAM", "thread_ts": "1731600000.000100", "text": "Yes"}),
        )
        .await
        .unwrap();
    assert_eq!(builtin.writes().len(), 1);
    assert!(fake.slack_writes().is_empty());
    assert!(!fake.calls().iter().any(|c| c.starts_with("tools/call:")));
    assert!(slack
        .upstream_args(caller, "post_reply", &json!({}))
        .is_none());
}
