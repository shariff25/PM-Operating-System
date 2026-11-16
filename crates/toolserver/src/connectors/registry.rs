//! The provider registry: every account provider PM OS can link, declared once.
//!
//! Before this table existed, the list `atlassian, slack, github` and the system-to-provider
//! mapping were repeated in the vault, OAuth, onboarding, the "no working account" message and
//! Slack Home. Those places now ask the registry. The protocol work that really differs per
//! provider (authorize URL, code exchange, refresh) stays in `oauth.rs`, keyed by [`Provider`].
//!
//! Nothing here holds or returns a token, client secret or vault key. [`LinkStatus`] is the
//! read-only view of one account for admin and PM connector pages, built from
//! `CredentialInfo`, which has no token fields.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::vault::{CredentialInfo, Provider};

/// Which connector mode a provider is usable in (`PMOS_CONNECTORS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ConnectorMode {
    Fake,
    Live,
}

/// How a person authorizes the provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthKind {
    /// OAuth 2.0 authorization code with a user-granted scope list.
    OAuth2Scopes,
    /// A GitHub App user token: permissions come from the app, not from requested scopes.
    AppUserToken,
}

/// Who may have this provider linked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkAudience {
    /// Team members, and team bots' own `bots.<name>` read-only accounts.
    MembersAndBots,
    /// Team members only: a team bot never links this provider.
    MembersOnly,
}

/// One provider's declaration.
#[derive(Debug, Clone, Copy)]
pub struct ProviderSpec {
    pub provider: Provider,
    /// Wire name: URL segment, storage value and JSON key. Never change it.
    pub id: &'static str,
    /// How the provider is named to people.
    pub label: &'static str,
    pub auth: AuthKind,
    /// Registry systems (`content/tools.yaml`) whose calls use this provider's token.
    pub systems: &'static [&'static str],
    /// Whether team bots may read with their own account at this provider.
    pub team_bots: bool,
    pub linking: LinkAudience,
    /// Modes in which linking and calls work. Fake mode has no accounts at all.
    pub modes: &'static [ConnectorMode],
}

impl ProviderSpec {
    pub fn enabled_in(&self, mode: ConnectorMode) -> bool {
        self.modes.contains(&mode)
    }
}

static SPECS: [ProviderSpec; 3] = [
    ProviderSpec {
        provider: Provider::Atlassian,
        id: "atlassian",
        label: "Atlassian (Jira and Confluence)",
        auth: AuthKind::OAuth2Scopes,
        systems: &["jira", "confluence"],
        team_bots: true,
        linking: LinkAudience::MembersAndBots,
        modes: &[ConnectorMode::Live],
    },
    ProviderSpec {
        provider: Provider::Slack,
        id: "slack",
        label: "Slack",
        auth: AuthKind::OAuth2Scopes,
        systems: &["slack"],
        team_bots: true,
        linking: LinkAudience::MembersAndBots,
        modes: &[ConnectorMode::Live],
    },
    // The memory repo and content commits; a team bot has no memory and commits nothing.
    ProviderSpec {
        provider: Provider::Github,
        id: "github",
        label: "GitHub",
        auth: AuthKind::AppUserToken,
        systems: &[],
        team_bots: false,
        linking: LinkAudience::MembersOnly,
        modes: &[ConnectorMode::Live],
    },
];

/// The registry. A zero-sized handle over the static table.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProviderRegistry;

impl ProviderRegistry {
    pub fn list(&self) -> &'static [ProviderSpec] {
        &SPECS
    }

    pub fn spec(&self, provider: Provider) -> &'static ProviderSpec {
        SPECS
            .iter()
            .find(|s| s.provider == provider)
            .expect("every Provider is declared in the registry")
    }

    /// Look up by wire name; `None` for an unknown provider.
    pub fn by_id(&self, id: &str) -> Option<&'static ProviderSpec> {
        SPECS.iter().find(|s| s.id == id)
    }

    /// The provider whose token a registry system uses.
    pub fn for_system(&self, system: &str) -> Option<&'static ProviderSpec> {
        SPECS.iter().find(|s| s.systems.contains(&system))
    }
}

/// The process-wide registry.
pub fn registry() -> ProviderRegistry {
    ProviderRegistry
}

/// Where one account stands, without any secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkState {
    NotConnected,
    Connected,
    /// The access token's expiry has passed. It is refreshed on next use; if the provider
    /// refuses the refresh, the link is removed and the state becomes `NotConnected`.
    AccessExpired,
}

/// A user's link to one provider, for display. Deliberately has no token, client secret or
/// vault field: the `link_status_has_no_secret_fields` test pins its serialized shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LinkStatus {
    pub provider: Provider,
    pub label: &'static str,
    pub state: LinkState,
    /// Granted scopes, split on whitespace/commas. Empty when not connected.
    pub scopes: Vec<String>,
    /// Requested scopes that were not granted (linked before they were added).
    pub missing_scopes: Vec<String>,
    pub external_id: Option<String>,
    pub site_url: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
}

