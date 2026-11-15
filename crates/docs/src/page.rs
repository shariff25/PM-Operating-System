//! The docs site's formats, and which of its pages are indexed.
//!
//! The site publishes a documentation index at `/llms.txt` (the llms.txt convention):
//! `## ...` sections of `- [Title](url.md)` lines, sometimes followed by `: description`; a
//! section whose heading mentions "API" is the API reference. Each page has a markdown
//! version at that URL, optionally with YAML front matter (`title`, `slug`, `updated`) and a
//! leading "Documentation Index" note, then the page body.

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
    /// Product id for filtering, such as `vault`; see [`Products::product_of`].
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

/// The docs site being indexed: where it is and which of its pages count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Site {
    /// `https://docs.example.com`, without a trailing slash.
    pub base: String,
    /// URL path prefixes whose pages are indexed, each starting and ending with `/`. `/`
    /// takes every page on the site.
    pub prefixes: Vec<String>,
    /// Pages under this prefix are the API reference (section `api`).
    pub api_prefix: Option<String>,
}

/// A path prefix as `/path/`: a leading and a trailing slash.
pub fn normalize_prefix(prefix: &str) -> Option<String> {
    let trimmed = prefix.trim().trim_matches('/');
    if prefix.trim().is_empty() {
        return None;
    }
    if trimmed.is_empty() {
        return Some("/".into());
    }
    let ok = !trimmed.contains(['?', '#', ' '])
        && !trimmed
            .split('/')
            .any(|s| s.is_empty() || s == "." || s == "..");
    ok.then(|| format!("/{trimmed}/"))
}

impl Site {
    /// Every page on `base`, no API section.
    pub fn new(base: impl Into<String>) -> Self {
        Self {
            base: base.into().trim_end_matches('/').to_string(),
            prefixes: vec!["/".into()],
            api_prefix: None,
        }
    }

    pub fn with_prefixes(mut self, prefixes: &[&str]) -> Self {
        self.prefixes = prefixes
            .iter()
            .filter_map(|p| normalize_prefix(p))
            .collect();
        self
    }

    pub fn with_api_prefix(mut self, prefix: &str) -> Self {
        self.api_prefix = normalize_prefix(prefix);
        self
    }

    /// The page path under one of the prefixes, if `url` is a page of this site:
    /// `{base}{prefix}{rest}`, where `rest` is one or more non-empty path segments with no
    /// `?`, `#` or `..`. Returns the prefix and `rest`.
    pub fn page_path<'a>(&'a self, url: &'a str) -> Option<(&'a str, &'a str)> {
        let rest = url.strip_prefix(self.base.as_str())?;
        // The longest prefix wins, so `/docs/api/` can sit inside `/docs/`.
        let mut prefixes: Vec<&String> = self.prefixes.iter().collect();
        prefixes.sort_by_key(|p| std::cmp::Reverse(p.len()));
        prefixes.into_iter().find_map(|prefix| {
            let slug = rest.strip_prefix(prefix.as_str())?;
            let ok = !slug.is_empty()
                && !slug.contains(['?', '#', '\\'])
                && !slug.contains("..")
                && !slug.split('/').any(str::is_empty);
            ok.then_some((prefix.as_str(), slug))
        })
    }

    /// Whether `url` is an API reference page: under [`Site::api_prefix`].
    pub fn is_api(&self, url: &str) -> bool {
        let Some(api) = &self.api_prefix else {
            return false;
        };
        url.strip_prefix(self.base.as_str())
            .is_some_and(|rest| rest.starts_with(api.as_str()))
    }

    /// `url`'s path on the site, such as `/docs/vault-key-rotation.md`.
    pub fn path_of<'a>(&self, url: &'a str) -> &'a str {
        url.strip_prefix(self.base.as_str()).unwrap_or(url)
    }

    /// The prefixes for a person: "https://docs.example.com/docs/ and /apidocs/", or the
    /// site alone for `/`.
    pub fn describe(&self) -> String {
        if self.prefixes.iter().all(|p| p == "/") {
            return self.base.clone();
        }
        let mut parts = self.prefixes.clone();
        if let Some(first) = parts.first_mut() {
            *first = format!("{}{first}", self.base);
        }
        parts.join(" and ")
    }
}

