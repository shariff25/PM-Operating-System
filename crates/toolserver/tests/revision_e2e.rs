//! R2: text a bot revised can't carry restricted data past the audience check. The real bot
//! runtime and the real tool server: a draft built from internal sources, revised by a
//! separate task (the web app's "ask for changes") that read a restricted document. Saving
//! the accepted revision raises the item to what the revision run read, so approving it for
//! a channel with someone outside the team is refused (`409 audience_blocked`) and nothing is
//! posted. The level can only go up: a plain edit keeps the item's own, a level the caller
//! names is only a floor, and a run the tool server knows nothing about counts as
//! `restricted`. A revision task itself starts its session at the draft's level (the W6
//! carry-forward for revisions).

mod common;

use std::sync::Arc;

use chrono::Utc;
use pmos_bots::clients::{Gateway, ToolServer};
use pmos_bots::model::{Sensitivity, Source};
use pmos_bots::runtime::NewTask;
use pmos_bots::store::MemoryStore;
use pmos_bots::Runtime;
use pmos_toolserver::connectors::fake::SLACK_WITH_OUTSIDER;
use pmos_toolserver::{router, AppConfig};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

const SERVICE_TOKEN: &str = "test-service-token-0123456789abcdef";
const THREAD: &str = "1731601200.000600";
const OTHER_THREAD: &str = "1731601200.000700";

fn task(text: &str, floor: Option<Sensitivity>) -> NewTask {
    NewTask {
        bot: "pm-ana".into(),
        requested_by: "ana".into(),
        text: text.into(),
        source: Source::Web,
        slack: None,
        level: None,
        conversation: None,
        sensitivity_floor: floor,
    }
}

struct Stack {
    f: common::Fixture,
    rt: Runtime,
    base: String,
    cancel: CancellationToken,
}

