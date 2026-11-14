//! PM OS sandbox policies (step 0f; `docs/bot-runtime-spec.md` section 7, decision D3).
//!
//! Every bot runs in its own NVIDIA OpenShell sandbox. This crate generates that sandbox's
//! policy from the bot's definition and its role, and checks any policy file against the PM OS
//! rules:
//!
//! - **Network:** egress is denied by default; the only allowed destinations are the tool
//!   server's MCP endpoint ([`TOOLSERVER_HOST`]`:`[`TOOLSERVER_PORT`]) and the model gateway
//!   ([`GATEWAY_HOST`]`:`[`GATEWAY_PORT`]), both inspected and enforced.
//! - **Filesystem:** the bot's workspace is writable; its role's skills and its master context
//!   are read-only; nothing else is readable beyond the system paths OpenShell needs.
//! - **Credentials:** only the bot's own tool-server session and gateway credentials, as
//!   OpenShell provider instances bound to those two endpoints.
//! - **Processes:** a non-root identity, and Landlock is a hard requirement.
//!
//! Three kinds of file are written under `policies/`:
//!
//! | File | Format | Used by |
//! | --- | --- | --- |
//! | `bots/<bot>.yaml` | OpenShell sandbox policy | `openshell sandbox create --policy` |
//! | `bots/<bot>.sandbox.yaml` | PM OS launch spec ([`spec::SandboxSpec`]) | the sandbox launcher: providers to attach, content to upload |
//! | `providers/<profile>.yaml` | OpenShell provider profile | `openshell provider profile import` |
//!
//! The OpenShell policy can't carry PM OS metadata (OpenShell rejects unknown fields), which is
//! why the launch spec is a separate file. Format details and what is still to verify on the
//! Spark are in `docs/sandbox-policies.md`.

pub mod check;
pub mod generate;
pub mod openshell;
pub mod spec;
pub mod yaml;

pub use check::{check, check_policy, check_spec, CheckReport, Violation};
pub use generate::{generate, write, BotContext, Generated, WriteSummary};

/// The tool server, as sandboxes reach it (spec section 7).
pub const TOOLSERVER_HOST: &str = "toolserver.pmos.internal";
pub const TOOLSERVER_PORT: u16 = 8710;
/// The tool server's Streamable HTTP MCP route (`docs/toolserver.md`).
pub const TOOLSERVER_MCP_PATH: &str = "/mcp";
/// The model gateway, as sandboxes reach it (spec section 7).
pub const GATEWAY_HOST: &str = "gateway.pmos.internal";
pub const GATEWAY_PORT: u16 = 8740;
/// The gateway routes a worker may call (`docs/gateway.md`): chat only. Embeddings, model
/// listing and the workaround registry aren't needed from inside a sandbox.
pub const GATEWAY_ROUTES: [(&str, &str); 1] = [("POST", "/v1/chat/completions")];

/// MCP methods a worker may send. The first three are what a 2025-revision client sends before
/// and around tool calls (OpenShell's own MCP example allows exactly these); tool calls are
/// further limited to the role's tools.
/// Verify on the Spark: the harness's MCP client sends nothing else (for example `ping` or
/// `notifications/cancelled`) during a normal task.
pub const MCP_METHODS: [&str; 4] = [
    "initialize",
    "notifications/initialized",
    "tools/list",
    "tools/call",
];

/// MCP revisions the tool-server endpoint accepts. OpenShell's default is only `2025-11-25`,
/// and it treats a request without an `MCP-Protocol-Version` header as `2025-03-26`, so all
/// three 2025 revisions are listed.
/// Verify on the Spark: the revision the harness client (`@modelcontextprotocol/sdk`) and the
/// tool server (`rmcp`) negotiate is one of these.
pub const MCP_VERSIONS: [&str; 3] = ["2025-03-26", "2025-06-18", "2025-11-25"];

/// The one executable allowed to open connections: the harness worker's Node.js.
/// Verify on the Spark: the real (non-symlink) path of `node` in the worker image. OpenShell
/// matches the binary that opens the connection, or any of its parents, by real path.
pub const WORKER_BINARY: &str = "/usr/local/bin/node";

/// Where content goes inside every sandbox: the content-relative path under this prefix, so
/// `skills/docs` is uploaded to `/sandbox/skills/docs`.
/// Verify on the Spark: `/sandbox` is the sandbox's working directory with the Docker driver,
/// and uploads into paths that Landlock makes read-only succeed at `sandbox create --upload`.
pub const CONTENT_MOUNT: &str = "/sandbox";
/// The bot's own writable workspace, and the working directory its worker runs in.
pub const WORKSPACE: &str = "/sandbox/workspace";

