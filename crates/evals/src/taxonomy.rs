//! The failure taxonomy: `evals/taxonomy.yaml`, second-level tags under the five bottlenecks.
//!
//! ```yaml
//! model:
//!   description: The bot had what it needed and got it wrong.
//!   tags:
//!     missing-citation: The answer doesn't link a page it had to cite.
//! ```
//!
//! A sub-tag is written in full as `<bottleneck>/<name>`, such as `model/missing-citation`.
//! Every sub-tag the automatic tagger emits ([`crate::tag::SUB_TAGS`]) must be listed.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

use crate::spec::is_slug;
use crate::tag::{Tag, SUB_TAGS};

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Group {
    pub description: String,
    #[serde(default)]
    pub tags: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Taxonomy {
    pub groups: BTreeMap<String, Group>,
}

impl Taxonomy {
    /// Load `<root>/taxonomy.yaml` and validate it. Errors are `path: message`, all at once.
    pub fn load(root: &Path) -> Result<Taxonomy, Vec<String>> {
        let path = root.join("taxonomy.yaml");
        let shown = path.display().to_string();
        let text = std::fs::read_to_string(&path)
            .map_err(|e| vec![format!("{shown}: can't read: {e}")])?;
        Self::parse(&text, &shown)
    }

    pub fn parse(text: &str, shown: &str) -> Result<Taxonomy, Vec<String>> {
        let groups: BTreeMap<String, Group> =
            serde_norway::from_str(text).map_err(|e| vec![format!("{shown}: {e}")])?;
        let t = Taxonomy { groups };
        let errors = t.validate(shown);
        if errors.is_empty() {
            Ok(t)
        } else {
            Err(errors)
        }
    }

    fn validate(&self, shown: &str) -> Vec<String> {
        let mut errors = Vec::new();
        let tops: Vec<&str> = Tag::ALL.iter().map(|t| t.as_str()).collect();
        for key in self.groups.keys() {
            if !tops.contains(&key.as_str()) {
                errors.push(format!(
                    "{shown}: `{key}` is not a bottleneck; the five are {}",
                    tops.join(", ")
                ));
            }
        }
        for top in &tops {
            if !self.groups.contains_key(*top) {
                errors.push(format!("{shown}: `{top}` is missing"));
            }
        }
        for (top, group) in &self.groups {
            if group.description.trim().is_empty() {
                errors.push(format!("{shown}: `{top}` has no description"));
            }
            for (name, description) in &group.tags {
                if !is_slug(name) {
                    errors.push(format!(
                        "{shown}: `{top}/{name}`: a sub-tag is lowercase letters, digits and hyphens"
                    ));
                }
                if description.trim().is_empty() {
                    errors.push(format!("{shown}: `{top}/{name}` has no description"));
                }
            }
        }
        for sub in SUB_TAGS {
            if !self.contains(sub) {
                errors.push(format!(
                    "{shown}: `{sub}` is missing; the automatic tagger emits it"
                ));
            }
        }
        errors
    }

    /// Whether a full sub-tag, such as `model/missing-citation`, is listed.
    pub fn contains(&self, full: &str) -> bool {
        self.describe(full).is_some()
    }

    pub fn describe(&self, full: &str) -> Option<&str> {
        let (top, name) = full.split_once('/')?;
        self.groups.get(top)?.tags.get(name).map(String::as_str)
    }

    /// Every listed sub-tag, in full, sorted.
    pub fn all(&self) -> Vec<String> {
        self.groups
            .iter()
            .flat_map(|(top, g)| g.tags.keys().map(move |n| format!("{top}/{n}")))
            .collect()
    }
}
