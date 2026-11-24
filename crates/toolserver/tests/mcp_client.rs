//! The MCP client layer (`docs/mcp-servers-spec.md`, slice M1) against the in-process fake MCP
//! server: the threats of §3 and the requirements of §7 that M1 covers. Nothing here reaches a
//! real server: every server is `pmos-fake-mcp` on a loopback port, reached through records
//! with the production shape (internal, pinned address, service token).

mod common;

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{fixture, now, Fixture};
use pmos_contract::Sensitivity;
use pmos_toolserver::approvals::{Decision, Status};
use pmos_toolserver::audit::Outcome;
use pmos_toolserver::catalog::Catalog;
use pmos_toolserver::connectors::fake::FakeConnector;
use pmos_toolserver::connectors::Connector;
use pmos_toolserver::mcp_client::fake::{self, FakeMcp, FAKE_HOST, INJECTED_DESCRIPTION};
use pmos_toolserver::mcp_client::server::Limits;
use pmos_toolserver::mcp_client::transport::TestRoutes;
use pmos_toolserver::mcp_client::{McpConnector, McpServer, ServerAuth};
use pmos_toolserver::service::ToolError;
use pmos_toolserver::vault::Secret;
use pmos_toolserver::ToolService;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

const TOKEN: &str = "fake-mcp-token-for-tests-0123456789abcdef";

struct Mcp {
    f: Fixture,
    fake: FakeMcp,
    server: Arc<McpServer>,
    _cancel: CancellationToken,
}

async fn start(fake: FakeMcp) -> (std::net::SocketAddr, CancellationToken) {
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

/// The repo's content with Jira served by a fake MCP server; everything else in-process fakes.
async fn mcp_with(limits: Limits, edit: impl FnOnce(&mut pmos_contract::mcp::ServerRecord)) -> Mcp {
    let fake = FakeMcp::new();
    let (addr, cancel) = start(fake.clone()).await;
    let mut record = fake::fake_records(addr).remove(0);
    edit(&mut record);
    let mut f = fixture();
    let server = Arc::new(
        McpServer::new(
            record,
            ServerAuth::Token(Secret::new(TOKEN)),
            Some(f.stores.audit.clone()),
            &TestRoutes::default(),
        )
        .unwrap()
        .with_limits(limits),
    );
    let jira: Arc<dyn Connector> =
        Arc::new(McpConnector::new("jira", server.clone(), "mcp").unwrap());
    let catalog = Catalog::load(&f.root, now().date_naive()).unwrap();
    f.svc = Arc::new(
        ToolService::new(
            catalog,
            f.stores.clone(),
            vec![
                jira,
                f.confluence.clone(),
                f.slack.clone(),
                Arc::new(FakeConnector::docs()),
            ],
        )
        .unwrap()
        .with_open_directory(),
    );
    Mcp {
        f,
        fake,
        server,
        _cancel: cancel,
    }
}

async fn mcp() -> Mcp {
    mcp_with(Limits::default(), |_| {}).await
}

async fn session(m: &Mcp, user: &str) -> pmos_toolserver::session::Session {
    m.f.svc
        .create_session(user, "field-watch", "r1", None, now())
        .await
        .unwrap()
}

fn events(m: &Mcp, tool: &str) -> Vec<pmos_toolserver::audit::AuditEvent> {
    m.f.audit
        .events()
        .into_iter()
        .filter(|e| e.tool == tool)
        .collect()
}

fn calls_to(m: &Mcp, tool: &str) -> usize {
    m.fake
        .calls()
        .iter()
        .filter(|c| *c == &format!("tools/call:{tool}"))
        .count()
}

#[tokio::test]
async fn reads_go_through_the_server_and_are_wrapped_as_untrusted() {
    let m = mcp().await;
    let s = session(&m, "ana").await;
    let out =
        m.f.svc
            .call(&s, "jira_search", json!({"jql": "sso"}), now())
            .await
            .unwrap();
    let results = out["results"].as_array().unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["untrusted"], true);
    assert_eq!(results[0]["source"]["id"], "PM-101");
    assert_eq!(calls_to(&m, "jira_search_issues"), 1);
    // The audit event: as a connector's, plus the server and the approved digest.
    let e = events(&m, "jira.search").pop().unwrap();
    assert_eq!(e.outcome, Outcome::Ok);
    assert_eq!(e.sources, vec!["jira:PM-101"]);
    let detail = e.detail.unwrap();
    assert!(
        detail.starts_with("via mcp server fake-jira, tool sha256:"),
        "{detail}"
    );
    // The connection was audited, with what the server calls itself (shown, never trusted).
    let connect = events(&m, "mcp.connect");
    assert!(connect[0]
        .detail
        .as_deref()
        .unwrap()
        .contains("pmos-fake-mcp"));
}

