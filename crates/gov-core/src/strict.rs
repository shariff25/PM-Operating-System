//! Reading JSON the way a gate must: one meaning or none (review finding GR-62).
//!
//! `serde_json` keeps the last of two members with the same name. Another parser may keep the
//! first. A gate that reads one message and forwards bytes a server reads as another has
//! recorded a call that did not happen and missed the one that did. So a gate refuses a body
//! with a repeated name, or a number it cannot carry exactly, and forwards what it parsed,
//! written out again, never the bytes it received.

use std::fmt;

use serde::de::{DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde::Deserializer;
use serde_json::{Map, Number, Value};

use crate::error::{Error, Result};

/// Parse `bytes` as one JSON value, refusing a repeated member name at any depth and any
/// number that would not be written back as it was read (an integer too large for 64 bits is
/// read as a float, and is refused).
pub fn parse(bytes: &[u8]) -> Result<Value> {
    let mut de = serde_json::Deserializer::from_slice(bytes);
    let value = Strict
        .deserialize(&mut de)
        .map_err(|_| Error::Malformed("JSON with one meaning"))?;
    de.end()
        .map_err(|_| Error::Malformed("JSON with one meaning"))?;
    Ok(value)
}

struct Strict;

impl<'de> DeserializeSeed<'de> for Strict {
    type Value = Value;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> std::result::Result<Value, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Strict {
    type Value = Value;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a JSON value")
    }
    fn visit_bool<E>(self, v: bool) -> std::result::Result<Value, E> {
        Ok(Value::Bool(v))
    }
    fn visit_i64<E>(self, v: i64) -> std::result::Result<Value, E> {
        Ok(Value::Number(v.into()))
    }
    fn visit_u64<E>(self, v: u64) -> std::result::Result<Value, E> {
        Ok(Value::Number(v.into()))
    }
    fn visit_f64<E: serde::de::Error>(self, v: f64) -> std::result::Result<Value, E> {
        // A float with no fraction at or beyond 2^53 is, as often as not, an integer that did
        // not fit and has already lost digits. It cannot be forwarded as it was sent.
        if !v.is_finite() || (v.fract() == 0.0 && v.abs() >= 9_007_199_254_740_992.0) {
            return Err(E::custom("a number that cannot be carried exactly"));
        }
        Number::from_f64(v)
            .map(Value::Number)
            .ok_or_else(|| E::custom("a number that cannot be carried exactly"))
    }
    fn visit_str<E>(self, v: &str) -> std::result::Result<Value, E> {
        Ok(Value::String(v.to_string()))
    }
    fn visit_string<E>(self, v: String) -> std::result::Result<Value, E> {
        Ok(Value::String(v))
    }
    fn visit_unit<E>(self) -> std::result::Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> std::result::Result<Value, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = seq.next_element_seed(Strict)? {
            items.push(item);
        }
        Ok(Value::Array(items))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> std::result::Result<Value, A::Error> {
        let mut out = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if out.contains_key(&key) {
                return Err(serde::de::Error::custom("a member name is repeated"));
            }
            let value = map.next_value_seed(Strict)?;
            out.insert(key, value);
        }
        Ok(Value::Object(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Review finding GR-62: a body two parsers could read two ways is refused.
    #[test]
    fn a_body_with_two_meanings_is_refused() {
        assert!(
            parse(br#"{"method":"tools/call","id":1,"params":{"name":"x","arguments":{}}}"#)
                .is_ok()
        );
        // The reviewer's probe: the first `method` is the call, the last a ping.
        assert!(parse(br#"{"method":"tools/call","method":"ping"}"#).is_err());
        // The same trick one level down, and inside an array.
        assert!(parse(br#"{"params":{"name":"read","name":"delete"}}"#).is_err());
        assert!(parse(br#"[{"a":1,"a":2}]"#).is_err());
        // Two values, or anything after the value.
        assert!(parse(br#"{"a":1}{"b":2}"#).is_err());
        assert!(parse(br#"{"a":1} x"#).is_err());
        assert!(parse(b"not json").is_err());
        // Numbers that would not be written back as they were read.
        assert!(parse(b"[18446744073709551616]").is_err());
        assert!(parse(b"[1e30]").is_err());
        assert!(parse(b"[18446744073709551615, -9223372036854775808, 0.5, 1e3]").is_ok());
    }
}
