//! MCP server records (`content/mcp/<id>.yaml`, `docs/mcp-servers-spec.md` §2.2).
//!
//! A server is usable only through a reviewed record: one file per server, approved by an
//! Admin, reaching the content repository by pull request like every content change. A tool is
//! usable only when its exact upstream name and the digest of its definition are listed here;
//! everything else a server offers is ignored. This module holds the record's format and the
//! rules its shape must follow; the tool server (`crates/toolserver/src/mcp_client/`) enforces
//! the rest when it connects.
//!
//! Where the spec left a choice, these rules take the more refusing one:
//! - `transport` must be `streamable_http`; `stdio` and the legacy `sse` are refused in every
//!   record (test fakes are built in code, never records).
//! - A remote server is `https://` at a DNS name (no IP literal, no wildcard, no user info), and
//!   its `identity.host` must be that name. `http://` is allowed only for an internal server at
//!   `<name>.pmos.internal` run by PM OS (`operator: pmos`) with a pinned `identity.address`.
//! - `oauth.resource` must be the server's exact URL; every OAuth endpoint is `https://` at a
//!   DNS name.
//! - Every requested scope must be needed by an approved tool, and a scope whose name contains
//!   `write` must be needed by a write tool.
//! - Each tool carries its own `approved_by` as well as the record's (the owner: "an Admin
//!   approves a server and each tool").
//! - A `third_party` record needs `owner_approval`; the tool server refuses every
//!   `third_party` record for now (where the owner's approval is kept is not decided).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::{Sensitivity, ToolKind, ToolRegistry};

/// The records' directory under the content root.
pub const MCP_DIR: &str = "mcp";

/// Registry systems served by PM OS itself, never by a server.
pub const LOCAL_SYSTEMS: [&str; 4] = ["context", "memory", "stats", "bets"];

/// Who runs the server (§2.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operator {
    /// PM OS runs it, on the PM OS network.
    Pmos,
    /// The vendor of the system of record (data goes where it already is).
    SystemVendor,
    /// Anyone else: needs the owner's recorded approval.
    ThirdParty,
}

impl Operator {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pmos => "pmos",
            Self::SystemVendor => "system_vendor",
            Self::ThirdParty => "third_party",
        }
    }
}

/// How the tool server reaches the server. Only `streamable_http` is accepted; the other two
/// parse so the lint can say why they are refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    StreamableHttp,
    Stdio,
    Sse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthKind {
    /// MCP authorization: OAuth 2.1 with PKCE, one token per PM, in the vault.
    OauthPerUser,
    /// PM OS's own servers: one bearer token held by the tool server.
    ServiceToken,
    /// Public data only.
    None,
}

impl AuthKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OauthPerUser => "oauth_per_user",
            Self::ServiceToken => "service_token",
            Self::None => "none",
        }
    }
}

/// Who the server is, beyond its URL (§2.3, threat 6).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    /// A remote server's TLS host name: must be the URL's host.
    #[serde(default)]
    pub host: Option<String>,
    /// Pinned SHA-256 hashes of the server certificate's public key (SPKI), `sha256:<hex>`.
    /// When listed, the certificate must match one, after the usual checks.
    #[serde(default)]
    pub spki_sha256: Vec<String>,
    /// An internal server's pinned IP address: the name is never looked up.
    #[serde(default)]
    pub address: Option<String>,
}

/// A pre-registered OAuth client (§2.7). Dynamic client registration is refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OAuthSettings {
    /// The client id an Admin registered with the authorization server (not a secret).
    pub client_id: String,
    /// The environment variable that holds the client secret, if the client has one: never the
    /// secret itself. `PMOS_MCP_CLIENT_SECRET_<…>`.
    #[serde(default)]
    pub client_secret_env: Option<String>,
    /// The authorization server's issuer, exactly as its metadata names it.
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    /// The exact scopes requested.
    pub scopes: Vec<String>,
    /// The `resource` indicator that binds tokens to this server: its exact URL.
    pub resource: String,
}

/// How a linked account is matched to the PM: an approved tool on the same server whose result
/// names the account's email.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdentityCheck {
    /// The upstream tool ("who am I").
    pub tool: String,
    /// Its approved definition digest.
    pub digest: String,
    /// Where the email is in its structured result, a dotted path such as `user.email`.
    pub email: String,
}

/// A tool's class, from the record only: an upstream hint never decides it (§2.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolClass {
    Read,
    Write,
}

/// How PM OS's approved arguments become the upstream arguments (§2.5): a pure function of
/// them and fixed values.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArgsMap {
    /// PM OS argument names sent under the same name.
    #[serde(default)]
    pub pass: Vec<String>,
    /// PM OS argument name to upstream name (a dotted path nests it, such as `fields.summary`).
    #[serde(default)]
    pub rename: BTreeMap<String, String>,
    /// PM OS arguments deliberately not sent.
    #[serde(default)]
    pub drop: Vec<String>,
    /// Upstream arguments with fixed values.
    #[serde(default)]
    pub fixed: BTreeMap<String, Value>,
    /// An upstream argument (after renaming) that gets "Drafted with PM OS · run <run id>"
    /// appended: the marker every write carries (SLK-7, OUT-1).
    #[serde(default)]
    pub marker: Option<String>,
}

/// Where an item's fields are in the structured result (dotted paths).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResultMap {
    /// The array of items; absent with `item` absent means the whole result is one item.
    #[serde(default)]
    pub items: Option<String>,
    /// One item at this path.
    #[serde(default)]
    pub item: Option<String>,
    /// The item's id (required for an item).
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    /// The item's content: a path to an object taken whole, or output field to path.
    #[serde(default)]
    pub content: Option<ContentMap>,
    /// With no items, a text at this path means "not set up here" (a note, not an error).
    #[serde(default)]
    pub empty_note: Option<String>,
    /// Paging, for an adapter that follows pages: the next page's token, and "last page".
    #[serde(default)]
    pub next_token: Option<String>,
    #[serde(default)]
    pub last: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ContentMap {
    Whole(String),
    Fields(BTreeMap<String, String>),
}

/// The error a tool failure maps to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    NotFound,
    BadArgs,
    Unavailable,
    NotConfigured,
    Forbidden,
    Reconnect,
    RateLimited,
}

/// How a failed call's structured result names its error.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ErrorMap {
    /// Path to the error code.
    #[serde(default)]
    pub code: Option<String>,
    /// Path to the message (clipped and shown as untrusted).
    #[serde(default)]
    pub message: Option<String>,
    /// Upstream code to error.
    #[serde(default)]
    pub codes: BTreeMap<String, ErrorKind>,
    /// For a code not listed, or none: default `unavailable`.
    #[serde(default)]
    pub default: Option<ErrorKind>,
}

