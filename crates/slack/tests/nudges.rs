//! The nudge engine (`docs/nudges.md`) against the fake Slack (`common`) and a mock tool
//! server's pending-drafts list, with a fake clock.

mod common;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use common::{person, serve, Call, Directory, FakeSlack, SLACK_BOT_TOKEN};
use pmos_slack::bots::{Bots, BotsConfig};
use pmos_slack::nudge_copy::{self, Line, DRAFTS_WAITING};
use pmos_slack::nudges::{self, Clock, NudgeConfig, Nudges, ACTION_NOT_NOW};
use pmos_slack::runtime::Runtime;
use pmos_slack::slackapi::SlackApi;
use pmos_slack::toolserver::ToolServer;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

const BOTS_TOKEN: &str = "test-bots-token-slack-0123456789abcdef";
const TOOL_BOT_TOKEN: &str = "test-bot-token-0123456789abcdef-0123";
/// The user directory: Ana, Bob and Carol.
fn team() -> Vec<pmos_directory::snapshot::DirectoryUser> {
    vec![
        person("ana", "U0ANA", &["pm"]),
        person("bob", "U0BOB", &["pm"]),
        person("carol", "U0CAROL", &["pm"]),
    ]
}

// Tuesday 17 Nov 2026; Los Angeles is UTC-8, so 17:00Z is 09:00 there.
const TUE_9AM_LA: &str = "2026-11-17T17:00:00Z";

fn utc(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}

struct FakeClock(Mutex<DateTime<Utc>>);

impl FakeClock {
    fn set(&self, at: &str) {
        *self.0.lock().unwrap() = utc(at);
    }
}

impl Clock for FakeClock {
    fn now(&self) -> DateTime<Utc> {
        *self.0.lock().unwrap()
    }
}

#[derive(Default)]
struct Drafts {
    by_user: Mutex<HashMap<String, Vec<Value>>>,
    down: AtomicBool,
}

