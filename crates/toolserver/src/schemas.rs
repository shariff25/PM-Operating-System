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

/// A calendar date, such as `2026-11-15`.
pub const DATE_PATTERN: &str = r"^\d{4}-\d{2}-\d{2}$";

/// Is `s` a real calendar date written as [`DATE_PATTERN`]? Checked by hand, and then as a
/// date (so `2026-02-30` is refused).
pub fn is_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
        && chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok()
}

/// A bet's key: 1 to 60 lowercase letters, digits and hyphens.
pub const BET_KEY_PATTERN: &str = r"^[a-z0-9-]{1,60}$";

/// Does `s` match [`BET_KEY_PATTERN`]?
pub fn is_bet_key(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 60
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// One dated entry in a bet's evidence or decisions.
fn dated_entry(what: &str) -> Value {
    json!({
        "type": "object",
        "description": format!("{what}: the date it happened, one or two sentences, and where it came from"),
        "properties": {
            "date": {"type": "string", "pattern": DATE_PATTERN, "description": "YYYY-MM-DD"},
            "text": {"type": "string", "maxLength": crate::bets::MAX_FIELD_CHARS},
            "source": {"type": "string", "maxLength": crate::bets::MAX_FIELD_CHARS, "description": "A link or a Jira or Confluence id"},
        },
        "required": ["date", "text"],
        "additionalProperties": false,
    })
}

/// The most characters of a Slack message's text.
pub const MAX_SLACK_TEXT: u64 = 40_000;
/// The most characters of a Confluence page body (markdown) and title.
pub const MAX_CONFLUENCE_BODY: u64 = 200_000;
pub const MAX_CONFLUENCE_TITLE: u64 = 255;
/// The most characters of a Confluence version comment.
pub const MAX_VERSION_COMMENT: u64 = 500;
/// The most characters of a Jira summary and description.
pub const MAX_JIRA_SUMMARY: u64 = 255;
pub const MAX_JIRA_DESCRIPTION: u64 = 32_000;

/// Arguments that say where a write goes, by tool: a decision of `edit` from the Slack bot
/// must leave each one as drafted, or it is refused (`422 target_changed`). The Slack bot
/// names the deciding PM without anything signed, so its edits may change what a draft
/// says, never where it goes. The orchestrator's edits are not limited.
///
/// The names are the schemas' own (below); a unit test checks each one is in its tool's
/// schema. An optional one missing from both the draft and the edit is unchanged. Keep
/// this in step with the Slack edit window (`editable_fields` in `crates/slack/src/cards.rs`),
/// which must not offer these fields.
pub const EDIT_IMMUTABLE_ARGS: &[(&str, &[&str])] = &[
    ("slack.post_reply", &["channel", "thread_ts", "message_ts"]),
    ("slack.send_message", &["channel"]),
    ("confluence.create_draft", &["space", "parent_id"]),
    ("confluence.update_page", &["page_id", "version"]),
    ("jira.create_issue", &["project", "issue_type", "epic"]),
    ("bets.record", &["key"]),
];

/// The arguments of `tool` that an edit from the Slack bot can't change.
pub fn edit_immutable_args(tool: &str) -> &'static [&'static str] {
    EDIT_IMMUTABLE_ARGS
        .iter()
        .find(|(t, _)| *t == tool)
        .map_or(&[], |(_, args)| args)
}

