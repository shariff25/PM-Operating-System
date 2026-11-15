//! The MCP surface: `search_docs`, `get_doc` and `list_products`.
//!
//! `get_doc` only serves pages already in the index. It never fetches the URL it's given, so
//! a caller can't use it to make this server request arbitrary addresses.

use std::sync::Arc;

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, Implementation, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool, ToolAnnotations,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler};
use serde_json::{json, Map, Value};

use crate::page::{Products, Site};
use crate::store::DocsStore;

/// The most of a page `get_doc` returns: enough for any guide's substance, small enough to
/// share a local model's context with other sessions.
pub const MAX_DOC_CHARS: usize = 12_000;
pub const MAX_RESULTS: u64 = 10;

/// What `search_docs` and `list_products` say, as a `note`, when no docs site is
/// configured; `get_doc` fails with it (code `not_configured`).
pub const NOT_CONFIGURED_NOTE: &str =
    "No docs site is configured; ask a platform owner to set PMOS_DOCS_BASE_URL.";

#[derive(Clone)]
pub struct DocsMcp {
    store: Arc<dyn DocsStore>,
    /// How tools name the site, such as "the public docs".
    name: String,
    /// The site, to turn page links into indexed `.md` URLs. `None`: no site configured.
    site: Option<Site>,
    products: Products,
}

impl DocsMcp {
    pub fn new(
        store: Arc<dyn DocsStore>,
        name: impl Into<String>,
        site: Option<Site>,
        products: Products,
    ) -> Self {
        Self {
            store,
            name: name.into(),
            site,
            products,
        }
    }

    /// "the public docs (https://docs.example.com)", or a note that there is no site.
    fn named_site(&self) -> String {
        match &self.site {
            Some(site) => format!("{} ({})", self.name, site.base),
            None => format!("{} (no docs site is configured yet)", self.name),
        }
    }

    pub fn tools(&self) -> Vec<Tool> {
        let schema = |v: Value| Arc::new(v.as_object().cloned().unwrap_or_default());
        vec![
            Tool::new(
                "search_docs",
                format!(
                    "Search {}. Returns titles, links, products and snippets.",
                    self.named_site()
                ),
                schema(json!({
                    "type": "object",
                    "properties": {
                        "query": {"type": "string", "description": "What to look for"},
                        "product": {"type": "string", "enum": self.products.ids()},
                        "limit": {"type": "integer", "minimum": 1, "maximum": MAX_RESULTS, "description": "Default 5"},
                    },
                    "required": ["query"],
                    "additionalProperties": false,
                })),
            )
            .annotate(ToolAnnotations::with_title("Search the docs").read_only(true).open_world(false)),
            Tool::new(
                "get_doc",
                format!(
                    "Read one indexed page of {} as markdown, by the url a search returned.",
                    self.name
                ),
                schema(json!({
                    "type": "object",
                    "properties": {"url": {"type": "string"}},
                    "required": ["url"],
                    "additionalProperties": false,
                })),
            )
            .annotate(ToolAnnotations::with_title("Read a docs page").read_only(true).open_world(false)),
            Tool::new(
                "list_products",
                "List the product labels in the index, with names and page counts.",
                schema(json!({"type": "object", "properties": {}, "additionalProperties": false})),
            )
            .annotate(ToolAnnotations::with_title("List products").read_only(true).open_world(false)),
        ]
    }

    /// The indexed `.md` URL for a link people may paste: the page URL or its `.md` form.
    fn index_url(site: &Site, url: &str) -> Option<String> {
        let (prefix, slug) = site.page_path(url.trim())?;
        let slug = slug.strip_suffix(".md").unwrap_or(slug);
        Some(format!("{}{prefix}{slug}.md", site.base))
    }

