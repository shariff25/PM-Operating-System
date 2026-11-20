//! What every admin proposal shares (`docs/policy-admin.md`, `docs/content-admin.md`): the
//! path allowlist and the caps, a scratch copy of the content tree to validate a change on,
//! clean titles and bodies, server-made branch names, the per-admin rate limit, a line diff,
//! and git blob ids for the stale-base check. `pmos-web` uses it to validate interactively;
//! the tool server uses it again ([`super::changeset`]) before it opens a pull request with
//! the admin's own GitHub token.
//!
//! **Allowlist.** A proposal may change only files in the content tree (`content/**` in the
//! repository), and the files `pmos policy generate` writes (`policies/bots/*.yaml`,
//! `policies/providers/*.yaml`), which the server regenerates itself and never takes from the
//! browser. Never `.github/`, `deploy/`, code, a hidden file, or anything outside.

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sha1::Digest as _;

/// The largest file a proposal may write, in bytes.
pub const MAX_FILE_BYTES: usize = 256 * 1024;
/// The most files one proposal may change (as sent; regenerated policy files don't count).
pub const MAX_FILES: usize = 50;
/// The longest path, in bytes, and the deepest.
pub const MAX_PATH: usize = 200;
pub const MAX_DEPTH: usize = 8;
/// Pull request title and reason caps, in characters, after cleaning.
pub const MAX_TITLE: usize = 120;
pub const MAX_REASON: usize = 4000;
/// GitHub's body limit is 65,536 characters; stay well under.
pub const MAX_BODY: usize = 60_000;
/// The label every proposal gets, and the kind's own.
pub const LABEL: &str = "pmos-proposal";
/// Every proposal branch starts with this; the rest is made by the server.
pub const BRANCH_PREFIX: &str = "pmos/proposal/";
/// The extensions a content file may have: all text.
pub const TEXT_EXTENSIONS: [&str; 4] = ["md", "yaml", "yml", "txt"];
/// Content files a proposal may not touch, with why.
pub const PROTECTED: [(&str, &str); 1] = [(
    "team.yaml",
    "team.yaml is retired: users and roles are in the PM OS user database, not content",
)];

/// What a proposal changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Policy,
    Content,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Policy => "policy",
            Self::Content => "content",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Policy => "pmos-proposal:policy",
            Self::Content => "pmos-proposal:content",
        }
    }
}

/// Why a path or a file was refused: a stable `code` and a sentence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Refusal {
    pub code: &'static str,
    pub message: String,
}

