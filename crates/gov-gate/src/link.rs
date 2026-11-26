//! The gate's network link to the Ledger: one POST per batch (spec §7.3, "Gate link").

use async_trait::async_trait;
use gov_core::Envelope;
use serde_json::{json, Value};

use crate::{LedgerLink, LinkAnswer};

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
        let body: Value = response.json().await.map_err(|e| e.to_string())?;
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
