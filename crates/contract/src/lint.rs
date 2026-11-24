//! The content lint: checks a `content/` tree against the contract in `docs/contract.md`.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::NaiveDate;
use serde::de::DeserializeOwned;
use serde::Serialize;
use walkdir::WalkDir;

use crate::frontmatter;

mod catalog;
use crate::model::{
    ContextFrontMatter, ModelRole, PlaybookFrontMatter, ReviewerFrontMatter, Sensitivity, Skill,
    SkillFrontMatter, TemplateFrontMatter, ToolRegistry, CONTEXT_DOMAINS, PHASES, SURFACES,
};

/// Days after `last-reviewed` before a context file is flagged stale (CTX-2).
pub const DEFAULT_STALE_AFTER_DAYS: i64 = 90;
/// Agent Skills spec limits.
pub const MAX_SKILL_NAME_LEN: usize = 64;
pub const MAX_SKILL_DESCRIPTION_LEN: usize = 1024;
/// The longest `metadata.track`: a catalog heading, such as "Strategy and decisions".
pub const MAX_SKILL_TRACK_LEN: usize = 40;

/// The retired team file's name. The lint only checks that it is absent; it never reads it.
const RETIRED_TEAM_FILE: &str = "team.yaml";

/// Metadata keys every PM OS skill must set.
pub const REQUIRED_SKILL_METADATA: [&str; 5] = ["owner", "version", "model", "data-class", "tools"];

#[derive(Debug, Clone)]
pub struct LintOptions {
    pub today: NaiveDate,
    pub stale_after_days: i64,
    /// How `team-file-retired` (a `team.yaml` in the content tree) is reported. An error for
    /// `pmos lint`, CI and `pmos policy generate` ([`LintOptions::new`]); a warning for a
    /// service loading its content ([`LintOptions::for_service`]). The file decides nothing
    /// in any service (`docs/user-database-spec.md` §12, slice U7), so a leftover one, such
    /// as during an upgrade before its pull request deletes it, must not stop a service from
    /// starting or reloading: the service logs the warning and ignores the file.
    pub retired_team_file: Severity,
    /// The personas whose eval categories meet their pass bar (D10: two weeks in a row), as
    /// `pmos lint` reads them from the eval reports (`pmos_evals::history::benchmarked_personas`).
    /// `Some`: an offered persona that is neither always-on nor `preview: true` must be in it
    /// (`persona-benchmark`, an error). `None`: the rule is not checked here (a service loading
    /// its content, the policy generator, the content admin): CI's `pmos lint` is the gate.
    pub benchmarked: Option<BTreeSet<String>>,
}

impl LintOptions {
    pub fn new(today: NaiveDate) -> Self {
        Self {
            today,
            stale_after_days: DEFAULT_STALE_AFTER_DAYS,
            retired_team_file: Severity::Error,
            benchmarked: None,
        }
    }

