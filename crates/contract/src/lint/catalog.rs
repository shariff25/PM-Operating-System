//! The catalog rules (`persona-*`): `personas/<key>/PERSONA.md`, checked against the roles,
//! the skills and the personal role (`docs/bot-marketplace-spec.md` §2).
//!
//! The ceiling rules are what keep a persona from doing more than a PM's own bot:
//! `persona-exceeds-base` (a tool the personal role doesn't have), `persona-data-class` (a
//! higher data class) and `persona-action` (re-enabling a write the personal role sets to
//! `never`) are errors, and `pmos policy generate` refuses content with errors, so the sandbox's
//! tool allowlist never depends on the catalog.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::{is_semver, sorted_entries, Cx, Severity};
use crate::bots::{role_tools, valid_name, ActionSetting, BotScope, Role};
use crate::model::{ToolKind, ToolRegistry};
use crate::personas::{
    check_display_name, check_text, drafts, need_met, reads, role_setting, write_label, Need,
    Persona, PersonaFrontMatter, ALWAYS_ON, MASCOTS, MAX_EXAMPLE, MAX_EXAMPLES, MAX_NEED_LABEL,
    MAX_NEED_WITHOUT, MAX_ORDER, MAX_PERSONAS, MAX_SUMMARY, MAX_TITLE, NEED_SOURCES, PERSONAL_ROLE,
};

impl Cx {
    fn error_count(&self) -> usize {
        self.report
            .diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .count()
    }

    /// `personas/<key>/PERSONA.md` (an optional folder). Runs after roles and bots.
    pub(super) fn check_personas(&mut self, registry: Option<&ToolRegistry>) {
        let dir = self.root.join("personas");
        let Some(entries) = sorted_entries(&dir) else {
            return;
        };
        let dirs: Vec<PathBuf> = entries.into_iter().filter(|p| p.is_dir()).collect();
        if dirs.len() > MAX_PERSONAS {
            self.error(
                "persona-count",
                &dir,
                format!(
                    "the catalog has {} personas; at most {MAX_PERSONAS}",
                    dirs.len()
                ),
            );
        }
        let base = self
            .report
            .roles
            .iter()
            .find(|r| r.name == PERSONAL_ROLE)
            .cloned();
        let team_roles: BTreeSet<String> = self
            .report
            .bots
            .iter()
            .filter(|b| b.scope == BotScope::Team)
            .map(|b| b.role.clone())
            .collect();

        let mut parsed: Vec<(PathBuf, PersonaFrontMatter)> = Vec::new();
        let mut personas = Vec::new();
        for dir in dirs {
            let path = dir.join("PERSONA.md");
            if !path.is_file() {
                self.error(
                    "persona-missing-file",
                    &dir,
                    "persona directory has no PERSONA.md",
                );
                continue;
            }
            let dir_name = dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            // The body is notes for maintainers: never shown, never sent to a model.
            let Some((fm, _notes)) =
                self.read_front_matter::<PersonaFrontMatter>(&path, "persona-parse")
            else {
                continue;
            };
            let before = self.error_count();
            let persona =
                self.check_persona(&path, &dir_name, &fm, base.as_ref(), &team_roles, registry);
            if self.error_count() == before {
                personas.extend(persona);
            }
            parsed.push((path, fm));
        }
        self.check_catalog(&dir, &parsed);
        personas.sort_by(|a: &Persona, b: &Persona| (a.order, &a.name).cmp(&(b.order, &b.name)));
        self.report.personas = personas;
    }