/// PM OS-side logic over a mapped tool, reviewed as code (§2.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Adapter {
    /// `jira.rollup` computed by PM OS over the mapped search, following pages.
    JiraRollup,
    /// A Confluence write (slice M8): the marker argument (the page body) is sent as Confluence
    /// storage, its markdown converted and HTML-escaped by PM OS, with the "Drafted with PM OS"
    /// footer the built-in connector adds. Required on `confluence.create_draft` and
    /// `confluence.update_page`.
    ConfluenceStorage,
    /// A Slack post (slice M8): the marker argument (the text) gets the marker the built-in
    /// connector adds (SLK-7). Required on `slack.post_reply` and `slack.send_message`.
    SlackText,
}

impl Adapter {
    /// The registry tools this adapter may serve, and that must use it.
    pub fn tools(self) -> &'static [&'static str] {
        match self {
            Self::JiraRollup => &["jira.rollup"],
            Self::ConfluenceStorage => &["confluence.create_draft", "confluence.update_page"],
            Self::SlackText => &["slack.post_reply", "slack.send_message"],
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::JiraRollup => "jira_rollup",
            Self::ConfluenceStorage => "confluence_storage",
            Self::SlackText => "slack_text",
        }
    }
}

/// What a check tool tells PM OS (slice M8). A check is never listed to a bot and has no
/// registry name: the tool server calls it itself, with the PM's own token, for a rule it
/// enforces (Slack's audience check before a post, `docs/slack-reply-spec.md` §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckRole {
    /// A Slack conversation's kind, name and sharing (as `conversations.info`).
    SlackConversation,
    /// A Slack conversation's members (as `conversations.members`), page by page.
    SlackMembers,
}

impl CheckRole {
    pub const ALL: [CheckRole; 2] = [Self::SlackConversation, Self::SlackMembers];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::SlackConversation => "slack_conversation",
            Self::SlackMembers => "slack_members",
        }
    }

    /// The registry system it serves.
    pub fn system(self) -> &'static str {
        "slack"
    }

    /// The arguments PM OS gives it, each to be passed, renamed or dropped by the record.
    /// `channel` is always sent.
    pub fn args(self) -> &'static [&'static str] {
        match self {
            Self::SlackConversation => &["channel"],
            Self::SlackMembers => &["channel", "limit", "cursor"],
        }
    }

    /// The fields of `result.content` a conversation check names (paths inside its item): the
    /// first three are required, so a channel's sharing is never guessed.
    pub const CONVERSATION_FIELDS: [&'static str; 7] = [
        "external", "shared", "im", "mpim", "private", "name", "user",
    ];
    pub const CONVERSATION_REQUIRED: [&'static str; 3] = ["external", "shared", "im"];
}

/// An approved check tool on a server (slice M8): the same approval as a mapped tool (exact
/// name, digest, its own `approved_by`, explicitly `class: read`), but no registry name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckTool {
    pub role: CheckRole,
    /// The upstream tool's exact name.
    pub upstream: String,
    /// `sha256:<hex>` of the upstream definition's canonical JSON.
    pub digest: String,
    /// Must be `read`: absent would mean write, and a check never writes.
    #[serde(default)]
    pub class: Option<ToolClass>,
    pub approved_by: String,
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default)]
    pub args: ArgsMap,
    /// Conversation: `item` (the conversation object) and `content` (field → path in it).
    /// Members: `items` (the member ids) and `next_token` (the next page's cursor).
    #[serde(default)]
    pub result: ResultMap,
    #[serde(default)]
    pub errors: ErrorMap,
}

impl CheckTool {
    /// The check as a mapping, for the client's call path (its `registry` is a label only:
    /// `check:<role>`, which no registry name can be).
    pub fn as_mapping(&self) -> ToolMapping {
        ToolMapping {
            registry: format!("check:{}", self.role.as_str()),
            upstream: self.upstream.clone(),
            digest: self.digest.clone(),
            class: self.class,
            approved_by: self.approved_by.clone(),
            scopes: self.scopes.clone(),
            args: self.args.clone(),
            result: self.result.clone(),
            errors: self.errors.clone(),
            adapter: None,
        }
    }
}

/// One approved tool: a registry tool mapped to one upstream tool.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolMapping {
    /// The registry name it serves (must be in `tools.yaml`).
    pub registry: String,
    /// The upstream tool's exact name.
    pub upstream: String,
    /// `sha256:<hex>` of the upstream definition's canonical JSON.
    pub digest: String,
    /// Absent means write.
    #[serde(default)]
    pub class: Option<ToolClass>,
    /// The Admin who approved this tool.
    pub approved_by: String,
    /// The OAuth scopes the tool needs (for `oauth_per_user`).
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default)]
    pub args: ArgsMap,
    #[serde(default)]
    pub result: ResultMap,
    #[serde(default)]
    pub errors: ErrorMap,
    #[serde(default)]
    pub adapter: Option<Adapter>,
}

impl ToolMapping {
    /// Unclassified is a write.
    pub fn class(&self) -> ToolClass {
        self.class.unwrap_or(ToolClass::Write)
    }
}

fn internal() -> Sensitivity {
    Sensitivity::Internal
}

/// `content/mcp/<id>.yaml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerRecord {
    pub id: String,
    pub operator: Operator,
    pub transport: Transport,
    pub url: String,
    #[serde(default)]
    pub identity: Identity,
    pub auth: AuthKind,
    #[serde(default)]
    pub oauth: Option<OAuthSettings>,
    #[serde(default)]
    pub identity_check: Option<IdentityCheck>,
    /// The highest class its reads return.
    #[serde(default = "internal")]
    pub data_class: Sensitivity,
    /// Registry systems it serves.
    pub systems: Vec<String>,
    pub tools: Vec<ToolMapping>,
    /// Approved check tools PM OS calls itself, never listed to a bot (slice M8).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<CheckTool>,
    /// The Admin who approved the server.
    pub approved_by: String,
    /// The pull request that added it.
    pub pull: String,
    /// For `third_party`: the reference of the owner's recorded approval.
    #[serde(default)]
    pub owner_approval: Option<String>,
    /// For `service_token`: the environment variable holding the bearer token
    /// (`PMOS_MCP_TOKEN_<…>`, or `PMOS_DOCS_TOKEN` for `pmos-docs`). Never the token.
    #[serde(default)]
    pub secret_env: Option<String>,
    /// A self-hosted server's own egress (hosts it reaches), listed for review.
    #[serde(default)]
    pub egress: Vec<String>,
}

impl ServerRecord {
    pub fn tool(&self, registry: &str) -> Option<&ToolMapping> {
        self.tools.iter().find(|t| t.registry == registry)
    }

    /// The approved check for `role`, if the record has one.
    pub fn check(&self, role: CheckRole) -> Option<&CheckTool> {
        self.checks.iter().find(|c| c.role == role)
    }

    /// The check for `role` only if it is explicitly `class: read`: the lint refuses any
    /// other, and the tool server never calls one that isn't (a check never writes).
    pub fn approved_check(&self, role: CheckRole) -> Option<&CheckTool> {
        self.check(role)
            .filter(|c| c.class == Some(ToolClass::Read))
    }

