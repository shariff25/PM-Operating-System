//! Server-rendered HTML: escaping, the page frame, tables, and the one stylesheet.
//!
//! Every dynamic value goes through [`Esc`] (text and attribute values alike). Pages carry
//! no script, and the Content-Security-Policy allows none: only the inline stylesheet, by
//! its hash.

use std::fmt::{self, Write};
use std::sync::LazyLock;

use base64::Engine;
use sha2::{Digest, Sha256};

use crate::auth::Viewer;

/// Escapes text for HTML element content and quoted attribute values.
pub struct Esc<'a>(pub &'a str);

impl fmt::Display for Esc<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut rest = self.0;
        while let Some(i) = rest.find(['&', '<', '>', '"', '\'']) {
            f.write_str(&rest[..i])?;
            f.write_str(match rest.as_bytes()[i] {
                b'&' => "&amp;",
                b'<' => "&lt;",
                b'>' => "&gt;",
                b'"' => "&quot;",
                _ => "&#39;",
            })?;
            rest = &rest[i + 1..];
        }
        f.write_str(rest)
    }
}

/// `s` escaped, as a `String`.
pub fn esc(s: &str) -> String {
    Esc(s).to_string()
}

/// `s` cut to `max` characters, with an ellipsis if anything was cut.
pub fn clip(s: &str, max: usize) -> String {
    let mut out: String = s.chars().take(max).collect();
    if s.chars().nth(max).is_some() {
        out.push('…');
    }
    out
}

/// A timestamp as `2026-11-14 08:00 UTC`, in a `<time>` element; `—` when empty.
pub fn when(at: &str) -> String {
    if at.is_empty() {
        return "—".into();
    }
    match chrono::DateTime::parse_from_rfc3339(at) {
        Ok(t) => format!(
            "<time datetime=\"{}\">{}</time>",
            Esc(at),
            t.with_timezone(&chrono::Utc).format("%Y-%m-%d %H:%M UTC")
        ),
        Err(_) => esc(at),
    }
}

/// A status as a coloured badge. The class comes from a fixed list, never from the value.
pub fn badge(status: &str) -> String {
    let class = match status {
        "done" | "approved" | "enabled" | "ok" => "good",
        "failed" | "rejected" | "denied" | "error" | "never" => "bad",
        "queued" | "working" | "needs_approval" | "pending" | "executing" | "paused"
        | "approve" | "started" => "wait",
        _ => "plain",
    };
    format!("<span class=\"badge {class}\">{}</span>", Esc(status))
}

/// A JSON value for a `<pre>`: strings as they are, anything else pretty-printed.
pub fn json_text(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    }
}

/// A table whose cells are already HTML. On a phone each row becomes a card, with each
/// cell labelled by its column.
pub fn table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let mut out = String::from("<div class=\"table\"><table><thead><tr>");
    for h in headers {
        let _ = write!(out, "<th scope=\"col\">{}</th>", Esc(h));
    }
    out.push_str("</tr></thead><tbody>");
    for row in rows {
        out.push_str("<tr>");
        for (i, cell) in row.iter().enumerate() {
            let label = headers.get(i).copied().unwrap_or("");
            let _ = write!(out, "<td data-label=\"{}\">{cell}</td>", Esc(label));
        }
        out.push_str("</tr>");
    }
    out.push_str("</tbody></table></div>");
    out
}

/// A paragraph saying there's nothing to show.
pub fn empty(text: &str) -> String {
    format!("<p class=\"empty\">{}</p>", Esc(text))
}

/// The four screens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Bots,
    Routines,
    Approvals,
    Activity,
}

impl Screen {
    const ALL: [Screen; 4] = [
        Screen::Bots,
        Screen::Routines,
        Screen::Approvals,
        Screen::Activity,
    ];