async fn approvals(
    State(d): State<Arc<Drafts>>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let ok = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == format!("Bearer {TOOL_BOT_TOKEN}"));
    if !ok {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if d.down.load(Ordering::SeqCst) {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let items = d
        .by_user
        .lock()
        .unwrap()
        .get(q.get("user").map_or("", String::as_str))
        .cloned()
        .unwrap_or_default();
    Json(json!({"approvals": items, "total": items.len()})).into_response()
}

/// A pending draft made at `created`.
fn draft(id: &str, tool: &str, created: &str) -> Value {
    json!({"id": id, "tool": tool, "created_at": created, "status": "pending"})
}

/// `n` Slack-reply drafts made the day before, so each is over four hours old.
fn old_drafts(n: usize) -> Vec<Value> {
    (1..=n)
        .map(|i| {
            draft(
                &format!("apr_{i}"),
                "slack.post_reply",
                "2026-11-16T12:00:00Z",
            )
        })
        .collect()
}

struct Stack {
    slack: Arc<FakeSlack>,
    drafts: Arc<Drafts>,
    clock: Arc<FakeClock>,
    slack_base: String,
    ts_base: String,
    state: tempfile::TempDir,
    content: tempfile::TempDir,
    directory: Directory,
    stop: Vec<CancellationToken>,
}

impl Stack {
    async fn new(now: &str) -> Self {
        let slack = Arc::new(FakeSlack::default());
        let (slack_base, stop_slack) = serve(common::routes().with_state(slack.clone())).await;
        *slack.base.lock().unwrap() = slack_base.clone();
        let drafts = Arc::new(Drafts::default());
        let (ts_base, stop_ts) = serve(
            Router::new()
                .route("/internal/approvals", get(approvals))
                .with_state(drafts.clone()),
        )
        .await;
        let content = tempfile::tempdir().unwrap();
        let directory = Directory::seeded(team()).await;
        Self {
            slack,
            drafts,
            clock: Arc::new(FakeClock(Mutex::new(utc(now)))),
            slack_base,
            ts_base,
            state: tempfile::tempdir().unwrap(),
            content,
            directory,
            stop: vec![stop_slack, stop_ts],
        }
    }

    fn set_drafts(&self, user: &str, items: Vec<Value>) {
        self.drafts
            .by_user
            .lock()
            .unwrap()
            .insert(user.into(), items);
    }

    fn bots(&self) -> Bots {
        let http = reqwest::Client::new();
        Bots::new(BotsConfig {
            slack: SlackApi::new(self.slack_base.clone(), SLACK_BOT_TOKEN, http.clone()),
            runtime: Runtime::new("http://127.0.0.1:9", BOTS_TOKEN, http),
            toolserver: ToolServer::new(&self.ts_base, TOOL_BOT_TOKEN),
            people: self.directory.people(),
            content_root: self.content.path().to_path_buf(),
            default_tz: "America/Los_Angeles".into(),
            github_org: None,
            response_url_prefix: "https://hooks.slack.com/".into(),
            state_dir: self.state.path().to_path_buf(),
            web_url: None,
        })
    }

    /// An engine as after a restart: only the state directory carries over.
    fn engine_with(&self, lines: Vec<Line>) -> Nudges {
        Nudges::with_parts(
            self.bots(),
            NudgeConfig::from_settings("America/Los_Angeles", None).unwrap(),
            self.state.path(),
            self.clock.clone(),
            lines,
            nudges::default_kinds(),
        )
    }

    fn engine(&self) -> Nudges {
        self.engine_with(nudge_copy::LINES.to_vec())
    }

    fn posts(&self) -> Vec<Call> {
        self.slack.calls("chat.postMessage")
    }

    fn state_json(&self) -> Value {
        serde_json::from_str(
            &std::fs::read_to_string(self.state.path().join(nudges::STATE_FILE)).unwrap(),
        )
        .unwrap()
    }

    fn stop(self) {
        for s in &self.stop {
            s.cancel();
        }
    }
}

fn text_of(call: &Call) -> String {
    call.body["text"].as_str().unwrap_or_default().to_string()
}

/// Enough distinct lines for a month of nudges.
const MANY: [Line; 8] = [
    line("t1"),
    line("t2"),
    line("t3"),
    line("t4"),
    line("t5"),
    line("t6"),
    line("t7"),
    line("t8"),
];

const fn line(id: &'static str) -> Line {
    Line {
        id,
        kind: DRAFTS_WAITING,
        expires: false,
        one: "One draft, test wording.",
        many: "{Count} drafts, test wording.",
    }
}

fn click(user: &str, channel: &str, ts: &str) -> Value {
    json!({
        "type": "block_actions",
        "user": {"id": user},
        "container": {"channel_id": channel, "message_ts": ts},
        "actions": [{"action_id": ACTION_NOT_NOW, "value": "x"}],
    })
}

#[tokio::test]
async fn off_unless_asked() {
    assert!(!nudges::enabled(None));
    assert!(!nudges::enabled(Some("0")));
    assert!(!nudges::enabled(Some("")));
    assert!(nudges::enabled(Some("1")));

    let s = Stack::new(TUE_9AM_LA).await;
    let started = nudges::spawn(
        &s.bots(),
        "America/Los_Angeles",
        s.state.path(),
        |_| None,
        CancellationToken::new(),
    )
    .unwrap();
    assert!(started.is_none(), "no engine without PMOS_NUDGES=1");
    assert!(nudges::action(&click("U0ANA", "D0ANA", "1.0")).is_none());
    assert!(!nudges::handle_dm("D0ANA", "1.0", "ana", "nudges off").await);
    assert!(s.posts().is_empty());
    assert!(!s.state.path().join(nudges::STATE_FILE).exists());
    s.stop();
}

#[tokio::test]
async fn one_nudge_a_day_in_the_dm_with_a_button() {
    let s = Stack::new(TUE_9AM_LA).await;
    s.set_drafts("ana", old_drafts(3));
    assert_eq!(s.engine().tick().await, 1);
    let posts = s.posts();
    assert_eq!(posts.len(), 1);
    // Posted to the PM's user id, which Slack turns into the DM; never a channel.
    assert_eq!(posts[0].body["channel"], "U0ANA");
    assert!(posts[0].body.get("thread_ts").is_none());
    assert!(text_of(&posts[0]).starts_with("Three drafts are waiting for you. They expire in "));
    let blocks = &posts[0].body["blocks"];
    assert_eq!(blocks[1]["elements"][0]["text"]["text"], "Not now");
    assert_eq!(blocks[1]["elements"][0]["action_id"], ACTION_NOT_NOW);

    // Later the same day, and after a restart: nothing more.
    let again = s.engine();
    s.clock.set("2026-11-17T20:00:00Z");
    assert_eq!(again.tick().await, 0);
    assert_eq!(s.engine().tick().await, 0);
    assert_eq!(s.posts().len(), 1);
    // The next day, another (with its own wording).
    s.clock.set("2026-11-18T17:30:00Z");
    assert_eq!(s.engine().tick().await, 1);
    assert_eq!(s.posts().len(), 2);
    assert_ne!(text_of(&s.posts()[0]), text_of(&s.posts()[1]));
    s.stop();
}

#[tokio::test]
async fn the_wording_is_right_for_the_data() {
    // Exactly 24 hours to expiry, three drafts: the sentence as written.
    let s = Stack::new(TUE_9AM_LA).await;
    let items: Vec<Value> = (1..=3)
        .map(|i| {
            let mut d = draft(
                &format!("apr_{i}"),
                "slack.post_reply",
                "2026-11-17T12:00:00Z",
            );
            d["expires_at"] = json!("2026-11-18T17:00:00Z");
            d
        })
        .collect();
    s.set_drafts("ana", items);
    // One draft that doesn't expire (a Jira write): no claim about expiry, and singular.
    s.set_drafts(
        "bob",
        vec![draft("apr_j", "jira.create_issue", "2026-11-16T12:00:00Z")],
    );
    // One Slack draft: singular with its expiry.
    s.set_drafts(
        "carol",
        vec![draft("apr_s", "slack.send_message", "2026-11-17T05:00:00Z")],
    );
    assert_eq!(s.engine().tick().await, 3);
    let by_user: HashMap<String, String> = s
        .posts()
        .iter()
        .map(|c| (c.body["channel"].as_str().unwrap().to_string(), text_of(c)))
        .collect();
    assert_eq!(
        by_user["U0ANA"],
        "Three drafts are waiting for you. They expire in 24 hours, and they know it."
    );
    assert!(!by_user["U0BOB"].contains("expire"), "{}", by_user["U0BOB"]);
    assert!(by_user["U0BOB"].starts_with("One draft is"));
    assert_eq!(
        by_user["U0CAROL"],
        "One draft is waiting for you. It expires in 12 hours, and it knows it."
    );
    s.stop();
}

#[tokio::test]
async fn no_drafts_or_only_fresh_ones_means_no_nudge() {
    let s = Stack::new(TUE_9AM_LA).await;
    let engine = s.engine();
    assert_eq!(engine.tick().await, 0, "no drafts");
    // Three hours old: too young.
    s.set_drafts(
        "ana",
        vec![draft("apr_1", "slack.post_reply", "2026-11-17T14:00:00Z")],
    );
    assert_eq!(engine.tick().await, 0);
    // The oldest is over four hours: now.
    s.set_drafts(
        "ana",
        vec![
            draft("apr_1", "slack.post_reply", "2026-11-17T14:00:00Z"),
            draft("apr_0", "slack.post_reply", "2026-11-17T12:30:00Z"),
        ],
    );
    assert_eq!(engine.tick().await, 1);
    s.stop();
}

#[tokio::test]
async fn working_hours_in_each_pms_own_time_zone() {
    let s = Stack::new("2026-11-17T17:00:00Z").await;
    s.slack
        .tz
        .lock()
        .unwrap()
        .insert("U0ANA".into(), "America/Los_Angeles".into());
    s.slack
        .tz
        .lock()
        .unwrap()
        .insert("U0BOB".into(), "Asia/Tokyo".into());
    // Carol has no time zone in Slack: the service default (Los Angeles).
    for who in ["ana", "bob", "carol"] {
        s.set_drafts(who, old_drafts(2));
    }
    let engine = s.engine();
    // 09:00 in Los Angeles, 02:00 Wednesday in Tokyo.
    assert_eq!(engine.tick().await, 2);
    let to: Vec<String> = s
        .posts()
        .iter()
        .map(|c| c.body["channel"].as_str().unwrap().to_string())
        .collect();
    assert!(to.contains(&"U0ANA".to_string()) && to.contains(&"U0CAROL".to_string()));
    // 08:30 Wednesday in Tokyo: not yet. 09:00: now.
    s.clock.set("2026-11-17T23:30:00Z");
    assert_eq!(engine.tick().await, 0);
    s.clock.set("2026-11-18T00:00:00Z");
    assert_eq!(engine.tick().await, 1);
    assert_eq!(s.posts().last().unwrap().body["channel"], "U0BOB");
    s.stop();
}

#[tokio::test]
async fn hours_and_days_off() {
    // (time, nudges expected) for a PM in Los Angeles.
    let cases = [
        ("2026-11-17T16:59:00Z", 0), // 08:59 Tuesday
        ("2026-11-17T17:00:00Z", 1), // 09:00
        ("2026-11-18T00:59:00Z", 1), // 16:59
        ("2026-11-18T01:00:00Z", 0), // 17:00: the day is over
        ("2026-11-21T20:00:00Z", 0), // Saturday noon
        ("2026-11-22T20:00:00Z", 0), // Sunday noon
        ("2026-11-23T20:00:00Z", 1), // Monday noon
    ];
    for (at, expected) in cases {
        let s = Stack::new(at).await;
        s.slack
            .tz
            .lock()
            .unwrap()
            .insert("U0ANA".into(), "America/Los_Angeles".into());
        s.set_drafts("ana", old_drafts(2));
        assert_eq!(s.engine().tick().await, expected, "at {at}");
        s.stop();
    }
}

#[tokio::test]
async fn not_now_dismisses_and_the_day_stays_used() {
    let s = Stack::new(TUE_9AM_LA).await;
    s.set_drafts("ana", old_drafts(3));
    let engine = s.engine();
    assert_eq!(engine.tick().await, 1);
    let open = &s.state_json()["pms"]["ana"]["open"];
    let (channel, ts) = (
        open["channel"].as_str().unwrap(),
        open["ts"].as_str().unwrap(),
    );
    assert_eq!(channel, "D0ANA");
    engine.handle_action(&click("U0ANA", channel, ts)).await;
    // The message loses its button.
    let updates = s.slack.calls("chat.update");
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].body["ts"], ts);
    assert_eq!(updates[0].body["text"], nudges::NOT_NOW_REPLY);
    assert!(!updates[0].body["blocks"].to_string().contains("Not now"));
    let st = s.state_json();
    assert_eq!(st["pms"]["ana"]["streak"], 1);
    assert!(st["pms"]["ana"]["open"].is_null());
    // Nothing more that day, however often it looks.
    assert_eq!(engine.tick().await, 0);
    // Someone else's click, or a stale one, counts nothing.
    engine.handle_action(&click("U0BOB", "D0BOB", ts)).await;
    engine.handle_action(&click("U0ANA", channel, ts)).await;
    assert_eq!(s.state_json()["pms"]["ana"]["streak"], 1);
    s.stop();
}

