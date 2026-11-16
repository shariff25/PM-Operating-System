//! Per-user OAuth tokens, encrypted at rest.
//!
//! Every connector call acts as the signed-in PM with that PM's own token: Atlassian OAuth 2.0
//! (3LO) for Jira and Confluence, the PM's Slack user token, and a GitHub App user token for
//! the PM's memory repo and the content-repo commits a platform owner approves. Tokens are sealed with
//! AES-256-GCM before they reach any store. The additional authenticated data binds each
//! ciphertext to its user, provider and field, so a sealed token copied into another
//! user's row will not decrypt.
//!
//! Keys come from `PMOS_VAULT_KEY`: a comma-separated list of `key_id:base64key` (32-byte
//! keys). The first key seals; every listed key can unseal, so a key is rotated by putting
//! the new one first. Rows sealed with an older key are re-sealed on their next read.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::{Arc, Mutex};

use aes_gcm::aead::{Aead, AeadCore, KeyInit, OsRng, Payload};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::connectors::registry::registry;
use crate::store::{StoreError, StoreResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Atlassian,
    Slack,
    Github,
}

impl Provider {
    /// Every provider, in registry order.
    pub const ALL: [Provider; 3] = [Self::Atlassian, Self::Slack, Self::Github];

    pub fn as_str(self) -> &'static str {
        registry().spec(self).id
    }

    pub fn parse(s: &str) -> Option<Self> {
        registry().by_id(s).map(|spec| spec.provider)
    }

    /// How the provider is named to people.
    pub fn label(self) -> &'static str {
        registry().spec(self).label
    }

    /// The provider whose token a registry system uses.
    pub fn for_system(system: &str) -> Option<Self> {
        registry().for_system(system).map(|spec| spec.provider)
    }
}

impl fmt::Display for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A token. Its `Debug` output is redacted so it can't leak through logs or panics.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// A user's linked account with one provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credential {
    pub user: String,
    pub provider: Provider,
    pub access_token: Secret,
    pub refresh_token: Option<Secret>,
    /// When the access token expires. `None` for tokens that don't expire (Slack without
    /// token rotation).
    pub expires_at: Option<DateTime<Utc>>,
    pub scopes: String,
    /// Atlassian site id (`cloudid`); requests go to `api.atlassian.com/ex/{product}/{cloud_id}`.
    pub cloud_id: Option<String>,
    /// Atlassian site URL, such as `https://example.atlassian.net`, for links back.
    pub site_url: Option<String>,
    /// The user's id at the provider: a Slack `U…` id, or a GitHub login.
    pub external_id: Option<String>,
    pub updated_at: DateTime<Utc>,
}

/// What can be shown about a linked account: never the tokens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CredentialInfo {
    pub provider: Provider,
    pub scopes: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub site_url: Option<String>,
    pub external_id: Option<String>,
    pub updated_at: DateTime<Utc>,
}

/// A credential as stored: tokens sealed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealedCredential {
    pub user: String,
    pub provider: Provider,
    pub key_id: String,
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub scopes: String,
    pub cloud_id: Option<String>,
    pub site_url: Option<String>,
    pub external_id: Option<String>,
    pub updated_at: DateTime<Utc>,
}

/// Storage for sealed credentials. Implementations never see plaintext tokens.
#[async_trait]
pub trait CredentialStore: Send + Sync {
    async fn get(&self, user: &str, provider: Provider) -> StoreResult<Option<SealedCredential>>;
    /// Insert or replace the user's credential for that provider.
    async fn put(&self, credential: &SealedCredential) -> StoreResult<()>;
    /// Replace the stored credential only if it is still the one read earlier (its
    /// `updated_at` is `expected`). False if it changed or was deleted meanwhile, so a
    /// stale write can't overwrite a newer token or bring back an unlinked account.
    async fn replace(
        &self,
        credential: &SealedCredential,
        expected: DateTime<Utc>,
    ) -> StoreResult<bool>;
    async fn delete(&self, user: &str, provider: Provider) -> StoreResult<bool>;
    async fn list(&self, user: &str) -> StoreResult<Vec<SealedCredential>>;
}

#[derive(Default)]
pub struct MemoryCredentials {
    rows: Mutex<HashMap<(String, Provider), SealedCredential>>,
}

#[async_trait]
impl CredentialStore for MemoryCredentials {
    async fn get(&self, user: &str, provider: Provider) -> StoreResult<Option<SealedCredential>> {
        Ok(self
            .rows
            .lock()
            .expect("credentials lock")
            .get(&(user.to_string(), provider))
            .cloned())
    }

