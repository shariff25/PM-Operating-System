//! Bot sessions (`docs/bot-runtime-spec.md` §2): a session for a named bot gets its role's
//! tools and data class, `never` writes are refused, a team bot reads only inside its read
//! scope, writes still run as the PM, and the bot token may decide approval items.
//!
//! The fixture (`common::content_tree`) has the team bots `docs`, `scout` (reads Jira `PM`,
//! Confluence `ENG`, Slack `C0FIELD`) and `blind` (reads nothing), and Ana's personal bot
//! `pm-ana` (`slack.send_message: never`). The fake connectors apply the read scope the way
//! the live ones do; `tests/live_connectors.rs` checks the live ones on the wire.

mod common;

use std::sync::Arc;

use chrono::Duration;
use common::{fixture, now};
use pmos_toolserver::app::BotAccess;
use pmos_toolserver::approvals::{Decision, Status};
use pmos_toolserver::audit::{Client, Outcome};
use pmos_toolserver::service::{DecisionError, SessionError, ToolError};
use pmos_toolserver::session::Session;
use pmos_toolserver::{router, AppConfig};
use rmcp::model::{CallToolRequestParams, ClientConfig};
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::ServiceExt;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

const SERVICE_TOKEN: &str = "test-service-token-0123456789abcdef";
const BOT_TOKEN: &str = "test-bot-token-0123456789abcdef-0123";

async fn bot_session(f: &common::Fixture, user: &str, bot: &str) -> Session {
    f.svc
        .create_bot_session(user, bot, "tsk_1", None, now())
        .await
        .unwrap()
}

fn tool_names(f: &common::Fixture, s: &Session) -> Vec<String> {
    f.svc
        .list_tools(s)
        .iter()
        .map(|t| t.harness_name.clone())
        .collect()
}

fn last_event(f: &common::Fixture) -> pmos_toolserver::audit::AuditEvent {
    f.audit.events().last().cloned().unwrap()
}

// ----- sessions ---------------------------------------------------------------------------

#[tokio::test]
async fn a_bot_session_gets_its_roles_tools_less_never() {
    let f = fixture();
    let docs = bot_session(&f, "ana", "docs").await;
    assert_eq!(docs.skill, None);
    assert_eq!(docs.bot.as_deref(), Some("docs"));
    assert_eq!(tool_names(&f, &docs), vec!["docs_read", "docs_search"]);

    // The role's `slack.post_reply: never` keeps it off the list.
    let scout = bot_session(&f, "ana", "scout").await;
    assert_eq!(
        tool_names(&f, &scout),
        vec![
            "confluence_read",
            "confluence_search",
            "context_read",
            "context_search",
            "jira_create_issue",
            "jira_read",
            "jira_search",
            "slack_read_thread",
            "slack_search",
        ]
    );

    // pm-assistant's skills, less the bot's own `slack.send_message: never`.
    let mine = bot_session(&f, "ana", "pm-ana").await;
    assert_eq!(
        tool_names(&f, &mine),
        vec![
            "bets_list",
            "bets_read",
            "bets_record",
            "confluence_create_draft",
            "confluence_read",
            "confluence_search",
            "context_read",
            "context_search",
            "docs_read",
            "docs_search",
            "jira_create_issue",
            "jira_read",
            "jira_rollup",
            "jira_search",
            "slack_post_reply",
            "slack_read_thread",
            "slack_search",
            "stats_cashflow",
            "stats_compare",
            "stats_sample_size",
        ]
    );
}

#[tokio::test]
async fn a_personal_bot_serves_only_its_owner() {
    let f = fixture();
    assert!(f
        .svc
        .create_bot_session("ana", "pm-ana", "tsk_1", None, now())
        .await
        .is_ok());
    assert_eq!(
        f.svc
            .create_bot_session("bo", "pm-ana", "tsk_1", None, now())
            .await,
        Err(SessionError::NotOwner {
            bot: "pm-ana".into(),
            owner: "ana".into()
        })
    );
    // A team bot serves anyone (team.yaml isn't enforced with fake connectors).
    assert!(f
        .svc
        .create_bot_session("bo", "scout", "tsk_1", None, now())
        .await
        .is_ok());
}