impl Refusal {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

fn safe_component(c: &str) -> bool {
    !c.is_empty()
        && c.len() <= 100
        && !c.starts_with('.')
        && c.chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
}

/// A relative, `/`-separated path of plain components: letters, digits, `._-`, none empty,
/// none `.`, `..` or hidden (starting with `.`).
pub fn safe_repo_path(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= MAX_PATH
        && !p.starts_with('/')
        && p.split('/').count() <= MAX_DEPTH
        && p.split('/').all(safe_component)
}

/// The content-relative path for a repository path inside the content tree, or why not.
/// `content_dir` is the content tree's folder in the repository (`content`), or empty.
pub fn content_rel(repo_path: &str, content_dir: &str) -> Result<String, Refusal> {
    let outside = || {
        Refusal::new(
            "path_not_allowed",
            format!(
                "Only files in the content tree ({}) can be read or changed here.",
                if content_dir.is_empty() {
                    "the repository root".to_string()
                } else {
                    format!("{content_dir}/")
                }
            ),
        )
    };
    if repo_path.contains('\\') || repo_path.chars().any(|c| c.is_control()) {
        return Err(Refusal::new(
            "bad_path",
            "Paths use `/`, with no control characters.",
        ));
    }
    let rel = if content_dir.is_empty() {
        repo_path
    } else {
        repo_path
            .strip_prefix(content_dir)
            .and_then(|r| r.strip_prefix('/'))
            .ok_or_else(outside)?
    };
    if !safe_repo_path(rel) {
        return Err(Refusal::new(
            "bad_path",
            "A path is relative and made of letters, digits, `.`, `_` and `-`, with no `..` or hidden files.",
        ));
    }
    Ok(rel.to_string())
}

/// May a proposal write `rel` (content-relative)? A text extension, not protected.
pub fn writable(rel: &str) -> Result<(), Refusal> {
    if let Some((_, why)) = PROTECTED.iter().find(|(p, _)| *p == rel) {
        return Err(Refusal::new(
            "path_not_allowed",
            format!("`{rel}` can't be changed here: {why}."),
        ));
    }
    let ext = rel.rsplit_once('.').map(|(_, e)| e).unwrap_or_default();
    if !TEXT_EXTENSIONS.contains(&ext) {
        return Err(Refusal::new(
            "not_text",
            format!(
                "Only {} files can be changed here.",
                TEXT_EXTENSIONS.join(", ")
            ),
        ));
    }
    Ok(())
}

/// A file's text is within the size cap and has no NUL (it is UTF-8 by construction).
pub fn check_text(text: &str) -> Result<(), Refusal> {
    if text.len() > MAX_FILE_BYTES {
        return Err(Refusal::new(
            "too_large",
            format!("A file can be at most {} KiB.", MAX_FILE_BYTES / 1024),
        ));
    }
    if text.contains('\0') {
        return Err(Refusal::new("not_text", "A file can't contain NUL bytes."));
    }
    Ok(())
}

/// `rel` under `root`, if it exists as a regular file, refusing any symlink on the way and
/// anything that resolves outside `root`. `Ok(None)` when nothing is there.
pub fn resolve(root: &Path, rel: &str) -> Result<Option<PathBuf>, Refusal> {
    let symlink = || Refusal::new("bad_path", "Symbolic links are not followed.");
    let mut at = root.to_path_buf();
    for part in rel.split('/') {
        at.push(part);
        match fs::symlink_metadata(&at) {
            Ok(m) if m.file_type().is_symlink() => return Err(symlink()),
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(Refusal::new("unreadable", "That file can't be read.")),
        }
    }
    let meta = fs::symlink_metadata(&at)
        .map_err(|_| Refusal::new("unreadable", "That file can't be read."))?;
    if !meta.is_file() {
        return Err(Refusal::new(
            "not_a_file",
            "That path is a folder, not a file.",
        ));
    }
    // Defence in depth: the canonical path is inside the canonical root.
    let canon_root = root
        .canonicalize()
        .map_err(|_| Refusal::new("unreadable", "The content tree can't be read."))?;
    let canon = at
        .canonicalize()
        .map_err(|_| Refusal::new("unreadable", "That file can't be read."))?;
    if !canon.starts_with(&canon_root) {
        return Err(symlink());
    }
    Ok(Some(at))
}

/// A file's text, if it is a regular file inside `root` (see [`resolve`]) of at most
/// [`MAX_FILE_BYTES`] that is UTF-8 with no NUL.
pub fn read_text(root: &Path, rel: &str) -> Result<Option<String>, Refusal> {
    let Some(path) = resolve(root, rel)? else {
        return Ok(None);
    };
    let bytes =
        fs::read(&path).map_err(|_| Refusal::new("unreadable", "That file can't be read."))?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err(Refusal::new(
            "too_large",
            format!("That file is over {} KiB.", MAX_FILE_BYTES / 1024),
        ));
    }
    let text = String::from_utf8(bytes)
        .map_err(|_| Refusal::new("not_text", "That file isn't UTF-8 text."))?;
    if text.contains('\0') {
        return Err(Refusal::new("not_text", "That file contains NUL bytes."));
    }
    Ok(Some(text))
}

/// A validation finding, as the API serves it: `path` is a repository path (`content/…`,
/// `policies/…`) or, for a proposed policy spec, a spec path (`spec.bots.docs.role`); `line` is
/// 1-based when known.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Issue {
    pub severity: &'static str,
    pub code: String,
    pub path: String,
    pub line: Option<u64>,
    pub message: String,
}

