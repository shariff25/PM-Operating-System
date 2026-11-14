//! The PM OS launch spec (`policies/bots/<bot>.sandbox.yaml`): what the sandbox launcher
//! attaches and uploads when it creates a bot's sandbox.
//!
//! OpenShell takes these as `openshell sandbox create` flags (`--provider`, `--upload`), not as
//! policy fields, so they can't live in the policy file. Keeping them in a generated, checked
//! file means a launcher can't quietly attach another bot's credentials or upload more content
//! than the bot's role allows.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SandboxSpec {
    pub bot: String,
    pub role: String,
    /// `team` or `personal`.
    pub scope: String,
    /// The OpenShell sandbox name (`pmos-<bot>`).
    pub sandbox: String,
    /// The policy file, relative to this one (`<bot>.yaml`).
    pub policy: String,
    /// Where the worker runs: the writable workspace.
    pub workdir: String,
    /// `--provider` flags: this bot's provider instances, and nothing else.
    #[serde(default)]
    pub providers: Vec<ProviderAttachment>,
    /// `--upload <from>:<to>` flags, `from` relative to the content root. All read-only.
    #[serde(default)]
    pub uploads: Vec<Upload>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderAttachment {
    /// The provider instance (`pmos-<bot>-gateway`).
    pub name: String,
    /// The profile it is created from (`pmos-gateway`).
    pub profile: String,
    /// The environment variable that holds the credential's placeholder in the sandbox.
    pub credential: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Upload {
    /// Relative to the content root, `/`-separated (`skills/docs`).
    pub from: String,
    /// The absolute path in the sandbox (`/sandbox/skills/docs`).
    pub to: String,
}

pub fn parse(text: &str) -> Result<SandboxSpec, String> {
    serde_norway::from_str(text).map_err(|e| e.to_string())
}
