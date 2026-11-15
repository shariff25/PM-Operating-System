//! The daily triage digest (`docs/improvement-loop-spec.md` §4) against the fake Slack
//! (`common`), a mock bots runtime (`GET /v1/bots`, with owners) and a mock tool server
//! ledger (`GET /internal/ledger`).
//!
//! - One DM per quality owner, grouped by bot; owners outside `team.yaml` are skipped.
//! - The ledger query asks for rejected rows, and edited ones from the threshold up.
//! - Nothing is sent when nothing qualifies, and at most once per owner per day, across a
//!   restart.

mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use chrono::{DateTime, NaiveTime, Utc};
use common::{serve, Call, FakeSlack, SLACK_BOT_TOKEN};
use pmos_slack::bots::{Bots, BotsConfig};
use pmos_slack::runtime::Runtime;
use pmos_slack::slackapi::SlackApi;
use pmos_slack::toolserver::ToolServer;
use pmos_slack::triage::{Triage, TriageConfig, SENT_FILE};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

const BOTS_TOKEN: &str = "test-bots-token-slack-0123456789abcdef";
const TOOL_BOT_TOKEN: &str = "test-bot-token-0123456789abcdef-0123";
const TEAM: &str = "members:\n  - {handle: ana, slack-id: U0ANA, roles: [pm]}\n  - {handle: bob, slack-id: U0BOB, roles: [pm]}\n";

fn bearer(headers: &HeaderMap) -> &str {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default()
}

fn utc(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}

async fn rt_bots(headers: HeaderMap) -> Response {
    if bearer(&headers) != BOTS_TOKEN {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    Json(json!([
        {"name": "pm-ana", "owner": "ana"},
        {"name": "docs", "owner": "bob"},
        {"name": "pm-bob", "owner": "bob"},
        {"name": "release-notes", "owner": "carol"},
        {"name": "orphan", "owner": null},
    ]))
    .into_response()
}

#[derive(Default)]
struct Ledger {
    rows: Mutex<Vec<Value>>,
    /// Each `GET /internal/ledger`'s query.
    queries: Mutex<Vec<HashMap<String, String>>>,
}

/// The ledger as the spec describes it: `outcome` and `min_edit` filter, `since` and
/// `until` bound `created_at`.
async fn ts_ledger(
    State(l): State<Arc<Ledger>>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    if bearer(&headers) != TOOL_BOT_TOKEN {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "bad token"})),
        )
            .into_response();
    }
    l.queries.lock().unwrap().push(q.clone());
    let outcomes: Vec<String> = q
        .get("outcome")
        .map(|o| o.split(',').map(String::from).collect())
        .unwrap_or_default();
    let min_edit: Option<f64> = q.get("min_edit").map(|m| m.parse().unwrap());
    let since = q.get("since").map(|s| utc(s));
    let until = q.get("until").map(|s| utc(s));
    let rows: Vec<Value> = l
        .rows
        .lock()
        .unwrap()
        .iter()
        .filter(|r| outcomes.is_empty() || outcomes.iter().any(|o| r["outcome"] == o.as_str()))
        .filter(|r| min_edit.is_none_or(|m| r["edit_distance"].as_f64().is_some_and(|d| d >= m)))
        .filter(|r| {
            let created = utc(r["created_at"].as_str().unwrap());
            since.is_none_or(|s| created >= s) && until.is_none_or(|u| created <= u)
        })
        .cloned()
        .collect();
    Json(json!({ "rows": rows })).into_response()
}

/// A ledger row with every field the spec lists; `fields` gives `id`, `bot`, `user`, `tool`,
/// `outcome`, and `edit_distance`, `reason`, `created_at` and `decided_at` where they apply.
fn row(fields: Value) -> Value {
    let id = fields["id"].as_str().unwrap();
    let mut row = json!({
        "run_id": format!("tsk_{id}"), "skill": null, "content_sha": null,
        "model_alias": "pmos/default", "edit_distance": null, "reason": null,
        "decided_by": fields["user"],
    });
    for (k, v) in fields.as_object().unwrap() {
        row[k] = v.clone();
    }
    row
}

struct Stack {
    slack: Arc<FakeSlack>,
    ledger: Arc<Ledger>,
    slack_base: String,
    rt_base: String,
    ts_base: String,
    state: tempfile::TempDir,
    team: tempfile::TempDir,
    stop: Vec<CancellationToken>,
}