#[tokio::test]
async fn three_in_a_row_ignored_then_fourteen_quiet_days() {
    let s = Stack::new(TUE_9AM_LA).await;
    s.set_drafts("ana", old_drafts(3));
    let lines = MANY.to_vec();
    // Tue, Wed, Thu: sent, and never acted on.
    for day in ["17", "18", "19"] {
        s.clock.set(&format!("2026-11-{day}T17:00:00Z"));
        assert_eq!(s.engine_with(lines.clone()).tick().await, 1, "day {day}");
    }
    // Friday: the third went unanswered, so quiet. Nothing for 14 days.
    for at in [
        "2026-11-20T17:00:00Z",
        "2026-11-23T17:00:00Z",
        "2026-12-03T17:00:00Z",
    ] {
        s.clock.set(at);
        assert_eq!(s.engine_with(lines.clone()).tick().await, 0, "at {at}");
    }
    assert_eq!(s.state_json()["pms"]["ana"]["quiet_until"], "2026-12-04");
    assert_eq!(s.posts().len(), 3);
    // The fourteenth day after: back.
    s.clock.set("2026-12-04T17:00:00Z");
    assert_eq!(s.engine_with(lines).tick().await, 1);
    assert_eq!(s.posts().len(), 4);
    s.stop();
}

