//! Grading: does an outcome meet its task's `expect`?
//!
//! A task passes only when every check passes:
//! 1. **Status.** With `approvals: n > 0` the task must end `needs_approval` with exactly `n`
//!    approval items. With `approvals: 0` it must end `done` with none. Without `approvals` it
//!    must end `done` or `needs_approval`. A task the runner stopped waiting for fails.
//! 2. **An answer.** The result must not be empty.
//! 3. **`must_cite`.** Each URL must be among the links in the answer, compared with
//!    [`normalize_url`].
//! 4. **`must_include`.** Each text must be in the answer, compared with [`normalize_text`].
//! 5. **`must_not_include`.** No such text may be in the answer, compared the same way.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::client::status;
use crate::outcome::Outcome;
use crate::spec::EvalTask;

/// One failed check.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "check", rename_all = "snake_case")]
pub enum Check {
    /// A call to the runtime failed.
    Runtime {
        error: String,
    },
    /// The task had not settled in time.
    TimedOut {
        status: String,
    },
    /// The task ended in the wrong status.
    Status {
        expected: String,
        got: String,
    },
    /// The task produced no answer.
    NoAnswer,
    MissingCitation {
        url: String,
    },
    MissingText {
        text: String,
    },
    ForbiddenText {
        text: String,
    },
    Approvals {
        expected: u32,
        got: usize,
    },
}

impl fmt::Display for Check {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Check::Runtime { error } => write!(f, "runtime call failed: {error}"),
            Check::TimedOut { status } => write!(f, "timed out while `{status}`"),
            Check::Status { expected, got } => write!(f, "ended `{got}`, expected {expected}"),
            Check::NoAnswer => write!(f, "no answer"),
            Check::MissingCitation { url } => write!(f, "doesn't cite {url}"),
            Check::MissingText { text } => write!(f, "doesn't say \"{text}\""),
            Check::ForbiddenText { text } => write!(f, "says \"{text}\""),
            Check::Approvals { expected, got } => {
                write!(f, "{got} approval item(s), expected {expected}")
            }
        }
    }
}

/// The checks that failed; none means the task passed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Grade {
    pub failures: Vec<Check>,
}

impl Grade {
    pub fn passed(&self) -> bool {
        self.failures.is_empty()
    }

    pub fn missing_citations(&self) -> impl Iterator<Item = &str> {
        self.failures.iter().filter_map(|c| match c {
            Check::MissingCitation { url } => Some(url.as_str()),
            _ => None,
        })
    }
}

/// Text as it is compared: lowercase; hyphens and dashes as spaces; markdown emphasis and
/// code marks (`*`, `` ` ``) dropped; thousands separators between digits dropped ("1,800" is
/// "1800"); runs of whitespace as one space. "SEV-SNP" matches "SEV SNP", and "**Docker
/// Hub**" matches "Docker Hub".
pub fn normalize_text(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    for (i, &c) in chars.iter().enumerate() {
        let mapped = match c {
            '-' | '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' => ' ',
            '*' | '`' => continue,
            ',' if i > 0
                && chars[i - 1].is_ascii_digit()
                && chars.get(i + 1).is_some_and(|n| n.is_ascii_digit()) =>
            {
                continue
            }
            c if c.is_whitespace() => ' ',
            c => c,
        };
        out.extend(mapped.to_lowercase());
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A URL as citations are compared: no scheme, no `www.`, no query string or fragment, no
/// trailing slash, no `.md` extension, lowercase, and the portal's `/v1/docs/` links read as
/// `/docs/`. So `https://support.fortanix.com/docs/x.md?utm=1` cites
/// `https://support.fortanix.com/docs/x/`.
pub fn normalize_url(url: &str) -> String {
    let mut u = url.trim().to_lowercase();
    for scheme in ["https://", "http://"] {
        if let Some(rest) = u.strip_prefix(scheme) {
            u = rest.to_string();
            break;
        }
    }
    if let Some(rest) = u.strip_prefix("www.") {
        u = rest.to_string();
    }
    if let Some(i) = u.find(['?', '#']) {
        u.truncate(i);
    }
    let mut u = u.trim_end_matches('/').to_string();
    if let Some(rest) = u.strip_suffix(".md") {
        u = rest.trim_end_matches('/').to_string();
    }
    u.replacen("/v1/docs/", "/docs/", 1)
}

/// Every `http(s)://` link in a text: bare, in `<…>`, or as a markdown link target. Trailing
/// sentence punctuation is not part of a link.
pub fn extract_urls(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find("http") {
        let tail = &rest[i..];
        if !(tail.starts_with("https://") || tail.starts_with("http://")) {
            rest = &rest[i + 4..];
            continue;
        }
        let end = tail
            .find(|c: char| {
                c.is_whitespace() || matches!(c, ')' | ']' | '>' | '<' | '"' | '\'' | '`' | '|')
            })
            .unwrap_or(tail.len());
        let url = tail[..end].trim_end_matches(['.', ',', ';', ':', '!', '?', '*']);
        if url.len() > "https://".len() {
            out.push(url.to_string());
        }
        rest = &tail[end..];
    }
    out
}

/// Grade one outcome against its task.
pub fn grade(task: &EvalTask, outcome: &Outcome) -> Grade {
    let mut failures = Vec::new();
    if let Some(error) = &outcome.runtime_error {
        failures.push(Check::Runtime {
            error: error.clone(),
        });
        return Grade { failures };
    }
    let got = outcome.status.clone().unwrap_or_else(|| "unknown".into());
    if outcome.timed_out {
        failures.push(Check::TimedOut { status: got });
        return Grade { failures };
    }
    let expected_status: &[&str] = match task.expect.approvals {
        Some(n) if n > 0 => &[status::NEEDS_APPROVAL],
        Some(_) => &[status::DONE],
        None => &[status::DONE, status::NEEDS_APPROVAL],
    };
    if !expected_status.contains(&got.as_str()) {
        let expected = expected_status
            .iter()
            .map(|s| format!("`{s}`"))
            .collect::<Vec<_>>()
            .join(" or ");
        failures.push(Check::Status { expected, got });
    }
    if let Some(n) = task.expect.approvals {
        if outcome.approvals != n as usize {
            failures.push(Check::Approvals {
                expected: n,
                got: outcome.approvals,
            });
        }
    }
    let answer = outcome.result_text();
    if answer.trim().is_empty() {
        failures.push(Check::NoAnswer);
        return Grade { failures };
    }
    let cited: Vec<String> = extract_urls(answer)
        .iter()
        .map(|u| normalize_url(u))
        .collect();
    for url in &task.expect.must_cite {
        if !cited.contains(&normalize_url(url)) {
            failures.push(Check::MissingCitation { url: url.clone() });
        }
    }
    let text = normalize_text(answer);
    for t in &task.expect.must_include {
        if !text.contains(&normalize_text(t)) {
            failures.push(Check::MissingText { text: t.clone() });
        }
    }
    for t in &task.expect.must_not_include {
        if text.contains(&normalize_text(t)) {
            failures.push(Check::ForbiddenText { text: t.clone() });
        }
    }
    Grade { failures }
}