impl Issue {
    pub fn error(
        code: impl Into<String>,
        path: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            severity: "error",
            code: code.into(),
            path: path.into(),
            line: None,
            message: message.into(),
        }
    }

    /// A content lint diagnostic, at its repository path. A parser's position inside a
    /// markdown file's front matter is moved past the opening `---`.
    pub fn from_lint(d: &pmos_contract::Diagnostic, content_dir: &str) -> Self {
        let path = repo_path(content_dir, &d.path);
        let line =
            message_line(&d.message).map(|n| if d.path.ends_with(".md") { n + 1 } else { n });
        Self {
            severity: match d.severity {
                pmos_contract::Severity::Error => "error",
                pmos_contract::Severity::Warning => "warning",
            },
            code: d.code.to_string(),
            path,
            line,
            message: d.message.clone(),
        }
    }
}

/// `dir/rel`, or `rel` for an empty `dir`.
pub fn repo_path(dir: &str, rel: &str) -> String {
    if dir.is_empty() {
        rel.to_string()
    } else if rel == "." || rel.is_empty() {
        dir.to_string()
    } else {
        format!("{dir}/{rel}")
    }
}

/// One change to a content file: `text` to write, `None` to delete. `rel` is content-relative.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub rel: String,
    pub text: Option<String>,
}

/// A scratch copy of the content tree with changes applied. Removed when dropped.
pub struct Overlay {
    _dir: tempfile::TempDir,
    pub root: PathBuf,
}

/// Copy `content_root` (regular files and folders only: symlinks and hidden entries are left
/// out) and apply `changes`. A deletion of a file that isn't there is refused; folders left
/// empty by deletions are removed, as git would.
pub fn overlay(content_root: &Path, changes: &[Change]) -> Result<Overlay, Refusal> {
    let unavailable = |_| Refusal::new("unavailable", "The content tree can't be copied.");
    let dir = tempfile::tempdir().map_err(unavailable)?;
    let root = dir.path().join("content");
    fs::create_dir_all(&root).map_err(unavailable)?;
    let walker = walkdir::WalkDir::new(content_root)
        .follow_links(false)
        .min_depth(1)
        .into_iter()
        .filter_entry(|e| !e.file_name().to_string_lossy().starts_with('.'));
    for entry in walker {
        let entry =
            entry.map_err(|_| Refusal::new("unavailable", "The content tree can't be read."))?;
        let rel = entry
            .path()
            .strip_prefix(content_root)
            .map_err(|_| Refusal::new("unavailable", "The content tree can't be read."))?;
        let to = root.join(rel);
        let ft = entry.file_type();
        if ft.is_dir() {
            fs::create_dir_all(&to).map_err(unavailable)?;
        } else if ft.is_file() {
            fs::copy(entry.path(), &to).map_err(unavailable)?;
        }
    }
    for c in changes {
        let at = root.join(&c.rel);
        // The copy holds no symlinks, so a plain check is enough here.
        match &c.text {
            Some(text) => {
                if at.is_dir() {
                    return Err(Refusal::new(
                        "not_a_file",
                        format!("`{}` is a folder, not a file.", c.rel),
                    ));
                }
                if let Some(parent) = at.parent() {
                    if parent.is_file() {
                        return Err(Refusal::new(
                            "not_a_file",
                            format!("A parent of `{}` is a file.", c.rel),
                        ));
                    }
                    fs::create_dir_all(parent).map_err(unavailable)?;
                }
                fs::write(&at, text).map_err(unavailable)?;
            }
            None => {
                if !at.is_file() {
                    return Err(Refusal::new(
                        "not_found",
                        format!("`{}` doesn't exist, so it can't be deleted.", c.rel),
                    ));
                }
                fs::remove_file(&at).map_err(unavailable)?;
                let mut parent = at.parent().map(Path::to_path_buf);
                while let Some(p) = parent {
                    if p == root || fs::remove_dir(&p).is_err() {
                        break;
                    }
                    parent = p.parent().map(Path::to_path_buf);
                }
            }
        }
    }
    Ok(Overlay { _dir: dir, root })
}

/// One line: control characters dropped, whitespace collapsed, at most `max` characters.
pub fn clean_line(s: &str, max: usize) -> String {
    let collapsed = s
        .split(|c: char| c.is_whitespace() || c.is_control())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    collapsed.chars().take(max).collect()
}

/// GitHub's closing keywords (`Fixes #12` closes issue 12 when the pull request merges).
const CLOSING_KEYWORDS: [&str; 9] = [
    "close", "closes", "closed", "fix", "fixes", "fixed", "resolve", "resolves", "resolved",
];

