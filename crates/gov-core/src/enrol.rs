//! Enrolment (spec §7.1, §7.6 choice 3): how an agent's or a gate's key comes to be trusted.
//!
//! An admin makes a one-time code for one named agent or gate. Whoever holds that name's
//! new key presents the code itself, with a request signed by the key being enrolled. The
//! Authority keeps only the code's digest, so the digest alone enrols nothing; and its
//! answer lets the caller check that the root key it is handed is the one the holder of
//! the code meant it to have.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::envelope::document;
use crate::error::{Error, Result};
use crate::ids::{Digest, Id};
use crate::keys::{random_salt, PublicKey};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnrolKind {
    Agent,
    Gate,
}

/// The request to enrol, signed by the key being enrolled. It names who is enrolling and
/// with which key, so a request seen on the way cannot be turned into one for another name
/// or another key; it carries the code's digest, never the code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnrolRequest {
    pub v: u32,
    pub tenant: Id,
    pub env: Id,
    /// The Authority asked.
    pub authority: Id,
    pub kind: EnrolKind,
    /// The agent's or the gate's own name.
    pub name: Id,
    pub key: PublicKey,
    pub code: Digest,
}

document!(EnrolRequest, "gov.enrol.v1");

/// A new one-time code: 128 random bits, as text.
pub fn new_code() -> Result<String> {
    let bytes = random_salt()?;
    Ok(URL_SAFE_NO_PAD.encode(&bytes[..16]))
}

/// What the Authority stores of a code, and what a request to enrol is signed over.
pub fn code_digest(code: &str) -> Digest {
    Digest::of(&[b"gov.enrol.code.v1\0", code.as_bytes()])
}

/// The value the Authority's answer carries beside its root key: computed from the code and
/// that key, so only something that knew the code could have made it. The caller computes
/// it again and pins the root key only when the two agree.
pub fn pin(code: &str, root: &PublicKey) -> Result<Digest> {
    let key = serde_json::to_vec(root).map_err(|_| Error::Malformed("key"))?;
    Ok(Digest::of(&[
        b"gov.enrol.pin.v1\0",
        code.as_bytes(),
        b"\0",
        &key,
    ]))
}

/// The digest of a public key, as an admin record carries it for an enrolled key. Whoever
/// holds the key itself can check it against the record; the record alone gives no key.
pub fn key_digest(key: &PublicKey) -> Result<Digest> {
    let bytes = serde_json::to_vec(key).map_err(|_| Error::Malformed("key"))?;
    Ok(Digest::of(&[b"gov.key.v1\0", &bytes]))
}

/// The Authority's word to the Ledger that a gate's key is enrolled, signed with the key
/// its admin records are signed with. The Ledger takes the key for the gate's receipts only
/// when its copy of the Authority's chain holds the record of that enrolment (spec §7.6,
/// choice 3): a gate's key reaches the Ledger from the Authority, and only a witnessed one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyGrant {
    pub v: u32,
    pub tenant: Id,
    pub env: Id,
    pub authority: Id,
    pub gate: Id,
    pub key: PublicKey,
}

document!(KeyGrant, "gov.keygrant.v1");