#[tokio::test]
async fn dismissing_counts_the_same_as_ignoring() {
    let s = Stack::new(TUE_9AM_LA).await;
    s.set_drafts("ana", old_drafts(3));
    let lines = MANY.to_vec();
    for day in ["17", "18", "19"] {
        s.clock.set(&format!("2026-11-{day}T17:00:00Z"));
        let engine = s.engine_with(lines.clone());
        assert_eq!(engine.tick().await, 1, "day {day}");
        let open = s.state_json()["pms"]["ana"]["open"].clone();
        let ts = open["ts"].as_str().unwrap().to_string();
        engine.handle_action(&click("U0ANA", "D0ANA", &ts)).await;
    }
    assert_eq!(s.state_json()["pms"]["ana"]["quiet_until"], "2026-12-03");
    s.clock.set("2026-11-20T17:00:00Z");
    assert_eq!(s.engine_with(lines).tick().await, 0);
    s.stop();
}

#[tokio::test]
async fn acting_on_drafts_resets_the_count() {
    let s = Stack::new(TUE_9AM_LA).await;
    s.set_drafts("ana", old_drafts(4));
    let lines = MANY.to_vec();
    for (day, drop_one) in [("17", false), ("18", false), ("19", true), ("20", false)] {
        s.clock.set(&format!("2026-11-{day}T17:00:00Z"));
        if drop_one {
            // The PM decided a draft since the last nudge.
            s.drafts
                .by_user
                .lock()
                .unwrap()
                .get_mut("ana")
                .unwrap()
                .remove(0);
        }
        assert_eq!(s.engine_with(lines.clone()).tick().await, 1, "day {day}");
    }
    // Ignored on Wed and Fri, but Thursday's decision reset the count in between.
    let st = s.state_json();
    assert!(st["pms"]["ana"]["quiet_until"].is_null());
    assert_eq!(st["pms"]["ana"]["streak"], 1);
    s.stop();
}

