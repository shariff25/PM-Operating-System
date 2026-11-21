//! What a Reviewer reads and recommends (`docs/user-database-spec.md` §8, slice U6a;
//! `crate::reviews` holds the types and the rules that don't need the service).
//!
//! **Who.** An active user holding `reviewer`, read from the user database for this call
//! (never the assertion's roles), else `Forbidden`. Holding `admin` is not enough: an admin
//! reviews only if they also hold `reviewer` and are assigned.
//!
//! **Whose drafts.** The pending drafts of the PMs assigned to them (`reviewer_assignments`),
//! while those PMs may use PM OS (a deactivated PM's held drafts are nobody's to review). A
//! draft of anyone else answers `NotFound`, exactly as a missing one, so a reviewer can't
//! learn which ids exist; their own draft answers `OwnDraft` (they decide it as its owner).
//!
//! **What they see.** The draft's text and arguments, its target and its sources, and their
//! own live recommendation; never other reviewers' recommendations or notes, never tasks,
//! traces or routines. A draft built from restricted sources (or unrecorded ones, which count
//! the same), including a follow-up whose restriction came from its conversation's history
//! (W6: the session started at that level), is **metadata only**: who, what kind, when, and
//! that it is restricted; no `args`, no `args_hash`, no target, no sources, and no
//! recommendation on it.
//!
//! **What it never does.** No route here reaches a decision, a write or a connector: the
//! store's `recommend` inserts a row and touches nothing else.
//!
//! **Audited.** Each inbox listing, each read and each recommendation, outcome `ok`, as the
//! reviewer with client `web`; every refusal `denied` with its code. The item's id and args
//! hash are recorded; never its text or the note (a recommendation's detail is
//! `verdict <v> revision <n>`).

use std::collections::BTreeSet;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use pmos_contract::Sensitivity;
use serde_json::{json, Value};

use super::{display_name, ToolService};
use crate::approvals::{ApprovalItem, Status, PENDING_CAP};
use crate::audit::{AuditEvent, Client, Outcome};
use crate::reviews::{
    recommend_detail, NewRecommendation, RecVerdict, RecommendError, Recommendation, INBOX_ACTION,
    READ_ACTION, RECOMMEND_ACTION, REVIEWS_RUN,
};
use crate::store::StoreError;
use crate::users::access::may_use;
use crate::users::{Directory, UserRole};

/// Why a reviewer's call was refused. [`ReviewError::code`] is the wire code.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ReviewError {
    /// Not an active user holding `reviewer`.
    #[error("only an active reviewer may review drafts")]
    Forbidden,
    /// No such draft, or it isn't a draft of a PM assigned to this reviewer.
    #[error("no such draft")]
    NotFound,
    #[error("a reviewer never reviews their own draft; they decide it as its owner")]
    OwnDraft,
    /// Built from restricted sources: its owner can share it, PM OS doesn't.
    #[error("this draft uses restricted sources; its owner can share it with you")]
    Restricted,
    #[error("this draft was already decided")]
    NotPending,
    #[error("this Slack draft expired before it was decided")]
    Expired,
    /// The draft changed since the reviewer read it: read it again.
    #[error("this draft changed since you read it; read it again")]
    DraftChanged { revision: i64, args_hash: String },
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl ReviewError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Forbidden => "forbidden",
            Self::NotFound => "not_found",
            Self::OwnDraft => "own_draft",
            Self::Restricted => "restricted_draft",
            Self::NotPending => "not_pending",
            Self::Expired => "expired",
            Self::DraftChanged { .. } => "draft_changed",
            Self::Store(_) => "unavailable",
        }
    }
}

impl From<RecommendError> for ReviewError {
    fn from(e: RecommendError) -> Self {
        match e {
            RecommendError::NotFound => Self::NotFound,
            RecommendError::OwnDraft => Self::OwnDraft,
            RecommendError::NotPending => Self::NotPending,
            RecommendError::Expired => Self::Expired,
            RecommendError::DraftChanged {
                revision,
                args_hash,
            } => Self::DraftChanged {
                revision,
                args_hash,
            },
            RecommendError::Store(e) => Self::Store(e),
        }
    }
}

/// A reviewer's inbox.
#[derive(Debug, Clone)]
pub struct ReviewInbox {
    /// Each pending draft as [`ToolService::review_view`] shows it, newest first.
    pub items: Vec<Value>,
    /// How many are pending in all: more than `items.len()` only past the safety cap.
    pub total: usize,
    /// The PMs whose drafts these are (assigned and active), sorted.
    pub pms: Vec<String>,
}

/// Is `item` built from restricted sources, or from unrecorded ones (which count the same)?
pub fn restricted(item: &ApprovalItem) -> bool {
    item.sources_sensitivity
        .is_none_or(|s| s == Sensitivity::Restricted)
}