impl Stack {
    async fn new() -> Self {
        let slack = Arc::new(FakeSlack::default());
        let (slack_base, stop_slack) = serve(common::routes().with_state(slack.clone())).await;
        *slack.base.lock().unwrap() = slack_base.clone();
        let (rt_base, stop_rt) = serve(Router::new().route("/v1/bots", get(rt_bots))).await;
        let ledger = Arc::new(Ledger::default());
        let (ts_base, stop_ts) = serve(
            Router::new()
                .route("/internal/ledger", get(ts_ledger))
                .with_state(ledger.clone()),
        )
        .await;
        let team = tempfile::tempdir().unwrap();
        std::fs::write(team.path().join("team.yaml"), TEAM).unwrap();
        Self {
            slack,
            ledger,
            slack_base,
            rt_base,
            ts_base,
            state: tempfile::tempdir().unwrap(),
            team,
            stop: vec![stop_slack, stop_rt, stop_ts],
        }
    }

    /// The bots, with this Slack token and tool server.
    fn bots(&self, slack_token: &str, toolserver: &str) -> Bots {
        let http = reqwest::Client::new();
        Bots::new(BotsConfig {
            slack: SlackApi::new(self.slack_base.clone(), slack_token, http.clone()),
            runtime: Runtime::new(self.rt_base.clone(), BOTS_TOKEN, http),
            toolserver: ToolServer::new(toolserver, TOOL_BOT_TOKEN),
            team_file: self.team.path().join("team.yaml"),
            default_tz: "America/Los_Angeles".into(),
            github_org: None,
            response_url_prefix: "https://hooks.slack.com/".into(),
        })
    }

    /// A fresh digest, as after a restart: only the state directory carries over.
    fn triage(&self, min_edit: f64) -> Triage {
        self.triage_with(self.bots(SLACK_BOT_TOKEN, &self.ts_base), min_edit)
    }

    fn triage_with(&self, bots: Bots, min_edit: f64) -> Triage {
        Triage::new(
            bots,
            TriageConfig {
                at: NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
                tz: "America/Los_Angeles".parse().unwrap(),
                min_edit,
                console_url: Some("https://console.example.com".into()),
            },
            self.state.path(),
        )
    }

    fn posts(&self) -> Vec<Call> {
        self.slack.calls("chat.postMessage")
    }

    fn stop(self) {
        for s in &self.stop {
            s.cancel();
        }
    }
}

fn text_of(call: &Call) -> String {
    format!("{} {}", call.body["text"], call.body["blocks"])
}

/// Monday 16 Nov 2026, 09:00 in Los Angeles. The window starts Friday 13 Nov, 09:00 (17:00Z).
const MONDAY: &str = "2026-11-16T17:00:00Z";

fn monday_rows() -> Vec<Value> {
    vec![
        // Ana's bot: a rejection with a reason, a heavy edit, a light edit, an approval.
        row(json!({
            "id": "a1", "bot": "pm-ana", "user": "ana", "tool": "jira.create_issue", "outcome": "rejected",
            "reason": "wrong_facts", "created_at": "2026-11-14T10:00:00Z", "decided_at": "2026-11-14T10:05:00Z",
        })),
        row(json!({
            "id": "a2", "bot": "pm-ana", "user": "ana", "tool": "confluence.create_draft", "outcome": "edited",
            "edit_distance": 0.45, "created_at": "2026-11-16T15:00:00Z", "decided_at": "2026-11-16T15:30:00Z",
        })),
        row(json!({
            "id": "a3", "bot": "pm-ana", "user": "ana", "tool": "confluence.update_page", "outcome": "edited",
            "edit_distance": 0.1, "created_at": "2026-11-15T10:00:00Z", "decided_at": "2026-11-15T10:01:00Z",
        })),
        row(json!({
            "id": "a4", "bot": "pm-ana", "user": "ana", "tool": "slack.post_reply", "outcome": "approved",
            "edit_distance": 0.0, "created_at": "2026-11-15T10:00:00Z", "decided_at": "2026-11-15T10:02:00Z",
        })),
        // Decided before the window (Friday morning): last week's digest had it.
        row(json!({
            "id": "a5", "bot": "pm-ana", "user": "ana", "tool": "jira.create_issue", "outcome": "rejected",
            "reason": "tone", "created_at": "2026-11-13T15:00:00Z", "decided_at": "2026-11-13T16:00:00Z",
        })),
        // Made before the window, decided in it: still listed.
        row(json!({
            "id": "a6", "bot": "pm-ana", "user": "ana", "tool": "jira.create_issue", "outcome": "rejected",
            "created_at": "2026-11-12T12:00:00Z", "decided_at": "2026-11-14T09:00:00Z",
        })),
        // Bob owns two bots: one DM for both.
        row(json!({
            "id": "b1", "bot": "docs", "user": "ana", "tool": "slack.post_reply", "outcome": "rejected",
            "reason": "should_not_draft", "created_at": "2026-11-15T11:00:00Z", "decided_at": "2026-11-15T11:00:00Z",
        })),
        row(json!({
            "id": "b2", "bot": "pm-bob", "user": "bob", "tool": "custom.<tool>", "outcome": "edited",
            "edit_distance": 0.9, "created_at": "2026-11-16T01:00:00Z", "decided_at": "2026-11-16T02:00:00Z",
        })),
        // Carol isn't on the team, and the orphan has no owner: skipped.
        row(json!({
            "id": "c1", "bot": "release-notes", "user": "bob", "tool": "confluence.create_draft", "outcome": "rejected",
            "created_at": "2026-11-15T11:00:00Z", "decided_at": "2026-11-15T11:00:00Z",
        })),
        row(json!({
            "id": "o1", "bot": "orphan", "user": "bob", "tool": "jira.create_issue", "outcome": "rejected",
            "created_at": "2026-11-15T11:00:00Z", "decided_at": "2026-11-15T11:00:00Z",
        })),
    ]
}