#[tokio::test]
async fn a_team_bot_serves_team_members_only_when_the_team_is_enforced() {
    let f = common::team_fixture(&["ana", "bo"]);
    for user in ["ana", "bo"] {
        assert!(f
            .svc
            .create_bot_session(user, "scout", "tsk_1", None, now())
            .await
            .is_ok());
    }
    assert_eq!(
        f.svc
            .create_bot_session("eve", "scout", "tsk_1", None, now())
            .await,
        Err(SessionError::NotAMember("eve".into()))
    );
    assert_eq!(
        f.svc
            .create_bot_session("eve", "pm-ana", "tsk_1", None, now())
            .await,
        Err(SessionError::NotAMember("eve".into()))
    );
}

#[tokio::test]
async fn bot_sessions_validate_their_inputs() {
    let f = fixture();
    assert_eq!(
        f.svc
            .create_bot_session("ana", "nobody", "tsk_1", None, now())
            .await,
        Err(SessionError::UnknownBot("nobody".into()))
    );
    assert_eq!(
        f.svc
            .create_bot_session("ana", "scout", "", None, now())
            .await,
        Err(SessionError::InvalidRun)
    );
    // A bot's service identity is never a PM: no session, bot or skill, runs as it.
    for user in ["bots.scout", "Ana"] {
        assert_eq!(
            f.svc
                .create_bot_session(user, "scout", "tsk_1", None, now())
                .await,
            Err(SessionError::InvalidUser(user.into()))
        );
    }
    assert_eq!(
        f.svc
            .create_session("bots.scout", "ask", "r1", None, now())
            .await,
        Err(SessionError::InvalidUser("bots.scout".into()))
    );
    let s = f
        .svc
        .create_bot_session("ana", "scout", "tsk_1", Some(Duration::minutes(5)), now())
        .await
        .unwrap();
    assert!(f
        .svc
        .session(&s.token, now() + Duration::minutes(4))
        .await
        .unwrap()
        .is_some());
    assert!(f
        .svc
        .session(&s.token, now() + Duration::minutes(5))
        .await
        .unwrap()
        .is_none());
}

// ----- the allowlist, data class and `never` ----------------------------------------------

#[tokio::test]
async fn tools_outside_the_role_are_denied_and_audited() {
    let f = fixture();
    let s = bot_session(&f, "ana", "scout").await;
    let err = f
        .svc
        .call(
            &s,
            "confluence_create_draft",
            json!({"space": "ENG", "title": "x", "body": "y"}),
            now(),
        )
        .await
        .unwrap_err();
    assert_eq!(err, ToolError::NotInRole("confluence_create_draft".into()));
    assert_eq!(
        err.to_string(),
        "tool `confluence_create_draft` is not one of this bot's tools"
    );
    let e = last_event(&f);
    assert_eq!(e.outcome, Outcome::Denied);
    assert_eq!(e.bot.as_deref(), Some("scout"));
    assert_eq!(e.skill, None);
    assert!(f.confluence.writes().is_empty());
}

