//! Configuration from environment variables.

use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

use crate::page::{normalize_prefix, Products, Site};
use crate::sync::SyncConfig;

/// What tools and Slack call the docs site when `PMOS_DOCS_NAME` is unset.
pub const DEFAULT_NAME: &str = "the public docs";

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
    /// `PMOS_DOCS_NAME`, default "the public docs": how tools name the site.
    pub name: String,
    /// `PMOS_DOCS_PRODUCTS`: the products file, loaded and checked at start-up. None: every
    /// page is `general`.
    pub products: Products,
    /// The site to index and how to crawl it. `None` when `PMOS_DOCS_BASE_URL` is unset:
    /// the server runs with an empty index and never syncs.
    pub sync: Option<SyncConfig>,
}

/// The shortest gap between requests to the docs site.
pub const MIN_DELAY_MS: u64 = 250;

impl Config {
    pub fn from_env() -> Result<Self, String> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        // Unset and empty are the same for every setting.
        let get = |k: &str| get(k).filter(|v| !v.trim().is_empty());
        let token = get("PMOS_DOCS_TOKEN").ok_or("PMOS_DOCS_TOKEN is required")?;
        if token.len() < 32 {
            return Err("PMOS_DOCS_TOKEN must be at least 32 characters".into());
        }
        let dev = get("PMOS_DEV").as_deref() == Some("1");
        let database_url = get("PMOS_DOCS_DATABASE_URL");
        if database_url.is_none() && !dev {
            return Err("PMOS_DOCS_DATABASE_URL is required (or PMOS_DEV=1 for memory)".into());
        }
        let num = |k: &str, default: u64| -> Result<u64, String> {
            get(k).map_or(Ok(default), |v| {
                v.parse().map_err(|_| format!("{k} must be a number"))
            })
        };
        let hours = num("PMOS_DOCS_SYNC_HOURS", 24)?;
        let defaults = SyncConfig::new(Site::new("https://docs.invalid"));
        let delay_ms = num("PMOS_DOCS_DELAY_MS", defaults.delay.as_millis() as u64)?;
        // A docs site may rate-limit a faster crawl; never go above 4 requests a second.
        if delay_ms < MIN_DELAY_MS {
            return Err(format!(
                "PMOS_DOCS_DELAY_MS must be at least {MIN_DELAY_MS} (docs sites rate-limit faster crawls)"
            ));
        }
        let products = match get("PMOS_DOCS_PRODUCTS") {
            None => Products::default(),
            Some(path) => Products::load(Path::new(path.trim()))
                .map_err(|e| format!("PMOS_DOCS_PRODUCTS: {e}"))?,
        };
        let prefix = |k: &str, v: &str| {
            normalize_prefix(v).ok_or_else(|| {
                format!("{k}: `{v}` is not a URL path prefix such as /docs/ (no `?`, `#` or `..`)")
            })
        };
        let prefixes = get("PMOS_DOCS_PAGE_PREFIXES")
            .unwrap_or_else(|| "/".into())
            .split(',')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(|p| prefix("PMOS_DOCS_PAGE_PREFIXES", p))
            .collect::<Result<Vec<_>, _>>()?;
        if prefixes.is_empty() {
            return Err("PMOS_DOCS_PAGE_PREFIXES names no prefix (use / for every page)".into());
        }
        let api_prefix = get("PMOS_DOCS_API_PREFIX")
            .map(|p| prefix("PMOS_DOCS_API_PREFIX", p.trim()))
            .transpose()?;
        let sync = match get("PMOS_DOCS_BASE_URL") {
            None => None,
            Some(base) => {
                let base = base.trim().trim_end_matches('/').to_string();
                if !base.starts_with("https://")
                    && !base.starts_with("http://127.0.0.1")
                    && !base.starts_with("http://localhost")
                {
                    return Err("PMOS_DOCS_BASE_URL must be https://".into());
                }
                let host_ok = reqwest::Url::parse(&base).is_ok_and(|u| {
                    u.host_str().is_some_and(|h| !h.is_empty())
                        && u.query().is_none()
                        && u.fragment().is_none()
                        && u.username().is_empty()
                });
                if !host_ok {
                    return Err(
                        "PMOS_DOCS_BASE_URL must be a site address such as https://docs.example.com"
                            .into(),
                    );
                }
                Some(SyncConfig {
                    site: Site {
                        base,
                        prefixes,
                        api_prefix,
                    },
                    products: products.clone(),
                    concurrency: num("PMOS_DOCS_CONCURRENCY", defaults.concurrency as u64)?
                        .clamp(1, 4) as usize,
                    delay: Duration::from_millis(delay_ms),
                    ..defaults
                })
            }
        };
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
            name: get("PMOS_DOCS_NAME")
                .map(|n| n.trim().to_string())
                .unwrap_or_else(|| DEFAULT_NAME.into()),
            products,
            sync,
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