#[tokio::test]
async fn nudges_off_and_on() {
    let s = Stack::new(TUE_9AM_LA).await;
    s.set_drafts("ana", old_drafts(3));
    let engine = s.engine();
    assert!(!engine.dm_command("D0ANA", "1.1", "ana", "level").await);
    assert!(engine.dm_command("D0ANA", "1.1", "ana", "nudges off").await);
    let posts = s.posts();
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].body["channel"], "D0ANA");
    assert_eq!(posts[0].body["thread_ts"], "1.1");
    assert_eq!(text_of(&posts[0]), nudges::ANSWER_OFF);
    // Off, also after a restart, and on later days.
    assert_eq!(engine.tick().await, 0);
    s.clock.set("2026-11-18T17:00:00Z");
    assert_eq!(s.engine().tick().await, 0);
    assert_eq!(s.posts().len(), 1);
    // Back on: the next look sends.
    assert!(
        s.engine()
            .dm_command("D0ANA", "1.2", "ana", "Nudges ON")
            .await
    );
    assert_eq!(text_of(s.posts().last().unwrap()), nudges::ANSWER_ON);
    assert_eq!(s.engine().tick().await, 1);
    // Bob never opted out and is unaffected by Ana's setting.
    s.set_drafts("bob", old_drafts(2));
    s.clock.set("2026-11-19T17:00:00Z");
    assert!(s.engine().tick().await >= 1);
    s.stop();
}