/// Read-only system paths: exactly OpenShell's baseline set, which it adds anyway to any policy
/// with a network rule. They are listed so the file says what the sandbox can read.
pub const SYSTEM_READ_ONLY: [&str; 7] = [
    "/app",
    "/dev/urandom",
    "/etc",
    "/lib",
    "/proc",
    "/usr",
    "/var/log",
];
/// Writable system paths: OpenShell's baseline. `/tmp` is the sandbox's own, not the host's.
pub const SYSTEM_READ_WRITE: [&str; 2] = ["/dev/null", "/tmp"];

/// The workload identity: OpenShell's non-root `sandbox` account.
pub const RUN_AS: &str = "sandbox";
/// UIDs and GIDs below this are system accounts; a numeric identity must be at least this.
pub const MIN_NUMERIC_ID: u64 = 1000;

/// The first line of every generated file.
pub const GENERATED_MARKER: &str = "# Generated by `pmos policy generate`";

/// The two services a sandbox may reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Service {
    ToolServer,
    Gateway,
}

impl Service {
    pub const ALL: [Service; 2] = [Service::ToolServer, Service::Gateway];

    pub fn host(self) -> &'static str {
        match self {
            Self::ToolServer => TOOLSERVER_HOST,
            Self::Gateway => GATEWAY_HOST,
        }
    }

    pub fn port(self) -> u16 {
        match self {
            Self::ToolServer => TOOLSERVER_PORT,
            Self::Gateway => GATEWAY_PORT,
        }
    }

    /// The `network_policies` key, which is also the name in OpenShell's logs.
    pub fn rule_name(self) -> &'static str {
        match self {
            Self::ToolServer => "pmos_toolserver",
            Self::Gateway => "pmos_gateway",
        }
    }

    pub fn protocol(self) -> &'static str {
        match self {
            Self::ToolServer => "mcp",
            Self::Gateway => "rest",
        }
    }

    /// The suffix of the bot's provider instance (`pmos-<bot>-<suffix>`).
    pub fn provider_suffix(self) -> &'static str {
        match self {
            Self::ToolServer => "session",
            Self::Gateway => "gateway",
        }
    }

    /// The OpenShell provider profile the instance is created from.
    pub fn profile(self) -> &'static str {
        match self {
            Self::ToolServer => "pmos-session",
            Self::Gateway => "pmos-gateway",
        }
    }

    /// The environment variable the worker reads the credential's placeholder from.
    pub fn credential_env(self) -> &'static str {
        match self {
            Self::ToolServer => "PMOS_SESSION_TOKEN",
            Self::Gateway => "PMOS_GATEWAY_TOKEN",
        }
    }

    /// The service an exact `host:port` names, if any.
    pub fn at(host: &str, port: u16) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|s| s.host().eq_ignore_ascii_case(host) && s.port() == port)
    }
}

/// The bot's provider instance for `service`: `pmos-<bot>-session` or `pmos-<bot>-gateway`.
pub fn provider_name(bot: &str, service: Service) -> String {
    format!("pmos-{bot}-{}", service.provider_suffix())
}

/// The bot a PM OS provider instance name belongs to, if it is one.
pub fn provider_owner(provider: &str) -> Option<(&str, Service)> {
    let rest = provider.strip_prefix("pmos-")?;
    Service::ALL.into_iter().find_map(|s| {
        rest.strip_suffix(s.provider_suffix())
            .and_then(|r| r.strip_suffix('-'))
            .filter(|bot| !bot.is_empty())
            .map(|bot| (bot, s))
    })
}

/// The OpenShell sandbox name for a bot.
pub fn sandbox_name(bot: &str) -> String {
    format!("pmos-{bot}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_names_round_trip() {
        for bot in ["docs", "pm-ana", "a"] {
            for s in Service::ALL {
                assert_eq!(provider_owner(&provider_name(bot, s)), Some((bot, s)));
            }
        }
        assert_eq!(provider_owner("atlassian"), None);
        assert_eq!(provider_owner("pmos--gateway"), None);
        assert_eq!(provider_owner("pmos-gateway"), None);
    }

    #[test]
    fn services_are_found_only_by_exact_host_and_port() {
        assert_eq!(
            Service::at("toolserver.pmos.internal", 8710),
            Some(Service::ToolServer)
        );
        assert_eq!(
            Service::at("GATEWAY.pmos.internal", 8740),
            Some(Service::Gateway)
        );
        assert_eq!(Service::at("toolserver.pmos.internal", 8740), None);
        assert_eq!(Service::at("docs.pmos.internal", 8730), None);
    }
}
