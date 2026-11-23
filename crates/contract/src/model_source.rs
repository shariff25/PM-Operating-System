//! Where prompts go (`PMOS_MODEL_SOURCE`, `docs/openshell-any-host-spec.md` §3.1 and §3.6),
//! and the fake-data rules that follow from it, shared by every service that enforces them.
//!
//! - `local`: a model server on this host, behind a `class: production` gateway. Real data
//!   may be used (after Gate 1).
//! - `hosted-test`: any OpenAI-compatible endpoint nobody approved, behind a `class: test`
//!   gateway. **Fake data only**: every service refuses what would put real data in a prompt
//!   or a database ([`fake_data_refusal`]).
//! - `hosted`: the owner's approved hosted model. Not built (spec slice H12), so refused.
//!
//! The setting is required by every service, with no default: a blank value is refused, never
//! read as the source that allows more.
//!
//! The gateway reports the source it serves on every answer (`x-pmos-model-source`) and in
//! `/readyz`; with its stub model on it reports `stub`, which sends nothing anywhere.
//! Callers state the source they expect on every model call ([`HEADER`]); the gateway refuses
//! a mismatch before it forwards anything.

use std::path::Path;

/// The header the gateway puts on every answer and every caller puts on every model call.
pub const HEADER: &str = "x-pmos-model-source";

/// What a gateway with its stub model on reports: no prompt goes anywhere.
pub const STUB: &str = "stub";

/// The marker file a content checkout needs in fake-data mode (§3.6): committed in that
/// checkout, stating it holds no company data.
pub const TEST_CONTENT_MARKER: &str = "TEST-CONTENT";

/// `PMOS_MODEL_SOURCE`, as this build accepts it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelSource {
    /// A model server on this host.
    Local,
    /// An unapproved hosted endpoint: fake data only.
    HostedTest,
}

impl ModelSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::HostedTest => "hosted-test",
        }
    }

    /// `PMOS_MODEL_SOURCE` as set (`None` when unset). Required: unset or blank is refused,
    /// never read as `local` (§3.1, "Defaults that allow less"); `hosted` is refused until
    /// slice H12 builds it.
    pub fn from_setting(value: Option<&str>) -> Result<Self, String> {
        match value.map(str::trim) {
            None | Some("") => Err(
                "PMOS_MODEL_SOURCE is required: `local` (a model server on this host) or \
                 `hosted-test` (an unapproved hosted endpoint, fake data only); run \
                 deploy/setup.sh, which writes it"
                    .into(),
            ),
            Some("local") => Ok(Self::Local),
            Some("hosted-test") => Ok(Self::HostedTest),
            Some("hosted") => Err(
                "PMOS_MODEL_SOURCE=hosted is not built yet (spec slice H12): use `local` or \
                 `hosted-test`"
                    .into(),
            ),
            Some(other) => Err(format!(
                "PMOS_MODEL_SOURCE must be `local` or `hosted-test`, not `{other}`"
            )),
        }
    }

    /// `PMOS_MODEL_SOURCE` read through `get`, as every service's config does.
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        Self::from_setting(get("PMOS_MODEL_SOURCE").as_deref())
    }

    /// Fake-data mode: nothing real may reach a prompt or a database (§3.6).
    pub fn fake_data_only(self) -> bool {
        self == Self::HostedTest
    }

    /// Whether a gateway reporting `reported` serves this source: the same one, or the stub,
    /// which sends no prompt anywhere.
    pub fn accepts(self, reported: &str) -> bool {
        reported == self.as_str() || reported == STUB
    }
}

/// A source as the gateway reports it: `local`, `hosted-test`, `stub`, or `hosted:<name>`
/// (H12) with a name of lowercase letters, digits and inner hyphens. Anything else is refused
/// by the services that read it.
pub fn valid_reported(s: &str) -> bool {
    match s {
        "local" | "hosted-test" | STUB => true,
        _ => s.strip_prefix("hosted:").is_some_and(|name| {
            !name.is_empty()
                && name.len() <= 64
                && !name.starts_with('-')
                && !name.ends_with('-')
                && name
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        }),
    }
}

/// The start-up refusal for one cause in fake-data mode, in the words §3.6 gives.
pub fn fake_data_refusal(cause: &str) -> String {
    format!(
        "PMOS_MODEL_SOURCE=hosted-test sends prompts to an endpoint nobody approved, so it allows \
         fake data only: {cause}"
    )
}

/// The refusal for a database that has held real data (its `held_real_data` marker).
pub const HELD_REAL_DATA: &str = "this database has held real data";

/// The content check of fake-data mode (§3.6): the content root carries a `TEST-CONTENT`
/// marker file, and no Markdown file under it is marked `sensitivity: restricted` in its
/// front matter. Run by the tool server, the runtime and the web app at start-up and at every
/// content reload in fake-data mode, and by `pmos content check-test` before `pmos-admin
/// sandbox` uploads anything. `Err`: one line per cause, for [`fake_data_refusal`].
///
/// Symbolic links are not followed, and a file that can't be read is a cause too: the check
/// refuses what it can't see.
pub fn check_test_content(root: &Path) -> Result<(), Vec<String>> {
    let mut causes = Vec::new();
    let marker = root.join(TEST_CONTENT_MARKER);
    match std::fs::symlink_metadata(&marker) {
        Ok(m) if m.is_file() => {}
        _ => causes.push(format!(
            "the content root has no {TEST_CONTENT_MARKER} marker (a committed file stating the \
             checkout holds no company data)"
        )),
    }
    if !root.is_dir() {
        causes.push("the content root is not a directory".into());
        return Err(causes);
    }
    for entry in walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| e.file_name() != ".git")
    {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => {
                causes.push("a folder in the content root can't be read".into());
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let rel = path.strip_prefix(root).unwrap_or(path);
        match std::fs::read_to_string(path) {
            Ok(text) if marked_restricted(&text) => {
                causes.push(format!("{} is marked restricted", display(rel)))
            }
            Ok(_) => {}
            Err(_) => causes.push(format!("{} can't be read", display(rel))),
        }
    }
    if causes.is_empty() {
        Ok(())
    } else {
        Err(causes)
    }
}

