//! The HTTP API end to end, with a mock tool server and gateway: tasks from Slack to a
//! worker's result, approvals, the notification feed, routines and readiness. Lifecycle cases
//! run on both stores.

#[macro_use]
mod common;

use std::time::{Duration, Instant};

use common::{Env, SessionMode};
use reqwest::Method;
use serde_json::{json, Value};

fn id(v: &Value) -> String {
    v["id"].as_str().unwrap().to_string()
}

// ----- bots -------------------------------------------------------------------------------

#[tokio::test]
async fn bots_are_listed_with_their_tools_and_settings() {
    let env = Env::memory().await;
    let (status, bots) = env.get("/v1/bots", "worker", None).await;
    assert_eq!(status, 200);
    assert_eq!(
        bots,
        json!([
            {"name": "docs", "role": "docs-helper", "scope": "team", "owner": "ana", "description": "The Docs bot"},
            {"name": "pm-ana", "role": "pm-assistant", "scope": "personal", "owner": "ana", "description": "Ana's assistant"},
        ])
    );
    let (status, detail) = env.get("/v1/bots/pm-ana", "slack", Some("ben")).await;
    assert_eq!(status, 200);
    assert_eq!(
        detail["tools"],
        json!(["jira.create_issue", "jira.search", "slack.send_message"])
    );
    assert_eq!(
        detail["actions"],
        json!({"jira.create_issue": "approve", "slack.send_message": "never"}),
        "write tools only, with the role's settings"
    );
    assert_eq!(detail["role"]["model"], "extraction");
    assert_eq!(detail["bot"]["job"], "Help Ana.");
    let (status, _) = env.get("/v1/bots/nobody", "console", None).await;
    assert_eq!(status, 404);
    env.finish().await;
}

// ----- the task lifecycle -----------------------------------------------------------------

async fn a_task_runs_from_slack_to_a_result(env: &Env) {
    let task = env.ask("What ports does Acme Vault need?").await;
    let tid = id(&task);
    assert!(tid.starts_with("tsk_") && tid.len() == 36);
    assert_eq!(task["status"], "queued");
    assert_eq!(task["priority"], "interactive");
    assert_eq!(task["source"], "slack");
    assert_eq!(task["attempts"], 0);
    assert_eq!(
        task["slack"],
        json!({"channel": "D0ANA", "thread_ts": "1731.0001"})
    );
    assert_eq!(task["result"], Value::Null);

    let (status, claim) = env.claim().await;
    assert_eq!(status, 200, "{claim}");
    assert_eq!(claim["task"]["id"], tid.as_str());
    assert_eq!(claim["task"]["status"], "working");
    assert_eq!(claim["task"]["attempts"], 1);
    assert_eq!(claim["bot"]["name"], "docs");
    assert_eq!(claim["role"]["name"], "docs-helper");
    assert_eq!(claim["tools"], json!(["docs.read", "docs.search"]));
    assert_eq!(claim["session_token"], "pmos_s_test1");
    assert!(claim["lease_until"].is_string());
    assert_eq!(
        claim["gateway"],
        json!({"model": "pmos/default", "headers": {
            "X-Pmos-User": "ana", "X-Pmos-Run": tid, "X-Pmos-Priority": "interactive"}})
    );
    assert_eq!(
        env.ts.sessions.lock().unwrap().clone(),
        vec![json!({"user": "ana", "bot": "docs", "run_id": tid})],
        "a bot session for the task, as the PM"
    );

    let w = |p: &str| format!("/v1/worker/tasks/{tid}/{p}");
    let (status, _) = env
        .post(
            &w("events"),
            "worker",
            None,
            json!({"kind": "status", "text": "Working on it"}),
        )
        .await;
    assert_eq!(status, 200);
    let (status, _) = env
        .post(
            &w("events"),
            "worker",
            None,
            json!({"kind": "checklist", "checklist": [
                {"item": "Search the docs", "done": true}, {"item": "Answer", "done": false}]}),
        )
        .await;
    assert_eq!(status, 200);
    let (status, _) = env
        .post(
            &w("events"),
            "worker",
            None,
            json!({"kind": "milestone", "text": "Searching the support docs", "worker": "w1"}),
        )
        .await;
    assert_eq!(status, 200);
    let (status, hb) = env.post(&w("heartbeat"), "worker", None, json!({})).await;
    assert_eq!(status, 200);
    assert!(hb["lease_until"].is_string());
    // An empty heartbeat body is fine too.
    let res = env
        .req(Method::POST, &w("heartbeat"), "worker", None)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let (status, rec) = env
        .post(
            &w("trace"),
            "worker",
            None,
            json!({"entries": [
                {"kind": "prompt", "body": {"messages": [{"role": "user", "content": "What ports?"}]}},
                {"kind": "tool_call", "body": {"tool": "docs.search", "args": {"query": "ports"}}},
                {"kind": "tool_result", "body": {"tool": "docs.search", "result": ["page"]}},
                {"kind": "model_reply", "body": {"content": "Port 443."}},
            ]}),
        )
        .await;
    assert_eq!((status, rec["recorded"].as_u64()), (200, Some(4)));
    let (status, done) = env
        .post(
            &w("complete"),
            "worker",
            None,
            json!({"result": "Acme Vault needs port 443.", "approvals": []}),
        )
        .await;
    assert_eq!(status, 200, "{done}");
    assert_eq!(done["status"], "done");

    let (status, got) = env
        .get(&format!("/v1/tasks/{tid}"), "slack", Some("ana"))
        .await;
    assert_eq!(status, 200);
    assert_eq!(got["result"], "Acme Vault needs port 443.");
    assert_eq!(
        got["checklist"][0],
        json!({"item": "Search the docs", "done": true})
    );
    assert!(got.get("lease_until").is_none(), "no lease once finished");

    let (_, events) = env
        .get(&format!("/v1/tasks/{tid}/events"), "slack", Some("ana"))
        .await;
    let kinds: Vec<(&str, &str)> = events
        .as_array()
        .unwrap()
        .iter()
        .map(|e| (e["kind"].as_str().unwrap(), e["text"].as_str().unwrap()))
        .collect();
    assert_eq!(
        kinds,
        vec![
            ("status", "queued"),
            ("status", "claimed by worker w1"),
            ("status", "Working on it"),
            ("checklist", "1 of 2 done"),
            ("milestone", "Searching the support docs"),
            ("status", "done"),
        ]
    );
    let (status, trace) = env
        .get(&format!("/v1/tasks/{tid}/trace"), "console", None)
        .await;
    assert_eq!(status, 200);
    assert_eq!(trace.as_array().unwrap().len(), 4);
    assert_eq!(trace[1]["kind"], "tool_call");
    assert_eq!(trace[1]["body"]["tool"], "docs.search");

    let feed = env.feed().await;
    assert_eq!(feed.len(), 2);
    assert_eq!(feed[0]["kind"], "milestone");
    assert_eq!(feed[0]["text"], "Searching the support docs");
    assert_eq!(feed[0]["task_id"], tid.as_str());
    assert_eq!(feed[0]["bot"], "docs");
    assert_eq!(feed[0]["requested_by"], "ana");
    assert_eq!(feed[0]["slack"]["channel"], "D0ANA");
    assert_eq!(feed[1]["kind"], "done");
    assert_eq!(feed[1]["text"], "Acme Vault needs port 443.");

    // Nothing else is queued.
    let (status, _) = env.claim().await;
    assert_eq!(status, 204);
    // A finished task can't be finished again.
    let (status, again) = env
        .post(&w("complete"), "worker", None, json!({"result": "again"}))
        .await;
    assert_eq!(status, 409);
    assert_eq!(again["status"], "done");
}

