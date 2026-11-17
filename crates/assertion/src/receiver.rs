//! What a receiving service (the bot runtime, the tool server) shares about checking
//! assertions: the roll-out mode, the start-up rule that keeps it secure, the reasons a call
//! is refused, and the verifier with its replay cache. Each receiver names itself with a
//! [`ReceiverSpec`] and uses `Receiver<ItsSpec>` (`docs/user-assertion.md`, In the services).

use std::collections::BTreeMap;
use std::marker::PhantomData;

use crate::key_list::{parse_key_list, KeyList, KeyUse, KEYS_VAR};
use crate::keys::{KeyRing, VerifyError, VerifyPolicy};
use crate::replay::MemoryReplayCache;
use crate::wire::Assertion;
use crate::RequestBinding;

/// `PMOS_ASSERTION_MODE`: `off`, `verify` or `require`.
pub const MODE_VAR: &str = "PMOS_ASSERTION_MODE";

/// `PMOS_ASSERTION_INSECURE_OFF=1`: a development override that lets a receiver start with a
/// mode other than `require` while a client that acts for a person (`web`, `console`) is
/// configured. Never for production: with it, anyone holding that client's token can act as
/// any PM. Every start-up with it logs a warning at error level. It is refused outright on
/// anything that looks like real data ([`real_data`]): live connectors, or a database without
/// `PMOS_DEV=1`.
pub const INSECURE_OFF_VAR: &str = "PMOS_ASSERTION_INSECURE_OFF";

/// The settings that say a receiver holds real data, where the development override is
/// refused: `PMOS_CONNECTORS=live` (the tool server), or one of these database URLs set
/// without `PMOS_DEV=1` (the tool server's and the runtime's).
pub const DATABASE_VARS: [&str; 2] = ["PMOS_DATABASE_URL", "PMOS_BOTS_DATABASE_URL"];

/// Why this receiver holds real data, if it does: `PMOS_CONNECTORS=live`, or a database URL
/// ([`DATABASE_VARS`]) without `PMOS_DEV=1`. `None` for a fake-data development stack.
pub fn real_data(get: &impl Fn(&str) -> Option<String>) -> Option<String> {
    let set = |k: &str| get(k).is_some_and(|v| !v.trim().is_empty());
    if get("PMOS_CONNECTORS").as_deref().map(str::trim) == Some("live") {
        return Some("PMOS_CONNECTORS=live".into());
    }
    let dev = get("PMOS_DEV").as_deref().map(str::trim) == Some("1");
    DATABASE_VARS
        .iter()
        .find(|k| set(k) && !dev)
        .map(|k| format!("{k} is set without PMOS_DEV=1"))
}

/// The roll-out switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Assertions are ignored: the named person is believed as before.
    #[default]
    Off,
    /// One that is present is checked, and a bad one is refused; one that is missing is fine.
    Verify,
    /// A call from `web` or `console` that names a person must carry a valid one.
    Require,
}

impl Mode {
    /// An empty or missing value is `None`: the caller picks the default.
    pub fn parse_set(value: Option<&str>) -> Result<Option<Self>, String> {
        match value.map(str::trim).unwrap_or_default() {
            "" => Ok(None),
            "off" => Ok(Some(Self::Off)),
            "verify" => Ok(Some(Self::Verify)),
            "require" => Ok(Some(Self::Require)),
            other => Err(format!(
                "{MODE_VAR} must be `off`, `verify` or `require`, not `{other}`"
            )),
        }
    }

    /// As [`Mode::parse_set`], with a missing value read as `off`.
    pub fn parse(value: Option<&str>) -> Result<Self, String> {
        Ok(Self::parse_set(value)?.unwrap_or_default())
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Verify => "verify",
            Self::Require => "require",
        }
    }
}

