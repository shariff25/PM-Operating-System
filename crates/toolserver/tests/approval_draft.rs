//! A PM's saved edit of a pending draft (`PUT /internal/approvals/{id}/draft`, the web app's
//! **Save**): it replaces the item's arguments, so the Slack card, the console and the web app
//! all show it, and an approve from any of them writes it. The same rules as an edit
//! decision: the owner only, pending only, no change to where the write goes, audited
//! without text. In memory and, with `PMOS_TEST_DATABASE_URL`, in Postgres.

mod common;

use std::sync::Arc;

use chrono::{Duration, Utc};
use common::Fixture;
use pmos_assertion::{unix_now, Claims, Key, KeyRing, RequestBinding};
use pmos_contract::model::Role;
use pmos_toolserver::app::BotAccess;
use pmos_toolserver::approvals::{Decision, Verdict};
use pmos_toolserver::assertion::{parse_keys, Assertions, Mode, AUDIENCE, ISSUERS};
use pmos_toolserver::audit::{Client, Outcome};
use pmos_toolserver::connectors::fake::{SLACK_ALL_TEAM, SLACK_PM, SLACK_WITH_OUTSIDER};
use pmos_toolserver::ledger::{LedgerOutcome, LedgerQuery};
use pmos_toolserver::service::DecisionError;
use pmos_toolserver::{router_full, AppConfig, PersonClients};
use reqwest::Method;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

const SERVICE_TOKEN: &str = "test-service-token-0123456789abcdef";
const BOT_TOKEN: &str = "test-bot-token-0123456789abcdef-0123";
const WEB_TOKEN: &str = "test-web-token-0123456789abcdef-01234";
const CONSOLE_TOKEN: &str = "test-console-token-0123456789abcdef-0";
// Test key only: fixed bytes, never used anywhere real.
const WEB_KEY: &str = "d2ViLXRvb2xzZXJ2ZXItdGVzdC1rZXktMDEyMzQ1Njc=";
const OTHER: &str = "ben";
const THREAD: &str = "1731600000.000100";
const EDITED: &str = "Yes, SSO ships in 4.2 (edited in the web app).";