    /// For a service loading its content (the tool server, the bot runtime, the Slack App
    /// Home): every rule as [`LintOptions::new`], except that `team-file-retired` is a warning.
    pub fn for_service(today: NaiveDate) -> Self {
        Self {
            retired_team_file: Severity::Warning,
            ..Self::new(today)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Diagnostic {
    pub severity: Severity,
    /// Stable rule id, such as `context-stale`.
    pub code: &'static str,
    /// Path relative to the content root, with `/` separators.
    pub path: String,
    pub message: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Report {
    pub files_checked: usize,
    pub skills: Vec<Skill>,
    /// Roles and bots that parsed, for services that load them (the bot runtime).
    pub roles: Vec<crate::bots::Role>,
    pub bots: Vec<crate::bots::Bot>,
    /// The bot marketplace's catalog (`personas/<key>/PERSONA.md`): each persona whose own file
    /// has no error, by `order`, then name.
    pub personas: Vec<crate::personas::Persona>,
    pub diagnostics: Vec<Diagnostic>,
}

impl Report {
    pub fn errors(&self) -> impl Iterator<Item = &Diagnostic> {
        self.diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error)
    }

    pub fn warnings(&self) -> impl Iterator<Item = &Diagnostic> {
        self.diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Warning)
    }

    pub fn has_errors(&self) -> bool {
        self.errors().next().is_some()
    }

    /// Codes of all diagnostics, for tests.
    pub fn codes(&self) -> Vec<&'static str> {
        self.diagnostics.iter().map(|d| d.code).collect()
    }
}

/// Lint the content tree at `root`.
pub fn lint(root: &Path, opts: &LintOptions) -> Report {
    let mut cx = Cx {
        root: root.to_path_buf(),
        report: Report::default(),
    };

    if !root.is_dir() {
        cx.error("root-missing", root, "content root is not a directory");
        return cx.report;
    }

    let registry = cx.load_registry();
    cx.check_master();
    cx.check_skills(registry.as_ref());
    cx.check_context(opts);
    cx.check_reviewers();
    cx.check_templates();
    cx.check_playbooks();
    cx.check_team_file_retired(opts);
    cx.check_people();
    cx.check_roles_and_bots(registry.as_ref());
    cx.check_personas(registry.as_ref(), opts);
    cx.check_mcp_servers(registry.as_ref());

    cx.report
        .diagnostics
        .sort_by(|a, b| (&a.path, a.code).cmp(&(&b.path, b.code)));
    cx.report
}

struct Cx {
    root: PathBuf,
    report: Report,
}

impl Cx {
    fn rel(&self, path: &Path) -> String {
        let rel = path.strip_prefix(&self.root).unwrap_or(path);
        let s = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        if s.is_empty() {
            ".".to_string()
        } else {
            s
        }
    }

    fn push(&mut self, severity: Severity, code: &'static str, path: &Path, message: String) {
        let path = self.rel(path);
        self.report.diagnostics.push(Diagnostic {
            severity,
            code,
            path,
            message,
        });
    }

    fn error(&mut self, code: &'static str, path: &Path, message: impl Into<String>) {
        self.push(Severity::Error, code, path, message.into());
    }

    fn warn(&mut self, code: &'static str, path: &Path, message: impl Into<String>) {
        self.push(Severity::Warning, code, path, message.into());
    }

    fn read(&mut self, path: &Path) -> Option<String> {
        self.report.files_checked += 1;
        match fs::read_to_string(path) {
            Ok(s) => Some(s),
            Err(e) => {
                self.error("read-failed", path, format!("cannot read file: {e}"));
                None
            }
        }
    }

    /// Read a markdown file and parse its front matter as `T`. Returns the value and body.
    fn read_front_matter<T: DeserializeOwned>(
        &mut self,
        path: &Path,
        code: &'static str,
    ) -> Option<(T, String)> {
        let text = self.read(path)?;
        let doc = match frontmatter::split(&text) {
            Ok(doc) => doc,
            Err(e) => {
                self.error("front-matter", path, e.to_string());
                return None;
            }
        };
        match serde_norway::from_str::<T>(doc.yaml) {
            Ok(v) => Some((v, doc.body.to_string())),
            Err(e) => {
                self.error(code, path, format!("invalid front matter: {e}"));
                None
            }
        }
    }

    fn check_owner(&mut self, path: &Path, owner: &str) {
        let owner = owner.trim();
        if owner.is_empty() {
            self.error("owner-missing", path, "owner is empty");
        } else if owner.to_ascii_uppercase().starts_with("TODO") {
            self.warn("owner-unassigned", path, "owner is not assigned yet");
        }
    }