#[tokio::test]
async fn a_never_write_is_refused_audited_and_never_queued() {
    let f = fixture();
    let s = bot_session(&f, "ana", "pm-ana").await;
    let err = f
        .svc
        .call(
            &s,
            "slack_send_message",
            json!({"channel": "C0FIELD", "text": "hi"}),
            now(),
        )
        .await
        .unwrap_err();
    assert_eq!(
        err,
        ToolError::Forbidden(
            "the pm-ana bot may never use slack.send_message: its setting for it is `never`".into()
        )
    );
    let e = last_event(&f);
    assert_eq!(
        (e.tool.as_str(), e.outcome),
        ("slack.send_message", Outcome::Denied)
    );
    assert_eq!(e.bot.as_deref(), Some("pm-ana"));
    assert!(f.svc.approvals("ana", None).await.unwrap().is_empty());
    assert!(f.slack.writes().is_empty());

    // A role-level `never` works the same, and a harness grant can't get around either.
    let scout = bot_session(&f, "ana", "scout").await;
    let args = json!({"channel": "C0FIELD", "thread_ts": "1731500000.000100", "text": "hi"});
    assert!(matches!(
        f.svc
            .call(&scout, "slack_post_reply", args.clone(), now())
            .await,
        Err(ToolError::Forbidden(_))
    ));
    assert!(matches!(
        f.svc.grant(&scout, "slack_post_reply", &args, now()).await,
        Err(ToolError::Forbidden(_))
    ));
    assert!(f.slack.writes().is_empty());
}

#[tokio::test]
async fn a_bot_reads_nothing_above_its_roles_data_class() {
    let f = fixture();
    // field-scout is internal; pm-assistant is restricted.
    let scout = bot_session(&f, "ana", "scout").await;
    let err = f
        .svc
        .call(
            &scout,
            "context_read",
            json!({"path": "context/partners/globex-nda.md"}),
            now(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Forbidden(ref m) if m.contains("at most internal")));
    assert_eq!(last_event(&f).outcome, Outcome::Denied);
    let found = f
        .svc
        .call(&scout, "context_search", json!({"query": "roadmap"}), now())
        .await
        .unwrap();
    assert!(!found.to_string().contains("globex-nda"));

    let mine = bot_session(&f, "ana", "pm-ana").await;
    let doc = f
        .svc
        .call(
            &mine,
            "context_read",
            json!({"path": "context/partners/globex-nda.md"}),
            now(),
        )
        .await
        .unwrap();
    assert_eq!(doc["sensitivity"], "restricted");
}

// ----- writes ------------------------------------------------------------------------------

#[tokio::test]
async fn a_team_bots_write_waits_for_approval_and_runs_as_the_pm() {
    let f = fixture();
    let s = bot_session(&f, "ana", "scout").await;
    let args = json!({"project": "PM", "issue_type": "Task", "summary": "Follow up"});
    let out = f
        .svc
        .call(&s, "jira_create_issue", args.clone(), now())
        .await
        .unwrap();
    assert_eq!(out["status"], "pending_approval");
    let id = out["approval_id"].as_str().unwrap().to_string();
    let item = f.svc.approval(&id).await.unwrap().unwrap();
    assert_eq!(item.bot.as_deref(), Some("scout"));
    assert_eq!(item.skill, None);
    assert_eq!(item.user, "ana");
    assert!(f.jira.writes().is_empty());

    let done = f
        .svc
        .decide(&id, "ana", Decision::Approve, None, now())
        .await
        .unwrap();
    assert_eq!(done.status, Status::Approved);
    let writes = f.jira.writes();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].user, "ana", "as the PM, never as bots.scout");

    // A harness grant runs it once, also as the PM.
    f.svc
        .grant(&s, "jira_create_issue", &args, now())
        .await
        .unwrap();
    let out = f
        .svc
        .call(&s, "jira_create_issue", args, now())
        .await
        .unwrap();
    assert_eq!(out["status"], "done");
    assert_eq!(f.jira.writes()[1].user, "ana");
}

// ----- read scope --------------------------------------------------------------------------

