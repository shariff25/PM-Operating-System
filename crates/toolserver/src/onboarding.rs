//! Onboarding (`docs/onboarding-spec.md` §2–3): memory-repo records, requests to join PM OS,
//! a platform owner's decision on them, removing a member, and each PM's setup status.
//!
//! The user database is the source of users and roles (`docs/user-database-spec.md` §4, slice
//! U3). Approving a join **first** creates the user in the database (`active`, role `pm` only
//! from the Slack bot client, the sign-in name unconfirmed when it came through that client),
//! so the person can use PM OS at once; **then** it commits the member's personal `BOT.md`
//! and its two generated sandbox policy files to the content repo in one commit, made with the
//! approving platform owner's own GitHub token, so the commit records who approved. Nothing
//! else is committed: since slice U7 there is no `team.yaml`, and no service reads one.
//! If the commit fails, the user stays active and the request is `approved` with the error;
//! the bot is missing until an admin retries. The content checkout is then pulled and reloaded
//! (`content_sync.rs`).
//!
//! Removing a member is a **deactivation** (§6): nothing is committed. The user store ends
//! their sessions and suspends their linked accounts in one transaction; the sweeper deletes
//! the accounts and closes their held drafts after the grace unless they are reactivated.
//! From the Slack bot client it is refused for an Admin or Auditor and capped per hour.
//!
//! The policy files are generated unchanged from the reviewed `pm-assistant` role template by
//! `pmos_policy`, from a scratch copy of the content with the new member in it, so the whole
//! content is linted as it will be after the commit. The approving platform owner is their
//! reviewer of record (POL-2), named in the `Approved-by` trailer.
//!
//! Decisions and removals run one at a time (a single tool server replica), so two admins
//! clicking Approve at once make one commit.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration as StdDuration;

use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use pmos_contract::model::{valid_email, valid_slack_id};
use pmos_contract::onboarding::{
    bot_path, personal_bot_name, propose_handle, render_personal_bot, spelled_handle,
    valid_new_handle,
};
use serde_json::{json, Value};

use crate::audit::{AuditEvent, Client, Outcome};
use crate::content_sync::ContentSync;
use crate::github::{valid_repo, Change, GitHub, GitHubError, COMMIT_RETRIES};
use crate::github_memory::{check_repo, RepoProblem};
use crate::members::{Created, Decided, MemberRequest, NewRequest, RequestStatus};
use crate::memory_repo::MemoryRepo;
use crate::oauth::{OAuth, TokenError, TokenSource};
use crate::service::ToolService;
use crate::session::valid_handle;
use crate::store::StoreError;
use crate::users::access::may_use;
use crate::users::{Actor, NewUser, Op, UserError, UserRole};
use crate::vault::{Provider, Secret};

/// How long an approval waits for the content checkout to pull the new commit.
const SYNC_WAIT: StdDuration = StdDuration::from_secs(20);
/// Setting a memory repo runs inside a Slack modal's acknowledgement, so GitHub gets this long.
const MEMORY_REPO_WAIT: StdDuration = StdDuration::from_millis(1800);
/// The longest display name kept with a request.
const MAX_NAME_CHARS: usize = 100;

/// GitHub and the content repo, when GitHub is configured.
pub struct GitHubSetup {
    pub github: Arc<GitHub>,
    /// `PMOS_GITHUB_ORG`.
    pub org: String,
    /// `PMOS_CONTENT_REPO`, `org/name`. Approving and removing members need it.
    pub content_repo: Option<String>,
    /// `PMOS_CONTENT_BRANCH`.
    pub content_branch: String,
    /// `PMOS_CONTENT_DIR`: where the content root sits in that repo; empty for the repo root.
    pub content_dir: String,
    /// `PMOS_POLICIES_DIR`: where the sandbox policies sit in that repo, from its root.
    pub policies_dir: String,
    /// The local content checkout (`PMOS_CONTENT_ROOT`), copied to generate a new member's
    /// sandbox policies.
    pub content_root: PathBuf,
}

fn join(dir: &str, rel: &str) -> String {
    if dir.is_empty() {
        rel.to_string()
    } else {
        format!("{dir}/{rel}")
    }
}

impl GitHubSetup {
    fn content_path(&self, rel: &str) -> String {
        join(&self.content_dir, rel)
    }

    /// `bots/<bot>.yaml` and `bots/<bot>.sandbox.yaml`, under the policies dir.
    fn policy_paths(&self, bot: &str) -> [String; 2] {
        [
            join(&self.policies_dir, &format!("bots/{bot}.yaml")),
            join(&self.policies_dir, &format!("bots/{bot}.sandbox.yaml")),
        ]
    }
}