/// Parse `llms.txt`. Keeps only `.md` pages of `site`, deduplicated, in order.
pub fn parse_index(text: &str, site: &Site) -> Vec<IndexEntry> {
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
        let valid = url.ends_with(".md") && site.page_path(url).is_some();
        if !valid || !seen.insert(url.to_string()) {
            continue;
        }
        // Pages under the API prefix are the API section wherever the index lists them.
        let section = if site.is_api(url) {
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
    /// "Acme Vault REST API".
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

/// The product every page belongs to when no configured product matches.
pub const GENERAL: &str = "general";

/// One product people can filter by.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Product {
    /// Lowercase letters, digits and `-`, such as `vault`.
    pub id: String,
    /// For people, such as "Acme Vault". Defaults to the id.
    #[serde(default)]
    pub name: Option<String>,
    /// Words (or phrases) that mark a page as this product's, matched as whole words in the
    /// page's title or URL path, ignoring case.
    pub keywords: Vec<String>,
}

/// The configured products (`PMOS_DOCS_PRODUCTS`), in match order. Empty: every page is
/// [`GENERAL`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Products(Vec<Product>);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProductsFile {
    #[serde(default)]
    products: Vec<Product>,
}

/// The longest product id: the tool server caps `product` at 64 characters.
pub const MAX_PRODUCT_ID: usize = 64;

impl Products {
    /// Check and keep `products`: ids are lowercase `[a-z0-9-]`, unique and not
    /// `general`, and each product has at least one non-empty keyword.
    pub fn new(products: Vec<Product>) -> Result<Self, String> {
        let mut seen = std::collections::HashSet::new();
        for p in &products {
            let valid_id = !p.id.is_empty()
                && p.id.len() <= MAX_PRODUCT_ID
                && p.id
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
            if !valid_id {
                return Err(format!(
                    "product id `{}` must be 1–{MAX_PRODUCT_ID} lowercase letters, digits or `-`",
                    p.id
                ));
            }
            if p.id == GENERAL {
                return Err("`general` is reserved for pages no product matches".into());
            }
            if !seen.insert(p.id.as_str()) {
                return Err(format!("product id `{}` appears twice", p.id));
            }
            if p.keywords.is_empty() || p.keywords.iter().any(|k| words(k).trim().is_empty()) {
                return Err(format!(
                    "product `{}` needs at least one keyword, and no empty ones",
                    p.id
                ));
            }
        }
        Ok(Self(products))
    }

    /// Parse a products file: `products:` with `id`, `name` and `keywords` for each.
    pub fn parse(yaml: &str) -> Result<Self, String> {
        let file: ProductsFile = serde_norway::from_str(yaml).map_err(|e| e.to_string())?;
        Self::new(file.products)
    }