struct Server {
    base: String,
    cancel: CancellationToken,
    mine: String,
    theirs: String,
    f: Fixture,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

async fn draft(f: &Fixture, user: &str, run: &str) -> String {
    draft_in(f, user, run, THREAD).await
}

async fn draft_in(f: &Fixture, user: &str, run: &str, thread: &str) -> String {
    let at = Utc::now();
    let s = f
        .svc
        .create_session(user, "reply-test", run, Some(Duration::hours(1)), at)
        .await
        .unwrap();
    let out = f
        .svc
        .call(
            &s,
            "slack_post_reply",
            json!({"channel": SLACK_ALL_TEAM, "thread_ts": thread, "text": "Yes."}),
            at,
        )
        .await
        .unwrap();
    out["approval_id"].as_str().unwrap().to_string()
}

async fn start_with(f: Fixture, mode: Mode) -> Server {
    let mine = draft(&f, SLACK_PM, "tsk_mine").await;
    let theirs = draft(&f, OTHER, "tsk_theirs").await;
    let keys = parse_keys(
        &format!("web:toolserver:w1:{WEB_KEY}"),
        "the tool server",
        |iss, aud| aud == AUDIENCE && ISSUERS.contains(&iss),
    )
    .unwrap();
    let cancel = CancellationToken::new();
    let app = router_full(
        Arc::clone(&f.svc),
        AppConfig {
            service_token: SERVICE_TOKEN.into(),
            bot: Some(BotAccess {
                token: BOT_TOKEN.into(),
                skills: vec!["docs".into()],
            }),
            console_token: Some(CONSOLE_TOKEN.into()),
            content_root: f.root.clone(),
            oauth: None,
            allowed_hosts: vec!["127.0.0.1".into(), "localhost".into()],
        },
        None,
        PersonClients {
            web_token: Some(WEB_TOKEN.into()),
            assertions: Arc::new(Assertions::new(mode, keys).unwrap()),
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
        mine,
        theirs,
        f,
    }
}

async fn start(mode: Mode) -> Server {
    let (f, _) = common::reply_fixture(false).await.unwrap();
    start_with(f, mode).await
}

fn sign(sub: &str, method: &str, target: &str, body: &[u8]) -> String {
    let ring = KeyRing::new(Key::from_base64("w1", WEB_KEY).unwrap());
    let claims = Claims::new("web", "toolserver", sub, &[Role::Pm], "ses_t").unwrap();
    let req = RequestBinding::new(method, target, body).unwrap();
    ring.sign(&claims, unix_now(), 60, Some(&req)).unwrap()
}

impl Server {
    async fn send(
        &self,
        method: Method,
        target: &str,
        token: &str,
        body: Option<&[u8]>,
        assertion: Option<&str>,
    ) -> (u16, Value) {
        let mut r = reqwest::Client::new()
            .request(method, format!("{}{target}", self.base))
            .bearer_auth(token);
        if let Some(a) = assertion {
            r = r.header("x-pmos-assertion", a);
        }
        if let Some(b) = body {
            r = r
                .header("content-type", "application/json")
                .body(b.to_vec());
        }
        let res = r.send().await.unwrap();
        let status = res.status().as_u16();
        let text = res.text().await.unwrap();
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    /// `pmos-web` saving `user`'s edit, signed for them.
    async fn save(&self, id: &str, user: &str, args: Value) -> (u16, Value) {
        let target = format!("/internal/approvals/{id}/draft?user={user}");
        let body = serde_json::to_vec(&json!({ "args": args })).unwrap();
        let a = sign(user, "PUT", &target, &body);
        self.send(Method::PUT, &target, WEB_TOKEN, Some(&body), Some(&a))
            .await
    }

    async fn item(&self, id: &str) -> Value {
        let (_, v) = self
            .send(
                Method::GET,
                &format!("/internal/approvals/{id}"),
                SERVICE_TOKEN,
                None,
                None,
            )
            .await;
        v
    }
}

fn reply(text: &str) -> Value {
    json!({"channel": SLACK_ALL_TEAM, "thread_ts": THREAD, "text": text})
}

#[tokio::test]
async fn an_edit_saved_in_the_web_app_is_what_slack_approves_and_writes() {
    let srv = start(Mode::Require).await;
    let before = srv.item(&srv.mine).await;
    assert_eq!(before["edited"], false);
    let (s, v) = srv.save(&srv.mine, SLACK_PM, reply(EDITED)).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["args"]["text"], EDITED);
    assert_eq!(v["edited"], true);
    assert!(v["edited_at"].is_string(), "{v}");
    assert_eq!(v["status"], "pending");
    assert_ne!(
        v["args_hash"], before["args_hash"],
        "the hash follows the text"
    );
    // Every reader sees it: the orchestrator here, the Slack bot's own read below.
    let shown = srv.item(&srv.mine).await;
    assert_eq!(shown["args"]["text"], EDITED);
    assert!(
        shown.get("drafted_args").is_none(),
        "the bot's own text is never served"
    );
    let read = format!("/internal/approvals/{}?user={SLACK_PM}", srv.mine);
    let (_, card) = srv.send(Method::GET, &read, BOT_TOKEN, None, None).await;
    assert_eq!(card["args"]["text"], EDITED);
    assert_eq!(card["edited"], true);

    // R4: approving on the card posted before the edit (its args hash) is refused as stale,
    // and an approve that says nothing about what was shown is refused too. Nothing is posted.
    let decide = format!("/internal/approvals/{}/decision", srv.mine);
    let old = &before["args_hash"].as_str().unwrap()[..16];
    for (body, want, code) in [
        (
            json!({"decided_by": SLACK_PM, "decision": "approve", "expected_args_hash": old}),
            409,
            "stale",
        ),
        (
            json!({"decided_by": SLACK_PM, "decision": "approve"}),
            400,
            "expected_args_hash_required",
        ),
        (
            json!({"decided_by": SLACK_PM, "decision": "approve", "expected_args_hash": "XYZ"}),
            400,
            "bad_expected_args_hash",
        ),
    ] {
        let body = serde_json::to_vec(&body).unwrap();
        let (s, v) = srv
            .send(Method::POST, &decide, BOT_TOKEN, Some(&body), None)
            .await;
        assert_eq!((s, v["code"].as_str()), (want, Some(code)), "{v}");
    }
    assert!(srv.f.slack.writes().is_empty());
    assert_eq!(srv.item(&srv.mine).await["status"], "pending");
    let stale = srv.f.audit.events();
    assert!(
        stale
            .iter()
            .any(|e| e.detail.as_deref().is_some_and(|d| d.starts_with("stale:"))),
        "the stale refusal is audited"
    );

    // The PM approves on the refreshed Slack card (the bot's token, decision `approve`, the
    // hash it shows): the edit is what is posted.
    let shown = &card["args_hash"].as_str().unwrap()[..16];
    let body = serde_json::to_vec(
        &json!({"decided_by": SLACK_PM, "decision": "approve", "expected_args_hash": shown}),
    )
    .unwrap();
    let (s, v) = srv
        .send(Method::POST, &decide, BOT_TOKEN, Some(&body), None)
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["status"], "approved");
    let writes = srv.f.slack.writes();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].args["text"], EDITED);
    // The ledger counts it as edited; the audit log has the save, without text.
    let rows = srv
        .f
        .svc
        .ledger(LedgerQuery::all(Utc::now()), Utc::now())
        .await
        .unwrap();
    let row = rows.iter().find(|r| r.id == srv.mine).unwrap();
    assert_eq!(row.outcome, LedgerOutcome::Edited);
    assert!(row.edit_distance.is_some_and(|d| d > 0.0), "{row:?}");
    let events = srv.f.audit.events();
    let saved: Vec<_> = events
        .iter()
        .filter(|e| e.tool == "approval.draft_saved")
        .collect();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].outcome, Outcome::Ok);
    assert_eq!(saved[0].client, Client::Web);
    assert_eq!(saved[0].output_ref.as_deref(), Some(srv.mine.as_str()));
    assert_eq!(saved[0].detail, None);
    let logged = serde_json::to_string(&events).unwrap();
    assert!(
        !logged.contains("edited in the web app"),
        "no text in the audit log"
    );
    assert!(events
        .iter()
        .any(|e| e.outcome == Outcome::Approved
            && e.detail.as_deref() == Some("edited before approval")));
    // Decided: a save is refused.
    let (s, v) = srv.save(&srv.mine, SLACK_PM, reply("Too late")).await;
    assert_eq!((s, v["code"].as_str()), (409, Some("not_pending")), "{v}");
}