    /// One persona's own rules. `None` when it can't be resolved (unknown role, no personal
    /// role, no tool registry).
    fn check_persona(
        &mut self,
        path: &Path,
        dir_name: &str,
        fm: &PersonaFrontMatter,
        base: Option<&Role>,
        team_roles: &BTreeSet<String>,
        registry: Option<&ToolRegistry>,
    ) -> Option<Persona> {
        if fm.name != dir_name || !valid_name(&fm.name) {
            self.error(
                "persona-name",
                path,
                format!(
                    "name `{}` must equal the folder name and use lowercase letters, digits and single hyphens (at most 32)",
                    fm.name
                ),
            );
        }
        if let Err(msg) = check_display_name(&fm.display_name) {
            self.error("persona-text", path, msg);
        }
        for (field, text, max) in [
            ("title", &fm.title, MAX_TITLE),
            ("summary", &fm.summary, MAX_SUMMARY),
        ] {
            if let Err(msg) = check_text(field, text, max) {
                self.error("persona-text", path, msg);
            }
        }
        if fm.examples.is_empty() || fm.examples.len() > MAX_EXAMPLES {
            self.error(
                "persona-text",
                path,
                format!(
                    "examples lists {} asks; it must list 1 to {MAX_EXAMPLES}",
                    fm.examples.len()
                ),
            );
        }
        for (i, example) in fm.examples.iter().enumerate() {
            if let Err(msg) = check_text(&format!("examples[{i}]"), example, MAX_EXAMPLE) {
                self.error("persona-text", path, msg);
            }
        }
        if !MASCOTS.contains(&fm.mascot.as_str()) {
            self.error(
                "persona-mascot",
                path,
                format!(
                    "mascot `{}` is not one of: {} (new art is a code change)",
                    fm.mascot,
                    MASCOTS.join(", ")
                ),
            );
        }
        self.check_owner(path, &fm.owner);
        if !is_semver(&fm.version) {
            self.error(
                "persona-version",
                path,
                format!("version `{}` is not MAJOR.MINOR.PATCH", fm.version),
            );
        }
        if !(0..=MAX_ORDER).contains(&fm.order) {
            self.error(
                "persona-order",
                path,
                format!("order {} is not between 0 and {MAX_ORDER}", fm.order),
            );
        }
        let mut sources = BTreeSet::new();
        for need in &fm.needs {
            if !NEED_SOURCES.contains(&need.source.as_str()) {
                self.error(
                    "persona-need",
                    path,
                    format!(
                        "need source `{}` is not one of: {}",
                        need.source,
                        NEED_SOURCES.join(", ")
                    ),
                );
            } else if !sources.insert(need.source.as_str()) {
                self.error(
                    "persona-need",
                    path,
                    format!("need source `{}` is listed twice", need.source),
                );
            }
            for (field, text, max) in [
                ("needs.label", &need.label, MAX_NEED_LABEL),
                ("needs.without", &need.without, MAX_NEED_WITHOUT),
            ] {
                if let Err(msg) = check_text(field, text, max) {
                    self.error("persona-need", path, msg);
                }
            }
        }

        let Some(role) = self
            .report
            .roles
            .iter()
            .find(|r| r.name == fm.role)
            .cloned()
        else {
            self.error(
                "persona-role-unknown",
                path,
                format!("role `{}` is not in roles/", fm.role),
            );
            return None;
        };
        if team_roles.contains(&role.name) {
            self.error(
                "persona-role-team",
                path,
                format!(
                    "role `{}` is a team bot's role; a team bot reads with its own accounts and a persona reads as the PM, so a persona needs a role of its own",
                    role.name
                ),
            );
        }
        let tools = role_tools(&role, &self.report.skills);

        for name in &role.skills {
            let Some(skill) = self.report.skills.iter().find(|s| &s.name == name) else {
                continue; // role-skill-unknown
            };
            if !skill
                .surfaces
                .iter()
                .any(|s| s.as_str() == "web" || s.as_str() == "slack")
            {
                self.warn(
                    "persona-skill-surface",
                    path,
                    format!("skill `{name}` lists neither `web` nor `slack` in surfaces, so a PM can't ask this bot for it"),
                );
            }
        }

        let can_do: Vec<String> = match &fm.can_do {
            Some(list) => list.split_whitespace().map(String::from).collect(),
            None => role.skills.clone(),
        };
        if fm.can_do.is_some() && can_do.is_empty() {
            self.error(
                "persona-can-do",
                path,
                "can-do is empty; list skills of the role, or leave it out for all of them",
            );
        }
        let mut listed = BTreeSet::new();
        for skill in &can_do {
            if !role.skills.contains(skill) {
                self.error(
                    "persona-can-do",
                    path,
                    format!(
                        "can-do skill `{skill}` is not a skill of role `{}`",
                        role.name
                    ),
                );
            } else if !listed.insert(skill.as_str()) {
                self.error(
                    "persona-can-do",
                    path,
                    format!("can-do skill `{skill}` is listed twice"),
                );
            }
        }

        if let Some(registry) = registry {
            for tool in &tools {
                let is_write = registry
                    .get(tool)
                    .is_some_and(|t| t.kind == ToolKind::Write);
                if is_write && write_label(tool).is_none() {
                    self.error(
                        "persona-write-label",
                        path,
                        format!("write tool `{tool}` has no label for \"What it may draft\" (`WRITE_LABELS` in crates/contract/src/personas.rs)"),
                    );
                }
            }
        }

        // The ceiling rules.
        let Some(base) = base else {
            self.error(
                "persona-base-missing",
                path,
                format!("the personal role `{PERSONAL_ROLE}` is not in roles/, so this persona's ceiling can't be checked"),
            );
            return None;
        };
        let base_tools = role_tools(base, &self.report.skills);
        let extra: Vec<&str> = tools.difference(&base_tools).map(String::as_str).collect();
        if !extra.is_empty() {
            self.error(
                "persona-exceeds-base",
                path,
                format!(
                    "role `{}` has tools the personal role `{PERSONAL_ROLE}` doesn't: {}. A persona can't do more than a PM's own bot: drop the skills that need them, or widen `{PERSONAL_ROLE}` itself (a network change, POL-2)",
                    role.name,
                    extra.join(", ")
                ),
            );
        }
        if role.data_class > base.data_class {
            self.error(
                "persona-data-class",
                path,
                format!(
                    "role `{}` handles {} data, above the personal role's {}",
                    role.name, role.data_class, base.data_class
                ),
            );
        }
        if let Some(registry) = registry {
            for tool in &tools {
                let is_write = registry
                    .get(tool)
                    .is_some_and(|t| t.kind == ToolKind::Write);
                if is_write
                    && role_setting(base, tool) == ActionSetting::Never
                    && role_setting(&role, tool) != ActionSetting::Never
                {
                    self.error(
                        "persona-action",
                        path,
                        format!(
                            "`{tool}` is `never` for the personal role `{PERSONAL_ROLE}`; role `{}` must set it to `never` too, since a persona can't re-enable what the personal bot may not do",
                            role.name
                        ),
                    );
                }
            }
        }

        let registry = registry?;
        Some(Persona {
            name: fm.name.clone(),
            display_name: fm.display_name.clone(),
            title: fm.title.clone(),
            role: role.name.clone(),
            mascot: fm.mascot.clone(),
            owner: crate::bots::owner_handle(&fm.owner),
            version: fm.version.clone(),
            offered: fm.offered,
            always_on: fm.always_on,
            order: u32::try_from(fm.order).unwrap_or(u32::MAX),
            summary: fm.summary.clone(),
            examples: fm.examples.clone(),
            can_do,
            needs: fm
                .needs
                .iter()
                .map(|n| Need {
                    source: n.source.clone(),
                    label: n.label.clone(),
                    without: n.without.clone(),
                    met: need_met(&n.source, &tools, registry),
                })
                .collect(),
            reads: reads(&tools, registry),
            drafts: drafts(&tools, registry, &role, base),
        })
    }

