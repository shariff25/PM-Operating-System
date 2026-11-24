//! Named bots and their roles (decisions D1, D3, D5, D7 and D12 in `docs/bot-decisions.md`).
//!
//! - A **role** (`content/roles/<role>/ROLE.md`) is a template for one job: the skills it may
//!   run, its data class, its model role and the default setting for each outbound action.
//!   Every bot is made from one role; a bot has exactly one (D12).
//! - A **bot** (`content/bots/<name>/BOT.md`) is a named teammate: its role, team or personal
//!   scope, owner, what a team bot may read, and per-action overrides. Its body is the job
//!   description the bot works to.
//!
//! In v1 every outbound action is `approve` or `never`; `auto` is refused by the lint until
//! the D10 benchmark backs it.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::model::{ModelRole, Sensitivity};

/// The event the bot runtime adds to a cancelled task once its worker has been told and let
/// it go (its first heartbeat, completion or failure after the cancel is refused). The
/// benchmark runner waits for it after cancelling a task it timed out, so the next attempt on
/// that lane doesn't wait behind the old one (`docs/bots-runtime.md`, `docs/evals.md`).
pub const WORKER_STOPPED_EVENT: &str = "the worker stopped after the cancel";

/// What happens when a bot uses an outbound (write) tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActionSetting {
    /// Runs without asking. Not allowed in v1.
    Auto,
    /// A PM approves each one (the v1 default for every outbound action).
    Approve,
    /// The bot may not do this at all.
    Never,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BotScope {
    /// Shared by the team; reads with its own read-only accounts, limited to `reads`.
    Team,
    /// Owned by one PM; reads as that PM; its memory is private to them.
    Personal,
}

/// What a team bot may read: chosen by its owner, enforced by the tool server.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Reads {
    #[serde(default)]
    pub jira_projects: Vec<String>,
    #[serde(default)]
    pub confluence_spaces: Vec<String>,
    #[serde(default)]
    pub slack_channels: Vec<String>,
}

impl Reads {
    pub fn is_empty(&self) -> bool {
        self.jira_projects.is_empty()
            && self.confluence_spaces.is_empty()
            && self.slack_channels.is_empty()
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub(crate) struct RoleFrontMatter {
    pub name: String,
    pub description: String,
    pub owner: String,
    pub version: String,
    /// Space-separated skill names.
    pub skills: String,
    pub data_class: String,
    pub model: String,
    #[serde(default)]
    pub actions: BTreeMap<String, ActionSetting>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Role {
    pub name: String,
    pub description: String,
    pub owner: String,
    pub version: String,
    pub skills: Vec<String>,
    pub data_class: Sensitivity,
    pub model: ModelRole,
    /// Per-tool defaults, by registry name (`jira.create_issue`).
    pub actions: BTreeMap<String, ActionSetting>,
    /// The role's standing instructions (the file body).
    pub instructions: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub(crate) struct BotFrontMatter {
    pub name: String,
    pub role: String,
    pub scope: BotScope,
    pub owner: String,
    pub description: String,
    #[serde(default)]
    pub reads: Option<Reads>,
    #[serde(default)]
    pub actions: BTreeMap<String, ActionSetting>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Bot {
    pub name: String,
    pub role: String,
    pub scope: BotScope,
    /// The owner's handle, without the `@`.
    pub owner: String,
    pub description: String,
    pub reads: Reads,
    /// Per-tool overrides of the role's settings.
    pub actions: BTreeMap<String, ActionSetting>,
    /// The bot's job description (the file body).
    pub job: String,
}

impl Bot {
    /// The setting for `tool` (a registry name): the bot's override, else the role's, else
    /// `approve`. Only meaningful for write tools; reads are always allowed.
    pub fn action_setting(&self, role: &Role, tool: &str) -> ActionSetting {
        self.actions
            .get(tool)
            .or_else(|| role.actions.get(tool))
            .copied()
            .unwrap_or(ActionSetting::Approve)
    }
}

/// The tools a role's bots may call: the union of its skills' declared tools.
pub fn role_tools<'a>(
    role: &Role,
    skills: impl IntoIterator<Item = &'a crate::model::Skill>,
) -> BTreeSet<String> {
    let wanted: BTreeSet<&str> = role.skills.iter().map(String::as_str).collect();
    skills
        .into_iter()
        .filter(|s| wanted.contains(s.name.as_str()))
        .flat_map(|s| s.tools.iter().cloned())
        .collect()
}

/// A valid bot or role name: lowercase letters, digits and single hyphens, at most 32.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--")
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

pub(crate) fn owner_handle(owner: &str) -> String {
    owner.trim().trim_start_matches('@').to_string()
}

impl RoleFrontMatter {
    pub(crate) fn into_role(self, body: String) -> Option<Role> {
        Some(Role {
            skills: self.skills.split_whitespace().map(String::from).collect(),
            data_class: Sensitivity::parse(&self.data_class)?,
            model: ModelRole::parse(&self.model)?,
            name: self.name,
            description: self.description,
            owner: owner_handle(&self.owner),
            version: self.version,
            actions: self.actions,
            instructions: body,
        })
    }
}

impl BotFrontMatter {
    pub(crate) fn into_bot(self, body: String) -> Bot {
        Bot {
            name: self.name,
            role: self.role,
            scope: self.scope,
            owner: owner_handle(&self.owner),
            description: self.description,
            reads: self.reads.unwrap_or_default(),
            actions: self.actions,
            job: body,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert!(valid_name("docs"));
        assert!(valid_name("pm-ana"));
        assert!(!valid_name("Docs"));
        assert!(!valid_name("a--b"));
        assert!(!valid_name("-a"));
        assert!(!valid_name(""));
    }

    #[test]
    fn action_settings_fall_back_to_the_role_then_approve() {
        let role = Role {
            name: "r".into(),
            description: "d".into(),
            owner: "pm".into(),
            version: "0.1.0".into(),
            skills: vec![],
            data_class: Sensitivity::Internal,
            model: ModelRole::Default,
            actions: BTreeMap::from([("slack.send_message".into(), ActionSetting::Never)]),
            instructions: String::new(),
        };
        let bot = Bot {
            name: "b".into(),
            role: "r".into(),
            scope: BotScope::Personal,
            owner: "ana".into(),
            description: "d".into(),
            reads: Reads::default(),
            actions: BTreeMap::from([("jira.create_issue".into(), ActionSetting::Never)]),
            job: String::new(),
        };
        assert_eq!(
            bot.action_setting(&role, "jira.create_issue"),
            ActionSetting::Never
        );
        assert_eq!(
            bot.action_setting(&role, "slack.send_message"),
            ActionSetting::Never
        );
        assert_eq!(
            bot.action_setting(&role, "confluence.create_draft"),
            ActionSetting::Approve
        );
    }
}