#[tokio::test]
async fn a_team_bot_reads_only_inside_its_scope() {
    let f = fixture();
    let s = bot_session(&f, "ana", "scout").await;
    let call = |tool: &'static str, args: Value| f.svc.call(&s, tool, args, now());

    // Jira: PM is listed.
    let hits = call("jira_search", json!({"jql": "sso"})).await.unwrap();
    assert_eq!(hits["results"][0]["source"]["id"], "PM-101");
    assert!(call("jira_read", json!({"key": "PM-101"})).await.is_ok());
    let err = call("jira_read", json!({"key": "OPS-7"}))
        .await
        .unwrap_err();
    assert!(
        matches!(err, ToolError::Forbidden(ref m) if m.contains("outside the scout bot's read scope (PM)")),
        "{err:?}"
    );
    assert_eq!(last_event(&f).outcome, Outcome::Denied);

    // Confluence: only ENG, and every fixture page is in PM.
    let hits = call("confluence_search", json!({"cql": "sso"}))
        .await
        .unwrap();
    assert_eq!(hits["results"], json!([]));
    let err = call("confluence_read", json!({"page_id": "9001"}))
        .await
        .unwrap_err();
    assert!(
        matches!(err, ToolError::Forbidden(ref m) if m.contains("page 9001")),
        "{err:?}"
    );
    assert_eq!(last_event(&f).outcome, Outcome::Denied);

    // Slack: C0FIELD only.
    let thread = json!({"channel": "C0FIELD", "ts": "1731500000.000100"});
    assert!(call("slack_read_thread", thread).await.is_ok());
    let err = call(
        "slack_read_thread",
        json!({"channel": "C0OTHER", "ts": "1731500000.000100"}),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ToolError::Forbidden(_)), "{err:?}");
    let e = last_event(&f);
    assert_eq!(
        (e.tool.as_str(), e.outcome),
        ("slack.read_thread", Outcome::Denied)
    );
    assert_eq!(e.bot.as_deref(), Some("scout"));
}

#[tokio::test]
async fn an_empty_read_scope_means_no_access() {
    let f = fixture();
    let s = bot_session(&f, "ana", "blind").await;
    for (tool, args) in [
        ("jira_search", json!({"jql": "sso"})),
        ("confluence_search", json!({"cql": "sso"})),
        ("slack_search", json!({"query": "sso"})),
    ] {
        let err = f.svc.call(&s, tool, args, now()).await.unwrap_err();
        assert!(
            matches!(err, ToolError::Forbidden(ref m) if m.contains("lists no")),
            "{tool}: {err:?}"
        );
        assert_eq!(last_event(&f).outcome, Outcome::Denied);
    }
}

#[tokio::test]
async fn a_personal_bot_reads_as_its_owner_without_a_scope() {
    let f = fixture();
    let s = bot_session(&f, "ana", "pm-ana").await;
    assert!(f
        .svc
        .call(&s, "confluence_read", json!({"page_id": "9001"}), now())
        .await
        .is_ok());
    assert!(f
        .svc
        .call(&s, "jira_read", json!({"key": "PM-103"}), now())
        .await
        .is_ok());
}

// ----- content changes ---------------------------------------------------------------------

#[tokio::test]
async fn reindex_reloads_bots_and_settings_for_live_sessions() {
    let f = fixture();
    let s = bot_session(&f, "ana", "pm-ana").await;
    let args = json!({"project": "PM", "issue_type": "Task", "summary": "x"});
    let out = f
        .svc
        .call(&s, "jira_create_issue", args.clone(), now())
        .await
        .unwrap();
    let id = out["approval_id"].as_str().unwrap().to_string();

    // A merge sets `jira.create_issue: never` for pm-ana.
    common::write_content(
        &f.root,
        "bots/pm-ana/BOT.md",
        "---\nname: pm-ana\nrole: pm-assistant\nscope: personal\nowner: \"@ana\"\ndescription: Ana's assistant.\nactions:\n  slack.send_message: never\n  jira.create_issue: never\n---\n\n# Ana's assistant\n\nHelp Ana.\n",
    );
    f.svc.reindex(&f.root, now()).await.unwrap();
    assert!(!tool_names(&f, &s).contains(&"jira_create_issue".to_string()));
    assert!(matches!(
        f.svc.call(&s, "jira_create_issue", args, now()).await,
        Err(ToolError::Forbidden(_))
    ));
    // The item queued before the change can't run now; it stays pending.
    assert_eq!(
        f.svc
            .decide(&id, "ana", Decision::Approve, None, now())
            .await
            .unwrap_err(),
        DecisionError::BotNotAllowed("jira.create_issue".into())
    );
    assert_eq!(
        f.svc.approval(&id).await.unwrap().unwrap().status,
        Status::Pending
    );
    assert!(f.jira.writes().is_empty());

    // A bot removed from the content takes its sessions with it.
    std::fs::remove_dir_all(f.root.join("bots/pm-ana")).unwrap();
    f.svc.reindex(&f.root, now()).await.unwrap();
    assert!(f.svc.session(&s.token, now()).await.unwrap().is_none());
    assert_eq!(
        f.svc
            .create_bot_session("ana", "pm-ana", "tsk_2", None, now())
            .await,
        Err(SessionError::UnknownBot("pm-ana".into()))
    );
}