    fn with(extra: &[(&'static str, &'static str)]) -> Result<Config, String> {
        let mut pairs = vec![("PMOS_DOCS_TOKEN", TOKEN), ("PMOS_DEV", "1")];
        pairs.extend_from_slice(extra);
        cfg(&pairs)
    }

    #[test]
    fn the_crawl_cannot_be_made_too_fast() {
        let err = with(&[("PMOS_DOCS_DELAY_MS", "0")]).unwrap_err();
        assert!(err.contains("at least 250"), "{err}");
    }

    #[test]
    fn defaults_and_limits() {
        assert!(cfg(&[]).is_err());
        assert!(cfg(&[("PMOS_DOCS_TOKEN", TOKEN)])
            .unwrap_err()
            .contains("DATABASE_URL"));
        let ok = with(&[]).unwrap();
        assert!(ok.sync.is_none(), "no docs site unless one is configured");
        assert_eq!(ok.name, "the public docs");
        assert_eq!(ok.products, Products::default());
        assert_eq!(ok.sync_every, Some(Duration::from_secs(86_400)));

        let site = with(&[("PMOS_DOCS_BASE_URL", "https://docs.example.com/")]).unwrap();
        let sync = site.sync.unwrap();
        assert_eq!(sync.site.base, "https://docs.example.com");
        assert_eq!(sync.site.prefixes, vec!["/"], "every page by default");
        assert_eq!(sync.site.api_prefix, None);
        assert_eq!(sync.concurrency, 2);
        assert_eq!(
            sync.delay,
            Duration::from_secs(1),
            "1 request/second by default"
        );

        let busy = with(&[
            ("PMOS_DOCS_BASE_URL", "https://docs.example.com"),
            ("PMOS_DOCS_CONCURRENCY", "50"),
            ("PMOS_DOCS_SYNC_HOURS", "0"),
        ])
        .unwrap();
        assert_eq!(busy.sync.unwrap().concurrency, 4, "politeness cap");
        assert_eq!(busy.sync_every, None);
        for bad in [
            "http://evil.example",
            "ftp://docs.example.com",
            "https://",
            "https://docs.example.com/?q=1",
            "https://user@docs.example.com",
        ] {
            assert!(with(&[("PMOS_DOCS_BASE_URL", bad)]).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_site_name_prefixes_and_api_prefix() {
        let c = with(&[
            ("PMOS_DOCS_BASE_URL", "https://help.example.com"),
            ("PMOS_DOCS_NAME", "the Acme Help Center"),
            ("PMOS_DOCS_PAGE_PREFIXES", "/docs/, apidocs ,"),
            ("PMOS_DOCS_API_PREFIX", "/apidocs/"),
        ])
        .unwrap();
        assert_eq!(c.name, "the Acme Help Center");
        let site = c.sync.unwrap().site;
        assert_eq!(site.prefixes, vec!["/docs/", "/apidocs/"]);
        assert_eq!(site.api_prefix.as_deref(), Some("/apidocs/"));
        assert!(with(&[("PMOS_DOCS_PAGE_PREFIXES", "/docs/../admin")])
            .unwrap_err()
            .contains("PMOS_DOCS_PAGE_PREFIXES"));
        assert!(with(&[("PMOS_DOCS_PAGE_PREFIXES", " , ")]).is_err());
        assert!(with(&[("PMOS_DOCS_API_PREFIX", "/a?b")]).is_err());
    }

    #[test]
    fn a_products_file_is_loaded_and_checked_at_start_up() {
        let dir = std::env::temp_dir().join(format!("pmos-docs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let good = dir.join("products.yaml");
        std::fs::write(
            &good,
            "products:\n  - id: vault\n    name: Acme Vault\n    keywords: [vault]\n",
        )
        .unwrap();
        let bad = dir.join("bad.yaml");
        std::fs::write(&bad, "products:\n  - id: general\n    keywords: [x]\n").unwrap();
        let get = |path: &std::path::Path| {
            let path = path.to_str().unwrap().to_string();
            Config::from_lookup(move |k| match k {
                "PMOS_DOCS_TOKEN" => Some(TOKEN.into()),
                "PMOS_DEV" => Some("1".into()),
                "PMOS_DOCS_BASE_URL" => Some("https://docs.example.com".into()),
                "PMOS_DOCS_PRODUCTS" => Some(path.clone()),
                _ => None,
            })
        };
        let ok = get(&good).unwrap();
        assert_eq!(ok.products.ids(), vec!["vault", "general"]);
        assert_eq!(ok.sync.unwrap().products, ok.products);
        assert!(get(&bad).unwrap_err().contains("reserved"));
        assert!(get(&dir.join("missing.yaml"))
            .unwrap_err()
            .contains("PMOS_DOCS_PRODUCTS"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
