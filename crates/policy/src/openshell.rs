//! The NVIDIA OpenShell sandbox policy format (`version: 1`), as far as PM OS uses it.
//!
//! Field names, types and the "unknown fields are rejected" rule are taken from OpenShell's
//! own schema: `docs/how-it-works/policies/schema.mdx` and the serde types in
//! `crates/openshell-policy-schema/src/lib.rs` of github.com/NVIDIA/OpenShell (main at
//! 2e5589b, 2026-11-13). Every struct here denies unknown fields, as OpenShell does, so a file
//! that parses here uses only field names OpenShell defines.
//!
//! PM OS mirrors more fields than it generates (`allowed_ips`, `tls`, credential rewriting,
//! middleware and so on) so that a hand edit using them parses and is then refused by a named
//! rule in [`crate::check`], instead of failing as an unknown field. Fields PM OS never needs
//! to inspect closely (GraphQL registries, JSON-RPC options, deny-rule matchers, middleware
//! configs) are kept as untyped YAML values. As of that commit every field of OpenShell's
//! `PolicyDocument`, `FilesystemPolicy`, `LandlockPolicy`, `ProcessPolicy`,
//! `NetworkPolicyRule`, `NetworkEndpoint`, `McpConfig` and `L7Allow` is mirrored; a field
//! OpenShell adds later is refused here as unknown until it is added (and given a rule).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_norway::Value;

/// A complete sandbox policy file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    /// Schema version; must be `1`.
    pub version: u32,
    /// Startup: Landlock filesystem rules.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filesystem_policy: Option<FilesystemPolicy>,
    /// Startup: what happens when Landlock can't be applied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub landlock: Option<Landlock>,
    /// Startup: the workload's user and group.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process: Option<Process>,
    /// Live: named egress rules. Anything no rule allows is denied.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub network_policies: BTreeMap<String, NetworkRule>,
    /// Live: middleware on allowed traffic. PM OS uses none.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub network_middlewares: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilesystemPolicy {
    /// Adds the sandbox's working directory to `read_write`. Defaults to `false` when
    /// `filesystem_policy` is present (and to `true` when it is omitted).
    #[serde(default)]
    pub include_workdir: bool,
    #[serde(default)]
    pub read_only: Vec<String>,
    #[serde(default)]
    pub read_write: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Landlock {
    /// `best_effort` (OpenShell's default: the sandbox runs *without* the filesystem rules if
    /// Landlock fails) or `hard_requirement` (the sandbox fails to start).
    #[serde(default = "best_effort")]
    pub compatibility: String,
}

