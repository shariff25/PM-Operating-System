//! Who may change the approved hosted list (`docs/openshell-any-host-spec.md` §3.6, "Who may
//! change the list", r6 V1 and r7 X2): the owner alone.
//!
//! One parser, used by both checks that must agree:
//!
//! - the CI lint `codeowners_hosted_list_is_owner_only` (`crates/contract/tests/lint.rs`),
//!   which checks `.github/CODEOWNERS` against the handle in `.github/owner-handle`, and the
//!   CODEOWNERS block in `policies/README.md` against its `@<owner>` placeholder;
//! - the approved list's own test in `crates/gateway`, which fails on any entry unless both
//!   files exist and hold the owner-only lines ([`hosted_list_may_have_entries`]).
//!
//! GitHub applies the **last** matching CODEOWNERS line, so for each of [`OWNER_ONLY_PATHS`]
//! the last line that matches it (and every file under it, for a folder) must name exactly the
//! owner's handle and nothing else.

use std::path::Path;

/// The paths only the owner may approve changes to (§3.6), as CODEOWNERS patterns.
pub const OWNER_ONLY_PATHS: [&str; 4] = [
    "/.github/CODEOWNERS",
    "/.github/owner-handle",
    "/crates/gateway/approved-hosted.yaml",
    "/docs/model-providers/",
];

/// The placeholder `policies/README.md`'s block uses for the owner, as `@<org>/<team>` stands
/// for the platform owners.
pub const OWNER_PLACEHOLDER: &str = "@<owner>";

/// Files below a folder pattern that the check tries, so a later line matching only some of
/// what is in the folder (`*.md`, `/docs/`) is caught too.
const FOLDER_SAMPLES: [&str; 3] = ["README.md", "record.md", "a/b.txt"];

/// The handle in `.github/owner-handle`: one line, `@` and a GitHub user name (letters, digits
/// and single hyphens, not at either end, at most 39 characters). Anything else, a team
/// (`@org/team`), an e-mail address or a second line included, is refused.
pub fn parse_owner_handle(text: &str) -> Result<String, String> {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let [line] = lines.as_slice() else {
        return Err(
            ".github/owner-handle must hold exactly one line, `@<GitHub user name>`".into(),
        );
    };
    let handle = line.trim();
    if !valid_user_handle(handle) {
        return Err(
            ".github/owner-handle must be `@` and one GitHub user name (letters, digits and \
             single hyphens, at most 39 characters), not a team or an e-mail address"
                .into(),
        );
    }
    Ok(handle.to_string())
}

/// `@` and a GitHub user name.
pub fn valid_user_handle(handle: &str) -> bool {
    let Some(name) = handle.strip_prefix('@') else {
        return false;
    };
    !name.is_empty()
        && name.len() <= 39
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--")
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// One CODEOWNERS line: its pattern and its owners, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub number: usize,
    pub pattern: String,
    pub owners: Vec<String>,
}

/// The CODEOWNERS lines of `text`: blank lines and comments dropped, a trailing comment cut.
pub fn parse(text: &str) -> Vec<Line> {
    text.lines()
        .enumerate()
        .filter_map(|(i, raw)| {
            let line = raw.split(" #").next().unwrap_or_default().trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let mut words = line.split_whitespace();
            let pattern = words.next()?.to_string();
            Some(Line {
                number: i + 1,
                pattern,
                owners: words.map(String::from).collect(),
            })
        })
        .collect()
}

/// Whether CODEOWNERS `pattern` matches the repository file `path` (no leading `/`), as
/// GitHub reads it (gitignore rules, without `!` or `[]`, which CODEOWNERS doesn't take):
/// a leading `/` anchors at the root, a pattern with no `/` but a trailing one matches at any
/// depth, a trailing `/` matches everything under the folder, `*` stays within one path part,
/// `**` crosses parts, and a pattern naming a folder matches what is under it.
pub fn matches(pattern: &str, path: &str) -> bool {
    let path = path.trim_start_matches('/');
    let dir_only = pattern.ends_with('/');
    let body = pattern.trim_end_matches('/');
    let anchored = body.starts_with('/') || body.trim_start_matches('/').contains('/');
    let body = body.trim_start_matches('/');
    if body.is_empty() {
        return true;
    }
    let pat: Vec<&str> = body.split('/').collect();
    let parts: Vec<&str> = path.split('/').collect();
    // Where the pattern may start: the root only when anchored, else any part.
    let starts: Vec<usize> = if anchored {
        vec![0]
    } else {
        (0..parts.len()).collect()
    };
    starts.into_iter().any(|s| {
        let rest = &parts[s..];
        // The pattern matches a prefix of the path's parts: the whole path (a file, unless
        // the pattern is a folder only) or a folder above it.
        (1..=rest.len()).any(|n| {
            let whole = n == rest.len();
            if dir_only && whole {
                return false;
            }
            match_parts(&pat, &rest[..n])
        })
    })
}