/// The mode a receiver runs in, given which clients that act for a person are configured
/// (`person_clients`, e.g. `["PMOS_WEB_TOKEN"]`: the names appear in the message).
///
/// - No such client: `PMOS_ASSERTION_MODE` as set, `off` when unset.
/// - Any such client: `require` when unset. `off` or `verify` refuses to start, unless
///   [`INSECURE_OFF_VAR`] is `1` on a fake-data development stack; then it starts, and the
///   second value is the warning to log. With real data ([`real_data`]) the override itself
///   refuses to start.
pub fn secure_mode(
    get: &impl Fn(&str) -> Option<String>,
    person_clients: &[&str],
) -> Result<(Mode, Option<String>), String> {
    let set = Mode::parse_set(get(MODE_VAR).as_deref())?;
    let insecure = match get(INSECURE_OFF_VAR).as_deref().map(str::trim) {
        None | Some("") | Some("0") => false,
        Some("1") => true,
        Some(_) => return Err(format!("{INSECURE_OFF_VAR} must be `1` or unset")),
    };
    if person_clients.is_empty() {
        return Ok((set.unwrap_or_default(), None));
    }
    match set {
        None | Some(Mode::Require) => Ok((Mode::Require, None)),
        Some(mode) if insecure => match real_data(get) {
            Some(why) => Err(format!(
                "{INSECURE_OFF_VAR}=1 is for fake-data development stacks only, and {why}: \
                 with {} set, set {MODE_VAR}=require (docs/user-assertion.md, Modes)",
                person_clients.join(", ")
            )),
            None => Ok((
                mode,
                Some(format!(
                    "INSECURE: {MODE_VAR}={} with {} set and {INSECURE_OFF_VAR}=1. Anyone holding \
                 that token can act as any PM. Development only, never production.",
                    mode.as_str(),
                    person_clients.join(", ")
                )),
            )),
        },
        Some(mode) => Err(format!(
            "{MODE_VAR}={} is not allowed while {} is set: a client that acts for a person \
             needs signed user assertions. Set {MODE_VAR}=require (or leave it unset), or, for \
             development only, {INSECURE_OFF_VAR}=1 (docs/user-assertion.md, Modes)",
            mode.as_str(),
            person_clients.join(", ")
        )),
    }
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

/// Who a receiver is.
pub trait ReceiverSpec: Send + Sync + 'static {
    /// Its name as an assertion audience, e.g. `toolserver`.
    const AUDIENCE: &'static str;
    /// Its name in error messages, e.g. `the tool server`.
    const SERVICE: &'static str;
    /// The clients that act for people here, and so may issue assertions.
    const ISSUERS: &'static [&'static str];
}

/// Parse `PMOS_ASSERTION_KEYS` for receiver `S`. `wanted` says which pairs it uses; an entry
/// for any other pair is refused. Errors name the entry and the pair, never a key.
pub fn parse_receiver_keys<S: ReceiverSpec>(
    spec: &str,
    service: &str,
    wanted: impl Fn(&str, &str) -> bool,
) -> Result<KeyList, String> {
    parse_key_list(spec, service, KeyUse::Checks(S::AUDIENCE), wanted)
}

/// A receiver's verifier: the mode, one key ring and policy per issuer, and the replay cache
/// (one per process: `docs/user-assertion.md`, Replay protection and replicas).
pub struct Receiver<S: ReceiverSpec> {
    mode: Mode,
    verifiers: BTreeMap<String, (KeyRing, VerifyPolicy)>,
    replay: MemoryReplayCache,
    warning: Option<String>,
    spec: PhantomData<S>,
}

impl<S: ReceiverSpec> Default for Receiver<S> {
    fn default() -> Self {
        Self::off()
    }
}

impl<S: ReceiverSpec> std::fmt::Debug for Receiver<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Assertions")
            .field("mode", &self.mode)
            .field("issuers", &self.key_ids())
            .finish()
    }
}

impl<S: ReceiverSpec> Receiver<S> {
    /// Assertions ignored, as before they existed.
    pub fn off() -> Self {
        Self {
            mode: Mode::Off,
            verifiers: BTreeMap::new(),
            replay: MemoryReplayCache::default(),
            warning: None,
            spec: PhantomData,
        }
    }

    /// `keys` maps `(issuer, audience)` to that pair's ring. Any mode but `off` needs at least
    /// one.
    pub fn new(mode: Mode, keys: KeyList) -> Result<Self, String> {
        let mut verifiers = BTreeMap::new();
        for ((issuer, audience), ring) in keys {
            if audience != S::AUDIENCE || !S::ISSUERS.contains(&issuer.as_str()) {
                return Err(format!(
                    "{KEYS_VAR}: {issuer}→{audience} is not a pair {} checks",
                    S::SERVICE
                ));
            }
            // The only issuer accepted is the client whose token was presented.
            let policy =
                VerifyPolicy::new(S::AUDIENCE, &[issuer.as_str()]).map_err(|e| e.to_string())?;
            verifiers.insert(issuer, (ring, policy));
        }
        if mode != Mode::Off && verifiers.is_empty() {
            let pairs: Vec<String> = S::ISSUERS
                .iter()
                .map(|i| format!("{i}:{}", S::AUDIENCE))
                .collect();
            return Err(format!(
                "{MODE_VAR}={} needs {KEYS_VAR} with a key for {} \
                 (docs/user-assertion.md, Keys)",
                mode.as_str(),
                pairs.join(" or ")
            ));
        }
        Ok(Self {
            mode,
            verifiers,
            replay: MemoryReplayCache::default(),
            warning: None,
            spec: PhantomData,
        })
    }