    pub fn load(path: &std::path::Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn iter(&self) -> impl Iterator<Item = &Product> {
        self.0.iter()
    }

    /// Every id a search can filter by: the configured ones, then `general`.
    pub fn ids(&self) -> Vec<&str> {
        self.0
            .iter()
            .map(|p| p.id.as_str())
            .chain([GENERAL])
            .collect()
    }

    /// The display name for `id`: the configured name, else the id.
    pub fn name_of<'a>(&'a self, id: &'a str) -> &'a str {
        self.0
            .iter()
            .find(|p| p.id == id)
            .and_then(|p| p.name.as_deref())
            .unwrap_or(id)
    }

    /// The product a page belongs to, or [`GENERAL`]: the first product with a keyword
    /// among the whole words of the title, the URL path or (for API pages) the OpenAPI
    /// title, so "Vaulted" is not "Vault". `path` is the URL's path on the site.
    pub fn product_of(&self, title: &str, path: &str, api_title: Option<&str>) -> &str {
        let path = path.trim_end_matches(".md");
        let haystacks = [
            words(title),
            words(path),
            words(api_title.unwrap_or_default()),
        ];
        self.0
            .iter()
            .find(|p| {
                p.keywords.iter().any(|k| {
                    let needle = words(k);
                    haystacks.iter().any(|h| h.contains(&needle))
                })
            })
            .map_or(GENERAL, |p| p.id.as_str())
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "https://docs.example.com";

    fn site() -> Site {
        Site::new(BASE)
            .with_prefixes(&["/docs/", "/apidocs/"])
            .with_api_prefix("/apidocs/")
    }

    fn products() -> Products {
        Products::parse(
            "products:\n  - id: vault\n    name: Acme Vault\n    keywords: [vault, key vault]\n  - id: cloud\n    keywords: [cloud]\n",
        )
        .unwrap()
    }

    #[test]
    fn index_sections_descriptions_and_filtering() {
        let text = "# Acme\n\n> Knowledge base.\n\n## v1\n\n\
            - [Getting Started](https://docs.example.com/docs/getting-started.md)\n\
            - [Oracle TDE](https://docs.example.com/docs/oracle-tde.md): Acme Vault with Oracle TDE\n\
            - [Dup](https://docs.example.com/docs/getting-started.md)\n\
            - [Elsewhere](https://docs.aws.amazon.com/x.md)\n\
            - [Html](https://docs.example.com/docs/page)\n\
            - [Other path](https://docs.example.com/blog/post.md)\n\
            - [Lookalike](https://docs.example.com.evil.test/docs/x.md)\n\
            - [Climb](https://docs.example.com/docs/../admin.md)\n\
            - [Nested](https://docs.example.com/docs/a/b.md)\n\
            ## v1-api (API Reference)\n\
            - [Create key](https://docs.example.com/docs/create-key.md)\n";
        let entries = parse_index(text, &site());
        let urls: Vec<_> = entries
            .iter()
            .map(|e| site().path_of(&e.url).to_string())
            .collect();
        assert_eq!(
            urls,
            vec![
                "/docs/getting-started.md",
                "/docs/oracle-tde.md",
                "/docs/a/b.md",
                "/docs/create-key.md"
            ]
        );
        assert_eq!(
            entries[1].description.as_deref(),
            Some("Acme Vault with Oracle TDE")
        );
        assert_eq!(entries[0].section, "docs");
        assert_eq!(entries[3].section, "api", "the index's API heading");
    }

    #[test]
    fn the_default_prefix_takes_every_page_nested_or_not() {
        let all = Site::new(format!("{BASE}/"));
        assert_eq!(all.base, BASE, "no trailing slash");
        assert_eq!(all.prefixes, vec!["/"]);
        for (url, ok) in [
            ("https://docs.example.com/intro.md", true),
            ("https://docs.example.com/guides/vault/sso.md", true),
            ("https://docs.example.com/", false),
            ("https://docs.example.com/a//b.md", false),
            ("https://docs.example.com/a/../b.md", false),
            ("https://docs.example.com/a.md?x=1", false),
            ("https://docs.example.com/a.md#top", false),
            ("https://docs.example.com.evil.test/a.md", false),
            ("https://other.example.com/a.md", false),
        ] {
            assert_eq!(all.page_path(url).is_some(), ok, "{url}");
        }
        let text = "## Docs\n- [Intro](https://docs.example.com/intro.md)\n- [SSO](https://docs.example.com/guides/vault/sso.md)\n";
        assert_eq!(parse_index(text, &all).len(), 2);
        assert_eq!(all.describe(), BASE);
        assert_eq!(
            site().describe(),
            "https://docs.example.com/docs/ and /apidocs/"
        );
    }

    #[test]
    fn prefixes_are_normalized() {
        assert_eq!(normalize_prefix("docs").as_deref(), Some("/docs/"));
        assert_eq!(
            normalize_prefix("/help/center").as_deref(),
            Some("/help/center/")
        );
        assert_eq!(normalize_prefix("/").as_deref(), Some("/"));
        assert_eq!(normalize_prefix(""), None);
        assert_eq!(normalize_prefix("/a/../b"), None);
        assert_eq!(normalize_prefix("/a?b"), None);
        // The longest prefix wins.
        let s = Site::new(BASE).with_prefixes(&["/docs/", "/docs/api/"]);
        assert_eq!(
            s.page_path("https://docs.example.com/docs/api/x.md"),
            Some(("/docs/api/", "x.md"))
        );
    }

    #[test]
    fn api_reference_pages_are_indexed_without_their_openapi_documents() {
        let text = "## Guides\n- [Create account](https://docs.example.com/apidocs/create-a-new-account.md)\n";
        let e = parse_index(text, &site());
        assert_eq!(e[0].section, "api", "under the API prefix");
        let md = "> ## Documentation Index\n> Fetch it.\n\n# Create a new account\n\nCreates an account.\n\n## OpenAPI\n\n````json POST /sys/v1/accounts\n{\n  \"openapi\": \"3.0.0\",\n  \"info\": {\n    \"title\": \"Acme Vault REST API\",\n    \"description\": \"long\"\n  }\n}\n````\n\nAfter.";
        let p = parse_page(md);
        assert!(p.body.contains("`POST /sys/v1/accounts`"));
        assert!(!p.body.contains("openapi"));
        assert!(p.body.ends_with("After."));
        assert_eq!(p.api_title.as_deref(), Some("Acme Vault REST API"));
        assert_eq!(
            products().product_of(
                "Create a new account",
                "/apidocs/create-a-new-account.md",
                p.api_title.as_deref()
            ),
            "vault"
        );
        // Ordinary code blocks stay.
        let code = parse_page("# T\n\n```bash\ncurl x\n```\n");
        assert!(code.body.contains("curl x"));
    }

    #[test]
    fn page_front_matter_and_index_note() {
        let md = "---\ntitle: \"Acme Support\"\nslug: \"acme-support\"\nupdated: 2026-08-01T05:56:18Z\n---\n\n\
            > ## Documentation Index\n> Fetch the complete documentation index at: https://docs.example.com/llms.txt\n> Use this file.\n\n\
            # Acme Support\n\n### Acme 24/7 Support\n\nText.\n\n#### **Support program**\n\n- item\n";
        let p = parse_page(md);
        assert_eq!(p.title.as_deref(), Some("Acme Support"));
        assert_eq!(p.slug.as_deref(), Some("acme-support"));
        assert_eq!(p.updated.unwrap().to_rfc3339(), "2026-08-01T05:56:18+00:00");
        assert!(p.body.starts_with("# Acme Support"));
        assert!(!p.body.contains("Documentation Index"));
        assert_eq!(p.headings, "Acme 24/7 Support\nSupport program");

        let plain = parse_page("# Just a body\n\ntext");
        assert!(plain.title.is_none());
        assert_eq!(plain.body, "# Just a body\n\ntext");
    }

    #[test]
    fn products_match_whole_words_in_order() {
        let p = products();
        assert_eq!(
            p.product_of("Acme Vault with Oracle TDE", "/docs/oracle-tde.md", None),
            "vault"
        );
        assert_eq!(
            p.product_of(
                "Backup and Restore",
                "/docs/acme-vault-backup-and-restore.md",
                None
            ),
            "vault"
        );
        assert_eq!(
            p.product_of("Acme Cloud Overview", "/docs/x.md", None),
            "cloud"
        );
        assert_eq!(
            p.product_of("Overview", "/cloud/setup/x.md", None),
            "cloud",
            "any path segment"
        );
        assert_eq!(
            p.product_of("A Key Vault primer", "/docs/x.md", None),
            "vault"
        );
        assert_eq!(
            p.product_of("Getting Started", "/docs/getting-started.md", None),
            GENERAL
        );
        // A word that merely contains a keyword doesn't count.
        assert_eq!(
            p.product_of("Vaulted ceilings", "/docs/vaulted.md", None),
            GENERAL
        );
        // The first product wins when several match.
        assert_eq!(
            p.product_of("Vault in the Cloud", "/docs/x.md", None),
            "vault"
        );
        assert_eq!(p.ids(), vec!["vault", "cloud", "general"]);
        assert_eq!(p.name_of("vault"), "Acme Vault");
        assert_eq!(p.name_of("cloud"), "cloud");
        // No products: everything is general.
        assert_eq!(
            Products::default().product_of("Acme Vault", "/vault.md", None),
            GENERAL
        );
        assert_eq!(Products::default().ids(), vec!["general"]);
    }

    #[test]
    fn a_products_file_is_checked() {
        let bad = |yaml: &str| Products::parse(yaml).unwrap_err();
        assert!(bad("products:\n  - id: Vault\n    keywords: [v]\n").contains("lowercase"));
        assert!(bad("products:\n  - id: a b\n    keywords: [v]\n").contains("lowercase"));
        assert!(bad("products:\n  - id: general\n    keywords: [v]\n").contains("reserved"));
        assert!(
            bad("products:\n  - id: v\n    keywords: [v]\n  - id: v\n    keywords: [w]\n")
                .contains("twice")
        );
        assert!(bad("products:\n  - id: v\n    keywords: []\n").contains("keyword"));
        assert!(bad("products:\n  - id: v\n    keywords: [\"--\"]\n").contains("keyword"));
        assert!(bad("products:\n  - id: v\n").contains("keywords"));
        assert!(
            bad("products:\n  - id: v\n    keywords: [v]\n    colour: red\n").contains("colour")
        );
        assert!(bad(&format!(
            "products:\n  - id: {}\n    keywords: [v]\n",
            "a".repeat(65)
        ))
        .contains("lowercase"));
        assert_eq!(
            Products::parse("products: []\n").unwrap(),
            Products::default()
        );
        // The example file that ships with the repository is valid.
        let example = Products::load(std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../config/docs-products.example.yaml"
        )))
        .unwrap();
        assert_eq!(example.ids(), vec!["vault", "cloud", "general"]);
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