    fn load_registry(&mut self) -> Option<ToolRegistry> {
        let path = self.root.join("tools.yaml");
        if !path.is_file() {
            self.error("tools-missing", &path, "tools.yaml is missing");
            return None;
        }
        let text = self.read(&path)?;
        let registry: ToolRegistry = match serde_norway::from_str(&text) {
            Ok(r) => r,
            Err(e) => {
                self.error("tools-parse", &path, format!("invalid tools.yaml: {e}"));
                return None;
            }
        };
        let mut seen = BTreeSet::new();
        let mut harness_names: std::collections::BTreeMap<String, &str> = Default::default();
        for tool in &registry.tools {
            if !seen.insert(tool.name.as_str()) {
                self.error(
                    "tool-duplicate",
                    &path,
                    format!("tool `{}` is listed more than once", tool.name),
                );
            }
            if let Err(msg) = validate_tool_name(&tool.name, &tool.system) {
                self.error("tool-name", &path, format!("tool `{}`: {msg}", tool.name));
            }
            let harness = crate::model::harness_tool_name(&tool.name);
            match harness_names.get(&harness) {
                Some(first) if *first != tool.name => {
                    let msg = format!(
                        "tools `{first}` and `{}` both map to harness name `{harness}`",
                        tool.name
                    );
                    self.error("tool-harness-collision", &path, msg);
                }
                Some(_) => {}
                None => {
                    harness_names.insert(harness, &tool.name);
                }
            }
        }
        Some(registry)
    }

    /// `mcp/<id>.yaml`: MCP server records (`docs/mcp-servers-spec.md` §2.2). Each must have
    /// the record's shape, name tools in `tools.yaml` and the approvals; no registry name may be
    /// mapped by two records.
    fn check_mcp_servers(&mut self, registry: Option<&ToolRegistry>) {
        let loaded = crate::mcp::read_dir(&self.root);
        let mut records = Vec::new();
        for l in loaded {
            self.report.files_checked += 1;
            match l.record {
                Err(e) => self.error("mcp-parse", &l.path, e),
                Ok(record) => {
                    if let Some(registry) = registry {
                        let stem = l
                            .path
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or_default()
                            .to_string();
                        for p in crate::mcp::validate(&record, &stem, registry) {
                            self.error(p.code, &l.path, p.message);
                        }
                    }
                    records.push(record);
                }
            }
        }
        let dir = self.root.join(crate::mcp::MCP_DIR);
        for p in crate::mcp::cross_check(&records.iter().collect::<Vec<_>>()) {
            self.error(p.code, &dir, p.message);
        }
    }

    fn check_master(&mut self) {
        let path = self.root.join("master").join("AGENTS.md");
        if !path.is_file() {
            self.error(
                "master-missing",
                &path,
                "team master context master/AGENTS.md is missing (CTX-3)",
            );
            return;
        }
        if let Some(text) = self.read(&path) {
            if text.trim().is_empty() {
                self.error("master-empty", &path, "team master context is empty");
            }
        }
    }

    fn check_skills(&mut self, registry: Option<&ToolRegistry>) {
        let dir = self.root.join("skills");
        let Some(entries) = sorted_entries(&dir) else {
            self.error("skills-missing", &dir, "skills/ directory is missing");
            return;
        };
        for entry in entries.into_iter().filter(|p| p.is_dir()) {
            let skill_md = entry.join("SKILL.md");
            if !skill_md.is_file() {
                self.error(
                    "skill-missing-file",
                    &entry,
                    "skill directory has no SKILL.md",
                );
                continue;
            }
            let dir_name = entry
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if let Some(skill) = self.check_skill(&skill_md, &dir_name, registry) {
                self.report.skills.push(skill);
            }
        }
    }

