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

/// Reads and caches the gateway's reported source.
pub struct GatewaySource {
    readyz: String,
    http: reqwest::Client,
    ttl: Duration,
    cache: tokio::sync::Mutex<Option<(Instant, String)>>,
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
        let mut cached = self.cache.lock().await;
        if let Some((at, source)) = cached.as_ref() {
            if at.elapsed() < self.ttl {
                return Ok(source.clone());
            }
        }
        *cached = None;
        let source = self.read().await?;
        *cached = Some((Instant::now(), source.clone()));
        Ok(source)
    }

    /// Drops the cached reading, so the next [`GatewaySource::current`] reads afresh.
    pub async fn forget(&self) {
        *self.cache.lock().await = None;
    }

    async fn read(&self) -> Result<String, String> {
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
        match body.get("model_source").and_then(|v| v.as_str()) {
            Some(s) if valid_reported(s) => Ok(s.to_string()),
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
