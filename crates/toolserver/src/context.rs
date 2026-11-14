//! The context library as the tool server serves it.
//!
//! [`ContextIndex`] is the seam for the Postgres index (pgvector plus full-text search) that
//! replaces [`MemoryIndex`] in 0c-2. Both serve only files under `content/context/`, so a
//! path a caller sends can never reach anything else on disk.

use std::collections::BTreeMap;
use std::path::Path;

use chrono::NaiveDate;
use pmos_contract::frontmatter;
use pmos_contract::model::parse_context_front_matter;
use pmos_contract::Sensitivity;
use serde::Serialize;
use walkdir::WalkDir;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContextDoc {
    /// Path relative to the content root, such as `context/products/ccm.md`. Also the
    /// citation reference.
    pub path: String,
    pub domain: String,
    pub title: String,
    pub owner: String,
    pub last_reviewed: NaiveDate,
    pub sensitivity: Sensitivity,
    #[serde(skip)]
    pub body: String,
}

impl ContextDoc {
    pub fn is_stale(&self, today: NaiveDate, stale_after_days: i64) -> bool {
        (today - self.last_reviewed).num_days() > stale_after_days
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Hit {
    pub doc: ContextDoc,
    pub score: f32,
    pub snippet: String,
}

pub trait ContextIndex: Send + Sync {
    /// Best matches for `query`, highest score first, optionally within one domain.
    fn search(&self, query: &str, domain: Option<&str>, limit: usize) -> Vec<Hit>;
    /// The document at `path`, if it is an indexed context file.
    fn get(&self, path: &str) -> Option<ContextDoc>;
}

/// Keyword index over the context files, built at start-up.
#[derive(Default)]
pub struct MemoryIndex {
    docs: BTreeMap<String, ContextDoc>,
}

impl MemoryIndex {
    /// Index every context file under `<content_root>/context/` that has valid front matter.
    /// Files the lint would reject are skipped; the server refuses to start on lint errors,
    /// so in practice nothing is skipped.
    pub fn load(content_root: &Path) -> Self {
        let dir = content_root.join("context");
        let mut docs = BTreeMap::new();
        for entry in WalkDir::new(&dir)
            .min_depth(2)
            .into_iter()
            .filter_map(Result::ok)
        {
            let path = entry.path();
            if !entry.file_type().is_file()
                || path.extension().is_none_or(|x| x != "md")
                || path.file_name().is_some_and(|n| n == "README.md")
            {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(path) else {
                continue;
            };
            let Ok(doc) = frontmatter::split(&text) else {
                continue;
            };
            let Ok(fm) = parse_context_front_matter(doc.yaml) else {
                continue;
            };
            let rel = path
                .strip_prefix(content_root)
                .unwrap_or(path)
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            let domain = rel.split('/').nth(1).unwrap_or_default().to_string();
            let title = fm
                .title
                .clone()
                .or_else(|| first_heading(doc.body))
                .unwrap_or_else(|| rel.clone());
            docs.insert(
                rel.clone(),
                ContextDoc {
                    path: rel,
                    domain,
                    title,
                    owner: fm.owner,
                    last_reviewed: fm.last_reviewed,
                    sensitivity: fm.sensitivity,
                    body: doc.body.to_string(),
                },
            );
        }
        Self { docs }
    }

    pub fn len(&self) -> usize {
        self.docs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }
}

fn first_heading(body: &str) -> Option<String> {
    body.lines()
        .find_map(|l| l.strip_prefix("# "))
        .map(|s| s.trim().to_string())
}

fn terms(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() > 1)
        .map(str::to_lowercase)
        .collect()
}

impl ContextIndex for MemoryIndex {
    fn search(&self, query: &str, domain: Option<&str>, limit: usize) -> Vec<Hit> {
        let query_terms = terms(query);
        if query_terms.is_empty() {
            return Vec::new();
        }
        let mut hits: Vec<Hit> = self
            .docs
            .values()
            .filter(|d| domain.is_none_or(|want| d.domain == want))
            .filter_map(|doc| {
                let title = terms(&doc.title);
                let body = terms(&doc.body);
                let mut score = 0.0;
                for q in &query_terms {
                    score += 3.0 * title.iter().filter(|t| *t == q).count() as f32;
                    score += body.iter().filter(|t| *t == q).count() as f32;
                }
                (score > 0.0).then(|| Hit {
                    snippet: snippet(&doc.body, &query_terms),
                    doc: doc.clone(),
                    score,
                })
            })
            .collect();
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.doc.path.cmp(&b.doc.path))
        });
        hits.truncate(limit);
        hits
    }

    fn get(&self, path: &str) -> Option<ContextDoc> {
        self.docs.get(path).cloned()
    }
}

/// The first non-empty body line that mentions a query term, cut to 200 characters.
fn snippet(body: &str, query_terms: &[String]) -> String {
    let line = body
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("<!--"))
        .find(|l| {
            let lower = l.to_lowercase();
            query_terms.iter().any(|q| lower.contains(q.as_str()))
        })
        .unwrap_or_default();
    let mut out: String = line.chars().take(200).collect();
    if line.chars().count() > 200 {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, text: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn doc(title: &str, sensitivity: &str, body: &str) -> String {
        format!("---\ntitle: {title}\nowner: \"@pm\"\nlast-reviewed: 2026-11-01\nsensitivity: {sensitivity}\n---\n\n{body}\n")
    }

    #[test]
    fn indexes_and_ranks() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "context/products/ccm.md",
            &doc("CCM", "internal", "Key management for clouds."),
        );
        write(
            root,
            "context/competitors/acme.md",
            &doc("Acme", "internal", "Acme sells key management. Key key."),
        );
        write(root, "context/products/README.md", "# Products\n");
        write(root, "context/loose.md", &doc("Loose", "internal", "key"));
        let index = MemoryIndex::load(root);
        assert_eq!(
            index.len(),
            2,
            "README and files outside a domain are skipped"
        );

        let hits = index.search("key management", None, 10);
        assert_eq!(hits[0].doc.path, "context/competitors/acme.md");
        assert_eq!(hits[1].doc.path, "context/products/ccm.md");
        assert!(hits[0].snippet.contains("key management"));

        let hits = index.search("key", Some("products"), 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].doc.domain, "products");

        assert!(index.search("", None, 10).is_empty());
        assert!(index.get("context/products/ccm.md").is_some());
        assert!(index.get("../secrets.md").is_none());
    }

    #[test]
    fn title_falls_back_to_heading() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "context/glossary/terms.md",
            "---\nowner: \"@pm\"\nlast-reviewed: 2026-11-01\nsensitivity: public\n---\n\n# Glossary\n",
        );
        let index = MemoryIndex::load(dir.path());
        assert_eq!(
            index.get("context/glossary/terms.md").unwrap().title,
            "Glossary"
        );
    }
}
