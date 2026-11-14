//! The support portal's formats.
//!
//! support.fortanix.com publishes a documentation index at `/llms.txt`:
//! `## v1` (docs) and `## v1-api (API Reference)` sections of `- [Title](url.md)` lines,
//! sometimes followed by `: description`. Each page has a markdown version at that URL:
//! YAML front matter (`title`, `slug`, `updated`, `published`, `canonical`), a short
//! "Documentation Index" note, then the page body.

use chrono::{DateTime, Utc};
use pmos_contract::frontmatter;
use serde::{Deserialize, Serialize};

/// One indexed page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DocPage {
    /// The `.md` URL from the index. Also the page's id.
    pub url: String,
    pub slug: String,
    pub title: String,
    /// `docs` or `api`.
    pub section: String,
    /// Product label for filtering, such as `dsm`; see [`product_of`].
    pub product: String,
    pub description: Option<String>,
    /// The page's `##`–`####` headings, one per line.
    pub headings: String,
    /// Markdown body, without front matter or the index note.
    #[serde(skip)]
    pub body: String,
    pub updated: Option<DateTime<Utc>>,
    #[serde(skip)]
    pub etag: Option<String>,
    pub fetched_at: DateTime<Utc>,
}

impl DocPage {
    /// The page people should open: the `.md` URL without the extension.
    pub fn web_url(&self) -> String {
        self.url
            .strip_suffix(".md")
            .unwrap_or(&self.url)
            .to_string()
    }
}

/// One entry of the index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexEntry {
    pub title: String,
    pub url: String,
    pub section: String,
    pub description: Option<String>,
}

/// The URL paths pages live under: guides and knowledge base, and the API reference.
pub const PAGE_PREFIXES: [&str; 2] = ["/docs/", "/apidocs/"];

/// The flat page path under one of [`PAGE_PREFIXES`], if `url` is one: `{base}{prefix}{slug}`
/// with a plain slug (no `/`, `?`, `#` or `..`).
pub fn page_path<'a>(url: &'a str, base: &str) -> Option<(&'static str, &'a str)> {
    let rest = url.strip_prefix(base.trim_end_matches('/'))?;
    PAGE_PREFIXES.iter().find_map(|prefix| {
        let slug = rest.strip_prefix(prefix)?;
        let ok = !slug.is_empty() && !slug.contains(['/', '?', '#']) && !slug.contains("..");
        ok.then_some((*prefix, slug))
    })
}

/// Parse `llms.txt`. Keeps only `.md` pages under [`PAGE_PREFIXES`], deduplicated, in order.
pub fn parse_index(text: &str, base: &str) -> Vec<IndexEntry> {
    let mut section = "docs".to_string();
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if let Some(heading) = line.strip_prefix("## ") {
            section = if heading.contains("api") || heading.contains("API") {
                "api".into()
            } else {
                "docs".into()
            };
            continue;
        }
        let Some(rest) = line.strip_prefix("- [") else {
            continue;
        };
        let Some((title, rest)) = rest.split_once("](") else {
            continue;
        };
        let Some((url, rest)) = rest.split_once(')') else {
            continue;
        };
        let url = url.trim();
        let valid = url.ends_with(".md") && page_path(url, base).is_some();
        if !valid || !seen.insert(url.to_string()) {
            continue;
        }
        // API reference pages are the API section wherever the index lists them.
        let section = if url.contains("/apidocs/") {
            "api".to_string()
        } else {
            section.clone()
        };
        let description = rest
            .trim()
            .strip_prefix(':')
            .map(str::trim)
            .filter(|d| !d.is_empty())
            .map(String::from);
        out.push(IndexEntry {
            title: title.trim().to_string(),
            url: url.to_string(),
            section,
            description,
        });
    }
    out
}

#[derive(Debug, Default, Deserialize)]
struct PageFrontMatter {
    title: Option<String>,
    slug: Option<String>,
    updated: Option<String>,
}

/// A page's markdown split into its parts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedPage {
    pub title: Option<String>,
    pub slug: Option<String>,
    pub updated: Option<DateTime<Utc>>,
    pub headings: String,
    pub body: String,
    /// For API reference pages: the OpenAPI document's `info.title`, such as
    /// "Fortanix DSM REST API".
    pub api_title: Option<String>,
}

