//! Ed25519 keys, named by a key id.

use std::collections::BTreeMap;

use aws_lc_rs::signature::{Ed25519KeyPair, KeyPair, UnparsedPublicKey, ED25519};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::ids::Id;

/// A private signing key. It has no `Debug`, no `Serialize` and no way to read the secret out.
pub struct SigningKey {
    kid: Id,
    pair: Ed25519KeyPair,
}

impl SigningKey {
    /// A new random key.
    pub fn generate(kid: Id) -> Result<Self> {
        let pair = Ed25519KeyPair::generate().map_err(|_| Error::Malformed("key"))?;
        Ok(SigningKey { kid, pair })
    }

    /// The key for a 32-byte seed. For a key service's driver and for tests.
    pub fn from_seed(kid: Id, seed: &[u8; 32]) -> Result<Self> {
        let pair =
            Ed25519KeyPair::from_seed_unchecked(seed).map_err(|_| Error::Malformed("key"))?;
        Ok(SigningKey { kid, pair })
    }

    pub fn kid(&self) -> &Id {
        &self.kid
    }

    pub fn public(&self) -> PublicKey {
        PublicKey {
            kid: self.kid.clone(),
            key: URL_SAFE_NO_PAD.encode(self.pair.public_key().as_ref()),
        }
    }

    pub(crate) fn sign(&self, message: &[u8]) -> Vec<u8> {
        self.pair.sign(message).as_ref().to_vec()
    }
}

/// A public key and its id, as it appears inside a document (a mandate names its agent's).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicKey {
    pub kid: Id,
    /// 32 bytes, base64url without padding.
    key: String,
}

impl PublicKey {
    pub(crate) fn verify(&self, message: &[u8], signature: &[u8]) -> Result<()> {
        let bytes = URL_SAFE_NO_PAD
            .decode(&self.key)
            .map_err(|_| Error::Malformed("key"))?;
        if bytes.len() != 32 {
            return Err(Error::Malformed("key"));
        }
        UnparsedPublicKey::new(&ED25519, bytes)
            .verify(message, signature)
            .map_err(|_| Error::BadSignature)
    }
}

/// The keys a verifier trusts for one kind of document. The caller chooses the set: the
/// Authority's keys for a mandate, one agent's key for its request, a gate's for its receipts.
#[derive(Debug, Clone, Default)]
pub struct KeySet(BTreeMap<Id, PublicKey>);

impl KeySet {
    pub fn new(keys: impl IntoIterator<Item = PublicKey>) -> Self {
        KeySet(keys.into_iter().map(|k| (k.kid.clone(), k)).collect())
    }
    pub fn get(&self, kid: &Id) -> Option<&PublicKey> {
        self.0.get(kid)
    }
}
