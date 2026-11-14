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

fn limit() -> Value {
    json!({"type": "integer", "minimum": 1, "maximum": 20, "description": "Maximum results (default 5)"})
}

pub fn input_schema(tool: &str) -> Option<Value> {
    let schema = match tool {
        "context.search" => object(
            json!({
                "query": {"type": "string", "description": "What to look for"},
                "domain": {"type": "string", "enum": CONTEXT_DOMAINS, "description": "Limit to one context domain"},
                "limit": limit(),
            }),
            &["query"],
        ),
        "context.read" => object(
            json!({"path": {"type": "string", "description": "Path from a search result, such as context/products/ccm.md"}}),
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
            json!({"jql": {"type": "string", "description": "JQL query"}, "limit": limit()}),
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
            json!({"cql": {"type": "string", "description": "CQL query"}, "limit": limit()}),
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
            json!({"query": {"type": "string"}, "limit": limit()}),
            &["query"],
        ),
        "slack.read_thread" => object(
            json!({"channel": {"type": "string"}, "ts": {"type": "string", "description": "Thread timestamp"}}),
            &["channel", "ts"],
        ),
        "slack.post_reply" => object(
            json!({"channel": {"type": "string"}, "thread_ts": {"type": "string"}, "text": {"type": "string"}}),
            &["channel", "thread_ts", "text"],
        ),
        "slack.send_message" => object(
            json!({"channel": {"type": "string", "description": "Channel or user id"}, "text": {"type": "string"}}),
            &["channel", "text"],
        ),
        "docs.search" => object(
            json!({
                "query": {"type": "string", "description": "What to look for in the support docs"},
                "product": {"type": "string", "enum": ["dsm", "ccm", "confidential-ai", "key-insight", "armor", "general"]},
                "limit": {"type": "integer", "minimum": 1, "maximum": 10, "description": "Maximum results (default 5)"},
            }),
            &["query"],
        ),
        "docs.read" => object(
            json!({"url": {"type": "string", "description": "A support.fortanix.com/docs link from a search result"}}),
            &["url"],
        ),
        _ => return None,
    };
    Some(schema)
}

/// Check the arguments' top level against a schema built by this module: required keys
/// present, no unknown keys, and string, integer and enum types right. Full JSON Schema
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
        assert!(check_args(&search, &json!("x")).is_err());
    }
}
