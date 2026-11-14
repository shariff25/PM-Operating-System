//! Configuration from environment variables.

use std::net::SocketAddr;
use std::time::Duration;

use crate::sync::SyncConfig;

#[derive(Debug, Clone)]
pub struct Config {
    /// `PMOS_DOCS_TOKEN`: required, at least 32 characters. Only the tool server and
    /// platform-owner PMs hold it.
    pub token: String,
    /// `PMOS_DOCS_DATABASE_URL`: required unless `PMOS_DEV=1` (in memory).
    pub database_url: Option<String>,
    /// `PMOS_DOCS_BIND`, default `127.0.0.1:8730`.
    pub bind: SocketAddr,
    /// `PMOS_DOCS_ALLOWED_HOSTS`: `Host` values `/mcp` accepts.
    pub allowed_hosts: Vec<String>,
    /// `PMOS_DOCS_SYNC_HOURS`, default 24. `0` turns the schedule off (sync only on demand).
    pub sync_every: Option<Duration>,
    pub sync: SyncConfig,
}

/// The shortest gap between requests to the portal.
pub const MIN_DELAY_MS: u64 = 250;

impl Config {
    pub fn from_env() -> Result<Self, String> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let token = get("PMOS_DOCS_TOKEN").ok_or("PMOS_DOCS_TOKEN is required")?;
        if token.len() < 32 {
            return Err("PMOS_DOCS_TOKEN must be at least 32 characters".into());
        }
        let dev = get("PMOS_DEV").as_deref() == Some("1");
        let database_url = get("PMOS_DOCS_DATABASE_URL").filter(|u| !u.is_empty());
        if database_url.is_none() && !dev {
            return Err("PMOS_DOCS_DATABASE_URL is required (or PMOS_DEV=1 for memory)".into());
        }
        let num = |k: &str, default: u64| -> Result<u64, String> {
            get(k).map_or(Ok(default), |v| {
                v.parse().map_err(|_| format!("{k} must be a number"))
            })
        };
        let hours = num("PMOS_DOCS_SYNC_HOURS", 24)?;
        let defaults = SyncConfig::default();
        let delay_ms = num("PMOS_DOCS_DELAY_MS", defaults.delay.as_millis() as u64)?;
        // The portal rate-limited a faster crawl; never go below 4 requests a second.
        if delay_ms < MIN_DELAY_MS {
            return Err(format!(
                "PMOS_DOCS_DELAY_MS must be at least {MIN_DELAY_MS} (the portal rate-limits faster crawls)"
            ));
        }
        let base = get("PMOS_DOCS_BASE_URL").unwrap_or(defaults.base.clone());
        if !base.starts_with("https://")
            && !base.starts_with("http://127.0.0.1")
            && !base.starts_with("http://localhost")
        {
            return Err("PMOS_DOCS_BASE_URL must be https://".into());
        }
        Ok(Self {
            token,
            database_url,
            bind: get("PMOS_DOCS_BIND")
                .unwrap_or_else(|| "127.0.0.1:8730".into())
                .parse()
                .map_err(|e| format!("PMOS_DOCS_BIND: {e}"))?,
            allowed_hosts: get("PMOS_DOCS_ALLOWED_HOSTS")
                .unwrap_or_else(|| "docs.pmos.internal,localhost,127.0.0.1".into())
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            sync_every: (hours > 0).then(|| Duration::from_secs(hours * 3600)),
            sync: SyncConfig {
                base,
                concurrency: num("PMOS_DOCS_CONCURRENCY", defaults.concurrency as u64)?.clamp(1, 4)
                    as usize,
                delay: Duration::from_millis(delay_ms),
                ..defaults
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn cfg(pairs: &[(&str, &str)]) -> Result<Config, String> {
        let map: HashMap<_, _> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Config::from_lookup(|k| map.get(k).cloned())
    }

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn the_crawl_cannot_be_made_too_fast() {
        let err = cfg(&[
            ("PMOS_DOCS_TOKEN", TOKEN),
            ("PMOS_DEV", "1"),
            ("PMOS_DOCS_DELAY_MS", "0"),
        ])
        .unwrap_err();
        assert!(err.contains("at least 250"), "{err}");
    }

    #[test]
    fn defaults_and_limits() {
        assert!(cfg(&[]).is_err());
        assert!(cfg(&[("PMOS_DOCS_TOKEN", TOKEN)])
            .unwrap_err()
            .contains("DATABASE_URL"));
        let ok = cfg(&[("PMOS_DOCS_TOKEN", TOKEN), ("PMOS_DEV", "1")]).unwrap();
        assert_eq!(ok.sync.base, "https://support.acme.com");
        assert_eq!(ok.sync.concurrency, 2);
        assert_eq!(
            ok.sync.delay,
            Duration::from_secs(1),
            "1 request/second by default"
        );
        assert_eq!(ok.sync_every, Some(Duration::from_secs(86_400)));
        let busy = cfg(&[
            ("PMOS_DOCS_TOKEN", TOKEN),
            ("PMOS_DEV", "1"),
            ("PMOS_DOCS_CONCURRENCY", "50"),
            ("PMOS_DOCS_SYNC_HOURS", "0"),
        ])
        .unwrap();
        assert_eq!(busy.sync.concurrency, 4, "politeness cap");
        assert_eq!(busy.sync_every, None);
        assert!(cfg(&[
            ("PMOS_DOCS_TOKEN", TOKEN),
            ("PMOS_DEV", "1"),
            ("PMOS_DOCS_BASE_URL", "http://evil.example")
        ])
        .is_err());
    }
}