fn best_effort() -> String {
    "best_effort".into()
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Process {
    /// `sandbox` or a numeric UID (1..=4294967294). OpenShell rejects root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_as_user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_as_group: Option<String>,
}

/// One entry of `network_policies`: every listed binary may reach every listed endpoint.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkRule {
    /// Display name in logs (`[policy:<name>]`); defaults to the map key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default)]
    pub endpoints: Vec<Endpoint>,
    /// An empty list matches no binary.
    #[serde(default)]
    pub binaries: Vec<Binary>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binary {
    pub path: String,
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Endpoint {
    // Destination.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ports: Vec<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_ips: Vec<String>,

    // Inspection.
    /// `rest`, `websocket`, `graphql`, `mcp`, `json-rpc` or `tcp`; omitted means no request
    /// inspection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
    /// `skip` relays traffic without terminating TLS, so it can't be inspected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls: Option<String>,
    /// `enforce` blocks requests that break the rules; `audit` (the default) only logs them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enforcement: Option<String>,
    /// Access preset: `read-only`, `read-write` or `full`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<AllowRule>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deny_rules: Vec<Value>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub allow_encoded_slash: bool,

    // Credentials.
    /// Binds the static credentials of an attached, endpointless provider to this endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_binding: Option<CredentialBinding>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub request_body_credential_rewrite: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub websocket_credential_rewrite: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub allow_uninspected_credentials: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_signing: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signing_service: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signing_region: Option<String>,

    // Protocol options.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persisted_queries: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graphql_persisted_queries: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graphql_max_body_bytes: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json_rpc: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp: Option<McpOptions>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialBinding {
    /// The attached provider *instance* (not profile) whose credentials resolve here.
    pub provider: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpOptions {
    /// Allowed MCP revisions; OpenShell's default is exactly `["2025-11-25"]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub versions: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_body_bytes: Option<u32>,
    /// Defaults to `true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strict_tool_names: Option<bool>,
    /// Allows every core MCP method, and every tool unless tool rules narrow it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_all_known_mcp_methods: Option<bool>,
}

/// An entry of `rules`; OpenShell wraps the matcher fields in `allow`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AllowRule {
    pub allow: Allow,
}

/// Allow matchers. Which fields apply depends on the endpoint's `protocol`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Allow {
    /// REST: HTTP method or `*`. MCP: the MCP method, such as `tools/call`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    /// REST and WebSocket: a path glob.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<Value>,
    /// MCP: the tool name for `tools/call`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<Matcher>,
    /// MCP: lower-level form of `tool` (`params.name`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fields: Option<Value>,
}

impl Allow {
    pub fn method(method: &str) -> Self {
        Self {
            method: Some(method.into()),
            ..Self::default()
        }
    }
}

/// A string glob, or `{any: [globs]}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Matcher {
    One(String),
    Any(AnyOf),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnyOf {
    pub any: Vec<String>,
}

impl Matcher {
    /// Every pattern the matcher accepts.
    pub fn patterns(&self) -> Vec<&str> {
        match self {
            Self::One(s) => vec![s.as_str()],
            Self::Any(a) => a.any.iter().map(String::as_str).collect(),
        }
    }
}

/// Parse a policy file. OpenShell itself refuses files over 4 MiB.
pub fn parse(text: &str) -> Result<Policy, String> {
    const MAX_BYTES: usize = 4 * 1024 * 1024;
    if text.len() > MAX_BYTES {
        return Err("the file is over OpenShell's 4 MiB policy limit".into());
    }
    serde_norway::from_str(text).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_openshells_own_full_example() {
        // The "Full Example" from OpenShell's schema reference, verbatim.
        let text = r#"
version: 1

filesystem_policy:
  include_workdir: true
  read_only: [/usr, /lib, /etc]
  read_write: [/tmp]

network_policies:
  github_rest_api:
    endpoints:
      - host: api.github.com
        port: 443
        protocol: rest
        enforcement: enforce
        access: read-only
    binaries:
      - path: /usr/bin/gh
  npm_registry:
    endpoints:
      - host: registry.npmjs.org
        port: 443
        protocol: rest
        enforcement: enforce
        access: read-only
        allow_encoded_slash: true
    binaries:
      - path: /usr/bin/node
"#;
        let p = parse(text).unwrap();
        assert_eq!(p.version, 1);
        assert!(p.filesystem_policy.as_ref().unwrap().include_workdir);
        assert_eq!(p.network_policies.len(), 2);
        assert!(p.network_policies["npm_registry"].endpoints[0].allow_encoded_slash);
    }

    #[test]
    fn parses_openshells_mcp_and_credential_examples() {
        let text = r#"
version: 1
network_policies:
  mcp_server:
    endpoints:
      - host: mcp.example.com
        port: 443
        path: /mcp
        protocol: mcp
        enforcement: enforce
        mcp:
          versions: ["2025-11-25"]
          allow_all_known_mcp_methods: true
        rules:
          - allow:
              method: initialize
          - allow:
              method: tools/call
              tool:
                any: [search_web, list_issues]
        deny_rules:
          - method: tools/call
            tool: send_email
    binaries:
      - path: /usr/bin/python3.12
  gcp_storage:
    endpoints:
      - host: storage.googleapis.com
        port: 443
        protocol: rest
        access: full
        credential_binding:
          provider: work-gcp
    binaries:
      - path: /usr/bin/curl
"#;
        let p = parse(text).unwrap();
        let mcp = &p.network_policies["mcp_server"].endpoints[0];
        assert_eq!(
            mcp.rules[1].allow.tool.as_ref().unwrap().patterns(),
            vec!["search_web", "list_issues"]
        );
        assert_eq!(
            p.network_policies["gcp_storage"].endpoints[0]
                .credential_binding
                .as_ref()
                .unwrap()
                .provider,
            "work-gcp"
        );
    }

    #[test]
    fn refuses_unknown_fields_like_openshell() {
        let err = parse("version: 1\nmounts: [/content]\n").unwrap_err();
        assert!(err.contains("mounts"), "{err}");
        let err = parse(
            "version: 1\nnetwork_policies:\n  a:\n    endpoints:\n      - host: x.example.com\n        port: 443\n        secret: y\n",
        )
        .unwrap_err();
        assert!(err.contains("secret"), "{err}");
    }

    #[test]
    fn requires_a_version() {
        assert!(parse("network_policies: {}\n").is_err());
    }
}
