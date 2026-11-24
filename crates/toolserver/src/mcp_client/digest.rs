//! A tool's definition digest (`docs/mcp-servers-spec.md` §2.4): SHA-256 of the canonical JSON
//! (object keys sorted at every level) of the definition's `name`, `title`, `description`,
//! `inputSchema`, `outputSchema` and `annotations`. The spec lists five fields; `title` is
//! added because a server can change it too. `icons` and `_meta` are left out: PM OS never
//! uses them.

use rmcp::model::Tool;
use serde_json::{json, Value};

/// The parts of a definition the digest covers.
pub fn definition(tool: &Tool) -> Value {
    json!({
        "name": tool.name,
        "title": tool.title,
        "description": tool.description,
        "inputSchema": tool.input_schema.as_ref(),
        "outputSchema": tool.output_schema.as_deref(),
        "annotations": tool.annotations,
    })
}

/// `sha256:<hex>`.
pub fn digest(tool: &Tool) -> String {
    format!("sha256:{}", crate::approvals::args_hash(&definition(tool)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn tool(description: &str, schema: Value) -> Tool {
        Tool::new(
            "search",
            description.to_string(),
            Arc::new(schema.as_object().cloned().unwrap()),
        )
    }

    #[test]
    fn any_change_to_the_definition_changes_the_digest() {
        let schema = json!({"type": "object", "properties": {"q": {"type": "string"}}});
        let base = digest(&tool("Search issues", schema.clone()));
        assert!(pmos_contract::mcp::valid_digest(&base));
        // Key order doesn't matter; content does.
        let reordered = json!({"properties": {"q": {"type": "string"}}, "type": "object"});
        assert_eq!(digest(&tool("Search issues", reordered)), base);
        assert_ne!(digest(&tool("Search issues.", schema.clone())), base);
        let wider = json!({"type": "object", "properties": {"q": {"type": "string"}, "all": {"type": "boolean"}}});
        assert_ne!(digest(&tool("Search issues", wider)), base);
        let mut renamed = tool("Search issues", schema.clone());
        renamed.name = "search2".into();
        assert_ne!(digest(&renamed), base);
        let hinted = tool("Search issues", schema)
            .annotate(rmcp::model::ToolAnnotations::with_title("x").read_only(true));
        assert_ne!(digest(&hinted), base);
    }
}
