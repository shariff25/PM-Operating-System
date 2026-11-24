//! The signed user assertion on the runtime's side (`docs/user-assertion.md`,
//! `docs/bot-runtime-spec.md` §3, Auth).
//!
//! The `web` and `console` clients act for people. With `PMOS_ASSERTION_MODE` at `verify` or
//! `require`, a call from either that carries `X-Pmos-Assertion` is believed only if the
//! assertion verifies: signed with that client's key for `bots`, issued by the client whose
//! token was presented, current, bound to this exact request, and not seen before. Its subject
//! must then equal every person the request names (`X-Pmos-User`, `requested_by=`, `owner=`,
//! `/members/{handle}`), and it becomes the PM the call is for. With `require`, a call from
//! `web` or `console` that names a person without one is refused. Every refusal is the same
//! `401 {"code":"bad_assertion"}`; the log says why, never what the assertion said.
//!
//! The Slack, worker and evals clients are never checked: Slack acts on Slack-signed events
//! and is a separate question.
//!
//! The shared parts (the mode, the start-up rule, the refusals, the verifier) live in
//! [`pmos_assertion::receiver`]; this module names the bot runtime as a receiver.

use std::collections::BTreeMap;

use pmos_assertion::receiver::{parse_receiver_keys, Receiver, ReceiverSpec};
use pmos_assertion::KeyRing;

/// `PMOS_ASSERTION_MODE` (`off`, `verify` or `require`; `require` by default once a client
/// that acts for a person is configured) and its development override.
pub use pmos_assertion::receiver::{Mode, Refusal, INSECURE_OFF_VAR, MODE_VAR};
/// `PMOS_ASSERTION_KEYS`: the keys, one entry per caller→receiver pair.
pub use pmos_assertion::KEYS_VAR;

/// This service's name as an assertion audience.
pub const AUDIENCE: &str = "bots";
/// The clients that act for people here, and so may issue assertions.
pub const ISSUERS: [&str; 2] = ["web", "console"];

/// the bot runtime as a receiver of assertions.
#[derive(Debug, Clone, Copy)]
pub struct Spec;

impl ReceiverSpec for Spec {
    const AUDIENCE: &'static str = AUDIENCE;
    const SERVICE: &'static str = "the bot runtime";
    const ISSUERS: &'static [&'static str] = &ISSUERS;
}

/// the bot runtime's verifier: the mode, one key ring and policy per issuer, and the replay cache.
pub type Assertions = Receiver<Spec>;

/// Parse `PMOS_ASSERTION_KEYS` (`pmos_assertion::parse_key_list`, shared by every service
/// that signs or checks). `wanted` says which pairs this service uses; an entry for any other
/// pair is refused. Errors name the entry and the pair, never a key.
pub fn parse_keys(
    spec: &str,
    service: &str,
    wanted: impl Fn(&str, &str) -> bool,
) -> Result<BTreeMap<(String, String), KeyRing>, String> {
    parse_receiver_keys::<Spec>(spec, service, wanted)
}

#[cfg(test)]
mod tests {
    use super::*;

    const K1: &str = "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=";

    #[test]
    fn only_this_services_pairs_are_accepted() {
        let ours = |iss: &str, aud: &str| aud == AUDIENCE && ISSUERS.contains(&iss);
        let keys = parse_keys(&format!("web:bots:k1:{K1}"), "the bot runtime", ours).unwrap();
        assert!(Assertions::new(Mode::Require, keys).is_ok());
        let err =
            parse_keys(&format!("web:toolserver:k1:{K1}"), "the bot runtime", ours).unwrap_err();
        assert!(err.contains("not a pair") && !err.contains(K1), "{err}");
        let err = parse_keys(&format!("slack:bots:k1:{K1}"), "the bot runtime", ours).unwrap_err();
        assert!(err.contains("not a pair"), "{err}");
    }
}