/// The `403` code when a member decision from the Slack bot asks for the `admin` role
/// (`users::Refusal::AdminNotFromSlack`; `admin_by_pull_request` before slice U3).
pub const ADMIN_NOT_FROM_SLACK: &str = "admin_not_from_slack";

/// A refusal or failure, as the API answers it: `{error, code, request?}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub status: StatusCode,
    pub code: &'static str,
    pub error: String,
    /// The request, for `already_decided` and a failed approval.
    pub request: Option<Box<MemberRequest>>,
}

impl Refusal {
    fn new(status: StatusCode, code: &'static str, error: impl Into<String>) -> Self {
        Self {
            status,
            code,
            error: error.into(),
            request: None,
        }
    }

    fn with_request(mut self, request: MemberRequest) -> Self {
        self.request = Some(Box::new(request));
        self
    }

    pub fn body(&self) -> Value {
        let mut body = json!({"error": self.error, "code": self.code});
        if let Some(r) = &self.request {
            body["request"] = json!(r);
        }
        body
    }
}

impl From<StoreError> for Refusal {
    fn from(e: StoreError) -> Self {
        tracing::error!(error = %e, "storage unavailable");
        Refusal::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            "PM OS storage is unavailable",
        )
    }
}

/// Why a content-repo change stopped: refused (nothing changes) or failed (an approval is
/// marked `failed`).
enum Stop {
    Refuse(Refusal),
    Fail(String),
}

impl From<Refusal> for Stop {
    fn from(r: Refusal) -> Self {
        Stop::Refuse(r)
    }
}

/// The team bots `handle` owns (their personal bot aside): a deactivation, from Slack or the
/// web, is refused `409 owns_bots` while there are any, so every team bot keeps an owner who
/// is a member (spec §6 "Deactivation").
pub fn owned_team_bots(svc: &ToolService, handle: &str) -> Vec<String> {
    let personal = personal_bot_name(handle);
    let mut owned: Vec<String> = svc
        .roster()
        .bots()
        .filter(|b| b.owner == handle && b.name != personal)
        .map(|b| b.name.clone())
        .collect();
    owned.sort();
    owned
}

/// A refused or failed user write, as the API answers it.
fn user_refusal(e: UserError) -> Refusal {
    use crate::users::Refusal as R;
    let Some(r) = e.refusal() else {
        if let UserError::Store(e) = e {
            return Refusal::from(e);
        }
        unreachable!("a user error is a refusal or a storage error");
    };
    let status = match r {
        R::AdminNotFromSlack
        | R::RoleNotFromSlack
        | R::ProtectedNotFromSlack
        | R::NotFromSlack
        | R::HostOnly
        | R::Forbidden
        | R::SelfChange => StatusCode::FORBIDDEN,
        R::DeactivationLimit => StatusCode::TOO_MANY_REQUESTS,
        R::NotFound => StatusCode::NOT_FOUND,
        R::Invalid => StatusCode::UNPROCESSABLE_ENTITY,
        _ => StatusCode::CONFLICT,
    };
    let code = match r {
        R::Forbidden => "not_admin",
        R::NotFound => "not_a_member",
        R::Exists | R::Taken => "already_member",
        R::Unchanged => "already_deactivated",
        R::Invalid => "bad_request",
        r => r.code(),
    };
    Refusal::new(status, code, e.to_string())
}

fn github_not_linked(who: &str) -> Refusal {
    Refusal::new(
        StatusCode::CONFLICT,
        "github_not_linked",
        format!("{who} hasn't linked GitHub in PM OS"),
    )
}

/// What `POST /internal/members/requests/{id}/decision` asks for.
pub struct DecisionInput {
    pub decision: String,
    pub decided_by: String,
    pub handle: Option<String>,
    pub roles: Option<Vec<String>>,
}

pub struct Onboarding {
    svc: Arc<ToolService>,
    oauth: Option<Arc<OAuth>>,
    github: Option<GitHubSetup>,
    sync: Option<Arc<ContentSync>>,
    /// Admin policy and content proposals (`crate::proposals`), when turned on.
    proposals: Option<crate::proposals::ProposalsSetup>,
    /// Held for each decision and removal.
    lock: tokio::sync::Mutex<()>,
}

impl Onboarding {
    pub fn new(
        svc: Arc<ToolService>,
        oauth: Option<Arc<OAuth>>,
        github: Option<GitHubSetup>,
    ) -> Self {
        Self {
            svc,
            oauth,
            github,
            sync: None,
            proposals: None,
            lock: tokio::sync::Mutex::new(()),
        }
    }