fn display(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// Whether a Markdown file's front matter has a top-level `sensitivity: restricted`. Read
/// line by line rather than as YAML, so a front matter that doesn't parse still can't hide
/// the marking: any top-level `sensitivity:` line whose value, unquoted and without a
/// comment, is `restricted` counts.
pub fn marked_restricted(text: &str) -> bool {
    let Ok(doc) = crate::frontmatter::split(text) else {
        return false;
    };
    doc.yaml.lines().any(|line| {
        let Some(rest) = line.strip_prefix("sensitivity:") else {
            return false;
        };
        let value = rest.split('#').next().unwrap_or_default().trim();
        let value = value.trim_matches(|c| c == '"' || c == '\'').trim();
        value.eq_ignore_ascii_case("restricted")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_setting_is_required_and_hosted_is_not_built() {
        assert_eq!(
            ModelSource::from_setting(Some("local")),
            Ok(ModelSource::Local)
        );
        assert_eq!(
            ModelSource::from_setting(Some("hosted-test")),
            Ok(ModelSource::HostedTest)
        );
        for blank in [None, Some(""), Some("  ")] {
            let err = ModelSource::from_setting(blank).unwrap_err();
            assert!(err.contains("is required"), "{err}");
        }
        assert!(ModelSource::from_setting(Some("hosted"))
            .unwrap_err()
            .contains("not built yet"));
        assert!(ModelSource::from_setting(Some("Local"))
            .unwrap_err()
            .contains("must be"));
        assert!(ModelSource::HostedTest.fake_data_only());
        assert!(!ModelSource::Local.fake_data_only());
    }

    #[test]
    fn a_source_accepts_itself_and_the_stub_only() {
        assert!(ModelSource::Local.accepts("local"));
        assert!(ModelSource::Local.accepts("stub"));
        assert!(!ModelSource::Local.accepts("hosted-test"));
        assert!(!ModelSource::Local.accepts("hosted:owner-hosted"));
        assert!(ModelSource::HostedTest.accepts("hosted-test"));
        assert!(!ModelSource::HostedTest.accepts("local"));
        assert!(!ModelSource::HostedTest.accepts(""));
    }

    #[test]
    fn reported_sources_are_checked() {
        for ok in [
            "local",
            "hosted-test",
            "stub",
            "hosted:owner-hosted",
            "hosted:a1",
        ] {
            assert!(valid_reported(ok), "{ok}");
        }
        for bad in [
            "",
            "hosted",
            "hosted:",
            "hosted:-a",
            "hosted:A",
            "hosted:a b",
            "https://x",
            "LOCAL",
        ] {
            assert!(!valid_reported(bad), "{bad}");
        }
    }

    #[test]
    fn restricted_marking_is_found_however_it_is_written() {
        for yes in [
            "---\nsensitivity: restricted\n---\nbody",
            "---\ntitle: x\nsensitivity: \"restricted\"   # restricted | internal\n---\n",
            "---\nsensitivity: 'Restricted'\n---\n",
            "---\nsensitivity: restricted\ntitle: [unclosed\n---\n",
        ] {
            assert!(marked_restricted(yes), "{yes}");
        }
        for no in [
            "---\nsensitivity: internal   # restricted | internal | public\n---\n",
            "no front matter\nsensitivity: restricted\n",
            "---\ndata-class: restricted\n---\n",
            "---\n  sensitivity: restricted\n---\n",
        ] {
            assert!(!marked_restricted(no), "{no}");
        }
    }

    #[test]
    fn the_content_check_needs_the_marker_and_no_restricted_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("context/a")).unwrap();
        std::fs::write(
            root.join("context/a/ok.md"),
            "---\nsensitivity: internal\n---\nx",
        )
        .unwrap();
        let err = check_test_content(root).unwrap_err();
        assert_eq!(err.len(), 1, "{err:?}");
        assert!(err[0].contains("no TEST-CONTENT marker"), "{err:?}");
        std::fs::write(root.join(TEST_CONTENT_MARKER), "test data only\n").unwrap();
        assert_eq!(check_test_content(root), Ok(()));
        std::fs::write(
            root.join("context/a/secret.md"),
            "---\nsensitivity: restricted\n---\nx",
        )
        .unwrap();
        let err = check_test_content(root).unwrap_err();
        assert_eq!(err, vec!["context/a/secret.md is marked restricted"]);
        // A marker that is a folder is no marker.
        let dir2 = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir2.path().join(TEST_CONTENT_MARKER)).unwrap();
        assert!(check_test_content(dir2.path()).is_err());
        // A missing root is refused.
        assert!(check_test_content(&root.join("nope")).is_err());
    }

    #[test]
    fn the_committed_seed_content_passes() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content");
        assert_eq!(check_test_content(&root), Ok(()));
    }
}
