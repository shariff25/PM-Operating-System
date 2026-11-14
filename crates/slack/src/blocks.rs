//! Block Kit messages for `/docs`.

use serde_json::{json, Value};

/// Escape text for Slack `mrkdwn`: `&`, `<` and `>` are control characters there.
pub fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// A link a Slack message can show: only `https://` URLs, with `|` and `>` removed so they
/// can't break out of the `<url|label>` syntax.
fn link(url: &str, label: &str) -> String {
    let safe_url: String = url.chars().filter(|c| !"|<> ".contains(*c)).collect();
    let label = escape(label).replace('|', "¦");
    if safe_url.starts_with("https://") {
        format!("<{safe_url}|{label}>")
    } else {
        label
    }
}

fn truncate(text: &str, max: usize) -> String {
    let mut out: String = text.chars().take(max).collect();
    if text.chars().count() > max {
        out.push('…');
    }
    out
}

/// A plain private reply.
pub fn text(message: &str) -> Value {
    json!({ "response_type": "ephemeral", "text": message })
}

pub const USAGE: &str = "Search the Fortanix support docs: `/docs <what you're looking for>`.\n\
     Narrow to a product with `product:dsm` (or `ccm`, `confidential-ai`, `key-insight`, \
     `armor`). Example: `/docs product:dsm rotate a key`";

pub fn searching(query: &str) -> Value {
    text(&format!(
        "Searching Fortanix support docs for “{}”…",
        escape(query)
    ))
}

/// The results message, built from the tool server's `docs_search` result.
pub fn results(query: &str, result: &Value) -> Value {
    let hits: Vec<&Value> = result["results"].as_array().into_iter().flatten().collect();
    let heading = format!(
        "*Fortanix support docs* · “{}”",
        escape(&truncate(query, 80))
    );
    if hits.is_empty() {
        return json!({
            "response_type": "ephemeral",
            "replace_original": true,
            "text": format!("No support pages match “{}”.", escape(query)),
            "blocks": [
                {"type": "section", "text": {"type": "mrkdwn", "text": heading}},
                {"type": "section", "text": {"type": "mrkdwn", "text": "No pages match. Try different or fewer words."}},
            ],
        });
    }
    let mut blocks = vec![json!({"type": "section", "text": {"type": "mrkdwn", "text": heading}})];
    for hit in hits.iter().take(5) {
        // Section text is capped at 3,000 characters by Slack; keep well inside it.
        let title = truncate(hit["title"].as_str().unwrap_or("Untitled"), 150);
        let url = hit["source"]["url"].as_str().unwrap_or_default();
        let snippet = hit["content"]["snippet"].as_str().unwrap_or_default();
        let mut body = format!("*{}*", link(url, &title));
        if !snippet.is_empty() {
            body.push('\n');
            body.push_str(&escape(&truncate(snippet, 200)));
        }
        blocks.push(json!({"type": "section", "text": {"type": "mrkdwn", "text": body}}));
        let mut meta: Vec<String> = Vec::new();
        if let Some(p) = hit["content"]["product"]
            .as_str()
            .filter(|p| *p != "general")
        {
            meta.push(p.to_uppercase().replace('-', " "));
        }
        if hit["content"]["section"] == "api" {
            meta.push("API reference".into());
        }
        if let Some(updated) = hit["content"]["updated"].as_str() {
            meta.push(format!("updated {}", updated.get(..10).unwrap_or(updated)));
        }
        if !meta.is_empty() {
            blocks.push(json!({"type": "context", "elements": [{"type": "mrkdwn", "text": escape(&meta.join(" · "))}]}));
        }
    }
    blocks.push(
        json!({"type": "context", "elements": [{"type": "mrkdwn", "text":
        "Only you can see this · from support.fortanix.com"}]}),
    );
    json!({
        "response_type": "ephemeral",
        "replace_original": true,
        "text": format!("{} support pages for “{}”", hits.len().min(5), escape(query)),
        "blocks": blocks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_and_escaping() {
        assert_eq!(escape("a <b> & c"), "a &lt;b&gt; &amp; c");
        assert_eq!(link("https://x.com/a", "T|x"), "<https://x.com/a|T¦x>");
        assert_eq!(
            link("https://x.com/a>|evil", "T"),
            "<https://x.com/aevil|T>"
        );
        assert_eq!(link("javascript:alert(1)", "T"), "T", "only https links");
    }

    #[test]
    fn results_and_empty() {
        let result = json!({"results": [{
            "title": "Configure SAML SSO <for> DSM",
            "source": {"url": "https://support.fortanix.com/docs/dsm-saml-sso"},
            "content": {"snippet": "DSM supports SAML 2.0.", "product": "dsm", "section": "docs", "updated": "2026-08-01T05:56:18Z"},
        }]});
        let msg = results("saml", &result);
        assert_eq!(msg["response_type"], "ephemeral");
        assert_eq!(msg["replace_original"], true);
        let text = serde_json::to_string(&msg["blocks"]).unwrap();
        assert!(text.contains(
            "<https://support.fortanix.com/docs/dsm-saml-sso|Configure SAML SSO &lt;for&gt; DSM>"
        ));
        assert!(text.contains("DSM · updated 2026-08-01"));
        assert!(text.contains("Only you can see this"));
        let long = results(
            "x",
            &json!({"results": [{"title": "T".repeat(5000), "source": {"url": "https://support.fortanix.com/docs/x"}, "content": {}}]}),
        );
        assert!(
            long["blocks"][1]["text"]["text"]
                .as_str()
                .unwrap()
                .chars()
                .count()
                < 400
        );
        let empty = results("zebra", &json!({"results": []}));
        assert!(serde_json::to_string(&empty)
            .unwrap()
            .contains("No pages match"));
    }
}
