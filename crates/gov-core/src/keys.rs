//! Ed25519 keys, named by a key id.

use std::collections::BTreeMap;

use aws_lc_rs::signature::{Ed25519KeyPair, KeyPair, UnparsedPublicKey, ED25519};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::ids::Id;

/// A fresh random identifier, from the system's random source: `prefix-` and 32 hex digits.
/// For nonces and challenges.
pub fn random_id(prefix: &str) -> Result<Id> {
    let mut bytes = [0u8; 16];
    aws_lc_rs::rand::fill(&mut bytes).map_err(|_| Error::Malformed("random source"))?;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    Id::new(&format!("{prefix}-{hex}"))
}

/// 32 fresh random bytes from the system's random source: a digest's salt.
pub fn random_salt() -> Result<[u8; 32]> {
    let mut bytes = [0u8; 32];
    aws_lc_rs::rand::fill(&mut bytes).map_err(|_| Error::Malformed("random source"))?;
    Ok(bytes)
}

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

    /// The key kept in a key file's text (see [`new_key_file`]). This crate reads no file:
    /// the program that does checks who else can read it.
    pub fn from_file_text(text: &str) -> Result<Self> {
        let file: KeyFile = serde_json::from_str(text).map_err(|_| Error::Malformed("key file"))?;
        let mut seed = [0u8; 32];
        let decoded = URL_SAFE_NO_PAD
            .decode(&file.seed)
            .map_err(|_| Error::Malformed("key file"))?;
        if decoded.len() != seed.len() {
            return Err(Error::Malformed("key file"));
        }
        seed.copy_from_slice(&decoded);
        SigningKey::from_seed(file.kid, &seed)
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

/// A private key as a file holds it. It has no `Debug`, so it cannot be printed by accident.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyFile {
    kid: Id,
    seed: String,
}

/// A new random key: the text of its key file, to be written where only the program that
/// signs with it can read, and the public key to enrol.
pub fn new_key_file(kid: Id) -> Result<(String, PublicKey)> {
    let seed = random_salt()?;
    let public = SigningKey::from_seed(kid.clone(), &seed)?.public();
    let text = serde_json::to_string(&KeyFile {
        kid,
        seed: URL_SAFE_NO_PAD.encode(seed),
    })
    .map_err(|_| Error::Malformed("key file"))?;
    Ok((text, public))
}

/// A public key and its id, as it appears inside a document (a mandate names its agent's).
/// Parsing one refuses the known small-order points (review finding GR-6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawPublicKey")]
pub struct PublicKey {
    pub kid: Id,
    /// 32 bytes, base64url without padding.
    key: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPublicKey {
    kid: Id,
    key: String,
}

impl TryFrom<RawPublicKey> for PublicKey {
    type Error = Error;
    fn try_from(raw: RawPublicKey) -> Result<Self> {
        let key = PublicKey {
            kid: raw.kid,
            key: raw.key,
        };
        let bytes = key.bytes()?;
        if SMALL_ORDER.iter().any(|weak| unhex(weak) == bytes) {
            return Err(Error::WeakKey);
        }
        Ok(key)
    }
}

/// The encodings of the eight points of small order on the curve, and the other spellings of
/// the same points that some libraries read. For any of them a signature anyone can write verifies some messages, so
/// its holder could disown everything it signed. `a_key_anyone_can_forge_for_is_refused` shows
/// each entry really is such a key.
const SMALL_ORDER: [&str; 14] = [
    "0100000000000000000000000000000000000000000000000000000000000000",
    "ecffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f",
    "0000000000000000000000000000000000000000000000000000000000000000",
    "0000000000000000000000000000000000000000000000000000000000000080",
    "26e8958fc2b227b045c3f489f2ef98f0d5dfac05d3c63339b13802886d53fc05",
    "26e8958fc2b227b045c3f489f2ef98f0d5dfac05d3c63339b13802886d53fc85",
    "c7176a703d4dd84fba3c0b760d10670f2a2053fa2c39ccc64ec7fd7792ac037a",
    "c7176a703d4dd84fba3c0b760d10670f2a2053fa2c39ccc64ec7fd7792ac03fa",
    "0100000000000000000000000000000000000000000000000000000000000080",
    "ecffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
    "edffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f",
    "eeffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f",
    "edffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
    "eeffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
];

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).expect("a constant in hex"))
        .collect()
}

impl PublicKey {
    /// For tests of what a verifier does with a key it should never have been given.
    #[doc(hidden)]
    pub fn from_bytes_unchecked(kid: Id, bytes: &[u8; 32]) -> Self {
        PublicKey {
            kid,
            key: URL_SAFE_NO_PAD.encode(bytes),
        }
    }

    /// The key's 32 bytes.
    pub fn bytes(&self) -> Result<Vec<u8>> {
        let bytes = URL_SAFE_NO_PAD
            .decode(&self.key)
            .map_err(|_| Error::Malformed("key"))?;
        if bytes.len() != 32 {
            return Err(Error::Malformed("key"));
        }
        Ok(bytes)
    }

    /// The check to run before a key is enrolled: slower than parsing, and it does not depend
    /// on a list. A signature of "the neutral point, zero" verifies under a key only when the
    /// key has small order, and then for about one message in eight; a key of full order
    /// never accepts it. 256 fixed messages are tried.
    /// The same key, whatever id it is listed under.
    pub fn same_key(&self, other: &PublicKey) -> bool {
        self.key == other.key
    }

    pub fn enrolable(&self) -> Result<()> {
        let bytes = self.bytes()?;
        let mut trivial = [0u8; 64];
        trivial[0] = 1;
        let key = UnparsedPublicKey::new(&ED25519, bytes);
        for n in 0..=255u8 {
            if key.verify(&[b'g', b'o', b'v', n], &trivial).is_ok() {
                return Err(Error::WeakKey);
            }
        }
        Ok(())
    }

    pub(crate) fn verify(&self, message: &[u8], signature: &[u8]) -> Result<()> {
        let bytes = self.bytes()?;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Review finding GR-6. Every listed encoding is refused when parsed; and each one the
    /// library will load at all is shown to be weak by the enrolment check, which knows no list.
    #[test]
    fn a_key_anyone_can_forge_for_is_refused() {
        let mut shown_weak = 0;
        for weak in SMALL_ORDER {
            let bytes: [u8; 32] = unhex(weak).try_into().unwrap();
            let json = serde_json::json!({"kid": "k", "key": URL_SAFE_NO_PAD.encode(bytes)});
            assert_eq!(
                serde_json::from_value::<PublicKey>(json)
                    .unwrap_err()
                    .to_string(),
                Error::WeakKey.to_string(),
                "{weak}"
            );
            let key = PublicKey::from_bytes_unchecked(Id::new("k").unwrap(), &bytes);
            if key.enrolable() == Err(Error::WeakKey) {
                shown_weak += 1;
            }
        }
        // The eight true points at least; the other spellings only if the library reads them.
        assert!(
            shown_weak >= 8,
            "only {shown_weak} of the listed keys were shown weak"
        );
        // A real key passes both, and survives a trip through JSON.
        let real = SigningKey::from_seed(Id::new("k").unwrap(), &[7; 32])
            .unwrap()
            .public();
        real.enrolable().unwrap();
        let back: PublicKey = serde_json::from_value(serde_json::to_value(&real).unwrap()).unwrap();
        assert_eq!(back, real);
        let extra = serde_json::json!({"kid": "k", "key": "AAAA", "alg": "none"});
        assert!(serde_json::from_value::<PublicKey>(extra).is_err());
    }
}