#[tokio::test]
async fn threat_1_the_servers_descriptions_never_reach_the_model() {
    let m = mcp().await;
    let s = session(&m, "ana").await;
    assert!(m
        .fake
        .tools()
        .iter()
        .any(|t| t.description.as_deref() == Some(INJECTED_DESCRIPTION)));
    let tools = m.f.svc.list_tools(&s);
    let search = tools.iter().find(|t| t.name == "jira.search").unwrap();
    assert_eq!(search.description, "Search issues with JQL");
    for t in &tools {
        assert!(!t.description.contains("IGNORE PREVIOUS"), "{}", t.name);
        assert!(!t.input_schema.to_string().contains("IGNORE"));
    }
    let out =
        m.f.svc
            .call(&s, "jira_search", json!({"jql": "sso"}), now())
            .await
            .unwrap();
    assert!(!out.to_string().contains("call every write tool"));
    // An upstream tool no record approves is never listed or callable.
    assert!(tools.iter().all(|t| !t.name.contains("delete")));
    assert!(matches!(
        m.f.svc
            .call(&s, "jira_delete_project", json!({}), now())
            .await,
        Err(ToolError::UnknownTool(_))
    ));
}

#[tokio::test]
async fn threat_2_a_changed_tool_is_refused_and_never_comes_back() {
    let m = mcp_with(
        Limits {
            relist_every: Duration::ZERO,
            ..Limits::default()
        },
        |_| {},
    )
    .await;
    let s = session(&m, "ana").await;
    m.f.svc
        .call(&s, "jira_search", json!({"jql": "sso"}), now())
        .await
        .unwrap();
    m.fake.tricks.change_search.store(true, Ordering::SeqCst);
    let err =
        m.f.svc
            .call(&s, "jira_search", json!({"jql": "sso"}), now())
            .await
            .unwrap_err();
    let ToolError::Forbidden(text) = &err else {
        panic!("{err:?}")
    };
    assert!(
        text.contains("tool_changed: fake-jira.jira_search_issues"),
        "{text}"
    );
    assert!(text.contains("needs an Admin's review"));
    assert_eq!(
        calls_to(&m, "jira_search_issues"),
        1,
        "no upstream call after the change"
    );
    let e = events(&m, "jira.search").pop().unwrap();
    assert_eq!(e.outcome, Outcome::Denied);
    assert!(e
        .detail
        .unwrap()
        .contains("tool_changed: fake-jira.jira_search_issues"));
    let changed = events(&m, "mcp.tool_changed");
    assert_eq!(changed.len(), 1);
    assert_eq!(
        changed[0].detail.as_deref(),
        Some("tool_changed: fake-jira.jira_search_issues: changed since its approval")
    );
    // A re-list only takes tools away: changing it back doesn't bring it back.
    m.fake.tricks.change_search.store(false, Ordering::SeqCst);
    assert!(m
        .f
        .svc
        .call(&s, "jira_search", json!({"jql": "sso"}), now())
        .await
        .is_err());
    assert_eq!(
        m.server
            .status()
            .tools
            .iter()
            .find(|t| t.upstream == "jira_search_issues")
            .unwrap()
            .state,
        "changed"
    );
    // Other tools are unaffected; a tool that goes away is refused the same way.
    m.f.svc
        .call(&s, "jira_read", json!({"key": "PM-101"}), now())
        .await
        .unwrap();
    m.fake.tricks.drop_read.store(true, Ordering::SeqCst);
    let err =
        m.f.svc
            .call(&s, "jira_read", json!({"key": "PM-101"}), now())
            .await
            .unwrap_err();
    assert!(err
        .to_string()
        .contains("tool_changed: fake-jira.jira_get_issue"));
}