    fn path(self) -> &'static str {
        match self {
            Self::Bots => "/bots",
            Self::Routines => "/routines",
            Self::Approvals => "/approvals",
            Self::Activity => "/activity",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Bots => "Bots",
            Self::Routines => "Routines",
            Self::Approvals => "Approvals",
            Self::Activity => "Activity",
        }
    }
}

/// A whole page: the header with the four screens and who is signed in, then `body` (HTML).
pub fn page(title: &str, active: Option<Screen>, viewer: Option<&Viewer>, body: &str) -> String {
    let mut nav = String::new();
    if viewer.is_some() {
        nav.push_str("<nav aria-label=\"Screens\">");
        for s in Screen::ALL {
            let current = if Some(s) == active {
                " aria-current=\"page\""
            } else {
                ""
            };
            let _ = write!(nav, "<a href=\"{}\"{current}>{}</a>", s.path(), s.label());
        }
        nav.push_str("</nav>");
    }
    let who = match viewer {
        Some(v) => format!(
            "<p class=\"who\">{}{}</p>",
            Esc(&v.handle),
            if v.admin { " · admin" } else { "" }
        ),
        None => String::new(),
    };
    format!(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
<meta name=\"color-scheme\" content=\"light dark\">\
<title>{title} · PM OS console</title><style>{CSS}</style></head>\
<body><header><div class=\"bar\"><a class=\"brand\" href=\"/\">PM OS console</a>{who}</div>{nav}</header>\
<main><h1>{title}</h1>{body}</main></body></html>",
        title = Esc(title),
    )
}

/// The `Content-Security-Policy` for every page: no scripts, no frames, forms only to the
/// console, and only the inline stylesheet.
pub static CSP: LazyLock<String> = LazyLock::new(|| {
    let hash = base64::engine::general_purpose::STANDARD.encode(Sha256::digest(CSS.as_bytes()));
    format!(
        "default-src 'none'; style-src 'sha256-{hash}'; img-src 'self'; form-action 'self'; \
         base-uri 'none'; frame-ancestors 'none'"
    )
});

