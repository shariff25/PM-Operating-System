//! A Reviewer's recommendations on the pending drafts of the PMs assigned to them
//! (`docs/user-database-spec.md` §8, slice U6a). The rule in `CLAUDE.md`: **a Reviewer
//! recommends and the draft's owner confirms.**
//!
//! What a recommendation is: a verdict (`approve` or `changes`), an optional note, and the
//! draft's `revision` and `args_hash` the reviewer read, stored by
//! [`crate::approvals::ApprovalRepo::recommend`] under the item's own lock, so it is refused
//! (`409 draft_changed`) when the draft changed since that read. One live recommendation per
//! reviewer per draft: a new one withdraws the old.
//!
//! What it never does: change the item's status, run a write, call a connector, or count in a
//! decision. `ToolService::decide_verdict` still requires `decided_by == item.user`; it reads
//! the recommendations only to word the decision's audit detail ([`decision_detail`]).
//!
//! Who may recommend, and what a reviewer reads, is decided by `ToolService` in
//! [`crate::service`] (the `review_*` methods), from the user database each time: an active
//! user holding `reviewer`, assigned to the draft's owner, never on their own draft, and for a
//! draft built from restricted sources (or unrecorded ones, which count the same) metadata
//! only.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::approvals::ApprovalItem;
use crate::store::StoreError;

/// The longest note, in characters, after cleaning.
pub const MAX_NOTE: usize = 1000;

/// The run id the reviewer routes' audit events are recorded under.
pub const REVIEWS_RUN: &str = "reviews";

/// The audit action of a recommendation (and of a refused one).
pub const RECOMMEND_ACTION: &str = "approvals.recommend";
/// The audit action of a reviewer's read of one draft.
pub const READ_ACTION: &str = "approvals.review_read";
/// The audit action of a reviewer's inbox listing.
pub const INBOX_ACTION: &str = "approvals.review_inbox";

/// What a reviewer recommends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum RecVerdict {
    /// "Ana recommends approving."
    Approve,
    /// "Ana suggests changes."
    Changes,
}

impl RecVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::Changes => "changes",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "approve" => Some(Self::Approve),
            "changes" => Some(Self::Changes),
            _ => None,
        }
    }

    /// The verdict as the decision's audit detail words it.
    fn advice(self) -> &'static str {
        match self {
            Self::Approve => "to approve",
            Self::Changes => "to make changes",
        }
    }
}

/// One stored recommendation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recommendation {
    pub id: String,
    pub approval_id: String,
    pub reviewer: String,
    pub verdict: RecVerdict,
    /// Untrusted plain text, cleaned ([`clean_note`]). `None` when there was none, or it was
    /// removed with its Slack draft's text (`note_removed_at`), or the recommendation was
    /// replaced.
    pub note: Option<String>,
    /// When the note was removed with its Slack draft's text.
    pub note_removed_at: Option<DateTime<Utc>>,
    /// The item's `revision` the reviewer read.
    pub draft_revision: i64,
    /// The item's full `args_hash` the reviewer read.
    pub draft_sha256: String,
    pub created_at: DateTime<Utc>,
    /// When a newer recommendation by the same reviewer replaced this one. Only live ones
    /// (`None`) are ever served.
    pub withdrawn_at: Option<DateTime<Utc>>,
}

impl Recommendation {
    /// Was it given on another version of `item` than the one it holds now: its revision moved
    /// on, or its arguments are others (either alone is enough)?
    pub fn stale(&self, item: &ApprovalItem) -> bool {
        self.draft_revision != item.revision || self.draft_sha256 != item.args_hash
    }

    /// As the owner (and the Slack card) sees it. `reviewer_name` is the reviewer's display
    /// name (untrusted: plain text everywhere), or their handle when they have none. `note` is
    /// left out entirely unless `with_note`: the Slack bot and the console never get it.
    pub fn owner_json(&self, item: &ApprovalItem, reviewer_name: &str, with_note: bool) -> Value {
        let mut v = json!({
            "id": self.id,
            "reviewer": self.reviewer,
            "reviewer_name": reviewer_name,
            "verdict": self.verdict.as_str(),
            "draft_revision": self.draft_revision,
            "created_at": self.created_at,
            "stale": self.stale(item),
            "note_removed": self.note_removed_at.is_some(),
        });
        if with_note {
            v["note"] = json!(self.note);
        }
        v
    }

    /// As the reviewer who gave it sees it: their own note always (while it exists).
    pub fn reviewer_json(&self, item: &ApprovalItem) -> Value {
        json!({
            "id": self.id,
            "verdict": self.verdict.as_str(),
            "note": self.note,
            "note_removed": self.note_removed_at.is_some(),
            "draft_revision": self.draft_revision,
            "created_at": self.created_at,
            "stale": self.stale(item),
        })
    }
}

/// A recommendation to store ([`crate::approvals::ApprovalRepo::recommend`]).
#[derive(Debug, Clone)]
pub struct NewRecommendation<'a> {
    pub approval_id: &'a str,
    pub reviewer: &'a str,
    pub verdict: RecVerdict,
    /// Already cleaned ([`clean_note`]).
    pub note: Option<String>,
    /// What the reviewer read: the item's `revision` and full `args_hash`.
    pub draft_revision: i64,
    pub draft_sha256: &'a str,
}

impl NewRecommendation<'_> {
    pub fn into_stored(self, now: DateTime<Utc>) -> Recommendation {
        Recommendation {
            id: format!("rec_{}", uuid::Uuid::new_v4().simple()),
            approval_id: self.approval_id.to_string(),
            reviewer: self.reviewer.to_string(),
            verdict: self.verdict,
            note: self.note,
            note_removed_at: None,
            draft_revision: self.draft_revision,
            draft_sha256: self.draft_sha256.to_string(),
            created_at: now,
            withdrawn_at: None,
        }
    }
}