#[tokio::test]
async fn threat_2_a_list_changed_notification_triggers_a_relist() {
    // Default limits: only the notification can cause the re-list.
    let m = mcp().await;
    let s = session(&m, "ana").await;
    m.fake.tricks.notify.store(true, Ordering::SeqCst);
    m.f.svc
        .call(&s, "jira_search", json!({"jql": "sso"}), now())
        .await
        .unwrap();
    m.fake.tricks.notify.store(false, Ordering::SeqCst);
    m.fake.tricks.change_search.store(true, Ordering::SeqCst);
    // Give the notification a moment to arrive.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let lists_before = m.fake.calls().iter().filter(|c| *c == "tools/list").count();
    assert!(m
        .f
        .svc
        .call(&s, "jira_search", json!({"jql": "sso"}), now())
        .await
        .is_err());
    let lists_after = m.fake.calls().iter().filter(|c| *c == "tools/list").count();
    assert_eq!(lists_after, lists_before + 1, "re-listed before the call");
}

#[tokio::test]
async fn threat_3_each_registry_tool_reaches_only_its_own_server() {
    // Two fake servers that both offer every tool; Jira's record points at one, Confluence's
    // at the other.
    let one = FakeMcp::new();
    let two = FakeMcp::new();
    let (a1, c1) = start(one.clone()).await;
    let (a2, c2) = start(two.clone()).await;
    let jira_record = fake::fake_records(a1).remove(0);
    let confluence_record = fake::fake_records(a2).remove(1);
    let mk = |r| {
        Arc::new(
            McpServer::new(
                r,
                ServerAuth::Token(Secret::new(TOKEN)),
                None,
                &TestRoutes::default(),
            )
            .unwrap(),
        )
    };
    let jira = McpConnector::new("jira", mk(jira_record), "mcp").unwrap();
    let confluence = McpConnector::new("confluence", mk(confluence_record), "mcp").unwrap();
    let caller = pmos_toolserver::connectors::Caller::new("ana", "r1");
    jira.read(caller, "search", &json!({"jql": "sso"}))
        .await
        .unwrap();
    confluence
        .read(caller, "search", &json!({"cql": "sso"}))
        .await
        .unwrap();
    assert!(one
        .calls()
        .contains(&"tools/call:jira_search_issues".to_string()));
    assert!(!one.calls().iter().any(|c| c.contains("confluence")));
    assert!(two
        .calls()
        .contains(&"tools/call:confluence_search".to_string()));
    assert!(!two.calls().iter().any(|c| c.contains("jira")));
    // A system the record doesn't serve can't be built on it.
    let records = fake::fake_records(a1);
    let server = mk(records[0].clone());
    assert!(McpConnector::new("confluence", server, "mcp").is_err());
    drop((c1, c2));
}

