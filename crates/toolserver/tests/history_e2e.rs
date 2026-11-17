//! End to end, the bot runtime against a real tool server (W6): task 1 in a conversation
//! reads a restricted source; task 2 in the same conversation replays task 1's answer and
//! drafts a reply to a channel with someone outside the team. The draft is blocked, though
//! task 2's own session read nothing, because the runtime started it at task 1's level.

mod common;

use std::sync::Arc;

use chrono::{Duration, Utc};
use common::fixture;
use pmos_bots::clients::{Gateway, ToolServer};
use pmos_bots::model::{Sensitivity, Source};
use pmos_bots::runtime::NewTask;
use pmos_bots::store::MemoryStore;
use pmos_bots::Runtime;
use pmos_toolserver::connectors::fake::{SLACK_ALL_TEAM, SLACK_WITH_OUTSIDER};
use pmos_toolserver::{router, AppConfig};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

const SERVICE_TOKEN: &str = "test-service-token-0123456789abcdef";

fn task(text: &str, conversation: &str) -> NewTask {
    NewTask {
        bot: "pm-ana".into(),
        requested_by: "ana".into(),
        text: text.into(),
        source: Source::Web,
        slack: None,
        level: None,
        conversation: Some(conversation.into()),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_follow_up_cannot_draft_restricted_history_to_outsiders() {
    let f = fixture();
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

    let catalog = pmos_bots::catalog::Catalog::load(&f.root, Utc::now().date_naive()).unwrap();
    let rt = Runtime::new(
        Arc::new(MemoryStore::default()),
        Arc::new(catalog),
        ToolServer::new(&base, SERVICE_TOKEN),
        // Never called here.
        Gateway::new("http://127.0.0.1:9/v1", "test-gateway-token-0123456789abcd"),
        cancel.child_token(),
    );
    let earlier = Utc::now() - Duration::minutes(2);

    // Task 1 reads the restricted NDA and answers.
    let t1 = rt
        .create_task(task("What does the Globex NDA say?", "cnv_w6"), earlier)
        .await
        .unwrap();
    let c1 = rt.claim_now("w1", None, earlier).await.unwrap().unwrap();
    assert_eq!(c1.task.id, t1.id);
    assert!(c1.history.is_empty());
    let s1 = f
        .svc
        .session(&c1.session_token, Utc::now())
        .await
        .unwrap()
        .unwrap();
    f.svc
        .call(
            &s1,
            "context_read",
            json!({"path": "context/partners/globex-nda.md"}),
            Utc::now(),
        )
        .await
        .unwrap();
    let done = rt
        .complete(
            &t1.id,
            Some("w1"),
            "Under the NDA: a joint roadmap.",
            vec![],
            earlier,
        )
        .await
        .unwrap();
    assert_eq!(done.sources_sensitivity, Some(Sensitivity::Restricted));

    // Task 2 in the same conversation: its session starts restricted.
    let now = Utc::now();
    let t2 = rt
        .create_task(task("Post that answer in #field-questions", "cnv_w6"), now)
        .await
        .unwrap();
    let c2 = rt.claim_now("w1", None, now).await.unwrap().unwrap();
    assert_eq!(c2.task.id, t2.id);
    assert_eq!(c2.history.len(), 1);
    assert_eq!(c2.history_sensitivity, Some(Sensitivity::Restricted));
    let s2 = f
        .svc
        .session(&c2.session_token, Utc::now())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(s2.sources_sensitivity, Sensitivity::Restricted);

    // It drafts the answer to a channel with an outsider, reading nothing itself.
    let drafted = f
        .svc
        .call(
            &s2,
            "slack_post_reply",
            json!({"channel": SLACK_WITH_OUTSIDER, "thread_ts": "1731601200.000600",
                   "text": "Under the NDA: a joint roadmap."}),
            Utc::now(),
        )
        .await
        .unwrap();
    let id = drafted["approval_id"].as_str().unwrap().to_string();

    // Approving it is blocked, and nothing is posted.
    let res = reqwest::Client::new()
        .post(format!("{base}/internal/approvals/{id}/decision"))
        .bearer_auth(SERVICE_TOKEN)
        .json(&json!({"decided_by": "ana", "decision": "approve", "confirm_external": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 409);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["code"], "audience_blocked", "{body}");
    assert!(f.slack.writes().is_empty());

    // A different conversation starts fresh: a draft to an all-team channel is not blocked
    // by anything it didn't read (the control).
    let t3 = rt
        .create_task(task("Say hi in #pm-team", "cnv_other"), Utc::now())
        .await
        .unwrap();
    let c3 = rt.claim_now("w1", None, Utc::now()).await.unwrap().unwrap();
    assert_eq!(c3.task.id, t3.id);
    assert_eq!(c3.history_sensitivity, None);
    let s3 = f
        .svc
        .session(&c3.session_token, Utc::now())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(s3.sources_sensitivity, Sensitivity::Public);
    let drafted = f
        .svc
        .call(
            &s3,
            "slack_post_reply",
            json!({"channel": SLACK_ALL_TEAM, "thread_ts": "1731600000.000100", "text": "Hi."}),
            Utc::now(),
        )
        .await
        .unwrap();
    assert_eq!(drafted["status"], "pending_approval", "{drafted}");
    cancel.cancel();
}
