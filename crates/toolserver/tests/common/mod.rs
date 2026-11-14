#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use pmos_toolserver::audit::MemoryAudit;
use pmos_toolserver::catalog::Catalog;
use pmos_toolserver::connectors::fake::FakeConnector;
use pmos_toolserver::connectors::Connector;
use pmos_toolserver::context::MemoryIndex;
use pmos_toolserver::memory::InMemoryStore;
use pmos_toolserver::ToolService;
use tempfile::TempDir;

pub fn now() -> DateTime<Utc> {
    "2026-11-14T10:00:00Z".parse().unwrap()
}

pub struct Fixture {
    pub _dir: TempDir,
    pub root: PathBuf,
    pub svc: Arc<ToolService>,
    pub audit: Arc<MemoryAudit>,
    pub jira: Arc<FakeConnector>,
    pub confluence: Arc<FakeConnector>,
    pub slack: Arc<FakeConnector>,
}

fn copy_dir(from: &Path, to: &Path) {
    for entry in walkdir::WalkDir::new(from) {
        let entry = entry.unwrap();
        let rel = entry.path().strip_prefix(from).unwrap();
        let dest = to.join(rel);
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&dest).unwrap();
        } else {
            std::fs::copy(entry.path(), &dest).unwrap();
        }
    }
}

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

/// The repo's content, plus a Restricted context file and an `internal-brief` skill whose
/// data class is `internal`.
pub fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("content");
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content"),
        &root,
    );
    write(
        &root,
        "context/partners/acme-nda.md",
        "---\ntitle: Acme partnership (NDA)\nowner: \"@pm\"\nlast-reviewed: 2026-11-01\nsensitivity: restricted\n---\n\n# Acme\n\nJoint roadmap for key management under NDA.\n",
    );
    write(
        &root,
        "context/products/key-management.md",
        "---\ntitle: Key management overview\nowner: \"@pm\"\nlast-reviewed: 2026-06-01\nsensitivity: internal\n---\n\n# Key management\n\nPublic overview of key management in DSM.\n",
    );
    write(
        &root,
        "skills/internal-brief/SKILL.md",
        "---\nname: internal-brief\ndescription: Test skill limited to internal data.\nmetadata:\n  owner: \"@pm\"\n  version: \"0.1.0\"\n  model: \"default\"\n  data-class: \"internal\"\n  tools: \"context.search context.read jira.search jira.create_issue memory.read memory.update\"\n---\n\n# /internal-brief\n\nTest.\n",
    );

    let catalog = Catalog::load(&root, now().date_naive()).expect("fixture content lints");
    let audit = Arc::new(MemoryAudit::default());
    let jira = Arc::new(FakeConnector::jira());
    let confluence = Arc::new(FakeConnector::confluence());
    let slack = Arc::new(FakeConnector::slack());
    let connectors: Vec<Arc<dyn Connector>> = vec![jira.clone(), confluence.clone(), slack.clone()];
    let svc = ToolService::new(
        catalog,
        Arc::new(MemoryIndex::load(&root)),
        Arc::new(InMemoryStore::default()),
        connectors,
        audit.clone(),
    )
    .unwrap();
    Fixture {
        _dir: dir,
        root,
        svc: Arc::new(svc),
        audit,
        jira,
        confluence,
        slack,
    }
}
