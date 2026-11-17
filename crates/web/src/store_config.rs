//! Where state lives and whether the brief is on, from the environment. Kept apart from
//! [`crate::config::Config`] so the two can change independently.
//!
//! | Variable | Default | Notes |
//! | --- | --- | --- |
//! | `PMOS_WEB_DATABASE_URL` | none | Postgres for sessions, settings and follow-ups. Without it everything is in memory and a restart signs everyone out |
//! | `PMOS_WEB_BRIEF` | off | `1` turns on `GET /api/brief` and the follow-up routes; off, they answer `501 not_built` and the app hides the section |
//! | `PMOS_WEB_BRIEF_TOKEN` | none | The service token `PUT /internal/brief/{handle}` needs, at least 32 characters. Without it that route answers `501` |

use std::sync::Arc;

use chrono::Utc;

use crate::app::Deps;
use crate::followups::{BriefConfig, FollowUpStore, MemoryFollowUpStore};
use crate::pg::PgStore;
use crate::session::{MemorySessionStore, SessionStore};
use crate::settings::{MemorySettingsStore, SettingsStore};

#[derive(Clone, Default)]
pub struct StoreConfig {
    pub database_url: Option<String>,
    pub brief: BriefConfig,
}

impl std::fmt::Debug for StoreConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoreConfig")
            .field(
                "database_url",
                &self.database_url.as_ref().map(|_| "<redacted>"),
            )
            .field("brief", &self.brief)
            .finish()
    }
}

impl StoreConfig {
    pub fn from_env() -> Result<Self, String> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let set = |k: &str| get(k).filter(|v| !v.trim().is_empty());
        let enabled = match get("PMOS_WEB_BRIEF").as_deref().map(str::trim) {
            None | Some("") | Some("0") => false,
            Some("1") => true,
            Some(other) => {
                return Err(format!("PMOS_WEB_BRIEF must be 1 or 0, not `{other}`"));
            }
        };
        let token = match set("PMOS_WEB_BRIEF_TOKEN") {
            Some(t) if t.len() < 32 => {
                return Err("PMOS_WEB_BRIEF_TOKEN must be at least 32 characters".into());
            }
            other => other,
        };
        Ok(Self {
            database_url: set("PMOS_WEB_DATABASE_URL"),
            brief: BriefConfig { enabled, token },
        })
    }

    /// The stores: Postgres (migrated) with a database URL, memory without.
    pub async fn deps(&self) -> Result<Deps, String> {
        let (sessions, settings, followups): (
            Arc<dyn SessionStore>,
            Arc<dyn SettingsStore>,
            Arc<dyn FollowUpStore>,
        ) = match &self.database_url {
            Some(url) => {
                let pg = PgStore::connect(url)
                    .await
                    .map_err(|e| format!("PMOS_WEB_DATABASE_URL: {e}"))?;
                pg.migrate()
                    .await
                    .map_err(|e| format!("web migrations: {e}"))?;
                let pg = Arc::new(pg);
                (pg.clone(), pg.clone(), pg)
            }
            None => {
                tracing::warn!(
                    "no PMOS_WEB_DATABASE_URL: sessions, settings and follow-ups are in memory, \
                     so a restart signs everyone out and forgets their edits, suggestions and \
                     follow-ups"
                );
                (
                    Arc::new(MemorySessionStore::new()),
                    Arc::new(MemorySettingsStore::new()),
                    Arc::new(MemoryFollowUpStore::new()),
                )
            }
        };
        if self.brief.enabled && self.brief.token.is_none() {
            tracing::info!(
                "PMOS_WEB_BRIEF is on with no PMOS_WEB_BRIEF_TOKEN: nothing can fill the brief"
            );
        }
        Ok(Deps {
            sessions,
            settings,
            followups,
            brief: self.brief.clone(),
            clock: Arc::new(Utc::now),
            // Set by `main` from the environment (`admin_config`).
            admin: Default::default(),
        })
    }
}

/// `StoreConfig::from_env()` then `.deps()`: what `main` calls instead of
/// `Deps::in_memory()`.
pub async fn deps_from_env() -> Result<Deps, String> {
    StoreConfig::from_env()?.deps().await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(pairs: &[(&str, &str)]) -> Result<StoreConfig, String> {
        StoreConfig::from_lookup(|k| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.to_string())
        })
    }

    #[test]
    fn defaults_are_memory_and_no_brief() {
        let c = cfg(&[]).unwrap();
        assert!(c.database_url.is_none() && !c.brief.enabled && c.brief.token.is_none());
    }

    #[test]
    fn reads_and_checks_its_variables() {
        let token = "t".repeat(32);
        let c = cfg(&[
            ("PMOS_WEB_DATABASE_URL", "postgres://u:p@db/web"),
            ("PMOS_WEB_BRIEF", "1"),
            ("PMOS_WEB_BRIEF_TOKEN", &token),
        ])
        .unwrap();
        assert!(c.database_url.is_some() && c.brief.enabled);
        assert_eq!(c.brief.token.as_deref(), Some(token.as_str()));
        assert!(cfg(&[("PMOS_WEB_BRIEF", "yes")]).is_err());
        assert!(cfg(&[("PMOS_WEB_BRIEF_TOKEN", "short")]).is_err());
        // Debug never shows the URL's password or the token.
        let shown = format!("{c:?}");
        assert!(!shown.contains("u:p@") && !shown.contains(&token));
    }
}
