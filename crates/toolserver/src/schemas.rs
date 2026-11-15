//! JSON Schemas for each tool's input. Every tool in `content/tools.yaml` needs one, or the
//! server refuses to start: a registry entry the server cannot serve is a bug.

use pmos_contract::model::CONTEXT_DOMAINS;
use serde_json::{json, Value};

fn object(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

/// A Slack message timestamp, such as `1731600300.000200`.
pub const SLACK_TS_PATTERN: &str = r"^\d{1,12}\.\d{1,9}$";

/// Does `s` match [`SLACK_TS_PATTERN`]? Checked by hand: no regex engine here.
fn is_slack_ts(s: &str) -> bool {
    let digits = |p: &str, max: usize| {
        !p.is_empty() && p.len() <= max && p.bytes().all(|b| b.is_ascii_digit())
    };
    s.split_once('.')
        .is_some_and(|(a, b)| digits(a, 12) && digits(b, 9))
}

fn limit() -> Value {
    json!({"type": "integer", "minimum": 1, "maximum": 20, "description": "Maximum results (default 5)"})
}

pub fn input_schema(tool: &str) -> Option<Value> {
    let schema = match tool {
        "context.search" => object(
            json!({
                "query": {"type": "string", "description": "What to look for", "maxLength": 500},
                "domain": {"type": "string", "enum": CONTEXT_DOMAINS, "description": "Limit to one context domain"},
                "limit": limit(),
            }),
            &["query"],
        ),
        "context.read" => object(
            json!({"path": {"type": "string", "description": "Path from a search result, such as context/products/example-product.md"}}),
            &["path"],
        ),
        "memory.read" => object(json!({}), &[]),
        "memory.update" => object(
            json!({
                "key": {"type": "string", "description": "Short slug: lowercase letters, digits and hyphens"},
                "text": {"type": "string", "maxLength": 4000},
            }),
            &["key", "text"],
        ),
        "jira.search" => object(
            json!({"jql": {"type": "string", "description": "JQL query", "maxLength": 2000}, "limit": limit()}),
            &["jql"],
        ),
        "jira.read" => object(
            json!({"key": {"type": "string", "description": "Issue key, such as PM-101"}}),
            &["key"],
        ),
        "jira.create_issue" => object(
            json!({
                "project": {"type": "string"},
                "issue_type": {"type": "string", "enum": ["Epic", "Story", "Task", "Bug"]},
                "summary": {"type": "string"},
                "description": {"type": "string"},
                "epic": {"type": "string", "description": "Parent epic key"},
            }),
            &["project", "issue_type", "summary"],
        ),
        "confluence.search" => object(
            json!({"cql": {"type": "string", "description": "CQL query", "maxLength": 2000}, "limit": limit()}),
            &["cql"],
        ),
        "confluence.read" => object(json!({"page_id": {"type": "string"}}), &["page_id"]),
        "confluence.create_draft" => object(
            json!({
                "space": {"type": "string", "description": "Space key"},
                "title": {"type": "string"},
                "body": {"type": "string", "description": "Page body in markdown"},
                "parent_id": {"type": "string"},
            }),
            &["space", "title", "body"],
        ),
        "confluence.update_page" => object(
            json!({
                "page_id": {"type": "string"},
                "body": {"type": "string", "description": "New page body in markdown"},
                "version_comment": {"type": "string"},
            }),
            &["page_id", "body"],
        ),
        "slack.search" => object(
            json!({"query": {"type": "string", "maxLength": 500}, "limit": limit()}),
            &["query"],
        ),
        "slack.read_thread" => object(
            json!({"channel": {"type": "string"}, "ts": {"type": "string", "description": "Thread timestamp"}}),
            &["channel", "ts"],
        ),
        "slack.post_reply" => object(
            json!({
                "channel": {"type": "string"},
                "thread_ts": {"type": "string"},
                "message_ts": {"type": "string", "pattern": SLACK_TS_PATTERN, "description": "The message the reply answers, when PM OS was tagged in it: one draft per message instead of per thread"},
                "text": {"type": "string"},
            }),
            &["channel", "thread_ts", "text"],
        ),
        "slack.send_message" => object(
            json!({"channel": {"type": "string", "description": "Channel or user id"}, "text": {"type": "string"}}),
            &["channel", "text"],
        ),
        "docs.search" => object(
            json!({
                "query": {"type": "string", "description": "What to look for in your company's public docs", "maxLength": 500},
                "product": {"type": "string", "maxLength": 64, "description": "A product id from the docs site's products list, or `general`"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 10, "description": "Maximum results (default 5)"},
            }),
            &["query"],
        ),
        "docs.read" => object(
            json!({"url": {"type": "string", "description": "A docs page link from a search result"}}),
            &["url"],
        ),
        "stats.sample_size" => object(
            json!({
                "metric": {"type": "string", "enum": ["proportion", "mean"], "description": "`proportion` for a rate such as conversion, `mean` for an average such as revenue per user"},
                "baseline": {"type": "number", "description": "The control's current value: a rate between 0 and 1 (exclusive) for a proportion, or the mean"},
                "std_dev": {"type": "number", "description": "The metric's standard deviation; required for `mean`, more than 0"},
                "mde": {"type": "number", "description": "The minimum detectable effect, more than 0: 0.05 is 5% of the baseline when `mde_kind` is relative"},
                "mde_kind": {"type": "string", "enum": ["relative", "absolute"], "description": "How to read `mde` (default relative)"},
                "alpha": {"type": "number", "description": "Significance level (default 0.05)"},
                "power": {"type": "number", "description": "Statistical power (default 0.8)"},
                "two_sided": {"type": "boolean", "description": "Two-sided test (default true)"},
                "variants": {"type": "integer", "minimum": 2, "maximum": crate::stats::MAX_VARIANTS, "description": "Arms including control (default 2)"},
                "daily_users": {"type": "integer", "minimum": 1, "description": "Users entering the experiment per day, all arms together; adds `days`"},
            }),
            &["metric", "baseline", "mde"],
        ),
        "stats.compare" => {
            let arm = |which: &str| {
                json!({
                    "type": "object",
                    "description": format!("The {which} arm: users exposed and how many converted"),
                    "properties": {
                        "users": {"type": "integer", "minimum": 1},
                        "conversions": {"type": "integer", "minimum": 0},
                    },
                    "required": ["users", "conversions"],
                    "additionalProperties": false,
                })
            };
            object(
                json!({
                    "control": arm("control"),
                    "treatment": arm("treatment"),
                    "alpha": {"type": "number", "description": "Significance level (default 0.05)"},
                }),
                &["control", "treatment"],
            )
        }
        _ => return None,
    };
    Some(schema)
}

/// Check the arguments' top level against a schema built by this module: required keys
/// present, no unknown keys, and string, integer and enum types right, and a string's
/// `pattern` when it is [`SLACK_TS_PATTERN`] (the only one used). Full JSON Schema
/// validation is not needed for these flat objects.
pub fn check_args(schema: &Value, args: &Value) -> Result<(), String> {
    let obj = args
        .as_object()
        .ok_or_else(|| "arguments must be an object".to_string())?;
    let props = schema["properties"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    for key in schema["required"].as_array().into_iter().flatten() {
        let key = key.as_str().unwrap_or_default();
        if !obj.contains_key(key) {
            return Err(format!("missing required argument `{key}`"));
        }
    }
    for (key, value) in obj {
        let Some(prop) = props.get(key) else {
            return Err(format!("unknown argument `{key}`"));
        };
        match prop["type"].as_str() {
            Some("string") => {
                let s = value
                    .as_str()
                    .ok_or_else(|| format!("argument `{key}` must be a string"))?;
                if let Some(max) = prop["maxLength"].as_u64() {
                    if s.chars().count() as u64 > max {
                        return Err(format!("argument `{key}` is longer than {max} characters"));
                    }
                }
                if prop["pattern"].as_str() == Some(SLACK_TS_PATTERN) && !is_slack_ts(s) {
                    return Err(format!(
                        "argument `{key}` must be a Slack message timestamp, such as 1731600300.000200"
                    ));
                }
                if let Some(options) = prop["enum"].as_array() {
                    if !options.iter().any(|o| o == value) {
                        return Err(format!(
                            "argument `{key}` must be one of {}",
                            Value::Array(options.clone())
                        ));
                    }
                }
            }
            Some("integer") => {
                let n = value
                    .as_i64()
                    .ok_or_else(|| format!("argument `{key}` must be an integer"))?;
                let min = prop["minimum"].as_i64().unwrap_or(i64::MIN);
                let max = prop["maximum"].as_i64().unwrap_or(i64::MAX);
                if n < min || n > max {
                    return Err(format!("argument `{key}` must be between {min} and {max}"));
                }
            }
            Some("number") if !value.is_number() => {
                return Err(format!("argument `{key}` must be a number"));
            }
            Some("boolean") if !value.is_boolean() => {
                return Err(format!("argument `{key}` must be true or false"));
            }
            Some("object") if !value.is_object() => {
                return Err(format!("argument `{key}` must be an object"));
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_required_unknown_and_types() {
        let s = input_schema("jira.create_issue").unwrap();
        let ok = json!({"project": "PM", "issue_type": "Story", "summary": "x"});
        assert!(check_args(&s, &ok).is_ok());
        assert!(
            check_args(&s, &json!({"project": "PM", "issue_type": "Story"}))
                .unwrap_err()
                .contains("summary")
        );
        assert!(check_args(
            &s,
            &json!({"project": "PM", "issue_type": "Saga", "summary": "x"})
        )
        .unwrap_err()
        .contains("one of"));
        assert!(check_args(
            &s,
            &json!({"project": "PM", "issue_type": "Story", "summary": "x", "assignee": "bo"})
        )
        .unwrap_err()
        .contains("unknown"));
        let search = input_schema("context.search").unwrap();
        assert!(check_args(&search, &json!({"query": "x", "limit": 50})).is_err());
        // Search input is bounded, so a huge query can't become a huge database query.
        assert!(check_args(&search, &json!({"query": "x".repeat(501)})).is_err());
        assert!(check_args(&search, &json!("x")).is_err());
    }

    #[test]
    fn checks_numbers_booleans_and_objects() {
        let s = input_schema("stats.sample_size").unwrap();
        let ok = json!({"metric": "proportion", "baseline": 0.1, "mde": 0.1, "two_sided": false});
        assert!(check_args(&s, &ok).is_ok());
        let bad = json!({"metric": "proportion", "baseline": "0.1", "mde": 0.1});
        assert!(check_args(&s, &bad).unwrap_err().contains("number"));
        let bad = json!({"metric": "proportion", "baseline": 0.1, "mde": 0.1, "two_sided": "yes"});
        assert!(check_args(&s, &bad).unwrap_err().contains("true or false"));
        let bad = json!({"metric": "proportion", "baseline": 0.1, "mde": 0.1, "variants": 11});
        assert!(check_args(&s, &bad).is_err());
        let c = input_schema("stats.compare").unwrap();
        assert!(check_args(&c, &json!({"control": 1, "treatment": {}})).is_err());
    }

    #[test]
    fn a_reply_may_name_the_message_it_answers() {
        let s = input_schema("slack.post_reply").unwrap();
        let base = json!({"channel": "C0PMTEAM", "thread_ts": "1731600000.000100", "text": "Yes."});
        assert!(check_args(&s, &base).is_ok(), "message_ts is optional");
        let mut tagged = base.clone();
        tagged["message_ts"] = json!("1731600300.000200");
        assert!(check_args(&s, &tagged).is_ok());
        for bad in [
            "",
            "1731600300",
            "1731600300.",
            ".000200",
            "17316x0300.000200",
            "1234567890123.1",
            "1.1234567890",
        ] {
            tagged["message_ts"] = json!(bad);
            assert!(
                check_args(&s, &tagged).unwrap_err().contains("timestamp"),
                "{bad:?}"
            );
        }
        tagged["message_ts"] = json!(1731600300);
        assert!(check_args(&s, &tagged).unwrap_err().contains("string"));
    }
}
