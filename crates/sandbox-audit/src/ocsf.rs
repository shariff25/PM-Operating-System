//! OpenShell's OCSF records, as an allowlist (`docs/openshell-prover-and-sandbox-audit-spec.md`
//! §B.2): the few fields PM OS keeps, cleaned; everything else is never read into a value that
//! leaves this module. Dropped always: `cmd_line`, `message`, headers, bodies, `pid`,
//! `src_endpoint`, the URL's scheme, credentials, query and fragment, and every field not named
//! here. Kept strings are printable ASCII (anything else becomes `?`), length-capped.
//!
//! **Verify on a real host (slice B6):** where OpenShell's OTLP log record carries the OCSF
//! event (read here as the body: a JSON string, or a key-value map), the class numbers
//! (network 4001, HTTP 4002, finding 2004, API 6003; the policy load read here as 5019, "device
//! config state change"), how a policy load names its hash (read as `unmapped.policy_hash` or
//! the attribute `openshell.policy.hash`), the gap record's form (read as an event named, or
//! an attribute starting with, `openshell.ocsf.gap`), and where the sandbox id is
//! (`openshell.sandbox.id` on the record or its resource).

use chrono::{DateTime, TimeZone, Utc};
use serde_json::Value;

use crate::otlp::LogRecord;

pub const MAX_HOST: usize = 253;
pub const MAX_IP: usize = 45;
pub const MAX_PATH: usize = 256;
pub const MAX_PROCESS: usize = 128;
pub const MAX_RULE: usize = 128;
pub const MAX_SANDBOX: usize = 64;
pub const MAX_UID: usize = 128;
pub const MAX_VERSION: usize = 64;

/// `s` as printable ASCII (anything else, line breaks, ANSI escapes and non-ASCII included,
/// becomes `?`), at most `max` bytes; `None` when nothing is left.
pub fn clean(s: &str, max: usize) -> Option<String> {
    let out: String = s
        .chars()
        .map(|c| if (' '..='~').contains(&c) { c } else { '?' })
        .take(max)
        .collect();
    let out = out.trim().to_string();
    (!out.is_empty()).then_some(out)
}

/// A URL or path as its path only: no scheme, credentials, host, query or fragment; cleaned;
/// at most 256 bytes. `None` if nothing path-like is left.
pub fn path_only(raw: &str) -> Option<String> {
    let no_frag = raw.split('#').next().unwrap_or("");
    let no_query = no_frag.split('?').next().unwrap_or("");
    let path = match no_query.find("://") {
        Some(i) => {
            let rest = &no_query[i + 3..];
            match rest.find('/') {
                Some(j) => &rest[j..],
                None => "/",
            }
        }
        None => no_query,
    };
    let path = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    };
    clean(&path, MAX_PATH)
}

/// The HTTP method, or `OTHER`.
pub fn method(raw: &str) -> String {
    let up = raw.trim().to_ascii_uppercase();
    if !up.is_empty() && up.len() <= 16 && up.bytes().all(|b| b.is_ascii_uppercase()) {
        up
    } else {
        "OTHER".into()
    }
}

/// OpenShell's `status_detail` as one of the fixed reasons.
pub fn reason(status_detail: Option<&str>) -> &'static str {
    let s = status_detail.unwrap_or("").to_ascii_lowercase();
    if s.contains("no matching policy") {
        "no_matching_policy"
    } else if s.contains("always-blocked") || s.contains("always blocked") {
        "always_blocked"
    } else if s.contains("allowed_ips") {
        "not_in_allowed_ips"
    } else if s.contains("l7 deny") || s.contains("l7_deny") {
        "l7_deny"
    } else if s.contains("cross_sandbox") {
        "cross_sandbox"
    } else {
        "other"
    }
}

/// What a decision is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Decision {
    Deny,
    Allow,
    Finding,
    PolicyLoaded,
}

/// The kept fields of one record.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Kept {
    pub at: Option<DateTime<Utc>>,
    pub uid: Option<String>,
    pub version: Option<String>,
    pub correlation_uid: Option<String>,
    pub rule: Option<String>,
    pub reason: Option<&'static str>,
    pub host: Option<String>,
    pub ip: Option<String>,
    pub port: Option<u32>,
    pub method: Option<String>,
    pub path: Option<String>,
    pub process: Option<String>,
    pub sandbox: Option<String>,
    pub sandbox_uid: Option<String>,
    pub policy_hash: Option<String>,
}

/// One record, classified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Record {
    Decision(Decision, Box<Kept>),
    /// OpenShell's own report of records it dropped (`openshell.ocsf.gap`).
    Gap {
        dropped: u64,
        since: Option<DateTime<Utc>>,
        sandbox_uid: Option<String>,
    },
    /// Not the documented shape: counted, never dropped silently.
    Unparsed {
        version: Option<String>,
    },
    /// A well-formed record of a class PM OS doesn't forward (not a decision).
    Ignored,
}

fn get<'a>(v: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut cur = v;
    for p in path {
        cur = cur.get(p)?;
    }
    Some(cur)
}