async fn results_with_approvals_wait_until_every_item_is_decided(env: &Env) {
    let (status, task) = env
        .post(
            "/v1/tasks",
            "slack",
            Some("ana"),
            json!({"bot": "pm-ana", "requested_by": "ana", "text": "File the bug", "source": "slack"}),
        )
        .await;
    assert_eq!(status, 201);
    let tid = id(&task);
    let (status, claim) = env.claim().await;
    assert_eq!(status, 200);
    assert_eq!(
        claim["gateway"]["model"], "pmos/extraction",
        "the role's model"
    );
    let (status, t) = env
        .post(
            &format!("/v1/worker/tasks/{tid}/complete"),
            "worker",
            None,
            json!({"result": "Drafted two items.", "approvals": ["apr_1", "apr_2", "apr_1"]}),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(t["status"], "needs_approval");
    assert_eq!(t["approvals"], json!(["apr_1", "apr_2"]));
    let feed = env.feed().await;
    assert_eq!(feed.last().unwrap()["kind"], "needs_approval");
    assert_eq!(feed.last().unwrap()["approvals"], json!(["apr_1", "apr_2"]));

    env.ts
        .set_approval("apr_1", "jira.create_issue", "approved", Some("ana"));
    env.ts
        .set_approval("apr_2", "slack.send_message", "pending", None);
    let now = pmos_bots::model::now();
    assert_eq!(env.rt.check_approvals(now).await.unwrap(), 0);
    env.ts
        .set_approval("apr_2", "slack.send_message", "executing", Some("ana"));
    assert_eq!(
        env.rt.check_approvals(now).await.unwrap(),
        0,
        "still writing"
    );
    env.ts
        .set_approval("apr_2", "slack.send_message", "rejected", Some("ana"));
    assert_eq!(env.rt.check_approvals(now).await.unwrap(), 1);
    assert_eq!(env.rt.check_approvals(now).await.unwrap(), 0, "closed once");

    let (_, t) = env.get(&format!("/v1/tasks/{tid}"), "console", None).await;
    assert_eq!(t["status"], "done");
    assert_eq!(t["result"], "Drafted two items.");
    let last = env.feed().await.last().unwrap().clone();
    assert_eq!(last["kind"], "approval_closed");
    assert_eq!(
        last["text"],
        "jira.create_issue was approved by ana; slack.send_message was rejected by ana."
    );
    assert_eq!(last["approvals"], json!(["apr_1", "apr_2"]));
    assert_eq!(last["outcomes"][1]["status"], "rejected");
}

/// A Slack draft nobody decided within a day is `expired`: a closed state, so the task
/// settles when its last item expires, and Slack hears `approval_closed`.
async fn a_task_settles_when_its_last_approval_expires(env: &Env) {
    let task = env.ask("Draft my replies").await;
    let tid = id(&task);
    env.claim().await;
    env.post(
        &format!("/v1/worker/tasks/{tid}/complete"),
        "worker",
        None,
        json!({"result": "Drafted two replies.", "approvals": ["apr_r1", "apr_r2"]}),
    )
    .await;
    env.ts
        .set_approval("apr_r1", "slack.post_reply", "approved", Some("ana"));
    env.ts
        .set_approval("apr_r2", "slack.post_reply", "pending", None);
    let now = pmos_bots::model::now();
    assert_eq!(
        env.rt.check_approvals(now).await.unwrap(),
        0,
        "still waiting"
    );
    env.ts
        .set_approval("apr_r2", "slack.post_reply", "expired", None);
    assert_eq!(env.rt.check_approvals(now).await.unwrap(), 1);
    assert_eq!(env.rt.check_approvals(now).await.unwrap(), 0, "closed once");

    let (_, t) = env.get(&format!("/v1/tasks/{tid}"), "console", None).await;
    assert_eq!(t["status"], "done");
    let last = env.feed().await.last().unwrap().clone();
    assert_eq!(last["kind"], "approval_closed");
    assert_eq!(
        last["text"],
        "slack.post_reply was approved by ana; slack.post_reply expired undecided; nothing was sent."
    );
    assert_eq!(last["outcomes"][1]["status"], "expired");
    assert_eq!(last["outcomes"][1]["decided_by"], Value::Null);
}

async fn approvals_the_tool_server_lost_still_close(env: &Env) {
    let task = env.ask("Anything").await;
    env.claim().await;
    env.post(
        &format!("/v1/worker/tasks/{}/complete", id(&task)),
        "worker",
        None,
        json!({"result": "r", "approvals": ["apr_gone"]}),
    )
    .await;
    assert_eq!(
        env.rt
            .check_approvals(pmos_bots::model::now())
            .await
            .unwrap(),
        1
    );
    let last = env.feed().await.last().unwrap().clone();
    assert_eq!(last["text"], "apr_gone is no longer in the approval queue.");
}

/// Two workers claiming at once through the API: one gets the task, the other `204`.
async fn two_workers_one_task_one_claim(env: &Env) {
    for _ in 0..5 {
        env.ask("race").await;
        let (a, b) = tokio::join!(
            env.post("/v1/worker/claim", "worker", None, json!({"worker": "w1"})),
            env.post("/v1/worker/claim", "worker", None, json!({"worker": "w2"})),
        );
        let mut statuses = [a.0, b.0];
        statuses.sort();
        assert_eq!(statuses, [200, 204]);
    }
}

async fn workers_are_held_to_their_claim(env: &Env) {
    let task = env.ask("Q").await;
    let tid = id(&task);
    let w = |p: &str| format!("/v1/worker/tasks/{tid}/{p}");
    let (status, body) = env
        .post(
            &w("events"),
            "worker",
            None,
            json!({"kind": "milestone", "text": "x"}),
        )
        .await;
    assert_eq!((status, body["status"].as_str()), (409, Some("queued")));
    let (status, _) = env
        .post(&w("trace"), "worker", None, json!({"entries": []}))
        .await;
    assert_eq!(status, 409, "no trace before a claim");
    env.claim().await;
    let (status, body) = env
        .post(&w("heartbeat"), "worker", None, json!({"worker": "w2"}))
        .await;
    assert_eq!(status, 409);
    assert!(body["error"].as_str().unwrap().contains("another worker"));
    let (status, _) = env
        .post(&w("heartbeat"), "worker", None, json!({"worker": "w1"}))
        .await;
    assert_eq!(status, 200);

    // Cancelled from Slack: the worker's next call says so.
    let (status, t) = env
        .post(
            &format!("/v1/tasks/{tid}/cancel"),
            "slack",
            Some("ana"),
            json!({}),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(t["status"], "cancelled");
    let (status, body) = env.post(&w("heartbeat"), "worker", None, json!({})).await;
    assert_eq!((status, body["status"].as_str()), (409, Some("cancelled")));
    let (status, _) = env
        .post(&w("fail"), "worker", None, json!({"reason": "late"}))
        .await;
    assert_eq!(status, 409);
    let (status, _) = env
        .post(
            &format!("/v1/tasks/{tid}/cancel"),
            "slack",
            Some("ana"),
            json!({}),
        )
        .await;
    assert_eq!(status, 409, "already cancelled");
    // The console cancels only signed for a viewer (R1), in every mode.
    let (status, _) = env
        .post(
            &format!("/v1/tasks/{tid}/cancel"),
            "console",
            None,
            json!({}),
        )
        .await;
    assert_eq!(status, 403, "an unsigned console cancel");
    let (status, _) = env
        .post(
            "/v1/worker/tasks/tsk_nope/heartbeat",
            "worker",
            None,
            json!({}),
        )
        .await;
    assert_eq!(status, 404);
}

async fn a_failed_task_says_why(env: &Env) {
    let task = env.ask("Q").await;
    env.claim().await;
    let path = format!("/v1/worker/tasks/{}/fail", id(&task));
    let (status, _) = env
        .post(&path, "worker", None, json!({"reason": "  "}))
        .await;
    assert_eq!(status, 400);
    let (status, t) = env
        .post(
            &path,
            "worker",
            None,
            json!({"reason": "The docs search is down."}),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(t["status"], "failed");
    assert_eq!(t["error"], "The docs search is down.");
    let last = env.feed().await.last().unwrap().clone();
    assert_eq!(
        (last["kind"].as_str(), last["text"].as_str()),
        (Some("failed"), Some("The docs search is down."))
    );
}

both_envs!(
    a_cursor_from_another_epoch_is_refused,
    a_task_runs_from_slack_to_a_result,
    results_with_approvals_wait_until_every_item_is_decided,
    a_task_settles_when_its_last_approval_expires,
    approvals_the_tool_server_lost_still_close,
    two_workers_one_task_one_claim,
    workers_are_held_to_their_claim,
    a_failed_task_says_why,
    routines_are_managed_by_their_owner,
    a_bots_own_worker_touches_only_that_bots_tasks,
    drafts_are_read_from_the_tool_server_for_console_and_evals,
    eval_tasks_run_as_the_benchmark_member_and_never_reach_slack,
);

/// The evals client (spec §3, S3): only as a handle in `PMOS_EVALS_AS` (ben, in these
/// tests), with no Slack target, and nothing it does reaches the Slack feed.
async fn eval_tasks_run_as_the_benchmark_member_and_never_reach_slack(env: &Env) {
    let new =
        |user: &str| json!({"bot": "docs", "requested_by": user, "text": "Q", "source": "eval"});
    let (status, body) = env.post("/v1/tasks", "evals", None, new("ana")).await;
    assert_eq!(status, 403, "ana is a PM, not the benchmark member: {body}");
    assert!(
        body["error"].as_str().unwrap().contains("PMOS_EVALS_AS"),
        "{body}"
    );
    let mut with_slack = new("ben");
    with_slack["slack"] = json!({"channel": "C0TEAM", "thread_ts": "1731.1"});
    let (status, body) = env.post("/v1/tasks", "evals", None, with_slack).await;
    assert_eq!(status, 400, "an eval task has no Slack target: {body}");

    let (status, task) = env.post("/v1/tasks", "evals", None, new("ben")).await;
    assert_eq!(status, 201, "{task}");
    let tid = id(&task);
    let (status, _) = env.claim().await;
    assert_eq!(status, 200);
    let (status, _) = env
        .post(
            &format!("/v1/worker/tasks/{tid}/events"),
            "worker",
            None,
            json!({"kind": "milestone", "text": "Reading"}),
        )
        .await;
    assert_eq!(status, 200);
    let (status, done) = env
        .post(
            &format!("/v1/worker/tasks/{tid}/complete"),
            "worker",
            None,
            json!({"result": "The answer."}),
        )
        .await;
    assert_eq!(status, 200, "{done}");
    let (_, task) = env.get(&format!("/v1/tasks/{tid}"), "evals", None).await;
    assert_eq!(task["status"], "done");
    assert!(
        env.feed()
            .await
            .iter()
            .all(|n| n["task_id"] != tid.as_str()),
        "no milestone or done notification for an eval task"
    );
}

/// `GET /v1/tasks/{id}/drafts` (`docs/improvement-loop-spec.md` §3): the runtime reads each
/// approval item from the tool server with its own service token.
async fn drafts_are_read_from_the_tool_server_for_console_and_evals(env: &Env) {
    let (status, task) = env
        .post(
            "/v1/tasks",
            "evals",
            None,
            json!({"bot": "docs", "requested_by": "ben", "text": "File it", "source": "eval"}),
        )
        .await;
    assert_eq!(status, 201);
    let tid = id(&task);
    let path = format!("/v1/tasks/{tid}/drafts");

    // Nothing drafted yet.
    let (status, body) = env.get(&path, "evals", None).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body, json!({"drafts": []}));

    let (status, _) = env.claim().await;
    assert_eq!(status, 200);
    env.post(
        &format!("/v1/worker/tasks/{tid}/complete"),
        "worker",
        None,
        json!({"result": "Drafted.", "approvals": ["apr_2", "apr_1", "apr_gone"]}),
    )
    .await;
    env.ts.approvals.lock().unwrap().insert(
        "apr_1".into(),
        json!({"id": "apr_1", "user": "ben", "bot": "docs", "run_id": tid,
            "tool": "jira.create_issue", "args": {"project": "PM", "summary": "SCIM"},
            "args_hash": "h", "status": "pending", "created_at": "2026-11-14T10:00:00Z"}),
    );
    env.ts
        .set_approval("apr_2", "slack.send_message", "rejected", Some("ben"));

    let reads = *env.ts.approval_reads.lock().unwrap();
    let (status, body) = env.get(&path, "evals", None).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body,
        json!({"drafts": [
            {"id": "apr_2", "tool": "slack.send_message", "args": {}, "status": "rejected"},
            {"id": "apr_1", "tool": "jira.create_issue",
             "args": {"project": "PM", "summary": "SCIM"}, "status": "pending"},
            {"id": "apr_gone", "tool": null, "args": null, "status": "missing"},
        ]}),
        "in the worker's order; an item the tool server lost reads missing"
    );
    assert_eq!(
        *env.ts.approval_reads.lock().unwrap(),
        reads + 3,
        "fetched from the tool server with the service token"
    );
    let (status, console) = env.get(&path, "console", None).await;
    assert_eq!(status, 200);
    assert_eq!(console, body);

    // Slack and workers may not read drafts; evals sees only eval tasks.
    let (status, _) = env.get(&path, "slack", Some("ben")).await;
    assert_eq!(status, 403);
    let (status, _) = env.get(&path, "worker", None).await;
    assert_eq!(status, 403);
    let slack_task = env.ask("Not an eval").await;
    let (status, _) = env
        .get(
            &format!("/v1/tasks/{}/drafts", id(&slack_task)),
            "evals",
            None,
        )
        .await;
    assert_eq!(status, 404);
    let (status, _) = env
        .get(
            &format!("/v1/tasks/{}/drafts", id(&slack_task)),
            "console",
            None,
        )
        .await;
    assert_eq!(status, 200);
    let (status, _) = env.get("/v1/tasks/tsk_nope/drafts", "console", None).await;
    assert_eq!(status, 404);

    // The tool server down: try again later, not an empty list.
    *env.ts.approvals_down.lock().unwrap() = true;
    let (status, body) = env.get(&path, "evals", None).await;
    assert_eq!(status, 503, "{body}");
    *env.ts.approvals_down.lock().unwrap() = false;
}

/// Each bot's sandbox holds its own worker token (`worker-docs`), so a compromised Docs-bot
/// sandbox can't claim Ana's personal-bot task and receive its restricted session.
async fn a_bots_own_worker_touches_only_that_bots_tasks(env: &Env) {
    let (status, personal) = env
        .post(
            "/v1/tasks",
            "slack",
            Some("ana"),
            json!({"bot": "pm-ana", "requested_by": "ana", "text": "draft my PRD", "source": "slack"}),
        )
        .await;
    assert_eq!(status, 201, "{personal}");
    let personal_id = id(&personal);

    // Asking for another bot's tasks is refused; asking for nothing gets only its own (none yet).
    let (status, _) = env
        .post(
            "/v1/worker/claim",
            "worker-docs",
            None,
            json!({"worker": "docs-1", "bots": ["pm-ana"]}),
        )
        .await;
    assert_eq!(status, 403);
    let (status, _) = env
        .post(
            "/v1/worker/claim",
            "worker-docs",
            None,
            json!({"worker": "docs-1"}),
        )
        .await;
    assert_eq!(status, 204, "the queued task is pm-ana's, not docs'");

    // Its own bot's task, it gets.
    let docs_task = id(&env.ask("What ports does Acme Vault need?").await);
    let (status, claim) = env
        .post(
            "/v1/worker/claim",
            "worker-docs",
            None,
            json!({"worker": "docs-1"}),
        )
        .await;
    assert_eq!(status, 200, "{claim}");
    assert_eq!(claim["task"]["id"], docs_task.as_str());

    // Ana's task is claimed by a worker that may serve it; the Docs worker can't touch it.
    let (status, claim) = env
        .post(
            "/v1/worker/claim",
            "worker",
            None,
            json!({"worker": "any-1", "bots": ["pm-ana"]}),
        )
        .await;
    assert_eq!(status, 200, "{claim}");
    for (path, body) in [
        ("heartbeat", json!({})),
        ("events", json!({"kind": "milestone", "text": "x"})),
        ("trace", json!({"entries": []})),
        ("complete", json!({"result": "stolen", "approvals": []})),
        ("fail", json!({"reason": "x"})),
    ] {
        let (status, _) = env
            .post(
                &format!("/v1/worker/tasks/{personal_id}/{path}"),
                "worker-docs",
                None,
                body,
            )
            .await;
        assert_eq!(status, 404, "{path}");
    }
}

// ----- task creation rules -----------------------------------------------------------------

#[tokio::test]
async fn only_people_who_may_use_a_bot_can_ask_it() {
    let env = Env::memory().await;
    let new = |bot: &str, user: &str, source: &str| json!({"bot": bot, "requested_by": user, "text": "hello", "source": source});
    // A team bot serves the users who may ask; eve isn't in the directory.
    let (status, body) = env
        .post(
            "/v1/tasks",
            "slack",
            Some("eve"),
            new("docs", "eve", "slack"),
        )
        .await;
    assert_eq!(status, 403);
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("`eve` isn't a PM OS user"),
        "{body}"
    );
    // A personal bot serves its owner only.
    let (status, body) = env
        .post(
            "/v1/tasks",
            "slack",
            Some("ben"),
            new("pm-ana", "ben", "slack"),
        )
        .await;
    assert_eq!(status, 403);
    assert!(body["error"].as_str().unwrap().contains("personal bot"));
    let (status, _) = env
        .post("/v1/tasks", "slack", Some("ben"), new("docs", "ben", "cli"))
        .await;
    assert_eq!(status, 201, "the CLI is interactive too");
    let (status, _) = env
        .post("/v1/tasks", "evals", None, new("pm-ana", "ben", "eval"))
        .await;
    assert_eq!(status, 403, "evals are held to the same rule");
    let (status, t) = env
        .post("/v1/tasks", "evals", None, new("docs", "ben", "eval"))
        .await;
    assert_eq!(status, 201);
    assert_eq!(t["priority"], "personal");
    let (status, _) = env
        .post(
            "/v1/tasks",
            "slack",
            Some("ana"),
            new("nobody", "ana", "slack"),
        )
        .await;
    assert_eq!(status, 404);
    let (status, _) = env
        .post(
            "/v1/tasks",
            "slack",
            Some("ana"),
            json!({"bot": "docs", "requested_by": "ana", "text": "  ", "source": "slack"}),
        )
        .await;
    assert_eq!(status, 400);
    let (status, body) = env
        .post("/v1/tasks", "slack", Some("ana"), json!({"bot": "docs"}))
        .await;
    assert_eq!(
        status, 400,
        "a body that doesn't fit is 400, not 422: {body}"
    );
    let (status, _) = env
        .post(
            "/v1/tasks",
            "slack",
            Some("ana"),
            new("docs", "ana", "routine"),
        )
        .await;
    assert_eq!(status, 403, "routine tasks come from the scheduler only");
    env.finish().await;
}