#[tokio::test]
async fn asking_for_nudges_back_ends_the_quiet_period() {
    let s = Stack::new(TUE_9AM_LA).await;
    s.set_drafts("ana", old_drafts(3));
    let lines = MANY.to_vec();
    for day in ["17", "18", "19", "20"] {
        s.clock.set(&format!("2026-11-{day}T17:00:00Z"));
        s.engine_with(lines.clone()).tick().await;
    }
    assert_eq!(s.posts().len(), 3);
    s.engine_with(lines.clone()).set_enabled("ana", true);
    s.clock.set("2026-11-23T17:00:00Z");
    assert_eq!(s.engine_with(lines).tick().await, 1);
    s.stop();
}

#[tokio::test]
async fn each_line_is_shown_to_a_pm_once() {
    let s = Stack::new(TUE_9AM_LA).await;
    s.set_drafts("ana", old_drafts(6));
    s.set_drafts("bob", old_drafts(6));
    let mut seen: HashMap<String, Vec<String>> = HashMap::new();
    // Five weekdays, with Ana and Bob each deciding a draft every day (so no quiet period).
    for day in ["17", "18", "19", "20", "23"] {
        s.clock.set(&format!("2026-11-{day}T17:00:00Z"));
        for who in ["ana", "bob"] {
            let mut map = s.drafts.by_user.lock().unwrap();
            if day != "17" {
                map.get_mut(who).unwrap().remove(0);
            }
        }
        let before = s.posts().len();
        s.engine().tick().await;
        for call in &s.posts()[before..] {
            seen.entry(call.body["channel"].as_str().unwrap().to_string())
                .or_default()
                .push(text_of(call));
        }
    }
    // Three lines exist; each PM got each exactly once, then nothing.
    for who in ["U0ANA", "U0BOB"] {
        let texts = &seen[who];
        assert_eq!(texts.len(), nudge_copy::LINES.len(), "{who}: {texts:?}");
        let mut unique = texts.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), texts.len(), "{who} saw a line twice");
    }
    s.stop();
}

#[tokio::test]
async fn another_daily_message_counts_as_the_nudge() {
    let s = Stack::new(TUE_9AM_LA).await;
    s.set_drafts("ana", old_drafts(3));
    let engine = s.engine();
    engine.record_daily_message("ana");
    assert_eq!(engine.tick().await, 0, "the digest was today's message");
    // Remembered across a restart, and only for that day.
    assert_eq!(s.engine().tick().await, 0);
    s.clock.set("2026-11-18T17:00:00Z");
    assert_eq!(s.engine().tick().await, 1);
    s.stop();
}

#[tokio::test]
async fn an_unreachable_tool_server_sends_nothing_and_loses_no_day() {
    let s = Stack::new(TUE_9AM_LA).await;
    s.set_drafts("ana", old_drafts(3));
    s.drafts.down.store(true, Ordering::SeqCst);
    let engine = s.engine();
    assert_eq!(engine.tick().await, 0);
    s.drafts.down.store(false, Ordering::SeqCst);
    assert_eq!(engine.tick().await, 1);
    s.stop();
}

#[tokio::test]
async fn state_is_a_small_json_file_that_survives_a_restart() {
    let s = Stack::new(TUE_9AM_LA).await;
    s.set_drafts("ana", old_drafts(3));
    s.engine().tick().await;
    let st = s.state_json();
    assert_eq!(st["pms"]["ana"]["last_day"], "2026-11-17");
    assert_eq!(st["pms"]["ana"]["used"][0], "dw-expire");
    assert!(!s.state.path().join("nudges.tmp").exists());
    // A restart the same day, even with the lines used up elsewhere, sends nothing.
    assert_eq!(s.engine().tick().await, 0);
    s.stop();
}
