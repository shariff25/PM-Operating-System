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
use crate::Envelope;

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

/// A new one-time code for an agent or gate that is to pin `root` as the Authority's root
/// key: 128 random bits, then a full stop, then the digest of that key.
///
/// The second half is how the enrolling party knows which root key to trust (review finding
/// GR-115). It is handed over by the admin, out of band, with the secret; nothing that
/// answers a request to enrol can change it, and a value sent back in the answer could have
/// been made by whoever was sent the code.
pub fn new_code(root: &PublicKey) -> Result<String> {
    let bytes = random_salt()?;
    Ok(format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(&bytes[..16]),
        key_digest(root)?.as_str()
    ))
}

/// What the Authority stores of a code, and what a request to enrol is signed over.
pub fn code_digest(code: &str) -> Digest {
    Digest::of(&[b"gov.enrol.code.v1\0", code.as_bytes()])
}

/// The key a request sent over the network is authenticated with, in place of the code
/// itself: derived from the code under a label of its own, so neither it nor the code's
/// digest gives the other (spec §7.6.4, point 6).
pub fn mac_key(code: &str) -> Digest {
    Digest::of(&[b"gov.enrol.mac.v1\0", code.as_bytes()])
}

fn mac_input(request: &Envelope) -> Vec<u8> {
    [
        request.kid.as_str(),
        request.payload.as_str(),
        request.sig.as_str(),
    ]
    .join("\n")
    .into_bytes()
}

/// The tag whoever holds the code sends with a signed request to enrol.
pub fn request_mac(key: &Digest, request: &Envelope) -> String {
    let key = aws_lc_rs::hmac::Key::new(aws_lc_rs::hmac::HMAC_SHA256, key.as_str().as_bytes());
    URL_SAFE_NO_PAD.encode(aws_lc_rs::hmac::sign(&key, &mac_input(request)))
}

pub fn mac_holds(key: &Digest, request: &Envelope, tag: &str) -> bool {
    let key = aws_lc_rs::hmac::Key::new(aws_lc_rs::hmac::HMAC_SHA256, key.as_str().as_bytes());
    URL_SAFE_NO_PAD
        .decode(tag)
        .is_ok_and(|tag| aws_lc_rs::hmac::verify(&key, &mac_input(request), &tag).is_ok())
}

/// Whether `root` is the root key the code was made for. The enrolling party pins a root
/// key only when this holds for the key it was answered with.
pub fn code_names_root(code: &str, root: &PublicKey) -> bool {
    let named = code.rsplit_once('.').map(|(_, digest)| digest);
    key_digest(root).is_ok_and(|d| named == Some(d.as_str()))
}

/// The digest of a public key, as an admin record carries it for an enrolled key and as a
/// one-time code carries it for the root key. Whoever holds the key itself can check it
/// against the digest; the digest alone gives no key.
pub fn key_digest(key: &PublicKey) -> Result<Digest> {
    let bytes = serde_json::to_vec(key).map_err(|_| Error::Malformed("key"))?;
    Ok(Digest::of(&[b"gov.key.v1\0", &bytes]))
}