    pub async fn call(&self, name: &str, args: &Map<String, Value>) -> Result<Value, ToolFailure> {
        let allowed: &[&str] = match name {
            "search_docs" => &["query", "product", "limit"],
            "get_doc" => &["url"],
            "list_products" => &[],
            other => return Err(ToolFailure::bad_args(format!("unknown tool `{other}`"))),
        };
        if let Some(extra) = args.keys().find(|k| !allowed.contains(&k.as_str())) {
            return Err(ToolFailure::bad_args(format!("unknown argument `{extra}`")));
        }
        let storage = |e: crate::store::StoreError| {
            tracing::error!(error = %e, "docs storage error");
            ToolFailure::unavailable("the docs index is unavailable; try again shortly")
        };
        match name {
            "search_docs" => {
                let query = args
                    .get("query")
                    .and_then(Value::as_str)
                    .filter(|q| !q.trim().is_empty())
                    .ok_or("`query` is required")?;
                let ids = self.products.ids();
                let product = match args.get("product") {
                    None | Some(Value::Null) => None,
                    Some(Value::String(p)) if ids.contains(&p.as_str()) => Some(p.as_str()),
                    Some(_) => {
                        return Err(ToolFailure::bad_args(format!(
                            "`product` must be one of {}",
                            ids.join(", ")
                        )))
                    }
                };
                let limit = match args.get("limit") {
                    None => 5,
                    Some(v) => v
                        .as_u64()
                        .filter(|n| (1..=MAX_RESULTS).contains(n))
                        .ok_or(format!("`limit` must be 1–{MAX_RESULTS}"))?,
                };
                if self.site.is_none() {
                    return Ok(json!({"count": 0, "results": [], "note": NOT_CONFIGURED_NOTE}));
                }
                let hits = self
                    .store
                    .search(query, product, limit as usize)
                    .await
                    .map_err(storage)?;
                Ok(json!({
                    "count": hits.len(),
                    "results": hits.iter().map(|h| json!({
                        "title": h.page.title,
                        "url": h.page.web_url(),
                        "source_url": h.page.url,
                        "product": h.page.product,
                        "section": h.page.section,
                        "updated": h.page.updated,
                        "snippet": h.snippet,
                    })).collect::<Vec<_>>(),
                }))
            }
            "get_doc" => {
                let given = args
                    .get("url")
                    .and_then(Value::as_str)
                    .ok_or("`url` is required")?;
                let Some(site) = &self.site else {
                    return Err(ToolFailure::not_configured(NOT_CONFIGURED_NOTE));
                };
                let url = Self::index_url(site, given).ok_or_else(|| {
                    ToolFailure::bad_args(format!(
                        "only pages of {} under {} can be read",
                        self.name,
                        site.describe()
                    ))
                })?;
                let page = self
                    .store
                    .get(&url)
                    .await
                    .map_err(storage)?
                    .ok_or_else(|| {
                        ToolFailure::not_found("that page isn't in the index; search for it first")
                    })?;
                let truncated = page.body.chars().count() > MAX_DOC_CHARS;
                let markdown: String = page.body.chars().take(MAX_DOC_CHARS).collect();
                Ok(json!({
                    "title": page.title,
                    "url": page.web_url(),
                    "source_url": page.url,
                    "product": page.product,
                    "section": page.section,
                    "updated": page.updated,
                    "markdown": markdown,
                    "truncated": truncated,
                }))
            }
            _ => {
                // Every configured id (and `general`) with its page count, then any label
                // still in the index from an earlier products file (until the next sync).
                let counts = self.store.products().await.map_err(storage)?;
                let pages = |id: &str| {
                    counts
                        .iter()
                        .find(|c| c.product == id)
                        .map_or(0, |c| c.pages)
                };
                let ids = self.products.ids();
                let listed = ids
                    .iter()
                    .copied()
                    .chain(
                        counts
                            .iter()
                            .map(|c| c.product.as_str())
                            .filter(|p| !ids.contains(p)),
                    )
                    .map(|id| {
                        json!({
                            "product": id,
                            "name": self.products.name_of(id),
                            "pages": pages(id),
                        })
                    })
                    .collect::<Vec<_>>();
                let mut out = json!({ "products": listed });
                if self.site.is_none() {
                    out["note"] = json!(NOT_CONFIGURED_NOTE);
                }
                Ok(out)
            }
        }
    }
}

/// Why a tool call failed: `code` is one of `bad_args`, `not_found`, `unavailable` or
/// `not_configured` (no docs site is set up).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolFailure {
    pub code: &'static str,
    pub message: String,
}

impl ToolFailure {
    pub fn bad_args(message: impl Into<String>) -> Self {
        Self {
            code: "bad_args",
            message: message.into(),
        }
    }
    pub fn not_found(message: impl Into<String>) -> Self {
        Self {
            code: "not_found",
            message: message.into(),
        }
    }
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self {
            code: "unavailable",
            message: message.into(),
        }
    }
    pub fn not_configured(message: impl Into<String>) -> Self {
        Self {
            code: "not_configured",
            message: message.into(),
        }
    }
}

impl From<String> for ToolFailure {
    fn from(message: String) -> Self {
        Self::bad_args(message)
    }
}

impl From<&str> for ToolFailure {
    fn from(message: &str) -> Self {
        Self::bad_args(message)
    }
}

impl ServerHandler for DocsMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("pmos-docs", env!("CARGO_PKG_VERSION")))
            .with_instructions(match &self.site {
                Some(_) => format!(
                    "Search and read {}. Cite the url of every page you use.",
                    self.named_site()
                ),
                None => format!(
                    "Search and read {}. No docs site is configured yet, so searches return \
                     nothing; say so rather than answering from memory.",
                    self.name
                ),
            })
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(self.tools()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let args = request.arguments.unwrap_or_default();
        let result = match self.call(&request.name, &args).await {
            Ok(value) => CallToolResult::structured(value),
            // A machine-readable code, so the tool server needn't parse the wording.
            Err(failure) => CallToolResult::structured_error(json!({
                "code": failure.code,
                "message": failure.message,
            })),
        };
        Ok(result.into())
    }
}
