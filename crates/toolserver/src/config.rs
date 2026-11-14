//! Configuration from environment variables.

use std::net::SocketAddr;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Config {
    /// `PMOS_CONTENT_ROOT`: the content repo checkout. Default `content`.
    pub content_root: PathBuf,
    /// `PMOS_BIND`: listen address. Default `127.0.0.1:8710`.
    pub bind: SocketAddr,
    /// `PMOS_SERVICE_TOKEN`: required; guards `/internal`. At least 32 characters.
    pub service_token: String,
    /// `PMOS_AUDIT_LOG`: JSON-lines audit file. Required unless `PMOS_DEV=1`.
    pub audit_log: Option<PathBuf>,
    /// `PMOS_ALLOWED_HOSTS`: comma-separated `Host` values for `/mcp`.
    /// Default `toolserver.pmos.internal,localhost,127.0.0.1`.
    pub allowed_hosts: Vec<String>,
    /// `PMOS_CONNECTORS`: only `fake` exists until 0c-2. Required, so nobody runs fakes by
    /// accident once real connectors land.
    pub connectors: String,
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let service_token = get("PMOS_SERVICE_TOKEN").ok_or("PMOS_SERVICE_TOKEN is required")?;
        if service_token.len() < 32 {
            return Err("PMOS_SERVICE_TOKEN must be at least 32 characters".into());
        }
        let dev = get("PMOS_DEV").as_deref() == Some("1");
        let audit_log = get("PMOS_AUDIT_LOG").map(PathBuf::from);
        if audit_log.is_none() && !dev {
            return Err("PMOS_AUDIT_LOG is required (or set PMOS_DEV=1 for a local run)".into());
        }
        let connectors = get("PMOS_CONNECTORS").ok_or("PMOS_CONNECTORS is required (fake)")?;
        if connectors != "fake" {
            return Err(format!(
                "PMOS_CONNECTORS={connectors} is not available yet; only `fake` until 0c-2"
            ));
        }
        let bind = get("PMOS_BIND")
            .unwrap_or_else(|| "127.0.0.1:8710".into())
            .parse()
            .map_err(|e| format!("PMOS_BIND: {e}"))?;
        let allowed_hosts = get("PMOS_ALLOWED_HOSTS")
            .unwrap_or_else(|| "toolserver.pmos.internal,localhost,127.0.0.1".into())
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        Ok(Self {
            content_root: get("PMOS_CONTENT_ROOT")
                .unwrap_or_else(|| "content".into())
                .into(),
            bind,
            service_token,
            audit_log,
            allowed_hosts,
            connectors,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn cfg(pairs: &[(&str, &str)]) -> Result<Config, String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Config::from_lookup(|k| map.get(k).cloned())
    }

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn requires_token_audit_and_connectors() {
        assert!(cfg(&[]).is_err());
        assert!(cfg(&[
            ("PMOS_SERVICE_TOKEN", "short"),
            ("PMOS_DEV", "1"),
            ("PMOS_CONNECTORS", "fake")
        ])
        .is_err());
        assert!(
            cfg(&[("PMOS_SERVICE_TOKEN", TOKEN), ("PMOS_CONNECTORS", "fake")])
                .unwrap_err()
                .contains("PMOS_AUDIT_LOG")
        );
        assert!(cfg(&[
            ("PMOS_SERVICE_TOKEN", TOKEN),
            ("PMOS_DEV", "1"),
            ("PMOS_CONNECTORS", "atlassian")
        ])
        .is_err());
        let ok = cfg(&[
            ("PMOS_SERVICE_TOKEN", TOKEN),
            ("PMOS_DEV", "1"),
            ("PMOS_CONNECTORS", "fake"),
        ])
        .unwrap();
        assert_eq!(ok.bind.port(), 8710);
        assert!(ok
            .allowed_hosts
            .contains(&"toolserver.pmos.internal".to_string()));
    }
}
