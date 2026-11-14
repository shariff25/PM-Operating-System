//! The tool server's rules, tested through `ToolService`.

mod common;

use chrono::Duration;
use common::{fixture, now};
use pmos_toolserver::approvals::{DecideError, Decision, Status};
use pmos_toolserver::audit::Outcome;
use pmos_toolserver::service::{DecisionError, SessionError, ToolError};
use serde_json::{json, Value};

fn outcomes(f: &common::Fixture) -> Vec<(String, Outcome)> {
    f.audit
        .events()
        .into_iter()
        .map(|e| (e.tool, e.outcome))
        .collect()
}

#[tokio::test]
async fn with_live_connectors_only_team_members_get_sessions() {
    let f = common::team_fixture(&["ana"]);
    assert!(f
        .svc
        .create_session("ana", "ask", "r1", None, now())
        .await
        .is_ok());
    assert_eq!(
        f.svc.create_session("eve", "ask", "r1", None, now()).await,
        Err(SessionError::NotAMember("eve".into()))
    );
}

#[tokio::test]
async fn sessions_validate_user_and_skill() {
    let f = fixture();
    assert_eq!(
        f.svc.create_session("Ana", "ask", "r1", None, now()).await,
        Err(SessionError::InvalidUser("Ana".into()))
    );
    assert_eq!(
        f.svc.create_session("ana", "nope", "r1", None, now()).await,
        Err(SessionError::UnknownSkill("nope".into()))
    );
    assert_eq!(
        f.svc.create_session("ana", "ask", "", None, now()).await,
        Err(SessionError::InvalidRun)
    );
    let s = f
        .svc
        .create_session("ana", "ask", "r1", Some(Duration::minutes(5)), now())
        .await
        .unwrap();
    let soon = now() + Duration::minutes(4);
    let expired = now() + Duration::minutes(5);
    assert!(f.svc.session(&s.token, soon).await.unwrap().is_some());
    assert!(f.svc.session(&s.token, expired).await.unwrap().is_none());
}

#[tokio::test]
async fn lists_only_declared_tools() {
    let f = fixture();
    let s = f
        .svc
        .create_session("ana", "prd-draft", "r1", None, now())
        .await
        .unwrap();
    let names: Vec<_> = f
        .svc
        .list_tools(&s)
        .iter()
        .map(|t| t.harness_name.clone())
        .collect();
    assert_eq!(
        names,
        vec![
            "confluence_create_draft",
            "confluence_read",
            "confluence_search",
            "context_read",
            "context_search",
            "docs_read",
            "docs_search",
            "jira_read",
            "jira_search",
        ]
    );
}

#[tokio::test]
async fn undeclared_and_unknown_tools_are_denied_and_audited() {
    let f = fixture();
    let s = f
        .svc
        .create_session("ana", "ask", "r1", None, now())
        .await
        .unwrap();
    let err = f
        .svc
        .call(
            &s,
            "slack_send_message",
            json!({"channel": "C1", "text": "hi"}),
            now(),
        )
        .await
        .unwrap_err();
    assert_eq!(err, ToolError::NotAllowed("slack_send_message".into()));
    let err = f
        .svc
        .call(&s, "shell_exec", json!({}), now())
        .await
        .unwrap_err();
    assert_eq!(err, ToolError::UnknownTool("shell_exec".into()));
    assert!(f.slack.writes().is_empty());
    assert_eq!(
        outcomes(&f),
        vec![
            ("slack.send_message".into(), Outcome::Denied),
            ("shell_exec".into(), Outcome::Denied),
        ]
    );
}

#[tokio::test]
async fn arguments_are_checked_against_the_schema() {
    let f = fixture();
    let s = f
        .svc
        .create_session("ana", "ask", "r1", None, now())
        .await
        .unwrap();
    let err = f
        .svc
        .call(
            &s,
            "context_search",
            json!({"query": "x", "sql": "drop"}),
            now(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::InvalidArgs(m) if m.contains("unknown argument")));
    let err = f
        .svc
        .call(&s, "context_search", Value::Null, now())
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::InvalidArgs(m) if m.contains("query")));
}

