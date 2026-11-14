//! The bots, their roles and who may use them, loaded from the content repo through the same
//! lint CI runs (`pmos_contract::lint`). A tree with lint errors stops the runtime from
//! starting, so a bad bot definition is caught before any task uses it.
//!
//! **Reloading** (onboarding spec §5): every 30 seconds, [`Reloader`] checks a fingerprint of
//! the files the catalog is built from (`team.yaml`, `tools.yaml`, `bots/**`, `roles/**` and
//! `skills/**`: paths, sizes and modification times). If it changed, the tree is linted and
//! loaded again and the new catalog replaces the old one whole. A tree that fails the lint
//! leaves the old catalog in place and logs the errors. A task already claimed keeps the bot,
//! role and tools it was claimed with.

use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

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

/// How often [`Reloader`] looks for a content change.
pub const RELOAD_EVERY: Duration = Duration::from_secs(30);

/// The files and folders under the content root the catalog is built from.
const WATCHED: [&str; 5] = ["team.yaml", "tools.yaml", "bots", "roles", "skills"];

/// A fingerprint of the watched files: their paths, sizes and modification times. A file
/// that can't be read still counts, as missing.
pub fn fingerprint(root: &Path) -> u64 {
    let mut entries: Vec<(String, u64, u128)> = Vec::new();
    for name in WATCHED {
        collect(root, &root.join(name), &mut entries);
    }
    entries.sort();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    entries.hash(&mut hasher);
    hasher.finish()
}

fn collect(root: &Path, path: &Path, out: &mut Vec<(String, u64, u128)>) {
    let Ok(meta) = std::fs::metadata(path) else {
        return;
    };
    if meta.is_dir() {
        let Ok(dir) = std::fs::read_dir(path) else {
            return;
        };
        for entry in dir.flatten() {
            collect(root, &entry.path(), out);
        }
        return;
    }
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let rel = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned();
    out.push((rel, meta.len(), modified));
}

/// What one [`Reloader::check`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reload {
    /// Nothing the catalog is built from changed.
    Unchanged,
    /// The content changed and the new catalog is in place, with this many bots.
    Reloaded { bots: usize },
    /// The content changed but doesn't pass the lint (or can't be read): the old catalog
    /// stays.
    Kept(String),
}

/// Reloads the runtime's catalog when the content changes.
pub struct Reloader {
    root: PathBuf,
    seen: u64,
}

impl Reloader {
    /// Watch `root`. Make it before loading the first catalog, so a change made while that
    /// loads is still seen.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let seen = fingerprint(&root);
        Self { root, seen }
    }

    /// Reload if the content changed since the last check.
    pub async fn check(&mut self, rt: &crate::Runtime, today: NaiveDate) -> Reload {
        let root = self.root.clone();
        let now = tokio::task::spawn_blocking(move || fingerprint(&root))
            .await
            .unwrap_or(self.seen);
        if now == self.seen {
            return Reload::Unchanged;
        }
        // Remembered whatever the outcome, so a tree that fails the lint is reported once,
        // not every 30 seconds.
        self.seen = now;
        let root = self.root.clone();
        let loaded = tokio::task::spawn_blocking(move || Catalog::load(&root, today))
            .await
            .unwrap_or_else(|e| Err(format!("loading the catalog panicked: {e}")));
        match loaded {
            Ok(catalog) => {
                let bots = catalog.bots.len();
                rt.set_catalog(Arc::new(catalog));
                tracing::info!(bots, "content changed; catalog reloaded");
                Reload::Reloaded { bots }
            }
            Err(e) => {
                tracing::error!(error = %e, "content changed but fails the lint; keeping the old catalog");
                Reload::Kept(e)
            }
        }
    }

    /// Check every `every` until the runtime stops.
    pub fn spawn(
        mut self,
        rt: Arc<crate::Runtime>,
        every: Duration,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(every);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            tick.tick().await;
            loop {
                tokio::select! {
                    _ = rt.stop.cancelled() => break,
                    _ = tick.tick() => {
                        self.check(&rt, chrono::Utc::now().date_naive()).await;
                    }
                }
            }
        })
    }
}