#[tokio::test]
async fn tasks_are_listed_by_who_may_see_them() {
    let env = Env::memory().await;
    let ana = env.ask("ana's").await;
    let (_, ben) = env
        .post(
            "/v1/tasks",
            "slack",
            Some("ben"),
            json!({"bot": "docs", "requested_by": "ben", "text": "ben's", "source": "slack"}),
        )
        .await;
    let (_, eval) = env
        .post(
            "/v1/tasks",
            "evals",
            None,
            json!({"bot": "docs", "requested_by": "ben", "text": "eval", "source": "eval"}),
        )
        .await;
    let ids = |v: &Value| -> Vec<String> { v.as_array().unwrap().iter().map(id).collect() };
    let (_, list) = env.get("/v1/tasks", "slack", Some("ana")).await;
    assert_eq!(ids(&list), vec![id(&ana)]);
    let (status, _) = env
        .get("/v1/tasks?requested_by=ben", "slack", Some("ana"))
        .await;
    assert_eq!(status, 403);
    let (_, list) = env.get("/v1/tasks", "evals", None).await;
    assert_eq!(ids(&list), vec![id(&eval)]);
    let (_, list) = env
        .get("/v1/tasks?requested_by=ben&limit=1", "console", None)
        .await;
    assert_eq!(ids(&list), vec![id(&eval)]);
    let (_, list) = env
        .get("/v1/tasks?bot=docs&status=queued", "console", None)
        .await;
    assert_eq!(list.as_array().unwrap().len(), 3);
    let (status, _) = env.get("/v1/tasks?status=bogus", "console", None).await;
    assert_eq!(status, 400);
    // Someone else's task looks like no task.
    let (status, _) = env
        .get(&format!("/v1/tasks/{}", id(&ben)), "slack", Some("ana"))
        .await;
    assert_eq!(status, 404);
    let (status, _) = env
        .get(&format!("/v1/tasks/{}", id(&ana)), "evals", None)
        .await;
    assert_eq!(status, 404);
    let (status, _) = env
        .post(
            &format!("/v1/tasks/{}/cancel", id(&ben)),
            "slack",
            Some("ana"),
            json!({}),
        )
        .await;
    assert_eq!(status, 404);
    let (status, _) = env
        .get(&format!("/v1/tasks/{}/events", id(&eval)), "evals", None)
        .await;
    assert_eq!(status, 200);
    env.finish().await;
}