    /// Turn admin proposals on: opened as pull requests in the content repository with the
    /// acting admin's own GitHub token. Needs the GitHub setup with a content repository;
    /// without one this changes nothing. `policies_root`: the checked-in policy files
    /// (`None`: found beside the content checkout).
    pub fn with_proposals(mut self, per_hour: u32, policies_root: Option<PathBuf>) -> Self {
        self.proposals = self
            .github
            .as_ref()
            .and_then(|gh| crate::proposals::ProposalsSetup::new(gh, per_hour, policies_root));
        self
    }

    /// The proposals set-up, when proposals are on.
    pub fn proposals(&self) -> Option<&crate::proposals::ProposalsSetup> {
        self.proposals.as_ref()
    }

    /// Account linking, when live connectors are on.
    pub fn oauth(&self) -> Option<&Arc<OAuth>> {
        self.oauth.as_ref()
    }

    /// Pull and reload the content checkout right after each member commit.
    pub fn with_sync(mut self, sync: Arc<ContentSync>) -> Self {
        self.sync = Some(sync);
        self
    }

    /// Is `handle` an active admin in the user database, now?
    async fn admin(&self, handle: &str) -> Result<bool, Refusal> {
        Ok(self.svc.people().is_admin(handle).await?)
    }

    /// The user's GitHub token, or why there isn't one.
    async fn github_token(&self, user: &str) -> Result<Secret, Stop> {
        let Some(oauth) = &self.oauth else {
            return Err(github_not_linked(user).into());
        };
        match oauth.token(user, Provider::Github).await {
            Ok(t) => Ok(t.token),
            Err(TokenError::NotConnected(_) | TokenError::Reconnect(_)) => {
                Err(github_not_linked(user).into())
            }
            Err(TokenError::Store(e)) => Err(Refusal::from(e).into()),
            Err(e) => Err(Stop::Fail(e.to_string())),
        }
    }

    // ----- memory repo ------------------------------------------------------------------

