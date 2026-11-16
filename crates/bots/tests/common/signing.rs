//! Signing user assertions as the web app and the console do, for the assertion tests.

use pmos_assertion::{unix_now, Claims, Key, KeyRing, RequestBinding};
use pmos_bots::assertion::{parse_keys, Assertions, Mode, AUDIENCE, ISSUERS};
use pmos_contract::model::Role;
use reqwest::Method;
use serde_json::{json, Value};

use super::Env;

// Test keys only: fixed bytes, never used anywhere real.
pub const WEB_KEY: &str = "d2ViLWJvdHMtdGVzdC1rZXktMDEyMzQ1Njc4OWFiY2Q=";
pub const CONSOLE_KEY: &str = "Y29uc29sZS1ib3RzLXRlc3Qta2V5LTAxMjM0NTY3ODk=";

pub fn assertions(mode: Mode) -> Assertions {
    let keys = parse_keys(
        &format!("web:bots:w1:{WEB_KEY},console:bots:c1:{CONSOLE_KEY}"),
        "the bot runtime",
        |iss, aud| aud == AUDIENCE && ISSUERS.contains(&iss),
    )
    .unwrap();
    Assertions::new(mode, keys).unwrap()
}

pub fn ring(key: &str, id: &str) -> KeyRing {
    KeyRing::new(Key::from_base64(id, key).unwrap())
}

/// What a caller signs: everything but the time and the request.
pub struct Signer {
    pub ring: KeyRing,
    pub issuer: &'static str,
    pub audience: &'static str,
}

impl Signer {
    pub fn web() -> Self {
        Self {
            ring: ring(WEB_KEY, "w1"),
            issuer: "web",
            audience: "bots",
        }
    }

    pub fn console() -> Self {
        Self {
            ring: ring(CONSOLE_KEY, "c1"),
            issuer: "console",
            audience: "bots",
        }
    }

    pub fn sign_at(&self, sub: &str, method: &str, target: &str, body: &[u8], now: u64) -> String {
        let claims = Claims::new(self.issuer, self.audience, sub, &[Role::Pm], "ses_test").unwrap();
        let req = RequestBinding::new(method, target, body).unwrap();
        self.ring.sign(&claims, now, 60, Some(&req)).unwrap()
    }

    pub fn sign(&self, sub: &str, method: &str, target: &str, body: &[u8]) -> String {
        self.sign_at(sub, method, target, body, unix_now())
    }
}

/// Send `body` exactly as given (so the binding sees the same bytes).
pub async fn send(
    env: &Env,
    method: Method,
    path: &str,
    client: &str,
    user: Option<&str>,
    body: Option<&[u8]>,
    assertion: Option<&str>,
) -> (u16, Value) {
    let mut r = env.req(method, path, client, user);
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

pub fn web_task(user: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({"bot": "docs", "requested_by": user, "text": "hi", "source": "web"}))
        .unwrap()
}