    fn check_skill(
        &mut self,
        path: &Path,
        dir_name: &str,
        registry: Option<&ToolRegistry>,
    ) -> Option<Skill> {
        let (fm, body) = self.read_front_matter::<SkillFrontMatter>(path, "skill-parse")?;
        let mut ok = true;

        if let Err(msg) = validate_skill_name(&fm.name, dir_name) {
            self.error("skill-name", path, msg);
            ok = false;
        }
        let description = fm.description.trim();
        if description.is_empty() {
            self.error("skill-description", path, "description is empty");
            ok = false;
        } else if description.chars().count() > MAX_SKILL_DESCRIPTION_LEN {
            self.error(
                "skill-description",
                path,
                format!("description is longer than {MAX_SKILL_DESCRIPTION_LEN} characters"),
            );
            ok = false;
        }
        if body.trim().is_empty() {
            self.error("skill-body-empty", path, "skill has no instructions");
            ok = false;
        }

        // Metadata values must be strings (Agent Skills spec).
        let mut meta = std::collections::BTreeMap::new();
        for (key, value) in &fm.metadata {
            match value {
                serde_norway::Value::String(s) => {
                    meta.insert(key.as_str(), s.as_str());
                }
                _ => {
                    self.error(
                        "skill-metadata-type",
                        path,
                        format!("metadata `{key}` must be a string; quote it"),
                    );
                    ok = false;
                }
            }
        }
        for key in REQUIRED_SKILL_METADATA {
            if !fm.metadata.contains_key(key) {
                self.error(
                    "skill-metadata-missing",
                    path,
                    format!("metadata `{key}` is required"),
                );
                ok = false;
            }
        }
        if !ok {
            return None;
        }

        let owner = meta["owner"].to_string();
        self.check_owner(path, &owner);

        let version = meta["version"].to_string();
        if !is_semver(&version) {
            self.error(
                "skill-version",
                path,
                format!("version `{version}` is not MAJOR.MINOR.PATCH"),
            );
            ok = false;
        }
        let model = ModelRole::parse(meta["model"]);
        if model.is_none() {
            self.error(
                "skill-model",
                path,
                format!(
                    "model `{}` is not one of: {}",
                    meta["model"],
                    ModelRole::ALL.join(", ")
                ),
            );
            ok = false;
        }
        let data_class = Sensitivity::parse(meta["data-class"]);
        if data_class.is_none() {
            self.error(
                "skill-data-class",
                path,
                format!(
                    "data-class `{}` is not one of: {}",
                    meta["data-class"],
                    Sensitivity::ALL.join(", ")
                ),
            );
            ok = false;
        }

        let tools: Vec<String> = meta["tools"].split_whitespace().map(String::from).collect();
        let mut seen = BTreeSet::new();
        for tool in &tools {
            if !seen.insert(tool.as_str()) {
                self.error(
                    "skill-tool-duplicate",
                    path,
                    format!("tool `{tool}` is declared twice"),
                );
                ok = false;
            }
            if let Some(registry) = registry {
                if registry.get(tool).is_none() {
                    self.error(
                        "skill-unknown-tool",
                        path,
                        format!("tool `{tool}` is not in tools.yaml"),
                    );
                    ok = false;
                }
            }
        }

        let surfaces: Vec<String> = meta
            .get("surfaces")
            .map(|s| s.split_whitespace().map(String::from).collect())
            .unwrap_or_else(|| SURFACES.iter().map(|s| s.to_string()).collect());
        for surface in &surfaces {
            if !SURFACES.contains(&surface.as_str()) {
                self.error(
                    "skill-surface",
                    path,
                    format!("surface `{surface}` is not one of: {}", SURFACES.join(", ")),
                );
                ok = false;
            }
        }

        let ships_in = meta.get("ships-in").map(|s| s.to_string());
        if let Some(phase) = &ships_in {
            if !PHASES.contains(&phase.as_str()) {
                self.error(
                    "skill-phase",
                    path,
                    format!("ships-in `{phase}` is not one of: {}", PHASES.join(", ")),
                );
                ok = false;
            }
        }

        // A catalog heading (the Slack App Home groups skills by it): optional, but not empty.
        let track = meta.get("track").map(|s| s.trim().to_string());
        if let Some(track) = &track {
            if track.is_empty() {
                self.error(
                    "skill-track",
                    path,
                    "track is empty; set it or leave it out",
                );
                ok = false;
            } else if track.chars().count() > MAX_SKILL_TRACK_LEN {
                self.error(
                    "skill-track",
                    path,
                    format!("track is longer than {MAX_SKILL_TRACK_LEN} characters"),
                );
                ok = false;
            }
        }

        if !ok {
            return None;
        }
        Some(Skill {
            name: fm.name,
            description: description.to_string(),
            owner,
            version,
            model: model?,
            data_class: data_class?,
            tools,
            surfaces,
            ships_in,
            track,
        })
    }