const CSS: &str = r#"
:root{color-scheme:light dark;--bg:#ffffff;--fg:#1c1e21;--muted:#5c6470;--line:#dde1e6;--panel:#f5f6f8;--accent:#1f55c0;--good:#1d7a37;--bad:#b3261e;--wait:#8a5a00;--goodbg:#e6f4ea;--badbg:#fdecea;--waitbg:#fff4dc}
@media (prefers-color-scheme:dark){:root{--bg:#15171a;--fg:#e5e7ea;--muted:#9aa2ad;--line:#2c3036;--panel:#1c1f24;--accent:#86a8ff;--good:#6bd18a;--bad:#ff8a80;--wait:#f0b44c;--goodbg:#173322;--badbg:#3a1c1a;--waitbg:#352a12}}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--fg);font:15px/1.5 system-ui,-apple-system,"Segoe UI",Roboto,sans-serif}
a{color:var(--accent)}
header{border-bottom:1px solid var(--line);background:var(--panel)}
.bar{display:flex;justify-content:space-between;align-items:center;gap:1rem;max-width:72rem;margin:0 auto;padding:.6rem 1rem}
.brand{font-weight:600;color:var(--fg);text-decoration:none}
.who{margin:0;color:var(--muted);font-size:.9rem}
nav{display:flex;flex-wrap:wrap;gap:.25rem;max-width:72rem;margin:0 auto;padding:0 1rem .5rem}
nav a{padding:.3rem .7rem;border-radius:6px;text-decoration:none;color:var(--fg)}
nav a[aria-current=page]{background:var(--bg);border:1px solid var(--line);font-weight:600}
main{max-width:72rem;margin:0 auto;padding:1rem}
h1{font-size:1.4rem;margin:.5rem 0 1rem}
h2{font-size:1.1rem;margin:1.75rem 0 .5rem}
h3{font-size:1rem;margin:0 0 .5rem}
.note{background:var(--panel);border:1px solid var(--line);border-radius:8px;padding:.6rem .9rem}
.notice{background:var(--goodbg);border-radius:8px;padding:.5rem .9rem}
.error{background:var(--badbg);border-radius:8px;padding:.6rem .9rem}
.empty{color:var(--muted)}
.muted{color:var(--muted)}
.table{overflow-x:auto}
table{width:100%;border-collapse:collapse;font-size:.92rem}
th,td{text-align:left;vertical-align:top;padding:.45rem .6rem;border-bottom:1px solid var(--line)}
th{color:var(--muted);font-weight:600;white-space:nowrap}
code,pre{font:.85rem/1.45 ui-monospace,SFMono-Regular,Menlo,Consolas,monospace}
pre{white-space:pre-wrap;word-break:break-word;background:var(--panel);border:1px solid var(--line);border-radius:6px;padding:.5rem .7rem;margin:.25rem 0;max-height:28rem;overflow:auto}
.badge{display:inline-block;padding:0 .45rem;border-radius:999px;font-size:.8rem;border:1px solid var(--line);white-space:nowrap}
.badge.good{color:var(--good);background:var(--goodbg);border-color:transparent}
.badge.bad{color:var(--bad);background:var(--badbg);border-color:transparent}
.badge.wait{color:var(--wait);background:var(--waitbg);border-color:transparent}
.cards{display:grid;grid-template-columns:repeat(auto-fill,minmax(20rem,1fr));gap:1rem}
.card{border:1px solid var(--line);border-radius:8px;padding:.8rem 1rem;background:var(--bg)}
dl{display:grid;grid-template-columns:max-content 1fr;gap:.2rem .8rem;margin:0}
dt{color:var(--muted)}
dd{margin:0;word-break:break-word}
ul.plain{margin:0;padding-left:1.1rem}
form.inline{display:inline}
button{font:inherit;padding:.2rem .7rem;border-radius:6px;border:1px solid var(--line);background:var(--panel);color:var(--fg);cursor:pointer}
button.danger{color:var(--bad)}
details summary{cursor:pointer;color:var(--accent)}
.actions{display:flex;flex-wrap:wrap;gap:.4rem;align-items:flex-start}
@media (max-width:640px){
 thead{display:none}
 table,tbody,tr,td{display:block;width:100%}
 tr{border:1px solid var(--line);border-radius:8px;margin:0 0 .75rem;padding:.25rem 0}
 td{border:0;padding:.25rem .7rem}
 td::before{content:attr(data-label);display:block;color:var(--muted);font-size:.78rem}
 .cards{grid-template-columns:1fr}
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_everything_that_matters() {
        assert_eq!(
            esc(r#"<script>alert("x")</script> & 'y'"#),
            "&lt;script&gt;alert(&quot;x&quot;)&lt;/script&gt; &amp; &#39;y&#39;"
        );
        assert_eq!(esc("plain ünïcode"), "plain ünïcode");
    }

    #[test]
    fn clips_by_character() {
        assert_eq!(clip("héllo", 3), "hél…");
        assert_eq!(clip("hi", 3), "hi");
    }

    #[test]
    fn formats_times_and_badges() {
        assert_eq!(
            when("2026-11-14T08:00:00Z"),
            "<time datetime=\"2026-11-14T08:00:00Z\">2026-11-14 08:00 UTC</time>"
        );
        assert_eq!(when("<b>"), "&lt;b&gt;");
        assert_eq!(when(""), "—");
        assert_eq!(badge("done"), "<span class=\"badge good\">done</span>");
        assert!(badge("\"><x").contains("class=\"badge plain\">&quot;&gt;&lt;x<"));
    }

    #[test]
    fn the_policy_allows_the_stylesheet_and_no_script() {
        assert!(CSP.contains("style-src 'sha256-"));
        assert!(CSP.starts_with("default-src 'none'"));
        assert!(!CSP.contains("script-src"));
    }
}
