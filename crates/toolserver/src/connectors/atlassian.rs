//! Jira and Confluence on Atlassian Cloud, called as the signed-in PM with their own OAuth
//! 2.0 (3LO) token through `api.atlassian.com/ex/{jira|confluence}/{cloudid}`.
//!
//! APIs used (checked against developer.atlassian.com, November 2026):
//! - Jira: `GET /rest/api/3/search/jql` (enhanced JQL search, paged by `nextPageToken`),
//!   `GET /rest/api/3/issue/{key}`, `POST /rest/api/3/issue` (description in ADF).
//! - Confluence: `GET /wiki/rest/api/search?cql=` (v1 CQL search),
//!   `GET /wiki/api/v2/pages/{id}?body-format=storage`, `GET /wiki/api/v2/spaces?keys=`,
//!   `POST /wiki/api/v2/pages` (`status: draft`), `PUT /wiki/api/v2/pages/{id}` (version + 1).

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use super::format::{adf_to_text, markdown_to_adf, markdown_to_storage, storage_to_text};
use super::http::{arg, client, issue_key, key_like, limit, numeric_id, send_json};
use super::{
    clip, Caller, Connector, ConnectorError, Item, SourceRef, DRAFTED_MARKER, MAX_BODY_CHARS,
};

/// Each Jira comment returned to the model, at most.
const MAX_COMMENT_CHARS: usize = 1_000;
use crate::oauth::{AccessToken, TokenSource};
use crate::vault::Provider;

struct Site {
    token: AccessToken,
    /// `{api}/ex/{product}/{cloud_id}`
    base: String,
    /// The site's own URL, for links people click.
    web: String,
}