    /// From `PMOS_ASSERTION_MODE`, `PMOS_ASSERTION_INSECURE_OFF` and `PMOS_ASSERTION_KEYS`,
    /// with [`secure_mode`]'s rule for the configured `person_clients`: each is `(issuer, the
    /// setting that configured it)`, e.g. `("web", "PMOS_WEB_TOKEN")`. In `verify` and
    /// `require`, each of them needs its own key, so a client is never half-configured. A
    /// malformed key list refuses to start in every mode.
    pub fn from_lookup(
        get: impl Fn(&str) -> Option<String>,
        person_clients: &[(&str, &str)],
    ) -> Result<Self, String> {
        let names: Vec<&str> = person_clients.iter().map(|(_, name)| *name).collect();
        let (mode, warning) = secure_mode(&get, &names)?;
        let keys = match get(KEYS_VAR).filter(|v| !v.trim().is_empty()) {
            None => BTreeMap::new(),
            Some(spec) => parse_receiver_keys::<S>(&spec, S::SERVICE, |iss, aud| {
                aud == S::AUDIENCE && S::ISSUERS.contains(&iss)
            })?,
        };
        let mut this = Self::new(mode, keys)?;
        if mode != Mode::Off {
            for (issuer, name) in person_clients {
                if !this.verifiers.contains_key(*issuer) {
                    return Err(format!(
                        "{name} is set, so {} needs a {issuer}:{} key in {KEYS_VAR} \
                         (docs/user-assertion.md, Keys)",
                        S::SERVICE,
                        S::AUDIENCE
                    ));
                }
            }
        }
        this.warning = warning;
        Ok(this)
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Set when the service started insecurely with [`INSECURE_OFF_VAR`]: log it loudly.
    pub fn insecure_warning(&self) -> Option<&str> {
        self.warning.as_deref()
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
    use crate::{Claims, Key};

    const K1: &str = "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=";
    const K2: &str = "AgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgI=";

    struct Ts;
    impl ReceiverSpec for Ts {
        const AUDIENCE: &'static str = "toolserver";
        const SERVICE: &'static str = "the tool server";
        const ISSUERS: &'static [&'static str] = &["web", "console"];
    }
    type R = Receiver<Ts>;

    fn ours(iss: &str, aud: &str) -> bool {
        aud == Ts::AUDIENCE && Ts::ISSUERS.contains(&iss)
    }

    fn lookup(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |k: &str| {
            pairs
                .iter()
                .find(|(key, _)| *key == k)
                .map(|(_, v)| v.to_string())
        }
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
    fn person_clients_make_require_the_default_and_insecure_modes_refuse_to_start() {
        let web = &["PMOS_WEB_TOKEN"][..];
        // No person client: as set, off by default.
        assert_eq!(secure_mode(&lookup(&[]), &[]).unwrap(), (Mode::Off, None));
        assert_eq!(
            secure_mode(&lookup(&[(MODE_VAR, "verify")]), &[]).unwrap(),
            (Mode::Verify, None)
        );
        // A person client: require when unset or set so.
        assert_eq!(
            secure_mode(&lookup(&[]), web).unwrap(),
            (Mode::Require, None)
        );
        assert_eq!(
            secure_mode(&lookup(&[(MODE_VAR, "require")]), web).unwrap(),
            (Mode::Require, None)
        );
        // Off or verify refuses to start, naming the token and the fix.
        for mode in ["off", "verify"] {
            let pairs: &'static [(&str, &str)] = if mode == "off" {
                &[(MODE_VAR, "off")]
            } else {
                &[(MODE_VAR, "verify")]
            };
            let err = secure_mode(&lookup(pairs), web).unwrap_err();
            assert!(
                err.contains("PMOS_WEB_TOKEN")
                    && err.contains("require")
                    && err.contains(INSECURE_OFF_VAR),
                "{err}"
            );
        }
        // ... unless the development override is set, and then it says so loudly.
        let (mode, warning) =
            secure_mode(&lookup(&[(MODE_VAR, "off"), (INSECURE_OFF_VAR, "1")]), web).unwrap();
        assert_eq!(mode, Mode::Off);
        assert!(warning.unwrap().starts_with("INSECURE"));
        // The override never lowers require.
        assert_eq!(
            secure_mode(&lookup(&[(INSECURE_OFF_VAR, "1")]), web).unwrap(),
            (Mode::Require, None)
        );
        assert!(secure_mode(&lookup(&[(INSECURE_OFF_VAR, "yes")]), web).is_err());
    }

    /// R3: the development override is refused on real data: live connectors, or a database
    /// without `PMOS_DEV=1`. A fake-data development stack (no database, or `PMOS_DEV=1`, and
    /// fake connectors) still gets it.
    #[test]
    fn the_override_is_refused_with_real_data() {
        let web = &["PMOS_WEB_TOKEN"][..];
        const OFF: (&str, &str) = (MODE_VAR, "off");
        const OV: (&str, &str) = (INSECURE_OFF_VAR, "1");
        let refused: [&'static [(&str, &str)]; 4] = [
            &[OFF, OV, ("PMOS_CONNECTORS", "live")],
            &[OFF, OV, ("PMOS_CONNECTORS", "live"), ("PMOS_DEV", "1")],
            &[OFF, OV, ("PMOS_DATABASE_URL", "postgresql://db/pmos")],
            &[OFF, OV, ("PMOS_BOTS_DATABASE_URL", "postgresql://db/bots")],
        ];
        for pairs in refused {
            let err = secure_mode(&lookup(pairs), web).unwrap_err();
            assert!(
                err.contains(INSECURE_OFF_VAR)
                    && err.contains("fake-data development")
                    && !err.contains("postgresql://"),
                "{err}"
            );
        }
        let allowed: [&'static [(&str, &str)]; 3] = [
            &[OFF, OV],
            &[OFF, OV, ("PMOS_CONNECTORS", "fake"), ("PMOS_DEV", "1")],
            &[
                OFF,
                OV,
                ("PMOS_DEV", "1"),
                ("PMOS_BOTS_DATABASE_URL", "postgresql://x"),
            ],
        ];
        for pairs in allowed {
            let (mode, warning) = secure_mode(&lookup(pairs), web).unwrap();
            assert_eq!(mode, Mode::Off);
            assert!(warning.is_some());
        }
        // Without a person client, nothing changes: the override isn't in play.
        assert_eq!(
            secure_mode(&lookup(&[OFF, OV, ("PMOS_CONNECTORS", "live")]), &[])
                .unwrap()
                .0,
            Mode::Off
        );
    }

    #[test]
    fn require_by_default_needs_keys() {
        let web = [("web", "PMOS_WEB_TOKEN")];
        let err = R::from_lookup(lookup(&[]), &web).unwrap_err();
        assert!(
            err.contains(KEYS_VAR) && err.contains("web:toolserver"),
            "{err}"
        );
        // Each configured client needs its own key: the console's alone doesn't do for web.
        let console_key: &'static [(&str, &str)] = &[(
            KEYS_VAR,
            "console:toolserver:c1:AgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgI=",
        )];
        let both = [
            ("web", "PMOS_WEB_TOKEN"),
            ("console", "PMOS_CONSOLE_TS_TOKEN"),
        ];
        let err = R::from_lookup(lookup(console_key), &both).unwrap_err();
        assert!(
            err.contains("PMOS_WEB_TOKEN")
                && err.contains("web:toolserver")
                && !err.contains("AgIC"),
            "{err}"
        );
        assert_eq!(
            R::from_lookup(lookup(console_key), &both[1..])
                .unwrap()
                .mode(),
            Mode::Require
        );
        assert_eq!(R::from_lookup(lookup(&[]), &[]).unwrap().mode(), Mode::Off);
        let err = R::from_lookup(lookup(&[(MODE_VAR, "require")]), &[]).unwrap_err();
        assert!(err.contains(KEYS_VAR), "{err}");
        // A malformed list is refused even when off.
        assert!(R::from_lookup(lookup(&[(KEYS_VAR, "web:toolserver:k1")]), &[]).is_err());
    }

    #[test]
    fn keys_parse_per_pair_with_an_optional_previous_key() {
        let keys = parse_receiver_keys::<Ts>(
            &format!("web:toolserver:k2:{K2}:k1:{K1}, console:toolserver:c1:{K1}"),
            "the tool server",
            ours,
        )
        .unwrap();
        let web = &keys[&("web".to_string(), "toolserver".to_string())];
        assert_eq!(web.current_id(), "k2");
        assert_eq!(web.previous_id(), Some("k1"));
    }

    #[test]
    fn debug_shows_key_ids_only() {
        let keys =
            parse_receiver_keys::<Ts>(&format!("web:toolserver:k1:{K1}"), "x", ours).unwrap();
        let a = R::new(Mode::Verify, keys).unwrap();
        let shown = format!("{a:?}");
        assert!(shown.contains("web:k1") && !shown.contains(K1), "{shown}");
    }

    #[test]
    fn verify_binds_the_issuer_to_the_presented_client() {
        let a = R::new(
            Mode::Verify,
            parse_receiver_keys::<Ts>(
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
        let now = crate::unix_now();
        let header = ring.sign(&claims, now, 60, Some(&req)).unwrap();
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
        // Replayed: refused.
        assert!(matches!(
            a.verify("web", &header, "GET", "/internal/approvals", b"", now),
            Err(Refusal::Verify(VerifyError::Replay))
        ));
    }
}