    /// Check `repo` with the PM's GitHub token and record it as their memory repo.
    pub async fn set_memory_repo(
        &self,
        user: &str,
        repo: &str,
        now: DateTime<Utc>,
    ) -> Result<MemoryRepo, Refusal> {
        let Some(gh) = &self.github else {
            return Err(Refusal::new(
                StatusCode::NOT_FOUND,
                "github_off",
                "GitHub is not configured (PMOS_GITHUB_CLIENT_ID)",
            ));
        };
        if !valid_handle(user) || !self.svc.people().allows(user).await? {
            return Err(Refusal::new(
                StatusCode::FORBIDDEN,
                "not_a_member",
                format!("`{user}` is not an active PM OS user"),
            ));
        }
        let repo = repo.trim();
        if !valid_repo(repo) {
            return Err(Refusal::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "bad_request",
                format!(
                    "`{repo}` is not a GitHub repo name like {}/pm-os-memory-{user}",
                    gh.org
                ),
            ));
        }
        let problem = |p: RepoProblem| {
            Refusal::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                p.code(),
                p.message(repo, &gh.org),
            )
        };
        let owner = repo.split('/').next().unwrap_or_default();
        if !owner.eq_ignore_ascii_case(&gh.org) {
            return Err(problem(RepoProblem::NotInOrg));
        }
        let checked =
            tokio::time::timeout(MEMORY_REPO_WAIT, self.check_memory_repo(gh, user, repo))
                .await
                .map_err(|_| {
                    Refusal::new(
                        StatusCode::GATEWAY_TIMEOUT,
                        "github_unavailable",
                        "GitHub didn't answer in time; try again",
                    )
                })?;
        let info = checked?;
        check_repo(&info, &gh.org).map_err(problem)?;
        let record = MemoryRepo {
            user: user.to_string(),
            repo: info.full_name,
            verified_at: now,
        };
        self.svc.stores().memory_repos.put(&record).await?;
        tracing::info!(user, repo = %record.repo, "memory repo set");
        Ok(record)
    }

    async fn check_memory_repo(
        &self,
        gh: &GitHubSetup,
        user: &str,
        repo: &str,
    ) -> Result<crate::github::RepoInfo, Refusal> {
        let problem = |p: RepoProblem| {
            Refusal::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                p.code(),
                p.message(repo, &gh.org),
            )
        };
        let token = match self.github_token(user).await {
            Ok(t) => t,
            Err(Stop::Refuse(r)) => return Err(r),
            Err(Stop::Fail(e)) => {
                return Err(Refusal::new(
                    StatusCode::BAD_GATEWAY,
                    "github_unavailable",
                    e,
                ))
            }
        };
        match gh.github.repo(&token, repo).await {
            Ok(info) => Ok(info),
            Err(GitHubError::NotFound(_)) => Err(problem(RepoProblem::NotFound)),
            Err(GitHubError::Reconnect) => Err(github_not_linked(user)),
            Err(e) => Err(Refusal::new(
                StatusCode::BAD_GATEWAY,
                "github_unavailable",
                e.to_string(),
            )),
        }
    }

    pub async fn memory_repo(&self, user: &str) -> Result<Option<MemoryRepo>, StoreError> {
        self.svc.stores().memory_repos.get(user).await
    }

    // ----- requests ---------------------------------------------------------------------

    /// A request to join, from someone not in the user database. `Ok((true, _))` for a new
    /// request, `Ok((false, _))` for the one already pending for that Slack id. A Slack id
    /// that belongs to a deactivated user is `409 deactivated`, not a new request.
    pub async fn request(
        &self,
        slack_id: &str,
        email: &str,
        name: &str,
        now: DateTime<Utc>,
    ) -> Result<(bool, MemberRequest), Refusal> {
        let email = email.trim();
        if !valid_slack_id(slack_id) || !valid_email(email) || email.len() > 254 {
            return Err(Refusal::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "bad_request",
                "a request needs a Slack member id and an email address",
            ));
        }
        let name: String = name
            .chars()
            .filter(|c| !c.is_control())
            .take(MAX_NAME_CHARS)
            .collect();
        let name = name.trim();
        let dir = self.svc.people().directory().await?;
        match crate::users::access::by_slack_id(&dir, slack_id) {
            Some(u) if u.active() => {
                return Err(Refusal::new(
                    StatusCode::CONFLICT,
                    "already_member",
                    "that Slack user is already a PM OS user",
                ))
            }
            Some(_) => {
                return Err(Refusal::new(
                    StatusCode::CONFLICT,
                    "deactivated",
                    "your access to PM OS was turned off; ask a platform owner",
                ))
            }
            None => {}
        }
        let store = &self.svc.stores().member_requests;
        // Handles are never reused, deactivated people's included.
        // So are their spelled forms (`ana-k` for `ana.k`): a proposed handle is always spelled
        // as itself, and one spelled as another user's would share that user's model slots
        // and bot name (the Low on colliding handles, review at 6bb806f).
        let mut taken: BTreeSet<String> = dir.users.keys().cloned().collect();
        taken.extend(dir.users.keys().map(|h| spelled_handle(h)));
        taken.extend(
            store
                .list(Some(RequestStatus::Pending))
                .await?
                .into_iter()
                .map(|r| r.proposed_handle),
        );
        let proposed = propose_handle(email, name, &taken);
        let created = store
            .create(
                NewRequest {
                    slack_id,
                    email,
                    name,
                    proposed_handle: &proposed,
                },
                now,
            )
            .await?;
        match created {
            Created::New(r) => {
                tracing::info!(id = %r.id, handle = %r.proposed_handle, "member request created");
                Ok((true, r))
            }
            Created::Existing(r) => Ok((false, r)),
            Created::TooMany => Err(Refusal::new(
                StatusCode::TOO_MANY_REQUESTS,
                "too_many_pending",
                "too many requests are waiting for a platform owner; try again later",
            )),
        }
    }

    pub async fn requests(
        &self,
        status: Option<RequestStatus>,
    ) -> Result<Vec<MemberRequest>, StoreError> {
        self.svc.stores().member_requests.list(status).await
    }

    // ----- decisions --------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    async fn audit(
        &self,
        client: Client,
        admin: &str,
        tool: &str,
        outcome: Outcome,
        run_id: &str,
        output_ref: Option<String>,
        detail: Option<String>,
        now: DateTime<Utc>,
    ) {
        self.svc
            .audit(AuditEvent {
                at: now,
                client,
                run_id: run_id.to_string(),
                user: admin.to_string(),
                skill: None,
                bot: None,
                tool: tool.to_string(),
                outcome,
                sources: Vec::new(),
                source_titles: Default::default(),
                args_hash: None,
                output_ref,
                detail,
                persona: None,
            })
            .await;
    }

    /// Approve or reject request `id` as the admin `input.decided_by`.
    pub async fn decide(
        &self,
        client: Client,
        id: &str,
        input: DecisionInput,
        now: DateTime<Utc>,
    ) -> Result<MemberRequest, Refusal> {
        let admin = input.decided_by.as_str();
        let approve = match input.decision.as_str() {
            "approve" => true,
            "reject" => false,
            other => {
                return Err(Refusal::new(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "bad_request",
                    format!("decision must be `approve` or `reject`, not `{other}`"),
                ))
            }
        };
        if !self.admin(admin).await? {
            self.audit(
                client,
                admin,
                "members.add",
                Outcome::Denied,
                id,
                None,
                Some("not_admin".into()),
                now,
            )
            .await;
            return Err(Refusal::new(
                StatusCode::FORBIDDEN,
                "not_admin",
                "only a platform owner (an active admin in PM OS) can decide this",
            ));
        }
        let _one_at_a_time = self.lock.lock().await;
        let store = &self.svc.stores().member_requests;
        let request = store
            .get(id)
            .await?
            .ok_or_else(|| Refusal::new(StatusCode::NOT_FOUND, "not_found", "no such request"))?;
        if request.status != RequestStatus::Pending {
            return Err(Refusal::new(
                StatusCode::CONFLICT,
                "already_decided",
                format!("this request was already {}", request.status.as_str()),
            )
            .with_request(request));
        }
        if !approve {
            let decided = Decided {
                status: RequestStatus::Rejected,
                decided_by: admin.to_string(),
                handle: None,
                commit: None,
                error: None,
            };
            let request = self.finish(id, &decided, now).await?;
            self.audit(
                client,
                admin,
                "members.add",
                Outcome::Rejected,
                id,
                None,
                None,
                now,
            )
            .await;
            return Ok(request);
        }

        // 1. The handle and roles.
        let handle = input
            .handle
            .clone()
            .unwrap_or_else(|| request.proposed_handle.clone());
        let bad_handle =
            |why: String| Refusal::new(StatusCode::UNPROCESSABLE_ENTITY, "bad_handle", why);
        if !valid_new_handle(&handle) {
            return Err(bad_handle(format!(
                "`{handle}` can't be a handle: lowercase letters, digits and single hyphens, at most 24"
            )));
        }
        if self.svc.people().user(&handle).await?.is_some()
            || self.svc.roster().bot(&personal_bot_name(&handle)).is_some()
        {
            return Err(bad_handle(format!("`{handle}` is already taken")));
        }
        let names: Vec<String> = input.roles.clone().unwrap_or_else(|| vec!["pm".into()]);
        if names.is_empty() {
            return Err(Refusal::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "bad_request",
                "roles must be some of pm, reviewer, auditor and admin",
            ));
        }
        // The Slack bot names the deciding admin unsigned (and the orchestrator signs nothing
        // either), so a join approved through them gets `pm` only. Admin is never granted by
        // onboarding, from any client: only `grant-admin` on the host makes an admin.
        let refused_role = if names.iter().any(|n| n == "admin") {
            Some(UserError::refused(
                crate::users::Refusal::AdminNotFromSlack,
                "PM OS never makes an admin from Slack: someone with host access runs `pmos-admin users grant-admin`",
            ))
        } else if names.iter().any(|n| n != "pm") {
            Some(UserError::refused(
                crate::users::Refusal::RoleNotFromSlack,
                "a join approved in Slack gets pm only; other roles are given in the web app",
            ))
        } else {
            None
        };
        if let Some(e) = refused_role {
            let r = user_refusal(e);
            self.audit(
                client,
                admin,
                "members.add",
                Outcome::Denied,
                id,
                None,
                Some(r.code.to_string()),
                now,
            )
            .await;
            return Err(r);
        }
        // 2. The user, in the database: they can use PM OS at once. A refusal (an address
        // that belongs to someone else, a forged admin) changes nothing.
        let created = self
            .svc
            .people()
            .apply(
                &Actor::bot(admin),
                Op::CreateUser {
                    user: NewUser {
                        handle: handle.clone(),
                        display_name: request.name.clone(),
                        sign_in_name: Some(request.email.clone()),
                        atlassian_email: Some(request.email.clone()),
                        github_login: None,
                        slack_id: Some(request.slack_id.clone()),
                        roles: [UserRole::Pm].into(),
                    },
                },
                now,
            )
            .await;
        if let Err(e) = created {
            let r = user_refusal(e);
            if r.status != StatusCode::SERVICE_UNAVAILABLE {
                self.audit(
                    client,
                    admin,
                    "members.add",
                    Outcome::Denied,
                    id,
                    None,
                    Some(r.code.to_string()),
                    now,
                )
                .await;
            }
            return Err(r);
        }
        // 3. The approver's GitHub, then one commit with the bot and its policies.
        let bot = personal_bot_name(&handle);
        let bot_md = render_personal_bot(&handle);
        let paths = self.member_paths(&bot);
        let message =
            format!("Add {handle} to PM OS\n\nApproved-by: {admin} (Slack, request {id})\n");
        let committed = self
            .commit_content(admin, &paths, &message, |existing| {
                if existing.contains(&paths[0]) {
                    return Err(bad_handle(format!("`{handle}` is already taken")).into());
                }
                let content_root = self
                    .github
                    .as_ref()
                    .map(|gh| gh.content_root.clone())
                    .unwrap_or_default();
                let [policy, spec] =
                    personal_policies(&content_root, &bot, &bot_md, Utc::now().date_naive())
                        .map_err(Stop::Fail)?;
                Ok(vec![
                    Change::write(&paths[0], &bot_md),
                    Change::write(&paths[1], policy),
                    Change::write(&paths[2], spec),
                ])
            })
            .await;

        // 4. Record the outcome. The user exists either way: a bot that didn't commit is
        // missing until an admin retries, and the request says why.
        let (commit, error) = match committed {
            Ok(commit) => (Some(commit), None),
            Err(Stop::Refuse(r)) => {
                tracing::warn!(id, handle = %handle, code = r.code, "the personal bot wasn't committed");
                (
                    None,
                    Some(format!("the personal bot wasn't committed: {}", r.error)),
                )
            }
            Err(Stop::Fail(error)) => {
                tracing::error!(id, handle = %handle, error = %error, "the personal bot wasn't committed");
                (
                    None,
                    Some(format!("the personal bot wasn't committed: {error}")),
                )
            }
        };
        let decided = Decided {
            status: RequestStatus::Approved,
            decided_by: admin.to_string(),
            handle: Some(handle.clone()),
            commit,
            error,
        };
        let request = self.finish(id, &decided, now).await?;
        self.audit(
            client,
            admin,
            "members.add",
            Outcome::Approved,
            id,
            decided.commit.clone(),
            Some(if decided.commit.is_some() {
                "roles pm; sign_in unconfirmed".to_string()
            } else {
                "roles pm; sign_in unconfirmed; bot missing".to_string()
            }),
            now,
        )
        .await;
        tracing::info!(id, handle = %handle, admin, bot = decided.commit.is_some(), "member added");
        if decided.commit.is_some() {
            self.sync_now(now).await;
        }
        Ok(request)
    }

    async fn finish(
        &self,
        id: &str,
        decided: &Decided,
        now: DateTime<Utc>,
    ) -> Result<MemberRequest, Refusal> {
        let store = &self.svc.stores().member_requests;
        match store.decide(id, decided, now).await? {
            Some(r) => Ok(r),
            // Decided elsewhere meanwhile, such as by a second tool server.
            None => {
                let current = store.get(id).await?.ok_or_else(|| {
                    Refusal::new(StatusCode::NOT_FOUND, "not_found", "no such request")
                })?;
                Err(Refusal::new(
                    StatusCode::CONFLICT,
                    "already_decided",
                    format!("this request was already {}", current.status.as_str()),
                )
                .with_request(current))
            }
        }
    }

    /// "Remove" `handle` as the admin `decided_by`: since slice U3 a **deactivation** in the
    /// user database (`docs/user-database-spec.md` §6), and nothing is committed. In one
    /// transaction: status `deactivated`, `admin` revoked if held (reactivation never restores
    /// it), their MCP sessions ended, their linked accounts suspended. Their pending drafts
    /// are held; after the grace the sweeper deletes the accounts and closes the drafts unless
    /// they are reactivated. From the Slack bot client it is refused for an Admin or Auditor
    /// (`403 protected_not_from_slack`) and capped (`429 deactivation_limit`). Returns a note
    /// for the admin.
    pub async fn remove(
        &self,
        client: Client,
        handle: &str,
        decided_by: &str,
        now: DateTime<Utc>,
    ) -> Result<String, Refusal> {
        let run_id = format!("remove-{handle}");
        if !self.admin(decided_by).await? {
            self.audit(
                client,
                decided_by,
                "members.remove",
                Outcome::Denied,
                &run_id,
                None,
                Some("not_admin".into()),
                now,
            )
            .await;
            return Err(Refusal::new(
                StatusCode::FORBIDDEN,
                "not_admin",
                "only a platform owner (an active admin in PM OS) can remove members",
            ));
        }
        if handle == decided_by {
            return Err(Refusal::new(
                StatusCode::FORBIDDEN,
                "self_removal",
                "a platform owner can't remove themselves",
            ));
        }
        let Some(user) = self.svc.people().user(handle).await? else {
            return Err(Refusal::new(
                StatusCode::NOT_FOUND,
                "not_a_member",
                format!("`{handle}` is not a PM OS user"),
            ));
        };
        // A team bot needs an owner who is a member: its owner must change first. The
        // personal bot stays in content, refused while its owner is deactivated.
        let personal = personal_bot_name(handle);
        let owned = owned_team_bots(&self.svc, handle);
        if user.active() && !owned.is_empty() {
            return Err(Refusal::new(
                StatusCode::CONFLICT,
                "owns_bots",
                format!(
                    "`{handle}` owns {}; give them another owner first",
                    owned.join(", ")
                ),
            ));
        }
        let _one_at_a_time = self.lock.lock().await;
        // The Slack bot names the deciding admin unsigned; the orchestrator signs nothing
        // either: both are the capped, unsigned client.
        self.svc
            .people()
            .apply(
                &Actor::bot(decided_by),
                Op::Deactivate {
                    handle: handle.to_string(),
                },
                now,
            )
            .await
            .map_err(user_refusal)?;
        self.audit(
            client,
            decided_by,
            "members.remove",
            Outcome::Ok,
            &run_id,
            None,
            Some("credentials suspended".into()),
            now,
        )
        .await;
        tracing::info!(handle, admin = decided_by, "member deactivated");
        Ok(format!(
            "`{handle}` is deactivated: their sessions ended and their linked accounts are suspended. \
             Reactivating them within {}h restores their accounts and held drafts. Their personal \
             bot's sandbox keeps idling until `pmos-admin pause {personal}`.",
            crate::users::CREDENTIAL_GRACE_HOURS
        ))
    }

    /// A personal bot's files in the content repo: its `BOT.md`, then its policy and its
    /// sandbox spec.
    fn member_paths(&self, bot: &str) -> [String; 3] {
        let Some(gh) = &self.github else {
            return Default::default();
        };
        let [policy, spec] = gh.policy_paths(bot);
        [gh.content_path(&bot_path(bot)), policy, spec]
    }

    /// Make the changes `changes` returns at the head of the content branch (`existing` says
    /// which of `probe` are there) and commit them in one commit with `admin`'s GitHub token.
    /// If the branch moves meanwhile, the change is made again on the new head, up to
    /// [`COMMIT_RETRIES`] times. Returns the commit.
    async fn commit_content(
        &self,
        admin: &str,
        probe: &[String],
        message: &str,
        changes: impl Fn(&BTreeSet<String>) -> Result<Vec<Change>, Stop>,
    ) -> Result<String, Stop> {
        let (gh, repo) = match &self.github {
            Some(gh) => match &gh.content_repo {
                Some(repo) => (gh, repo.as_str()),
                None => return Err(not_configured().into()),
            },
            None => return Err(not_configured().into()),
        };
        let token = self.github_token(admin).await?;
        let no_push = || {
            Refusal::new(
                StatusCode::CONFLICT,
                "no_push",
                format!("{admin}'s GitHub account can't push to {repo}"),
            )
        };
        let fail = |e: GitHubError| match e {
            GitHubError::Reconnect => Stop::Refuse(github_not_linked(admin)),
            e => Stop::Fail(e.to_string()),
        };
        match gh.github.repo(&token, repo).await {
            Ok(info) if info.push => {}
            Ok(_) | Err(GitHubError::NotFound(_)) => return Err(no_push().into()),
            Err(e) => return Err(fail(e)),
        }
        let branch = gh.content_branch.as_str();
        for attempt in 0..=COMMIT_RETRIES {
            let head = gh.github.head(&token, repo, branch).await.map_err(fail)?;
            let mut existing = BTreeSet::new();
            for path in probe {
                let found = gh
                    .github
                    .file(&token, repo, path, &head.commit)
                    .await
                    .map_err(fail)?;
                if found.is_some() {
                    existing.insert(path.clone());
                }
            }
            let changes = changes(&existing)?;
            let commit = gh
                .github
                .create_commit(&token, repo, &head, message, &changes)
                .await
                .map_err(fail)?;
            match gh.github.update_ref(&token, repo, branch, &commit).await {
                Ok(()) => return Ok(commit),
                Err(GitHubError::Conflict) => {
                    tracing::info!(
                        repo,
                        branch,
                        attempt,
                        "content branch moved; committing again"
                    );
                }
                Err(e) => return Err(fail(e)),
            }
        }
        Err(Stop::Fail(format!(
            "{branch} kept moving; gave up after {} tries",
            COMMIT_RETRIES + 1
        )))
    }

    /// Pull and reload the content checkout now, so the change is live when the answer
    /// goes back. A failure is logged; the periodic sync tries again.
    async fn sync_now(&self, now: DateTime<Utc>) {
        let Some(sync) = &self.sync else {
            tracing::warn!(
                "content sync is off (PMOS_CONTENT_SYNC_SECS=0): reindex to load the change"
            );
            return;
        };
        match tokio::time::timeout(SYNC_WAIT, sync.sync_once(now)).await {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => tracing::error!(error = %e, "content sync after a member change failed"),
            Err(_) => {
                tracing::warn!(
                    "content sync after a member change is slow; it continues in the background"
                );
                sync.trigger();
            }
        }
    }

    // ----- status -----------------------------------------------------------------------

    /// What `setup` shows: for a member by handle or Slack id, or for someone who isn't one
    /// yet, by Slack id, with their pending request.
    pub async fn status(
        &self,
        user: Option<&str>,
        slack_id: Option<&str>,
    ) -> Result<Value, StoreError> {
        let dir = self.svc.people().directory().await?;
        let member = match (user, slack_id) {
            (Some(user), _) if !crate::session::is_bot_identity(user) => dir.get(user),
            (None, Some(slack_id)) => crate::users::access::by_slack_id(&dir, slack_id),
            _ => None,
        }
        .filter(|u| may_use(u))
        .cloned();
        let Some(member) = member else {
            let pending = match slack_id {
                Some(slack_id) => {
                    self.svc
                        .stores()
                        .member_requests
                        .pending_for(slack_id)
                        .await?
                }
                None => None,
            };
            return Ok(json!({
                "member": false,
                "handle": null,
                "admin": false,
                "linked": Provider::ALL
                    .iter()
                    .map(|p| (p.as_str().to_string(), json!(false)))
                    .collect::<serde_json::Map<_, _>>(),
                "memory_repo": null,
                "personal_bot": null,
                "pending_request": pending,
            }));
        };
        let handle = member.handle.as_str();
        let linked: Vec<Provider> = match &self.oauth {
            Some(oauth) => oauth
                .vault()
                .list(handle)
                .await?
                .into_iter()
                .map(|c| c.provider)
                .collect(),
            None => Vec::new(),
        };
        let memory_repo = self
            .svc
            .stores()
            .memory_repos
            .get(handle)
            .await?
            .map(|r| r.repo);
        let bot = personal_bot_name(handle);
        let personal_bot = self
            .svc
            .roster()
            .bot(&bot)
            .is_some_and(|(b, _)| b.owner == handle)
            .then_some(bot);
        Ok(json!({
            "member": true,
            "handle": handle,
            "admin": member.has(UserRole::Admin),
            "linked": Provider::ALL
                .iter()
                .map(|p| (p.as_str().to_string(), json!(linked.contains(p))))
                .collect::<serde_json::Map<_, _>>(),
            "memory_repo": memory_repo,
            "personal_bot": personal_bot,
            "pending_request": null,
        }))
    }
}