fn text(v: &Value, path: &[&str], max: usize) -> Option<String> {
    match get(v, path)? {
        Value::String(s) => clean(s, max),
        Value::Number(n) => clean(&n.to_string(), max),
        _ => None,
    }
}

fn attr_text(r: &LogRecord, key: &str, max: usize) -> Option<String> {
    r.attributes
        .get(key)
        .or_else(|| r.resource.get(key))
        .and_then(|v| match v {
            Value::String(s) => clean(s, max),
            Value::Number(n) => clean(&n.to_string(), max),
            _ => None,
        })
}

fn ms_time(v: Option<&Value>) -> Option<DateTime<Utc>> {
    let ms = v?.as_i64()?;
    Utc.timestamp_millis_opt(ms).single()
}

/// Classify one OTLP log record.
pub fn classify(r: &LogRecord) -> Record {
    let gap = r.event_name.as_deref() == Some("openshell.ocsf.gap")
        || r.attributes
            .keys()
            .any(|k| k.starts_with("openshell.ocsf.gap"))
        || matches!(&r.body, Some(Value::String(s)) if s == "openshell.ocsf.gap");
    if gap {
        let dropped = r
            .attributes
            .get("openshell.ocsf.gap.dropped")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        return Record::Gap {
            dropped,
            since: ms_time(r.attributes.get("openshell.ocsf.gap.since_unix_ms")),
            sandbox_uid: attr_text(r, "openshell.sandbox.id", MAX_UID),
        };
    }
    let event: Value = match &r.body {
        Some(Value::String(s)) => match serde_json::from_str::<Value>(s) {
            Ok(v @ Value::Object(_)) => v,
            _ => return Record::Unparsed { version: None },
        },
        Some(v @ Value::Object(_)) => v.clone(),
        _ => return Record::Unparsed { version: None },
    };
    let version = text(&event, &["metadata", "product", "version"], MAX_VERSION);
    let Some(class) = event.get("class_uid").and_then(Value::as_u64) else {
        return Record::Unparsed { version };
    };
    let action = event
        .get("action")
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase)
        .or_else(|| {
            event
                .get("action_id")
                .and_then(Value::as_u64)
                .map(|id| match id {
                    1 => "allowed".into(),
                    2 => "denied".into(),
                    _ => "other".into(),
                })
        });
    let status_detail = event.get("status_detail").and_then(Value::as_str);
    let at = ms_time(event.get("time"))
        .or_else(|| (r.time_unix_nano > 0).then(|| Utc.timestamp_nanos(r.time_unix_nano as i64)));
    let kept = |reason: Option<&'static str>| Kept {
        at,
        uid: text(&event, &["metadata", "uid"], MAX_UID)
            .or_else(|| attr_text(r, "ocsf.metadata.uid", MAX_UID)),
        version: version.clone(),
        correlation_uid: text(&event, &["metadata", "correlation_uid"], MAX_UID),
        rule: text(&event, &["firewall_rule", "name"], MAX_RULE),
        reason,
        host: text(&event, &["dst_endpoint", "domain"], MAX_HOST),
        ip: text(&event, &["dst_endpoint", "ip"], MAX_IP),
        port: get(&event, &["dst_endpoint", "port"])
            .and_then(Value::as_u64)
            .filter(|p| *p <= 65535)
            .map(|p| p as u32),
        method: get(&event, &["http_request", "http_method"])
            .and_then(Value::as_str)
            .map(method),
        path: get(&event, &["http_request", "url", "path"])
            .or_else(|| get(&event, &["http_request", "url", "url_string"]))
            .and_then(Value::as_str)
            .and_then(path_only),
        process: text(&event, &["actor", "process", "name"], MAX_PROCESS),
        sandbox: text(&event, &["device", "container", "name"], MAX_SANDBOX),
        sandbox_uid: text(&event, &["device", "container", "uid"], MAX_UID)
            .or_else(|| attr_text(r, "openshell.sandbox.id", MAX_UID)),
        policy_hash: text(&event, &["unmapped", "policy_hash"], MAX_UID)
            .or_else(|| attr_text(r, "openshell.policy.hash", MAX_UID)),
    };
    let denied = matches!(action.as_deref(), Some("denied" | "blocked"));
    let allowed = action.as_deref() == Some("allowed");
    match class {
        4001 | 4002 if denied => {
            Record::Decision(Decision::Deny, Box::new(kept(Some(reason(status_detail)))))
        }
        4001 | 4002 if allowed => Record::Decision(Decision::Allow, Box::new(kept(None))),
        4001 | 4002 => Record::Unparsed { version },
        2004 => Record::Decision(Decision::Finding, Box::new(kept(Some("finding")))),
        6003 if denied && reason(status_detail) == "cross_sandbox" => {
            Record::Decision(Decision::Deny, Box::new(kept(Some("cross_sandbox"))))
        }
        5019 => Record::Decision(Decision::PolicyLoaded, Box::new(kept(None))),
        _ => Record::Ignored,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rec(event: Value) -> LogRecord {
        LogRecord {
            time_unix_nano: 1,
            body: Some(Value::String(event.to_string())),
            ..LogRecord::default()
        }
    }

    #[test]
    fn a_documented_deny_keeps_the_allowlist_only() {
        // The documented example: curl to httpbin.org:443, no matching policy.
        let r = rec(json!({
            "class_uid": 4001, "action": "Denied", "time": 1764000000000_i64,
            "status_detail": "no matching policy", "disposition": "Blocked",
            "dst_endpoint": {"domain": "httpbin.org", "ip": "54.0.0.1", "port": 443},
            "src_endpoint": {"ip": "10.0.0.5", "port": 51234},
            "actor": {"process": {"name": "/usr/bin/curl", "pid": 64, "cmd_line": "curl -H 'Authorization: Bearer SECRET' https://httpbin.org"}},
            "firewall_rule": {"name": "-", "type": "opa"},
            "metadata": {"uid": "u-1", "product": {"version": "0.1.9"}},
            "device": {"container": {"name": "pmos-docs", "uid": "c-1"}},
            "message": "OCSF NET:OPEN [MED] DENIED /usr/bin/curl(64) -> httpbin.org:443",
        }));
        let Record::Decision(Decision::Deny, k) = classify(&r) else {
            panic!("not a deny");
        };
        assert_eq!(k.host.as_deref(), Some("httpbin.org"));
        assert_eq!(k.port, Some(443));
        assert_eq!(k.reason, Some("no_matching_policy"));
        assert_eq!(k.process.as_deref(), Some("/usr/bin/curl"));
        assert_eq!(k.sandbox.as_deref(), Some("pmos-docs"));
        let all = format!("{k:?}");
        for secret in ["SECRET", "cmd_line", "51234", "10.0.0.5", "OCSF NET:OPEN"] {
            assert!(!all.contains(secret), "{secret} reached the kept fields");
        }
    }

    #[test]
    fn strings_are_cleaned_and_capped() {
        assert_eq!(clean("a\nb\u{1b}[31mc", 64).as_deref(), Some("a?b?[31mc"));
        assert_eq!(clean("café", 64).as_deref(), Some("caf?"));
        assert_eq!(
            clean(&"h".repeat(10_000), MAX_HOST).unwrap().len(),
            MAX_HOST
        );
        assert_eq!(clean(" \n ", 8).as_deref(), Some("?"));
        assert_eq!(
            path_only("https://user:pw@api.example/v1/x?token=SECRET#f").as_deref(),
            Some("/v1/x")
        );
        assert_eq!(
            path_only("/v1/worker/claim?x=1").as_deref(),
            Some("/v1/worker/claim")
        );
        assert_eq!(path_only("https://host").as_deref(), Some("/"));
        assert_eq!(method("post"), "POST");
        assert_eq!(method("<script>"), "OTHER");
        assert_eq!(
            reason(Some(
                "resolves to 10.0.0.1 which is not in allowed_ips, connection rejected"
            )),
            "not_in_allowed_ips"
        );
        assert_eq!(
            reason(Some("resolves to always-blocked address")),
            "always_blocked"
        );
        assert_eq!(reason(Some("l7 deny")), "l7_deny");
        assert_eq!(reason(Some("something new")), "other");
    }

    #[test]
    fn other_shapes_are_counted_or_ignored_never_kept_whole() {
        assert!(matches!(
            classify(&LogRecord {
                body: Some(json!("not json")),
                ..LogRecord::default()
            }),
            Record::Unparsed { .. }
        ));
        assert!(matches!(
            classify(&rec(json!({"no_class": 1}))),
            Record::Unparsed { .. }
        ));
        assert!(matches!(
            classify(&rec(json!({"class_uid": 4001}))),
            Record::Unparsed { .. }
        ));
        assert!(matches!(
            classify(&rec(json!({"class_uid": 1007, "action": "Allowed"}))),
            Record::Ignored
        ));
        assert!(matches!(
            classify(&rec(json!({"class_uid": 4002, "action": "Allowed"}))),
            Record::Decision(Decision::Allow, _)
        ));
        assert!(matches!(
            classify(&rec(json!({"class_uid": 2004}))),
            Record::Decision(Decision::Finding, _)
        ));
        assert!(matches!(
            classify(&rec(
                json!({"class_uid": 6003, "action": "Denied", "status_detail": "cross_sandbox"})
            )),
            Record::Decision(Decision::Deny, _)
        ));
        let gap = LogRecord {
            event_name: Some("openshell.ocsf.gap".into()),
            attributes: [
                ("openshell.ocsf.gap.dropped".to_string(), json!(12)),
                (
                    "openshell.ocsf.gap.since_unix_ms".to_string(),
                    json!(1764000000000_i64),
                ),
            ]
            .into(),
            ..LogRecord::default()
        };
        assert!(matches!(
            classify(&gap),
            Record::Gap {
                dropped: 12,
                since: Some(_),
                ..
            }
        ));
    }
}