// ----- decisions ---------------------------------------------------------------------------

#[tokio::test]
async fn deciders_must_be_in_team_yaml_when_it_is_enforced() {
    let f = common::team_fixture(&["ana"]);
    let s = bot_session(&f, "ana", "scout").await;
    let out = f
        .svc
        .call(
            &s,
            "jira_create_issue",
            json!({"project": "PM", "issue_type": "Task", "summary": "x"}),
            now(),
        )
        .await
        .unwrap();
    let id = out["approval_id"].as_str().unwrap().to_string();

    for who in ["eve", "bots.scout"] {
        assert_eq!(
            f.svc
                .decide_as(Client::Bot, &id, who, Decision::Approve, None, now())
                .await
                .unwrap_err(),
            DecisionError::NotAMember(who.into())
        );
        let e = last_event(&f);
        assert_eq!((e.outcome, e.client), (Outcome::Denied, Client::Bot));
        assert_eq!(e.user, who);
    }
    assert!(f.jira.writes().is_empty());

    let done = f
        .svc
        .decide_as(Client::Bot, &id, "ana", Decision::Approve, None, now())
        .await
        .unwrap();
    assert_eq!(done.status, Status::Approved);
    let e = last_event(&f);
    assert_eq!((e.outcome, e.client), (Outcome::Approved, Client::Bot));
    assert_eq!(e.bot.as_deref(), Some("scout"));
}

// ----- over HTTP and MCP -------------------------------------------------------------------

struct Server {
    base: String,
    cancel: CancellationToken,
    fixture: common::Fixture,
}