/// The first of `tool`'s [`edit_immutable_args`] whose value differs between the drafted
/// and the edited arguments, if any. Absent in both counts as equal.
pub fn changed_target(tool: &str, drafted: &Value, edited: &Value) -> Option<&'static str> {
    edit_immutable_args(tool)
        .iter()
        .copied()
        .find(|arg| drafted.get(arg) != edited.get(arg))
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
        "jira.rollup" => object(
            json!({
                "jql": {"type": "string", "description": "JQL query for the issues to count, such as project in (PM, CLD) AND sprint in openSprints()", "maxLength": 2000},
                "group_by": {"type": "string", "enum": crate::connectors::rollup::GROUP_BY, "description": "How to group the counts (default project). With fix_version an issue counts in each of its versions"},
                "max_issues": {"type": "integer", "minimum": 1, "maximum": crate::connectors::rollup::MAX_ISSUES, "description": "Most issues to read (default 500)"},
            }),
            &["jql"],
        ),
        "jira.create_issue" => object(
            json!({
                "project": {"type": "string"},
                "issue_type": {"type": "string", "enum": ["Epic", "Story", "Task", "Bug"]},
                "summary": {"type": "string", "maxLength": MAX_JIRA_SUMMARY},
                "description": {"type": "string", "maxLength": MAX_JIRA_DESCRIPTION},
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
                "title": {"type": "string", "maxLength": MAX_CONFLUENCE_TITLE},
                "body": {"type": "string", "description": "Page body in markdown", "maxLength": MAX_CONFLUENCE_BODY},
                "parent_id": {"type": "string"},
            }),
            &["space", "title", "body"],
        ),
        "confluence.update_page" => object(
            json!({
                "page_id": {"type": "string"},
                "version": {"type": "integer", "minimum": 1, "description": "The page's version number as confluence_read returned it: the update is refused if the page changed since"},
                "body": {"type": "string", "description": "New page body in markdown", "maxLength": MAX_CONFLUENCE_BODY},
                "version_comment": {"type": "string", "maxLength": MAX_VERSION_COMMENT},
            }),
            &["page_id", "version", "body"],
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
                "text": {"type": "string", "maxLength": MAX_SLACK_TEXT},
            }),
            &["channel", "thread_ts", "text"],
        ),
        "slack.send_message" => object(
            json!({"channel": {"type": "string", "description": "Channel or user id"}, "text": {"type": "string", "maxLength": MAX_SLACK_TEXT}}),
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
        "stats.cashflow" => object(
            json!({
                "scenarios": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": crate::stats::MAX_SCENARIOS,
                    "description": "1 to 5 scenarios, such as low, base and high",
                    "items": {
                        "type": "object",
                        "properties": {
                            "name": {"type": "string", "maxLength": 100},
                            "upfront": {"type": "number", "minimum": 0, "description": "Spent before year 1 (year 0)"},
                            "years": {
                                "type": "array",
                                "minItems": 1,
                                "maxItems": crate::stats::MAX_YEARS,
                                "description": "Year 1, year 2, …: 1 to 10 years",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "revenue": {"type": "number", "minimum": 0},
                                        "cost": {"type": "number", "minimum": 0},
                                    },
                                    "required": ["revenue", "cost"],
                                    "additionalProperties": false,
                                },
                            },
                        },
                        "required": ["name", "upfront", "years"],
                        "additionalProperties": false,
                    },
                },
                "discount_rate": {"type": "number", "minimum": 0, "maximum": 1, "description": "Yearly discount rate for the net present value (default 0.1)"},
            }),
            &["scenarios"],
        ),
        "bets.list" => object(
            json!({"status": {"type": "string", "enum": crate::bets::STATUSES, "description": "Only bets with this status"}}),
            &[],
        ),
        "bets.read" => object(
            json!({"key": {"type": "string", "pattern": BET_KEY_PATTERN, "description": "The bet's key, from bets_list"}}),
            &["key"],
        ),
        "bets.record" => {
            let field = |d: &str| json!({"type": "string", "maxLength": crate::bets::MAX_FIELD_CHARS, "description": d});
            object(
                json!({
                    "key": {"type": "string", "pattern": BET_KEY_PATTERN, "description": "Short slug for the bet, such as sso-enterprise-tier: 1 to 60 lowercase letters, digits and hyphens"},
                    "title": field("A short name for the bet"),
                    "status": {"type": "string", "enum": crate::bets::STATUSES, "description": "Change only when the metric or the test's result supports it, with that evidence in the same call"},
                    "bet": field("The bet in one sentence: what we believe and what we are doing about it"),
                    "metric": field("The number that shows whether the bet is paying off"),
                    "riskiest_assumption": field("The assumption that, if wrong, sinks the bet"),
                    "test": field("How and when the riskiest assumption is tested"),
                    "review_by": {"type": "string", "pattern": DATE_PATTERN, "description": "YYYY-MM-DD: when to review the bet next"},
                    "evidence": {"type": "array", "maxItems": crate::bets::MAX_EVIDENCE_PER_CALL, "items": dated_entry("Evidence"), "description": "New evidence, appended to what the bet has"},
                    "decision": dated_entry("A decision taken on the bet, appended"),
                }),
                &["key"],
            )
        }
        _ => return None,
    };
    Some(schema)
}