    /// Does the record approve everything Slack's audience check needs (a conversation's
    /// sharing and its members), each as a read? Without both, Slack posts stay on the
    /// built-in connector.
    pub fn slack_audience(&self) -> bool {
        self.approved_check(CheckRole::SlackConversation).is_some()
            && self.approved_check(CheckRole::SlackMembers).is_some()
    }

    /// Is this a server PM OS runs on its own network (`http://<name>.pmos.internal`)?
    pub fn internal(&self) -> bool {
        parse_url(&self.url).is_ok_and(|u| !u.https)
    }

    /// The hosts the tool server may connect to for this record: the server and, with OAuth,
    /// the authorization server's endpoints (§5).
    pub fn hosts(&self) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        if let Ok(u) = parse_url(&self.url) {
            out.insert(u.host);
        }
        if let Some(o) = &self.oauth {
            for url in [&o.issuer, &o.authorization_endpoint, &o.token_endpoint] {
                if let Ok(u) = parse_url(url) {
                    out.insert(u.host);
                }
            }
        }
        out
    }
}

/// A URL split the strict way these records need.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedUrl {
    pub https: bool,
    pub host: String,
    pub port: Option<u16>,
    pub path: String,
}

/// Parse `http(s)://host[:port][/path]`: no user info, query, fragment, wildcard or
/// whitespace; the host lowercase.
pub fn parse_url(url: &str) -> Result<ParsedUrl, String> {
    let (https, rest) = if let Some(r) = url.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = url.strip_prefix("http://") {
        (false, r)
    } else {
        return Err(format!("`{url}` is not an http:// or https:// URL"));
    };
    if url.chars().any(|c| c.is_whitespace() || c.is_control())
        || rest.contains(['@', '?', '#', '*', '\\'])
    {
        return Err(format!(
            "`{url}` has user info, a query, a fragment, a wildcard or white space"
        ));
    }
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], rest[i..].to_string()),
        None => (rest, "/".to_string()),
    };
    if path.contains("/../") || path.ends_with("/..") || path.contains("/./") {
        return Err(format!("`{url}` has a relative path segment"));
    }
    let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
        let (h, after) = v6
            .split_once(']')
            .ok_or_else(|| format!("`{url}` has a bad IPv6 address"))?;
        let port = match after.strip_prefix(':') {
            Some(p) => Some(p.parse().map_err(|_| format!("`{url}` has a bad port"))?),
            None if after.is_empty() => None,
            None => return Err(format!("`{url}` has a bad host")),
        };
        (h.to_string(), port)
    } else {
        match authority.rsplit_once(':') {
            Some((h, p)) => (
                h.to_string(),
                Some(p.parse().map_err(|_| format!("`{url}` has a bad port"))?),
            ),
            None => (authority.to_string(), None),
        }
    };
    if host.is_empty() || host != host.to_ascii_lowercase() {
        return Err(format!("`{url}` must have a lowercase host"));
    }
    Ok(ParsedUrl {
        https,
        host,
        port,
        path,
    })
}

/// Is `host` an IP literal (v4 or v6)?
pub fn is_ip_literal(host: &str) -> bool {
    host.parse::<std::net::IpAddr>().is_ok()
}

/// A DNS name: labels of letters, digits and `-`, at least two labels.
fn dns_name(host: &str) -> bool {
    let labels: Vec<&str> = host.split('.').collect();
    labels.len() >= 2
        && host.len() <= 253
        && labels.iter().all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        })
}

/// `sha256:` and 64 lowercase hex digits.
pub fn valid_digest(d: &str) -> bool {
    d.strip_prefix("sha256:").is_some_and(|h| {
        h.len() == 64
            && h.chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
    })
}