#[tokio::test]
async fn web_tasks_belong_to_their_pm_and_stay_off_the_slack_feed() {
    let env = Env::memory().await;
    let new =
        |text: &str| json!({"bot": "docs", "requested_by": "ana", "text": text, "source": "web"});
    let (status, task) = env
        .post("/v1/tasks", "web", Some("ana"), new("from web"))
        .await;
    assert_eq!(status, 201);
    assert_eq!(task["source"], "web");
    assert_eq!(task["priority"], "interactive");
    assert!(task["slack"].is_null());
    let tid = id(&task);
    let (_, ben) = env
        .post(
            "/v1/tasks",
            "web",
            Some("ben"),
            json!({"bot": "docs", "requested_by": "ben", "text": "ben's", "source": "web"}),
        )
        .await;
    let (_, list) = env.get("/v1/tasks", "web", Some("ana")).await;
    let ids: Vec<String> = list.as_array().unwrap().iter().map(id).collect();
    assert_eq!(ids, vec![tid.clone()]);
    let (status, got) = env
        .get(&format!("/v1/tasks/{tid}"), "web", Some("ana"))
        .await;
    assert_eq!((status, got["source"].clone()), (200, json!("web")));
    // Slack's PM view of the same handle sees it too; another PM's task is a 404.
    let (status, _) = env
        .get(&format!("/v1/tasks/{tid}"), "slack", Some("ana"))
        .await;
    assert_eq!(status, 200);
    for path in [
        format!("/v1/tasks/{}", id(&ben)),
        format!("/v1/tasks/{}/events", id(&ben)),
    ] {
        let (status, _) = env.get(&path, "web", Some("ana")).await;
        assert_eq!(status, 404, "{path}");
    }
    let (status, _) = env
        .post(
            &format!("/v1/tasks/{}/cancel", id(&ben)),
            "web",
            Some("ana"),
            json!({}),
        )
        .await;
    assert_eq!(status, 404);
    let (status, _) = env
        .get(&format!("/v1/tasks/{tid}/events"), "web", Some("ana"))
        .await;
    assert_eq!(status, 200);
    let (status, _) = env
        .post(
            &format!("/v1/tasks/{tid}/cancel"),
            "web",
            Some("ana"),
            json!({}),
        )
        .await;
    assert_eq!(status, 200);
    // Cancelling and creating a web task wrote nothing to the Slack feed.
    assert!(env.feed().await.is_empty());
    env.finish().await;
}

