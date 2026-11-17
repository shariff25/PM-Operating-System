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

use std::collections::BTreeMap;

use pmos_assertion::{
    Assertion, KeyRing, KeyUse, MemoryReplayCache, RequestBinding, VerifyError, VerifyPolicy,
};

/// `PMOS_ASSERTION_KEYS`: the keys, one entry per caller→receiver pair.
pub use pmos_assertion::KEYS_VAR;
/// `PMOS_ASSERTION_MODE`: `off` (default), `verify` or `require`.
pub const MODE_VAR: &str = "PMOS_ASSERTION_MODE";
/// This service's name as an assertion audience.
pub const AUDIENCE: &str = "toolserver";
/// The clients that act for people here, and so may issue assertions.
pub const ISSUERS: [&str; 2] = ["web", "console"];

/// The roll-out switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Assertions are ignored: `X-Pmos-User` is believed as before.
    #[default]
    Off,
    /// One that is present is checked, and a bad one is refused; one that is missing is fine.
    Verify,
    /// A call from `web` or `console` that names a person must carry a valid one.
    Require,
}

impl Mode {
    pub fn parse(value: Option<&str>) -> Result<Self, String> {
        match value.map(str::trim).unwrap_or_default() {
            "" | "off" => Ok(Self::Off),
            "verify" => Ok(Self::Verify),
            "require" => Ok(Self::Require),
            other => Err(format!(
                "{MODE_VAR} must be `off`, `verify` or `require`, not `{other}`"
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Verify => "verify",
            Self::Require => "require",
        }
    }
}

/// Parse `PMOS_ASSERTION_KEYS` (`pmos_assertion::parse_key_list`, shared by every service
/// that signs or checks): comma-separated `<issuer>:<audience>:<key id>:<base64 key>` entries,
/// optionally followed by the previous key. `wanted` says which pairs this service uses; an
/// entry for any other pair is refused. Errors name the entry and the pair, never a key.
pub fn parse_keys(
    spec: &str,
    service: &str,
    wanted: impl Fn(&str, &str) -> bool,
) -> Result<BTreeMap<(String, String), KeyRing>, String> {
    pmos_assertion::parse_key_list(spec, service, KeyUse::Checks(AUDIENCE), wanted)
}

/// Why a call's assertion was refused. Logged; the caller only ever sees one `401`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// `require`, the call names a person, and there is no assertion.
    Missing,
    /// The header is not visible ASCII.
    Header,
    /// No key is configured for this issuer.
    NoKey,
    /// The body could not be read for the request binding.
    Body,
    /// The request's method or target can't be bound.
    Target,
    /// The assertion itself failed.
    Verify(VerifyError),
    /// A person the request names is not the assertion's subject.
    SubjectMismatch,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing => f.write_str("missing"),
            Self::Header => f.write_str("header is not visible ASCII"),
            Self::NoKey => f.write_str("no key for this issuer"),
            Self::Body => f.write_str("body unreadable"),
            Self::Target => f.write_str("request can't be bound"),
            Self::Verify(e) => write!(f, "{e}"),
            Self::SubjectMismatch => f.write_str("subject is not the person the request names"),
        }
    }
}

/// The tool server's verifier: the mode, one key ring and policy per issuer, and the replay cache.
pub struct Assertions {
    mode: Mode,
    verifiers: BTreeMap<String, (KeyRing, VerifyPolicy)>,
    replay: MemoryReplayCache,
}

impl Default for Assertions {
    fn default() -> Self {
        Self::off()
    }
}

impl std::fmt::Debug for Assertions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Assertions")
            .field("mode", &self.mode)
            .field("issuers", &self.key_ids())
            .finish()
    }
}

impl Assertions {
    /// Assertions ignored, as before they existed.
    pub fn off() -> Self {
        Self {
            mode: Mode::Off,
            verifiers: BTreeMap::new(),
            replay: MemoryReplayCache::default(),
        }
    }

    /// `keys` maps `(issuer, toolserver)` to that pair's ring. Any mode but `off` needs at least one.
    pub fn new(mode: Mode, keys: BTreeMap<(String, String), KeyRing>) -> Result<Self, String> {
        let mut verifiers = BTreeMap::new();
        for ((issuer, audience), ring) in keys {
            if audience != AUDIENCE || !ISSUERS.contains(&issuer.as_str()) {
                return Err(format!(
                    "{KEYS_VAR}: {issuer}→{audience} is not a pair the tool server checks"
                ));
            }
            // The only issuer accepted is the client whose token was presented.
            let policy =
                VerifyPolicy::new(AUDIENCE, &[issuer.as_str()]).map_err(|e| e.to_string())?;
            verifiers.insert(issuer, (ring, policy));
        }
        if mode != Mode::Off && verifiers.is_empty() {
            return Err(format!(
                "{MODE_VAR}={} needs {KEYS_VAR} with a key for web:toolserver or console:toolserver \
                 (docs/user-assertion.md, Keys)",
                mode.as_str()
            ));
        }
        Ok(Self {
            mode,
            verifiers,
            replay: MemoryReplayCache::default(),
        })
    }

    /// From `PMOS_ASSERTION_MODE` and `PMOS_ASSERTION_KEYS`. A malformed key list refuses to
    /// start in every mode.
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let mode = Mode::parse(get(MODE_VAR).as_deref())?;
        let keys = match get(KEYS_VAR).filter(|v| !v.trim().is_empty()) {
            None => BTreeMap::new(),
            Some(spec) => parse_keys(&spec, "the tool server", |iss, aud| {
                aud == AUDIENCE && ISSUERS.contains(&iss)
            })?,
        };
        Self::new(mode, keys)
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Each issuer with a key, and the key ids it may sign with (never the keys).
    pub fn key_ids(&self) -> Vec<String> {
        self.verifiers
            .iter()
            .map(|(issuer, (ring, _))| match ring.previous_id() {
                Some(prev) => format!("{issuer}:{},{prev}", ring.current_id()),
                None => format!("{issuer}:{}", ring.current_id()),
            })
            .collect()
    }