    fn check_context(&mut self, opts: &LintOptions) {
        let dir = self.root.join("context");
        if !dir.is_dir() {
            self.error("context-missing", &dir, "context/ directory is missing");
            return;
        }
        let mut files: Vec<PathBuf> = WalkDir::new(&dir)
            .min_depth(1)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_file())
            .map(|e| e.into_path())
            .filter(|p| p.extension().is_some_and(|x| x == "md"))
            .collect();
        files.sort();

        for path in files {
            if path.file_name().is_some_and(|n| n == "README.md") {
                continue;
            }
            let rel = path.strip_prefix(&dir).unwrap_or(&path);
            let domain = rel
                .components()
                .next()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .unwrap_or_default();
            if rel.components().count() < 2 || !CONTEXT_DOMAINS.contains(&domain.as_str()) {
                self.error(
                    "context-domain",
                    &path,
                    format!(
                        "context files must sit under one of: {}",
                        CONTEXT_DOMAINS.join(", ")
                    ),
                );
                continue;
            }
            let Some((fm, body)) =
                self.read_front_matter::<ContextFrontMatter>(&path, "context-parse")
            else {
                continue;
            };
            self.check_owner(&path, &fm.owner);
            if body.trim().is_empty() {
                self.warn(
                    "context-body-empty",
                    &path,
                    "context file has no content yet",
                );
            }
            let age = (opts.today - fm.last_reviewed).num_days();
            if age < 0 {
                self.error(
                    "context-future-date",
                    &path,
                    format!("last-reviewed {} is in the future", fm.last_reviewed),
                );
            } else if age > opts.stale_after_days {
                self.warn(
                    "context-stale",
                    &path,
                    format!(
                        "last reviewed {} ({age} days ago; limit {})",
                        fm.last_reviewed, opts.stale_after_days
                    ),
                );
            }
        }
    }

    fn check_reviewers(&mut self) {
        let dir = self.root.join("reviewers");
        let Some(entries) = sorted_entries(&dir) else {
            self.error("reviewers-missing", &dir, "reviewers/ directory is missing");
            return;
        };
        for path in markdown_files(entries) {
            let Some((fm, body)) =
                self.read_front_matter::<ReviewerFrontMatter>(&path, "reviewer-parse")
            else {
                continue;
            };
            if let Some(owner) = &fm.owner {
                self.check_owner(&path, owner);
            }
            if fm.flags.is_empty() {
                self.error("reviewer-flags", &path, "reviewer lists no typical flags");
            }
            if body.trim().is_empty() {
                self.error("reviewer-body-empty", &path, "reviewer has no rubric");
            }
        }
    }

    fn check_templates(&mut self) {
        let dir = self.root.join("templates");
        let Some(entries) = sorted_entries(&dir) else {
            self.error("templates-missing", &dir, "templates/ directory is missing");
            return;
        };
        for path in markdown_files(entries) {
            let Some((fm, body)) =
                self.read_front_matter::<TemplateFrontMatter>(&path, "template-parse")
            else {
                continue;
            };
            self.check_owner(&path, &fm.owner);
            if !is_semver(&fm.version) {
                self.error(
                    "template-version",
                    &path,
                    format!("version `{}` is not MAJOR.MINOR.PATCH", fm.version),
                );
            }
            if body.trim().is_empty() {
                self.error("template-body-empty", &path, "template is empty");
            }
        }
    }