fn valid_id(id: &str) -> bool {
    let mut chars = id.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
        && id.len() <= 40
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn valid_upstream(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/'))
}

fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && path.split('.').all(|seg| {
            !seg.is_empty() && seg.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
}

fn valid_env(name: &str, prefixes: &[&str]) -> bool {
    prefixes.iter().any(|p| name.starts_with(p))
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// An Admin's handle as it is written in a record (checked against the user database by the
/// tool server).
fn valid_handle(h: &str) -> bool {
    let h = h.trim();
    !h.is_empty()
        && h.len() <= 64
        && h.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !h.to_ascii_uppercase().starts_with("TODO")
}

/// A pull request reference: `https://…/pull/<n>` or `#<n>`.
fn valid_pull(p: &str) -> bool {
    let number = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    p.strip_prefix('#').is_some_and(number)
        || (p.starts_with("https://") && p.rsplit_once("/pull/").is_some_and(|(_, n)| number(n)))
}

/// One problem with a record: a stable code and a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub code: &'static str,
    pub message: String,
}

fn problem(code: &'static str, message: impl Into<String>) -> Problem {
    Problem {
        code,
        message: message.into(),
    }
}

/// Check one record's shape against the registry. `stem` is its file name without `.yaml`.
pub fn validate(record: &ServerRecord, stem: &str, registry: &ToolRegistry) -> Vec<Problem> {
    let mut out = Vec::new();
    let r = record;
    if !valid_id(&r.id) {
        out.push(problem(
            "mcp-id",
            format!(
                "`{}` is not a server id (lowercase letters, digits and `-`, at most 40, starting with a letter)",
                r.id
            ),
        ));
    } else if r.id != stem {
        out.push(problem(
            "mcp-id",
            format!("the record for `{}` must be named `{}.yaml`", r.id, r.id),
        ));
    }
    match r.transport {
        Transport::StreamableHttp => {}
        Transport::Stdio => out.push(problem(
            "mcp-transport",
            "stdio is refused: a stdio server would run inside the tool server's container with its secrets; run it as its own container over streamable_http",
        )),
        Transport::Sse => out.push(problem(
            "mcp-transport",
            "the legacy HTTP+SSE transport is refused; use streamable_http",
        )),
    }
    match parse_url(&r.url) {
        Err(e) => out.push(problem("mcp-url", e)),
        Ok(u) if u.https => {
            if is_ip_literal(&u.host) {
                out.push(problem(
                    "mcp-url",
                    format!(
                        "`{}`: a remote server is named by DNS, never an IP address",
                        r.url
                    ),
                ));
            } else if !dns_name(&u.host) {
                out.push(problem(
                    "mcp-url",
                    format!("`{}` has no valid host name", r.url),
                ));
            } else if u.host.ends_with(".pmos.internal") {
                out.push(problem(
                    "mcp-url",
                    "an internal `.pmos.internal` server is reached over http:// at a pinned address",
                ));
            }
            if r.identity.host.as_deref() != Some(u.host.as_str()) {
                out.push(problem(
                    "mcp-identity",
                    format!("identity.host must be the URL's host, `{}`", u.host),
                ));
            }
            if r.identity.address.is_some() {
                out.push(problem(
                    "mcp-identity",
                    "identity.address is for internal servers only",
                ));
            }
        }
        Ok(u) => {
            if r.operator != Operator::Pmos || !u.host.ends_with(".pmos.internal") {
                out.push(problem(
                    "mcp-url",
                    format!(
                        "`{}`: http:// is only for a server PM OS runs at `<name>.pmos.internal`; every other server is https://",
                        r.url
                    ),
                ));
            }
            match r.identity.address.as_deref() {
                Some(a) if is_ip_literal(a) => {}
                _ => out.push(problem(
                    "mcp-identity",
                    "an internal server needs identity.address, its pinned IP address",
                )),
            }
            if r.auth == AuthKind::OauthPerUser {
                out.push(problem(
                    "mcp-auth",
                    "an internal server uses service_token or none, not per-user OAuth",
                ));
            }
        }
    }
    for pin in &r.identity.spki_sha256 {
        if !valid_digest(pin) {
            out.push(problem(
                "mcp-identity",
                format!("SPKI pin `{pin}` must be `sha256:` and 64 lowercase hex digits"),
            ));
        }
    }
    let scopes_used: BTreeSet<&str> = r
        .tools
        .iter()
        .flat_map(|t| t.scopes.iter().map(String::as_str))
        .chain(
            r.checks
                .iter()
                .flat_map(|c| c.scopes.iter().map(String::as_str)),
        )
        .collect();
    match (r.auth, &r.oauth) {
        (AuthKind::OauthPerUser, None) => out.push(problem(
            "mcp-auth",
            "auth oauth_per_user needs an `oauth` section",
        )),
        (AuthKind::OauthPerUser, Some(o)) => {
            for (what, url) in [
                ("issuer", &o.issuer),
                ("authorization_endpoint", &o.authorization_endpoint),
                ("token_endpoint", &o.token_endpoint),
            ] {
                match parse_url(url) {
                    Ok(u) if u.https && dns_name(&u.host) && !is_ip_literal(&u.host) => {}
                    _ => out.push(problem(
                        "mcp-oauth",
                        format!("oauth.{what} must be an https:// URL at a host name"),
                    )),
                }
            }
            if o.resource != r.url {
                out.push(problem(
                    "mcp-oauth",
                    "oauth.resource must be the server's exact url (it binds tokens to it)",
                ));
            }
            if o.client_id.trim().is_empty() {
                out.push(problem("mcp-oauth", "oauth.client_id is empty"));
            }
            if let Some(env) = &o.client_secret_env {
                if !valid_env(env, &["PMOS_MCP_CLIENT_SECRET_"]) {
                    out.push(problem(
                        "mcp-oauth",
                        "oauth.client_secret_env must be a PMOS_MCP_CLIENT_SECRET_<NAME> variable name",
                    ));
                }
            }
            if o.scopes.is_empty() {
                out.push(problem("mcp-scope", "oauth.scopes lists no scope"));
            }
            let mut seen = BTreeSet::new();
            for s in &o.scopes {
                if s.trim().is_empty() || s.contains(char::is_whitespace) || s.contains('*') {
                    out.push(problem(
                        "mcp-scope",
                        format!("scope `{s}` must be one word with no wildcard"),
                    ));
                }
                if !seen.insert(s.as_str()) {
                    out.push(problem("mcp-scope", format!("scope `{s}` is listed twice")));
                }
                if !scopes_used.contains(s.as_str()) {
                    out.push(problem(
                        "mcp-scope",
                        format!("scope `{s}` is needed by no approved tool"),
                    ));
                }
                let writes = r
                    .tools
                    .iter()
                    .any(|t| t.class() == ToolClass::Write && t.scopes.iter().any(|x| x == s));
                if s.to_ascii_lowercase().contains("write") && !writes {
                    out.push(problem(
                        "mcp-scope",
                        format!("write scope `{s}` is needed by no approved write tool"),
                    ));
                }
            }
            for used in &scopes_used {
                if !o.scopes.iter().any(|s| s == used) {
                    out.push(problem(
                        "mcp-scope",
                        format!("a tool needs scope `{used}`, which oauth.scopes doesn't request"),
                    ));
                }
            }
            match &r.identity_check {
                None => out.push(problem(
                    "mcp-auth",
                    "oauth_per_user needs an identity_check: a server whose accounts can't be matched to the PM can't be linked",
                )),
                Some(c) => {
                    if !valid_upstream(&c.tool) || !valid_digest(&c.digest) || !valid_path(&c.email)
                    {
                        out.push(problem(
                            "mcp-auth",
                            "identity_check needs a tool name, its sha256 digest and the email's path",
                        ));
                    }
                }
            }
        }
        (_, Some(_)) => out.push(problem(
            "mcp-auth",
            "an `oauth` section is only for auth oauth_per_user",
        )),
        (_, None) => {
            if r.identity_check.is_some() {
                out.push(problem(
                    "mcp-auth",
                    "identity_check is only for auth oauth_per_user",
                ));
            }
            if !scopes_used.is_empty() {
                out.push(problem(
                    "mcp-scope",
                    "tools list scopes but the server has no OAuth",
                ));
            }
        }
    }
    match (r.auth, &r.secret_env) {
        (AuthKind::ServiceToken, Some(env))
            if valid_env(env, &["PMOS_MCP_TOKEN_"]) || env == "PMOS_DOCS_TOKEN" => {}
        (AuthKind::ServiceToken, _) => out.push(problem(
            "mcp-auth",
            "auth service_token needs secret_env: a PMOS_MCP_TOKEN_<NAME> variable (PMOS_DOCS_TOKEN for pmos-docs)",
        )),
        (_, Some(_)) => out.push(problem(
            "mcp-auth",
            "secret_env is only for auth service_token",
        )),
        _ => {}
    }
    if r.auth == AuthKind::ServiceToken && r.operator != Operator::Pmos {
        out.push(problem(
            "mcp-auth",
            "only a server PM OS runs takes PM OS's own service token; PM OS never sends its tokens to a third party",
        ));
    }
    if r.auth == AuthKind::None && r.data_class != Sensitivity::Public {
        out.push(problem(
            "mcp-auth",
            "auth none is for public data only: data_class must be public",
        ));
    }
    if r.operator == Operator::ThirdParty
        && r.owner_approval
            .as_deref()
            .is_none_or(|a| a.trim().is_empty())
    {
        out.push(problem(
            "mcp-third-party",
            "a third_party server needs owner_approval: the reference of the owner's recorded approval of its operator and data classes",
        ));
    }
    if !valid_handle(&r.approved_by) {
        out.push(problem(
            "mcp-approval",
            "approved_by must name the Admin who approved the server",
        ));
    }
    if !valid_pull(&r.pull) {
        out.push(problem(
            "mcp-approval",
            "pull must name the pull request that added the record (`#<n>` or its https URL)",
        ));
    }
    for host in &r.egress {
        if !dns_name(host) || host.contains('*') {
            out.push(problem(
                "mcp-url",
                format!("egress host `{host}` must be an exact host name"),
            ));
        }
    }
    if r.systems.is_empty() {
        out.push(problem("mcp-system", "systems lists no registry system"));
    }
    for s in &r.systems {
        if LOCAL_SYSTEMS.contains(&s.as_str()) {
            out.push(problem(
                "mcp-system",
                format!("`{s}` is served by PM OS itself, never by a server"),
            ));
        } else if !registry.tools.iter().any(|t| &t.system == s) {
            out.push(problem(
                "mcp-system",
                format!("system `{s}` has no tool in tools.yaml"),
            ));
        }
    }
    let mut mapped = BTreeSet::new();
    for t in &r.tools {
        let at = format!("tool `{}`", t.registry);
        if !mapped.insert(t.registry.as_str()) {
            out.push(problem("mcp-tool", format!("{at} is mapped twice")));
        }
        if !valid_upstream(&t.upstream) {
            out.push(problem(
                "mcp-tool",
                format!("{at}: `{}` is not an upstream tool name", t.upstream),
            ));
        }
        if !valid_digest(&t.digest) {
            out.push(problem(
                "mcp-digest",
                format!("{at}: digest must be `sha256:` and 64 lowercase hex digits"),
            ));
        }
        if !valid_handle(&t.approved_by) {
            out.push(problem(
                "mcp-approval",
                format!("{at}: approved_by must name the Admin who approved the tool"),
            ));
        }
        match registry.get(&t.registry) {
            None => out.push(problem("mcp-tool", format!("{at} is not in tools.yaml"))),
            Some(def) => {
                if !r.systems.contains(&def.system) {
                    out.push(problem(
                        "mcp-system",
                        format!(
                            "{at} is in system `{}`, which `systems` doesn't list",
                            def.system
                        ),
                    ));
                }
                match (def.kind, t.class) {
                    (ToolKind::Read, Some(ToolClass::Read)) => {}
                    (ToolKind::Read, _) => out.push(problem(
                        "mcp-class",
                        format!("{at} is a read in tools.yaml but has no `class: read` (unclassified is a write)"),
                    )),
                    (ToolKind::Write, Some(ToolClass::Read)) => out.push(problem(
                        "mcp-class",
                        format!("{at} is a write in tools.yaml and can't be mapped as a read"),
                    )),
                    (ToolKind::Write, _) => {}
                    (ToolKind::Internal, _) => out.push(problem(
                        "mcp-class",
                        format!("{at} changes PM OS's own state and is never served by a server"),
                    )),
                }
            }
        }
        if let Some(a) = t.adapter {
            if !a.tools().contains(&t.registry.as_str()) {
                out.push(problem(
                    "mcp-tool",
                    format!(
                        "{at}: the {} adapter is only for {}",
                        a.as_str(),
                        a.tools().join(" and ")
                    ),
                ));
            }
        }
        // Slice M8: a Confluence or Slack write needs its adapter, so PM OS's own rules (HTML
        // escaping and the footer; Slack's marker) run on what is sent.
        for a in [Adapter::ConfluenceStorage, Adapter::SlackText] {
            if a.tools().contains(&t.registry.as_str()) && t.adapter != Some(a) {
                out.push(problem(
                    "mcp-tool",
                    format!("{at} needs `adapter: {}`", a.as_str()),
                ));
            }
        }
        if t.registry == "slack.post_reply" && !t.args.drop.iter().any(|d| d == "message_ts") {
            out.push(problem(
                "mcp-tool",
                format!("{at} must drop `message_ts`: it is PM OS's own bookkeeping and never leaves PM OS"),
            ));
        }
        if t.registry == "confluence.update_page" && r.tool("confluence.read").is_none() {
            out.push(problem(
                "mcp-tool",
                format!("{at} needs confluence.read on the same server: PM OS checks the page's version with it before the update"),
            ));
        }
        if matches!(
            t.registry.as_str(),
            "slack.post_reply" | "slack.send_message"
        ) && !r.slack_audience()
        {
            out.push(problem(
                "mcp-slack-audience",
                format!("{at}: a Slack post needs the audience check, so the record must approve both checks (slack_conversation and slack_members); without them Slack posts stay on the built-in connector"),
            ));
        }
        if t.class() == ToolClass::Write && t.args.marker.is_none() {
            out.push(problem(
                "mcp-tool",
                format!("{at}: a write must name the argument that carries the Drafted with PM OS marker (args.marker)"),
            ));
        }
        let paths = [
            &t.result.items,
            &t.result.item,
            &t.result.id,
            &t.result.title,
            &t.result.url,
            &t.result.empty_note,
            &t.result.next_token,
            &t.result.last,
            &t.errors.code,
            &t.errors.message,
        ];
        let mut bad = paths
            .iter()
            .filter_map(|p| p.as_deref())
            .filter(|p| !valid_path(p));
        if let Some(p) = bad.next() {
            out.push(problem(
                "mcp-tool",
                format!("{at}: `{p}` is not a dotted path"),
            ));
        }
        let content_paths: Vec<&str> = match &t.result.content {
            Some(ContentMap::Whole(p)) => vec![p],
            Some(ContentMap::Fields(m)) => m.values().map(String::as_str).collect(),
            None => vec![],
        };
        if let Some(p) = content_paths.into_iter().find(|p| !valid_path(p)) {
            out.push(problem(
                "mcp-tool",
                format!("{at}: `{p}` is not a dotted path"),
            ));
        }
        let targets = t
            .args
            .rename
            .values()
            .map(String::as_str)
            .chain(t.args.fixed.keys().map(String::as_str))
            .chain(t.args.pass.iter().map(String::as_str))
            .chain(t.args.marker.as_deref());
        if let Some(p) = targets.into_iter().find(|p| !valid_path(p)) {
            out.push(problem(
                "mcp-tool",
                format!("{at}: `{p}` is not a dotted path"),
            ));
        }
        if t.result.items.is_some() && t.result.item.is_some() {
            out.push(problem(
                "mcp-tool",
                format!("{at}: result takes items or item, not both"),
            ));
        }
        if t.class() == ToolClass::Read && t.adapter.is_none() && t.result.id.is_none() {
            out.push(problem(
                "mcp-tool",
                format!("{at}: result.id must say where an item's id is"),
            ));
        }
    }
    out.extend(validate_checks(r));
    out
}

/// The record's check tools (slice M8).
fn validate_checks(r: &ServerRecord) -> Vec<Problem> {
    let mut out = Vec::new();
    let mut roles = BTreeSet::new();
    for c in &r.checks {
        let at = format!("check `{}`", c.role.as_str());
        if !roles.insert(c.role.as_str()) {
            out.push(problem("mcp-check", format!("{at} is listed twice")));
        }
        if !r.systems.iter().any(|s| s == c.role.system()) {
            out.push(problem(
                "mcp-check",
                format!(
                    "{at} is for `{}`, which `systems` doesn't list",
                    c.role.system()
                ),
            ));
        }
        if c.class != Some(ToolClass::Read) {
            out.push(problem(
                "mcp-check",
                format!("{at} must be `class: read` (unclassified is a write, and a check never writes)"),
            ));
        }
        if !valid_upstream(&c.upstream) {
            out.push(problem(
                "mcp-check",
                format!("{at}: `{}` is not an upstream tool name", c.upstream),
            ));
        }
        if !valid_digest(&c.digest) {
            out.push(problem(
                "mcp-digest",
                format!("{at}: digest must be `sha256:` and 64 lowercase hex digits"),
            ));
        }
        if !valid_handle(&c.approved_by) {
            out.push(problem(
                "mcp-approval",
                format!("{at}: approved_by must name the Admin who approved the tool"),
            ));
        }
        if c.args.marker.is_some() {
            out.push(problem(
                "mcp-check",
                format!("{at}: a check carries no marker"),
            ));
        }
        // Every argument PM OS gives it is passed, renamed or dropped, once; `channel` is sent.
        let a = &c.args;
        let named: Vec<&str> = a
            .pass
            .iter()
            .map(String::as_str)
            .chain(a.rename.keys().map(String::as_str))
            .chain(a.drop.iter().map(String::as_str))
            .collect();
        let mut seen = BTreeSet::new();
        for n in &named {
            if !c.role.args().contains(n) || !seen.insert(*n) {
                out.push(problem(
                    "mcp-check",
                    format!("{at}: argument `{n}` is unknown or named twice"),
                ));
            }
        }
        if let Some(missing) = c.role.args().iter().find(|n| !seen.contains(*n)) {
            out.push(problem(
                "mcp-check",
                format!("{at} doesn't say what happens to argument `{missing}`"),
            ));
        }
        if a.drop.iter().any(|d| d == "channel") {
            out.push(problem("mcp-check", format!("{at} must send `channel`")));
        }
        let paths = a
            .rename
            .values()
            .map(String::as_str)
            .chain(a.fixed.keys().map(String::as_str))
            .chain(a.pass.iter().map(String::as_str))
            .chain(
                [
                    &c.result.items,
                    &c.result.item,
                    &c.result.next_token,
                    &c.errors.code,
                    &c.errors.message,
                ]
                .into_iter()
                .filter_map(|p| p.as_deref()),
            );
        if let Some(p) = paths.into_iter().find(|p| !valid_path(p)) {
            out.push(problem(
                "mcp-check",
                format!("{at}: `{p}` is not a dotted path"),
            ));
        }
        match c.role {
            CheckRole::SlackConversation => {
                let fields = match &c.result.content {
                    Some(ContentMap::Fields(m)) => Some(m),
                    _ => None,
                };
                match fields {
                    None => out.push(problem(
                        "mcp-check",
                        format!("{at}: result.content must name where each of the conversation's fields is"),
                    )),
                    Some(m) => {
                        for (k, p) in m {
                            if !CheckRole::CONVERSATION_FIELDS.contains(&k.as_str())
                                || !valid_path(p)
                            {
                                out.push(problem(
                                    "mcp-check",
                                    format!("{at}: `{k}: {p}` is not a conversation field and path"),
                                ));
                            }
                        }
                        for k in CheckRole::CONVERSATION_REQUIRED {
                            if !m.contains_key(k) {
                                out.push(problem(
                                    "mcp-check",
                                    format!("{at}: result.content must say where `{k}` is: a channel's sharing is never guessed"),
                                ));
                            }
                        }
                    }
                }
            }
            CheckRole::SlackMembers => {
                if c.result.items.is_none() {
                    out.push(problem(
                        "mcp-check",
                        format!("{at}: result.items must say where the member ids are"),
                    ));
                }
            }
        }
    }
    out
}

/// One record file as loaded: its path relative to the content root and what it held.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub path: PathBuf,
    pub record: Result<ServerRecord, String>,
}

/// Every `content/mcp/*.yaml`, sorted by file name. No directory means no records.
pub fn read_dir(root: &Path) -> Vec<Loaded> {
    let dir = root.join(MCP_DIR);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file())
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let record = if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
                Err("only <id>.yaml server records belong in mcp/".to_string())
            } else {
                std::fs::read_to_string(&path)
                    .map_err(|e| format!("cannot read: {e}"))
                    .and_then(|t| parse(&t))
            };
            Loaded { path, record }
        })
        .collect()
}