// ----- claims and the tool server ----------------------------------------------------------

#[tokio::test]
async fn a_refused_session_fails_the_task_and_a_down_tool_server_requeues_it() {
    let env = Env::memory().await;
    let task = env.ask("Q").await;
    let tid = id(&task);

    *env.ts.mode.lock().unwrap() = SessionMode::Down;
    let (status, body) = env.claim().await;
    assert_eq!(status, 503, "{body}");
    let (_, t) = env.get(&format!("/v1/tasks/{tid}"), "console", None).await;
    assert_eq!(t["status"], "queued");
    assert_eq!(t["attempts"], 0, "an outage doesn't use up an attempt");

    *env.ts.mode.lock().unwrap() = SessionMode::Refuse;
    let (status, _) = env.claim().await;
    assert_eq!(status, 204, "the refused task failed; nothing else queued");
    let (_, t) = env.get(&format!("/v1/tasks/{tid}"), "console", None).await;
    assert_eq!(t["status"], "failed");
    assert_eq!(
        t["error"],
        "the tool server wouldn't start this bot's session: pm-ana is ana's personal bot"
    );
    assert_eq!(env.feed().await.last().unwrap()["kind"], "failed");

    *env.ts.mode.lock().unwrap() = SessionMode::Ok;
    env.ask("next").await;
    let (status, claim) = env.claim().await;
    assert_eq!(status, 200);
    assert_eq!(claim["task"]["attempts"], 1);
    env.finish().await;
}

#[tokio::test]
async fn claims_long_poll_for_new_work() {
    let env = Env::memory().await;
    // Nothing queued: waits the full time, then 204.
    let started = Instant::now();
    let (status, _) = env
        .post(
            "/v1/worker/claim",
            "worker",
            None,
            json!({"worker": "w1", "wait_ms": 300}),
        )
        .await;
    assert_eq!(status, 204);
    assert!(started.elapsed() >= Duration::from_millis(300));

    // A task that arrives while waiting is handed over at once.
    let started = Instant::now();
    let waiting = env.post(
        "/v1/worker/claim",
        "worker",
        None,
        json!({"worker": "w1", "bots": ["docs"], "wait_ms": 10_000}),
    );
    let asking = async {
        tokio::time::sleep(Duration::from_millis(150)).await;
        env.ask("late").await
    };
    let ((status, claim), task) = tokio::join!(waiting, asking);
    assert_eq!(status, 200);
    assert_eq!(claim["task"]["id"], task["id"]);
    assert!(started.elapsed() < Duration::from_secs(5));

    // A worker for another bot doesn't take it.
    env.ask("for docs").await;
    let (status, _) = env
        .post(
            "/v1/worker/claim",
            "worker",
            None,
            json!({"worker": "w9", "bots": ["pm-ana"], "wait_ms": 100}),
        )
        .await;
    assert_eq!(status, 204);
    let (status, _) = env
        .post("/v1/worker/claim", "worker", None, json!({"worker": " "}))
        .await;
    assert_eq!(status, 400);
    env.finish().await;
}

#[tokio::test]
async fn long_polls_end_on_shutdown() {
    let env = Env::memory().await;
    let started = Instant::now();
    let waiting = env.post(
        "/v1/worker/claim",
        "worker",
        None,
        json!({"worker": "w1", "wait_ms": 25_000}),
    );
    let feed = env.get("/v1/notifications?wait_ms=25000", "slack", Some("ana"));
    let stop = async {
        tokio::time::sleep(Duration::from_millis(150)).await;
        env.rt.stop.cancel();
    };
    let ((claim, _), (feed, body), ()) = tokio::join!(waiting, feed, stop);
    assert_eq!(claim, 204);
    assert_eq!(feed, 200);
    assert_eq!(body["items"], json!([]));
    assert!(started.elapsed() < Duration::from_secs(5));
    env.finish().await;
}

// ----- notifications ----------------------------------------------------------------------

#[tokio::test]
async fn the_feed_long_polls_and_resumes_from_a_cursor() {
    let env = Env::memory().await;
    let (status, empty) = env.get("/v1/notifications", "slack", Some("ana")).await;
    assert_eq!(status, 200);
    assert_eq!(empty, json!({"cursor": "0", "items": [], "head": "0"}));

    let task = env.ask("Q").await;
    env.claim().await;
    let events = format!("/v1/worker/tasks/{}/events", id(&task));
    let waiting = env.get(
        "/v1/notifications?after=0&wait_ms=10000",
        "slack",
        Some("ana"),
    );
    let milestone = async {
        tokio::time::sleep(Duration::from_millis(150)).await;
        env.post(
            &events,
            "worker",
            None,
            json!({"kind": "milestone", "text": "Reading Jira"}),
        )
        .await
    };
    let started = Instant::now();
    let ((status, page), _) = tokio::join!(waiting, milestone);
    assert_eq!(status, 200);
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
    assert_eq!(page["items"][0]["text"], "Reading Jira");
    let cursor = page["cursor"].as_str().unwrap().to_string();

    env.post(
        &events,
        "worker",
        None,
        json!({"kind": "milestone", "text": "Drafting"}),
    )
    .await;
    let (_, next) = env
        .get(
            &format!("/v1/notifications?after={cursor}"),
            "slack",
            Some("ana"),
        )
        .await;
    assert_eq!(next["items"].as_array().unwrap().len(), 1);
    assert_eq!(next["items"][0]["text"], "Drafting");
    let (_, none) = env
        .get(
            &format!(
                "/v1/notifications?after={}&wait_ms=100",
                next["cursor"].as_str().unwrap()
            ),
            "slack",
            Some("ana"),
        )
        .await;
    assert_eq!(none["items"], json!([]));
    assert_eq!(none["cursor"], next["cursor"], "the cursor stays put");
    let (status, _) = env
        .get("/v1/notifications?after=abc", "slack", Some("ana"))
        .await;
    assert_eq!(status, 400);
    env.finish().await;
}