#[tokio::test]
async fn a_saved_edit_keeps_the_target_the_owner_and_the_clients_rules() {
    let srv = start(Mode::Require).await;
    // Where it goes can't change.
    for moved in [
        json!({"channel": SLACK_WITH_OUTSIDER, "thread_ts": THREAD, "text": "x"}),
        json!({"channel": SLACK_ALL_TEAM, "thread_ts": "1731600000.000999", "text": "x"}),
    ] {
        let (s, v) = srv.save(&srv.mine, SLACK_PM, moved).await;
        assert_eq!(s, 422, "{v}");
        assert_eq!(v["code"], "target_changed");
    }
    assert_eq!(srv.item(&srv.mine).await["args"]["text"], "Yes.");
    // Arguments that don't fit the tool.
    let (s, _) = srv
        .save(&srv.mine, SLACK_PM, json!({"channel": SLACK_ALL_TEAM}))
        .await;
    assert_eq!(s, 400);
    // Someone else's item, or one that doesn't exist, is `404` alike.
    let (s, _) = srv.save(&srv.theirs, SLACK_PM, reply("x")).await;
    assert_eq!(s, 404);
    let (s, _) = srv.save("apr_nope", SLACK_PM, reply("x")).await;
    assert_eq!(s, 404);
    // Without an assertion (require), or signed for someone else.
    let target = format!("/internal/approvals/{}/draft?user={SLACK_PM}", srv.mine);
    let body = serde_json::to_vec(&json!({"args": reply("x")})).unwrap();
    let (s, v) = srv
        .send(Method::PUT, &target, WEB_TOKEN, Some(&body), None)
        .await;
    assert_eq!((s, v["code"].as_str()), (401, Some("bad_assertion")));
    let a = sign(OTHER, "PUT", &target, &body);
    let (s, _) = srv
        .send(Method::PUT, &target, WEB_TOKEN, Some(&body), Some(&a))
        .await;
    assert_eq!(s, 401);
    // The Slack bot (its edit is a decision) and the console (read-only) may not save.
    for token in [BOT_TOKEN, CONSOLE_TOKEN] {
        let (s, _) = srv
            .send(Method::PUT, &target, token, Some(&body), None)
            .await;
        assert_eq!(s, 403);
    }
    // Source markers may stay in the saved text; they are removed when it is written.
    let (s, v) = srv
        .save(&srv.mine, SLACK_PM, reply("Yes [[src-a1b2c3]]."))
        .await;
    assert_eq!(s, 200, "{v}");
    // Saving the bot's own text back is no edit at all.
    let (s, v) = srv.save(&srv.mine, SLACK_PM, reply("Yes.")).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["edited"], false);
    assert!(v.get("edited_at").is_none(), "{v}");
}