/// Parse one record.
pub fn parse(text: &str) -> Result<ServerRecord, String> {
    serde_norway::from_str(text).map_err(|e| format!("invalid server record: {e}"))
}

/// Problems across records: two records with one id, or one registry name mapped by two
/// records (threat 3, tool-name shadowing).
pub fn cross_check(records: &[&ServerRecord]) -> Vec<Problem> {
    let mut out = Vec::new();
    let mut ids = BTreeSet::new();
    let mut served: BTreeMap<&str, &str> = BTreeMap::new();
    for r in records {
        if !ids.insert(r.id.as_str()) {
            out.push(problem("mcp-id", format!("two records have id `{}`", r.id)));
        }
        for t in &r.tools {
            match served.get(t.registry.as_str()) {
                Some(other) if *other != r.id => out.push(problem(
                    "mcp-shadow",
                    format!(
                        "`{}` is mapped by both `{other}` and `{}`: a registry name maps to one server",
                        t.registry, r.id
                    ),
                )),
                _ => {
                    served.insert(&t.registry, &r.id);
                }
            }
        }
    }
    out
}

/// Load and check every record under `root`: the records, or every problem as
/// `<path>: <code>: <message>` lines.
pub fn load(root: &Path, registry: &ToolRegistry) -> Result<Vec<ServerRecord>, Vec<String>> {
    let mut errors = Vec::new();
    let mut records = Vec::new();
    for loaded in read_dir(root) {
        let rel = loaded
            .path
            .strip_prefix(root)
            .unwrap_or(&loaded.path)
            .display()
            .to_string();
        match loaded.record {
            Err(e) => errors.push(format!("{rel}: mcp-parse: {e}")),
            Ok(record) => {
                let stem = loaded
                    .path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default();
                for p in validate(&record, stem, registry) {
                    errors.push(format!("{rel}: {}: {}", p.code, p.message));
                }
                records.push(record);
            }
        }
    }
    for p in cross_check(&records.iter().collect::<Vec<_>>()) {
        errors.push(format!("{MCP_DIR}: {}: {}", p.code, p.message));
    }
    if errors.is_empty() {
        Ok(records)
    } else {
        Err(errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIGEST: &str = "sha256:0000000000000000000000000000000000000000000000000000000000000000";

    fn registry() -> ToolRegistry {
        ToolRegistry::from_yaml(
            "tools:\n  - {name: jira.search, system: jira, kind: read}\n  - {name: jira.read, system: jira, kind: read}\n  - {name: jira.create_issue, system: jira, kind: write}\n  - {name: memory.update, system: memory, kind: internal}\n",
        )
        .unwrap()
    }

    fn good() -> String {
        format!(
            r##"
id: acme-jira
operator: system_vendor
transport: streamable_http
url: https://mcp.example.com/v1/mcp
identity: {{host: mcp.example.com}}
auth: oauth_per_user
oauth:
  client_id: pmos-client
  issuer: https://auth.example.com
  authorization_endpoint: https://auth.example.com/authorize
  token_endpoint: https://auth.example.com/token
  scopes: [read:issues, write:issues]
  resource: https://mcp.example.com/v1/mcp
identity_check: {{tool: whoami, digest: "{DIGEST}", email: email}}
systems: [jira]
approved_by: ana
pull: "#12"
tools:
  - registry: jira.search
    upstream: search
    digest: "{DIGEST}"
    class: read
    approved_by: ana
    scopes: [read:issues]
    args: {{pass: [jql, limit]}}
    result: {{items: issues, id: key, title: summary, url: url, content: fields}}
  - registry: jira.create_issue
    upstream: create
    digest: "{DIGEST}"
    approved_by: ana
    scopes: [write:issues]
    args: {{pass: [project, issue_type, summary, description, epic], marker: description}}
    result: {{id: key, url: url}}
"##
        )
    }

    fn codes(text: &str) -> Vec<&'static str> {
        let r = parse(text).unwrap();
        validate(&r, &r.id.clone(), &registry())
            .into_iter()
            .map(|p| p.code)
            .collect()
    }

    #[test]
    fn a_good_record_passes() {
        assert_eq!(codes(&good()), Vec::<&str>::new());
    }

    #[test]
    fn transports_other_than_streamable_http_are_refused() {
        assert_eq!(
            codes(&good().replace("transport: streamable_http", "transport: stdio")),
            ["mcp-transport"]
        );
        assert_eq!(
            codes(&good().replace("transport: streamable_http", "transport: sse")),
            ["mcp-transport"]
        );
    }

    #[test]
    fn remote_urls_are_https_at_a_name() {
        let http = good().replace(
            "url: https://mcp.example.com",
            "url: http://mcp.example.com",
        );
        assert!(codes(&http).contains(&"mcp-url"));
        let ip = good()
            .replace("https://mcp.example.com/v1/mcp", "https://10.0.0.5/v1/mcp")
            .replace("host: mcp.example.com", "host: 10.0.0.5");
        assert!(codes(&ip).contains(&"mcp-url"));
        let wild = good().replace("url: https://mcp.example.com", "url: https://*.example.com");
        assert!(codes(&wild).contains(&"mcp-url"));
        let other_host = good().replace("{host: mcp.example.com}", "{host: other.example.com}");
        assert_eq!(codes(&other_host), ["mcp-identity"]);
        let userinfo = good().replace(
            "url: https://mcp.example.com",
            "url: https://a@mcp.example.com",
        );
        assert!(codes(&userinfo).contains(&"mcp-url"));
    }

    #[test]
    fn an_unclassified_read_is_refused_and_a_write_cant_be_a_read() {
        let unclassified = good().replacen("    class: read\n", "", 1);
        assert!(codes(&unclassified).contains(&"mcp-class"));
        let write_as_read = good().replace(
            "    upstream: create\n",
            "    upstream: create\n    class: read\n",
        );
        assert!(codes(&write_as_read).contains(&"mcp-class"));
    }

    #[test]
    fn scopes_follow_the_tools() {
        let unused = good().replace(
            "scopes: [read:issues, write:issues]",
            "scopes: [read:issues, write:issues, admin:all]",
        );
        assert_eq!(codes(&unused), ["mcp-scope"]);
        // A write scope with only read tools.
        let read_only = good().replace(
            "    scopes: [write:issues]\n",
            "    scopes: [read:issues]\n",
        );
        let r = parse(&read_only).unwrap();
        assert!(
            validate(&r, "acme-jira", &registry())
                .iter()
                .any(|p| p.message
                    == "write scope `write:issues` is needed by no approved write tool")
        );
        let wildcard = good().replace("[read:issues, write:issues]", "[\"read:*\", write:issues]");
        assert!(codes(&wildcard).contains(&"mcp-scope"));
    }

    #[test]
    fn oauth_needs_an_identity_check_and_its_own_resource() {
        let no_check = good().replace(
            &format!("identity_check: {{tool: whoami, digest: \"{DIGEST}\", email: email}}\n"),
            "",
        );
        assert_eq!(codes(&no_check), ["mcp-auth"]);
        let other_resource = good().replace(
            "resource: https://mcp.example.com/v1/mcp",
            "resource: https://mcp.example.com/",
        );
        assert_eq!(codes(&other_resource), ["mcp-oauth"]);
        let metadata_ip = good().replace(
            "token_endpoint: https://auth.example.com/token",
            "token_endpoint: https://169.254.169.254/token",
        );
        assert_eq!(codes(&metadata_ip), ["mcp-oauth"]);
    }

    #[test]
    fn third_party_needs_the_owners_approval() {
        let tp = good().replace("operator: system_vendor", "operator: third_party");
        assert_eq!(codes(&tp), ["mcp-third-party"]);
        let approved = format!("{tp}owner_approval: OWN-2026-11-24\n");
        assert_eq!(codes(&approved), Vec::<&str>::new());
    }

    #[test]
    fn approvals_digests_and_markers_are_required() {
        assert!(
            codes(&good().replace("approved_by: ana\npull", "approved_by: \"\"\npull"))
                .contains(&"mcp-approval")
        );
        assert!(codes(&good().replace("pull: \"#12\"", "pull: soon")).contains(&"mcp-approval"));
        assert!(codes(&good().replacen(
            &format!("digest: \"{DIGEST}\"\n    class"),
            "digest: sha256:abc\n    class",
            1
        ))
        .contains(&"mcp-digest"));
        assert!(codes(&good().replace(", marker: description", "")).contains(&"mcp-tool"));
    }

    #[test]
    fn unknown_tools_systems_and_fields() {
        assert!(
            codes(&good().replace("registry: jira.search", "registry: jira.nope"))
                .contains(&"mcp-tool")
        );
        assert!(
            codes(&good().replace("systems: [jira]", "systems: [jira, memory]"))
                .contains(&"mcp-system")
        );
        assert!(
            parse(&format!("{}extra: 1\n", good())).is_err(),
            "unknown field"
        );
    }

    #[test]
    fn internal_servers_are_pinned_and_pmos_run() {
        let internal = r##"
id: pmos-docs
operator: pmos
transport: streamable_http
url: http://docs.pmos.internal:8730/mcp
identity: {address: 172.28.0.30}
auth: service_token
secret_env: PMOS_DOCS_TOKEN
data_class: public
systems: [jira]
approved_by: ana
pull: "#3"
tools: []
"##;
        assert_eq!(codes(internal), Vec::<&str>::new());
        assert!(
            codes(&internal.replace("identity: {address: 172.28.0.30}\n", ""))
                .contains(&"mcp-identity")
        );
        assert!(
            codes(&internal.replace("operator: pmos", "operator: system_vendor"))
                .contains(&"mcp-url")
        );
        assert!(
            codes(&internal.replace("secret_env: PMOS_DOCS_TOKEN", "secret_env: HOME"))
                .contains(&"mcp-auth")
        );
    }

    #[test]
    fn a_registry_name_maps_to_one_server() {
        let a = parse(&good()).unwrap();
        let mut b = a.clone();
        b.id = "other".into();
        let problems = cross_check(&[&a, &b]);
        assert!(problems.iter().any(|p| p.code == "mcp-shadow"));
        assert!(cross_check(&[&a, &a]).iter().any(|p| p.code == "mcp-id"));
    }

    fn m8_registry() -> ToolRegistry {
        ToolRegistry::from_yaml(
            "tools:\n  - {name: confluence.search, system: confluence, kind: read}\n  - {name: confluence.read, system: confluence, kind: read}\n  - {name: confluence.create_draft, system: confluence, kind: write}\n  - {name: confluence.update_page, system: confluence, kind: write}\n  - {name: slack.search, system: slack, kind: read}\n  - {name: slack.read_thread, system: slack, kind: read}\n  - {name: slack.post_reply, system: slack, kind: write}\n  - {name: slack.send_message, system: slack, kind: write}\n",
        )
        .unwrap()
    }

    /// A self-hosted Confluence and Slack server PM OS runs (slice M8).
    fn m8() -> String {
        format!(
            r##"
id: team-mcp
operator: pmos
transport: streamable_http
url: http://team-mcp.pmos.internal:8800/mcp
identity: {{address: 172.28.0.40}}
auth: service_token
secret_env: PMOS_MCP_TOKEN_TEAM
systems: [confluence, slack]
approved_by: ana
pull: "#14"
tools:
  - registry: confluence.read
    upstream: get_page
    digest: "{DIGEST}"
    class: read
    approved_by: ana
    args: {{pass: [page_id]}}
    result: {{item: page, id: id, title: title, content: content}}
  - registry: confluence.update_page
    upstream: update_page
    digest: "{DIGEST}"
    approved_by: ana
    adapter: confluence_storage
    args: {{pass: [page_id, version, body, version_comment], marker: body}}
    result: {{id: id, url: url}}
  - registry: slack.post_reply
    upstream: reply
    digest: "{DIGEST}"
    approved_by: ana
    adapter: slack_text
    args: {{pass: [channel, thread_ts, text], drop: [message_ts], marker: text}}
    result: {{id: id, url: url}}
checks:
  - role: slack_conversation
    upstream: conversation_info
    digest: "{DIGEST}"
    class: read
    approved_by: ana
    args: {{pass: [channel]}}
    result: {{item: channel, content: {{external: is_ext_shared, shared: is_shared, im: is_im, name: name}}}}
  - role: slack_members
    upstream: conversation_members
    digest: "{DIGEST}"
    class: read
    approved_by: ana
    args: {{pass: [channel, limit, cursor]}}
    result: {{items: members, next_token: next_cursor}}
"##
        )
    }

    fn m8_codes(text: &str) -> Vec<&'static str> {
        let r = parse(text).unwrap();
        validate(&r, &r.id.clone(), &m8_registry())
            .into_iter()
            .map(|p| p.code)
            .collect()
    }

    #[test]
    fn confluence_and_slack_writes_carry_their_rules() {
        assert_eq!(m8_codes(&m8()), Vec::<&str>::new());
        let r = parse(&m8()).unwrap();
        assert!(r.slack_audience());
        // Each write needs its adapter, and an adapter serves only its own tools.
        assert!(
            m8_codes(&m8().replace("    adapter: confluence_storage\n", "")).contains(&"mcp-tool")
        );
        assert!(
            m8_codes(&m8().replace("adapter: slack_text", "adapter: confluence_storage"))
                .contains(&"mcp-tool")
        );
        // A reply's message_ts never leaves PM OS.
        assert!(m8_codes(&m8().replace(
            "{pass: [channel, thread_ts, text], drop: [message_ts], marker: text}",
            "{pass: [channel, thread_ts, text, message_ts], marker: text}"
        ))
        .contains(&"mcp-tool"));
        // The version check needs the page read on the same server.
        let no_read = m8().replacen(
            "  - registry: confluence.read\n",
            "  - registry: confluence.search\n",
            1,
        );
        assert!(m8_codes(&no_read).contains(&"mcp-tool"));
        // A Slack post without both audience checks is refused.
        let without = m8().split("checks:\n").next().unwrap().to_string();
        assert!(m8_codes(&without).contains(&"mcp-slack-audience"));
        let one = m8().replace(
            "  - role: slack_members\n",
            "  - role: slack_conversation\n",
        );
        let codes = m8_codes(&one);
        assert!(
            codes.contains(&"mcp-check") && codes.contains(&"mcp-slack-audience"),
            "{codes:?}"
        );
    }

    #[test]
    fn checks_are_explicit_reads_with_every_argument_said() {
        // Unclassified is a write; a check never writes.
        assert!(m8_codes(&m8().replacen(
            "    class: read\n    approved_by: ana\n    args: {pass: [channel]}",
            "    approved_by: ana\n    args: {pass: [channel]}",
            1
        ))
        .contains(&"mcp-check"));
        // Every argument named; channel always sent.
        assert!(m8_codes(&m8().replace(
            "{pass: [channel, limit, cursor]}",
            "{pass: [channel, limit]}"
        ))
        .contains(&"mcp-check"));
        assert!(m8_codes(&m8().replace(
            "{pass: [channel, limit, cursor]}",
            "{pass: [limit, cursor], drop: [channel]}"
        ))
        .contains(&"mcp-check"));
        // A channel's sharing is never guessed.
        assert!(m8_codes(&m8().replace("external: is_ext_shared, ", "")).contains(&"mcp-check"));
        assert!(
            m8_codes(&m8().replace("name: name}", "name: name, colour: c}")).contains(&"mcp-check")
        );
        // A check is approved like a tool, and only for a system the record serves.
        assert!(m8_codes(&m8().replacen(&format!("digest: \"{DIGEST}\"\n    class: read\n    approved_by: ana\n    args: {{pass: [channel]}}"), "digest: sha256:00\n    class: read\n    approved_by: ana\n    args: {pass: [channel]}", 1))
            .contains(&"mcp-digest"));
        let r = parse(&m8()).unwrap();
        let mut confluence_only = r.clone();
        confluence_only.systems = vec!["confluence".into()];
        confluence_only
            .tools
            .retain(|t| t.registry.starts_with("confluence."));
        assert!(validate(&confluence_only, "team-mcp", &m8_registry())
            .iter()
            .any(|p| p.code == "mcp-check"));
        assert!(parse(&m8().replace("role: slack_members", "role: anything")).is_err());
    }

    #[test]
    fn urls_parse_strictly() {
        let u = parse_url("https://mcp.example.com:8443/v1/mcp").unwrap();
        assert_eq!(
            (u.https, u.host.as_str(), u.port),
            (true, "mcp.example.com", Some(8443))
        );
        for bad in [
            "ftp://x.example.com",
            "https://X.example.com",
            "https://x.example.com/a?b",
            "https://x.example.com/../a",
            "https://x.example.com/a b",
        ] {
            assert!(parse_url(bad).is_err(), "{bad}");
        }
    }
}