/// A cursor past the feed's head came from another epoch of the feed (an in-memory runtime
/// that restarted, or a reset database). It is refused with `409 cursor_ahead` and the head,
/// instead of answering "nothing new" until the sequence catches up, which silently lost
/// every reply in between. Every case runs on both stores.
async fn a_cursor_from_another_epoch_is_refused(env: &Env) {
    let (_, empty) = env.get("/v1/notifications", "slack", Some("ana")).await;
    assert_eq!(empty["head"], "0");
    // A cursor the Slack service saved from the runtime before it restarted.
    let (status, body) = env
        .get("/v1/notifications?after=57", "slack", Some("ana"))
        .await;
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["code"], "cursor_ahead");
    assert_eq!(body["head"], "0");
    assert!(body["error"].as_str().unwrap().starts_with("cursor_ahead"));

    // New notifications arrive: the head moves, and a cursor at or below it is fine.
    let task = env.ask("Q").await;
    env.claim().await;
    env.post(
        &format!("/v1/worker/tasks/{}/events", id(&task)),
        "worker",
        None,
        json!({"kind": "milestone", "text": "Reading Jira"}),
    )
    .await;
    let (status, page) = env
        .get("/v1/notifications?after=0", "slack", Some("ana"))
        .await;
    assert_eq!(status, 200);
    let head: i64 = page["head"].as_str().unwrap().parse().unwrap();
    assert!(head >= 1);
    assert_eq!(page["cursor"], page["head"]);
    let (status, _) = env
        .get(
            &format!("/v1/notifications?after={head}"),
            "slack",
            Some("ana"),
        )
        .await;
    assert_eq!(status, 200);
    let (status, body) = env
        .get(
            &format!("/v1/notifications?after={}", head + 1),
            "slack",
            Some("ana"),
        )
        .await;
    assert_eq!(status, 409, "{body}");
    // Pruning never lowers the head: a cursor from before a prune is still this feed's.
    env.rt
        .housekeeping(chrono::Utc::now() + chrono::Duration::days(60))
        .await
        .unwrap();
    let (status, page) = env
        .get(
            &format!("/v1/notifications?after={head}"),
            "slack",
            Some("ana"),
        )
        .await;
    assert_eq!(status, 200, "{page}");
    assert_eq!(page["head"], head.to_string());
}

// ----- routines ---------------------------------------------------------------------------

const GOOD_REPLY: &str = r#"{"schedule": {"cron": "0 8 * * 1-5", "tz": "Europe/London"},
  "prompt": "Summarize overnight #launch and draft replies to anything addressed to me",
  "output": {"kind": "dm"}, "approval": "approve"}"#;

#[tokio::test]
async fn routines_are_parsed_by_the_model_and_checked_in_code() {
    let env = Env::memory().await;
    *env.gw.reply.lock().unwrap() = GOOD_REPLY.into();
    let ask = json!({"bot": "pm-ana", "owner": "ana", "tz": "Europe/London",
        "text": "every weekday at 8am summarize overnight #launch and draft replies to anything addressed to me"});
    let (status, body) = env
        .post("/v1/routines/parse", "slack", Some("ana"), ask.clone())
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body["draft"],
        json!({"bot": "pm-ana", "owner": "ana",
            "schedule": {"cron": "0 8 * * 1-5", "tz": "Europe/London"},
            "prompt": "Summarize overnight #launch and draft replies to anything addressed to me",
            "output": {"kind": "dm"}, "approval": "approve"})
    );
    assert_eq!(
        body["summary"],
        "Every weekday at 8:00 London time, pm-ana will summarize overnight #launch and draft replies to anything addressed to me."
    );
    {
        let seen = env.gw.seen.lock().unwrap();
        let (headers, req) = seen.last().unwrap();
        assert_eq!(req["model"], "pmos/extraction");
        assert_eq!(req["response_format"], json!({"type": "json_object"}));
        assert_eq!(req["messages"][1]["content"], ask["text"]);
        assert!(req["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("Europe/London"));
        assert_eq!(headers.get("x-pmos-user").map(String::as_str), Some("ana"));
        assert_eq!(
            headers.get("x-pmos-priority").map(String::as_str),
            Some("interactive")
        );
    }

    // The model tries to widen what the routine may do: ignored.
    *env.gw.reply.lock().unwrap() =
        r#"{"schedule": {"cron": "0 8 * * 1-5"}, "prompt": "Post the summary",
        "approval": "auto", "bot": "docs", "owner": "eve"}"#
            .into();
    let (status, body) = env
        .post("/v1/routines/parse", "slack", Some("ana"), ask.clone())
        .await;
    assert_eq!(status, 200);
    assert_eq!(body["draft"]["approval"], "approve");
    assert_eq!(body["draft"]["bot"], "pm-ana");
    assert_eq!(body["draft"]["owner"], "ana");
    assert_eq!(
        body["draft"]["schedule"]["tz"], "Europe/London",
        "the PM's zone by default"
    );

    // Not JSON at all: a question back, not an error.
    *env.gw.reply.lock().unwrap() = "Sure! I'll set that up for you.".into();
    let (status, body) = env
        .post("/v1/routines/parse", "slack", Some("ana"), ask.clone())
        .await;
    assert_eq!(status, 422);
    assert_eq!(body["question"], pmos_bots::routines::ASK_AGAIN);
    // JSON, but not a valid routine.
    *env.gw.reply.lock().unwrap() =
        r#"{"schedule": {"cron": "8am weekdays"}, "prompt": "x"}"#.into();
    let (status, body) = env
        .post("/v1/routines/parse", "slack", Some("ana"), ask.clone())
        .await;
    assert_eq!(status, 422);
    assert_eq!(body["question"], pmos_bots::routines::ASK_WHEN);
    // The model's own question.
    *env.gw.reply.lock().unwrap() = r#"{"question": "Which channel should I summarize?"}"#.into();
    let (status, body) = env
        .post("/v1/routines/parse", "slack", Some("ana"), ask.clone())
        .await;
    assert_eq!(
        (status, body["question"].as_str()),
        (422, Some("Which channel should I summarize?"))
    );

    // The request itself.
    *env.gw.reply.lock().unwrap() = GOOD_REPLY.into();
    let with = |k: &str, v: &str| {
        let mut a = ask.clone();
        a[k] = json!(v);
        a
    };
    let (status, _) = env
        .post(
            "/v1/routines/parse",
            "slack",
            Some("ana"),
            with("tz", "London"),
        )
        .await;
    assert_eq!(status, 400);
    let (status, _) = env
        .post(
            "/v1/routines/parse",
            "slack",
            Some("ana"),
            with("bot", "nobody"),
        )
        .await;
    assert_eq!(status, 404);
    let (status, _) = env
        .post(
            "/v1/routines/parse",
            "slack",
            Some("ben"),
            with("owner", "ben"),
        )
        .await;
    assert_eq!(status, 403, "ben may not use ana's personal bot");
    let (status, _) = env
        .post("/v1/routines/parse", "slack", Some("ben"), ask.clone())
        .await;
    assert_eq!(
        status, 403,
        "a routine is made for the PM in X-Pmos-User only"
    );
    *env.gw.status.lock().unwrap() = 503;
    let (status, body) = env
        .post("/v1/routines/parse", "slack", Some("ana"), ask.clone())
        .await;
    assert_eq!(status, 503);
    assert!(body["error"].as_str().unwrap().contains("model gateway"));
    env.finish().await;
}

