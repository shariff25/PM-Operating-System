//! JSON in the canonical form of RFC 8785, for digests only. Signed documents are never
//! re-serialised before their signature is checked (spec §4.1); this form is what the digest
//! of an action's arguments is taken over (spec §4.3).

use serde_json::Value;

use crate::error::{Error, Result};

/// The largest integer a JSON number carries exactly (2^53).
const MAX_SAFE: u64 = 1 << 53;

/// `value` in RFC 8785 canonical form.
///
/// Refuses an integer beyond ±2^53: RFC 8785 numbers are IEEE 754 doubles, and writing such an
/// integer exactly would give a digest another implementation cannot reproduce.
pub fn canonical(value: &Value) -> Result<String> {
    let mut out = String::new();
    write(value, &mut out)?;
    Ok(out)
}

fn write(value: &Value, out: &mut String) -> Result<()> {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(n) => {
            if let Some(u) = n.as_u64() {
                if u > MAX_SAFE {
                    return Err(Error::Number);
                }
                out.push_str(&u.to_string());
            } else if let Some(i) = n.as_i64() {
                if i.unsigned_abs() > MAX_SAFE {
                    return Err(Error::Number);
                }
                out.push_str(&i.to_string());
            } else {
                let f = n.as_f64().ok_or(Error::Number)?;
                out.push_str(&es_number(f)?);
            }
        }
        Value::String(s) => write_string(s, out),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write(item, out)?;
            }
            out.push(']');
        }
        Value::Object(map) => {
            // RFC 8785 §3.2.3: members sorted by their names as UTF-16 code units.
            let mut members: Vec<(Vec<u16>, &String, &Value)> = map
                .iter()
                .map(|(k, v)| (k.encode_utf16().collect(), k, v))
                .collect();
            members.sort_by(|a, b| a.0.cmp(&b.0));
            out.push('{');
            for (i, (_, k, v)) in members.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_string(k, out);
                out.push(':');
                write(v, out)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

/// RFC 8785 §3.2.2.2: `"` and `\` escaped, the five short escapes, other controls as `\u00xx`
/// in lower case, everything else as it is.
fn write_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\u{09}' => out.push_str("\\t"),
            '\u{0a}' => out.push_str("\\n"),
            '\u{0c}' => out.push_str("\\f"),
            '\u{0d}' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// A double as ECMAScript's `Number::toString` writes it (RFC 8785 §3.2.2.3).
fn es_number(f: f64) -> Result<String> {
    if !f.is_finite() {
        return Err(Error::Number);
    }
    if f == 0.0 {
        return Ok("0".to_string());
    }
    let (sign, f) = if f < 0.0 { ("-", -f) } else { ("", f) };
    let (digits, n) = shortest(f)?;
    let k = digits.len() as i32;
    let body = if k <= n && n <= 21 {
        format!("{digits}{}", "0".repeat((n - k) as usize))
    } else if 0 < n && n <= 21 {
        format!("{}.{}", &digits[..n as usize], &digits[n as usize..])
    } else if -6 < n && n <= 0 {
        format!("0.{}{digits}", "0".repeat((-n) as usize))
    } else {
        let e = n - 1;
        let es = if e < 0 { '-' } else { '+' };
        if k == 1 {
            format!("{digits}e{es}{}", e.abs())
        } else {
            format!("{}.{}e{es}{}", &digits[..1], &digits[1..], e.abs())
        }
    };
    Ok(format!("{sign}{body}"))
}

/// The digits and decimal exponent ECMAScript chooses for a positive, finite double: the
/// fewest digits that read back as the same double and, among those, the nearest to its
/// exact value, the even one on a tie. The value is `0.<digits> × 10^n`.
///
/// Rust's own shortest form has the right number of digits but rounds a tie upwards
/// (RFC 8785's vector `1424953923781206.2` shows it), so the last digit is chosen here from
/// the double's exact decimal expansion.
fn shortest(f: f64) -> Result<(String, i32)> {
    fn split(sci: &str) -> Result<(String, i32)> {
        let (mantissa, exp) = sci.split_once('e').ok_or(Error::Number)?;
        let digits = mantissa.chars().filter(|c| *c != '.').collect();
        Ok((digits, exp.parse().map_err(|_| Error::Number)?))
    }
    let (rust_digits, rust_exp) = split(&format!("{f:e}"))?;
    let k = rust_digits.len();
    // A double's exact expansion ends within 767 significant digits.
    let (exact, exp) = split(&format!("{f:.800e}"))?;
    let (head, rest) = exact.split_at(k);
    let half = format!("5{}", "0".repeat(rest.len() - 1));
    let down = (head.to_string(), exp);
    let up = {
        let mut d: Vec<u8> = head.bytes().collect();
        let mut i = d.len();
        loop {
            if i == 0 {
                // 99…9 rounds up to 10…0: one more power of ten.
                d.insert(0, b'1');
                d.pop();
                break (String::from_utf8(d).map_err(|_| Error::Number)?, exp + 1);
            }
            i -= 1;
            if d[i] == b'9' {
                d[i] = b'0';
            } else {
                d[i] += 1;
                break (String::from_utf8(d).map_err(|_| Error::Number)?, exp);
            }
        }
    };
    let last_even = (head.as_bytes()[k - 1] - b'0') % 2 == 0;
    let order = match rest.cmp(half.as_str()) {
        std::cmp::Ordering::Less => [down, up],
        std::cmp::Ordering::Greater => [up, down],
        std::cmp::Ordering::Equal if last_even => [down, up],
        std::cmp::Ordering::Equal => [up, down],
    };
    for (digits, exp) in order {
        // Rust's parser is correctly rounded, so this is an exact round-trip test.
        if format!("0.{digits}e{}", exp + 1).parse::<f64>() == Ok(f) {
            let trimmed = digits.trim_end_matches('0');
            let digits = if trimmed.is_empty() { "0" } else { trimmed };
            return Ok((digits.to_string(), exp + 1));
        }
    }
    Ok((rust_digits, rust_exp + 1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// RFC 8785, Appendix B: IEEE 754 bit patterns and their expected text.
    #[test]
    fn rfc_8785_number_vectors() {
        let cases: &[(u64, &str)] = &[
            (0x0000000000000000, "0"),
            (0x8000000000000000, "0"),
            (0x0000000000000001, "5e-324"),
            (0x8000000000000001, "-5e-324"),
            (0x7fefffffffffffff, "1.7976931348623157e+308"),
            (0xffefffffffffffff, "-1.7976931348623157e+308"),
            (0x4340000000000000, "9007199254740992"),
            (0xc340000000000000, "-9007199254740992"),
            (0x4430000000000000, "295147905179352830000"),
            (0x44b52d02c7e14af5, "9.999999999999997e+22"),
            (0x44b52d02c7e14af6, "1e+23"),
            (0x44b52d02c7e14af7, "1.0000000000000001e+23"),
            (0x444b1ae4d6e2ef4e, "999999999999999700000"),
            (0x444b1ae4d6e2ef4f, "999999999999999900000"),
            (0x444b1ae4d6e2ef50, "1e+21"),
            (0x3eb0c6f7a0b5ed8c, "9.999999999999997e-7"),
            (0x3eb0c6f7a0b5ed8d, "0.000001"),
            (0x41b3de4355555553, "333333333.3333332"),
            (0x41b3de4355555554, "333333333.33333325"),
            (0x41b3de4355555555, "333333333.3333333"),
            (0x41b3de4355555556, "333333333.3333334"),
            (0x41b3de4355555557, "333333333.33333343"),
            (0xbecbf647612f3696, "-0.0000033333333333333333"),
            (0x43143ff3c1cb0959, "1424953923781206.2"),
        ];
        for (bits, want) in cases {
            assert_eq!(
                es_number(f64::from_bits(*bits)).unwrap(),
                *want,
                "{bits:#x}"
            );
        }
        assert!(es_number(f64::NAN).is_err());
        assert!(es_number(f64::INFINITY).is_err());
    }

    /// RFC 8785 §3.2.3: the sorting example, by UTF-16 code units.
    #[test]
    fn rfc_8785_sorting() {
        let v: Value = serde_json::from_str(
            r#"{"\u20ac":"Euro Sign","\r":"Carriage Return","\ufb33":"Hebrew Letter Dalet With Dagesh","1":"One","\ud83d\ude00":"Emoji: Grinning Face","\u0080":"Control","\u00f6":"Latin Small Letter O With Diaeresis"}"#,
        )
        .unwrap();
        let got = canonical(&v).unwrap();
        let order: Vec<usize> = [
            "Carriage Return",
            "One",
            "Control",
            "Latin Small Letter O With Diaeresis",
            "Euro Sign",
            "Emoji: Grinning Face",
            "Hebrew Letter Dalet With Dagesh",
        ]
        .iter()
        .map(|name| got.find(name).unwrap())
        .collect();
        assert!(order.windows(2).all(|w| w[0] < w[1]), "{got}");
        assert!(got.starts_with("{\"\\r\":"));
    }

    /// RFC 8785 §3.2.2: the serialisation example.
    #[test]
    fn rfc_8785_literals_and_strings() {
        let v: Value = serde_json::from_str(
            r#"{"numbers":[333333333.33333329,1E30,4.50,2e-3,0.000000000000000000000000001],"string":"\u20ac$\u000F\u000aA'\u0042\u0022\u005c\\\"\/","literals":[null,true,false]}"#,
        )
        .unwrap();
        assert_eq!(
            canonical(&v).unwrap(),
            "{\"literals\":[null,true,false],\"numbers\":[333333333.3333333,1e+30,4.5,0.002,1e-27],\"string\":\"€$\\u000f\\nA'B\\\"\\\\\\\\\\\"/\"}"
        );
    }

    /// Whatever is written reads back as the same double, on 200,000 doubles from a fixed seed.
    #[test]
    fn every_number_reads_back() {
        let mut x: u64 = 0x2545f4914f6cdd1d;
        let mut checked = 0;
        while checked < 200_000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let f = f64::from_bits(x);
            if !f.is_finite() {
                continue;
            }
            let text = es_number(f).unwrap();
            assert_eq!(text.parse::<f64>().unwrap(), f, "{x:#x} as {text}");
            // No shorter form was missed: Rust's shortest has the same count of digits.
            let count = |s: &str| {
                let m = s.split('e').next().unwrap();
                m.chars()
                    .filter(char::is_ascii_digit)
                    .collect::<String>()
                    .trim_matches('0')
                    .len()
            };
            assert_eq!(count(&text), count(&format!("{f:e}")), "{x:#x} as {text}");
            checked += 1;
        }
    }

    #[test]
    fn key_order_and_spacing_do_not_matter() {
        let a: Value = serde_json::from_str(r#"{"b":1,"a":{"y":[1,2],"x":"s"}}"#).unwrap();
        let b: Value =
            serde_json::from_str("{ \"a\": {\"x\":\"s\", \"y\":[1, 2]}, \"b\": 1.0 }").unwrap();
        assert_eq!(canonical(&a).unwrap(), canonical(&b).unwrap());
        assert_eq!(canonical(&a).unwrap(), r#"{"a":{"x":"s","y":[1,2]},"b":1}"#);
    }

    #[test]
    fn integers_past_two_to_the_53_are_refused() {
        assert!(canonical(&json!(9007199254740992u64)).is_ok());
        assert!(canonical(&json!(9007199254740993u64)).is_err());
        assert!(canonical(&json!(-9007199254740993i64)).is_err());
    }
}
