//! Converting between the markdown the agent writes and the formats Atlassian stores, and
//! back to plain text for the model.

use pulldown_cmark::{html, Event, Options, Parser};
use serde_json::{json, Value};

/// Markdown to Confluence storage format (XHTML).
///
/// Raw HTML in the markdown is escaped, not passed through, so a draft can't inject
/// Confluence macros or markup the PM didn't see in the approval card.
pub fn markdown_to_storage(markdown: &str) -> String {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    let events = Parser::new_ext(markdown, options).map(|event| match event {
        Event::Html(raw) | Event::InlineHtml(raw) => Event::Text(raw),
        other => other,
    });
    let mut out = String::new();
    html::push_html(&mut out, events);
    out
}

fn text_node(text: &str) -> Value {
    json!({"type": "text", "text": text})
}

fn paragraph(text: &str) -> Value {
    if text.is_empty() {
        json!({"type": "paragraph", "content": []})
    } else {
        json!({"type": "paragraph", "content": [text_node(text)]})
    }
}

/// Markdown to Atlassian Document Format, for Jira descriptions.
///
/// Covers what drafted tickets use: paragraphs, `#` headings, `-`/`*` bullets and `1.`
/// numbered items. Inline markdown is kept as literal text.
pub fn markdown_to_adf(markdown: &str) -> Value {
    let mut content: Vec<Value> = Vec::new();
    let mut para: Vec<&str> = Vec::new();
    let mut list: Option<(&str, Vec<Value>)> = None;

    fn flush_para(para: &mut Vec<&str>, content: &mut Vec<Value>) {
        if !para.is_empty() {
            content.push(paragraph(&para.join(" ")));
            para.clear();
        }
    }
    fn flush_list(list: &mut Option<(&str, Vec<Value>)>, content: &mut Vec<Value>) {
        if let Some((kind, items)) = list.take() {
            content.push(json!({"type": kind, "content": items}));
        }
    }

    for line in markdown.lines() {
        let trimmed = line.trim();
        let bullet = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "));
        let numbered = trimmed
            .split_once(". ")
            .filter(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
            .map(|(_, rest)| rest);
        let heading = (1..=6).rev().find_map(|level| {
            trimmed
                .strip_prefix(&format!("{} ", "#".repeat(level)))
                .map(|t| (level, t))
        });

        if trimmed.is_empty() {
            flush_para(&mut para, &mut content);
            flush_list(&mut list, &mut content);
        } else if let Some((level, text)) = heading {
            flush_para(&mut para, &mut content);
            flush_list(&mut list, &mut content);
            content.push(json!({
                "type": "heading",
                "attrs": {"level": level},
                "content": [text_node(text.trim())],
            }));
        } else if let Some((kind, text)) = bullet
            .map(|t| ("bulletList", t))
            .or(numbered.map(|t| ("orderedList", t)))
        {
            flush_para(&mut para, &mut content);
            if list.as_ref().is_some_and(|(k, _)| *k != kind) {
                flush_list(&mut list, &mut content);
            }
            let item = json!({"type": "listItem", "content": [paragraph(text.trim())]});
            list.get_or_insert_with(|| (kind, Vec::new())).1.push(item);
        } else {
            flush_list(&mut list, &mut content);
            para.push(trimmed);
        }
    }
    flush_para(&mut para, &mut content);
    flush_list(&mut list, &mut content);
    json!({"type": "doc", "version": 1, "content": content})
}

/// Plain text of an ADF document (Jira descriptions and comments).
pub fn adf_to_text(doc: &Value) -> String {
    fn walk(node: &Value, out: &mut String) {
        if let Some(text) = node["text"].as_str() {
            out.push_str(text);
        }
        if node["type"] == "hardBreak" {
            out.push('\n');
        }
        if let Some(children) = node["content"].as_array() {
            for child in children {
                walk(child, out);
            }
        }
        let block = matches!(
            node["type"].as_str(),
            Some("paragraph" | "heading" | "listItem" | "codeBlock" | "blockquote")
        );
        if block && !out.ends_with('\n') {
            out.push('\n');
        }
    }
    let mut out = String::new();
    walk(doc, &mut out);
    out.trim().to_string()
}

/// Plain text of Confluence storage format: tags dropped, block ends become line breaks,
/// common entities decoded.
pub fn storage_to_text(storage: &str) -> String {
    let mut out = String::with_capacity(storage.len());
    let mut rest = storage;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        let Some(end) = rest[start..].find('>') else {
            rest = "";
            break;
        };
        let tag = &rest[start + 1..start + end];
        let name = tag
            .trim_start_matches('/')
            .split(|c: char| c.is_whitespace() || c == '/')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if matches!(
            name.as_str(),
            "p" | "br" | "li" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "tr" | "div" | "pre"
        ) && !out.ends_with('\n')
        {
            out.push('\n');
        }
        rest = &rest[start + end + 1..];
    }
    out.push_str(rest);
    let decoded = out
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&");
    decoded
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_from_markdown_escapes_raw_html() {
        let out = markdown_to_storage(
            "# PRD\n\nHello **world**.\n\n- one\n- two\n\n<ac:structured-macro ac:name=\"html\"/>",
        );
        assert!(out.contains("<h1>PRD</h1>"));
        assert!(out.contains("<strong>world</strong>"));
        assert!(out.contains("<li>one</li>"));
        assert!(!out.contains("<ac:structured-macro"));
        assert!(out.contains("&lt;ac:structured-macro"));
    }

    #[test]
    fn adf_from_markdown() {
        let doc = markdown_to_adf(
            "# Goal\n\nShip SSO\nfor Acme Vault.\n\n- SAML\n- OIDC\n\n1. Design\n2. Build",
        );
        assert_eq!(doc["type"], "doc");
        assert_eq!(doc["version"], 1);
        let c = doc["content"].as_array().unwrap();
        assert_eq!(c[0]["type"], "heading");
        assert_eq!(c[0]["attrs"]["level"], 1);
        assert_eq!(c[1]["content"][0]["text"], "Ship SSO for Acme Vault.");
        assert_eq!(c[2]["type"], "bulletList");
        assert_eq!(c[2]["content"].as_array().unwrap().len(), 2);
        assert_eq!(c[3]["type"], "orderedList");
        assert_eq!(
            adf_to_text(&doc),
            "Goal\nShip SSO for Acme Vault.\nSAML\nOIDC\nDesign\nBuild"
        );
    }

    #[test]
    fn text_from_storage() {
        let s = "<h1>Title</h1><p>Para &amp; more&nbsp;text</p><ul><li>a</li><li>b</li></ul><p>x<br/>y</p>";
        assert_eq!(storage_to_text(s), "Title\nPara & more text\na\nb\nx\ny");
        assert_eq!(storage_to_text("no tags"), "no tags");
        assert_eq!(storage_to_text("broken <p"), "broken");
    }
}
