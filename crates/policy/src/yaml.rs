//! A small, deterministic YAML writer: block style, map keys sorted, two-space indents.
//!
//! Generated policy files are compared byte for byte by `pmos policy check`, so the output
//! must not depend on field declaration order or on a YAML library's formatting choices.
//! Strings are written plain only when no YAML 1.1 or 1.2 reader could take them for anything
//! but a string (so `2025-11-25`, `*`, `true` and `8710` as text are quoted).

use serde::Serialize;
use serde_json::{Map, Value};

/// Render `value` as YAML. Fails only for values JSON can't hold (non-string map keys).
pub fn to_yaml<T: Serialize>(value: &T) -> Result<String, serde_json::Error> {
    let value = serde_json::to_value(value)?;
    let mut out = String::new();
    match &value {
        Value::Object(map) if !map.is_empty() => write_map(map, 0, &mut out),
        other => {
            out.push_str(&inline(other));
            out.push('\n');
        }
    }
    Ok(out)
}

fn sorted(map: &Map<String, Value>) -> Vec<(&String, &Value)> {
    let mut entries: Vec<_> = map.iter().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    entries
}

fn pad(out: &mut String, indent: usize) {
    out.extend(std::iter::repeat_n(' ', indent));
}

fn write_map(map: &Map<String, Value>, indent: usize, out: &mut String) {
    for (key, value) in sorted(map) {
        pad(out, indent);
        write_entry(key, value, indent, out);
    }
}

/// `key: value` where the key is already positioned; nested blocks go at `indent + 2`.
fn write_entry(key: &str, value: &Value, indent: usize, out: &mut String) {
    out.push_str(&string(key));
    out.push(':');
    match value {
        Value::Object(map) if !map.is_empty() => {
            out.push('\n');
            write_map(map, indent + 2, out);
        }
        Value::Array(items) if !items.is_empty() => {
            out.push('\n');
            write_seq(items, indent + 2, out);
        }
        other => {
            out.push(' ');
            out.push_str(&inline(other));
            out.push('\n');
        }
    }
}

fn write_seq(items: &[Value], indent: usize, out: &mut String) {
    for item in items {
        pad(out, indent);
        out.push_str("- ");
        match item {
            Value::Object(map) if !map.is_empty() => {
                for (i, (key, value)) in sorted(map).into_iter().enumerate() {
                    if i > 0 {
                        pad(out, indent + 2);
                    }
                    write_entry(key, value, indent + 2, out);
                }
            }
            Value::Array(inner) if !inner.is_empty() => {
                out.push('\n');
                write_seq(inner, indent + 2, out);
            }
            other => {
                out.push_str(&inline(other));
                out.push('\n');
            }
        }
    }
}

/// A scalar, or an empty collection, on one line.
fn inline(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => string(s),
        Value::Array(_) => "[]".into(),
        Value::Object(_) => "{}".into(),
    }
}

fn string(s: &str) -> String {
    if plain(s) {
        s.to_string()
    } else {
        // A JSON string literal is a valid YAML double-quoted scalar.
        serde_json::to_string(s).unwrap_or_else(|_| format!("\"{s}\""))
    }
}

/// Safe to write unquoted: starts with a letter, `_` or `/`, uses only letters, digits and
/// `_ - . /`, and isn't a word some YAML reader treats as a boolean or null.
fn plain(s: &str) -> bool {
    const RESERVED: [&str; 11] = [
        "true", "false", "yes", "no", "on", "off", "y", "n", "null", "nan", "inf",
    ];
    let Some(first) = s.chars().next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_' || first == '/')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/'))
        && !RESERVED.contains(&s.to_ascii_lowercase().as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sorts_keys_and_uses_block_style() {
        let v = json!({
            "zeta": 1,
            "alpha": {"b": [1, 2], "a": "x"},
            "list": [{"second": "two", "first": "one"}, "plain"],
            "empty_list": [],
            "empty_map": {},
        });
        let expected = "\
alpha:
  a: x
  b:
    - 1
    - 2
empty_list: []
empty_map: {}
list:
  - first: one
    second: two
  - plain
zeta: 1
";
        assert_eq!(to_yaml(&v).unwrap(), expected);
    }

    #[test]
    fn quotes_anything_a_reader_could_misread() {
        for s in [
            "2025-11-25",
            "*",
            "true",
            "No",
            "8710",
            "",
            "a b",
            "a: b",
            "~",
            "-x",
        ] {
            assert!(!plain(s), "{s} should be quoted");
        }
        for s in [
            "sandbox",
            "/usr/local/bin/node",
            "tools/call",
            "docs_search",
            "pmos-docs-gateway",
            "gateway.pmos.internal",
        ] {
            assert!(plain(s), "{s} should be plain");
        }
        assert_eq!(string("2025-11-25"), "\"2025-11-25\"");
    }

    #[test]
    fn nested_lists_in_lists_and_maps_in_lists_round_trip() {
        let v = json!({"a": [[1, 2], {"k": {"deep": [true]}}]});
        let yaml = to_yaml(&v).unwrap();
        let back: Value = serde_norway::from_str(&yaml).unwrap();
        assert_eq!(back, v);
    }

    #[test]
    fn output_is_valid_yaml_that_reads_back_identically() {
        let v = json!({
            "version": 1,
            "s": ["2025-03-26", "*", "x y", "quote\"inside", "/mcp"],
            "m": {"k-1": {"n": null, "b": false}},
        });
        let back: Value = serde_norway::from_str(&to_yaml(&v).unwrap()).unwrap();
        assert_eq!(back, v);
    }
}