    async fn put(&self, credential: &SealedCredential) -> StoreResult<()> {
        self.rows.lock().expect("credentials lock").insert(
            (credential.user.clone(), credential.provider),
            credential.clone(),
        );
        Ok(())
    }

    async fn replace(
        &self,
        credential: &SealedCredential,
        expected: DateTime<Utc>,
    ) -> StoreResult<bool> {
        let mut rows = self.rows.lock().expect("credentials lock");
        match rows.get_mut(&(credential.user.clone(), credential.provider)) {
            Some(row) if row.updated_at == expected => {
                *row = credential.clone();
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn delete(&self, user: &str, provider: Provider) -> StoreResult<bool> {
        Ok(self
            .rows
            .lock()
            .expect("credentials lock")
            .remove(&(user.to_string(), provider))
            .is_some())
    }

    async fn list(&self, user: &str) -> StoreResult<Vec<SealedCredential>> {
        let rows = self.rows.lock().expect("credentials lock");
        let mut out: Vec<_> = rows.values().filter(|c| c.user == user).cloned().collect();
        out.sort_by_key(|c| c.provider);
        Ok(out)
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum KeyError {
    #[error("PMOS_VAULT_KEY is empty")]
    Empty,
    #[error("vault key entries must be `key_id:base64key`")]
    Format,
    #[error("vault key `{0}` must be 32 bytes, base64-encoded")]
    Length(String),
    #[error("vault key id `{0}` must be 1–32 letters, digits, `-` or `_`")]
    BadId(String),
    #[error("vault key id `{0}` is listed twice")]
    Duplicate(String),
}

/// The vault's keys. The first one seals.
#[derive(Clone)]
pub struct KeyRing {
    active: String,
    keys: BTreeMap<String, Aes256Gcm>,
}

impl fmt::Debug for KeyRing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KeyRing")
            .field("active", &self.active)
            .field("key_ids", &self.keys.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl KeyRing {
    /// Parse `key_id:base64key[,key_id:base64key…]`.
    pub fn from_spec(spec: &str) -> Result<Self, KeyError> {
        let mut keys = BTreeMap::new();
        let mut active = None;
        for entry in spec.split(',').map(str::trim).filter(|e| !e.is_empty()) {
            let (id, b64) = entry.split_once(':').ok_or(KeyError::Format)?;
            let valid_id = !id.is_empty()
                && id.len() <= 32
                && id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
            if !valid_id {
                return Err(KeyError::BadId(id.to_string()));
            }
            let bytes = B64
                .decode(b64.trim())
                .map_err(|_| KeyError::Length(id.to_string()))?;
            if bytes.len() != 32 {
                return Err(KeyError::Length(id.to_string()));
            }
            let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&bytes));
            if keys.insert(id.to_string(), cipher).is_some() {
                return Err(KeyError::Duplicate(id.to_string()));
            }
            active.get_or_insert_with(|| id.to_string());
        }
        Ok(Self {
            active: active.ok_or(KeyError::Empty)?,
            keys,
        })
    }

    /// A fresh random 32-byte key in spec form, for setup and tests.
    pub fn generate_spec(key_id: &str) -> String {
        let key = Aes256Gcm::generate_key(OsRng);
        format!("{key_id}:{}", B64.encode(key))
    }

    pub fn active_id(&self) -> &str {
        &self.active
    }

    fn seal(&self, plaintext: &str, aad: &str) -> StoreResult<String> {
        let cipher = &self.keys[&self.active];
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let sealed = cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: plaintext.as_bytes(),
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| StoreError("sealing failed".into()))?;
        Ok(format!(
            "v1.{}.{}.{}",
            self.active,
            B64.encode(nonce),
            B64.encode(sealed)
        ))
    }

    fn unseal(&self, sealed: &str, aad: &str) -> StoreResult<String> {
        let bad = || StoreError("a stored token could not be unsealed".into());
        let mut parts = sealed.split('.');
        let (Some("v1"), Some(key_id), Some(nonce), Some(body), None) = (
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
        ) else {
            return Err(bad());
        };
        let cipher = self.keys.get(key_id).ok_or_else(|| {
            StoreError(format!(
                "a stored token was sealed with unknown key `{key_id}`"
            ))
        })?;
        let nonce = B64.decode(nonce).map_err(|_| bad())?;
        if nonce.len() != 12 {
            return Err(bad());
        }
        let body = B64.decode(body).map_err(|_| bad())?;
        let plain = cipher
            .decrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: &body,
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| bad())?;
        String::from_utf8(plain).map_err(|_| bad())
    }
}

fn aad(user: &str, provider: Provider, field: &str) -> String {
    format!("pmos-vault|{user}|{provider}|{field}")
}

/// Seals and unseals credentials on top of a [`CredentialStore`].
#[derive(Clone)]
pub struct Vault {
    keys: KeyRing,
    store: Arc<dyn CredentialStore>,
}

impl Vault {
    pub fn new(keys: KeyRing, store: Arc<dyn CredentialStore>) -> Self {
        Self { keys, store }
    }

    fn seal(&self, c: &Credential) -> StoreResult<SealedCredential> {
        Ok(SealedCredential {
            user: c.user.clone(),
            provider: c.provider,
            key_id: self.keys.active_id().to_string(),
            access_token: self
                .keys
                .seal(c.access_token.expose(), &aad(&c.user, c.provider, "access"))?,
            refresh_token: c
                .refresh_token
                .as_ref()
                .map(|t| {
                    self.keys
                        .seal(t.expose(), &aad(&c.user, c.provider, "refresh"))
                })
                .transpose()?,
            expires_at: c.expires_at,
            scopes: c.scopes.clone(),
            cloud_id: c.cloud_id.clone(),
            site_url: c.site_url.clone(),
            external_id: c.external_id.clone(),
            updated_at: c.updated_at,
        })
    }

    fn unseal(&self, s: &SealedCredential) -> StoreResult<Credential> {
        Ok(Credential {
            user: s.user.clone(),
            provider: s.provider,
            access_token: Secret(
                self.keys
                    .unseal(&s.access_token, &aad(&s.user, s.provider, "access"))?,
            ),
            refresh_token: s
                .refresh_token
                .as_ref()
                .map(|t| {
                    self.keys
                        .unseal(t, &aad(&s.user, s.provider, "refresh"))
                        .map(Secret)
                })
                .transpose()?,
            expires_at: s.expires_at,
            scopes: s.scopes.clone(),
            cloud_id: s.cloud_id.clone(),
            site_url: s.site_url.clone(),
            external_id: s.external_id.clone(),
            updated_at: s.updated_at,
        })
    }

    pub async fn put(&self, credential: &Credential) -> StoreResult<()> {
        self.store.put(&self.seal(credential)?).await
    }

    /// The user's credential, re-sealed under the active key if an older key sealed it.
    pub async fn get(&self, user: &str, provider: Provider) -> StoreResult<Option<Credential>> {
        let Some(sealed) = self.store.get(user, provider).await? else {
            return Ok(None);
        };
        let credential = self.unseal(&sealed)?;
        if sealed.key_id != self.keys.active_id() {
            // Re-seal under the active key, unless someone wrote a newer credential meanwhile.
            self.store
                .replace(&self.seal(&credential)?, sealed.updated_at)
                .await?;
        }
        Ok(Some(credential))
    }

    /// Replace the credential only if it is still the version with `expected` as its
    /// `updated_at`. See [`CredentialStore::replace`].
    pub async fn replace(
        &self,
        credential: &Credential,
        expected: DateTime<Utc>,
    ) -> StoreResult<bool> {
        self.store.replace(&self.seal(credential)?, expected).await
    }

    pub async fn delete(&self, user: &str, provider: Provider) -> StoreResult<bool> {
        self.store.delete(user, provider).await
    }

    /// The user's linked accounts, without tokens.
    pub async fn list(&self, user: &str) -> StoreResult<Vec<CredentialInfo>> {
        Ok(self
            .store
            .list(user)
            .await?
            .into_iter()
            .map(|c| CredentialInfo {
                provider: c.provider,
                scopes: c.scopes,
                expires_at: c.expires_at,
                site_url: c.site_url,
                external_id: c.external_id,
                updated_at: c.updated_at,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        "2026-11-14T10:00:00Z".parse().unwrap()
    }

    fn cred(user: &str) -> Credential {
        Credential {
            user: user.to_string(),
            provider: Provider::Atlassian,
            access_token: Secret::new("at-secret-123"),
            refresh_token: Some(Secret::new("rt-secret-456")),
            expires_at: Some(now()),
            scopes: "read:jira-work offline_access".into(),
            cloud_id: Some("cloud-1".into()),
            site_url: Some("https://example.atlassian.net".into()),
            external_id: None,
            updated_at: now(),
        }
    }

    fn vault(spec: &str, store: Arc<MemoryCredentials>) -> Vault {
        Vault::new(KeyRing::from_spec(spec).unwrap(), store)
    }

    #[tokio::test]
    async fn round_trips_and_stores_no_plaintext() {
        let store = Arc::new(MemoryCredentials::default());
        let v = vault(&KeyRing::generate_spec("k1"), store.clone());
        v.put(&cred("ana")).await.unwrap();
        let sealed = store
            .get("ana", Provider::Atlassian)
            .await
            .unwrap()
            .unwrap();
        assert!(!sealed.access_token.contains("at-secret"));
        assert!(!sealed.refresh_token.as_ref().unwrap().contains("rt-secret"));
        assert!(sealed.access_token.starts_with("v1.k1."));
        assert_eq!(
            v.get("ana", Provider::Atlassian).await.unwrap(),
            Some(cred("ana"))
        );
        assert!(v.get("ana", Provider::Slack).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn the_wrong_key_cannot_unseal() {
        let store = Arc::new(MemoryCredentials::default());
        vault(&KeyRing::generate_spec("k1"), store.clone())
            .put(&cred("ana"))
            .await
            .unwrap();
        let other_key_same_id = vault(&KeyRing::generate_spec("k1"), store.clone());
        assert!(other_key_same_id
            .get("ana", Provider::Atlassian)
            .await
            .is_err());
        let unknown_id = vault(&KeyRing::generate_spec("k9"), store);
        let err = unknown_id
            .get("ana", Provider::Atlassian)
            .await
            .unwrap_err();
        assert!(err.0.contains("unknown key `k1`"));
    }

    #[tokio::test]
    async fn a_token_moved_to_another_user_does_not_unseal() {
        let store = Arc::new(MemoryCredentials::default());
        let v = vault(&KeyRing::generate_spec("k1"), store.clone());
        v.put(&cred("ana")).await.unwrap();
        v.put(&cred("bo")).await.unwrap();
        let mut stolen = store
            .get("ana", Provider::Atlassian)
            .await
            .unwrap()
            .unwrap();
        stolen.user = "bo".into();
        store.put(&stolen).await.unwrap();
        assert!(v.get("bo", Provider::Atlassian).await.is_err());
    }

    #[tokio::test]
    async fn rotation_reseals_on_read() {
        let store = Arc::new(MemoryCredentials::default());
        let old = KeyRing::generate_spec("k1");
        vault(&old, store.clone()).put(&cred("ana")).await.unwrap();

        let new = KeyRing::generate_spec("k2");
        let rotated = vault(&format!("{new},{old}"), store.clone());
        assert_eq!(
            rotated.get("ana", Provider::Atlassian).await.unwrap(),
            Some(cred("ana"))
        );
        let resealed = store
            .get("ana", Provider::Atlassian)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(resealed.key_id, "k2");
        assert!(resealed.access_token.starts_with("v1.k2."));
        // The old key is no longer needed.
        let new_only = vault(&new, store);
        assert!(new_only
            .get("ana", Provider::Atlassian)
            .await
            .unwrap()
            .is_some());
    }

    #[test]
    fn key_spec_errors() {
        assert_eq!(KeyRing::from_spec("").unwrap_err(), KeyError::Empty);
        assert_eq!(KeyRing::from_spec("nokey").unwrap_err(), KeyError::Format);
        assert_eq!(
            KeyRing::from_spec("k1:c2hvcnQ=").unwrap_err(),
            KeyError::Length("k1".into())
        );
        let good = KeyRing::generate_spec("k1");
        assert_eq!(
            KeyRing::from_spec(&format!("{good},{good}")).unwrap_err(),
            KeyError::Duplicate("k1".into())
        );
        let body = good.split_once(':').unwrap().1;
        assert_eq!(
            KeyRing::from_spec(&format!("bad.id:{body}")).unwrap_err(),
            KeyError::BadId("bad.id".into())
        );
    }

    #[test]
    fn secrets_and_keys_never_print() {
        let c = cred("ana");
        let shown = format!("{c:?}");
        assert!(!shown.contains("at-secret"));
        assert!(!shown.contains("rt-secret"));
        let ring = KeyRing::from_spec(&KeyRing::generate_spec("k1")).unwrap();
        let body = format!("{ring:?}");
        assert!(body.contains("k1"));
        assert!(!body.contains("Aes"));
    }

    #[tokio::test]
    async fn list_shows_no_tokens() {
        let store = Arc::new(MemoryCredentials::default());
        let v = vault(&KeyRing::generate_spec("k1"), store);
        v.put(&cred("ana")).await.unwrap();
        let listed = v.list("ana").await.unwrap();
        assert_eq!(listed.len(), 1);
        let json = serde_json::to_string(&listed).unwrap();
        assert!(!json.contains("secret"));
        assert!(json.contains("example.atlassian.net"));
    }
}