/// Check the arguments against a schema built by this module: required keys present, no
/// unknown keys, string, integer, number, boolean, object and array types right, `enum`,
/// `maxLength`, `minimum`/`maximum`, `minItems`/`maxItems`, and a string's `pattern` when it
/// is one of the patterns defined here ([`SLACK_TS_PATTERN`], [`DATE_PATTERN`],
/// [`BET_KEY_PATTERN`]), checked by hand. Nested objects and array items are checked the
/// same way, and errors name the path, such as `scenarios[0].years[1].cost`. Full JSON
/// Schema validation is not needed for these schemas.
pub fn check_args(schema: &Value, args: &Value) -> Result<(), String> {
    if !args.is_object() {
        return Err("arguments must be an object".to_string());
    }
    check_object(schema, args, "")
}

fn check_object(schema: &Value, value: &Value, path: &str) -> Result<(), String> {
    let obj = value.as_object().ok_or_else(|| match path {
        "" => "arguments must be an object".to_string(),
        p => format!("argument `{p}` must be an object"),
    })?;
    let join = |key: &str| match path {
        "" => key.to_string(),
        p => format!("{p}.{key}"),
    };
    let props = schema["properties"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    for key in schema["required"].as_array().into_iter().flatten() {
        let key = key.as_str().unwrap_or_default();
        if !obj.contains_key(key) {
            return Err(format!("missing required argument `{}`", join(key)));
        }
    }
    for (key, value) in obj {
        let Some(prop) = props.get(key) else {
            return Err(format!("unknown argument `{}`", join(key)));
        };
        check_value(prop, value, &join(key))?;
    }
    Ok(())
}

fn check_value(prop: &Value, value: &Value, key: &str) -> Result<(), String> {
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
            match prop["pattern"].as_str() {
                Some(SLACK_TS_PATTERN) if !is_slack_ts(s) => {
                    return Err(format!(
                        "argument `{key}` must be a Slack message timestamp, such as 1731600300.000200"
                    ));
                }
                Some(DATE_PATTERN) if !is_date(s) => {
                    return Err(format!(
                        "argument `{key}` must be a date written YYYY-MM-DD, such as 2026-11-15"
                    ));
                }
                Some(BET_KEY_PATTERN) if !is_bet_key(s) => {
                    return Err(format!(
                        "argument `{key}` must be 1 to 60 lowercase letters, digits or hyphens"
                    ));
                }
                _ => {}
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
        Some("number") => {
            let n = value
                .as_f64()
                .filter(|n| n.is_finite())
                .ok_or_else(|| format!("argument `{key}` must be a number"))?;
            match (prop["minimum"].as_f64(), prop["maximum"].as_f64()) {
                (Some(min), Some(max)) if n < min || n > max => {
                    return Err(format!("argument `{key}` must be between {min} and {max}"));
                }
                (Some(min), None) if n < min => {
                    return Err(format!("argument `{key}` must be {min} or more"));
                }
                (None, Some(max)) if n > max => {
                    return Err(format!("argument `{key}` must be {max} or less"));
                }
                _ => {}
            }
        }
        Some("boolean") if !value.is_boolean() => {
            return Err(format!("argument `{key}` must be true or false"));
        }
        Some("object") => {
            if !value.is_object() {
                return Err(format!("argument `{key}` must be an object"));
            }
            if prop.get("properties").is_some() {
                check_object(prop, value, key)?;
            }
        }
        Some("array") => {
            let items = value
                .as_array()
                .ok_or_else(|| format!("argument `{key}` must be a list"))?;
            let min = prop["minItems"].as_u64().unwrap_or(0);
            let max = prop["maxItems"].as_u64().unwrap_or(u64::MAX);
            let n = items.len() as u64;
            if n < min || n > max {
                return Err(match (min, max) {
                    (0, max) => format!("argument `{key}` has at most {max} entries (got {n})"),
                    (min, u64::MAX) => {
                        format!("argument `{key}` needs at least {min} entries (got {n})")
                    }
                    (min, max) => {
                        format!("argument `{key}` must have {min} to {max} entries (got {n})")
                    }
                });
            }
            if let Some(item) = prop.get("items") {
                for (i, v) in items.iter().enumerate() {
                    check_value(item, v, &format!("{key}[{i}]"))?;
                }
            }
        }
        _ => {}
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
    fn immutable_targets_are_real_schema_arguments() {
        for (tool, args) in EDIT_IMMUTABLE_ARGS {
            let schema = input_schema(tool).unwrap_or_else(|| panic!("{tool} has a schema"));
            for arg in *args {
                assert!(
                    schema["properties"].get(arg).is_some(),
                    "{tool} has no argument `{arg}`"
                );
            }
        }
        let drafted = json!({"channel": "C1", "thread_ts": "1.1", "text": "a"});
        let text_only = json!({"channel": "C1", "thread_ts": "1.1", "text": "b"});
        assert_eq!(
            changed_target("slack.post_reply", &drafted, &text_only),
            None
        );
        let moved = json!({"channel": "C2", "thread_ts": "1.1", "text": "a"});
        assert_eq!(
            changed_target("slack.post_reply", &drafted, &moved),
            Some("channel")
        );
        let tagged = json!({"channel": "C1", "thread_ts": "1.1", "message_ts": "1.2", "text": "a"});
        assert_eq!(
            changed_target("slack.post_reply", &drafted, &tagged),
            Some("message_ts"),
            "adding an optional target changes it"
        );
        assert_eq!(changed_target("stats.compare", &drafted, &moved), None);
    }

    #[test]
    fn write_text_is_bounded() {
        let reply = input_schema("slack.post_reply").unwrap();
        let ok = json!({"channel": "C1", "thread_ts": "1.1", "text": "x".repeat(40_000)});
        assert!(check_args(&reply, &ok).is_ok());
        let long = json!({"channel": "C1", "thread_ts": "1.1", "text": "x".repeat(40_001)});
        assert!(check_args(&reply, &long).unwrap_err().contains("40000"));
        let page = input_schema("confluence.create_draft").unwrap();
        let title = json!({"space": "PM", "title": "t".repeat(256), "body": "b"});
        assert!(check_args(&page, &title).is_err());
        let issue = input_schema("jira.create_issue").unwrap();
        let summary = json!({"project": "PM", "issue_type": "Task", "summary": "s".repeat(256)});
        assert!(check_args(&issue, &summary).is_err());
        let update = input_schema("confluence.update_page").unwrap();
        let comment =
            json!({"page_id": "1", "version": 2, "body": "b", "version_comment": "c".repeat(501)});
        assert!(check_args(&update, &comment).is_err());
        assert!(
            check_args(&update, &json!({"page_id": "1", "body": "b"}))
                .unwrap_err()
                .contains("version"),
            "version is required"
        );
        assert!(check_args(&update, &json!({"page_id": "1", "version": 0, "body": "b"})).is_err());
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
