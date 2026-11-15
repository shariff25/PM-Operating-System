//! Grading: does an outcome meet its task's `expect`?
//!
//! A task passes only when every check passes:
//! 1. **Status.** With `approvals: n > 0` the task must end `needs_approval` with exactly `n`
//!    approval items. With `approvals: 0` it must end `done` with none. Without `approvals` it
//!    must end `done` or `needs_approval`. A task the runner stopped waiting for fails.
//! 2. **`drafts`.** For each entry, the task's drafts of that tool: their number, then each
//!    draft's required fields, allowed values and text (see [`draft_text`]).
//! 3. **An answer.** The result must not be empty.
//! 4. **`must_cite`.** Each URL must be among the links in the answer, compared with
//!    [`normalize_url`].
//! 5. **`must_include`.** Each text must be in the answer, compared with [`normalize_text`].
//! 6. **`must_not_include`.** No such text may be in the answer, compared the same way.
//! 7. **`must_decline`.** The answer must contain one of the decline phrases.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::client::{status, Draft};
use crate::outcome::Outcome;
use crate::spec::{DraftCheck, EvalTask};

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
    /// A `must_decline` answer didn't say it couldn't answer.
    NotDeclined,
    /// The task's drafts couldn't be read, so its `drafts` checks couldn't run.
    DraftsUnreadable {
        error: String,
    },
    /// The wrong number of drafts of a tool. `expected: None` means at least one.
    DraftCount {
        tool: String,
        expected: Option<u32>,
        got: usize,
    },
    /// A draft is missing a required argument, or it is empty.
    DraftMissingField {
        tool: String,
        draft: String,
        field: String,
    },
    /// A draft's argument isn't one of the allowed values.
    DraftNotAllowed {
        tool: String,
        draft: String,
        field: String,
        value: Option<String>,
        allowed: Vec<String>,
    },
    DraftMissingText {
        tool: String,
        draft: String,
        text: String,
    },
    DraftForbiddenText {
        tool: String,
        draft: String,
        text: String,
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
            Check::NotDeclined => write!(f, "answered instead of saying its sources don't"),
            Check::DraftsUnreadable { error } => write!(f, "drafts not readable: {error}"),
            Check::DraftCount {
                tool,
                expected,
                got,
            } => match expected {
                Some(n) => write!(f, "{got} `{tool}` draft(s), expected {n}"),
                None => write!(f, "no `{tool}` draft, expected at least one"),
            },
            Check::DraftMissingField { tool, draft, field } => {
                write!(f, "`{tool}` draft {draft} has no `{field}`")
            }
            Check::DraftNotAllowed {
                tool,
                draft,
                field,
                value,
                allowed,
            } => write!(
                f,
                "`{tool}` draft {draft}: `{field}` is {}, allowed {}",
                value
                    .as_deref()
                    .map(|v| format!("`{v}`"))
                    .unwrap_or_else(|| "missing".into()),
                allowed
                    .iter()
                    .map(|a| format!("`{a}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Check::DraftMissingText { tool, draft, text } => {
                write!(f, "`{tool}` draft {draft} doesn't say \"{text}\"")
            }
            Check::DraftForbiddenText { tool, draft, text } => {
                write!(f, "`{tool}` draft {draft} says \"{text}\"")
            }
        }
    }
}