#[tokio::test]
async fn one_digest_per_owner_grouped_by_bot() {
    let stack = Stack::new().await;
    *stack.ledger.rows.lock().unwrap() = monday_rows();
    let sent = stack.triage(0.3).send(utc(MONDAY)).await.unwrap();
    assert_eq!(sent, 2);
    let posts = stack.posts();
    assert_eq!(posts.len(), 2, "Ana and Bob, nobody else");
    let to = |user: &str| {
        posts
            .iter()
            .find(|p| p.body["channel"] == user)
            .unwrap_or_else(|| panic!("no digest for {user}"))
            .clone()
    };

    let ana = text_of(&to("U0ANA"));
    assert!(
        ana.contains(
            "*Drafts worth a look:* 3 rejected or heavily edited since Fri 13 Nov 09:00 PST"
        ),
        "{ana}"
    );
    assert!(ana.contains("*PM Ana* (`pm-ana`)"));
    assert!(ana.contains("• `jira.create_issue` for ana · rejected: Wrong facts · <https://console.example.com/tasks/tsk_a1|open the task>"), "{ana}");
    assert!(ana.contains("• `confluence.create_draft` for ana · edited, 45% changed · <https://console.example.com/tasks/tsk_a2|open the task>"));
    assert!(ana.contains("tsk_a6") && ana.contains("rejected, no reason given"));
    for left_out in ["tsk_a3", "tsk_a4", "tsk_a5", "tsk_b1", "tsk_c1", "tsk_o1"] {
        assert!(!ana.contains(left_out), "{left_out} in Ana's digest");
    }
    // Newest first.
    let (a2, a1, a6) = (
        ana.find("tsk_a2").unwrap(),
        ana.find("tsk_a1").unwrap(),
        ana.find("tsk_a6").unwrap(),
    );
    assert!(a2 < a1 && a1 < a6, "{ana}");
    assert!(ana.contains("Worth keeping? Write a case card: `evals/case-card.md`"));
    assert!(ana.contains("Slack text leaves the trace after 24 hours"));
    assert!(
        to("U0ANA").body.get("username").is_none(),
        "from PM OS, not a bot"
    );

    let bob = text_of(&to("U0BOB"));
    assert!(
        bob.contains("*Docs* (`docs`)") && bob.contains("*PM Bob* (`pm-bob`)"),
        "{bob}"
    );
    assert!(bob.contains("rejected: Shouldn't have drafted"));
    assert!(
        bob.contains("`custom.&lt;tool&gt;` for bob · edited, 90% changed"),
        "escaped: {bob}"
    );
    assert!(!bob.contains("tsk_c1") && !bob.contains("tsk_o1"));

    // Two queries: rejected rows, and edited ones from the threshold up, through Monday's
    // time, reaching a week before the window.
    let queries = stack.ledger.queries.lock().unwrap().clone();
    assert_eq!(queries.len(), 2);
    let q = |outcome: &str| {
        queries
            .iter()
            .find(|q| q["outcome"] == outcome)
            .unwrap()
            .clone()
    };
    let rejected = q("rejected");
    assert_eq!(rejected.get("min_edit"), None);
    assert_eq!(rejected["until"], MONDAY);
    assert_eq!(rejected["since"], "2026-11-06T17:00:00Z");
    assert_eq!(rejected["limit"], "1000");
    let edited = q("edited");
    assert_eq!(edited["min_edit"], "0.3");
    assert_eq!(edited["until"], MONDAY);
    stack.stop();
}

#[tokio::test]
async fn the_threshold_is_the_configured_one() {
    let stack = Stack::new().await;
    *stack.ledger.rows.lock().unwrap() = monday_rows();
    stack.triage(0.05).send(utc(MONDAY)).await.unwrap();
    let queries = stack.ledger.queries.lock().unwrap().clone();
    let edited = queries.iter().find(|q| q["outcome"] == "edited").unwrap();
    assert_eq!(edited["min_edit"], "0.05");
    let ana = stack
        .posts()
        .into_iter()
        .find(|p| p.body["channel"] == "U0ANA")
        .unwrap();
    assert!(
        text_of(&ana).contains("edited, 10% changed"),
        "the light edit counts now"
    );
    stack.stop();
}

