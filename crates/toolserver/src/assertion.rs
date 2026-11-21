//! The signed user assertion on the approvals routes (`docs/user-assertion.md`,
//! `docs/toolserver.md`, Approvals auth).
//!
//! `pmos-web` and the console act for people with their own tokens (`PMOS_WEB_TOKEN`,
//! `PMOS_CONSOLE_TS_TOKEN`), which reach only `/internal/approvals*` (the console only its two
//! reads; `pmos-web` also the admin reads). With `PMOS_ASSERTION_MODE`
//! at `verify` or `require`, a call from either that carries `X-Pmos-Assertion` is believed
//! only if the assertion verifies: signed with that client's key for `toolserver`, issued by
//! the client whose token was presented, current, bound to this exact request, and not seen
//! before. Its subject must then equal every person the request names (`user=`,
//! `decided_by`), and a single item it reads must be the subject's. With `require`, a call
//! that names a person without one is refused. Every refusal is the same
//! `401 {"code":"bad_assertion"}`; the log says why, never what the assertion said.
//!
//! The orchestrator's service token and the Slack bot's token are never checked.
//!
//! The shared parts (the mode, the start-up rule, the refusals, the verifier) live in
//! [`pmos_assertion::receiver`]; this module names the tool server as a receiver.

use std::collections::BTreeMap;

use pmos_assertion::receiver::{parse_receiver_keys, Receiver, ReceiverSpec};
use pmos_assertion::KeyRing;

/// `PMOS_ASSERTION_MODE` (`off`, `verify` or `require`; `require` by default once a client
/// that acts for a person is configured) and its development override.
pub use pmos_assertion::receiver::{Mode, Refusal, INSECURE_OFF_VAR, MODE_VAR};
/// `PMOS_ASSERTION_KEYS`: the keys, one entry per caller→receiver pair.
pub use pmos_assertion::KEYS_VAR;

/// This service's name as an assertion audience.
pub const AUDIENCE: &str = "toolserver";
/// The clients that act for people here, and so may issue assertions.
pub const ISSUERS: [&str; 2] = ["web", "console"];

/// the tool server as a receiver of assertions.
#[derive(Debug, Clone, Copy)]
pub struct Spec;

impl ReceiverSpec for Spec {
    const AUDIENCE: &'static str = AUDIENCE;
    const SERVICE: &'static str = "the tool server";
    const ISSUERS: &'static [&'static str] = &ISSUERS;
}

/// the tool server's verifier: the mode, one key ring and policy per issuer, and the replay cache.
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
        let keys = parse_keys(&format!("web:toolserver:k1:{K1}"), "the tool server", ours).unwrap();
        assert!(Assertions::new(Mode::Require, keys).is_ok());
        let err = parse_keys(&format!("web:bots:k1:{K1}"), "the tool server", ours).unwrap_err();
        assert!(err.contains("not a pair") && !err.contains(K1), "{err}");
        let err = parse_keys(
            &format!("slack:toolserver:k1:{K1}"),
            "the tool server",
            ours,
        )
        .unwrap_err();
        assert!(err.contains("not a pair"), "{err}");
    }
}