async fn routines_are_managed_by_their_owner(env: &Env) {
    let draft = json!({"bot": "pm-ana", "owner": "ana",
        "schedule": {"cron": "0 8 * * 1-5", "tz": "Europe/London"},
        "prompt": "Summarize #launch", "output": {"kind": "dm"}, "approval": "approve"});
    let (status, r) = env
        .post(
            "/v1/routines",
            "slack",
            Some("ana"),
            json!({"draft": draft}),
        )
        .await;
    assert_eq!(status, 201, "{r}");
    let rid = id(&r);
    assert!(rid.starts_with("rtn_"));
    assert_eq!(r["enabled"], true);
    assert!(r["next_run_at"].is_string());
    assert_eq!(r["last_run_at"], Value::Null);

    let mut bad = draft.clone();
    bad["approval"] = json!("auto");
    let (status, _) = env
        .post("/v1/routines", "slack", Some("ana"), json!({"draft": bad}))
        .await;
    assert_eq!(status, 400);
    let mut bad = draft.clone();
    bad["schedule"]["cron"] = json!("0 8 * *");
    let (status, body) = env
        .post("/v1/routines", "slack", Some("ana"), json!({"draft": bad}))
        .await;
    assert_eq!(status, 400);
    assert!(body["error"].as_str().unwrap().contains("five fields"));
    let mut bad = draft.clone();
    bad["owner"] = json!("ben");
    let (status, _) = env
        .post("/v1/routines", "slack", Some("ben"), json!({"draft": bad}))
        .await;
    assert_eq!(status, 403, "ben may not use pm-ana");

    let (_, mine) = env.get("/v1/routines", "slack", Some("ana")).await;
    assert_eq!(mine.as_array().unwrap().len(), 1);
    let (_, theirs) = env.get("/v1/routines", "slack", Some("ben")).await;
    assert_eq!(theirs, json!([]));
    let (status, _) = env
        .get("/v1/routines?owner=ana", "slack", Some("ben"))
        .await;
    assert_eq!(status, 403);
    let (_, all) = env.get("/v1/routines", "console", None).await;
    assert_eq!(all.as_array().unwrap().len(), 1);

    let path = format!("/v1/routines/{rid}");
    let (status, paused) = env
        .patch(&path, "console", None, json!({"enabled": false}))
        .await;
    assert_eq!(status, 200);
    assert_eq!(
        (paused["enabled"].as_bool(), &paused["next_run_at"]),
        (Some(false), &Value::Null)
    );
    let (status, _) = env
        .patch(&path, "console", None, json!({"prompt": "x"}))
        .await;
    assert_eq!(
        status, 403,
        "the console pauses and resumes; it doesn't rewrite"
    );
    let (status, resumed) = env
        .patch(&path, "slack", Some("ana"), json!({"enabled": true}))
        .await;
    assert_eq!(status, 200);
    assert!(resumed["next_run_at"].is_string());
    let (status, changed) = env
        .patch(
            &path,
            "slack",
            Some("ana"),
            json!({"schedule": {"cron": "30 9 * * 1", "tz": "America/New_York"}, "prompt": "Weekly digest"}),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(changed["schedule"]["cron"], "30 9 * * 1");
    assert_eq!(changed["prompt"], "Weekly digest");
    let (status, _) = env
        .patch(
            &path,
            "slack",
            Some("ana"),
            json!({"schedule": {"cron": "x", "tz": "UTC"}}),
        )
        .await;
    assert_eq!(status, 400);
    let (status, _) = env
        .patch(&path, "slack", Some("ana"), json!({"colour": "red"}))
        .await;
    assert_eq!(status, 400);
    let (status, _) = env
        .patch(&path, "slack", Some("ben"), json!({"enabled": false}))
        .await;
    assert_eq!(status, 404, "someone else's routine looks like none");
    let (status, _) = env.delete(&path, "slack", Some("ben")).await;
    assert_eq!(status, 404);
    let (status, _) = env.delete(&path, "slack", Some("ana")).await;
    assert_eq!(status, 204);
    let (status, _) = env.delete(&path, "console", None).await;
    assert_eq!(status, 404);
}

// ----- auth -------------------------------------------------------------------------------

#[tokio::test]
async fn every_endpoint_checks_the_client() {
    let env = Env::memory().await;
    let task = env.ask("Q").await;
    let tid = id(&task);
    let (_, r) = env
        .post(
            "/v1/routines",
            "slack",
            Some("ana"),
            json!({"draft": {"bot": "docs", "owner": "ana",
                "schedule": {"cron": "0 8 * * *", "tz": "UTC"}, "prompt": "p",
                "output": {"kind": "dm"}, "approval": "approve"}}),
        )
        .await;
    let rid = id(&r);

    // (method, path, body, clients allowed)
    let rows: Vec<(Method, String, Value, &[&str])> = vec![
        (
            Method::GET,
            "/v1/bots".into(),
            json!(null),
            &["slack", "web", "worker", "console", "evals"],
        ),
        (
            Method::GET,
            "/v1/bots/docs".into(),
            json!(null),
            &["slack", "web", "worker", "console", "evals"],
        ),
        (
            Method::POST,
            "/v1/tasks".into(),
            // ben is the benchmark member (PMOS_EVALS_AS) in these tests.
            json!({"bot": "docs", "requested_by": "ben", "text": "t", "source": "eval"}),
            &["evals"],
        ),
        (
            Method::GET,
            "/v1/tasks".into(),
            json!(null),
            &["slack", "web", "console", "evals"],
        ),
        // Evals may read tasks, but only eval tasks: this Slack one is a 404 for it.
        (
            Method::GET,
            format!("/v1/tasks/{tid}"),
            json!(null),
            &["slack", "web", "console", "evals"],
        ),
        (
            Method::GET,
            format!("/v1/tasks/{tid}/events"),
            json!(null),
            &["slack", "web", "console", "evals"],
        ),
        (
            Method::GET,
            format!("/v1/tasks/{tid}/trace"),
            json!(null),
            &["console"],
        ),
        (
            Method::GET,
            format!("/v1/tasks/{tid}/drafts"),
            json!(null),
            &["console", "evals"],
        ),
        (
            Method::POST,
            "/v1/worker/claim".into(),
            json!({"worker": "w1"}),
            &["worker"],
        ),
        (
            Method::POST,
            format!("/v1/worker/tasks/{tid}/heartbeat"),
            json!({}),
            &["worker"],
        ),
        (
            Method::POST,
            format!("/v1/worker/tasks/{tid}/events"),
            json!({"kind": "status", "text": "x"}),
            &["worker"],
        ),
        (
            Method::POST,
            format!("/v1/worker/tasks/{tid}/trace"),
            json!({"entries": []}),
            &["worker"],
        ),
        (
            Method::POST,
            format!("/v1/worker/tasks/{tid}/complete"),
            json!({"result": "r"}),
            &["worker"],
        ),
        (
            Method::POST,
            format!("/v1/worker/tasks/{tid}/fail"),
            json!({"reason": "r"}),
            &["worker"],
        ),
        (
            Method::GET,
            "/v1/notifications".into(),
            json!(null),
            &["slack"],
        ),
        (
            Method::POST,
            "/v1/routines/parse".into(),
            json!({"bot": "docs", "owner": "ana", "text": "t", "tz": "UTC"}),
            &["slack", "web"],
        ),
        (
            Method::POST,
            "/v1/routines".into(),
            json!({"draft": {}}),
            &["slack", "web"],
        ),
        (
            Method::GET,
            "/v1/routines".into(),
            json!(null),
            &["slack", "web", "console"],
        ),
        (
            Method::PATCH,
            format!("/v1/routines/{rid}"),
            json!({"enabled": true}),
            &["slack", "web", "console"],
        ),
        (
            Method::DELETE,
            format!("/v1/routines/{rid}"),
            json!(null),
            &["slack", "web", "console"],
        ),
        // The console cancels only signed for a viewer (R1): unsigned, as here, it is `403`.
        // `tests/assertion.rs` checks it signed.
        (
            Method::POST,
            format!("/v1/tasks/{tid}/cancel"),
            json!({}),
            &["slack", "web"],
        ),
    ];
    for (method, path, body, allowed) in &rows {
        for client in ["slack", "web", "worker", "console", "evals", "none"] {
            let user = matches!(client, "slack" | "web").then_some("ana");
            let mut req = env.req(method.clone(), path, client, user);
            if !body.is_null() {
                req = req.json(body);
            }
            let status = req.send().await.unwrap().status().as_u16();
            let want_allowed = allowed.contains(&client);
            match client {
                "none" => assert_eq!(status, 401, "{method} {path} with no token"),
                _ if want_allowed => assert!(
                    status != 401 && status != 403,
                    "{method} {path} as {client}: {status}"
                ),
                _ => assert_eq!(status, 403, "{method} {path} as {client}"),
            }
        }
    }

    // A wrong token, and Slack without a PM.
    let res = env
        .http
        .get(format!("{}/v1/bots", env.base))
        .bearer_auth("not-a-token-0123456789abcdef0123456789")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 401);
    // The shared feed and the bot list need no PM; the Slack bot reads them for everyone.
    let (status, _) = env.get("/v1/bots", "slack", None).await;
    assert_eq!(status, 200);
    let (status, _) = env.get("/v1/notifications", "slack", None).await;
    assert_eq!(status, 200);
    // A malformed PM is refused; per-PM calls without one are refused.
    let (status, _) = env.get("/v1/bots", "slack", Some("Not A Handle")).await;
    assert_eq!(status, 400);
    let (status, _) = env
        .post(
            "/v1/tasks",
            "slack",
            None,
            json!({"bot": "docs", "requested_by": "ana", "text": "t", "source": "slack"}),
        )
        .await;
    assert_eq!(status, 403);
    let (status, _) = env.get("/v1/routines", "slack", None).await;
    assert_eq!(status, 403);
    let (status, _) = env.get("/v1/tasks", "slack", None).await;
    assert_eq!(status, 403);
    // Slack acts only for its PM.
    let (status, _) = env
        .post(
            "/v1/tasks",
            "slack",
            Some("ben"),
            json!({"bot": "docs", "requested_by": "ana", "text": "t", "source": "slack"}),
        )
        .await;
    assert_eq!(status, 403);
    let (status, _) = env
        .post(
            "/v1/tasks",
            "evals",
            None,
            json!({"bot": "docs", "requested_by": "ana", "text": "t", "source": "slack"}),
        )
        .await;
    assert_eq!(status, 403, "evals creates eval tasks only");
    // Web acts only for its PM too, and creates only `web` tasks; Slack cannot set `web`.
    let (status, _) = env
        .post(
            "/v1/tasks",
            "web",
            Some("ben"),
            json!({"bot": "docs", "requested_by": "ana", "text": "t", "source": "web"}),
        )
        .await;
    assert_eq!(status, 403);
    let (status, _) = env.get("/v1/tasks", "web", None).await;
    assert_eq!(status, 403);
    for (client, source) in [("web", "slack"), ("web", "cli"), ("slack", "web")] {
        let (status, _) = env
            .post(
                "/v1/tasks",
                client,
                Some("ana"),
                json!({"bot": "docs", "requested_by": "ana", "text": "t", "source": source}),
            )
            .await;
        assert_eq!(status, 403, "{client} may not set source {source}");
    }
    // Health needs no token.
    let (status, _) = env.get("/healthz", "none", None).await;
    assert_eq!(status, 200);
    env.finish().await;
}