#[tokio::test]
async fn nothing_to_triage_sends_nothing() {
    let stack = Stack::new().await;
    // Only rows that don't qualify, or that fall outside the window.
    *stack.ledger.rows.lock().unwrap() = vec![
        row(json!({
            "id": "x1", "bot": "pm-ana", "user": "ana", "tool": "jira.create_issue", "outcome": "approved",
            "edit_distance": 0.0, "created_at": "2026-11-16T10:00:00Z", "decided_at": "2026-11-16T10:00:00Z",
        })),
        row(json!({
            "id": "x2", "bot": "pm-ana", "user": "ana", "tool": "jira.create_issue", "outcome": "edited",
            "edit_distance": 0.2, "created_at": "2026-11-16T10:00:00Z", "decided_at": "2026-11-16T10:00:00Z",
        })),
        row(json!({
            "id": "x3", "bot": "pm-ana", "user": "ana", "tool": "jira.create_issue", "outcome": "rejected",
            "created_at": "2026-11-16T18:00:00Z", "decided_at": "2026-11-16T18:00:00Z",
        })),
    ];
    assert_eq!(stack.triage(0.3).send(utc(MONDAY)).await.unwrap(), 0);
    assert!(stack.posts().is_empty());
    assert!(
        !stack.state.path().join(SENT_FILE).exists(),
        "nothing recorded"
    );
    stack.stop();
}

#[tokio::test]
async fn at_most_once_per_owner_per_day_across_a_restart() {
    let stack = Stack::new().await;
    *stack.ledger.rows.lock().unwrap() = monday_rows();
    assert_eq!(stack.triage(0.3).send(utc(MONDAY)).await.unwrap(), 2);
    let saved: Value =
        serde_json::from_str(&std::fs::read_to_string(stack.state.path().join(SENT_FILE)).unwrap())
            .unwrap();
    assert_eq!(saved, json!({"ana": "2026-11-16", "bob": "2026-11-16"}));

    // A restart later the same day (a new digest, the same state directory) sends nothing.
    assert_eq!(stack.triage(0.3).send(utc(MONDAY)).await.unwrap(), 0);
    assert_eq!(
        stack
            .triage(0.3)
            .send(utc("2026-11-16T20:00:00Z"))
            .await
            .unwrap(),
        0
    );
    assert_eq!(stack.posts().len(), 2);

    // Tuesday is a new day: a rejection decided since Monday's digest goes out.
    stack.ledger.rows.lock().unwrap().push(row(json!({
        "id": "t1", "bot": "pm-ana", "user": "ana", "tool": "jira.create_issue", "outcome": "rejected",
        "reason": "incomplete", "created_at": "2026-11-17T09:00:00Z", "decided_at": "2026-11-17T09:10:00Z",
    })));
    assert_eq!(
        stack
            .triage(0.3)
            .send(utc("2026-11-17T17:00:00Z"))
            .await
            .unwrap(),
        1
    );
    let posts = stack.posts();
    assert_eq!(posts.len(), 3);
    let tuesday = text_of(&posts[2]);
    assert_eq!(posts[2].body["channel"], "U0ANA");
    assert!(tuesday.contains("tsk_t1") && tuesday.contains("rejected: Incomplete"));
    assert!(!tuesday.contains("tsk_a1"), "Monday's rows aren't repeated");
    stack.stop();
}

#[tokio::test]
async fn an_unreachable_ledger_is_an_error_and_sends_nothing() {
    let stack = Stack::new().await;
    let triage = stack.triage_with(stack.bots(SLACK_BOT_TOKEN, "http://127.0.0.1:9"), 0.3);
    assert!(triage.send(utc(MONDAY)).await.is_err());
    assert!(stack.posts().is_empty());
    stack.stop();
}

#[tokio::test]
async fn a_failed_dm_is_not_recorded_so_trying_again_sends_it() {
    let stack = Stack::new().await;
    *stack.ledger.rows.lock().unwrap() = monday_rows();
    // Slack refuses the posts (a bad token): an error, and no owner is marked as sent.
    let refused = stack.triage_with(stack.bots("xoxb-wrong", &stack.ts_base), 0.3);
    let err = refused.send(utc(MONDAY)).await.unwrap_err();
    assert!(err.contains("2 of 2 digests"), "{err}");
    let saved = std::fs::read_to_string(stack.state.path().join(SENT_FILE)).unwrap();
    assert_eq!(serde_json::from_str::<Value>(&saved).unwrap(), json!({}));
    // The retry sends both.
    assert_eq!(stack.triage(0.3).send(utc(MONDAY)).await.unwrap(), 2);
    assert_eq!(stack.posts().len(), 2);
    stack.stop();
}
