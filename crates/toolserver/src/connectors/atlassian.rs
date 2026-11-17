//! Jira and Confluence on Atlassian Cloud, called as the signed-in PM with their own OAuth
//! 2.0 (3LO) token through `api.atlassian.com/ex/{jira|confluence}/{cloudid}`. A team bot's
//! reads use its own account instead, inside its read scope (`scope.rs`).
//!
//! APIs used (checked against developer.atlassian.com, November 2026):
//! - Jira: `GET /rest/api/3/search/jql` (enhanced JQL search, paged by `nextPageToken`;
//!   `jira.rollup` follows the pages),
//!   `GET /rest/api/3/issue/{key}`, `POST /rest/api/3/issue` (description in ADF).
//! - Confluence: `GET /wiki/rest/api/search?cql=` (v1 CQL search),
//!   `GET /wiki/api/v2/pages/{id}?body-format=storage`, `GET /wiki/api/v2/spaces?keys=`,
//!   `POST /wiki/api/v2/pages` (`status: draft`), `PUT /wiki/api/v2/pages/{id}` (version + 1,
//!   only when the page is still at the `version` the call names; a `409` means the same).

use std::sync::Arc;

use async_trait::async_trait;
use chrono::NaiveDate;
use serde_json::{json, Value};

use super::format::{adf_to_text, markdown_to_adf, markdown_to_storage, storage_to_text};
use super::http::{
    arg, client, decode_json, issue_key, key_like, limit, numeric_id, send, send_json,
};
use super::rollup;
use super::{
    clip, Caller, Connector, ConnectorError, Item, SourceRef, TeamBot, DRAFTED_MARKER,
    MAX_BODY_CHARS, PAGE_CHANGED,
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

/// An Atlassian id, which some endpoints send as a string and some as a number.
fn json_id(v: &Value) -> Option<String> {
    v.as_str()
        .map(String::from)
        .or_else(|| v.as_u64().map(|n| n.to_string()))
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
    /// How long one `jira.rollup` may take in all.
    rollup_budget: std::time::Duration,
    /// "Today" for `overdue`, fixed for tests; `None` is the real date (UTC).
    today: Option<NaiveDate>,
}

impl Jira {
    pub fn new(tokens: Arc<dyn TokenSource>, api_base: impl Into<String>) -> Self {
        Self {
            tokens,
            api_base: api_base.into(),
            http: client(),
            rollup_budget: rollup::DEFAULT_BUDGET,
            today: None,
        }
    }

    /// Give `jira.rollup` this long in all, instead of 45 seconds (for tests).
    pub fn with_rollup_budget(mut self, budget: std::time::Duration) -> Self {
        self.rollup_budget = budget;
        self
    }

    /// Count `overdue` as of `today` instead of the real date (for tests).
    pub fn with_today(mut self, today: NaiveDate) -> Self {
        self.today = Some(today);
        self
    }

    /// `jira.rollup`: page through the search, 100 issues at a time, following
    /// `nextPageToken`, until the last page, `max_issues`, or the time budget.
    async fn rollup(&self, site: &Site, jql: &str, args: &Value) -> Result<Item, ConnectorError> {
        let (group_by, max) = rollup::options(args)?;
        let deadline = tokio::time::Instant::now() + self.rollup_budget;
        let mut issues = Vec::new();
        let mut token: Option<String> = None;
        let mut truncated = None;
        loop {
            let want = (max - issues.len()).min(rollup::PAGE_SIZE).to_string();
            let mut query = vec![
                ("jql", jql.to_string()),
                ("maxResults", want),
                ("fields", rollup::FIELDS.to_string()),
            ];
            if let Some(t) = &token {
                query.push(("nextPageToken", t.clone()));
            }
            let request = send_json(
                self.http
                    .get(format!("{}/rest/api/3/search/jql", site.base))
                    .bearer_auth(site.token.token.expose())
                    .query(&query),
                "Jira",
                "the rollup's search",
            );
            let page = match tokio::time::timeout_at(deadline, request).await {
                Ok(page) => page?,
                Err(_) => {
                    truncated = Some(rollup::Truncated::Time);
                    break;
                }
            };
            let found = page["issues"].as_array().map_or(&[][..], Vec::as_slice);
            let room = max - issues.len();
            issues.extend(
                found
                    .iter()
                    .take(room)
                    .filter_map(rollup::RollupIssue::from_jira),
            );
            let next = page["nextPageToken"]
                .as_str()
                .filter(|t| !t.is_empty() && page["isLast"].as_bool() != Some(true))
                .map(String::from);
            if found.len() >= room {
                if found.len() > room || next.is_some() {
                    truncated = Some(rollup::Truncated::Cap);
                }
                break;
            }
            // An empty page with a token would loop for ever.
            match next {
                Some(t) if !found.is_empty() => token = Some(t),
                _ => break,
            }
            if tokio::time::Instant::now() >= deadline {
                truncated = Some(rollup::Truncated::Time);
                break;
            }
        }
        let today = self
            .today
            .unwrap_or_else(|| chrono::Utc::now().date_naive());
        let content = rollup::rollup(&issues, group_by, today, truncated);
        let url =
            reqwest::Url::parse_with_params(&format!("{}/issues/", site.web), &[("jql", jql)])
                .map(String::from)
                .unwrap_or_else(|_| format!("{}/issues/", site.web));
        Ok(rollup::item(content, url))
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
        // A team bot's scope is checked before its token is fetched or anything is sent.
        let jql = match (action, caller.team_bot) {
            ("search" | "rollup", Some(bot)) => Some(bot.jql(arg(args, "jql")?)?),
            ("search" | "rollup", None) => Some(arg(args, "jql")?.to_string()),
            _ => None,
        };
        if let (Some(bot), "read") = (caller.team_bot, action) {
            bot.check_issue(issue_key(arg(args, "key")?)?)?;
        }
        let site = site(
            self.tokens.as_ref(),
            &self.api_base,
            "jira",
            caller.reader(),
        )
        .await?;
        match action {
            "rollup" => Ok(vec![
                self.rollup(&site, &jql.unwrap_or_default(), args).await?,
            ]),
            "search" => {
                let jql = jql.unwrap_or_default();
                let max = limit(args, 10, 50).to_string();
                let body = send_json(
                    self.http
                        .get(format!("{}/rest/api/3/search/jql", site.base))
                        .bearer_auth(site.token.token.expose())
                        .query(&[
                            ("jql", jql.as_str()),
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
                // A moved issue comes back under its new key: check where it is now.
                if let Some(bot) = caller.team_bot {
                    bot.check_issue(issue["key"].as_str().unwrap_or_default())?;
                }
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

    /// Is `page` in one of the team bot's spaces? The v2 page names its space by id, so the
    /// listed keys are looked up (`GET /wiki/api/v2/spaces?keys=`) and compared by id. A page
    /// with no space id is refused.
    async fn check_page_space(
        &self,
        site: &Site,
        bot: TeamBot<'_>,
        id: &str,
        page: &Value,
    ) -> Result<(), ConnectorError> {
        let spaces = bot.confluence_spaces()?;
        let page_space = json_id(&page["spaceId"]);
        let listed = send_json(
            self.http
                .get(format!("{}/wiki/api/v2/spaces", site.base))
                .bearer_auth(site.token.token.expose())
                .query(&[("keys", spaces.join(",").as_str()), ("limit", "250")]),
            "Confluence",
            "the bot's spaces",
        )
        .await?;
        let allowed = listed["results"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|s| {
                s["key"]
                    .as_str()
                    .is_some_and(|k| spaces.iter().any(|w| w == k))
            })
            .filter_map(|s| json_id(&s["id"]));
        match page_space {
            Some(space) if allowed.into_iter().any(|a| a == space) => Ok(()),
            _ => Err(bot.outside_spaces(id, spaces)),
        }
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
        // A team bot's scope is checked before its token is fetched or anything is sent.
        let cql = match (action, caller.team_bot) {
            ("search", Some(bot)) => Some(bot.cql(arg(args, "cql")?)?),
            ("search", None) => Some(arg(args, "cql")?.to_string()),
            _ => None,
        };
        if let (Some(bot), "read") = (caller.team_bot, action) {
            bot.confluence_spaces()?;
        }
        let site = site(
            self.tokens.as_ref(),
            &self.api_base,
            "confluence",
            caller.reader(),
        )
        .await?;
        match action {
            "search" => {
                let cql = cql.unwrap_or_default();
                let max = limit(args, 10, 50).to_string();
                let body = send_json(
                    self.http
                        .get(format!("{}/wiki/rest/api/search", site.base))
                        .bearer_auth(site.token.token.expose())
                        .query(&[("cql", cql.as_str()), ("limit", max.as_str())]),
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
                // After the fetch, before anything is returned: the page's space must be one
                // the team bot may read.
                if let Some(bot) = caller.team_bot {
                    self.check_page_space(&site, bot, id, &page).await?;
                }
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
                let read = args["version"]
                    .as_u64()
                    .filter(|v| *v >= 1)
                    .ok_or_else(|| {
                        ConnectorError::BadArgs(
                        "`version` is required: the page's version as confluence_read returned it"
                            .into(),
                    )
                    })?;
                let current = self.page(&site, id, false).await?;
                let version = current["version"]["number"].as_u64().ok_or_else(|| {
                    ConnectorError::Unavailable(format!("Confluence page {id} has no version"))
                })?;
                // Someone saved a newer version after PM OS read it: refused before anything
                // that changes the page is sent.
                if version != read {
                    return Err(ConnectorError::BadArgs(PAGE_CHANGED.into()));
                }
                let message = args["version_comment"]
                    .as_str()
                    .unwrap_or("Updated with PM OS");
                let res = send(
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
                            "version": {"number": read + 1, "message": message},
                        })),
                    "Confluence",
                )
                .await?;
                // Someone saved in between the check and the write.
                if res.status() == reqwest::StatusCode::CONFLICT {
                    return Err(ConnectorError::BadArgs(PAGE_CHANGED.into()));
                }
                let updated = decode_json(res, "Confluence", &format!("page {id}")).await?;
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
