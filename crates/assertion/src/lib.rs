//! PM OS user assertion: a short-lived signed statement of who a service is acting for.
//!
//! A calling service (the console, `pmos-web`) signs an assertion naming the signed-in
//! person and sends it as the [`HEADER`] header; the receiving service (the bot runtime, the
//! tool server) verifies it with the same [`KeyRing`] instead of trusting a plain
//! `X-Pmos-User` header or a `decided_by` field. See `docs/user-assertion.md`.
//!
//! ```
//! use pmos_assertion::{Claims, Key, KeyRing, MemoryReplayCache, RequestBinding, VerifyPolicy};
//! use pmos_contract::model::Role;
//!
//! let ring = KeyRing::new(Key::generate("k1").unwrap());
//! let claims = Claims::new("web", "bots", "ana", &[Role::Pm], "ses_1").unwrap();
//! let req = RequestBinding::new("POST", "/v1/tasks", br#"{"bot":"docs"}"#).unwrap();
//! let now = 1_800_000_000;
//! let header = ring.sign(&claims, now, 60, Some(&req)).unwrap();
//!
//! let policy = VerifyPolicy::new("bots", &["web", "console"]).unwrap();
//! let cache = MemoryReplayCache::default();
//! let who = ring.verify(&header, &policy, now + 1, Some(&req), &cache).unwrap();
//! assert_eq!(who.subject(), "ana");
//! ```

mod fields;
mod key_list;
mod keys;
pub mod receiver;
mod replay;
mod wire;

#[cfg(test)]
mod tests;

pub use fields::{
    Claims, FieldError, RequestBinding, MAX_METHOD_LEN, MAX_NAME_LEN, MAX_PATH_LEN, MAX_SESSION_LEN,
};
pub use key_list::{parse_key_list, KeyList, KeyUse, KEYS_VAR};
pub use keys::{
    unix_now, Key, KeyError, KeyRing, PolicyError, SignError, VerifyError, VerifyPolicy,
    DEFAULT_CLOCK_SKEW_SECS, MAX_CLOCK_SKEW_SECS, MAX_KEY_LEN, MIN_KEY_LEN,
};
pub use replay::{MemoryReplayCache, ReplayGuard, ReplayRejection};
pub use wire::{Assertion, Malformed, Nonce, MAX_LEN, MAX_LIFETIME_SECS, NONCE_LEN, VERSION};

/// The HTTP header that carries an assertion.
pub const HEADER: &str = "x-pmos-assertion";