fn split_scopes(scopes: &str) -> Vec<String> {
    scopes
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// One row per registered provider for a user's `linked` accounts. `requested` returns the
/// scopes PM OS asks for now (to spot drift).
pub fn link_statuses(
    linked: &[CredentialInfo],
    now: DateTime<Utc>,
    requested: impl Fn(Provider) -> Vec<String>,
) -> Vec<LinkStatus> {
    registry()
        .list()
        .iter()
        .map(|spec| match linked.iter().find(|c| c.provider == spec.provider) {
            Some(c) => {
                let scopes = split_scopes(&c.scopes);
                let missing_scopes = requested(spec.provider)
                    .into_iter()
                    .filter(|s| !scopes.contains(s))
                    .collect();
                LinkStatus {
                    provider: spec.provider,
                    label: spec.label,
                    state: if c.expires_at.is_some_and(|e| e <= now) {
                        LinkState::AccessExpired
                    } else {
                        LinkState::Connected
                    },
                    scopes,
                    missing_scopes,
                    external_id: c.external_id.clone(),
                    site_url: c.site_url.clone(),
                    expires_at: c.expires_at,
                    updated_at: Some(c.updated_at),
                }
            }
            None => LinkStatus {
                provider: spec.provider,
                label: spec.label,
                state: LinkState::NotConnected,
                scopes: Vec::new(),
                missing_scopes: Vec::new(),
                external_id: None,
                site_url: None,
                expires_at: None,
                updated_at: None,
            },
        })
        .collect()
}

/// The outcome of a health probe. Carries a short message, never provider payloads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Health {
    /// No probe is implemented for this provider.
    NotProbed,
    Healthy,
    Degraded(String),
}

/// A cheap check that a provider's app credentials and API answer. No probes are implemented
/// yet: the default does nothing, so adding one later changes no caller.
#[async_trait]
pub trait HealthProbe: Send + Sync {
    async fn probe(&self, provider: Provider) -> Health {
        let _ = provider;
        Health::NotProbed
    }
}

/// The probe used until real ones exist.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoProbe;

#[async_trait]
impl HealthProbe for NoProbe {}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(provider: Provider, scopes: &str, expires: Option<DateTime<Utc>>) -> CredentialInfo {
        CredentialInfo {
            provider,
            scopes: scopes.into(),
            expires_at: expires,
            site_url: None,
            external_id: Some("U1".into()),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn every_provider_is_declared_once_with_stable_wire_names() {
        let ids: Vec<_> = registry().list().iter().map(|s| s.id).collect();
        assert_eq!(ids, ["atlassian", "slack", "github"]);
        for s in registry().list() {
            assert_eq!(registry().spec(s.provider).id, s.id);
            assert_eq!(registry().by_id(s.id).unwrap().provider, s.provider);
        }
    }

    #[test]
    fn lookups() {
        assert_eq!(registry().by_id("slack").unwrap().label, "Slack");
        assert!(registry().by_id("gitlab").is_none());
        assert!(registry().by_id("Slack").is_none());
        assert_eq!(registry().for_system("jira").unwrap().provider, Provider::Atlassian);
        assert_eq!(registry().for_system("confluence").unwrap().provider, Provider::Atlassian);
        assert_eq!(registry().for_system("slack").unwrap().provider, Provider::Slack);
        assert!(registry().for_system("docs").is_none());
        assert!(registry().for_system("github").is_none());
    }

    #[test]
    fn policy_flags() {
        let gh = registry().spec(Provider::Github);
        assert!(!gh.team_bots);
        assert_eq!(gh.linking, LinkAudience::MembersOnly);
        assert!(registry().spec(Provider::Slack).team_bots);
        assert!(gh.enabled_in(ConnectorMode::Live));
        assert!(!gh.enabled_in(ConnectorMode::Fake));
    }

    #[test]
    fn link_statuses_cover_every_provider() {
        let now = Utc::now();
        let linked = vec![
            info(Provider::Slack, "chat:write search:read.public", None),
            info(Provider::Atlassian, "read:jira-work", Some(now - chrono::Duration::hours(1))),
        ];
        let rows = link_statuses(&linked, now, |p| match p {
            Provider::Slack => vec!["chat:write".into(), "im:history".into()],
            _ => vec![],
        });
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].state, LinkState::AccessExpired);
        assert_eq!(rows[1].state, LinkState::Connected);
        assert_eq!(rows[1].missing_scopes, ["im:history"]);
        assert_eq!(rows[2].state, LinkState::NotConnected);
        assert!(rows[2].scopes.is_empty());
    }

    #[test]
    fn link_status_has_no_secret_fields() {
        let rows = link_statuses(&[info(Provider::Slack, "a", None)], Utc::now(), |_| vec![]);
        for row in &rows {
            let v = serde_json::to_value(row).unwrap();
            for key in v.as_object().unwrap().keys() {
                let k = key.to_lowercase();
                for banned in ["token", "secret", "password", "key", "cloud_id"] {
                    assert!(!k.contains(banned), "LinkStatus field `{key}`");
                }
            }
        }
    }

    #[tokio::test]
    async fn default_probe_is_a_no_op() {
        assert_eq!(NoProbe.probe(Provider::Slack).await, Health::NotProbed);
    }
}
