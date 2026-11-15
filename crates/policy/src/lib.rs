//! PM OS sandbox policies (step 0f; `docs/bot-runtime-spec.md` section 7, decision D3).
//!
//! Every bot runs in its own NVIDIA OpenShell sandbox. This crate generates that sandbox's
//! policy from the bot's definition and its role, and checks any policy file against the PM OS
//! rules:
//!
//! - **Network:** egress is denied by default; the only allowed destinations are the tool
//!   server's MCP endpoint ([`TOOLSERVER_HOST`]`:`[`TOOLSERVER_PORT`]), the model gateway
//!   ([`GATEWAY_HOST`]`:`[`GATEWAY_PORT`]) and the bot runtime's worker routes
//!   ([`RUNTIME_HOST`]`:`[`RUNTIME_PORT`]), all inspected and enforced.
//! - **Filesystem:** the bot's workspace is writable; its role's skills and its master context
//!   are read-only; nothing else is readable beyond the system paths OpenShell needs.
//! - **Credentials:** only the bot's own gateway and runtime-worker tokens, as OpenShell
//!   provider instances bound to those two endpoints. The tool-server session token is per
//!   task and arrives in the claim response, so the tool-server endpoint binds no provider.
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
/// The bot runtime, as sandboxes reach it: the harness worker claims and reports tasks here.
pub const RUNTIME_HOST: &str = "bots.pmos.internal";
pub const RUNTIME_PORT: u16 = 8750;
/// The runtime routes a worker may call (spec section 3, client `worker`), and nothing else:
/// no task, routine or notification reads. In OpenShell's REST path globs `*` matches one path
/// segment, so `*` stands for exactly one task id.
pub const RUNTIME_ROUTES: [(&str, &str); 6] = [
    ("POST", "/v1/worker/claim"),
    ("POST", "/v1/worker/tasks/*/complete"),
    ("POST", "/v1/worker/tasks/*/events"),
    ("POST", "/v1/worker/tasks/*/fail"),
    ("POST", "/v1/worker/tasks/*/heartbeat"),
    ("POST", "/v1/worker/tasks/*/trace"),
];

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
/// The tool registry, uploaded with the content: the worker reads it at start-up to tell a
/// bot's read tools from its writes. It holds only tool names, systems and kinds.
pub const TOOL_REGISTRY: &str = "tools.yaml";
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

/// The three services a sandbox may reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Service {
    ToolServer,
    Gateway,
    Runtime,
}

/// A per-bot OpenShell provider: the instance is `pmos-<bot>-<suffix>`, created from `profile`,
/// and the worker reads its placeholder from `env`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Credential {
    pub suffix: &'static str,
    pub profile: &'static str,
    pub env: &'static str,
}

impl Service {
    pub const ALL: [Service; 3] = [Service::ToolServer, Service::Gateway, Service::Runtime];

    pub fn host(self) -> &'static str {
        match self {
            Self::ToolServer => TOOLSERVER_HOST,
            Self::Gateway => GATEWAY_HOST,
            Self::Runtime => RUNTIME_HOST,
        }
    }

    pub fn port(self) -> u16 {
        match self {
            Self::ToolServer => TOOLSERVER_PORT,
            Self::Gateway => GATEWAY_PORT,
            Self::Runtime => RUNTIME_PORT,
        }
    }

    /// The `network_policies` key, which is also the name in OpenShell's logs.
    pub fn rule_name(self) -> &'static str {
        match self {
            Self::ToolServer => "pmos_toolserver",
            Self::Gateway => "pmos_gateway",
            Self::Runtime => "pmos_runtime",
        }
    }

    pub fn protocol(self) -> &'static str {
        match self {
            Self::ToolServer => "mcp",
            Self::Gateway | Self::Runtime => "rest",
        }
    }

    /// The REST routes allowed on this service, exactly (`None` for the MCP endpoint).
    pub fn routes(self) -> Option<&'static [(&'static str, &'static str)]> {
        match self {
            Self::ToolServer => None,
            Self::Gateway => Some(&GATEWAY_ROUTES),
            Self::Runtime => Some(&RUNTIME_ROUTES),
        }
    }

    /// The bot's own provider bound to this endpoint, if any. The tool server has none: its
    /// session token is minted per task and arrives in the claim response (the v1 decision in
    /// `docs/sandbox-policies.md`).
    pub fn credential(self) -> Option<Credential> {
        match self {
            Self::ToolServer => None,
            Self::Gateway => Some(Credential {
                suffix: "gateway",
                profile: "pmos-gateway",
                env: "PMOS_GATEWAY_TOKEN",
            }),
            Self::Runtime => Some(Credential {
                suffix: "worker",
                profile: "pmos-worker",
                env: "PMOS_WORKER_TOKEN",
            }),
        }
    }

    /// The services that bind a per-bot provider, with that provider.
    pub fn credentialed() -> impl Iterator<Item = (Service, Credential)> {
        Self::ALL
            .into_iter()
            .filter_map(|s| s.credential().map(|c| (s, c)))
    }

    /// The service an exact `host:port` names, if any.
    pub fn at(host: &str, port: u16) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|s| s.host().eq_ignore_ascii_case(host) && s.port() == port)
    }
}

/// The bot's provider instance for `service` (`pmos-<bot>-gateway`, `pmos-<bot>-worker`), or
/// `None` for the tool server, which binds no provider.
pub fn provider_name(bot: &str, service: Service) -> Option<String> {
    service
        .credential()
        .map(|c| format!("pmos-{bot}-{}", c.suffix))
}

/// The bot a PM OS provider instance name belongs to, if it is one.
pub fn provider_owner(provider: &str) -> Option<(&str, Service)> {
    let rest = provider.strip_prefix("pmos-")?;
    Service::credentialed().find_map(|(s, c)| {
        rest.strip_suffix(c.suffix)
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
            for (s, _) in Service::credentialed() {
                let name = provider_name(bot, s).unwrap();
                assert_eq!(provider_owner(&name), Some((bot, s)));
            }
        }
        assert_eq!(provider_owner("atlassian"), None);
        assert_eq!(provider_owner("pmos--gateway"), None);
        assert_eq!(provider_owner("pmos-gateway"), None);
        // The tool server binds no provider, so a session provider is nobody's.
        assert_eq!(provider_name("docs", Service::ToolServer), None);
        assert_eq!(provider_owner("pmos-docs-session"), None);
        assert_eq!(
            provider_name("docs", Service::Runtime).as_deref(),
            Some("pmos-docs-worker")
        );
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
        assert_eq!(
            Service::at("bots.pmos.internal", 8750),
            Some(Service::Runtime)
        );
        assert_eq!(Service::at("bots.pmos.internal", 8751), None);
        assert_eq!(Service::at("toolserver.pmos.internal", 8740), None);
        assert_eq!(Service::at("docs.pmos.internal", 8730), None);
    }
}
