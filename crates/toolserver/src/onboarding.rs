//! Onboarding (`docs/onboarding-spec.md` §2–3): memory-repo records, requests to join PM OS,
//! a platform owner's decision on them, removing a member, and each PM's setup status.
//!
//! `team.yaml` stays the source of truth. Approving a member commits the new `team.yaml`, the
//! member's personal `BOT.md` and its two generated sandbox policy files to the content repo
//! in one commit, made with the approving platform owner's own GitHub token, so the commit
//! records who approved. The in-memory team changes at once; the content checkout is then
//! pulled and reloaded (`content_sync.rs`).
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
use pmos_contract::model::{valid_email, valid_slack_id, Role, TeamFile, TeamMember};
use pmos_contract::onboarding::{
    bot_path, personal_bot_name, propose_handle, render_personal_bot, render_team_yaml,
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

/// The `403` code when a member decision from the Slack bot asks for the `admin` role.
pub const ADMIN_BY_PULL_REQUEST: &str = "admin_by_pull_request";

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
            lock: tokio::sync::Mutex::new(()),
        }
    }

    /// Pull and reload the content checkout right after each member commit.
    pub fn with_sync(mut self, sync: Arc<ContentSync>) -> Self {
        self.sync = Some(sync);
        self
    }

    fn admin(&self, handle: &str) -> bool {
        self.svc.team().member(handle).is_some_and(|m| m.is_admin())
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
        if !valid_handle(user) || !self.svc.team().allows(user) {
            return Err(Refusal::new(
                StatusCode::FORBIDDEN,
                "not_a_member",
                format!("`{user}` is not in team.yaml"),
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

    /// A request to join, from someone not in `team.yaml`. `Ok((true, _))` for a new
    /// request, `Ok((false, _))` for the one already pending for that Slack id.
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
        let team = self.svc.team();
        if team.by_slack_id(slack_id).is_some() {
            return Err(Refusal::new(
                StatusCode::CONFLICT,
                "already_member",
                "that Slack user is already in team.yaml",
            ));
        }
        let store = &self.svc.stores().member_requests;
        let mut taken: BTreeSet<String> =
            team.file().members.into_iter().map(|m| m.handle).collect();
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
        if !self.admin(admin) {
            self.audit(
                client,
                admin,
                "members.add",
                Outcome::Denied,
                id,
                None,
                Some("decision by someone who isn't an admin in team.yaml".into()),
                now,
            )
            .await;
            return Err(Refusal::new(
                StatusCode::FORBIDDEN,
                "not_admin",
                "only a platform owner (admin in team.yaml) can decide this",
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
        if self.svc.team().member(&handle).is_some()
            || self.svc.roster().bot(&personal_bot_name(&handle)).is_some()
        {
            return Err(bad_handle(format!("`{handle}` is already taken")));
        }
        let roles = match &input.roles {
            None => vec![Role::Pm],
            Some(names) => {
                let parsed: Result<Vec<Role>, _> = names
                    .iter()
                    .map(|n| serde_json::from_value::<Role>(json!(n)))
                    .collect();
                match parsed {
                    Ok(roles) if !roles.is_empty() => roles,
                    _ => {
                        return Err(Refusal::new(
                            StatusCode::UNPROCESSABLE_ENTITY,
                            "bad_request",
                            "roles must be some of pm, skill-author and admin",
                        ))
                    }
                }
            }
        };
        // The Slack bot names the deciding admin unsigned, so it may grant `pm` and
        // `skill-author` only. A new admin is made by a pull request to team.yaml.
        if client == Client::Bot && roles.contains(&Role::Admin) {
            self.audit(
                client,
                admin,
                "members.add",
                Outcome::Denied,
                id,
                None,
                Some("an admin grant from Slack: admins are added by a pull request".into()),
                now,
            )
            .await;
            return Err(Refusal::new(
                StatusCode::FORBIDDEN,
                ADMIN_BY_PULL_REQUEST,
                "PM OS won't make an admin from Slack: add the admin role with a pull request to team.yaml",
            ));
        }
        let member = TeamMember {
            handle: handle.clone(),
            slack_id: request.slack_id.clone(),
            email: Some(request.email.clone()),
            github: None,
            entra_oid: None,
            roles,
        };

        // 2–3. The approver's GitHub, then one commit with team.yaml, the bot and its policies.
        let bot = personal_bot_name(&handle);
        let bot_md = render_personal_bot(&handle);
        let paths = self.member_paths(&bot);
        let message =
            format!("Add {handle} to PM OS\n\nApproved-by: {admin} (Slack, request {id})\n");
        let committed = self
            .commit_team(
                admin,
                &paths,
                &message,
                |team, existing| {
                    if team.by_handle(&handle).is_some() || existing.contains(&paths[0]) {
                        return Err(bad_handle(format!("`{handle}` is already taken")).into());
                    }
                    if team.by_slack_id(&member.slack_id).is_some() {
                        return Err(Refusal::new(
                            StatusCode::CONFLICT,
                            "already_member",
                            "that Slack user is already in team.yaml",
                        )
                        .into());
                    }
                    team.members.push(member.clone());
                    Ok(vec![Change::write(&paths[0], &bot_md)])
                },
                |yaml| {
                    let content_root = self
                        .github
                        .as_ref()
                        .map(|gh| gh.content_root.clone())
                        .unwrap_or_default();
                    let [policy, spec] = personal_policies(
                        &content_root,
                        yaml,
                        &bot,
                        &bot_md,
                        Utc::now().date_naive(),
                    )
                    .map_err(Stop::Fail)?;
                    Ok(vec![
                        Change::write(&paths[1], policy),
                        Change::write(&paths[2], spec),
                    ])
                },
            )
            .await;

        // 4–5. Record the outcome.
        let (decided, outcome) = match committed {
            Ok((commit, team)) => {
                // The member can use PM OS at once, before the checkout is pulled.
                self.svc.team().replace(team);
                (
                    Decided {
                        status: RequestStatus::Approved,
                        decided_by: admin.to_string(),
                        handle: Some(handle.clone()),
                        commit: Some(commit),
                        error: None,
                    },
                    Outcome::Approved,
                )
            }
            Err(Stop::Refuse(r)) => return Err(r),
            Err(Stop::Fail(error)) => {
                tracing::error!(id, handle = %handle, error = %error, "adding a member failed");
                (
                    Decided {
                        status: RequestStatus::Failed,
                        decided_by: admin.to_string(),
                        handle: Some(handle.clone()),
                        commit: None,
                        error: Some(format!("PM OS couldn't commit team.yaml: {error}")),
                    },
                    Outcome::Failed,
                )
            }
        };
        let request = self.finish(id, &decided, now).await?;
        self.audit(
            client,
            admin,
            "members.add",
            outcome,
            id,
            decided.commit.clone(),
            Some(format!("handle {handle}")),
            now,
        )
        .await;
        if outcome == Outcome::Failed {
            return Err(Refusal::new(
                StatusCode::BAD_GATEWAY,
                "commit_failed",
                decided.error.unwrap_or_default(),
            )
            .with_request(request));
        }
        tracing::info!(id, handle = %handle, admin, "member added");
        self.sync_now(now).await;
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

    /// Remove `handle` from PM OS as the admin `decided_by`: commit `team.yaml` without them
    /// and delete their personal `BOT.md`, then unlink every account and drop their memory-repo
    /// record. Returns the commit.
    pub async fn remove(
        &self,
        client: Client,
        handle: &str,
        decided_by: &str,
        now: DateTime<Utc>,
    ) -> Result<String, Refusal> {
        let run_id = format!("remove-{handle}");
        if !self.admin(decided_by) {
            self.audit(
                client,
                decided_by,
                "members.remove",
                Outcome::Denied,
                &run_id,
                None,
                Some("removal by someone who isn't an admin in team.yaml".into()),
                now,
            )
            .await;
            return Err(Refusal::new(
                StatusCode::FORBIDDEN,
                "not_admin",
                "only a platform owner (admin in team.yaml) can remove members",
            ));
        }
        if handle == decided_by {
            return Err(Refusal::new(
                StatusCode::FORBIDDEN,
                "self_removal",
                "a platform owner can't remove themselves",
            ));
        }
        let not_member = || {
            Refusal::new(
                StatusCode::NOT_FOUND,
                "not_a_member",
                format!("`{handle}` is not in team.yaml"),
            )
        };
        if self.svc.team().member(handle).is_none() {
            return Err(not_member());
        }
        // A bot whose owner isn't in team.yaml fails the lint, and the content would stop
        // loading: its owner must change first. The personal bot goes with its owner.
        let personal = personal_bot_name(handle);
        let owned: Vec<String> = self
            .svc
            .roster()
            .bots()
            .filter(|b| b.owner == handle && b.name != personal)
            .map(|b| b.name.clone())
            .collect();
        if !owned.is_empty() {
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
        let paths = self.member_paths(&personal);
        let message = format!("Remove {handle} from PM OS\n\nRemoved-by: {decided_by} (Slack)\n");
        let committed = self
            .commit_team(
                decided_by,
                &paths,
                &message,
                |team, existing| {
                    if team.by_handle(handle).is_none() {
                        return Err(not_member().into());
                    }
                    team.members.retain(|m| m.handle != handle);
                    // The bot and its policy files go with them.
                    Ok(paths
                        .iter()
                        .filter(|p| existing.contains(*p))
                        .map(Change::delete)
                        .collect())
                },
                |_| Ok(Vec::new()),
            )
            .await;
        let commit = match committed {
            Ok((commit, team)) => {
                self.svc.team().replace(team);
                commit
            }
            Err(Stop::Refuse(r)) => return Err(r),
            Err(Stop::Fail(error)) => {
                tracing::error!(handle, error = %error, "removing a member failed");
                self.audit(
                    client,
                    decided_by,
                    "members.remove",
                    Outcome::Failed,
                    &run_id,
                    None,
                    Some(error.clone()),
                    now,
                )
                .await;
                return Err(Refusal::new(
                    StatusCode::BAD_GATEWAY,
                    "commit_failed",
                    format!("PM OS couldn't commit team.yaml: {error}"),
                ));
            }
        };
        // Their accounts and memory record go; the memory repo itself is theirs.
        for provider in Provider::ALL {
            let unlinked = match &self.oauth {
                Some(oauth) => oauth.unlink_account(handle, provider).await,
                None => self.svc.stores().credentials.delete(handle, provider).await,
            };
            if let Err(e) = unlinked {
                tracing::error!(handle, provider = %provider, error = %e, "REMOVED MEMBER'S ACCOUNT NOT UNLINKED");
            }
        }
        if let Err(e) = self.svc.stores().memory_repos.delete(handle).await {
            tracing::error!(handle, error = %e, "removed member's memory-repo record not deleted");
        }
        self.audit(
            client,
            decided_by,
            "members.remove",
            Outcome::Ok,
            &run_id,
            Some(commit.clone()),
            Some(format!("handle {handle}")),
            now,
        )
        .await;
        tracing::info!(handle, admin = decided_by, commit = %commit, "member removed");
        self.sync_now(now).await;
        Ok(commit)
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

    /// Change `team.yaml` at the head of the content branch with `edit`, which may also
    /// return other changes (`existing` says which of `probe` are there), then add what
    /// `with_yaml` makes from the new `team.yaml`, and commit it all in one commit with
    /// `admin`'s GitHub token. The new `team.yaml` must parse and pass the team lint rules.
    /// If the branch moves meanwhile, the change is made again on the new head, up to
    /// [`COMMIT_RETRIES`] times. Returns the commit and the team it committed.
    async fn commit_team(
        &self,
        admin: &str,
        probe: &[String],
        message: &str,
        edit: impl Fn(&mut TeamFile, &BTreeSet<String>) -> Result<Vec<Change>, Stop>,
        with_yaml: impl Fn(&str) -> Result<Vec<Change>, Stop>,
    ) -> Result<(String, TeamFile), Stop> {
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
        let team_path = gh.content_path("team.yaml");
        for attempt in 0..=COMMIT_RETRIES {
            let head = gh.github.head(&token, repo, branch).await.map_err(fail)?;
            let current = gh
                .github
                .file(&token, repo, &team_path, &head.commit)
                .await
                .map_err(fail)?
                .map(|f| f.text)
                .unwrap_or_default();
            let mut team = TeamFile::from_yaml(&current).map_err(|e| {
                Stop::Fail(format!(
                    "{team_path} at the head of {branch} doesn't parse: {e}"
                ))
            })?;
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
            let mut changes = edit(&mut team, &existing)?;
            let yaml = render_team_yaml(&current, &team.members);
            lint_team(&yaml, chrono::Utc::now().date_naive()).map_err(Stop::Fail)?;
            changes.extend(with_yaml(&yaml)?);
            changes.insert(0, Change::write(&team_path, yaml));
            let commit = gh
                .github
                .create_commit(&token, repo, &head, message, &changes)
                .await
                .map_err(fail)?;
            match gh.github.update_ref(&token, repo, branch, &commit).await {
                Ok(()) => return Ok((commit, team)),
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
        let team = self.svc.team();
        let member = match (user, slack_id) {
            (Some(user), _) => team.member(user),
            (None, Some(slack_id)) => team.by_slack_id(slack_id),
            (None, None) => None,
        };
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
            "admin": member.is_admin(),
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
/// the content at `content_root` with `team_yaml` and the bot's `BOT.md` in place. The copy
/// must pass `pmos lint` as a whole, as the content will after the commit.
pub fn personal_policies(
    content_root: &Path,
    team_yaml: &str,
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
    std::fs::write(root.join("team.yaml"), team_yaml).map_err(io)?;
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

/// Does `yaml` parse as `team.yaml` and pass the team lint rules (`team-*`)? It is linted
/// alone in a scratch content root, so only the team rules can fire.
pub fn lint_team(yaml: &str, today: chrono::NaiveDate) -> Result<(), String> {
    TeamFile::from_yaml(yaml).map_err(|e| format!("the new team.yaml doesn't parse: {e}"))?;
    let dir = tempfile::tempdir().map_err(|e| format!("a scratch directory: {e}"))?;
    std::fs::write(dir.path().join("team.yaml"), yaml)
        .map_err(|e| format!("a scratch team.yaml: {e}"))?;
    let report = pmos_contract::lint(dir.path(), &pmos_contract::LintOptions::new(today));
    let problems: Vec<String> = report
        .errors()
        .filter(|d| d.code.starts_with("team-"))
        .map(|d| format!("{}: {}", d.code, d.message))
        .collect();
    if problems.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "the new team.yaml fails the team lint: {}",
            problems.join("; ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_team_lint_catches_what_the_contract_catches() {
        let today = "2026-11-14".parse().unwrap();
        let ok = "members:\n  - handle: \"ana\"\n    slack-id: \"U012AB3CD\"\n    roles: [pm]\n";
        assert_eq!(lint_team(ok, today), Ok(()));
        let dup = format!("{ok}  - handle: \"ana\"\n    slack-id: \"U0X\"\n    roles: [pm]\n");
        assert!(lint_team(&dup, today)
            .unwrap_err()
            .contains("team-duplicate"));
        let bad_slack = "members:\n  - handle: \"ana\"\n    slack-id: \"nope\"\n    roles: [pm]\n";
        assert!(lint_team(bad_slack, today)
            .unwrap_err()
            .contains("team-slack-id"));
        assert!(lint_team("members: [", today)
            .unwrap_err()
            .contains("parse"));
    }
}