/// Why the store refused a recommendation. Each is checked under the item's lock.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RecommendError {
    #[error("approval item not found")]
    NotFound,
    /// The reviewer is the draft's own owner: they decide it, they don't review it.
    #[error("a reviewer never recommends on their own draft")]
    OwnDraft,
    #[error("this draft was already decided")]
    NotPending,
    #[error("this Slack draft expired before it was decided")]
    Expired,
    /// The draft changed since the reviewer read it: what it holds now.
    #[error("this draft changed since you read it; read it again")]
    DraftChanged { revision: i64, args_hash: String },
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Why a note was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteError {
    TooLong,
}

/// Clean a note as plain text: a CR LF or lone CR becomes a line feed, a tab a space, every
/// other control character (and the Unicode line and paragraph separators) is dropped, and
/// the whole is trimmed. Empty after that is no note. More than [`MAX_NOTE`] characters is
/// refused, never cut.
pub fn clean_note(raw: &str) -> Result<Option<String>, NoteError> {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push('\n');
            }
            '\n' => out.push('\n'),
            '\t' => out.push(' '),
            '\u{2028}' | '\u{2029}' => out.push('\n'),
            c if c.is_control() => {}
            // Bidirectional overrides and isolates: they make text read other than it is.
            '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' => {}
            c => out.push(c),
        }
    }
    let trimmed = out.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.chars().count() > MAX_NOTE {
        return Err(NoteError::TooLong);
    }
    Ok(Some(trimmed.to_string()))
}

/// Is `hash` a full args hash: 64 lowercase hex characters?
pub fn valid_full_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The parts the decision's audit detail adds for `recs` (live recommendations on `item`, the
/// item as decided): "after a recommendation to approve, on this version", "… on an earlier
/// version", "after a recommendation to make changes, …", each once, approvals first. Empty
/// when there are none.
pub fn decision_detail(recs: &[Recommendation], item: &ApprovalItem) -> Vec<String> {
    let mut seen: Vec<(RecVerdict, bool)> =
        recs.iter().map(|r| (r.verdict, r.stale(item))).collect();
    seen.sort();
    seen.dedup();
    seen.into_iter()
        .map(|(verdict, stale)| decision_part(verdict, stale))
        .collect()
}

fn decision_part(verdict: RecVerdict, stale: bool) -> String {
    format!(
        "after a recommendation {}, on {}",
        verdict.advice(),
        if stale {
            "an earlier version"
        } else {
            "this version"
        }
    )
}

/// The audit detail of a recommendation: `verdict <v> revision <n>`. Never the note.
pub fn recommend_detail(verdict: RecVerdict, revision: i64) -> String {
    format!("verdict {} revision {revision}", verdict.as_str())
}

/// The codes a refused reviewer call is recorded with.
pub const REFUSAL_CODES: [&str; 10] = [
    "forbidden",
    "not_found",
    "own_draft",
    "restricted_draft",
    "not_pending",
    "expired",
    "draft_changed",
    "bad_request",
    "invalid",
    "assertion_required",
];

/// Is `part` one of the fixed formats this module writes into an audit detail (for the admin
/// and auditor views, `app/admin.rs` `safe_detail`)?
pub fn is_safe_part(part: &str) -> bool {
    if REFUSAL_CODES.contains(&part) {
        return true;
    }
    if let Some(rest) = part.strip_prefix("verdict ") {
        if let Some((v, n)) = rest.split_once(" revision ") {
            return RecVerdict::parse(v).is_some()
                && !n.is_empty()
                && n.len() <= 9
                && n.bytes().all(|b| b.is_ascii_digit());
        }
        return false;
    }
    [RecVerdict::Approve, RecVerdict::Changes]
        .iter()
        .flat_map(|v| [decision_part(*v, false), decision_part(*v, true)])
        .any(|p| p == part)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_are_cleaned_as_plain_text() {
        assert_eq!(clean_note("  ").unwrap(), None);
        assert_eq!(
            clean_note(" Looks right.\r\nShip it.\t\u{7}\u{202E} ").unwrap(),
            Some("Looks right.\nShip it.".into())
        );
        assert_eq!(
            clean_note("<!channel> <img src=x onerror=alert(1)>").unwrap(),
            Some("<!channel> <img src=x onerror=alert(1)>".into()),
            "kept as text: every surface renders it as text"
        );
        assert_eq!(
            clean_note(&"é".repeat(MAX_NOTE))
                .unwrap()
                .unwrap()
                .chars()
                .count(),
            MAX_NOTE
        );
        assert_eq!(
            clean_note(&"a".repeat(MAX_NOTE + 1)),
            Err(NoteError::TooLong)
        );
    }

    #[test]
    fn the_audit_formats_are_recognised_and_nothing_else() {
        for ok in [
            "verdict approve revision 1",
            "verdict changes revision 42",
            "after a recommendation to approve, on this version",
            "after a recommendation to make changes, on an earlier version",
            "own_draft",
            "draft_changed",
        ] {
            assert!(is_safe_part(ok), "{ok}");
        }
        for bad in [
            "verdict reject revision 1",
            "verdict approve revision x",
            "verdict approve revision 1 note: hi",
            "after a recommendation to approve",
            "Looks right. Ship it.",
        ] {
            assert!(!is_safe_part(bad), "{bad}");
        }
    }

    #[test]
    fn full_hashes_only() {
        assert!(valid_full_hash(&"a".repeat(64)));
        assert!(!valid_full_hash(&"a".repeat(63)));
        assert!(!valid_full_hash(&"A".repeat(64)));
    }
}
