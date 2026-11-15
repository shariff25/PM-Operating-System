//! `pmos-docs`: an MCP server over your company's public documentation site.
//!
//! It keeps a searchable copy of the site named by `PMOS_DOCS_BASE_URL` (its `llms.txt`
//! index and each page's markdown, under the configured path prefixes) and serves
//! `search_docs`, `get_doc` and `list_products` over MCP. With no site configured it starts
//! with an empty index and says so. Only the PM OS tool server calls it; the tool server
//! applies allowlists, audit and untrusted-content wrapping before anything reaches an agent
//! or Slack.

pub mod app;
pub mod config;
pub mod mcp;
pub mod page;
pub mod robots;
pub mod store;
pub mod sync;