    /// `playbooks/*.md`: the craft guidance skills read before drafting. The folder is
    /// optional; each file needs `owner` and `version` and a body.
    fn check_playbooks(&mut self) {
        let Some(entries) = sorted_entries(&self.root.join("playbooks")) else {
            return;
        };
        for path in markdown_files(entries) {
            let Some((fm, body)) =
                self.read_front_matter::<PlaybookFrontMatter>(&path, "playbook-parse")
            else {
                continue;
            };
            self.check_owner(&path, &fm.owner);
            if body.trim().is_empty() {
                self.error("playbook-body-empty", &path, "playbook is empty");
            }
        }
    }
}

impl Cx {
    /// `roles/<role>/ROLE.md` and `bots/<name>/BOT.md` (both optional folders).
    fn check_roles_and_bots(&mut self, registry: Option<&ToolRegistry>) {
        use crate::bots::{valid_name, ActionSetting, BotFrontMatter, BotScope, RoleFrontMatter};

        // Each outbound setting must name a write tool; `auto` waits for the benchmark (D7, D10).
        let check_actions =
            |cx: &mut Cx,
             path: &Path,
             actions: &std::collections::BTreeMap<String, ActionSetting>| {
                for (tool, setting) in actions {
                    match registry.and_then(|r| r.get(tool)) {
                        Some(t) if t.kind == crate::model::ToolKind::Write => {}
                        Some(_) => cx.error(
                            "action-not-write",
                            path,
                            format!("`{tool}` is not an outbound (write) tool"),
                        ),
                        None if registry.is_some() => cx.error(
                            "action-unknown-tool",
                            path,
                            format!("`{tool}` is not in tools.yaml"),
                        ),
                        None => {}
                    }
                    if *setting == ActionSetting::Auto {
                        cx.error(
                        "action-auto",
                        path,
                        format!(
                            "`{tool}: auto` is not allowed in v1: every outbound action is approve or never"
                        ),
                    );
                    }
                }
            };

        let mut roles = std::collections::BTreeMap::new();
        let roles_dir = self.root.join("roles");
        if let Some(entries) = sorted_entries(&roles_dir) {
            for dir in entries.into_iter().filter(|p| p.is_dir()) {
                let path = dir.join("ROLE.md");
                let dir_name = dir
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if !path.is_file() {
                    self.error("role-missing-file", &dir, "role directory has no ROLE.md");
                    continue;
                }
                let Some((fm, body)) =
                    self.read_front_matter::<RoleFrontMatter>(&path, "role-parse")
                else {
                    continue;
                };
                if fm.name != dir_name || !valid_name(&fm.name) {
                    self.error("role-name", &path, format!("name `{}` must equal the folder name and use lowercase letters, digits and single hyphens", fm.name));
                }
                self.check_owner(&path, &fm.owner);
                if !is_semver(&fm.version) {
                    self.error(
                        "role-version",
                        &path,
                        format!("version `{}` is not MAJOR.MINOR.PATCH", fm.version),
                    );
                }
                if fm.description.trim().is_empty() {
                    self.error("role-description", &path, "description is empty");
                }
                if body.trim().is_empty() {
                    self.error("role-body-empty", &path, "role has no instructions");
                }
                check_actions(self, &path, &fm.actions);
                let Some(role) = fm.clone().into_role(body) else {
                    self.error(
                        "role-parse",
                        &path,
                        format!(
                            "data-class `{}` or model `{}` is not valid",
                            fm.data_class, fm.model
                        ),
                    );
                    continue;
                };
                if role.skills.is_empty() {
                    self.error("role-skills", &path, "role lists no skills");
                }
                for name in &role.skills {
                    match self.report.skills.iter().find(|s| &s.name == name) {
                        None => self.error(
                            "role-skill-unknown",
                            &path,
                            format!("skill `{name}` is not in skills/"),
                        ),
                        Some(skill) if skill.data_class > role.data_class => self.error(
                            "role-skill-data-class",
                            &path,
                            format!(
                                "skill `{name}` handles {} data, above the role's {}",
                                skill.data_class, role.data_class
                            ),
                        ),
                        Some(_) => {}
                    }
                }
                roles.insert(role.name.clone(), role);
            }
        }

        let bots_dir = self.root.join("bots");
        if let Some(entries) = sorted_entries(&bots_dir) {
            for dir in entries.into_iter().filter(|p| p.is_dir()) {
                let path = dir.join("BOT.md");
                let dir_name = dir
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if !path.is_file() {
                    self.error("bot-missing-file", &dir, "bot directory has no BOT.md");
                    continue;
                }
                let Some((fm, body)) = self.read_front_matter::<BotFrontMatter>(&path, "bot-parse")
                else {
                    continue;
                };
                if fm.name != dir_name || !valid_name(&fm.name) {
                    self.error("bot-name", &path, format!("name `{}` must equal the folder name and use lowercase letters, digits and single hyphens", fm.name));
                }
                if fm.description.trim().is_empty() {
                    self.error("bot-description", &path, "description is empty");
                }
                if body.trim().is_empty() {
                    self.error("bot-body-empty", &path, "bot has no job description");
                }
                self.check_owner(&path, &fm.owner);
                check_actions(self, &path, &fm.actions);
                let bot = fm.into_bot(body);
                // Who the owner is, and whether they may still use PM OS, is the user
                // database's to say (the tool server's overview lists bots whose owner can't;
                // a personal bot serves only its owner, while active). Here the owner must at
                // least be a handle.
                if !bot.owner.is_empty()
                    && !bot.owner.to_ascii_uppercase().starts_with("TODO")
                    && !crate::model::valid_handle(&bot.owner)
                {
                    self.error(
                        "bot-owner-handle",
                        &path,
                        format!(
                            "owner `{}` is not a PM OS handle (lowercase letters, digits, `.`, `_`, `-`)",
                            bot.owner
                        ),
                    );
                }
                match roles.get(&bot.role) {
                    None => self.error(
                        "bot-role-unknown",
                        &path,
                        format!("role `{}` is not in roles/", bot.role),
                    ),
                    Some(role) => {
                        if bot.scope == BotScope::Personal && !bot.reads.is_empty() {
                            self.error(
                                "bot-reads-personal",
                                &path,
                                "a personal bot reads as its owner; `reads` is only for team bots",
                            );
                        }
                        // A team bot that reads Jira, Confluence or Slack needs a chosen scope.
                        let tools = crate::bots::role_tools(role, &self.report.skills);
                        let reads_systems = tools.iter().any(|t| {
                            t.starts_with("jira.")
                                || t.starts_with("confluence.")
                                || t.starts_with("slack.")
                        });
                        if bot.scope == BotScope::Team && reads_systems && bot.reads.is_empty() {
                            self.warn("bot-reads-empty", &path, "this team bot can read Jira, Confluence or Slack but lists nothing under `reads`, so it will read nothing there");
                        }
                    }
                }
                self.report.bots.push(bot);
            }
        }
        self.report.roles = roles.into_values().collect();
    }