#[tokio::test]
async fn context_respects_the_skill_data_class() {
    let f = fixture();
    let internal = f
        .svc
        .create_session("ana", "internal-brief", "r1", None, now())
        .await
        .unwrap();
    let restricted = f
        .svc
        .create_session("ana", "ask", "r2", None, now())
        .await
        .unwrap();

    // Search: the internal skill never sees the Restricted file.
    let res = f
        .svc
        .call(
            &internal,
            "context_search",
            json!({"query": "key management"}),
            now(),
        )
        .await
        .unwrap();
    let paths: Vec<_> = res["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["path"].as_str().unwrap().to_string())
        .collect();
    assert!(paths.contains(&"context/products/key-management.md".to_string()));
    assert!(!paths.contains(&"context/partners/acme-nda.md".to_string()));
    let search_event = f.audit.events().pop().unwrap();
    assert!(search_event.detail.unwrap().contains("withheld"));

    let res = f
        .svc
        .call(
            &restricted,
            "context_search",
            json!({"query": "key management"}),
            now(),
        )
        .await
        .unwrap();
    assert!(res["results"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["path"] == "context/partners/acme-nda.md"));
    assert!(
        f.audit.events().pop().unwrap().detail.is_none(),
        "nothing withheld"
    );

    // Read: refused for the internal skill, allowed for the restricted one.
    let err = f
        .svc
        .call(
            &internal,
            "context_read",
            json!({"path": "context/partners/acme-nda.md"}),
            now(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Forbidden(_)));
    assert_eq!(f.audit.events().pop().unwrap().outcome, Outcome::Denied);
    let doc = f
        .svc
        .call(
            &restricted,
            "context_read",
            json!({"path": "context/partners/acme-nda.md"}),
            now(),
        )
        .await
        .unwrap();
    assert!(doc["body"].as_str().unwrap().contains("under NDA"));
    assert_eq!(doc["cite"], "context/partners/acme-nda.md");
}

#[tokio::test]
async fn context_flags_stale_files() {
    let f = fixture();
    let s = f
        .svc
        .create_session("ana", "ask", "r1", None, now())
        .await
        .unwrap();
    let doc = f
        .svc
        .call(
            &s,
            "context_read",
            json!({"path": "context/products/key-management.md"}),
            now(),
        )
        .await
        .unwrap();
    assert_eq!(doc["stale"], true);
    assert!(doc["stale_note"].as_str().unwrap().contains("@pm"));
}

#[tokio::test]
async fn context_read_rejects_paths_outside_the_library() {
    let f = fixture();
    let s = f
        .svc
        .create_session("ana", "ask", "r1", None, now())
        .await
        .unwrap();
    for path in [
        "context/../tools.yaml",
        "/etc/passwd",
        "skills/ask/SKILL.md",
        "master/AGENTS.md",
    ] {
        let err = f
            .svc
            .call(&s, "context_read", json!({ "path": path }), now())
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)), "{path}: {err:?}");
    }
    let err = f
        .svc
        .call(
            &s,
            "context_read",
            json!({"path": "context/products/none.md"}),
            now(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::NotFound(_)));
}

#[tokio::test]
async fn connector_results_are_wrapped_as_untrusted() {
    let f = fixture();
    let s = f
        .svc
        .create_session("ana", "ask", "r1", None, now())
        .await
        .unwrap();
    let res = f
        .svc
        .call(&s, "jira_read", json!({"key": "PM-103"}), now())
        .await
        .unwrap();
    let item = &res["results"][0];
    assert_eq!(item["untrusted"], true);
    assert_eq!(item["source"]["id"], "PM-103");
    assert!(item["content"]["description"]
        .as_str()
        .unwrap()
        .contains("IGNORE PREVIOUS INSTRUCTIONS"));
    assert_eq!(f.audit.events().pop().unwrap().sources, vec!["jira:PM-103"]);
    assert!(
        f.slack.writes().is_empty(),
        "reading injected text writes nothing"
    );
}

fn draft() -> Value {
    json!({"space": "PM", "title": "PRD: SSO", "body": "Draft"})
}

