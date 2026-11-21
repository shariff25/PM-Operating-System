//! Block Kit messages for `/docs`, and the escaping every Slack message uses.

use serde_json::{json, Value};

/// Escape text for Slack `mrkdwn`: `&`, `<` and `>` are control characters there.
pub fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// A person's display name for `mrkdwn`, at most `max` characters, rendered inert. Display
/// names come from Slack profiles (`users.info`, and the user database's `display_name`,
/// which came from there) and are untrusted text (`docs/user-database-spec.md` §1, §11):
///
/// - control characters and line breaks become spaces, and runs of spaces one;
/// - `&`, `<` and `>` are escaped, so `<!channel>`, `<!here>`, `<@U…>`, `<#C…>` and
///   `<https://…|label>` show as typed and notify or link nothing;
/// - the `mrkdwn` formatting characters `*`, `_`, `~` and `` ` `` become look-alikes, so a
///   name can't make bold, italic, strike or code, nor close a span around it;
/// - `.`, `/`, `:` and `@` become look-alikes, so Slack doesn't auto-link a URL, a domain
///   or an address written into a name.
///
/// It reads the same to a person; only the characters Slack would act on change.
pub fn name(text: &str, max: usize) -> String {
    let mut flat = String::with_capacity(text.len());
    for c in text.chars() {
        let c = if c.is_control() || c.is_whitespace() {
            ' '
        } else {
            c
        };
        if c == ' ' && (flat.is_empty() || flat.ends_with(' ')) {
            continue;
        }
        flat.push(c);
    }
    let flat = truncate(flat.trim_end(), max);
    let mut out = String::with_capacity(flat.len() + 8);
    for c in flat.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '*' => out.push('\u{2217}'), // ∗ asterisk operator
            '_' => out.push('\u{2017}'), // ‗ double low line
            '~' => out.push('\u{223C}'), // ∼ tilde operator
            '`' => out.push('\u{02CB}'), // ˋ modifier letter grave accent
            '.' => out.push('\u{2024}'), // ․ one dot leader
            '/' => out.push('\u{2215}'), // ∕ division slash
            ':' => out.push('\u{2236}'), // ∶ ratio
            '@' => out.push('\u{FF20}'), // ＠ fullwidth commercial at
            c => out.push(c),
        }
    }
    out
}

/// A link a Slack message can show: only `https://` URLs, with `|` and `>` removed so they
/// can't break out of the `<url|label>` syntax.
pub fn link(url: &str, label: &str) -> String {
    let safe_url: String = url.chars().filter(|c| !"|<> ".contains(*c)).collect();
    let label = escape(label).replace('|', "¦");
    if safe_url.starts_with("https://") {
        format!("<{safe_url}|{label}>")
    } else {
        label
    }
}

/// At most `max` characters, with an ellipsis if anything was cut.
pub fn truncate(text: &str, max: usize) -> String {
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

pub const USAGE: &str = "Search the docs: `/docs <what you're looking for>`.\n\
     Narrow to one product with `product:<id>`, using an id from your docs products list \
     (or `general`). Example: `/docs product:vault rotate a key`";

pub fn searching(query: &str) -> Value {
    text(&format!("Searching the docs for “{}”…", escape(query)))
}

/// The results message, built from the tool server's `docs_search` result.
pub fn results(query: &str, result: &Value) -> Value {
    let hits: Vec<&Value> = result["results"].as_array().into_iter().flatten().collect();
    let heading = format!("*Docs* · “{}”", escape(&truncate(query, 80)));
    // No docs site is configured: the tool server passes the docs server's note on.
    if let Some(note) = result["note"].as_str().filter(|_| hits.is_empty()) {
        return json!({
            "response_type": "ephemeral",
            "replace_original": true,
            "text": escape(note),
            "blocks": [
                {"type": "section", "text": {"type": "mrkdwn", "text": heading}},
                {"type": "section", "text": {"type": "mrkdwn", "text": escape(&truncate(note, 500))}},
            ],
        });
    }
    if hits.is_empty() {
        return json!({
            "response_type": "ephemeral",
            "replace_original": true,
            "text": format!("No docs pages match “{}”.", escape(query)),
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
        "Only you can see this · from the docs"}]}),
    );
    json!({
        "response_type": "ephemeral",
        "replace_original": true,
        "text": format!("{} docs pages for “{}”", hits.len().min(5), escape(query)),
        "blocks": blocks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_names_render_inert() {
        for hostile in [
            "<!channel>",
            "<!here>",
            "<@U0ANA>",
            "<#C0GENERAL>",
            "<https://evil.example|click>",
            "<!subteam^S123>",
        ] {
            let shown = name(hostile, 100);
            assert!(!shown.contains('<') && !shown.contains('>'), "{shown}");
            assert!(shown.starts_with("&lt;"), "{shown}");
        }
        let shown = name(
            "*Bold* _it_ ~st~ `code` https://evil.example ana@example.com",
            200,
        );
        for c in ['*', '_', '~', '`', '.', '/', ':', '@'] {
            assert!(!shown.contains(c), "{c} in {shown}");
        }
        assert_eq!(name("Ana  \n\t Ruiz\u{7}", 100), "Ana Ruiz");
        assert_eq!(name("Ben & Jerry", 100), "Ben &amp; Jerry");
        assert_eq!(name("Abcdef", 3), "Abc…");
        assert_eq!(name("", 10), "");
    }

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
            "title": "Configure SAML SSO <for> Acme Vault",
            "source": {"url": "https://docs.example.com/docs/vault-saml-sso"},
            "content": {"snippet": "Acme Vault supports SAML 2.0.", "product": "vault", "section": "docs", "updated": "2026-08-01T05:56:18Z"},
        }]});
        let msg = results("saml", &result);
        assert_eq!(msg["response_type"], "ephemeral");
        assert_eq!(msg["replace_original"], true);
        let text = serde_json::to_string(&msg["blocks"]).unwrap();
        assert!(text.contains(
            "<https://docs.example.com/docs/vault-saml-sso|Configure SAML SSO &lt;for&gt; Acme Vault>"
        ));
        assert!(text.contains("VAULT · updated 2026-08-01"));
        assert!(text.contains("Only you can see this"));
        let long = results(
            "x",
            &json!({"results": [{"title": "T".repeat(5000), "source": {"url": "https://docs.example.com/docs/x"}, "content": {}}]}),
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
        let unconfigured = results(
            "saml",
            &json!({"results": [], "note": "No docs site is configured; ask a platform owner to set PMOS_DOCS_BASE_URL."}),
        );
        let s = serde_json::to_string(&unconfigured).unwrap();
        assert!(s.contains("No docs site is configured"), "{s}");
        assert!(!s.contains("No pages match"));
    }
}
