//! The daily brief's feed to the web app (`brief_feed.rs`), end to end through the
//! notification loop: the fake Slack (`common`), the mock bots runtime (`support`) and a mock
//! pmos-web that records each `PUT /internal/brief/{handle}`.

mod common;
mod support;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::put;
use axum::Router;
use common::{serve, until};
use pmos_slack::brief_feed::Producer;
use pmos_slack::notify::Notifier;
use serde_json::{json, Value};
use support::{call_text, Stack};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

const WEB_TOKEN: &str = "test-web-brief-token-0123456789abcdef";

/// One request the mock pmos-web got: (handle, Authorization, Content-Type, body).
type Put = (String, String, String, Value);

#[derive(Default)]
struct MockWeb {
    puts: Mutex<Vec<Put>>,
    /// The status to answer with; `200` when unset.
    status: Mutex<Option<StatusCode>>,
}

async fn web_put(
    State(web): State<Arc<MockWeb>>,
    Path(handle): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> (StatusCode, &'static str) {
    let header = |k: &str| {
        headers
            .get(k)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string()
    };
    web.puts.lock().unwrap().push((
        handle,
        header("authorization"),
        header("content-type"),
        serde_json::from_slice(&body).unwrap_or(Value::Null),
    ));
    let status = web.status.lock().unwrap().unwrap_or(StatusCode::OK);
    (status, "{}")
}

async fn mock_web() -> (Arc<MockWeb>, String, CancellationToken) {
    let web = Arc::new(MockWeb::default());
    let (base, stop) = serve(
        Router::new()
            .route("/internal/brief/{handle}", put(web_put))
            .with_state(web.clone()),
    )
    .await;
    (web, base, stop)
}

fn producer(base: &str) -> Producer {
    Producer::new(base, WEB_TOKEN, reqwest::Client::new()).unwrap()
}

/// Ana's daily brief routine, as `brief me at …` makes it.
const ROUTINE: &str = "rtn_brief_ana";

fn start(stack: &Stack, producer: Option<Producer>) -> (CancellationToken, JoinHandle<()>) {
    {
        let mut routines = stack.runtime.routines.lock().unwrap();
        if !routines.iter().any(|r| r["id"] == ROUTINE) {
            let mut r = pmos_slack::brief::draft("ana", 8, 30, "UTC");
            r["id"] = json!(ROUTINE);
            routines.push(r);
        }
    }
    let shutdown = CancellationToken::new();
    let mut notifier =
        Notifier::new(stack.bots.clone(), stack.state.path()).with_brief_feed(producer);
    notifier.wait = Duration::from_millis(200);
    (shutdown.clone(), tokio::spawn(notifier.run(shutdown)))
}

const BRIEF: &str = "Morning, Ana. One person is waiting on you.\n\n**Waiting on you**\n- Ben in #pm-team asked about SAML SSO, 2 days ago. ([Slack](https://slack.com/archives/C0PMTEAM/p1731600000000100))";

fn block() -> String {
    let today = chrono::Utc::now().date_naive();
    let asked = today - chrono::Days::new(2);
    format!(
        "```pmos-brief\n{}\n```",
        json!({
            "version": 1, "date": today.to_string(),
            "changed": [{"system": "jira", "title": "PAY-212 moved to Blocked",
                         "summary": "waiting on the vendor sandbox",
                         "url": "https://jira.example.com/browse/PAY-212"}],
            "waiting_on_you": [{"system": "slack", "who": "Ben",
                                "summary": "asked about SAML SSO for the console",
                                "url": "https://slack.com/archives/C0PMTEAM/p1731600000000100",
                                "asked_on": asked.to_string()}],
        })
    )
}

/// A finished run of Ana's brief routine for `handle` by `bot`, with `text` as its answer.
fn done(id: &str, bot: &str, handle: &str, text: &str) -> Value {
    json!({"id": id, "task_id": format!("tsk_{id}"), "bot": bot, "requested_by": handle,
           "kind": "done", "text": text, "routine_id": ROUTINE})
}

/// A finished task that isn't a brief routine's run.
fn done_plain(id: &str, text: &str) -> Value {
    let mut d = done(id, "pm-ana", "ana", text);
    d.as_object_mut().unwrap().remove("routine_id");
    d
}

fn push(stack: &Stack, items: &[Value]) {
    stack
        .runtime
        .feed
        .lock()
        .unwrap()
        .extend(items.iter().cloned());
}

/// The text of the `n`th `chat.postMessage`, once it has arrived.
async fn posted(stack: &Stack, n: usize) -> String {
    call_text(&stack.calls("chat.postMessage", n).await[n - 1])
}

#[tokio::test]
async fn a_brief_is_put_with_the_token_and_shown_without_its_block() {
    let stack = Stack::new().await;
    let (web, base, stop_web) = mock_web().await;
    let (stop, notifier) = start(&stack, Some(producer(&base)));
    push(
        &stack,
        &[done(
            "ntf_1",
            "pm-ana",
            "ana",
            &format!("{BRIEF}\n\n{}", block()),
        )],
    );

    let shown = posted(&stack, 1).await;
    assert!(shown.contains("SAML SSO"), "{shown}");
    assert!(
        !shown.contains("pmos-brief") && !shown.contains("\\\"version\\\""),
        "{shown}"
    );
    let calls = stack.slack.calls("chat.postMessage");
    assert_eq!(
        calls[0].body["channel"], "U0ANA",
        "the brief goes to Ana's DM"
    );

    let (handle, auth, content_type, body) =
        until("the PUT", || web.puts.lock().unwrap().first().cloned()).await;
    assert_eq!(handle, "ana");
    assert_eq!(auth, format!("Bearer {WEB_TOKEN}"));
    assert!(
        content_type.starts_with("application/json"),
        "{content_type}"
    );
    let today = chrono::Utc::now().date_naive();
    let asked = (today - chrono::Days::new(2)).to_string();
    assert_eq!(
        body,
        json!({
            "date": today.to_string(),
            "changed": [{"system": "jira", "title": "PAY-212 moved to Blocked",
                         "summary": "waiting on the vendor sandbox",
                         "url": "https://jira.example.com/browse/PAY-212"}],
            "waiting_on_you": [{"system": "slack", "who": "Ben",
                                "summary": "asked about SAML SSO for the console",
                                "url": "https://slack.com/archives/C0PMTEAM/p1731600000000100",
                                "created_at": format!("{asked}T00:00:00Z")}],
            "you_wait_on": [],
        })
    );
    stop.cancel();
    let _ = notifier.await;
    stop_web.cancel();
    stack.stop().await;
}

#[tokio::test]
async fn a_failing_web_app_never_holds_up_the_brief() {
    let stack = Stack::new().await;
    let (web, base, stop_web) = mock_web().await;
    *web.status.lock().unwrap() = Some(StatusCode::INTERNAL_SERVER_ERROR);
    // Nothing listens here: bind a port, then let it go.
    let closed = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", l.local_addr().unwrap())
    };

    // A 500: the brief is posted and the loop goes on to the next item.
    let (stop, notifier) = start(&stack, Some(producer(&base)));
    push(
        &stack,
        &[
            done("ntf_1", "pm-ana", "ana", &format!("{BRIEF}\n\n{}", block())),
            done("ntf_2", "pm-ana", "ana", "Second answer."),
        ],
    );
    assert!(posted(&stack, 1).await.contains("SAML SSO"));
    assert!(posted(&stack, 2).await.contains("Second answer."));
    until("the PUT", || web.puts.lock().unwrap().first().cloned()).await;
    stop.cancel();
    let _ = notifier.await;

    // Connection refused: the same.
    let (stop, notifier) = start(&stack, Some(producer(&closed)));
    push(
        &stack,
        &[
            done("ntf_3", "pm-ana", "ana", &format!("{BRIEF}\n\n{}", block())),
            done("ntf_4", "pm-ana", "ana", "Fourth answer."),
        ],
    );
    let third = posted(&stack, 3).await;
    assert!(third.contains("SAML SSO") && !third.contains("pmos-brief"));
    assert!(posted(&stack, 4).await.contains("Fourth answer."));
    stop.cancel();
    let _ = notifier.await;
    stop_web.cancel();
    stack.stop().await;
}

