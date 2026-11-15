//! Your company's public docs, through the `pmos-docs` MCP server.
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
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;

use super::{Caller, Connector, ConnectorError, Item, SourceRef};

type Client = RunningService<RoleClient, ClientConfig>;

/// How long one docs call may take, connecting included.
const CALL_TIMEOUT: Duration = Duration::from_secs(10);

pub struct Docs {
    /// `http://docs.pmos.internal:8730/mcp`
    url: String,
    token: String,
    http: reqwest::Client,
    /// The live connection. The lock is held only to read or replace it, never across a
    /// call, so calls from many sessions run concurrently and one slow call blocks no one.
    client: Mutex<Option<Arc<Client>>>,
    timeout: Duration,
}

impl Docs {
    pub fn new(url: impl Into<String>, token: impl Into<String>) -> Self {
        Self::with_timeout(url, token, CALL_TIMEOUT)
    }

    pub fn with_timeout(
        url: impl Into<String>,
        token: impl Into<String>,
        timeout: Duration,
    ) -> Self {
        Self {
            url: url.into(),
            token: token.into(),
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(3))
                .build()
                .expect("http client builds"),
            client: Mutex::new(None),
            timeout,
        }
    }

    async fn connect(&self) -> Result<Client, ConnectorError> {
        let transport = StreamableHttpClientTransport::with_client(
            self.http.clone(),
            StreamableHttpClientTransportConfig::with_uri(self.url.clone())
                .auth_header(self.token.clone()),
        );
        ClientConfig::default()
            .serve(transport)
            .await
            .map_err(|e| ConnectorError::Unavailable(format!("docs server: {e}")))
    }

    /// The live client, connecting if there is none.
    async fn client(&self) -> Result<Arc<Client>, ConnectorError> {
        let mut guard = self.client.lock().await;
        if let Some(c) = guard.as_ref().filter(|c| !c.is_closed()) {
            return Ok(c.clone());
        }
        let c = Arc::new(self.connect().await?);
        *guard = Some(c.clone());
        Ok(c)
    }

    /// Drop `broken`, unless another call already replaced it.
    async fn forget(&self, broken: &Arc<Client>) {
        let mut guard = self.client.lock().await;
        if guard.as_ref().is_some_and(|c| Arc::ptr_eq(c, broken)) {
            *guard = None;
        }
    }

    /// Call a `pmos-docs` tool, reconnecting once if the connection dropped. Each attempt
    /// is bounded by the timeout.
    async fn call(
        &self,
        tool: &'static str,
        args: Map<String, Value>,
    ) -> Result<Value, ConnectorError> {
        for attempt in 0..2 {
            let attempt_result = tokio::time::timeout(self.timeout, async {
                let client = self.client().await?;
                let result = client
                    .call_tool(CallToolRequestParams::new(tool).with_arguments(args.clone()))
                    .await;
                Ok::<_, ConnectorError>((client, result))
            })
            .await;
            let (client, result) = match attempt_result {
                Ok(Ok(pair)) => pair,
                Ok(Err(e)) => return Err(e),
                Err(_) => {
                    return Err(ConnectorError::Unavailable(format!(
                        "the docs server didn't answer within {} s",
                        self.timeout.as_secs()
                    )))
                }
            };
            match result {
                Ok(result) if result.is_error == Some(true) => return Err(tool_failure(&result)),
                Ok(result) => {
                    return result.structured_content.ok_or_else(|| {
                        ConnectorError::Unavailable("the docs server returned no data".into())
                    })
                }
                Err(e) if attempt == 0 => {
                    tracing::warn!(error = %e, "docs server call failed; reconnecting");
                    self.forget(&client).await;
                }
                Err(e) => return Err(ConnectorError::Unavailable(format!("docs server: {e}"))),
            }
        }
        unreachable!("the loop returns on its second attempt")
    }
}

/// `pmos-docs` reports failures as `{code, message}`; map the code, not the wording.
fn tool_failure(result: &rmcp::model::CallToolResult) -> ConnectorError {
    let failure = result.structured_content.clone().unwrap_or_default();
    let message = failure["message"]
        .as_str()
        .map(String::from)
        .unwrap_or_else(|| {
            result
                .content
                .iter()
                .filter_map(|c| c.as_text().map(|t| t.text.clone()))
                .collect::<Vec<_>>()
                .join(" ")
        });
    match failure["code"].as_str() {
        Some("not_found") => ConnectorError::NotFound(message),
        Some("not_configured") => ConnectorError::NotConfigured(message),
        Some("unavailable") => ConnectorError::Unavailable(message),
        _ => ConnectorError::BadArgs(message),
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
                // No docs site is configured: the server says so in a note.
                if let Some(note) = out["note"].as_str() {
                    if out["results"].as_array().is_none_or(|r| r.is_empty()) {
                        return Err(ConnectorError::NotConfigured(note.to_string()));
                    }
                }
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
            "docs.{action}: the docs are read-only"
        )))
    }

    async fn ready(&self) -> Result<(), String> {
        match tokio::time::timeout(Duration::from_secs(2), self.client()).await {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(e)) => Err(e.to_string()),
            Err(_) => Err("the docs server didn't answer within 2 s".into()),
        }
    }
}