#[tokio::test]
async fn writes_without_a_grant_wait_for_approval() {
    let f = fixture();
    let s = f
        .svc
        .create_session("ana", "prd-draft", "r1", None, now())
        .await
        .unwrap();
    let res = f
        .svc
        .call(&s, "confluence_create_draft", draft(), now())
        .await
        .unwrap();
    assert_eq!(res["status"], "pending_approval");
    assert!(f.confluence.writes().is_empty());
    let id = res["approval_id"].as_str().unwrap();
    let items = f.svc.approvals("ana", Some(Status::Pending)).await.unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].id, id);
    assert_eq!(items[0].tool, "confluence.create_draft");
    let event = f.audit.events().pop().unwrap();
    assert_eq!(event.outcome, Outcome::Pending);
    assert_eq!(event.output_ref.as_deref(), Some(id));
}

#[tokio::test]
async fn a_grant_runs_exactly_the_approved_call_once() {
    let f = fixture();
    let s = f
        .svc
        .create_session("ana", "prd-draft", "r1", None, now())
        .await
        .unwrap();
    f.svc
        .grant(&s, "confluence_create_draft", &draft(), now())
        .await
        .unwrap();

    // Different arguments: no match, so it queues.
    let mut other = draft();
    other["title"] = json!("PRD: something else");
    let res = f
        .svc
        .call(&s, "confluence_create_draft", other, now())
        .await
        .unwrap();
    assert_eq!(res["status"], "pending_approval");

    // Same arguments, keys in another order: runs, as the session's user.
    let reordered = json!({"body": "Draft", "title": "PRD: SSO", "space": "PM"});
    let res = f
        .svc
        .call(&s, "confluence_create_draft", reordered.clone(), now())
        .await
        .unwrap();
    assert_eq!(res["status"], "done");
    let writes = f.confluence.writes();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].user, "ana");
    assert_eq!(writes[0].action, "create_draft");

    // The grant is used up.
    let res = f
        .svc
        .call(&s, "confluence_create_draft", reordered, now())
        .await
        .unwrap();
    assert_eq!(res["status"], "pending_approval");
    assert_eq!(f.confluence.writes().len(), 1);

    let seen: Vec<_> = outcomes(&f).into_iter().map(|(_, o)| o).collect();
    assert_eq!(
        seen,
        vec![
            Outcome::Granted,
            Outcome::Pending,
            Outcome::Started,
            Outcome::Approved,
            Outcome::Pending
        ]
    );
}

#[tokio::test]
async fn grants_belong_to_their_session_and_expire() {
    let f = fixture();
    let a = f
        .svc
        .create_session("ana", "prd-draft", "r1", None, now())
        .await
        .unwrap();
    let b = f
        .svc
        .create_session("ana", "prd-draft", "r2", None, now())
        .await
        .unwrap();
    f.svc
        .grant(&a, "confluence_create_draft", &draft(), now())
        .await
        .unwrap();
    let res = f
        .svc
        .call(&b, "confluence_create_draft", draft(), now())
        .await
        .unwrap();
    assert_eq!(
        res["status"], "pending_approval",
        "another session can't use it"
    );
    let later = now() + Duration::minutes(16);
    let res = f
        .svc
        .call(&a, "confluence_create_draft", draft(), later)
        .await
        .unwrap();
    assert_eq!(res["status"], "pending_approval", "expired");
    assert!(f.confluence.writes().is_empty());
}

#[tokio::test]
async fn grants_only_for_declared_write_tools() {
    let f = fixture();
    let s = f
        .svc
        .create_session("ana", "prd-draft", "r1", None, now())
        .await
        .unwrap();
    assert!(matches!(
        f.svc
            .grant(&s, "jira_search", &json!({"jql": "x"}), now())
            .await,
        Err(ToolError::InvalidArgs(_))
    ));
    assert!(matches!(
        f.svc
            .grant(&s, "slack_send_message", &json!({}), now())
            .await,
        Err(ToolError::NotAllowed(_))
    ));
}