impl Check {
    /// Which deterministic check this is, the same across runs (`must_cite:<url>`,
    /// `drafts:<tool>:count`, …), so the gate can see a check that passed in the baseline
    /// fail now. `None` for a failure of the run itself (runtime, timeout, unreadable drafts):
    /// those say nothing about the answer.
    pub fn key(&self) -> Option<String> {
        Some(match self {
            Check::Runtime { .. } | Check::TimedOut { .. } | Check::DraftsUnreadable { .. } => {
                return None
            }
            Check::Status { .. } => "status".into(),
            Check::NoAnswer => "answer".into(),
            Check::MissingCitation { url } => format!("must_cite:{}", normalize_url(url)),
            Check::MissingText { text } => format!("must_include:{}", normalize_text(text)),
            Check::ForbiddenText { text } => {
                format!("must_not_include:{}", normalize_text(text))
            }
            Check::Approvals { .. } => "approvals".into(),
            Check::NotDeclined => "must_decline".into(),
            Check::DraftCount { tool, .. } => format!("drafts:{tool}:count"),
            Check::DraftMissingField { tool, field, .. } => {
                format!("drafts:{tool}:require_fields:{field}")
            }
            Check::DraftNotAllowed { tool, field, .. } => format!("drafts:{tool}:allowed:{field}"),
            Check::DraftMissingText { tool, text, .. } => {
                format!("drafts:{tool}:must_include:{}", normalize_text(text))
            }
            Check::DraftForbiddenText { tool, text, .. } => {
                format!("drafts:{tool}:must_not_include:{}", normalize_text(text))
            }
        })
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
/// Hub**" matches "Docker Hub". Curly apostrophes count as straight ones, so "can’t"
/// matches "can't".
pub fn normalize_text(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    for (i, &c) in chars.iter().enumerate() {
        let mapped = match c {
            '-' | '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' => ' ',
            '\u{2018}' | '\u{2019}' => '\'',
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
/// `/docs/`. So `https://support.acme.com/docs/x.md?utm=1` cites
/// `https://support.acme.com/docs/x/`.
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

/// A draft's text, as its `must_include` and `must_not_include` see it: its top-level string
/// arguments, sorted by key and joined by newlines (as the feedback ledger measures edits).
pub fn draft_text(args: &Value) -> String {
    let Some(map) = args.as_object() else {
        return args.as_str().unwrap_or_default().to_string();
    };
    let mut keys: Vec<&String> = map.keys().collect();
    keys.sort();
    keys.iter()
        .filter_map(|k| map[k.as_str()].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Whether an argument is there and not empty: a non-blank string, a non-empty list or
/// object, or any number or boolean.
fn has_value(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::String(s)) => !s.trim().is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
        Some(_) => true,
    }
}

/// An argument as `allowed` compares it: a string as it is, a number or boolean as written.
fn scalar(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) => Some(s.trim().to_string()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// Grade a task's drafts against its `drafts` checks.
pub fn grade_drafts(checks: &[DraftCheck], drafts: &[Draft]) -> Vec<Check> {
    let mut failures = Vec::new();
    for c in checks {
        let mine: Vec<&Draft> = drafts.iter().filter(|d| d.tool == c.tool).collect();
        let count_ok = match c.count {
            Some(n) => mine.len() == n as usize,
            None => !mine.is_empty(),
        };
        if !count_ok {
            failures.push(Check::DraftCount {
                tool: c.tool.clone(),
                expected: c.count,
                got: mine.len(),
            });
        }
        for d in mine {
            for field in &c.require_fields {
                if !has_value(d.args.get(field)) {
                    failures.push(Check::DraftMissingField {
                        tool: c.tool.clone(),
                        draft: d.id.clone(),
                        field: field.clone(),
                    });
                }
            }
            for (field, allowed) in &c.allowed {
                let value = scalar(d.args.get(field));
                if !value.as_ref().is_some_and(|v| allowed.contains(v)) {
                    failures.push(Check::DraftNotAllowed {
                        tool: c.tool.clone(),
                        draft: d.id.clone(),
                        field: field.clone(),
                        value,
                        allowed: allowed.clone(),
                    });
                }
            }
            let text = normalize_text(&draft_text(&d.args));
            for t in &c.must_include {
                if !text.contains(&normalize_text(t)) {
                    failures.push(Check::DraftMissingText {
                        tool: c.tool.clone(),
                        draft: d.id.clone(),
                        text: t.clone(),
                    });
                }
            }
            for t in &c.must_not_include {
                if text.contains(&normalize_text(t)) {
                    failures.push(Check::DraftForbiddenText {
                        tool: c.tool.clone(),
                        draft: d.id.clone(),
                        text: t.clone(),
                    });
                }
            }
        }
    }
    failures
}

/// Grade one outcome against its task. `decline_phrases` are `config.yaml`'s, for
/// `must_decline`.
pub fn grade(task: &EvalTask, outcome: &Outcome, decline_phrases: &[String]) -> Grade {
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
    if !task.expect.drafts.is_empty() {
        match &outcome.drafts_error {
            Some(error) => failures.push(Check::DraftsUnreadable {
                error: error.clone(),
            }),
            None => failures.extend(grade_drafts(&task.expect.drafts, &outcome.drafts)),
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
    if task.expect.must_decline
        && !decline_phrases
            .iter()
            .any(|p| text.contains(&normalize_text(p)))
    {
        failures.push(Check::NotDeclined);
    }
    Grade { failures }
}
