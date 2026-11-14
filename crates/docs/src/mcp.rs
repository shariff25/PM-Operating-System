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

use crate::page::PRODUCTS;
use crate::store::DocsStore;

/// `get_doc` returns at most this many characters of markdown.
pub const MAX_DOC_CHARS: usize = 40_000;
pub const MAX_RESULTS: u64 = 10;

#[derive(Clone)]
pub struct DocsMcp {
    store: Arc<dyn DocsStore>,
    /// `https://support.fortanix.com`, to turn page links into indexed `.md` URLs.
    base: String,
}

impl DocsMcp {
    pub fn new(store: Arc<dyn DocsStore>, base: impl Into<String>) -> Self {
        Self {
            store,
            base: base.into(),
        }
    }

    fn products() -> Vec<&'static str> {
        let mut p: Vec<&str> = PRODUCTS.iter().map(|(name, _)| *name).collect();
        p.push("general");
        p
    }

    pub fn tools() -> Vec<Tool> {
        let schema = |v: Value| Arc::new(v.as_object().cloned().unwrap_or_default());
        vec![
            Tool::new(
                "search_docs",
                "Search the Fortanix support documentation (support.fortanix.com). Returns titles, links, products and snippets.",
                schema(json!({
                    "type": "object",
                    "properties": {
                        "query": {"type": "string", "description": "What to look for"},
                        "product": {"type": "string", "enum": Self::products()},
                        "limit": {"type": "integer", "minimum": 1, "maximum": MAX_RESULTS, "description": "Default 5"},
                    },
                    "required": ["query"],
                    "additionalProperties": false,
                })),
            )
            .annotate(ToolAnnotations::with_title("Search support docs").read_only(true).open_world(false)),
            Tool::new(
                "get_doc",
                "Read one indexed support page as markdown, by the url a search returned.",
                schema(json!({
                    "type": "object",
                    "properties": {"url": {"type": "string"}},
                    "required": ["url"],
                    "additionalProperties": false,
                })),
            )
            .annotate(ToolAnnotations::with_title("Read a support page").read_only(true).open_world(false)),
            Tool::new(
                "list_products",
                "List the product labels in the index, with page counts.",
                schema(json!({"type": "object", "properties": {}, "additionalProperties": false})),
            )
            .annotate(ToolAnnotations::with_title("List products").read_only(true).open_world(false)),
        ]
    }

    /// The indexed `.md` URL for a link people may paste: the page URL or its `.md` form.
    fn index_url(&self, url: &str) -> Option<String> {
        let (prefix, slug) = crate::page::page_path(url.trim(), &self.base)?;
        let slug = slug.strip_suffix(".md").unwrap_or(slug);
        Some(format!(
            "{}{prefix}{slug}.md",
            self.base.trim_end_matches('/')
        ))
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
                let product = match args.get("product") {
                    None | Some(Value::Null) => None,
                    Some(Value::String(p)) if Self::products().contains(&p.as_str()) => {
                        Some(p.as_str())
                    }
                    Some(_) => {
                        return Err(ToolFailure::bad_args(format!(
                            "`product` must be one of {}",
                            Self::products().join(", ")
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
                let url = self
                    .index_url(given)
                    .ok_or("only support.fortanix.com/docs and /apidocs pages can be read")?;
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
                let products = self.store.products().await.map_err(storage)?;
                Ok(json!({ "products": products }))
            }
        }
    }
}

/// Why a tool call failed: `code` is one of `bad_args`, `not_found` or `unavailable`.
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
            .with_instructions(
                "Search and read the public Fortanix support documentation. Cite the url of \
                 every page you use.",
            )
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(Self::tools()))
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
