//! The gate's network link to the Ledger: one POST per batch (spec §7.3, "Gate link").

use async_trait::async_trait;
use gov_core::Envelope;
use serde_json::{json, Value};

use crate::{LedgerLink, LinkAnswer};

const MAX_ANSWER: usize = 64 * 1024;

/// Ships to a Ledger's listener at a fixed address.
pub struct HttpLedgerLink {
    url: String,
    http: reqwest::Client,
}

impl HttpLedgerLink {
    /// `base` is the Ledger's address, for example `http://ledger:8790`.
    pub fn new(base: &str) -> Result<Self, String> {
        Ok(HttpLedgerLink {
            url: format!("{}/v1/receipts", base.trim_end_matches('/')),
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .map_err(|e| e.to_string())?,
        })
    }
}

#[async_trait]
impl LedgerLink for HttpLedgerLink {
    async fn ship(&self, request: &Envelope, batch: &[Envelope]) -> Result<LinkAnswer, String> {
        let response = self
            .http
            .post(&self.url)
            .json(&json!({"request": request, "receipts": batch}))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let status = response.status();
        if !status.is_success() {
            return Err(format!("the Ledger answered {}", status.as_u16()));
        }
        // The answer is an acknowledgement and a word of refusal: read no more than that
        // could ever need (review finding GR-78).
        let mut response = response;
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
            if bytes.len() + chunk.len() > MAX_ANSWER {
                return Err("the Ledger's answer is larger than an answer can be".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        let body: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        let ack = serde_json::from_value(body.get("ack").cloned().unwrap_or(Value::Null))
            .map_err(|_| "the Ledger's answer holds no acknowledgement".to_string())?;
        Ok(LinkAnswer {
            ack,
            refused: body
                .get("refused")
                .and_then(Value::as_str)
                .map(str::to_string),
        })
    }
}

/// The largest answer to a renewal: a whole bundle and the revocation feed.
const MAX_RENEWED: usize = 8 * 1024 * 1024;

#[derive(Debug)]
pub enum RenewFailed {
    /// The Authority's bare refusal: this gate's key is not enrolled, is revoked, or the
    /// gate is disabled. Asking again at once changes nothing.
    Refused,
    Other(String),
}

/// Carries a gate's request to renew to its Authority's listener for gates.
pub struct HttpAuthorityLink {
    url: String,
    http: reqwest::Client,
}

impl HttpAuthorityLink {
    pub fn new(base: &str) -> Result<Self, String> {
        Ok(HttpAuthorityLink {
            url: format!("{}/v1/renew", base.trim_end_matches('/')),
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .map_err(|e| e.to_string())?,
        })
    }

    pub async fn renew(&self, request: &Envelope) -> Result<gov_core::renew::Renewed, RenewFailed> {
        let other = |e: reqwest::Error| RenewFailed::Other(e.to_string());
        let mut response = self
            .http
            .post(&self.url)
            .json(request)
            .send()
            .await
            .map_err(other)?;
        match response.status().as_u16() {
            200 => {}
            403 => return Err(RenewFailed::Refused),
            status => {
                return Err(RenewFailed::Other(format!(
                    "the Authority answered {status}"
                )))
            }
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(other)? {
            if bytes.len() + chunk.len() > MAX_RENEWED {
                return Err(RenewFailed::Other(
                    "the Authority's answer is larger than an answer can be".into(),
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).map_err(|e| RenewFailed::Other(e.to_string()))
    }
}