fn not_configured() -> Refusal {
    Refusal::new(
        StatusCode::CONFLICT,
        "not_configured",
        "changing members needs PMOS_CONTENT_REPO and the GitHub App (PMOS_GITHUB_CLIENT_ID)",
    )
}

/// A new member's sandbox policy and spec, generated by `pmos_policy` from a scratch copy of
/// the content at `content_root` with the bot's `BOT.md` in place. The copy must pass
/// `pmos lint` as a whole, as the content will after the commit.
pub fn personal_policies(
    content_root: &Path,
    bot: &str,
    bot_md: &str,
    today: chrono::NaiveDate,
) -> Result<[String; 2], String> {
    let dir = tempfile::tempdir().map_err(|e| format!("a scratch directory: {e}"))?;
    let root = dir.path().join("content");
    let io = |e: std::io::Error| format!("copying the content to generate policies: {e}");
    for entry in walkdir::WalkDir::new(content_root)
        .into_iter()
        .filter_entry(|e| e.file_name() != ".git")
    {
        let entry = entry.map_err(|e| format!("reading the content: {e}"))?;
        let rel = entry
            .path()
            .strip_prefix(content_root)
            .map_err(|e| e.to_string())?;
        let dest = root.join(rel);
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&dest).map_err(io)?;
        } else if entry.file_type().is_file() {
            std::fs::copy(entry.path(), &dest).map_err(io)?;
        }
    }
    let bot_file = root.join(bot_path(bot));
    std::fs::create_dir_all(bot_file.parent().expect("a bot file has a folder")).map_err(io)?;
    std::fs::write(&bot_file, bot_md).map_err(io)?;
    let generated = pmos_policy::generate(&root, today).map_err(|errors| {
        let lines: Vec<String> = errors
            .iter()
            .map(|d| format!("{}: {}: {}", d.code, d.path, d.message))
            .collect();
        format!(
            "the content with the new member fails the lint: {}",
            lines.join("; ")
        )
    })?;
    let take = |rel: String| {
        generated
            .files
            .get(&rel)
            .cloned()
            .ok_or_else(|| format!("no generated {rel}"))
    };
    Ok([
        take(format!("bots/{bot}.yaml"))?,
        take(format!("bots/{bot}.sandbox.yaml"))?,
    ])
}