/// `s` with every closing keyword, as a whole word in any letter case, wrapped in single
/// quotes (`'Fixes' #12`), so a proposal's title or reason can never close an issue on merge:
/// GitHub links an issue only when the keyword is followed directly by a colon or a space and
/// the reference (review of `b469d5b`, L11). Everything else is kept as written.
pub fn no_closing_keywords(s: &str) -> String {
    let wordish = |c: char| c.is_alphanumeric() || c == '_';
    let mut out = String::with_capacity(s.len() + 8);
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        if CLOSING_KEYWORDS.contains(&word.to_ascii_lowercase().as_str()) {
            out.push('\'');
            out.push_str(word);
            out.push('\'');
        } else {
            out.push_str(word);
        }
        word.clear();
    };
    for c in s.chars() {
        if wordish(c) {
            word.push(c);
        } else {
            flush(&mut word, &mut out);
            out.push(c);
        }
    }
    flush(&mut word, &mut out);
    out
}

/// Free text: control characters other than newlines and tabs dropped, `\r\n` as `\n`, at most
/// `max` characters.
pub fn clean_text(s: &str, max: usize) -> String {
    s.replace("\r\n", "\n")
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .take(max)
        .collect::<String>()
        .trim()
        .to_string()
}

/// `pmos/proposal/<kind>/<handle>-<yyyymmdd-hhmmss>-<random>`: made here, never taken from
/// the browser.
pub fn branch_name(kind: Kind, handle: &str, now: DateTime<Utc>) -> String {
    let handle: String = handle
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .take(32)
        .collect();
    let mut bytes = [0u8; 4];
    getrandom::fill(&mut bytes).expect("the operating system's random number generator");
    let suffix: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{BRANCH_PREFIX}{}/{}-{}-{suffix}",
        kind.as_str(),
        handle.trim_matches('-'),
        now.format("%Y%m%d-%H%M%S"),
    )
}

/// A branch name PM OS is willing to use: letters, digits, `-_./`, no `..`, no leading or
/// trailing `/`, at most 200.
pub fn valid_branch(b: &str) -> bool {
    !b.is_empty()
        && b.len() <= 200
        && b.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
        && !b.contains("..")
        && !b.contains("//")
        && !b.starts_with('/')
        && !b.ends_with('/')
        && !b.ends_with(".lock")
}

/// Proposals per admin per hour, in memory: a restart forgets, and each process counts its
/// own (the tool server runs as a single replica).
pub struct RateLimiter {
    per_hour: usize,
    hits: Mutex<HashMap<String, VecDeque<DateTime<Utc>>>>,
}

impl RateLimiter {
    pub fn new(per_hour: u32) -> Self {
        Self {
            per_hour: per_hour as usize,
            hits: Mutex::new(HashMap::new()),
        }
    }

    /// Count one proposal for `handle`, or the seconds until the next is allowed.
    pub fn take(&self, handle: &str, now: DateTime<Utc>) -> Result<(), u64> {
        let mut hits = self.hits.lock().unwrap_or_else(|e| e.into_inner());
        let q = hits.entry(handle.to_string()).or_default();
        while q.front().is_some_and(|t| now - *t >= Duration::hours(1)) {
            q.pop_front();
        }
        if q.len() >= self.per_hour {
            let oldest = q.front().copied().unwrap_or(now);
            let wait = (oldest + Duration::hours(1) - now).num_seconds().max(1);
            return Err(wait as u64);
        }
        q.push_back(now);
        Ok(())
    }
}