async fn start(fixture: common::Fixture) -> Server {
    let cancel = CancellationToken::new();
    let app = router(
        Arc::clone(&fixture.svc),
        AppConfig {
            service_token: SERVICE_TOKEN.into(),
            bot: Some(BotAccess {
                token: BOT_TOKEN.into(),
                skills: vec!["docs".into()],
            }),
            content_root: fixture.root.clone(),
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
    Server {
        base,
        cancel,
        fixture,
    }
}

async fn post(base: &str, token: &str, path: &str, body: Value) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{base}{path}"))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn bot_sessions_over_http() {
    let s = start(fixture()).await;
    let new = |token: &'static str, body: Value| post(&s.base, token, "/internal/sessions", body);

    let res = new(
        SERVICE_TOKEN,
        json!({"user": "ana", "bot": "scout", "run_id": "tsk_1", "ttl_minutes": 30}),
    )
    .await;
    assert_eq!(res.status(), 201);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["session"]["bot"], "scout");
    assert_eq!(body["session"]["skill"], Value::Null);
    let tools: Vec<&str> = body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(tools.contains(&"slack_search"));
    assert!(!tools.contains(&"slack_post_reply"), "never is not listed");
    let token = body["token"].as_str().unwrap().to_string();

    // One of `skill` and `bot`, exactly.
    for both_or_neither in [
        json!({"user": "ana", "bot": "scout", "skill": "ask", "run_id": "r"}),
        json!({"user": "ana", "run_id": "r"}),
    ] {
        assert_eq!(new(SERVICE_TOKEN, both_or_neither).await.status(), 400);
    }
    let res = new(
        SERVICE_TOKEN,
        json!({"user": "ana", "bot": "nobody", "run_id": "r"}),
    )
    .await;
    assert_eq!(res.status(), 404);
    let res = new(
        SERVICE_TOKEN,
        json!({"user": "bo", "bot": "pm-ana", "run_id": "r"}),
    )
    .await;
    assert_eq!(res.status(), 403);
    let body: Value = res.json().await.unwrap();
    assert_eq!(
        body["error"],
        "`pm-ana` is ana's personal bot; only ana can use it"
    );
    // Bot sessions need the service token.
    let res = new(
        BOT_TOKEN,
        json!({"user": "ana", "bot": "docs", "run_id": "r"}),
    )
    .await;
    assert_eq!(res.status(), 403);

    // Calls: `never` and out-of-scope are 403, audited as denied; the bot token can't see
    // a bot session at all.
    let call = |bearer: &'static str, tool: &'static str, args: Value| {
        let token = token.clone();
        let base = s.base.clone();
        async move {
            reqwest::Client::new()
                .post(format!("{base}/internal/call"))
                .bearer_auth(bearer)
                .header("x-pmos-session", token)
                .json(&json!({"tool": tool, "args": args}))
                .send()
                .await
                .unwrap()
        }
    };
    let res = call(
        SERVICE_TOKEN,
        "slack_post_reply",
        json!({"channel": "C0FIELD", "thread_ts": "1731500000.000100", "text": "hi"}),
    )
    .await;
    assert_eq!(res.status(), 403);
    let res = call(SERVICE_TOKEN, "jira_read", json!({"key": "OPS-1"})).await;
    assert_eq!(res.status(), 403);
    let res = call(SERVICE_TOKEN, "jira_read", json!({"key": "PM-101"})).await;
    assert_eq!(res.status(), 200);
    let res = call(BOT_TOKEN, "jira_read", json!({"key": "PM-101"})).await;
    assert_eq!(res.status(), 404);
    let outcomes: Vec<_> = s
        .fixture
        .audit
        .events()
        .into_iter()
        .map(|e| (e.tool, e.outcome))
        .collect();
    assert_eq!(
        outcomes,
        vec![
            ("slack.post_reply".into(), Outcome::Denied),
            ("jira.read".into(), Outcome::Denied),
            ("jira.read".into(), Outcome::Ok),
        ]
    );
    s.cancel.cancel();
}