    /// `team.yaml` is retired (slice U7): users and roles are in the PM OS user database, and
    /// nothing reads the file. One left in the content tree would look like the truth to a
    /// reader, and a pull request to it like a role grant, so `pmos lint` refuses one anywhere
    /// in the content tree. The file is not read here, only found.
    fn check_team_file_retired(&mut self, opts: &LintOptions) {
        let found: Vec<PathBuf> = WalkDir::new(&self.root)
            .sort_by_file_name()
            .into_iter()
            .filter_entry(|e| e.file_name() != ".git")
            .filter_map(Result::ok)
            .filter(|e| !e.file_type().is_dir() && e.file_name() == RETIRED_TEAM_FILE)
            .map(|e| e.into_path())
            .collect();
        for path in found {
            self.push(
                opts.retired_team_file,
                "team-file-retired",
                &path,
                "team.yaml is retired: users and roles are in the PM OS user database, and \
                 nothing reads this file. Import it once on the host (`pmos-admin users import \
                 --from <file>`, docs/runbooks/users.md), then delete it"
                    .to_string(),
            );
        }
    }

    /// A personal master-context folder is named after its owner's handle
    /// (`master/people/<handle>/`). Whether that person is a user is the database's to say.
    fn check_people(&mut self) {
        if let Some(people) = sorted_entries(&self.root.join("master").join("people")) {
            for dir in people.into_iter().filter(|p| p.is_dir()) {
                let name = dir
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if !crate::model::valid_handle(&name) {
                    self.warn(
                        "people-handle",
                        &dir,
                        format!(
                            "`{name}` is not a PM OS handle, so no one's personal context is read from it"
                        ),
                    );
                }
            }
        }
    }
}