    /// The rules across persona files: one always-on Nudge, names that don't collide (the
    /// Slack prefix matches a display name or a key, in any case), and orders.
    fn check_catalog(&mut self, dir: &Path, parsed: &[(PathBuf, PersonaFrontMatter)]) {
        if parsed.is_empty() {
            return;
        }
        let always_on: Vec<&(PathBuf, PersonaFrontMatter)> =
            parsed.iter().filter(|(_, fm)| fm.always_on).collect();
        if always_on.len() != 1 {
            self.error(
                "persona-always-on",
                dir,
                format!(
                    "exactly one persona must be `always-on: true` (`{ALWAYS_ON}`, role `{PERSONAL_ROLE}`); found {}",
                    always_on.len()
                ),
            );
        }
        for (path, fm) in parsed {
            if fm.always_on {
                if fm.name != ALWAYS_ON {
                    self.error(
                        "persona-always-on",
                        path,
                        format!("only `{ALWAYS_ON}` may be always-on"),
                    );
                }
                if fm.role != PERSONAL_ROLE {
                    self.error(
                        "persona-always-on",
                        path,
                        format!("the always-on persona's role must be the personal role `{PERSONAL_ROLE}`"),
                    );
                }
                if !fm.offered {
                    self.error(
                        "persona-always-on",
                        path,
                        "the always-on persona is on every team, so it must be `offered: true`",
                    );
                }
            } else if fm.name == ALWAYS_ON {
                self.error(
                    "persona-always-on",
                    path,
                    format!("`{ALWAYS_ON}` is reserved for the always-on persona; set `always-on: true`"),
                );
            }
        }

        // Keys and display names, ignoring case, belong to one persona each. Every persona
        // in a collision is named.
        let mut names: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        let mut orders: BTreeMap<i64, Vec<usize>> = BTreeMap::new();
        for (i, (_, fm)) in parsed.iter().enumerate() {
            let own: BTreeSet<String> = [fm.name.to_lowercase(), fm.display_name.to_lowercase()]
                .into_iter()
                .collect();
            for name in own {
                names.entry(name).or_default().push(i);
            }
            orders.entry(fm.order).or_default().push(i);
        }
        let others = |group: &[usize], me: usize| -> String {
            group
                .iter()
                .filter(|&&j| j != me)
                .map(|&j| format!("`{}`", parsed[j].1.name))
                .collect::<Vec<_>>()
                .join(", ")
        };
        for (name, group) in names.iter().filter(|(_, g)| g.len() > 1) {
            for &i in group {
                self.error(
                    "persona-name",
                    &parsed[i].0,
                    format!(
                        "`{name}` is also the name or display name of {}; a Slack prefix must pick one bot",
                        others(group, i)
                    ),
                );
            }
        }
        for (order, group) in orders.iter().filter(|(_, g)| g.len() > 1) {
            for &i in group {
                self.warn(
                    "persona-order",
                    &parsed[i].0,
                    format!(
                        "order {order} is also that of {}; they are shown by name",
                        others(group, i)
                    ),
                );
            }
        }
    }
}
