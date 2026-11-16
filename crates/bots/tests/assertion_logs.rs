//! A refused user assertion is logged with its reason, and nothing secret. On its own in this
//! binary: the log capture is per thread, and tracing caches per call site across threads.

mod common;

use std::io::Write;
use std::sync::{Arc, Mutex};

use common::signing::*;
use common::*;
use pmos_bots::assertion::Mode;
use reqwest::Method;

/// A log sink for one test.
#[derive(Clone, Default)]
struct Logs(Arc<Mutex<Vec<u8>>>);

impl Write for Logs {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

// One thread, so the server's tasks log to this test's subscriber.
#[tokio::test(flavor = "current_thread")]
async fn refusals_log_the_reason_and_nothing_secret() {
    let logs = Logs::default();
    let sink = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || sink.clone())
        .with_max_level(tracing::Level::DEBUG)
        .with_ansi(false)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let env = Env::memory_with_assertions(assertions(Mode::Require)).await;
    let web = Signer::web();
    let body = web_task("ana");
    let good = web.sign("ana", "POST", "/v1/tasks", &body);
    let tampered = good.replace("sub=ana", "sub=ben");
    let (s, _) = send(
        &env,
        Method::POST,
        "/v1/tasks",
        "web",
        Some("ben"),
        Some(&body),
        Some(&tampered),
    )
    .await;
    assert_eq!(s, 401);
    let (s, _) = send(
        &env,
        Method::POST,
        "/v1/tasks",
        "web",
        Some("ana"),
        Some(&body),
        Some(&good),
    )
    .await;
    assert_eq!(s, 201);
    let (s, _) = send(
        &env,
        Method::POST,
        "/v1/tasks",
        "web",
        Some("ana"),
        Some(&body),
        None,
    )
    .await;
    assert_eq!(s, 401);
    env.finish().await;

    let text = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    assert!(text.contains("refused a user assertion"), "{text}");
    assert!(
        text.contains("bad signature") && text.contains("missing"),
        "{text}"
    );
    let sig = good.rsplit("sig=").next().unwrap();
    let nonce = good
        .split("nonce=")
        .nth(1)
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    for secret in [WEB_KEY, CONSOLE_KEY, &good, &tampered, sig, nonce, WEB] {
        assert!(!text.contains(secret), "the log holds {secret}");
    }
}