fn sorted_entries(dir: &Path) -> Option<Vec<PathBuf>> {
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .collect();
    entries.sort();
    Some(entries)
}

fn markdown_files(entries: Vec<PathBuf>) -> impl Iterator<Item = PathBuf> {
    entries.into_iter().filter(|p| {
        p.is_file()
            && p.extension().is_some_and(|x| x == "md")
            && p.file_name().is_some_and(|n| n != "README.md")
    })
}

/// Agent Skills spec: 1–64 characters, lowercase letters, digits and single hyphens, no
/// leading or trailing hyphen, and equal to the skill's directory name.
pub fn validate_skill_name(name: &str, dir_name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("name is empty".into());
    }
    if name.len() > MAX_SKILL_NAME_LEN {
        return Err(format!(
            "name is longer than {MAX_SKILL_NAME_LEN} characters"
        ));
    }
    let valid_chars = name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !valid_chars || name.starts_with('-') || name.ends_with('-') || name.contains("--") {
        return Err(format!(
            "name `{name}` must be lowercase letters, digits and single hyphens"
        ));
    }
    if name != dir_name {
        return Err(format!(
            "name `{name}` must match its directory name `{dir_name}`"
        ));
    }
    Ok(())
}

/// Tool names are `<system>.<action>`, such as `jira.search`: lowercase segments of letters,
/// digits and underscores, the first equal to the tool's system.
pub fn validate_tool_name(name: &str, system: &str) -> Result<(), String> {
    let segments: Vec<&str> = name.split('.').collect();
    let valid = segments.len() >= 2
        && segments.iter().all(|s| {
            !s.is_empty()
                && s.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        });
    if !valid {
        return Err("name must be `<system>.<action>` in lowercase".into());
    }
    if segments[0] != system {
        return Err(format!("name must start with its system `{system}.`"));
    }
    Ok(())
}

fn is_semver(v: &str) -> bool {
    let parts: Vec<&str> = v.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_names() {
        assert!(validate_skill_name("prd-draft", "prd-draft").is_ok());
        assert!(validate_skill_name("ask", "ask").is_ok());
        assert!(validate_skill_name("PRD", "PRD").is_err());
        assert!(validate_skill_name("prd--draft", "prd--draft").is_err());
        assert!(validate_skill_name("-prd", "-prd").is_err());
        assert!(validate_skill_name("prd_draft", "prd_draft").is_err());
        assert!(validate_skill_name("prd-draft", "prd").is_err());
        let long = "a".repeat(65);
        assert!(validate_skill_name(&long, &long).is_err());
    }

    #[test]
    fn tool_names() {
        assert!(validate_tool_name("jira.search", "jira").is_ok());
        assert!(validate_tool_name("confluence.create_draft", "confluence").is_ok());
        assert!(validate_tool_name("jira", "jira").is_err());
        assert!(validate_tool_name("Jira.search", "Jira").is_err());
        assert!(validate_tool_name("slack.post", "jira").is_err());
        assert!(validate_tool_name("jira..search", "jira").is_err());
    }

    #[test]
    fn semver() {
        assert!(is_semver("0.1.0"));
        assert!(!is_semver("0.1"));
        assert!(!is_semver("v0.1.0"));
        assert!(!is_semver("0.1.x"));
    }
}