/// What a reviewer call's audit event is about.
#[derive(Debug, Clone, Copy)]
pub enum About<'a> {
    /// The inbox: no one draft.
    Nothing,
    /// A draft read: its id and args hash are recorded.
    Item(&'a ApprovalItem),
    /// A draft id as the caller named it (a refusal).
    Id(&'a str),
}

impl ToolService {
    /// Record one reviewer call.
    pub async fn record_review(
        &self,
        reviewer: &str,
        action: &str,
        outcome: Outcome,
        about: About<'_>,
        detail: Option<String>,
        now: DateTime<Utc>,
    ) {
        let (item, target) = match about {
            About::Nothing => (None, None),
            About::Item(item) => (Some(item), None),
            About::Id(id) => (None, Some(id)),
        };
        self.audit(AuditEvent {
            at: now,
            client: Client::Web,
            run_id: REVIEWS_RUN.to_string(),
            user: reviewer.to_string(),
            skill: None,
            bot: None,
            tool: action.to_string(),
            outcome,
            sources: Vec::new(),
            source_titles: Default::default(),
            args_hash: item.map(|i| i.args_hash.clone()),
            output_ref: item.map(|i| i.id.clone()).or(target.map(String::from)),
            detail,
            persona: item.and_then(|i| i.persona.clone()),
        })
        .await;
    }

    async fn refused(
        &self,
        reviewer: &str,
        action: &str,
        target: Option<&str>,
        e: ReviewError,
        now: DateTime<Utc>,
    ) -> ReviewError {
        if !matches!(e, ReviewError::Store(_)) {
            let about = target.map_or(About::Nothing, About::Id);
            self.record_review(
                reviewer,
                action,
                Outcome::Denied,
                about,
                Some(e.code().to_string()),
                now,
            )
            .await;
        }
        e
    }

    /// The directory, and the PMs `reviewer` may review now: assigned to them and able to
    /// use PM OS. `Forbidden` unless `reviewer` is an active user holding `reviewer`.
    async fn review_scope(
        &self,
        reviewer: &str,
    ) -> Result<(Arc<Directory>, BTreeSet<String>), ReviewError> {
        let dir = self.people.directory().await?;
        let is_reviewer = dir
            .get(reviewer)
            .is_some_and(|u| u.active() && u.has(UserRole::Reviewer));
        if !is_reviewer {
            return Err(ReviewError::Forbidden);
        }
        let pms = dir
            .reviews(reviewer)
            .into_iter()
            .filter(|pm| pm != reviewer && dir.get(pm).is_some_and(may_use))
            .collect();
        Ok((dir, pms))
    }

    /// Item `id`, if `reviewer` may review it now: pending, of a PM in their scope, not their
    /// own. A Slack draft past its time expires first.
    async fn reviewable(
        &self,
        reviewer: &str,
        id: &str,
        now: DateTime<Utc>,
    ) -> Result<(ApprovalItem, Arc<Directory>), ReviewError> {
        let (dir, pms) = self.review_scope(reviewer).await?;
        let Some(mut item) = self.stores.approvals.get(id).await? else {
            return Err(ReviewError::NotFound);
        };
        if item.user == reviewer {
            return Err(ReviewError::OwnDraft);
        }
        if !pms.contains(&item.user) {
            return Err(ReviewError::NotFound);
        }
        if self.overdue(&item, now) {
            self.expire_drafts(now).await?;
            item = self
                .stores
                .approvals
                .get(id)
                .await?
                .ok_or(ReviewError::NotFound)?;
        }
        match item.status {
            Status::Pending => Ok((item, dir)),
            Status::Expired => Err(ReviewError::Expired),
            _ => Err(ReviewError::NotPending),
        }
    }

    /// One pending draft as a reviewer sees it (see the module docs). `mine` is their own
    /// live recommendation on it; `sources` the drafting run's sources, for a single read.
    pub fn review_view(
        &self,
        item: &ApprovalItem,
        dir: &Directory,
        mine: Option<&Recommendation>,
        sources: Option<&crate::sources::SourcesReport>,
    ) -> Value {
        let restricted = restricted(item);
        let mut v = json!({
            "id": item.id,
            "owner": item.user,
            "owner_name": display_name(Some(dir), &item.user),
            "tool": item.tool,
            "bot": item.bot,
            "persona": item.persona,
            "skill": item.skill,
            "status": item.status.as_str(),
            "created_at": item.created_at,
            "expires_at": self.expires_at(item),
            "revision": item.revision,
            "restricted": restricted,
            "sources_sensitivity": item.sources_sensitivity.map(|s| s.to_string()),
        });
        if restricted {
            return v;
        }
        v["args"] = item.args.clone();
        v["args_hash"] = json!(item.args_hash);
        v["edited"] = json!(item.edited());
        v["edited_at"] = json!(item.edited_at);
        if let Some(url) = crate::sources::target_url(&item.tool, &item.args, None) {
            v["target_url"] = json!(url);
        }
        // A public channel's name only: a private channel's name and a direct message's other
        // person are the owner's to know (they are in it), not a reviewer's.
        if let Some(target) = self.target_channel(item).filter(|t| t["kind"] == "channel") {
            v["target_channel"] = target;
        }
        v["my_recommendation"] = mine.map_or(Value::Null, |r| r.reviewer_json(item));
        if let Some(report) = sources {
            v["sources"] = json!(report.sources);
            v["sources_available"] = json!(report.available);
        }
        v
    }

    /// `reviewer`'s own live recommendations on `items`.
    async fn own_recommendations(
        &self,
        reviewer: &str,
        items: &[ApprovalItem],
    ) -> Result<Vec<Recommendation>, StoreError> {
        let ids: Vec<String> = items.iter().map(|i| i.id.clone()).collect();
        Ok(self
            .stores
            .approvals
            .recommendations(&ids)
            .await?
            .into_iter()
            .filter(|r| r.reviewer == reviewer)
            .collect())
    }

    /// `reviewer`'s inbox: the pending drafts of their PMs, newest first, at most
    /// [`PENDING_CAP`]. Audited.
    pub async fn review_inbox(
        &self,
        reviewer: &str,
        now: DateTime<Utc>,
    ) -> Result<ReviewInbox, ReviewError> {
        let (dir, pms) = match self.review_scope(reviewer).await {
            Ok(scope) => scope,
            Err(e) => return Err(self.refused(reviewer, INBOX_ACTION, None, e, now).await),
        };
        let pms: Vec<String> = pms.into_iter().collect();
        // Slack drafts past their time expire first, as a decision would find them.
        self.expire_drafts(now).await?;
        let (items, total) = self.stores.approvals.pending_of(&pms, PENDING_CAP).await?;
        let mine = self.own_recommendations(reviewer, &items).await?;
        let views = items
            .iter()
            .map(|i| {
                let own = mine.iter().find(|r| r.approval_id == i.id);
                self.review_view(i, &dir, own, None)
            })
            .collect();
        self.record_review(
            reviewer,
            INBOX_ACTION,
            Outcome::Ok,
            About::Nothing,
            None,
            now,
        )
        .await;
        Ok(ReviewInbox {
            items: views,
            total,
            pms,
        })
    }

    /// One draft `reviewer` may review, with its sources unless it is restricted. Audited.
    pub async fn review_item(
        &self,
        reviewer: &str,
        id: &str,
        now: DateTime<Utc>,
    ) -> Result<Value, ReviewError> {
        let (item, dir) = match self.reviewable(reviewer, id, now).await {
            Ok(found) => found,
            Err(e) => return Err(self.refused(reviewer, READ_ACTION, Some(id), e, now).await),
        };
        let sources = if restricted(&item) {
            None
        } else {
            Some(self.approval_sources(&item).await?)
        };
        let mine = self
            .own_recommendations(reviewer, std::slice::from_ref(&item))
            .await?;
        let view = self.review_view(&item, &dir, mine.first(), sources.as_ref());
        self.record_review(
            reviewer,
            READ_ACTION,
            Outcome::Ok,
            About::Item(&item),
            None,
            now,
        )
        .await;
        Ok(view)
    }

    /// Record `reviewer`'s recommendation on draft `id`, bound to the `revision` and full
    /// `args_hash` they read; `note` already cleaned (`crate::reviews::clean_note`). Refused
    /// on a restricted draft, and as the store refuses (`draft_changed` with what the draft
    /// holds now). Never changes the item or calls anything. Audited, without the note.
    /// Answers the recommendation and the draft as the reviewer now sees it.
    #[allow(clippy::too_many_arguments)]
    pub async fn recommend(
        &self,
        reviewer: &str,
        id: &str,
        verdict: RecVerdict,
        note: Option<String>,
        revision: i64,
        args_hash: &str,
        now: DateTime<Utc>,
    ) -> Result<(Recommendation, Value), ReviewError> {
        let found = match self.reviewable(reviewer, id, now).await {
            Ok((item, _)) if restricted(&item) => Err(ReviewError::Restricted),
            other => other,
        };
        let (item, dir) = match found {
            Ok(found) => found,
            Err(e) => {
                return Err(self
                    .refused(reviewer, RECOMMEND_ACTION, Some(id), e, now)
                    .await)
            }
        };
        let stored = self
            .stores
            .approvals
            .recommend(
                NewRecommendation {
                    approval_id: &item.id,
                    reviewer,
                    verdict,
                    note,
                    draft_revision: revision,
                    draft_sha256: args_hash,
                },
                now,
            )
            .await;
        let rec = match stored {
            Ok(rec) => rec,
            Err(e) => {
                let e = ReviewError::from(e);
                return Err(self
                    .refused(reviewer, RECOMMEND_ACTION, Some(id), e, now)
                    .await);
            }
        };
        self.record_review(
            reviewer,
            RECOMMEND_ACTION,
            Outcome::Ok,
            About::Item(&item),
            Some(recommend_detail(verdict, revision)),
            now,
        )
        .await;
        let view = self.review_view(&item, &dir, Some(&rec), None);
        Ok((rec, view))
    }
}