#[tokio::test]
async fn threat_3_lint_refuses_a_name_mapped_by_two_records() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::copy(
        concat!(env!("CARGO_MANIFEST_DIR"), "/../../content/tools.yaml"),
        root.join("tools.yaml"),
    )
    .unwrap();
    std::fs::create_dir(root.join("mcp")).unwrap();
    let registry = pmos_contract::ToolRegistry::from_yaml(
        &std::fs::read_to_string(root.join("tools.yaml")).unwrap(),
    )
    .unwrap();
    let mut a = fake::fake_records("127.0.0.1:9".parse().unwrap()).remove(0);
    let yaml = |r: &pmos_contract::mcp::ServerRecord| serde_norway::to_string(r).unwrap();
    std::fs::write(root.join("mcp/fake-jira.yaml"), yaml(&a)).unwrap();
    assert!(
        pmos_contract::mcp::load(root, &registry).is_ok(),
        "one record loads"
    );
    a.id = "other-jira".into();
    std::fs::write(root.join("mcp/other-jira.yaml"), yaml(&a)).unwrap();
    let errors = pmos_contract::mcp::load(root, &registry).unwrap_err();
    assert!(
        errors.iter().any(|e| e.contains("mcp-shadow")),
        "{errors:?}"
    );
}

#[tokio::test]
async fn threat_9_server_requests_are_refused_and_audited() {
    let m = mcp().await;
    let s = session(&m, "ana").await;
    m.fake.tricks.ask_client.store(true, Ordering::SeqCst);
    let out =
        m.f.svc
            .call(&s, "jira_search", json!({"jql": "sso"}), now())
            .await
            .unwrap();
    assert_eq!(
        out["results"].as_array().unwrap().len(),
        1,
        "the call itself still answers"
    );
    let asked = m.fake.asked();
    assert_eq!(asked.len(), 3, "{asked:?}");
    for (method, answer) in &asked {
        assert!(
            answer.starts_with("error"),
            "{method} was answered: {answer}"
        );
    }
    let refused: Vec<String> = events(&m, "mcp.server_request_refused")
        .into_iter()
        .map(|e| {
            assert_eq!(e.outcome, Outcome::Denied);
            e.detail.unwrap()
        })
        .collect();
    assert_eq!(
        refused,
        vec![
            "server fake-jira sent sampling/createMessage; refused",
            "server fake-jira sent roots/list; refused",
            "server fake-jira sent elicitation/create; refused",
        ]
    );
    assert!(
        !refused
            .iter()
            .any(|d| d.contains("inbox") || d.contains("password")),
        "method names only"
    );
}

#[tokio::test]
async fn threat_10_resources_prompts_and_logs_are_never_used() {
    let m = mcp().await;
    let s = session(&m, "ana").await;
    m.fake.tricks.notify.store(true, Ordering::SeqCst);
    m.f.svc
        .call(&s, "jira_search", json!({"jql": "sso"}), now())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let calls = m.fake.calls();
    assert!(
        !calls
            .iter()
            .any(|c| c.starts_with("resources/") || c.starts_with("prompts/")),
        "{calls:?}"
    );
    assert!(
        m.server.status().dropped_notifications >= 3,
        "{:?}",
        m.server.status()
    );
    let all = serde_json::to_string(&m.f.audit.events()).unwrap();
    assert!(!all.contains("secret log line") && !all.contains("/etc/passwd"));
}