impl Drop for Stack {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

async fn start() -> Stack {
    let f = common::fixture();
    let cancel = CancellationToken::new();
    let app = router(
        Arc::clone(&f.svc),
        AppConfig {
            service_token: SERVICE_TOKEN.into(),
            bot: None,
            console_token: None,
            content_root: f.root.clone(),
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
    // The runtime reads who may ask from this tool server's user directory (slice U4a).
    common::seed_users(
        &f.stores,
        vec![common::member(
            "ana",
            "U0ANA",
            vec![pmos_contract::model::Role::Pm],
        )],
    )
    .await;
    let people = Arc::new(pmos_bots::people::SnapshotClient::http(
        &base,
        SERVICE_TOKEN,
    ));
    people.refresh().await.expect("the directory answers");
    let catalog = pmos_bots::catalog::Catalog::load(&f.root, Utc::now().date_naive()).unwrap();
    let rt = Runtime::new(
        Arc::new(MemoryStore::default()),
        Arc::new(catalog),
        people,
        ToolServer::new(&base, SERVICE_TOKEN),
        // Only its model source is read (slice H4).
        Gateway::new(
            &common::gateway_serving("local").await,
            "test-gateway-token-0123456789abcd",
            pmos_contract::model_source::ModelSource::Local,
        ),
        cancel.child_token(),
    );
    Stack {
        f,
        rt,
        base,
        cancel,
    }
}

impl Stack {
    /// Claim `new` and run it as a worker would: the reads, then complete it with `answer`.
    /// Returns the task id and its session's starting level.
    async fn run(&self, new: NewTask, reads: &[&str], answer: &str) -> (String, Sensitivity) {
        let now = Utc::now();
        let t = self.rt.create_task(new, now).await.unwrap();
        let claim = self.rt.claim_now("w1", None, now).await.unwrap().unwrap();
        assert_eq!(claim.task.id, t.id);
        let s = self
            .f
            .svc
            .session(&claim.session_token, Utc::now())
            .await
            .unwrap()
            .unwrap();
        let started = s.sources_sensitivity;
        for path in reads {
            self.f
                .svc
                .call(&s, "context_read", json!({ "path": path }), Utc::now())
                .await
                .unwrap();
        }
        self.rt
            .complete(&t.id, Some("w1"), answer, vec![], Utc::now())
            .await
            .unwrap();
        (t.id, started)
    }

    /// An internal draft: a task that read an internal document drafts a reply to a channel
    /// with someone outside the team.
    async fn internal_draft(&self, thread: &str) -> String {
        let now = Utc::now();
        let t = self
            .rt
            .create_task(task("Answer the key management question", None), now)
            .await
            .unwrap();
        let claim = self.rt.claim_now("w1", None, now).await.unwrap().unwrap();
        assert_eq!(claim.task.id, t.id);
        let s = self
            .f
            .svc
            .session(&claim.session_token, Utc::now())
            .await
            .unwrap()
            .unwrap();
        self.f
            .svc
            .call(
                &s,
                "context_read",
                json!({"path": "context/products/key-management.md"}),
                Utc::now(),
            )
            .await
            .unwrap();
        let drafted = self
            .f
            .svc
            .call(
                &s,
                "slack_post_reply",
                json!({"channel": SLACK_WITH_OUTSIDER, "thread_ts": thread,
                       "text": "Key management is documented in the overview."}),
                Utc::now(),
            )
            .await
            .unwrap();
        drafted["approval_id"].as_str().unwrap().to_string()
    }

    async fn post(&self, method: reqwest::Method, path: &str, body: Value) -> (u16, Value) {
        let res = reqwest::Client::new()
            .request(method, format!("{}{path}", self.base))
            .bearer_auth(SERVICE_TOKEN)
            .json(&body)
            .send()
            .await
            .unwrap();
        let status = res.status().as_u16();
        (status, res.json().await.unwrap_or(Value::Null))
    }

    async fn save(
        &self,
        id: &str,
        thread: &str,
        text: &str,
        revision: Option<(&str, &str)>,
    ) -> Value {
        let mut body = json!({"args": {"channel": SLACK_WITH_OUTSIDER, "thread_ts": thread,
                                       "text": text}});
        if let Some((run, floor)) = revision {
            body["revision_run"] = json!(run);
            body["revision_sensitivity"] = json!(floor);
        }
        let (s, v) = self
            .post(
                reqwest::Method::PUT,
                &format!("/internal/approvals/{id}/draft?user=ana"),
                body,
            )
            .await;
        assert_eq!(s, 200, "{v}");
        v
    }

    async fn approve(&self, id: &str) -> (u16, Value) {
        self.post(
            reqwest::Method::POST,
            &format!("/internal/approvals/{id}/decision"),
            json!({"decided_by": "ana", "decision": "approve", "confirm_external": true}),
        )
        .await
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_accepted_revision_that_read_restricted_data_is_blocked_for_outsiders() {
    let stack = start().await;
    let id = stack.internal_draft(THREAD).await;
    let item = stack.f.svc.approval(&id).await.unwrap().unwrap();
    assert_eq!(item.sources_sensitivity, Some(Sensitivity::Internal));

    // The PM asks for a change: the revision task carries the draft's level as its floor (its
    // prompt holds the draft's text), and reads the restricted NDA while revising.
    let (revision, started) = stack
        .run(
            task("Revise this draft: …", Some(Sensitivity::Internal)),
            &["context/partners/globex-nda.md"],
            "Under the Globex NDA, key management ships with a joint roadmap.",
        )
        .await;
    assert_eq!(
        started,
        Sensitivity::Internal,
        "the revision starts at the draft's level"
    );

    // The PM accepts it: the web app saves the text naming the revision run, and claims
    // (wrongly) it was public. The tool server trusts its own sessions, not that.
    let saved = stack
        .save(
            &id,
            THREAD,
            "Under the Globex NDA, key management ships with a joint roadmap.",
            Some((&revision, "public")),
        )
        .await;
    assert_eq!(saved["sources_sensitivity"], "restricted", "{saved}");
    let event = stack.f.audit.events().last().cloned().unwrap();
    assert_eq!(
        event.detail.as_deref(),
        Some("a bot revision's text; sources internal -> restricted")
    );

    // Approve: blocked, and nothing is posted.
    let (s, v) = stack.approve(&id).await;
    assert_eq!(s, 409, "{v}");
    assert_eq!(v["code"], "audience_blocked", "{v}");
    assert!(stack.f.slack.writes().is_empty());
    let item = stack.f.svc.approval(&id).await.unwrap().unwrap();
    assert_eq!(item.status.as_str(), "pending");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_level_only_goes_up() {
    let stack = start().await;

    // A plain edit (text a person typed) keeps the item's own level.
    let id = stack.internal_draft(THREAD).await;
    let saved = stack.save(&id, THREAD, "Typed by the PM.", None).await;
    assert_eq!(saved["sources_sensitivity"], "internal");

    // A revision that read nothing starts at, and stays at, the draft's level; a floor the
    // caller names can raise it but never lower it.
    let (quiet, started) = stack
        .run(
            task("Revise: shorter", Some(Sensitivity::Internal)),
            &[],
            "Shorter.",
        )
        .await;
    assert_eq!(started, Sensitivity::Internal);
    let saved = stack
        .save(&id, THREAD, "Shorter.", Some((&quiet, "public")))
        .await;
    assert_eq!(saved["sources_sensitivity"], "internal");
    let (s, v) = stack.approve(&id).await;
    assert_eq!(s, 200, "an internal draft may go to the channel: {v}");
    assert_eq!(stack.f.slack.writes().len(), 1);

    // A run the tool server has no session for counts as restricted.
    let other = stack.internal_draft(OTHER_THREAD).await;
    let saved = stack
        .save(
            &other,
            OTHER_THREAD,
            "From nowhere.",
            Some(("tsk_unknown", "public")),
        )
        .await;
    assert_eq!(saved["sources_sensitivity"], "restricted");
    let (s, v) = stack.approve(&other).await;
    assert_eq!((s, v["code"].as_str()), (409, Some("audience_blocked")));
    // And once raised, a later plain edit doesn't lower it.
    let saved = stack.save(&other, OTHER_THREAD, "Typed again.", None).await;
    assert_eq!(saved["sources_sensitivity"], "restricted");
    assert_eq!(stack.f.slack.writes().len(), 1);
}

/// A revision of a restricted draft starts its session restricted: anything it drafts
/// itself is held to the same audience rule.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_revision_of_a_restricted_draft_starts_restricted() {
    let stack = start().await;
    let (_, started) = stack
        .run(
            task("Revise: …", Some(Sensitivity::Restricted)),
            &[],
            "Revised.",
        )
        .await;
    assert_eq!(started, Sensitivity::Restricted);
}
