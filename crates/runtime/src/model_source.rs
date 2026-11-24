//! What source the model gateway serves now, read from its `/readyz` (slice H4,
//! `docs/openshell-any-host-spec.md` §3.6, "The runtime and the tool server cross-check the
//! gateway" and "Keeping every caller on one source").
//!
//! `/readyz` needs no token, so a service that must not be able to call the model (the tool
//! server) can still read it. The answer is kept for at most [`SOURCE_CACHE`] and dropped on
//! any failed read: a caller that can't read it gets `Err` and must refuse (fail closed).

use std::time::{Duration, Instant};

use pmos_contract::model_source::valid_reported;

/// How long one reading of the gateway's source is reused.
pub const SOURCE_CACHE: Duration = Duration::from_secs(10);

/// The gateway's `/readyz` for its base URL, with or without the OpenAI `/v1`.
pub fn readyz_url(gateway_url: &str) -> String {
    let base = gateway_url.trim_end_matches('/');
    let base = base.strip_suffix("/v1").unwrap_or(base);
    format!("{base}/readyz")
}

/// What the gateway's `/readyz` says about where prompts go (slice H12): its source, and the
/// approved list's digest, the build's commit and (under `hosted`) the effective data-class
/// ceiling. Names and digests only.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GatewayReport {
    /// `local`, `hosted-test`, `stub` or `hosted:<name>`.
    pub source: String,
    /// `release`, or `stack-test` for the test-only build (fake-data mode wherever seen).
    pub approved_list: Option<String>,
    pub approved_list_sha256: Option<String>,
    pub commit: Option<String>,
    /// Under `hosted`: the effective ceiling (`public`, `internal`, `restricted`).
    pub max_data_class: Option<String>,
}

impl GatewayReport {
    /// The ceiling as a class, when the gateway reports one. A value no build knows reads as
    /// the lowest class, `public` (fail closed).
    pub fn ceiling(&self) -> Option<pmos_contract::model::Sensitivity> {
        self.max_data_class.as_deref().map(|c| {
            pmos_contract::model::Sensitivity::parse(c)
                .unwrap_or(pmos_contract::model::Sensitivity::Public)
        })
    }

    /// Fake-data mode as the gateway reports it: `hosted-test`, or the test-only list
    /// (`hosted:stack-test`, or `approved_list: stack-test` whatever the source says).
    pub fn fake_data_only(&self) -> bool {
        pmos_contract::model_source::reported_fake_data_only(&self.source)
            || self.approved_list.as_deref() == Some("stack-test")
    }
}

/// Reads and caches the gateway's reported source.
pub struct GatewaySource {
    readyz: String,
    http: reqwest::Client,
    ttl: Duration,
    cache: tokio::sync::Mutex<Option<(Instant, GatewayReport)>>,
}

impl std::fmt::Debug for GatewaySource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GatewaySource")
            .field("ttl", &self.ttl)
            .finish_non_exhaustive()
    }
}

impl GatewaySource {
    /// `gateway_url`: the gateway's base (`http://gateway.pmos.internal:8740`, `/v1` or not).
    pub fn new(gateway_url: &str) -> Self {
        Self {
            readyz: readyz_url(gateway_url),
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(2))
                .timeout(Duration::from_secs(5))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("http client builds"),
            ttl: SOURCE_CACHE,
            cache: tokio::sync::Mutex::new(None),
        }
    }

    /// The same, reusing a reading for `ttl` (at most [`SOURCE_CACHE`]; tests use less).
    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl.min(SOURCE_CACHE);
        self
    }

    /// The source the gateway reports (`local`, `hosted-test`, `stub`, `hosted:<name>`), from
    /// a reading at most [`SOURCE_CACHE`] old. `Err` (why, with no URL) when the gateway can't
    /// be reached, answers with no readable `model_source`, or reports a form no service
    /// knows; the cached reading is dropped then, so the next call reads again.
    pub async fn current(&self) -> Result<String, String> {
        self.report().await.map(|r| r.source)
    }

    /// The whole report ([`GatewayReport`]), from the same cache as [`GatewaySource::current`].
    pub async fn report(&self) -> Result<GatewayReport, String> {
        let mut cached = self.cache.lock().await;
        if let Some((at, report)) = cached.as_ref() {
            if at.elapsed() < self.ttl {
                return Ok(report.clone());
            }
        }
        *cached = None;
        let report = self.read().await?;
        *cached = Some((Instant::now(), report.clone()));
        Ok(report)
    }

    /// Drops the cached reading, so the next [`GatewaySource::current`] reads afresh.
    pub async fn forget(&self) {
        *self.cache.lock().await = None;
    }

    async fn read(&self) -> Result<GatewayReport, String> {
        let res = self
            .http
            .get(&self.readyz)
            .send()
            .await
            .map_err(|_| "the model gateway can't be reached".to_string())?;
        // A `503` still says the source (a model server is down); anything else is no answer.
        let status = res.status().as_u16();
        if status != 200 && status != 503 {
            return Err(format!("the model gateway's /readyz answered {status}"));
        }
        let body: serde_json::Value = res
            .json()
            .await
            .map_err(|_| "the model gateway's /readyz isn't JSON".to_string())?;
        let text = |k: &str| {
            body.get(k)
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty() && s.len() <= 128)
                .map(String::from)
        };
        match body.get("model_source").and_then(|v| v.as_str()) {
            Some(s) if valid_reported(s) => Ok(GatewayReport {
                source: s.to_string(),
                approved_list: text("approved_list"),
                approved_list_sha256: text("approved_list_sha256"),
                commit: text("commit"),
                max_data_class: text("max_data_class"),
            }),
            Some(_) => {
                Err("the model gateway reports a model source this build doesn't know".into())
            }
            None => Err("the model gateway doesn't report its model source".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readyz_is_at_the_root() {
        for base in [
            "http://gateway.pmos.internal:8740/v1",
            "http://gateway.pmos.internal:8740/v1/",
            "http://gateway.pmos.internal:8740",
        ] {
            assert_eq!(readyz_url(base), "http://gateway.pmos.internal:8740/readyz");
        }
    }
}