async fn pending_issue(f: &common::Fixture) -> String {
    let s = f
        .svc
        .create_session("ana", "meeting-notes", "r1", None, now())
        .await
        .unwrap();
    let res = f
        .svc
        .call(
            &s,
            "jira_create_issue",
            json!({"project": "PM", "issue_type": "Story", "summary": "SCIM"}),
            now(),
        )
        .await
        .unwrap();
    res["approval_id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn only_the_items_user_can_approve() {
    let f = fixture();
    let id = pending_issue(&f).await;
    let err = f
        .svc
        .decide(&id, "bo", Decision::Approve, None, now())
        .await
        .unwrap_err();
    assert_eq!(err, DecisionError::Decide(DecideError::Forbidden));
    assert!(f.jira.writes().is_empty());
    let denied = f.audit.events().pop().unwrap();
    assert_eq!(denied.outcome, Outcome::Denied);
    assert_eq!(denied.user, "bo");

    let item = f
        .svc
        .decide(&id, "ana", Decision::Approve, None, now())
        .await
        .unwrap();
    assert_eq!(item.status, Status::Approved);
    assert_eq!(item.result.as_ref().unwrap()["status"], "done");
    assert_eq!(f.jira.writes().len(), 1);
    assert_eq!(f.jira.writes()[0].user, "ana");
    assert_eq!(f.audit.events().pop().unwrap().outcome, Outcome::Approved);

    let err = f
        .svc
        .decide(&id, "ana", Decision::Approve, None, now())
        .await
        .unwrap_err();
    assert_eq!(err, DecisionError::Decide(DecideError::NotPending));
    assert_eq!(f.jira.writes().len(), 1, "never runs twice");
}

#[tokio::test]
async fn edit_runs_the_edited_arguments_and_reject_runs_nothing() {
    let f = fixture();
    let id = pending_issue(&f).await;
    let edited = json!({"project": "PM", "issue_type": "Story", "summary": "SCIM provisioning"});
    f.svc
        .decide(&id, "ana", Decision::Edit, Some(edited.clone()), now())
        .await
        .unwrap();
    assert_eq!(f.jira.writes()[0].args, edited);

    let id = pending_issue(&f).await;
    let item = f
        .svc
        .decide(&id, "ana", Decision::Reject, None, now())
        .await
        .unwrap();
    assert_eq!(item.status, Status::Rejected);
    assert_eq!(f.jira.writes().len(), 1);
    assert_eq!(f.audit.events().pop().unwrap().outcome, Outcome::Rejected);
}

#[tokio::test]
async fn an_edit_is_checked_like_any_call() {
    let f = fixture();
    let id = pending_issue(&f).await;
    for bad in [
        json!({"project": "PM", "issue_type": "Sub-task", "summary": "x"}),
        json!({"project": "PM", "issue_type": "Story", "summary": "x", "epic": 101}),
        json!({"project": "PM", "issue_type": "Story", "summary": "x", "assignee": "bo"}),
        json!("not an object"),
    ] {
        let err = f
            .svc
            .decide(&id, "ana", Decision::Edit, Some(bad.clone()), now())
            .await
            .unwrap_err();
        assert!(
            matches!(err, DecisionError::InvalidArgs(_)),
            "{bad}: {err:?}"
        );
    }
    assert!(f.jira.writes().is_empty());
    let item = f.svc.approval(&id).await.unwrap().unwrap();
    assert_eq!(
        item.status,
        Status::Pending,
        "a bad edit doesn't use the item up"
    );
}

#[tokio::test]
async fn an_item_runs_only_if_its_skill_still_declares_the_tool() {
    let f = fixture();
    // An item queued under `ask`, which declares no write tools (say, after a skill change).
    let item = f
        .stores
        .approvals
        .create(
            pmos_toolserver::approvals::NewItem {
                user: "ana",
                skill: "ask",
                run_id: "r1",
                tool: "jira.create_issue",
                args: &json!({"project": "PM", "issue_type": "Story", "summary": "x"}),
            },
            now(),
        )
        .await
        .unwrap();
    let err = f
        .svc
        .decide(&item.id, "ana", Decision::Approve, None, now())
        .await
        .unwrap_err();
    assert_eq!(err, DecisionError::NotAllowed("jira.create_issue".into()));
    assert!(f.jira.writes().is_empty());
    assert_eq!(
        f.svc.approval(&item.id).await.unwrap().unwrap().status,
        Status::Pending
    );
}

#[tokio::test]
async fn no_write_runs_unless_the_audit_log_records_it() {
    let f = fixture();
    let id = pending_issue(&f).await;
    f.audit_switch
        .down
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let err = f
        .svc
        .decide(&id, "ana", Decision::Approve, None, now())
        .await
        .unwrap_err();
    assert!(
        matches!(err, DecisionError::Decide(DecideError::Store(_))),
        "{err:?}"
    );
    assert!(f.jira.writes().is_empty(), "fail closed");
    let item = f.svc.approval(&id).await.unwrap().unwrap();
    assert_eq!(item.status, Status::Failed);
    assert!(item.result.unwrap()["error"]
        .as_str()
        .unwrap()
        .contains("audit log"));

    // The same holds for a write approved in the harness, and its grant survives.
    let s = f
        .svc
        .create_session("ana", "meeting-notes", "r2", None, now())
        .await
        .unwrap();
    let args = json!({"project": "PM", "issue_type": "Story", "summary": "SCIM"});
    f.audit_switch
        .down
        .store(false, std::sync::atomic::Ordering::SeqCst);
    f.svc
        .grant(&s, "jira_create_issue", &args, now())
        .await
        .unwrap();
    f.audit_switch
        .down
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let err = f
        .svc
        .call(&s, "jira_create_issue", args.clone(), now())
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Unavailable(_)), "{err:?}");
    assert!(f.jira.writes().is_empty());
    f.audit_switch
        .down
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let res = f
        .svc
        .call(&s, "jira_create_issue", args, now())
        .await
        .unwrap();
    assert_eq!(res["status"], "done", "the grant was put back");
}

