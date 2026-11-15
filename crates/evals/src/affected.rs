//! Which categories a change affects, for the eval gate in CI (`pmos-evals affected`).
//!
//! - A change under `evals/`, or to `deploy/gateway.yaml`, affects every category.
//! - Otherwise a category is affected when the change touches its bot's `BOT.md` directory
//!   (`content/bots/<bot>/`), its role's directory (`content/roles/<role>/`), or the directory
//!   of any skill the role runs (`content/skills/<skill>/`).
//!
//! For `bot: personal` the bot is `pm-<requested_by>`. If that bot has no `BOT.md` in the
//! content root, its role is `pm-assistant`, the role every personal bot is made from.

use std::path::Path;

use pmos_contract::frontmatter;
use serde::Deserialize;

use crate::spec::{category_names, load_category, resolve_bot, PERSONAL_BOT};

/// The role every personal bot is made from (`pmos_contract::onboarding::render_personal_bot`).
pub const PERSONAL_ROLE: &str = "pm-assistant";

#[derive(Deserialize)]
struct BotFront {
    role: String,
}

#[derive(Deserialize)]
struct RoleFront {
    skills: String,
}

fn front<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("{}: can't read: {e}", path.display()))?;
    let doc = frontmatter::split(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_norway::from_str(doc.yaml).map_err(|e| format!("{}: {e}", path.display()))
}

/// The content directories a category depends on, relative to the repository root, each
/// ending in `/`: its bot's, its role's and its role's skills'.
pub fn dependencies(
    content: &Path,
    content_prefix: &str,
    bot: &str,
    requested_by: &str,
) -> Result<Vec<String>, String> {
    let resolved = resolve_bot(bot, requested_by);
    let bot_md = content.join("bots").join(&resolved).join("BOT.md");
    let role = if bot_md.is_file() {
        front::<BotFront>(&bot_md)?.role
    } else if bot == PERSONAL_BOT {
        PERSONAL_ROLE.to_string()
    } else {
        return Err(format!(
            "{}: no such bot (category bot `{bot}`)",
            bot_md.display()
        ));
    };
    let role_md = content.join("roles").join(&role).join("ROLE.md");
    let skills = front::<RoleFront>(&role_md)?.skills;
    let p = content_prefix.trim_end_matches('/');
    let mut deps = vec![
        format!("{p}/bots/{resolved}/"),
        format!("{p}/roles/{role}/"),
    ];
    deps.extend(
        skills
            .split_whitespace()
            .map(|s| format!("{p}/skills/{s}/")),
    );
    Ok(deps)
}

/// The categories under `root` that the changed paths (relative to the repository root)
/// affect, sorted. `content_prefix` is the content root's path in the repository, `content`.
pub fn affected(
    root: &Path,
    content: &Path,
    content_prefix: &str,
    requested_by: &str,
    changed: &[String],
) -> Result<Vec<String>, String> {
    let names = category_names(root).map_err(|e| e.to_string())?;
    let changed: Vec<&str> = changed
        .iter()
        .map(|p| p.trim().trim_start_matches("./"))
        .filter(|p| !p.is_empty())
        .collect();
    if changed
        .iter()
        .any(|p| p.starts_with("evals/") || *p == "deploy/gateway.yaml")
    {
        return Ok(names);
    }
    let mut out = Vec::new();
    for name in names {
        let category = load_category(root, &name).map_err(|e| e.to_string())?;
        let deps = dependencies(content, content_prefix, &category.bot, requested_by)?;
        if changed
            .iter()
            .any(|p| deps.iter().any(|d| p.starts_with(d.as_str())))
        {
            out.push(name);
        }
    }
    Ok(out)
}