    /// Verify `header` from client `issuer` for the request `method target` with `body`, at
    /// `now` (unix seconds).
    pub fn verify(
        &self,
        issuer: &str,
        header: &str,
        method: &str,
        target: &str,
        body: &[u8],
        now: u64,
    ) -> Result<Assertion, Refusal> {
        let (ring, policy) = self.verifiers.get(issuer).ok_or(Refusal::NoKey)?;
        let binding = RequestBinding::new(method, target, body).map_err(|_| Refusal::Target)?;
        ring.verify(header, policy, now, Some(&binding), &self.replay)
            .map_err(Refusal::Verify)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pmos_assertion::{Claims, Key};

    const K1: &str = "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=";
    const K2: &str = "AgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgI=";

    fn ours(iss: &str, aud: &str) -> bool {
        aud == AUDIENCE && ISSUERS.contains(&iss)
    }

    #[test]
    fn modes() {
        assert_eq!(Mode::parse(None).unwrap(), Mode::Off);
        assert_eq!(Mode::parse(Some("")).unwrap(), Mode::Off);
        assert_eq!(Mode::parse(Some("verify")).unwrap(), Mode::Verify);
        assert_eq!(Mode::parse(Some(" require ")).unwrap(), Mode::Require);
        assert!(Mode::parse(Some("on")).unwrap_err().contains(MODE_VAR));
    }

    #[test]
    fn keys_parse_per_pair_with_an_optional_previous_key() {
        let keys = parse_keys(
            &format!("web:toolserver:k2:{K2}:k1:{K1}, console:toolserver:c1:{K1}"),
            "the tool server",
            ours,
        )
        .unwrap();
        let web = &keys[&("web".to_string(), "toolserver".to_string())];
        assert_eq!(web.current_id(), "k2");
        assert_eq!(web.previous_id(), Some("k1"));
        assert_eq!(
            keys[&("console".to_string(), "toolserver".to_string())].current_id(),
            "c1"
        );
    }

    #[test]
    fn bad_keys_refuse_to_start_without_showing_the_key() {
        let short = "AQEBAQEBAQEBAQEBAQEBAQ=="; // 16 bytes
        for (spec, want) in [
            (format!("web:toolserver:{K1}"), "expected issuer:audience"),
            (
                format!("web:toolserver:k1:{K1}:k2"),
                "expected issuer:audience",
            ),
            (format!("Web:toolserver:k1:{K1}"), "client names"),
            (format!("web:bots:k1:{K1}"), "not a pair"),
            (format!("bot:toolserver:k1:{K1}"), "not a pair"),
            (format!("web:toolserver:k1:{short}"), "32 to 64 bytes"),
            ("web:toolserver:k1:not base64!".to_string(), "base64"),
            (format!("web:toolserver:k1:{K1}:k1:{K2}"), "same id"),
            (
                format!("web:toolserver:k1:{K1},web:toolserver:k2:{K2}"),
                "listed twice",
            ),
            (format!("web:toolserver:{K1}:k1"), "current key"),
        ] {
            let err = parse_keys(&spec, "the tool server", ours).unwrap_err();
            assert!(err.contains(want), "{spec}: {err}");
            assert!(
                !err.contains(K1) && !err.contains(K2) && !err.contains(short),
                "{err}"
            );
        }
    }

    #[test]
    fn a_mode_on_needs_keys() {
        let get = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == k)
                    .map(|(_, v)| v.to_string())
            }
        };
        assert_eq!(Assertions::from_lookup(get(&[])).unwrap().mode(), Mode::Off);
        let err = Assertions::from_lookup(get(&[(MODE_VAR, "require")])).unwrap_err();
        assert!(err.contains(KEYS_VAR), "{err}");
        // A malformed list is refused even when off.
        assert!(Assertions::from_lookup(get(&[(KEYS_VAR, "web:toolserver:k1")])).is_err());
    }

    #[test]
    fn debug_shows_key_ids_only() {
        let keys = parse_keys(&format!("web:toolserver:k1:{K1}"), "x", ours).unwrap();
        let a = Assertions::new(Mode::Verify, keys).unwrap();
        let shown = format!("{a:?}");
        assert!(shown.contains("web:k1") && !shown.contains(K1), "{shown}");
    }

    #[test]
    fn verify_binds_the_issuer_to_the_presented_client() {
        let a = Assertions::new(
            Mode::Verify,
            parse_keys(
                &format!("web:toolserver:k1:{K1},console:toolserver:c1:{K2}"),
                "x",
                ours,
            )
            .unwrap(),
        )
        .unwrap();
        let ring = KeyRing::new(Key::from_base64("k1", K1).unwrap());
        let claims = Claims::new("web", "toolserver", "ana", &[], "s1").unwrap();
        let req = RequestBinding::new("GET", "/internal/approvals", b"").unwrap();
        let now = pmos_assertion::unix_now();
        let header = ring.sign(&claims, now, 60, Some(&req)).unwrap();
        // Presented with the console's token: the web key isn't the console's.
        assert_eq!(
            a.verify("console", &header, "GET", "/internal/approvals", b"", now),
            Err(Refusal::Verify(VerifyError::UnknownKey))
        );
        assert_eq!(
            a.verify("web", &header, "GET", "/internal/approvals", b"", now)
                .unwrap()
                .subject(),
            "ana"
        );
    }
}
