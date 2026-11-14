//! `pmos-docs`: an MCP server over the public Acme support documentation.
//!
//! It keeps a searchable copy of support.acme.com (the `llms.txt` index and each page's
//! markdown) and serves `search_docs`, `get_doc` and `list_products` over MCP. Only the PM
//! OS tool server calls it; the tool server applies allowlists, audit and untrusted-content
//! wrapping before anything reaches an agent or Slack.

pub mod app;
pub mod config;
pub mod mcp;
pub mod page;
pub mod robots;
pub mod store;
pub mod sync;