#[tokio::test]
async fn an_expired_slack_draft_loses_a_saved_edit_with_its_text() {
    let (f, _) = common::reply_fixture(false).await.unwrap();
    let srv = start_with(f, Mode::Off).await;
    let (s, _) = srv.save(&srv.mine, SLACK_PM, reply(EDITED)).await;
    assert_eq!(s, 200);
    let later = Utc::now() + Duration::hours(25);
    srv.f.svc.sweep_slack_drafts(later).await.unwrap();
    let item = srv.f.svc.approval(&srv.mine).await.unwrap().unwrap();
    assert_eq!(item.status.as_str(), "expired");
    assert!(item.drafted_args.is_none(), "the bot's text is gone too");
    assert_ne!(item.args["text"], EDITED);
}

/// The same in Postgres: the save, the ledger's edit and the expiry.
#[tokio::test]
async fn a_saved_edit_in_postgres() {
    let Some((f, Some(_pg))) = common::reply_fixture(true).await else {
        eprintln!("PMOS_TEST_DATABASE_URL not set; skipping the Postgres case");
        return;
    };
    let id = draft(&f, SLACK_PM, "tsk_pg").await;
    let now = Utc::now();
    let saved = f
        .svc
        .save_draft(Client::Web, &id, SLACK_PM, reply(EDITED), None, now)
        .await
        .unwrap();
    assert!(saved.edited() && saved.edited_at.is_some());
    assert_eq!(saved.drafted()["text"], "Yes.");
    let read = f.svc.approval(&id).await.unwrap().unwrap();
    assert_eq!(read.args["text"], EDITED);
    assert_eq!(read.drafted_args.as_ref().unwrap()["text"], "Yes.");
    // Someone else can't.
    assert!(f
        .svc
        .save_draft(Client::Web, &id, OTHER, reply("x"), None, now)
        .await
        .is_err());
    let shown = read.args_hash[..16].to_string();
    // A card from before the save: stale, in Postgres too.
    let before_save = pmos_toolserver::approvals::args_hash(&reply("Yes."));
    assert_eq!(
        f.svc
            .decide_verdict(
                Client::Bot,
                &id,
                SLACK_PM,
                Verdict::new(Decision::Approve, None).shown(Some(before_save[..16].into())),
                now,
            )
            .await
            .unwrap_err(),
        DecisionError::Decide(pmos_toolserver::approvals::DecideError::Stale)
    );
    let done = f
        .svc
        .decide_verdict(
            Client::Bot,
            &id,
            SLACK_PM,
            Verdict::new(Decision::Approve, None).shown(Some(shown)),
            now,
        )
        .await
        .unwrap();
    assert_eq!(done.status.as_str(), "approved");
    assert_eq!(f.slack.writes()[0].args["text"], EDITED);
    let rows = f.svc.ledger(LedgerQuery::all(now), now).await.unwrap();
    let row = rows.iter().find(|r| r.id == id).unwrap();
    assert_eq!(row.outcome, LedgerOutcome::Edited);
    // A second item expires with its edit and loses both texts.
    const OTHER_THREAD: &str = "1731600000.000777";
    let other = draft_in(&f, SLACK_PM, "tsk_pg2", OTHER_THREAD).await;
    let edit = json!({"channel": SLACK_ALL_TEAM, "thread_ts": OTHER_THREAD, "text": EDITED});
    f.svc
        .save_draft(Client::Web, &other, SLACK_PM, edit, None, now)
        .await
        .unwrap();
    f.svc
        .sweep_slack_drafts(now + Duration::hours(25))
        .await
        .unwrap();
    let gone = f.svc.approval(&other).await.unwrap().unwrap();
    assert_eq!(gone.status.as_str(), "expired");
    assert!(gone.drafted_args.is_none());
}
