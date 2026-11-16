//! `PMOS_ASSERTION_KEYS`, the key list every service that signs or checks assertions reads
//! (`docs/user-assertion.md`, Keys).

use std::collections::BTreeMap;

use crate::keys::{Key, KeyRing, VerifyPolicy};

/// The variable the key list is read from.
pub const KEYS_VAR: &str = "PMOS_ASSERTION_KEYS";

/// What the service reading the list does with its keys. It only changes the wording of an
/// error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyUse<'a> {
    /// A caller (`pmos-web`, the console): it signs.
    Signs,
    /// A receiver (the bot runtime, the tool server): it checks assertions for this audience.
    Checks(&'a str),
}

/// The keys by `(issuer, audience)` pair.
pub type KeyList = BTreeMap<(String, String), KeyRing>;

/// Parse [`KEYS_VAR`]: comma-separated entries, one per caller→receiver pair,
///
/// ```text
/// <issuer>:<audience>:<key id>:<base64 key>[:<key id>:<base64 key>]
/// ```
///
/// the current key first, then optionally the previous one (still accepted while a rotation
/// finishes). Keys are 32 to 64 bytes in standard base64. `wanted` says which pairs this
/// service (named `service` in errors) uses; an entry for any other pair is refused, so a
/// service never holds a key it has no use for. Errors name the entry and the pair, never a
/// key.
pub fn parse_key_list(
    spec: &str,
    service: &str,
    uses: KeyUse<'_>,
    wanted: impl Fn(&str, &str) -> bool,
) -> Result<KeyList, String> {
    let (verb, example) = match uses {
        KeyUse::Signs => ("signs", "bots"),
        KeyUse::Checks(audience) => ("signs or checks", audience),
    };
    let mut out = BTreeMap::new();
    let entries = spec.split(',').map(str::trim).filter(|e| !e.is_empty());
    for (i, entry) in entries.enumerate() {
        let bad = |why: &str| format!("{KEYS_VAR} entry {}: {why}", i + 1);
        let parts: Vec<&str> = entry.split(':').map(str::trim).collect();
        if parts.len() != 4 && parts.len() != 6 {
            return Err(bad(
                "expected issuer:audience:key-id:base64-key, optionally followed by \
                 :key-id:base64-key for the previous key",
            ));
        }
        let (issuer, audience) = (parts[0], parts[1]);
        if VerifyPolicy::new(audience, &[issuer]).is_err() {
            return Err(bad(&format!(
                "the issuer and audience must be client names such as `web` and `{example}`"
            )));
        }
        if !wanted(issuer, audience) {
            return Err(bad(&format!(
                "{issuer}→{audience} is not a pair {service} {verb}; give each service only \
                 its own keys"
            )));
        }
        let key = |id: &str, b64: &str, which: &str| {
            Key::from_base64(id, b64).map_err(|e| bad(&format!("the {which} key: {e}")))
        };
        let mut ring = KeyRing::new(key(parts[2], parts[3], "current")?);
        if parts.len() == 6 {
            ring = ring
                .with_previous(key(parts[4], parts[5], "previous")?)
                .map_err(|e| bad(&e.to_string()))?;
        }
        if out
            .insert((issuer.to_string(), audience.to_string()), ring)
            .is_some()
        {
            return Err(bad(&format!(
                "{issuer}→{audience} is listed twice; list each pair once, current key first"
            )));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const K1: &str = "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=";
    const K2: &str = "AgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgI=";

    fn web_bots(iss: &str, aud: &str) -> bool {
        iss == "web" && aud == "bots"
    }

    #[test]
    fn entries_parse_per_pair_with_an_optional_previous_key() {
        let keys = parse_key_list(
            &format!(" web:bots:k2:{K2}:k1:{K1} ,"),
            "x",
            KeyUse::Signs,
            web_bots,
        )
        .unwrap();
        let ring = &keys[&("web".to_string(), "bots".to_string())];
        assert_eq!(ring.current_id(), "k2");
        assert_eq!(ring.previous_id(), Some("k1"));
        assert!(parse_key_list("", "x", KeyUse::Signs, web_bots)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn the_wording_follows_what_the_service_does() {
        let other = format!("web:toolserver:k1:{K1}");
        let signs = parse_key_list(&other, "pmos-web", KeyUse::Signs, web_bots).unwrap_err();
        assert_eq!(
            signs,
            "PMOS_ASSERTION_KEYS entry 1: web→toolserver is not a pair pmos-web signs; give \
             each service only its own keys"
        );
        let checks = parse_key_list(&other, "the bot runtime", KeyUse::Checks("bots"), web_bots)
            .unwrap_err();
        assert!(
            checks.contains("is not a pair the bot runtime signs or checks;"),
            "{checks}"
        );
        let bad_name = format!("Web:toolserver:k1:{K1}");
        let named =
            parse_key_list(&bad_name, "x", KeyUse::Checks("toolserver"), web_bots).unwrap_err();
        assert!(named.ends_with("such as `web` and `toolserver`"), "{named}");
        let named = parse_key_list(&bad_name, "x", KeyUse::Signs, web_bots).unwrap_err();
        assert!(named.ends_with("such as `web` and `bots`"), "{named}");
    }

    #[test]
    fn errors_never_show_a_key() {
        for spec in [
            format!("web:bots:{K1}"),
            format!("web:bots:k1:{K1},web:bots:k2:{K2}"),
            format!("web:bots:k1:{K1}:k1:{K2}"),
            format!("web:bots:{K1}:k1"),
        ] {
            let err = parse_key_list(&spec, "x", KeyUse::Signs, web_bots).unwrap_err();
            assert!(err.starts_with(KEYS_VAR), "{err}");
            assert!(!err.contains(K1) && !err.contains(K2), "{err}");
        }
    }
}