#[tokio::test]
async fn an_interrupted_write_is_marked_outcome_unknown() {
    let f = fixture();
    let id = pending_issue(&f).await;
    // The process died after the decision, mid-write: the item is left `executing`.
    f.stores
        .approvals
        .decide(&id, "ana", Decision::Approve, None, now())
        .await
        .unwrap();

    // Not yet stale.
    assert_eq!(
        f.svc
            .reconcile(Duration::minutes(10), now() + Duration::minutes(5))
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        f.svc
            .reconcile(Duration::minutes(10), now() + Duration::minutes(11))
            .await
            .unwrap(),
        1
    );
    let item = f.svc.approval(&id).await.unwrap().unwrap();
    assert_eq!(item.status, Status::Failed);
    let message = item.result.unwrap()["error"].as_str().unwrap().to_string();
    assert!(
        message.contains("may or may not have happened"),
        "{message}"
    );
    assert!(message.contains("check jira"), "{message}");
    let last = f.audit.events().pop().unwrap();
    assert_eq!((last.outcome, last.output_ref), (Outcome::Error, Some(id)));
    assert!(f.jira.writes().is_empty(), "nothing is retried");
}

#[tokio::test]
async fn memory_is_per_user() {
    let f = fixture();
    let ana = f
        .svc
        .create_session("ana", "internal-brief", "r1", None, now())
        .await
        .unwrap();
    let bo = f
        .svc
        .create_session("bo", "internal-brief", "r2", None, now())
        .await
        .unwrap();
    f.svc
        .call(
            &ana,
            "memory_update",
            json!({"key": "areas", "text": "CCM"}),
            now(),
        )
        .await
        .unwrap();
    let err = f
        .svc
        .call(
            &ana,
            "memory_update",
            json!({"key": "../x", "text": "no"}),
            now(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::InvalidArgs(_)));

    let mine = f
        .svc
        .call(&ana, "memory_read", json!({}), now())
        .await
        .unwrap();
    assert_eq!(mine["notes"][0]["text"], "CCM");
    let theirs = f
        .svc
        .call(&bo, "memory_read", json!({}), now())
        .await
        .unwrap();
    assert_eq!(theirs["notes"], json!([]));

    // Memory writes stay inside PM OS, so they need no approval.
    assert!(f.svc.approvals("ana", None).await.unwrap().is_empty());

    // A skill that didn't declare memory tools can't use them.
    let ask = f
        .svc
        .create_session("ana", "ask", "r3", None, now())
        .await
        .unwrap();
    let err = f
        .svc
        .call(&ask, "memory_read", json!({}), now())
        .await
        .unwrap_err();
    assert_eq!(err, ToolError::NotAllowed("memory_read".into()));
}