#[tokio::test]
async fn a_bad_block_or_someone_elses_bot_sends_nothing() {
    let stack = Stack::new().await;
    let (web, base, stop_web) = mock_web().await;
    let (stop, notifier) = start(&stack, Some(producer(&base)));
    let bad = "```pmos-brief\n{\"version\": 1, \"waiting_on_you\": [{\"system\": \"slack\", \"who\": \"Ben\", \"summary\": \"line one\\nline two\", \"url\": \"https://slack.com/x\"}]}\n```";
    push(
        &stack,
        &[
            // Malformed: a two-line summary.
            done("ntf_1", "pm-ana", "ana", &format!("{BRIEF}\n\n{bad}")),
            // Well formed, but not Ana's own bot.
            done("ntf_2", "docs", "ana", &format!("{BRIEF}\n\n{}", block())),
            done("ntf_3", "pm-bob", "ana", &format!("{BRIEF}\n\n{}", block())),
            // Someone not on the team.
            done("ntf_4", "pm-zed", "zed", &format!("{BRIEF}\n\n{}", block())),
        ],
    );
    for n in 1..=3 {
        let shown = posted(&stack, n).await;
        assert!(
            shown.contains("SAML SSO") && !shown.contains("pmos-brief"),
            "{shown}"
        );
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(web.puts.lock().unwrap().is_empty());
    stop.cancel();
    let _ = notifier.await;
    stop_web.cancel();
    stack.stop().await;
}

#[tokio::test]
async fn only_a_brief_task_sends_its_block() {
    let stack = Stack::new().await;
    let (web, base, stop_web) = mock_web().await;
    stack.runtime.routines.lock().unwrap().push(json!({
        "id": "rtn_other", "owner": "ana", "bot": "pm-ana",
        "prompt": "[reply-assistant] Draft replies", "schedule": {"cron": "0 8 * * 1-5", "tz": "UTC"},
    }));
    let (stop, notifier) = start(&stack, Some(producer(&base)));
    let with_block = format!(
        "{BRIEF}

{}",
        block()
    );
    let mut other_routine = done("ntf_2", "pm-ana", "ana", &with_block);
    other_routine["routine_id"] = json!("rtn_other");
    let mut unknown_routine = done("ntf_3", "pm-ana", "ana", &with_block);
    unknown_routine["routine_id"] = json!("rtn_nope");
    push(
        &stack,
        &[
            // An ordinary answer that contains a block (a page quoting one, a prompt injection):
            // the block is hidden and nothing is sent.
            done_plain("ntf_1", &with_block),
            // Another routine's run, or a routine Ana doesn't have.
            other_routine,
            unknown_routine,
        ],
    );
    for n in 1..=3 {
        let shown = posted(&stack, n).await;
        assert!(!shown.contains("pmos-brief"), "{shown}");
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(web.puts.lock().unwrap().is_empty());
    // A `brief now` this service started is a brief: marked at creation.
    pmos_slack::brief::mark_brief_now("tsk_ntf_4");
    push(&stack, &[done_plain("ntf_4", &with_block)]);
    let (handle, ..) = until("the PUT", || web.puts.lock().unwrap().first().cloned()).await;
    assert_eq!(handle, "ana");
    stop.cancel();
    let _ = notifier.await;
    stop_web.cancel();
    stack.stop().await;
}

#[tokio::test]
async fn a_saved_cursor_ahead_of_the_runtime_starts_again_from_now() {
    let stack = Stack::new().await;
    // Saved before the runtime's database was reset: far past its feed.
    let cursor_file = stack.state.path().join(pmos_slack::notify::CURSOR_FILE);
    std::fs::write(&cursor_file, "900").unwrap();
    let (stop, notifier) = start(&stack, None);
    // Without the reset nothing new would ever be posted; with it, the next answer is.
    until("the reset cursor", || {
        std::fs::read_to_string(&cursor_file)
            .ok()
            .filter(|c| c.trim() == "0")
    })
    .await;
    push(&stack, &[done_plain("ntf_1", "After the reset.")]);
    assert!(posted(&stack, 1).await.contains("After the reset."));
    stop.cancel();
    let _ = notifier.await;
    stack.stop().await;
}

#[tokio::test]
async fn with_the_feed_off_the_block_is_still_hidden() {
    let stack = Stack::new().await;
    let (stop, notifier) = start(&stack, None);
    push(
        &stack,
        &[done(
            "ntf_1",
            "pm-ana",
            "ana",
            &format!("{BRIEF}\n\n{}", block()),
        )],
    );
    let shown = posted(&stack, 1).await;
    assert!(
        shown.contains("SAML SSO") && !shown.contains("pmos-brief"),
        "{shown}"
    );
    stop.cancel();
    let _ = notifier.await;
    stack.stop().await;
}

/// Slice R2: a brief Chief wrote reaches the web app's panels as Nudge's does, and says who
/// wrote it (from the runtime's notification, never from the block); Nudge's says nothing.
#[tokio::test]
async fn a_brief_chief_wrote_still_fills_the_panels_and_names_chief() {
    let stack = Stack::new().await;
    let (web, base, stop_web) = mock_web().await;
    let (stop, notifier) = start(&stack, Some(producer(&base)));
    let text = format!("{BRIEF}\n\n{}", block());
    let mut chiefs = done("ntf_1", "pm-ana", "ana", &text);
    chiefs["persona"] = json!("chief");
    push(&stack, &[chiefs]);
    let shown = posted(&stack, 1).await;
    assert!(
        shown.contains("SAML SSO") && !shown.contains("pmos-brief"),
        "{shown}"
    );
    let (handle, _, _, body) = until("the PUT", || web.puts.lock().unwrap().first().cloned()).await;
    assert_eq!(handle, "ana");
    assert_eq!(body["persona"], "chief");
    assert_eq!(body["changed"].as_array().unwrap().len(), 1);
    assert_eq!(body["waiting_on_you"].as_array().unwrap().len(), 1);

    // Nudge's (`nudge`, or no persona) and a key not shaped like one: no `persona` sent.
    let mut nudges = done("ntf_2", "pm-ana", "ana", &text);
    nudges["persona"] = json!("nudge");
    let mut odd = done("ntf_3", "pm-ana", "ana", &text);
    odd["persona"] = json!("<!channel>");
    push(&stack, &[nudges, odd]);
    let puts = until("three PUTs", || {
        let p = web.puts.lock().unwrap().clone();
        (p.len() >= 3).then_some(p)
    })
    .await;
    for (_, _, _, body) in &puts[1..] {
        assert!(body.get("persona").is_none(), "{body}");
    }
    stop.cancel();
    let _ = notifier.await;
    stop_web.cancel();
    stack.stop().await;
}