fn match_parts(pat: &[&str], parts: &[&str]) -> bool {
    match (pat.first(), parts.first()) {
        (None, None) => true,
        (Some(&"**"), _) => {
            match_parts(&pat[1..], parts) || (!parts.is_empty() && match_parts(pat, &parts[1..]))
        }
        (Some(p), Some(q)) => glob(p, q) && match_parts(&pat[1..], &parts[1..]),
        _ => false,
    }
}

/// `*` and `?` within one path part.
fn glob(p: &str, s: &str) -> bool {
    let (p, s): (Vec<char>, Vec<char>) = (p.chars().collect(), s.chars().collect());
    fn go(p: &[char], s: &[char]) -> bool {
        match (p.first(), s.first()) {
            (None, None) => true,
            (Some('*'), _) => go(&p[1..], s) || (!s.is_empty() && go(p, &s[1..])),
            (Some('?'), Some(_)) => go(&p[1..], &s[1..]),
            (Some(a), Some(b)) => a == b && go(&p[1..], &s[1..]),
            _ => false,
        }
    }
    go(&p, &s)
}

/// The problems with `text` (a CODEOWNERS file, or the block in `policies/README.md`) for the
/// owner-only rule, with `owner` the one owner every line for [`OWNER_ONLY_PATHS`] must name
/// (the recorded handle, or [`OWNER_PLACEHOLDER`] for the block). Empty: every path's last
/// matching line names exactly `owner`.
pub fn owner_only_problems(text: &str, owner: &str) -> Vec<String> {
    let lines = parse(text);
    let mut problems = Vec::new();
    for path in OWNER_ONLY_PATHS {
        if !lines.iter().any(|l| l.pattern == path) {
            problems.push(format!("no line for {path}"));
            continue;
        }
        let file = path.trim_start_matches('/');
        let samples: Vec<String> = if file.ends_with('/') {
            FOLDER_SAMPLES
                .iter()
                .map(|s| format!("{file}{s}"))
                .collect()
        } else {
            vec![file.to_string()]
        };
        for sample in samples {
            let Some(last) = lines.iter().rev().find(|l| matches(&l.pattern, &sample)) else {
                problems.push(format!("no line matches {sample}"));
                continue;
            };
            if last.pattern != path {
                problems.push(format!(
                    "line {} (`{}`) comes after the line for {path} and also matches {sample}, \
                     so it would decide who reviews it",
                    last.number, last.pattern
                ));
            } else if last.owners != [owner.to_string()] {
                problems.push(format!(
                    "line {} for {path} must name exactly {owner}, and nothing else (it names \
                     {})",
                    last.number,
                    if last.owners.is_empty() {
                        "no one".to_string()
                    } else {
                        last.owners.join(" ")
                    }
                ));
            }
        }
    }
    problems.dedup();
    problems
}

/// What the repository at `root` says about who owns the approved list:
/// `.github/owner-handle` and `.github/CODEOWNERS`, read and checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoOwnership {
    /// Neither file exists: the owner hasn't added them yet.
    NotYet,
    /// Both exist and every owner-only path's last line names exactly the recorded handle.
    OwnerOnly { handle: String },
    /// Something is wrong (one file without the other, a malformed handle, a missing or wrong
    /// line): each problem.
    Problems(Vec<String>),
}

/// Reads and checks `.github/owner-handle` and `.github/CODEOWNERS` under `root`.
pub fn repo_ownership(root: &Path) -> RepoOwnership {
    let handle = std::fs::read_to_string(root.join(".github/owner-handle"));
    let codeowners = std::fs::read_to_string(root.join(".github/CODEOWNERS"));
    let missing = |r: &std::io::Result<String>| matches!(r, Err(e) if e.kind() == std::io::ErrorKind::NotFound);
    match (handle, codeowners) {
        (h, c) if missing(&h) && missing(&c) => RepoOwnership::NotYet,
        (Ok(h), Ok(c)) => match parse_owner_handle(&h) {
            Err(e) => RepoOwnership::Problems(vec![e]),
            Ok(handle) => {
                let problems: Vec<String> = owner_only_problems(&c, &handle)
                    .into_iter()
                    .map(|p| format!(".github/CODEOWNERS: {p}"))
                    .collect();
                if problems.is_empty() {
                    RepoOwnership::OwnerOnly { handle }
                } else {
                    RepoOwnership::Problems(problems)
                }
            }
        },
        (h, c) => {
            let mut problems = Vec::new();
            if h.is_err() {
                problems.push(".github/owner-handle is missing or can't be read".to_string());
            }
            if c.is_err() {
                problems.push(".github/CODEOWNERS is missing or can't be read".to_string());
            }
            RepoOwnership::Problems(problems)
        }
    }
}