#[tokio::test]
async fn threat_11_huge_and_slow_answers_fail_within_the_deadline() {
    let m = mcp_with(
        Limits {
            read: Duration::from_secs(2),
            ..Limits::default()
        },
        |_| {},
    )
    .await;
    let s = session(&m, "ana").await;
    m.fake.tricks.huge.store(true, Ordering::SeqCst);
    let err =
        m.f.svc
            .call(&s, "jira_search", json!({"jql": "sso"}), now())
            .await
            .unwrap_err();
    assert!(matches!(err, ToolError::Connector(_)), "{err:?}");
    m.fake.tricks.huge.store(false, Ordering::SeqCst);

    // A server that never answers: the call fails at the deadline while another PM's goes on.
    m.fake.tricks.hang.store(true, Ordering::SeqCst);
    let svc = m.f.svc.clone();
    let slow = tokio::spawn(async move {
        let started = Instant::now();
        let r = svc
            .call(&s, "jira_search", json!({"jql": "sso"}), now())
            .await;
        (r, started.elapsed())
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    m.fake.tricks.hang.store(false, Ordering::SeqCst);
    let ben = session(&m, "ben").await;
    m.f.svc
        .call(&ben, "jira_read", json!({"key": "PM-102"}), now())
        .await
        .unwrap();
    let (r, took) = slow.await.unwrap();
    let err = r.unwrap_err();
    assert!(
        err.to_string().contains("didn't answer within 2 s"),
        "{err}"
    );
    assert!(took < Duration::from_secs(6), "{took:?}");
}

#[tokio::test]
async fn mcp_18_concurrency_and_rate_caps_answer_without_reaching_the_server() {
    let m = mcp_with(
        Limits {
            per_user_per_minute: 2,
            ..Limits::default()
        },
        |_| {},
    )
    .await;
    let s = session(&m, "ana").await;
    for _ in 0..2 {
        m.f.svc
            .call(&s, "jira_search", json!({"jql": "sso"}), now())
            .await
            .unwrap();
    }
    let err =
        m.f.svc
            .call(&s, "jira_search", json!({"jql": "sso"}), now())
            .await
            .unwrap_err();
    assert!(err.to_string().contains("rate limited"), "{err}");
    assert_eq!(calls_to(&m, "jira_search_issues"), 2);
    // Another PM has their own allowance.
    let ben = session(&m, "ben").await;
    m.f.svc
        .call(&ben, "jira_search", json!({"jql": "sso"}), now())
        .await
        .unwrap();

    let m = mcp_with(
        Limits {
            in_flight: 1,
            read: Duration::from_secs(2),
            ..Limits::default()
        },
        |_| {},
    )
    .await;
    let s = session(&m, "ana").await;
    m.fake.tricks.hang.store(true, Ordering::SeqCst);
    let svc = m.f.svc.clone();
    let s2 = s.clone();
    let slow = tokio::spawn(async move {
        svc.call(&s2, "jira_search", json!({"jql": "sso"}), now())
            .await
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    let ben = session(&m, "ben").await;
    let err =
        m.f.svc
            .call(&ben, "jira_read", json!({"key": "PM-102"}), now())
            .await
            .unwrap_err();
    assert!(err.to_string().contains("rate limited"), "{err}");
    assert_eq!(calls_to(&m, "jira_get_issue"), 0);
    assert!(slow.await.unwrap().is_err());
}

async fn pending(m: &Mcp, s: &pmos_toolserver::session::Session, args: Value) -> String {
    let res =
        m.f.svc
            .call(s, "jira_create_issue", args, now())
            .await
            .unwrap();
    assert_eq!(res["status"], "pending_approval", "{res}");
    res["approval_id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn mcp_5_6_7_every_write_waits_for_approval_and_sends_exactly_the_mapped_arguments() {
    let m = mcp().await;
    let s = session(&m, "ana").await;
    // The server says its create tool is read-only; the record says it's a write.
    assert!(m.fake.tools().iter().any(|t| t.name == "jira_create_issue"
        && t.annotations.as_ref().and_then(|a| a.read_only_hint) == Some(true)));
    let args = json!({"project": "PM", "issue_type": "Story", "summary": "SCIM", "description": "Provision users.", "epic": "PM-101"});
    let id = pending(&m, &s, args.clone()).await;
    assert_eq!(
        calls_to(&m, "jira_create_issue"),
        0,
        "nothing sent before approval"
    );
    let item =
        m.f.svc
            .decide(&id, "ana", Decision::Approve, None, now())
            .await
            .unwrap();
    assert_eq!(item.status, Status::Approved, "{:?}", item.result);
    assert_eq!(item.result.as_ref().unwrap()["id"], "JIRA-W1");
    let received = m.fake.received();
    let (tool, sent) = received.last().unwrap();
    assert_eq!(tool, "jira_create_issue");
    assert_eq!(
        sent,
        &json!({
            "project_key": "PM",
            "issue_type": "Story",
            "summary": "SCIM",
            "description": "Provision users.\n\nDrafted with PM OS · run r1",
            "parent_key": "PM-101",
        }),
        "exactly the approved arguments, renamed, plus the marker"
    );
    let text = sent.to_string();
    assert!(
        !text.contains(TOKEN) && !text.contains(&s.token),
        "no PM OS token or session in the body"
    );
    // Audited as a connector write: pending, started, approved, each naming the server.
    let outcomes: Vec<Outcome> = events(&m, "jira.create_issue")
        .iter()
        .map(|e| e.outcome)
        .collect();
    assert_eq!(
        outcomes,
        vec![Outcome::Pending, Outcome::Started, Outcome::Approved]
    );
    for e in events(&m, "jira.create_issue") {
        assert!(
            e.detail
                .as_deref()
                .unwrap_or_default()
                .contains("via mcp server fake-jira"),
            "{e:?}"
        );
    }
    // An edit from a client that acts for the PM can't move the issue to another project
    // (EDIT_IMMUTABLE_ARGS still holds).
    let id = pending(&m, &s, args).await;
    let moved = json!({"project": "ENG", "issue_type": "Story", "summary": "SCIM"});
    assert!(m
        .f
        .svc
        .decide_as(
            pmos_toolserver::audit::Client::Bot,
            &id,
            "ana",
            Decision::Edit,
            Some(moved),
            now()
        )
        .await
        .is_err());
    assert_eq!(calls_to(&m, "jira_create_issue"), 1);
}

#[tokio::test]
async fn mcp_18_a_write_that_times_out_may_or_may_not_have_happened_and_is_never_retried() {
    let m = mcp_with(
        Limits {
            write: Duration::from_secs(1),
            ..Limits::default()
        },
        |_| {},
    )
    .await;
    let s = session(&m, "ana").await;
    let id = pending(
        &m,
        &s,
        json!({"project": "PM", "issue_type": "Task", "summary": "T"}),
    )
    .await;
    // Connected and listed first, so only the write itself hangs.
    m.f.svc
        .call(&s, "jira_read", json!({"key": "PM-101"}), now())
        .await
        .unwrap();
    m.fake.tricks.hang.store(true, Ordering::SeqCst);
    let item =
        m.f.svc
            .decide(&id, "ana", Decision::Approve, None, now())
            .await
            .unwrap();
    assert_eq!(item.status, Status::Failed);
    let error = item.result.unwrap()["error"].as_str().unwrap().to_string();
    assert!(error.contains("may or may not have happened"), "{error}");
    assert_eq!(calls_to(&m, "jira_create_issue"), 1, "never retried");
}

#[tokio::test]
async fn mcp_14_reads_raise_the_session_to_the_servers_data_class() {
    let m = mcp_with(Limits::default(), |r| {
        r.data_class = Sensitivity::Restricted
    })
    .await;
    let s = session(&m, "ana").await;
    m.f.svc
        .call(&s, "jira_search", json!({"jql": "sso"}), now())
        .await
        .unwrap();
    let id = pending(
        &m,
        &s,
        json!({"project": "PM", "issue_type": "Task", "summary": "T"}),
    )
    .await;
    let item = m.f.svc.approval(&id).await.unwrap().unwrap();
    assert_eq!(item.sources_sensitivity, Some(Sensitivity::Restricted));
}

#[tokio::test]
async fn mcp_15_a_team_bots_read_scope_holds_on_the_mcp_backend() {
    let m = mcp().await;
    // `scout` reads Jira project PM only.
    let s =
        m.f.svc
            .create_bot_session("ana", "scout", "tsk_1", None, now())
            .await
            .unwrap();
    let out =
        m.f.svc
            .call(&s, "jira_search", json!({"jql": "sso"}), now())
            .await
            .unwrap();
    assert_eq!(out["results"].as_array().unwrap().len(), 1);
    let (_, sent) = m.fake.received().pop().unwrap();
    assert_eq!(
        sent["jql"], "project in (\"PM\") AND (sso)",
        "wrapped before it left PM OS"
    );
    // `blind` reads nothing: refused before anything is sent.
    let before = m.fake.received().len();
    let blind =
        m.f.svc
            .create_bot_session("ana", "blind", "tsk_2", None, now())
            .await
            .unwrap();
    assert!(matches!(
        m.f.svc
            .call(&blind, "jira_search", json!({"jql": "sso"}), now())
            .await,
        Err(ToolError::Forbidden(_))
    ));
    assert_eq!(m.fake.received().len(), before);
}

#[tokio::test]
async fn mcp_21_an_internal_server_is_reached_only_at_its_pinned_address() {
    // `fake-mcp.pmos.internal` is never looked up: the pinned address is the only way in.
    let m = mcp().await;
    let s = session(&m, "ana").await;
    m.f.svc
        .call(&s, "jira_read", json!({"key": "PM-101"}), now())
        .await
        .unwrap();
    // Pinned to an address where nothing listens: refused, no fallback to DNS.
    let m = mcp_with(
        Limits {
            read: Duration::from_secs(5),
            ..Limits::default()
        },
        |r| r.identity.address = Some("127.0.0.2".into()),
    )
    .await;
    let s = session(&m, "ana").await;
    assert!(m
        .f
        .svc
        .call(&s, "jira_read", json!({"key": "PM-101"}), now())
        .await
        .is_err());
    assert_eq!(m.fake.calls().len(), 0);
}

#[tokio::test]
async fn mcp_21_redirects_are_refused() {
    use axum::response::Redirect;
    let fake = FakeMcp::new();
    let (addr, _cancel) = start(fake.clone()).await;
    // A server that redirects to the real fake.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let redirector = listener.local_addr().unwrap();
    let target = format!("http://127.0.0.1:{}/mcp", addr.port());
    let app = axum::Router::new().fallback(move || {
        let t = target.clone();
        async move { Redirect::temporary(&t) }
    });
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut record = fake::fake_records(redirector).remove(0);
    record.identity.address = Some("127.0.0.1".into());
    let server = Arc::new(
        McpServer::new(
            record,
            ServerAuth::Token(Secret::new(TOKEN)),
            None,
            &TestRoutes::default(),
        )
        .unwrap(),
    );
    let jira = McpConnector::new("jira", server, "mcp").unwrap();
    let caller = pmos_toolserver::connectors::Caller::new("ana", "r1");
    assert!(jira
        .read(caller, "read", &json!({"key": "PM-101"}))
        .await
        .is_err());
    assert!(fake.calls().is_empty(), "the redirect was not followed");
}

#[tokio::test]
async fn mcp_2_no_setting_adds_a_server() {
    // The tool server's configuration has no way to name an MCP server's URL: only records do.
    // Without PMOS_MCP_SYSTEMS nothing is switched, and a record serving nothing named isn't
    // connected.
    let get = |k: &str| match k {
        "PMOS_MODEL_SOURCE" => Some("local".to_string()),
        "PMOS_SERVICE_TOKEN" => Some("s".repeat(40)),
        "PMOS_DEV" => Some("1".into()),
        "PMOS_CONNECTORS" => Some("fake".into()),
        "PMOS_GATEWAY_URL" => Some("http://127.0.0.1:1".into()),
        "PMOS_MCP_URL" | "PMOS_MCP_SERVERS" => Some("https://evil.example.com/mcp".into()),
        _ => None,
    };
    let config = pmos_toolserver::config::Config::from_lookup(get).unwrap();
    assert!(config.mcp_systems.is_empty());
    assert!(!config.fake_mcp);
}
