//! The bots, their roles and who may use them, loaded once at start-up from the content
//! repo through the same lint CI runs (`pmos_contract::lint`). A tree with lint errors stops
//! the runtime from starting, so a bad bot definition is caught before any task uses it.
//! After a content change, restart the runtime to pick it up.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use chrono::NaiveDate;
use pmos_contract::bots::{role_tools, ActionSetting, Bot, BotScope, Role};
use pmos_contract::model::{TeamFile, ToolKind, ToolRegistry};
use pmos_contract::{lint, LintOptions, Skill};
use serde::Serialize;

pub struct Catalog {
    bots: BTreeMap<String, Bot>,
    roles: BTreeMap<String, Role>,
    skills: Vec<Skill>,
    registry: ToolRegistry,
    team: TeamFile,
}

/// A bot as `GET /v1/bots` lists it.
#[derive(Debug, Clone, Serialize)]
pub struct BotSummary {
    pub name: String,
    pub role: String,
    pub scope: BotScope,
    pub owner: String,
    pub description: String,
}

/// A bot as `GET /v1/bots/{name}` shows it: its role, its tools, and each write tool's
/// setting (`approve` or `never`).
#[derive(Debug, Clone, Serialize)]
pub struct BotDetail {
    pub bot: Bot,
    pub role: Role,
    pub tools: Vec<String>,
    pub actions: BTreeMap<String, ActionSetting>,
}

impl Catalog {
    /// Lint `root` and load what it defines. Errors list every lint error.
    pub fn load(root: &Path, today: NaiveDate) -> Result<Self, String> {
        let report = lint(root, &LintOptions::new(today));
        if report.has_errors() {
            let errors: Vec<String> = report
                .errors()
                .map(|d| format!("{}: {} ({})", d.path, d.message, d.code))
                .collect();
            return Err(format!(
                "the content in {} has lint errors; fix them before starting:\n  {}",
                root.display(),
                errors.join("\n  ")
            ));
        }
        let warnings = report.warnings().count();
        if warnings > 0 {
            tracing::warn!(
                warnings,
                "the content has lint warnings; `pmos lint` lists them"
            );
        }
        let tools_path = root.join("tools.yaml");
        let text = std::fs::read_to_string(&tools_path)
            .map_err(|e| format!("{}: {e}", tools_path.display()))?;
        let registry =
            ToolRegistry::from_yaml(&text).map_err(|e| format!("{}: {e}", tools_path.display()))?;
        let team = TeamFile::load(root)?;
        if team.members.is_empty() {
            tracing::warn!("team.yaml lists nobody: no one can use the team bots until it does");
        }
        Ok(Self::from_parts(
            report.bots,
            report.roles,
            report.skills,
            registry,
            team,
        ))
    }

    /// From already-validated parts (tests).
    pub fn from_parts(
        bots: Vec<Bot>,
        roles: Vec<Role>,
        skills: Vec<Skill>,
        registry: ToolRegistry,
        team: TeamFile,
    ) -> Self {
        Self {
            bots: bots.into_iter().map(|b| (b.name.clone(), b)).collect(),
            roles: roles.into_iter().map(|r| (r.name.clone(), r)).collect(),
            skills,
            registry,
            team,
        }
    }

    pub fn bot(&self, name: &str) -> Option<&Bot> {
        self.bots.get(name)
    }

    pub fn role(&self, bot: &Bot) -> Option<&Role> {
        self.roles.get(&bot.role)
    }

    pub fn is_member(&self, handle: &str) -> bool {
        self.team.by_handle(handle).is_some()
    }

    pub fn summaries(&self) -> Vec<BotSummary> {
        self.bots
            .values()
            .map(|b| BotSummary {
                name: b.name.clone(),
                role: b.role.clone(),
                scope: b.scope,
                owner: b.owner.clone(),
                description: b.description.clone(),
            })
            .collect()
    }

    /// The registry names of the tools `bot` may call: its role's skills' tools.
    pub fn tools(&self, bot: &Bot) -> BTreeSet<String> {
        match self.role(bot) {
            Some(role) => role_tools(role, &self.skills),
            None => BTreeSet::new(),
        }
    }

    pub fn detail(&self, name: &str) -> Option<BotDetail> {
        let bot = self.bot(name)?;
        let role = self.role(bot)?;
        let tools = self.tools(bot);
        let actions = tools
            .iter()
            .filter(|t| {
                self.registry
                    .get(t)
                    .is_some_and(|d| d.kind == ToolKind::Write)
            })
            .map(|t| (t.clone(), bot.action_setting(role, t)))
            .collect();
        Some(BotDetail {
            bot: bot.clone(),
            role: role.clone(),
            tools: tools.into_iter().collect(),
            actions,
        })
    }

    /// May `handle` use `bot`? A personal bot serves its owner only; a team bot serves the
    /// members of `team.yaml`. The error says why not, for the PM.
    pub fn may_use(&self, handle: &str, bot: &Bot) -> Result<(), String> {
        match bot.scope {
            BotScope::Personal if bot.owner == handle => Ok(()),
            BotScope::Personal => Err(format!(
                "`{}` is {}'s personal bot; only they can use it",
                bot.name, bot.owner
            )),
            BotScope::Team if self.is_member(handle) => Ok(()),
            BotScope::Team => Err(format!(
                "`{handle}` isn't listed in team.yaml, so can't use the team bot `{}`",
                bot.name
            )),
        }
    }
}