/// The approved list's own rule (r7, X2): a list with any entry needs `.github/owner-handle`
/// and `.github/CODEOWNERS` in place with the owner-only lines. `Err`: why not.
pub fn hosted_list_may_have_entries(root: &Path) -> Result<String, Vec<String>> {
    match repo_ownership(root) {
        RepoOwnership::OwnerOnly { handle } => Ok(handle),
        RepoOwnership::NotYet => Err(vec![
            "no .github/owner-handle and no .github/CODEOWNERS: no entry may be added to the \
             approved hosted list until the owner has added both (spec §3.6, \"Who may change \
             the list\")"
                .into(),
        ]),
        RepoOwnership::Problems(p) => Err(p),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn good(owner: &str) -> String {
        format!(
            "* @org/everyone\n/crates/gateway/ @org/platform\n\
             /.github/CODEOWNERS {owner}\n/.github/owner-handle {owner}\n\
             /crates/gateway/approved-hosted.yaml {owner}\n/docs/model-providers/ {owner}\n"
        )
    }

    #[test]
    fn handles_are_one_user() {
        assert_eq!(parse_owner_handle("@octo-cat\n").unwrap(), "@octo-cat");
        for bad in [
            "",
            "octocat",
            "@",
            "@org/team",
            "@a--b",
            "@-a",
            "@a-",
            "me@example.com",
            "@a\n@b\n",
            &format!("@{}", "a".repeat(40)),
        ] {
            assert!(parse_owner_handle(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn patterns_match_as_github_reads_them() {
        assert!(matches("*", "docs/a.md"));
        assert!(matches("/docs/", "docs/model-providers/x.md"));
        assert!(!matches("/docs/", "docs"));
        assert!(matches("docs/", "x/docs/a.md"));
        assert!(matches("*.md", "docs/model-providers/README.md"));
        assert!(!matches("*.md", "crates/gateway/approved-hosted.yaml"));
        assert!(matches(
            "/crates/gateway/",
            "crates/gateway/approved-hosted.yaml"
        ));
        assert!(matches("/crates/**/x.yaml", "crates/a/b/x.yaml"));
        assert!(matches("/.github/", ".github/CODEOWNERS"));
        assert!(matches("/.github/CODEOWNERS", ".github/CODEOWNERS"));
        assert!(!matches("/.github/CODEOWNERS", "a/.github/CODEOWNERS"));
        assert!(matches(
            "approved-hosted.yaml",
            "crates/gateway/approved-hosted.yaml"
        ));
        assert!(matches(
            "/crates/gateway",
            "crates/gateway/approved-hosted.yaml"
        ));
    }

    #[test]
    fn the_owner_lines_pass_last_and_alone() {
        assert_eq!(
            owner_only_problems(&good("@owner"), "@owner"),
            Vec::<String>::new()
        );
        assert!(owner_only_problems(&good(OWNER_PLACEHOLDER), OWNER_PLACEHOLDER).is_empty());
    }

    #[test]
    fn every_way_of_getting_it_wrong_fails() {
        // A different single handle: r6's "one handle" rule passed it.
        assert!(!owner_only_problems(&good("@someone-else"), "@owner").is_empty());
        // A second owner, or a team.
        let two = good("@owner").replace(
            "/docs/model-providers/ @owner",
            "/docs/model-providers/ @owner @other",
        );
        assert!(!owner_only_problems(&two, "@owner").is_empty());
        let team = good("@owner").replace(
            "/.github/owner-handle @owner",
            "/.github/owner-handle @org/team",
        );
        assert!(!owner_only_problems(&team, "@owner").is_empty());
        // A missing line, for each of the four paths.
        for path in OWNER_ONLY_PATHS {
            let text: String = good("@owner")
                .lines()
                .filter(|l| !l.starts_with(&format!("{path} ")))
                .map(|l| format!("{l}\n"))
                .collect();
            let p = owner_only_problems(&text, "@owner");
            assert!(p.iter().any(|p| p.contains(path)), "{path}: {p:?}");
        }
        // A later line that also matches wins.
        for later in [
            "*.md @org/docs",
            "/.github/ @org/team",
            "/crates/gateway/ @org/x",
            "* @a",
        ] {
            let text = format!("{}{later}\n", good("@owner"));
            assert!(!owner_only_problems(&text, "@owner").is_empty(), "{later}");
        }
        // The owner line with no owner at all.
        let none =
            good("@owner").replace("/docs/model-providers/ @owner", "/docs/model-providers/");
        assert!(!owner_only_problems(&none, "@owner").is_empty());
    }

    #[test]
    fn the_repository_files_are_read_together() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        assert_eq!(repo_ownership(root), RepoOwnership::NotYet);
        assert!(hosted_list_may_have_entries(root).is_err());
        std::fs::create_dir_all(root.join(".github")).unwrap();
        std::fs::write(root.join(".github/owner-handle"), "@owner\n").unwrap();
        assert!(matches!(repo_ownership(root), RepoOwnership::Problems(_)));
        std::fs::write(root.join(".github/CODEOWNERS"), good("@other")).unwrap();
        assert!(matches!(repo_ownership(root), RepoOwnership::Problems(_)));
        std::fs::write(root.join(".github/CODEOWNERS"), good("@owner")).unwrap();
        assert_eq!(hosted_list_may_have_entries(root).unwrap(), "@owner");
        std::fs::write(root.join(".github/owner-handle"), "@org/team\n").unwrap();
        assert!(hosted_list_may_have_entries(root).is_err());
        std::fs::remove_file(root.join(".github/owner-handle")).unwrap();
        assert!(matches!(repo_ownership(root), RepoOwnership::Problems(_)));
    }
}