async fn site(
    tokens: &dyn TokenSource,
    api_base: &str,
    product: &str,
    user: &str,
) -> Result<Site, ConnectorError> {
    let token = tokens.token(user, Provider::Atlassian).await?;
    let cloud_id = token.cloud_id.clone().ok_or_else(|| {
        ConnectorError::Reconnect("no Atlassian site is linked; reconnect Jira in PM OS".into())
    })?;
    let web = token
        .site_url
        .clone()
        .unwrap_or_else(|| format!("https://{cloud_id}.atlassian.net"))
        .trim_end_matches('/')
        .to_string();
    Ok(Site {
        base: format!("{}/ex/{product}/{cloud_id}", api_base.trim_end_matches('/')),
        token,
        web,
    })
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

// ----- Jira ---------------------------------------------------------------------------

pub struct Jira {
    tokens: Arc<dyn TokenSource>,
    api_base: String,
    http: reqwest::Client,
}

impl Jira {
    pub fn new(tokens: Arc<dyn TokenSource>, api_base: impl Into<String>) -> Self {
        Self {
            tokens,
            api_base: api_base.into(),
            http: client(),
        }
    }

    fn issue_item(site: &Site, issue: &Value) -> Item {
        let key = issue["key"].as_str().unwrap_or_default();
        let f = &issue["fields"];
        let mut content = json!({
            "type": f["issuetype"]["name"],
            "status": f["status"]["name"],
            "assignee": f["assignee"]["displayName"],
            "updated": f["updated"],
        });
        if f["description"].is_object() {
            let (text, cut) = clip(&adf_to_text(&f["description"]), MAX_BODY_CHARS);
            content["description"] = json!(text);
            if cut {
                content["truncated"] = json!(true);
            }
        }
        if let Some(parent) = f["parent"]["key"].as_str() {
            content["parent"] = json!(parent);
        }
        if let Some(comments) = f["comment"]["comments"].as_array() {
            content["comments"] = json!(comments
                .iter()
                .rev()
                .take(20)
                .rev()
                .map(|c| json!({
                    "author": c["author"]["displayName"],
                    "created": c["created"],
                    "text": clip(&adf_to_text(&c["body"]), MAX_COMMENT_CHARS).0,
                }))
                .collect::<Vec<_>>());
        }
        Item {
            source: SourceRef {
                system: "jira".into(),
                id: key.to_string(),
                url: format!("{}/browse/{key}", site.web),
            },
            title: format!("{key}: {}", f["summary"].as_str().unwrap_or_default()),
            content,
        }
    }
}

#[async_trait]
impl Connector for Jira {
    fn system(&self) -> &str {
        "jira"
    }

    async fn read(
        &self,
        caller: Caller<'_>,
        action: &str,
        args: &Value,
    ) -> Result<Vec<Item>, ConnectorError> {
        let site = site(self.tokens.as_ref(), &self.api_base, "jira", caller.user).await?;
        match action {
            "search" => {
                let jql = arg(args, "jql")?;
                let max = limit(args, 10, 50).to_string();
                let body = send_json(
                    self.http
                        .get(format!("{}/rest/api/3/search/jql", site.base))
                        .bearer_auth(site.token.token.expose())
                        .query(&[
                            ("jql", jql),
                            ("maxResults", max.as_str()),
                            ("fields", "summary,status,issuetype,assignee,updated"),
                        ]),
                    "Jira",
                    "the search",
                )
                .await?;
                Ok(body["issues"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|i| Self::issue_item(&site, i))
                    .collect())
            }
            "read" => {
                let key = issue_key(arg(args, "key")?)?;
                let issue = send_json(
                    self.http
                        .get(format!("{}/rest/api/3/issue/{key}", site.base))
                        .bearer_auth(site.token.token.expose())
                        .query(&[(
                            "fields",
                            "summary,status,issuetype,assignee,updated,description,comment,parent",
                        )]),
                    "Jira",
                    &format!("issue {key}"),
                )
                .await?;
                Ok(vec![Self::issue_item(&site, &issue)])
            }
            other => Err(ConnectorError::Unsupported(format!("jira.{other}"))),
        }
    }

    async fn write(
        &self,
        caller: Caller<'_>,
        action: &str,
        args: &Value,
    ) -> Result<SourceRef, ConnectorError> {
        if action != "create_issue" {
            return Err(ConnectorError::Unsupported(format!("jira.{action}")));
        }
        let site = site(self.tokens.as_ref(), &self.api_base, "jira", caller.user).await?;
        let project = key_like(arg(args, "project")?, "Jira project key")?;
        let description = format!(
            "{}\n\n{DRAFTED_MARKER} · run {}",
            args["description"].as_str().unwrap_or_default(),
            caller.run_id
        );
        let mut fields = json!({
            "project": {"key": project},
            "issuetype": {"name": arg(args, "issue_type")?},
            "summary": arg(args, "summary")?,
            "description": markdown_to_adf(description.trim()),
        });
        if let Some(epic) = args["epic"].as_str() {
            fields["parent"] = json!({"key": issue_key(epic)?});
        }
        let created = send_json(
            self.http
                .post(format!("{}/rest/api/3/issue", site.base))
                .bearer_auth(site.token.token.expose())
                .json(&json!({ "fields": fields })),
            "Jira",
            "the new issue",
        )
        .await?;
        let key = created["key"]
            .as_str()
            .ok_or_else(|| ConnectorError::Unavailable("Jira returned no issue key".into()))?;
        Ok(SourceRef {
            system: "jira".into(),
            id: key.to_string(),
            url: format!("{}/browse/{key}", site.web),
        })
    }
}

// ----- Confluence ---------------------------------------------------------------------

pub struct Confluence {
    tokens: Arc<dyn TokenSource>,
    api_base: String,
    http: reqwest::Client,
}

impl Confluence {
    pub fn new(tokens: Arc<dyn TokenSource>, api_base: impl Into<String>) -> Self {
        Self {
            tokens,
            api_base: api_base.into(),
            http: client(),
        }
    }

    fn web_url(site: &Site, webui: Option<&str>, id: &str) -> String {
        match webui {
            Some(path) => format!("{}/wiki{path}", site.web),
            None => format!("{}/wiki/pages/viewpage.action?pageId={id}", site.web),
        }
    }

    async fn page(&self, site: &Site, id: &str, body: bool) -> Result<Value, ConnectorError> {
        let mut req = self
            .http
            .get(format!("{}/wiki/api/v2/pages/{id}", site.base))
            .bearer_auth(site.token.token.expose());
        if body {
            req = req.query(&[("body-format", "storage")]);
        }
        send_json(req, "Confluence", &format!("page {id}")).await
    }

    fn footer(caller: Caller<'_>) -> String {
        format!(
            "<p><em>{DRAFTED_MARKER} · run {}</em></p>",
            escape_html(caller.run_id)
        )
    }
}

#[async_trait]
impl Connector for Confluence {
    fn system(&self) -> &str {
        "confluence"
    }

    async fn read(
        &self,
        caller: Caller<'_>,
        action: &str,
        args: &Value,
    ) -> Result<Vec<Item>, ConnectorError> {
        let site = site(
            self.tokens.as_ref(),
            &self.api_base,
            "confluence",
            caller.user,
        )
        .await?;
        match action {
            "search" => {
                let cql = arg(args, "cql")?;
                let max = limit(args, 10, 50).to_string();
                let body = send_json(
                    self.http
                        .get(format!("{}/wiki/rest/api/search", site.base))
                        .bearer_auth(site.token.token.expose())
                        .query(&[("cql", cql), ("limit", max.as_str())]),
                    "Confluence",
                    "the search",
                )
                .await?;
                Ok(body["results"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|r| {
                        let id = r["content"]["id"].as_str()?;
                        Some(Item {
                            source: SourceRef {
                                system: "confluence".into(),
                                id: id.to_string(),
                                url: Self::web_url(&site, r["url"].as_str(), id),
                            },
                            title: r["title"]
                                .as_str()
                                .or(r["content"]["title"].as_str())
                                .unwrap_or_default()
                                .to_string(),
                            content: json!({
                                "type": r["content"]["type"],
                                "excerpt": storage_to_text(r["excerpt"].as_str().unwrap_or_default()),
                                "last_modified": r["lastModified"],
                            }),
                        })
                    })
                    .collect())
            }
            "read" => {
                let id = numeric_id(arg(args, "page_id")?, "Confluence page id")?;
                let page = self.page(&site, id, true).await?;
                Ok(vec![Item {
                    source: SourceRef {
                        system: "confluence".into(),
                        id: id.to_string(),
                        url: Self::web_url(&site, page["_links"]["webui"].as_str(), id),
                    },
                    title: page["title"].as_str().unwrap_or_default().to_string(),
                    content: {
                        let (body, cut) = clip(
                            &storage_to_text(
                                page["body"]["storage"]["value"]
                                    .as_str()
                                    .unwrap_or_default(),
                            ),
                            MAX_BODY_CHARS,
                        );
                        json!({
                            "status": page["status"],
                            "version": page["version"]["number"],
                            "body": body,
                            "truncated": cut,
                        })
                    },
                }])
            }
            other => Err(ConnectorError::Unsupported(format!("confluence.{other}"))),
        }
    }

    async fn write(
        &self,
        caller: Caller<'_>,
        action: &str,
        args: &Value,
    ) -> Result<SourceRef, ConnectorError> {
        let site = site(
            self.tokens.as_ref(),
            &self.api_base,
            "confluence",
            caller.user,
        )
        .await?;
        match action {
            "create_draft" => {
                let key = key_like(arg(args, "space")?, "Confluence space key")?;
                let spaces = send_json(
                    self.http
                        .get(format!("{}/wiki/api/v2/spaces", site.base))
                        .bearer_auth(site.token.token.expose())
                        .query(&[("keys", key)]),
                    "Confluence",
                    &format!("space {key}"),
                )
                .await?;
                let space_id = spaces["results"][0]["id"]
                    .as_str()
                    .map(String::from)
                    .or_else(|| spaces["results"][0]["id"].as_u64().map(|n| n.to_string()))
                    .ok_or_else(|| ConnectorError::NotFound(format!("Confluence space {key}")))?;
                let mut body = json!({
                    "spaceId": space_id,
                    "status": "draft",
                    "title": arg(args, "title")?,
                    "body": {
                        "representation": "storage",
                        "value": format!("{}{}", markdown_to_storage(arg(args, "body")?), Self::footer(caller)),
                    },
                });
                if let Some(parent) = args["parent_id"].as_str() {
                    body["parentId"] = json!(numeric_id(parent, "Confluence page id")?);
                }
                let created = send_json(
                    self.http
                        .post(format!("{}/wiki/api/v2/pages", site.base))
                        .bearer_auth(site.token.token.expose())
                        .json(&body),
                    "Confluence",
                    "the new page",
                )
                .await?;
                let id = created["id"].as_str().ok_or_else(|| {
                    ConnectorError::Unavailable("Confluence returned no page id".into())
                })?;
                Ok(SourceRef {
                    system: "confluence".into(),
                    id: id.to_string(),
                    url: Self::web_url(&site, created["_links"]["webui"].as_str(), id),
                })
            }
            "update_page" => {
                let id = numeric_id(arg(args, "page_id")?, "Confluence page id")?;
                let current = self.page(&site, id, false).await?;
                let version = current["version"]["number"].as_u64().ok_or_else(|| {
                    ConnectorError::Unavailable(format!("Confluence page {id} has no version"))
                })?;
                let message = args["version_comment"]
                    .as_str()
                    .unwrap_or("Updated with PM OS");
                let updated = send_json(
                    self.http
                        .put(format!("{}/wiki/api/v2/pages/{id}", site.base))
                        .bearer_auth(site.token.token.expose())
                        .json(&json!({
                            "id": id,
                            // Keep the page's status: updating a draft PM OS made must not
                            // publish it.
                            "status": current["status"].as_str().unwrap_or("current"),
                            "title": current["title"],
                            "body": {
                                "representation": "storage",
                                "value": format!("{}{}", markdown_to_storage(arg(args, "body")?), Self::footer(caller)),
                            },
                            "version": {"number": version + 1, "message": message},
                        })),
                    "Confluence",
                    &format!("page {id}"),
                )
                .await
                .map_err(|e| match e {
                    // Someone saved a newer version after PM OS read it.
                    ConnectorError::BadArgs(m) if m.to_lowercase().contains("version") => ConnectorError::BadArgs(format!(
                        "page {id} changed since PM OS read it; read it again and redo the update ({m})"
                    )),
                    other => other,
                })?;
                Ok(SourceRef {
                    system: "confluence".into(),
                    id: id.to_string(),
                    url: Self::web_url(&site, updated["_links"]["webui"].as_str(), id),
                })
            }
            other => Err(ConnectorError::Unsupported(format!("confluence.{other}"))),
        }
    }
}