/// Git's blob id for `bytes`: `sha1("blob <len>\0" + bytes)`, hex.
pub fn git_blob_sha(bytes: &[u8]) -> String {
    let mut h = sha1::Sha1::new();
    h.update(format!("blob {}\0", bytes.len()).as_bytes());
    h.update(bytes);
    hex(&h.finalize())
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    hex(&sha2::Sha256::digest(bytes))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The line a parser's message points at (`… at line 3 column 5`), if any.
pub fn message_line(message: &str) -> Option<u64> {
    let at = message.find("line ")?;
    let digits: String = message[at + 5..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok().filter(|n| *n > 0)
}

/// A unified diff of `old` and `new` (either may be absent), with three lines of context.
/// Lines are compared exactly; at most [`DIFF_MAX_LINES`] lines a side are diffed, beyond which
/// the file is shown as replaced whole.
pub fn unified_diff(path: &str, old: Option<&str>, new: Option<&str>) -> String {
    let a: Vec<&str> = old.map(|t| t.lines().collect()).unwrap_or_default();
    let b: Vec<&str> = new.map(|t| t.lines().collect()).unwrap_or_default();
    let mut out = format!(
        "--- {}\n+++ {}\n",
        if old.is_some() {
            format!("a/{path}")
        } else {
            "/dev/null".into()
        },
        if new.is_some() {
            format!("b/{path}")
        } else {
            "/dev/null".into()
        }
    );
    let ops = line_ops(&a, &b);
    // Group into hunks with 3 lines of context.
    const CTX: usize = 3;
    let changed: Vec<usize> = ops
        .iter()
        .enumerate()
        .filter(|(_, o)| !matches!(o, Op::Same(..)))
        .map(|(i, _)| i)
        .collect();
    if changed.is_empty() {
        return String::new();
    }
    let mut i = 0;
    while i < changed.len() {
        let start = changed[i].saturating_sub(CTX);
        let mut end = (changed[i] + CTX + 1).min(ops.len());
        let mut j = i + 1;
        while j < changed.len() && changed[j] <= end + CTX {
            end = (changed[j] + CTX + 1).min(ops.len());
            j += 1;
        }
        let (mut a_start, mut b_start) = (None, None);
        let (mut a_len, mut b_len) = (0, 0);
        let mut body = String::new();
        for op in &ops[start..end] {
            match op {
                Op::Same(ai, bi) => {
                    a_start.get_or_insert(*ai);
                    b_start.get_or_insert(*bi);
                    a_len += 1;
                    b_len += 1;
                    body.push_str(&format!(" {}\n", a[*ai]));
                }
                Op::Del(ai, bi) => {
                    a_start.get_or_insert(*ai);
                    b_start.get_or_insert(*bi);
                    a_len += 1;
                    body.push_str(&format!("-{}\n", a[*ai]));
                }
                Op::Add(ai, bi) => {
                    a_start.get_or_insert(*ai);
                    b_start.get_or_insert(*bi);
                    b_len += 1;
                    body.push_str(&format!("+{}\n", b[*bi]));
                }
            }
        }
        let pos = |start: Option<usize>, len: usize| {
            let s = start.unwrap_or(0);
            if len == 0 {
                format!("{s},0")
            } else {
                format!("{},{len}", s + 1)
            }
        };
        out.push_str(&format!(
            "@@ -{} +{} @@\n{body}",
            pos(a_start, a_len),
            pos(b_start, b_len)
        ));
        i = j;
    }
    out
}

/// The most lines a side the diff aligns line by line.
pub const DIFF_MAX_LINES: usize = 4000;

/// A diff step, with the positions in `a` and `b` where it happens.
enum Op {
    Same(usize, usize),
    Del(usize, usize),
    Add(usize, usize),
}

/// The shortest edit (by longest common subsequence) from `a` to `b`.
fn line_ops(a: &[&str], b: &[&str]) -> Vec<Op> {
    // Trim the common head and tail first: policy and content diffs are usually small.
    let head = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let tail = a[head..]
        .iter()
        .rev()
        .zip(b[head..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let (am, bm) = (&a[head..a.len() - tail], &b[head..b.len() - tail]);
    let mut ops: Vec<Op> = (0..head).map(|i| Op::Same(i, i)).collect();
    if am.len() > DIFF_MAX_LINES || bm.len() > DIFF_MAX_LINES {
        ops.extend((0..am.len()).map(|i| Op::Del(head + i, head)));
        ops.extend((0..bm.len()).map(|j| Op::Add(head + am.len(), head + j)));
    } else {
        let (n, m) = (am.len(), bm.len());
        let mut lcs = vec![0u32; (n + 1) * (m + 1)];
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                lcs[i * (m + 1) + j] = if am[i] == bm[j] {
                    lcs[(i + 1) * (m + 1) + j + 1] + 1
                } else {
                    lcs[(i + 1) * (m + 1) + j].max(lcs[i * (m + 1) + j + 1])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < n || j < m {
            if i < n && j < m && am[i] == bm[j] {
                ops.push(Op::Same(head + i, head + j));
                i += 1;
                j += 1;
            } else if i < n && (j == m || lcs[(i + 1) * (m + 1) + j] >= lcs[i * (m + 1) + j + 1]) {
                ops.push(Op::Del(head + i, head + j));
                i += 1;
            } else {
                ops.push(Op::Add(head + i, head + j));
                j += 1;
            }
        }
    }
    let (a_end, b_end) = (a.len() - tail, b.len() - tail);
    ops.extend((0..tail).map(|k| Op::Same(a_end + k, b_end + k)));
    ops
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closing_keywords_never_close_an_issue() {
        for (raw, want) in [
            ("Fixes #12", "'Fixes' #12"),
            (
                "fix: #3 and CLOSES org/repo#4",
                "'fix': #3 and 'CLOSES' org/repo#4",
            ),
            (
                "resolved https://github.com/o/r/issues/9",
                "'resolved' https://github.com/o/r/issues/9",
            ),
            ("Closed\t#1\nresolves GH-2", "'Closed'\t#1\n'resolves' GH-2"),
            // Not a keyword: a longer word, or part of one.
            (
                "prefixes #1, fixer #2, fix_up #3, fixes1",
                "prefixes #1, fixer #2, fix_up #3, fixes1",
            ),
            ("Add the docs tool", "Add the docs tool"),
        ] {
            assert_eq!(no_closing_keywords(raw), want, "{raw:?}");
        }
    }

    #[test]
    fn paths_must_be_plain_and_inside_the_content_tree() {
        assert_eq!(
            content_rel("content/skills/docs/SKILL.md", "content").unwrap(),
            "skills/docs/SKILL.md"
        );
        assert_eq!(content_rel("skills/a.md", "").unwrap(), "skills/a.md");
        for bad in [
            "content/../deploy/compose.yaml",
            "content/./skills/x.md",
            "content//skills/x.md",
            "content/.github/workflows/ci.yml",
            "content/skills/.hidden.md",
            "/content/skills/x.md",
            "deploy/compose.yaml",
            ".github/workflows/ci.yml",
            "policies/bots/docs.yaml",
            "crates/web/src/app.rs",
            "contentx/skills/a.md",
            "content",
            "content/",
            "content/skills\\x.md",
            "content/skills/x\u{0}.md",
            "content/skills/a b.md",
            "content/sk%2e%2e/x.md",
        ] {
            assert!(content_rel(bad, "content").is_err(), "{bad}");
        }
        let deep = format!("content/{}x.md", "a/".repeat(MAX_DEPTH));
        assert!(content_rel(&deep, "content").is_err());
    }

    #[test]
    fn only_text_files_and_not_team_yaml() {
        assert!(writable("skills/docs/SKILL.md").is_ok());
        assert!(writable("tools.yaml").is_ok());
        assert_eq!(writable("team.yaml").unwrap_err().code, "path_not_allowed");
        assert_eq!(writable("skills/docs/run.sh").unwrap_err().code, "not_text");
        assert_eq!(
            writable("skills/docs/logo.png").unwrap_err().code,
            "not_text"
        );
        assert_eq!(writable("README").unwrap_err().code, "not_text");
        assert!(check_text(&"a".repeat(MAX_FILE_BYTES)).is_ok());
        assert_eq!(
            check_text(&"a".repeat(MAX_FILE_BYTES + 1))
                .unwrap_err()
                .code,
            "too_large"
        );
        assert_eq!(check_text("a\0b").unwrap_err().code, "not_text");
    }

    #[test]
    fn titles_and_bodies_are_cleaned() {
        assert_eq!(
            clean_line("  Add\u{7}\r\n the  tool\t", 120),
            "Add the tool"
        );
        assert_eq!(clean_line(&"x".repeat(500), MAX_TITLE).len(), MAX_TITLE);
        assert_eq!(clean_text("a\r\nb\u{1b}[31m\tc\u{0}", 100), "a\nb[31m\tc");
    }

    #[test]
    fn branch_names_are_the_servers() {
        let now: DateTime<Utc> = "2026-11-17T10:11:12Z".parse().unwrap();
        let b = branch_name(Kind::Policy, "Ana.Silva/../x", now);
        assert!(
            b.starts_with("pmos/proposal/policy/ana-silva----x-20261117-101112-"),
            "{b}"
        );
        assert!(valid_branch(&b), "{b}");
        assert_ne!(b, branch_name(Kind::Policy, "Ana.Silva/../x", now));
    }

    #[test]
    fn the_rate_limit_is_per_admin_per_hour() {
        let rl = RateLimiter::new(2);
        let t: DateTime<Utc> = "2026-11-17T10:00:00Z".parse().unwrap();
        assert!(rl.take("adam", t).is_ok());
        assert!(rl.take("adam", t + Duration::minutes(1)).is_ok());
        assert_eq!(rl.take("adam", t + Duration::minutes(2)), Err(58 * 60));
        assert!(rl.take("ada", t).is_ok());
        assert!(rl.take("adam", t + Duration::minutes(60)).is_ok());
    }

    #[test]
    fn blob_ids_match_git() {
        // `printf 'hello\n' | git hash-object --stdin`
        assert_eq!(
            git_blob_sha(b"hello\n"),
            "ce013625030ba8dba906f756967f9e9ca394464a"
        );
        assert_eq!(
            git_blob_sha(b""),
            "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391"
        );
    }

    #[test]
    fn lines_are_found_in_parser_messages() {
        assert_eq!(
            message_line("invalid front matter: missing field `owner` at line 3 column 1"),
            Some(3)
        );
        assert_eq!(message_line("no position"), None);
    }

    #[test]
    fn unified_diffs() {
        let old = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\n";
        let new = "a\nb\nc\nD\ne\nf\ng\nh\ni\nj\nk\n";
        let d = unified_diff("x.yaml", Some(old), Some(new));
        assert!(d.starts_with("--- a/x.yaml\n+++ b/x.yaml\n"), "{d}");
        assert!(d.contains("-d\n+D\n"), "{d}");
        assert!(d.contains("+k\n"), "{d}");
        assert_eq!(unified_diff("x", Some(old), Some(old)), "");
        let added = unified_diff("n.yaml", None, Some("x\ny\n"));
        assert!(added.contains("--- /dev/null") && added.contains("@@ -0,0 +1,2 @@\n+x\n+y\n"));
        let gone = unified_diff("n.yaml", Some("x\n"), None);
        assert!(gone.contains("+++ /dev/null") && gone.contains("@@ -1,1 +0,0 @@\n-x\n"));
    }

    #[test]
    fn overlays_apply_changes_and_never_copy_symlinks() {
        let src = tempfile::tempdir().unwrap();
        fs::create_dir_all(src.path().join("skills/a")).unwrap();
        fs::write(src.path().join("skills/a/SKILL.md"), "a").unwrap();
        fs::create_dir_all(src.path().join("skills/b")).unwrap();
        fs::write(src.path().join("skills/b/SKILL.md"), "b").unwrap();
        fs::create_dir_all(src.path().join(".git")).unwrap();
        fs::write(src.path().join(".git/HEAD"), "x").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("/etc/passwd", src.path().join("skills/passwd.md")).unwrap();
        let o = overlay(
            src.path(),
            &[
                Change {
                    rel: "skills/a/SKILL.md".into(),
                    text: None,
                },
                Change {
                    rel: "playbooks/new.md".into(),
                    text: Some("new".into()),
                },
            ],
        )
        .unwrap();
        assert!(!o.root.join("skills/a").exists(), "emptied folders go");
        assert!(o.root.join("skills").is_dir());
        assert_eq!(
            fs::read_to_string(o.root.join("playbooks/new.md")).unwrap(),
            "new"
        );
        assert!(!o.root.join(".git").exists());
        assert!(!o.root.join("skills/passwd.md").exists());
        let err = overlay(
            src.path(),
            &[Change {
                rel: "nope.md".into(),
                text: None,
            }],
        )
        .err()
        .unwrap();
        assert_eq!(err.code, "not_found");
    }
}