#[tokio::test]
async fn mcp_lists_and_calls_only_what_the_bot_may() {
    let s = start(fixture()).await;
    let res = post(
        &s.base,
        SERVICE_TOKEN,
        "/internal/sessions",
        json!({"user": "ana", "bot": "pm-ana", "run_id": "tsk_mcp"}),
    )
    .await;
    let token = res.json::<Value>().await.unwrap()["token"]
        .as_str()
        .unwrap()
        .to_string();
    let transport = StreamableHttpClientTransport::from_config(
        StreamableHttpClientTransportConfig::with_uri(format!("{}/mcp", s.base)).auth_header(token),
    );
    let client = ClientConfig::default().serve(transport).await.unwrap();
    let tools = client.list_tools(Default::default()).await.unwrap();
    let names: Vec<String> = tools.tools.iter().map(|t| t.name.to_string()).collect();
    assert!(names.contains(&"jira_create_issue".to_string()));
    assert!(!names.contains(&"slack_send_message".to_string()), "never");
    // The slack-reply skill's tools are in the role; the reply itself still needs approval.
    assert!(names.contains(&"slack_post_reply".to_string()));
    assert!(
        !names.contains(&"memory_update".to_string()),
        "not in the role"
    );

    let never = client
        .call_tool(
            CallToolRequestParams::new("slack_send_message").with_arguments(
                json!({"channel": "C0FIELD", "text": "hi"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_eq!(never.is_error, Some(true));
    let text = format!("{:?}", never.content);
    assert!(text.contains("never"), "{text}");
    let e = s.fixture.audit.events().last().cloned().unwrap();
    assert_eq!(
        (e.tool.as_str(), e.outcome, e.client),
        ("slack.send_message", Outcome::Denied, Client::Mcp)
    );
    assert!(s.fixture.slack.writes().is_empty());
    let _ = client.cancel().await;
    s.cancel.cancel();
}

#[tokio::test]
async fn the_bot_token_reads_and_decides_one_item() {
    let s = start(common::team_fixture(&["ana", "bo"])).await;
    let session = s
        .fixture
        .svc
        .create_bot_session("ana", "scout", "tsk_1", None, now())
        .await
        .unwrap();
    let pending = |summary: &'static str| {
        let svc = s.fixture.svc.clone();
        let session = session.clone();
        async move {
            let out = svc
                .call(
                    &session,
                    "jira_create_issue",
                    json!({"project": "PM", "issue_type": "Task", "summary": summary}),
                    now(),
                )
                .await
                .unwrap();
            out["approval_id"].as_str().unwrap().to_string()
        }
    };
    let id = pending("one").await;
    let http = reqwest::Client::new();

    let res = http
        .get(format!("{}/internal/approvals/{id}", s.base))
        .bearer_auth(BOT_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let item: Value = res.json().await.unwrap();
    assert_eq!(
        (item["bot"].as_str(), item["status"].as_str()),
        (Some("scout"), Some("pending"))
    );

    let decide = |id: String, who: &'static str, decision: &'static str| {
        let base = s.base.clone();
        async move {
            post(
                &base,
                BOT_TOKEN,
                &format!("/internal/approvals/{id}/decision"),
                json!({"decided_by": who, "decision": decision}),
            )
            .await
        }
    };
    // Someone outside team.yaml, and a member who isn't the item's PM, can't decide it.
    assert_eq!(decide(id.clone(), "eve", "approve").await.status(), 403);
    assert_eq!(decide(id.clone(), "bo", "approve").await.status(), 403);
    assert!(s.fixture.jira.writes().is_empty());

    let res = decide(id.clone(), "ana", "approve").await;
    assert_eq!(res.status(), 200);
    let item: Value = res.json().await.unwrap();
    assert_eq!(item["status"], "approved");
    assert_eq!(item["decided_by"], "ana");
    assert_eq!(s.fixture.jira.writes()[0].user, "ana");
    let e = s.fixture.audit.events().last().cloned().unwrap();
    assert_eq!((e.outcome, e.client), (Outcome::Approved, Client::Bot));
    assert_eq!(decide(id, "ana", "reject").await.status(), 409);

    let rejected = pending("two").await;
    let res = decide(rejected, "ana", "reject").await;
    assert_eq!(res.status(), 200);
    let e = s.fixture.audit.events().last().cloned().unwrap();
    assert_eq!((e.outcome, e.client), (Outcome::Rejected, Client::Bot));

    // It may list a member's pending items (the App Home; `tests/slack_reply.rs`): none are
    // left. Everything else about approvals stays orchestrator-only.
    let res = http
        .get(format!("{}/internal/approvals?user=ana", s.base))
        .bearer_auth(BOT_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let listed: Value = res.json().await.unwrap();
    assert_eq!(listed, json!({"approvals": [], "total": 0}));
    let res = post(
        &s.base,
        BOT_TOKEN,
        "/internal/grants",
        json!({"session_token": session.token, "tool": "jira_create_issue", "args": {}}),
    )
    .await;
    assert_eq!(res.status(), 403);
    // The service token still decides too.
    let third = pending("three").await;
    let res = post(
        &s.base,
        SERVICE_TOKEN,
        &format!("/internal/approvals/{third}/decision"),
        json!({"decided_by": "ana", "decision": "reject"}),
    )
    .await;
    assert_eq!(res.status(), 200);
    assert_eq!(
        s.fixture.audit.events().last().unwrap().client,
        Client::Orchestrator
    );
    s.cancel.cancel();
}