/// R2 (W6 for revisions): the web client may give a task outside any conversation a
/// sensitivity floor (a draft's revision, whose prompt holds the draft's text). The task's
/// session starts there; no other client, and no task in a conversation, may set one.
#[tokio::test]
async fn a_revision_task_starts_its_session_at_its_floor() {
    let env = Env::memory().await;
    let task = |extra: Value| {
        let mut body = json!({"bot": "docs", "requested_by": "ana", "text": "Revise: …",
                              "source": "web", "sensitivity_floor": "restricted"});
        if let (Some(b), Value::Object(extra)) = (body.as_object_mut(), extra) {
            b.extend(extra);
        }
        body
    };
    let (status, t) = env
        .post("/v1/tasks", "web", Some("ana"), task(json!({})))
        .await;
    assert_eq!(status, 201, "{t}");
    assert_eq!(t["sources_sensitivity"], "restricted");
    let (status, claim) = env.claim().await;
    assert_eq!(status, 200, "{claim}");
    let sessions = env.ts.sessions.lock().unwrap().clone();
    assert_eq!(
        sessions.last().unwrap()["history_sensitivity"],
        "restricted",
        "{sessions:?}"
    );
    // Not in a conversation, and not from Slack.
    let (status, _) = env
        .post(
            "/v1/tasks",
            "web",
            Some("ana"),
            task(json!({"conversation": "cnv_1"})),
        )
        .await;
    assert_eq!(status, 403);
    let (status, _) = env
        .post(
            "/v1/tasks",
            "slack",
            Some("ana"),
            task(json!({"source": "slack"})),
        )
        .await;
    assert_eq!(status, 403);
    env.finish().await;
}

#[tokio::test]
async fn readiness_covers_storage_the_tool_server_the_gateway_and_the_directory() {
    let env = Env::memory().await;
    let (status, body) = env.get("/readyz", "none", None).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body,
        json!({"ready": true, "checks": {"storage": "ok", "toolserver": "ok", "gateway": "ok", "directory": "ok"}})
    );
    *env.ts.ready.lock().unwrap() = false;
    let (status, body) = env.get("/readyz", "none", None).await;
    assert_eq!(status, 503);
    assert_eq!(body["checks"]["toolserver"], "down");
    assert_eq!(body["checks"]["gateway"], "ok");
    // A directory snapshot more than 30 seconds old is not ready either.
    *env.ts.ready.lock().unwrap() = true;
    env.clock
        .advance(pmos_directory::snapshot::ALLOW_AGE + std::time::Duration::from_secs(1));
    let (status, body) = env.get("/readyz", "none", None).await;
    assert_eq!(status, 503);
    assert_eq!(body["checks"]["directory"], "down");
    env.refresh().await;
    let (status, _) = env.get("/readyz", "none", None).await;
    assert_eq!(status, 200);
    env.finish().await;
}
