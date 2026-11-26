//! The signed envelope every document travels in (spec §4.1).
//!
//! The signature is over a type tag, a zero byte and the exact payload bytes. A verifier
//! checks the signature on the bytes it received and only then parses them, so nothing is
//! re-serialised before verification and one kind of document never verifies as another.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::ids::{Digest, Id, Scope};
use crate::keys::{KeySet, SigningKey};

/// The only format version there is.
pub const VERSION: u32 = 1;

/// A payload that can be sealed. `TAG` is part of what is signed.
pub trait Document: Serialize + DeserializeOwned {
    const TAG: &'static str;
    fn version(&self) -> u32;
    fn tenant(&self) -> &Id;
    fn env(&self) -> &Id;
}

/// `{ v, kid, payload, sig }`: the payload's exact bytes and a signature over them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub v: u32,
    pub kid: Id,
    /// base64url, no padding.
    pub payload: String,
    /// base64url, no padding.
    pub sig: String,
}

/// A document whose signature, version, tenant and environment have been checked.
#[derive(Debug, Clone)]
pub struct Opened<D> {
    pub doc: D,
    /// The key that signed it.
    pub kid: Id,
    /// SHA-256 over the signed bytes and the signature: what a chain links on.
    pub hash: Digest,
}

fn signed_bytes(tag: &str, payload: &[u8]) -> Vec<u8> {
    let mut m = Vec::with_capacity(tag.len() + 1 + payload.len());
    m.extend_from_slice(tag.as_bytes());
    m.push(0);
    m.extend_from_slice(payload);
    m
}

/// Sign `doc` with `key`.
pub fn seal<D: Document>(doc: &D, key: &SigningKey) -> Result<Envelope> {
    let payload = serde_json::to_vec(doc).map_err(|_| Error::Malformed("payload"))?;
    let sig = key.sign(&signed_bytes(D::TAG, &payload));
    Ok(Envelope {
        v: VERSION,
        kid: key.kid().clone(),
        payload: URL_SAFE_NO_PAD.encode(payload),
        sig: URL_SAFE_NO_PAD.encode(sig),
    })
}

/// Check `envelope` and return its document. In order: the envelope's version; a trusted key
/// with that id; the signature over the tag and the received bytes; then the parse; then the
/// payload's version, tenant and environment. Nothing in the payload is read before the
/// signature holds.
pub fn open<D: Document>(envelope: &Envelope, keys: &KeySet, scope: &Scope) -> Result<Opened<D>> {
    if envelope.v != VERSION {
        return Err(Error::UnknownVersion);
    }
    let payload = URL_SAFE_NO_PAD
        .decode(&envelope.payload)
        .map_err(|_| Error::BadEnvelope)?;
    let sig = URL_SAFE_NO_PAD
        .decode(&envelope.sig)
        .map_err(|_| Error::BadEnvelope)?;
    let key = keys.get(&envelope.kid).ok_or(Error::UnknownKey)?;
    let message = signed_bytes(D::TAG, &payload);
    key.verify(&message, &sig)?;
    let doc: D = serde_json::from_slice(&payload).map_err(|_| Error::Malformed("payload"))?;
    if doc.version() != VERSION {
        return Err(Error::UnknownVersion);
    }
    if doc.tenant() != &scope.tenant {
        return Err(Error::WrongTenant);
    }
    if doc.env() != &scope.env {
        return Err(Error::WrongEnv);
    }
    Ok(Opened {
        doc,
        kid: envelope.kid.clone(),
        hash: Digest::of(&[&message, &sig]),
    })
}

/// Implements [`Document`] for a struct with `v`, `tenant` and `env` fields.
macro_rules! document {
    ($ty:ty, $tag:literal) => {
        impl $crate::envelope::Document for $ty {
            const TAG: &'static str = $tag;
            fn version(&self) -> u32 {
                self.v
            }
            fn tenant(&self) -> &$crate::ids::Id {
                &self.tenant
            }
            fn env(&self) -> &$crate::ids::Id {
                &self.env
            }
        }
    };
}
pub(crate) use document;
