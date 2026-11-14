//! The Fortanix support docs, through the `pmos-docs` MCP server.
//!
//! The docs are public, so no user token is needed. Calls still go through the tool server
//! like any other system: the skill must declare `docs.search` or `docs.read`, every call is
//! audited, and results come back wrapped as untrusted data.

use async_trait::async_trait;
use rmcp::model::{CallToolRequestParams, ClientConfig};
use rmcp::service::RunningService;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::{RoleClient, ServiceExt};
use serde_json::{json, Map, Value};
use tokio::sync::Mutex;

use super::{Caller, Connector, ConnectorError, Item, SourceRef};

type Client = RunningService<RoleClient, ClientConfig>;

pub struct Docs {
    /// `http://docs.pmos.internal:8730/mcp`
    url: String,
    token: String,
    client: Mutex<Option<Client>>,
}

impl Docs {
    pub fn new(url: impl Into<String>, token: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            token: token.into(),
            client: Mutex::new(None),
        }
    }

    async fn connect(&self) -> Result<Client, ConnectorError> {
        let transport = StreamableHttpClientTransport::from_config(
            StreamableHttpClientTransportConfig::with_uri(self.url.clone())
                .auth_header(self.token.clone()),
        );
        ClientConfig::default()
            .serve(transport)
            .await
            .map_err(|e| ConnectorError::Unavailable(format!("support docs: {e}")))
    }

    /// Call a `pmos-docs` tool, reconnecting once if the connection dropped.
    async fn call(
        &self,
        tool: &'static str,
        args: Map<String, Value>,
    ) -> Result<Value, ConnectorError> {
        let mut guard = self.client.lock().await;
        for attempt in 0..2 {
            if guard.as_ref().is_none_or(|c| c.is_closed()) {
                *guard = Some(self.connect().await?);
            }
            let client = guard.as_ref().expect("connected above");
            match client
                .call_tool(CallToolRequestParams::new(tool).with_arguments(args.clone()))
                .await
            {
                Ok(result) if result.is_error == Some(true) => {
                    let message = result
                        .content
                        .iter()
                        .filter_map(|c| c.as_text().map(|t| t.text.clone()))
                        .collect::<Vec<_>>()
                        .join(" ");
                    return Err(if message.contains("isn't in the index") {
                        ConnectorError::NotFound(message)
                    } else if message.contains("unavailable") {
                        ConnectorError::Unavailable(message)
                    } else {
                        ConnectorError::BadArgs(message)
                    });
                }
                Ok(result) => {
                    return result.structured_content.ok_or_else(|| {
                        ConnectorError::Unavailable("support docs returned no data".into())
                    })
                }
                Err(e) if attempt == 0 => {
                    tracing::warn!(error = %e, "support docs call failed; reconnecting");
                    *guard = None;
                }
                Err(e) => return Err(ConnectorError::Unavailable(format!("support docs: {e}"))),
            }
        }
        unreachable!("the loop returns on its second attempt")
    }
}

fn page_item(v: &Value, content: Value) -> Item {
    Item {
        source: SourceRef {
            system: "docs".into(),
            id: v["source_url"].as_str().unwrap_or_default().to_string(),
            url: v["url"].as_str().unwrap_or_default().to_string(),
        },
        title: v["title"].as_str().unwrap_or_default().to_string(),
        content,
    }
}

#[async_trait]
impl Connector for Docs {
    fn system(&self) -> &str {
        "docs"
    }

    async fn read(
        &self,
        _caller: Caller<'_>,
        action: &str,
        args: &Value,
    ) -> Result<Vec<Item>, ConnectorError> {
        let args = args.as_object().cloned().unwrap_or_default();
        match action {
            "search" => {
                let out = self.call("search_docs", args).await?;
                Ok(out["results"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|r| {
                        page_item(
                            r,
                            json!({
                                "product": r["product"],
                                "section": r["section"],
                                "updated": r["updated"],
                                "snippet": r["snippet"],
                            }),
                        )
                    })
                    .collect())
            }
            "read" => {
                let out = self.call("get_doc", args).await?;
                Ok(vec![page_item(
                    &out,
                    json!({
                        "product": out["product"],
                        "updated": out["updated"],
                        "markdown": out["markdown"],
                        "truncated": out["truncated"],
                    }),
                )])
            }
            other => Err(ConnectorError::Unsupported(format!("docs.{other}"))),
        }
    }

    async fn write(
        &self,
        _caller: Caller<'_>,
        action: &str,
        _args: &Value,
    ) -> Result<SourceRef, ConnectorError> {
        Err(ConnectorError::Unsupported(format!(
            "docs.{action}: the support docs are read-only"
        )))
    }
}