/// Parse a page's markdown. Pages without front matter are kept as plain bodies.
pub fn parse_page(markdown: &str) -> ParsedPage {
    let (fm, body) = match frontmatter::split(markdown) {
        Ok(doc) => (
            serde_norway::from_str::<PageFrontMatter>(doc.yaml).unwrap_or_default(),
            doc.body,
        ),
        Err(_) => (PageFrontMatter::default(), markdown),
    };
    // Drop the portal's "Documentation Index" note: a leading blockquote.
    let mut lines = body.lines().peekable();
    while lines.peek().is_some_and(|l| l.trim().is_empty()) {
        lines.next();
    }
    if lines
        .peek()
        .is_some_and(|l| l.trim_start().starts_with("> ## Documentation Index"))
    {
        while lines
            .peek()
            .is_some_and(|l| l.trim_start().starts_with('>'))
        {
            lines.next();
        }
    }
    let (body, api_title) = strip_openapi(&lines.collect::<Vec<_>>().join("\n"));
    let body = body.trim().to_string();
    let headings = body
        .lines()
        .filter_map(|l| {
            let t = l.trim_start();
            ["#### ", "### ", "## "]
                .iter()
                .find_map(|p| t.strip_prefix(p))
                .map(|h| h.trim().trim_matches('*').trim().to_string())
        })
        .filter(|h| !h.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    ParsedPage {
        title: fm.title,
        slug: fm.slug,
        updated: fm
            .updated
            .and_then(|u| DateTime::parse_from_rfc3339(&u).ok())
            .map(|d| d.with_timezone(&Utc)),
        headings,
        body,
        api_title,
    }
}

/// Replace each embedded OpenAPI document (a ```` ```json POST /path ```` fence) with its
/// endpoint line. The documents are large and repetitive, and would drown the page's prose
/// in search; the page link keeps the full schema. Returns the body and the API's title.
fn strip_openapi(body: &str) -> (String, Option<String>) {
    let mut out = Vec::new();
    let mut title = None;
    let mut fence: Option<String> = None;
    for line in body.lines() {
        let trimmed = line.trim_start();
        if let Some(marker) = &fence {
            if trimmed.starts_with(marker.as_str()) && trimmed.trim_matches('`').is_empty() {
                fence = None;
            } else if title.is_none() {
                if let Some(rest) = trimmed.strip_prefix("\"title\":") {
                    title = rest
                        .trim()
                        .trim_end_matches(',')
                        .trim_matches('"')
                        .split('"')
                        .next()
                        .map(String::from);
                }
            }
            continue;
        }
        let ticks = trimmed.len() - trimmed.trim_start_matches('`').len();
        if ticks >= 3 {
            let info = trimmed[ticks..].trim();
            let mut parts = info.split_whitespace();
            let is_openapi = parts.next() == Some("json")
                && parts.next().is_some_and(|m| {
                    ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD"].contains(&m)
                });
            if is_openapi {
                let endpoint = info.trim_start_matches("json").trim();
                out.push(format!("`{endpoint}`"));
                fence = Some("`".repeat(ticks));
                continue;
            }
        }
        out.push(line.to_string());
    }
    (out.join("\n"), title)
}

/// Product labels people filter by, matched against the title and URL. The first rule that
/// matches wins, so more specific names come before shorter ones.
pub const PRODUCTS: [(&str, &[&str]); 5] = [
    ("confidential-ai", &["confidential ai", "confidential-ai"]),
    ("ccm", &["confidential computing manager", "ccm"]),
    ("key-insight", &["key insight", "key-insight"]),
    ("armor", &["armor"]),
    ("dsm", &["data security manager", "dsm", "sdkms"]),
];

/// Lowercase words separated by single spaces, padded with a space at each end, so a
/// whole-word match is a plain `contains(" word ")`.
fn words(text: &str) -> String {
    let joined = text
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    format!(" {joined} ")
}

/// The product a page belongs to, or `general`. Matches whole words in the title or the URL
/// slug, so "Armored" is not "Armor". `api_title` is an API page's OpenAPI title, which
/// names the product for pages whose own title doesn't.
pub fn product_of(title: &str, url: &str, api_title: Option<&str>) -> &'static str {
    let slug = url
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .trim_end_matches(".md");
    let haystacks = [
        words(title),
        words(slug),
        words(api_title.unwrap_or_default()),
    ];
    for (product, needles) in PRODUCTS {
        for needle in needles {
            let needle = words(needle);
            if haystacks.iter().any(|h| h.contains(&needle)) {
                return product;
            }
        }
    }
    "general"
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "https://support.fortanix.com";

    #[test]
    fn index_sections_descriptions_and_filtering() {
        let text = "# Fortanix\n\n> Knowledge base.\n\n## v1\n\n\
            - [Getting Started](https://support.fortanix.com/docs/getting-started.md)\n\
            - [Oracle TDE](https://support.fortanix.com/docs/oracle-tde.md): Fortanix DSM with Oracle TDE\n\
            - [Dup](https://support.fortanix.com/docs/getting-started.md)\n\
            - [Elsewhere](https://docs.aws.amazon.com/x.md)\n\
            - [Html](https://support.fortanix.com/docs/page)\n\
            - [Nested](https://support.fortanix.com/docs/a/b.md)\n\
            ## v1-api (API Reference)\n\
            - [Create key](https://support.fortanix.com/docs/create-key.md)\n";
        let entries = parse_index(text, BASE);
        let urls: Vec<_> = entries
            .iter()
            .map(|e| e.url.rsplit('/').next().unwrap())
            .collect();
        assert_eq!(
            urls,
            vec!["getting-started.md", "oracle-tde.md", "create-key.md"]
        );
        assert_eq!(
            entries[1].description.as_deref(),
            Some("Fortanix DSM with Oracle TDE")
        );
        assert_eq!(entries[0].section, "docs");
        assert_eq!(entries[2].section, "api");
    }

    #[test]
    fn api_reference_pages_are_indexed_without_their_openapi_documents() {
        let text = "## v1-api (API Reference)\n- [Create account](https://support.fortanix.com/apidocs/create-a-new-account.md)\n";
        let e = parse_index(text, BASE);
        assert_eq!(e[0].section, "api");
        let md = "> ## Documentation Index\n> Fetch it.\n\n# Create a new account\n\nCreates an account.\n\n## OpenAPI\n\n````json POST /sys/v1/accounts\n{\n  \"openapi\": \"3.0.0\",\n  \"info\": {\n    \"title\": \"Fortanix DSM REST API\",\n    \"description\": \"long\"\n  }\n}\n````\n\nAfter.";
        let p = parse_page(md);
        assert!(p.body.contains("`POST /sys/v1/accounts`"));
        assert!(!p.body.contains("openapi"));
        assert!(p.body.ends_with("After."));
        assert_eq!(p.api_title.as_deref(), Some("Fortanix DSM REST API"));
        let url = format!("{BASE}/apidocs/create-a-new-account.md");
        assert_eq!(
            product_of("Create a new account", &url, p.api_title.as_deref()),
            "dsm"
        );
        // Ordinary code blocks stay.
        let code = parse_page("# T\n\n```bash\ncurl x\n```\n");
        assert!(code.body.contains("curl x"));
    }

    #[test]
    fn page_front_matter_and_index_note() {
        let md = "---\ntitle: \"Fortanix Support\"\nslug: \"fortanix-support\"\nupdated: 2026-08-01T05:56:18Z\n---\n\n\
            > ## Documentation Index\n> Fetch the complete documentation index at: https://support.fortanix.com/llms.txt\n> Use this file.\n\n\
            # Fortanix Support\n\n### Fortanix 24/7 Support\n\nText.\n\n#### **Support program**\n\n- item\n";
        let p = parse_page(md);
        assert_eq!(p.title.as_deref(), Some("Fortanix Support"));
        assert_eq!(p.slug.as_deref(), Some("fortanix-support"));
        assert_eq!(p.updated.unwrap().to_rfc3339(), "2026-08-01T05:56:18+00:00");
        assert!(p.body.starts_with("# Fortanix Support"));
        assert!(!p.body.contains("Documentation Index"));
        assert_eq!(p.headings, "Fortanix 24/7 Support\nSupport program");

        let plain = parse_page("# Just a body\n\ntext");
        assert!(plain.title.is_none());
        assert_eq!(plain.body, "# Just a body\n\ntext");
    }

    #[test]
    fn products() {
        let u = |s: &str| format!("{BASE}/docs/{s}.md");
        assert_eq!(
            product_of("Fortanix DSM with Oracle TDE", &u("oracle-tde"), None),
            "dsm"
        );
        assert_eq!(
            product_of(
                "Backup and Restore",
                &u("fortanix-dsm-backup-and-restore"),
                None
            ),
            "dsm"
        );
        assert_eq!(
            product_of("Confidential AI Overview", &u("x"), None),
            "confidential-ai"
        );
        assert_eq!(
            product_of("Fortanix Confidential Computing Manager", &u("x"), None),
            "ccm"
        );
        assert_eq!(
            product_of("Key Insight Scans", &u("key-insight-scans"), None),
            "key-insight"
        );
        assert_eq!(
            product_of("Getting Started", &u("getting-started"), None),
            "general"
        );
        // A word that merely contains a product name doesn't count.
        assert_eq!(
            product_of("Armored transport", &u("armored-transport"), None),
            "general"
        );
    }

    #[test]
    fn web_url_drops_the_extension() {
        let p = DocPage {
            url: format!("{BASE}/docs/getting-started.md"),
            slug: "getting-started".into(),
            title: "Getting Started".into(),
            section: "docs".into(),
            product: "general".into(),
            description: None,
            headings: String::new(),
            body: String::new(),
            updated: None,
            etag: None,
            fetched_at: Utc::now(),
        };
        assert_eq!(p.web_url(), format!("{BASE}/docs/getting-started"));
    }
}
