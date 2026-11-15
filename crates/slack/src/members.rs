//! Onboarding in Slack (`docs/onboarding-spec.md` §4).
//!
//! 1. **Request access.** Someone not in `team.yaml` DMs PM OS or uses `/docs` and gets a
//!    note with **Request access**. A tap reads their name and email from `users.info` and
//!    files the request with the tool server.
//! 2. **The platform owners decide.** Each admin in `team.yaml` gets a DM card with
//!    **Approve**, **Approve with another handle** and **Reject**. The bot keeps every card's
//!    `(channel, ts)` by request id, in memory, so once someone decides, all the cards show
//!    the outcome. The tool server commits the change to the content repo with the deciding
//!    admin's GitHub account.
//! 3. **The new member sets up.** They get a welcome DM and the setup checklist, which
//!    `setup` (or `status`, `onboarding`) shows again: Jira and Confluence, Slack and GitHub
//!    links, the memory repo, and their bot.
//!
//! Platform owners also have `requests` (the pending requests, as cards) and
//! `remove <handle>` (with a confirmation).

use std::sync::Mutex;
use std::time::Duration;

use pmos_contract::model::{valid_handle, valid_slack_id, TeamFile};
use pmos_contract::onboarding::{personal_bot_name, valid_new_handle};
use serde_json::{json, Value};

use crate::bots::{Bots, Click, Dm};
use crate::bounded::Bounded;
use crate::cards;
use crate::commands::{FollowUp, NOT_A_MEMBER};
use crate::member_cards::{self as mc, action, Setup, HANDLE_BLOCK, REPO_BLOCK};
use crate::toolserver::{request_id_ok, request_of, ApiError, PROVIDERS};

/// How long the memory-repo modal waits for the tool server: the answer has to be in the
/// ack, which Slack wants within 3 seconds of sending the submission.
pub const MEMORY_ACK_WAIT: Duration = Duration::from_millis(2200);

const ONLY_ADMINS: &str = "Only platform owners can decide this.";
const TOOLSERVER_DOWN: &str =
    "PM OS couldn't reach the tool server, so nothing was changed. Try again in a minute.";

/// A DM command, typed alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DmCommand {
    /// `setup`, `status` or `onboarding`: the checklist.
    Setup,
    /// `requests` (admins): the pending access requests.
    Requests,
    /// `remove <handle>` (admins): the confirmation card.
    Remove(String),
}

/// The DM command `text` is, if any. `requests` and `remove` are commands for admins only;
/// for anyone else they are ordinary messages to their bot. `remove <@U…>` names the member
/// by their Slack mention.
pub fn dm_command(text: &str, admin: bool, team: &TeamFile) -> Option<DmCommand> {
    let t = text
        .trim()
        .trim_end_matches(['?', '.', '!'])
        .trim()
        .to_lowercase();
    match t.as_str() {
        "setup" | "status" | "onboarding" => return Some(DmCommand::Setup),
        "requests" if admin => return Some(DmCommand::Requests),
        _ => {}
    }
    if !admin {
        return None;
    }
    let mut words = text.split_whitespace();
    let (Some(verb), Some(who), None) = (words.next(), words.next(), words.next()) else {
        return None;
    };
    if !verb.eq_ignore_ascii_case("remove") {
        return None;
    }
    let who = who.trim_end_matches(['?', '.', '!']);
    if let Some(id) = who.strip_prefix("<@").and_then(|w| w.strip_suffix('>')) {
        let id = id.split('|').next().unwrap_or_default();
        return team
            .by_slack_id(id)
            .map(|m| DmCommand::Remove(m.handle.clone()));
    }
    let handle = who.trim_start_matches('@').to_lowercase();
    valid_handle(&handle).then_some(DmCommand::Remove(handle))
}

/// A request, as last seen, and every card showing it.
struct Tracked {
    request: Value,
    cards: Vec<(String, String)>,
}

/// What the bot keeps for onboarding.
pub struct Onboarding {
    github_org: Option<String>,
    response_url_prefix: String,
    /// Request id → the cards showing it. In memory: after a restart, a decision updates only
    /// the card that was tapped, and `requests` posts fresh ones.
    tracked: Mutex<Bounded<String, Tracked>>,
}

impl Onboarding {
    pub fn new(github_org: Option<String>, response_url_prefix: String) -> Self {
        Self {
            github_org: github_org.filter(|o| !o.trim().is_empty()),
            response_url_prefix,
            tracked: Mutex::new(Bounded::new(500)),
        }
    }

    fn track(&self, request: &Value, channel: &str, ts: &str) {
        let Some(id) = request["id"].as_str() else {
            return;
        };
        let mut tracked = self.tracked.lock().expect("tracked lock");
        let card = (channel.to_string(), ts.to_string());
        match tracked.get_mut(&id.to_string()) {
            Some(t) => {
                t.request = request.clone();
                if !t.cards.contains(&card) {
                    t.cards.push(card);
                }
            }
            None => tracked.insert(
                id.to_string(),
                Tracked {
                    request: request.clone(),
                    cards: vec![card],
                },
            ),
        }
    }

    fn request(&self, id: &str) -> Option<Value> {
        self.tracked
            .lock()
            .expect("tracked lock")
            .get(&id.to_string())
            .map(|t| t.request.clone())
    }

    /// Every card for `id`, with `also` (the card that was tapped) if it isn't tracked.
    fn cards(&self, id: &str, also: Option<(&str, &str)>) -> Vec<(String, String)> {
        let mut cards = self
            .tracked
            .lock()
            .expect("tracked lock")
            .get(&id.to_string())
            .map(|t| t.cards.clone())
            .unwrap_or_default();
        if let Some((channel, ts)) = also {
            let card = (channel.to_string(), ts.to_string());
            if !cards.contains(&card) {
                cards.push(card);
            }
        }
        cards
    }
}

fn str_of<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or_default()
}

/// A modal's text input, by block id.
fn input_value(payload: &Value, block: &str) -> String {
    payload["view"]["state"]["values"][block]["value"]["value"]
        .as_str()
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn field_error(block: &str, message: &str) -> Value {
    json!({"response_action": "errors", "errors": {block: message}})
}

impl Bots {
    // ---- Request access ----

    /// Answer a click on the **Request access** note: through its `response_url` (the note
    /// is ephemeral), replacing it when `replace`, else as a new private note.
    async fn answer_note(&self, click: &Click, text: &str, replace: bool) {
        let prefix = &self.onboarding().response_url_prefix;
        match click
            .response_url
            .as_deref()
            .filter(|u| u.starts_with(prefix.as_str()))
        {
            Some(url) => {
                let message = json!({
                    "response_type": "ephemeral",
                    "replace_original": replace,
                    "text": text,
                });
                self.slack().respond(url, &message).await;
            }
            None => {
                self.private(&click.channel, &click.user, None, text).await;
            }
        }
    }

    pub(crate) async fn request_access(&self, click: Click) {
        let team = self.team();
        if team.by_slack_id(&click.user).is_some() {
            self.answer_note(
                &click,
                "You're already a PM OS member. DM me `setup` to see what's left to set up.",
                true,
            )
            .await;
            return;
        }
        let info = match self.slack().user_info(&click.user).await {
            Ok(info) => info,
            Err(e) => {
                tracing::warn!(error = %e, "users.info failed for an access request");
                self.answer_note(
                    &click,
                    "Slack couldn't tell me who you are right now. Try again in a minute.",
                    false,
                )
                .await;
                return;
            }
        };
        let email = info["profile"]["email"].as_str().unwrap_or_default().trim();
        if email.is_empty() {
            self.answer_note(
                &click,
                "Slack has no email address for you, so I can't send the request. Ask a \
                 platform owner to add you.",
                true,
            )
            .await;
            return;
        }
        let name = [
            &info["real_name"],
            &info["profile"]["real_name"],
            &info["profile"]["display_name"],
            &info["name"],
        ]
        .into_iter()
        .filter_map(|v| v.as_str())
        .map(str::trim)
        .find(|n| !n.is_empty())
        .unwrap_or_default()
        .to_string();
        match self
            .toolserver()
            .request_access(&click.user, email, &name)
            .await
        {
            Ok((true, request)) => {
                tracing::info!(request = %request["id"], "access request filed");
                let told = self.tell_admins(&team, &request).await;
                let text = if told > 0 {
                    "Sent to the platform owners. I'll DM you when it's decided."
                } else {
                    "Your request is saved, but I couldn't reach a platform owner just now. \
                     They'll see it in their list of requests; I'll DM you when it's decided."
                };
                self.answer_note(&click, text, true).await;
            }
            Ok((false, _)) => {
                self.answer_note(
                    &click,
                    "Your request is already with the platform owners. I'll DM you when it's \
                     decided.",
                    true,
                )
                .await;
            }
            Err(e) => {
                let (text, replace) = match e.code() {
                    Some("already_member") => (
                        "You're already a PM OS member. DM me `setup` to see what's left to \
                         set up."
                            .to_string(),
                        true,
                    ),
                    Some("too_many_pending") => (
                        "Too many access requests are waiting right now. Try again later."
                            .to_string(),
                        false,
                    ),
                    Some("bad_request") => (
                        "Slack's details for you don't look right, so I can't send the \
                         request. Ask a platform owner to add you."
                            .to_string(),
                        true,
                    ),
                    _ if e == ApiError::Unavailable => (
                        "PM OS couldn't send your request right now. Try again in a minute."
                            .to_string(),
                        false,
                    ),
                    _ => (
                        format!("PM OS couldn't send your request: {}", cards_escape(&e)),
                        false,
                    ),
                };
                self.answer_note(&click, &text, replace).await;
            }
        }
    }

    /// DM every admin a card for `request`. Returns how many got one.
    async fn tell_admins(&self, team: &TeamFile, request: &Value) -> usize {
        let mut told = 0;
        for admin in team.admins() {
            if !valid_slack_id(&admin.slack_id) {
                continue;
            }
            let channel = match self.slack().open_dm(&admin.slack_id).await {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(error = %e, admin = %admin.handle, "couldn't open a DM with an admin");
                    continue;
                }
            };
            if let Some(posted) = self
                .post(&channel, None, None, mc::request_card(request))
                .await
            {
                self.onboarding()
                    .track(request, &posted.channel, &posted.ts);
                told += 1;
            }
        }
        if told == 0 {
            tracing::warn!("no platform owner could be told about an access request");
        }
        told
    }

    // ---- Deciding ----

    /// The clicker's handle if they are an admin; anyone else gets a private note.
    async fn admin(&self, user: &str, channel: &str, refusal: &str) -> Option<String> {
        let team = self.team();
        match team.by_slack_id(user) {
            Some(m) if m.is_admin() => Some(m.handle.clone()),
            _ => {
                tracing::info!(slack_id = user, "member decision by a non-admin; refused");
                self.private(channel, user, None, refusal).await;
                None
            }
        }
    }

    pub(crate) async fn decide_member(&self, click: Click, decision: &str, handle: Option<String>) {
        self.decide_on(
            &click.user,
            &click.channel,
            &click.message_ts,
            &click.value,
            decision,
            handle.as_deref(),
        )
        .await;
    }

    async fn decide_on(
        &self,
        user: &str,
        channel: &str,
        ts: &str,
        id: &str,
        decision: &str,
        handle: Option<&str>,
    ) {
        let Some(admin) = self.admin(user, channel, ONLY_ADMINS).await else {
            return;
        };
        if !request_id_ok(id) {
            tracing::warn!("member decision without a request id; ignored");
            return;
        }
        let outcome = self
            .toolserver()
            .decide_member(id, &admin, decision, handle)
            .await;
        match outcome {
            Ok(request) => {
                tracing::info!(request = id, status = %request["status"], "access request decided");
                self.show_member_decision(&request, &admin, (channel, ts))
                    .await;
            }
            Err(e) => match e.code() {
                Some("github_not_linked") => {
                    let text = "To approve, PM OS commits the new member to the content repo \
                                as you, so your GitHub account has to be linked first. Connect \
                                it, then tap *Approve* again.";
                    let mut blocks =
                        vec![json!({"type": "section", "text": {"type": "mrkdwn", "text": text}})];
                    if let Ok(url) = self.toolserver().oauth_start("github", &admin).await {
                        blocks.push(json!({"type": "actions", "elements": [{
                            "type": "button",
                            "text": {"type": "plain_text", "text": "Connect GitHub"},
                            "action_id": action::CONNECT_GITHUB,
                            "url": url,
                        }]}));
                    }
                    if let Err(e) = self
                        .slack()
                        .ephemeral_blocks(channel, user, None, text, Value::Array(blocks))
                        .await
                    {
                        tracing::warn!(error = %e, "private note failed");
                    }
                }
                Some("no_push") => {
                    self.private(
                        channel,
                        user,
                        None,
                        "Your GitHub account can't push to the content repo, so PM OS can't \
                         commit the new member as you. Ask for write access to it, or let \
                         another platform owner approve. Nothing was changed.",
                    )
                    .await;
                }
                Some("already_decided") => {
                    let request = match &e {
                        ApiError::Refused { body, .. } => request_of(body).clone(),
                        ApiError::Unavailable => Value::Null,
                    };
                    if request["id"].is_string() {
                        // Whoever decided it updated the other cards and told the requester.
                        let line = mc::decision_line(&request, &admin);
                        self.update(channel, ts, mc::request_closed(&request, &line))
                            .await;
                        self.onboarding().track(&request, channel, ts);
                    } else {
                        self.private(channel, user, None, "This request was already decided.")
                            .await;
                    }
                }
                Some("bad_handle") => {
                    let why = match &e {
                        ApiError::Refused { error, .. } if !error.is_empty() => {
                            format!(" ({})", cards_escape(&e))
                        }
                        _ => String::new(),
                    };
                    let note = format!(
                        "That handle can't be used{why}. Tap *Approve with another handle* to \
                         pick one. Nothing was changed."
                    );
                    self.private(channel, user, None, &note).await;
                }
                _ if e.status() == Some(502) => {
                    // The approval failed and the request is closed: show it on every card.
                    let request = match &e {
                        ApiError::Refused { body, .. } => request_of(body).clone(),
                        ApiError::Unavailable => Value::Null,
                    };
                    if request["id"].is_string() {
                        self.show_member_decision(&request, &admin, (channel, ts))
                            .await;
                    } else {
                        let note = format!("Approving failed: {}", cards_escape(&e));
                        self.private(channel, user, None, &note).await;
                    }
                }
                _ if e.status() == Some(403) => {
                    self.private(channel, user, None, ONLY_ADMINS).await;
                }
                _ if e.status() == Some(404) => {
                    self.private(channel, user, None, "That request no longer exists.")
                        .await;
                }
                _ if e == ApiError::Unavailable => {
                    self.private(channel, user, None, TOOLSERVER_DOWN).await;
                }
                _ => {
                    let note = format!(
                        "That didn't go through: {}. Nothing was changed.",
                        cards_escape(&e)
                    );
                    self.private(channel, user, None, &note).await;
                }
            },
        }
    }

    /// Put a request this click decided on every admin's card, and DM the requester.
    async fn show_member_decision(&self, request: &Value, admin: &str, tapped: (&str, &str)) {
        let id = str_of(request, "id");
        let line = mc::decision_line(request, admin);
        for (channel, ts) in self.onboarding().cards(id, Some(tapped)) {
            self.update(&channel, &ts, mc::request_closed(request, &line))
                .await;
        }
        self.onboarding().track(request, tapped.0, tapped.1);
        self.tell_requester(request, admin).await;
    }

    /// The requester's DM: a welcome and the checklist, or a short note.
    async fn tell_requester(&self, request: &Value, admin: &str) {
        let slack_id = str_of(request, "slack_id");
        if !valid_slack_id(slack_id) {
            return;
        }
        let status = str_of(request, "status");
        if !matches!(status, "approved" | "rejected" | "failed") {
            return;
        }
        let channel = match self.slack().open_dm(slack_id).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "couldn't open a DM with the requester");
                return;
            }
        };
        match status {
            "approved" => {
                let handle = str_of(request, "handle");
                self.post(
                    &channel,
                    None,
                    None,
                    mc::welcome(str_of(request, "name"), handle, admin),
                )
                .await;
                let setup = self.setup(handle).await;
                self.post(&channel, None, None, mc::checklist(&setup)).await;
            }
            "rejected" => {
                self.post(
                    &channel,
                    None,
                    None,
                    cards::note(
                        "A platform owner turned down your request for PM OS access. If you \
                         think that's a mistake, ask them directly.",
                    ),
                )
                .await;
            }
            _ => {
                self.post(
                    &channel,
                    None,
                    None,
                    cards::note(
                        "Adding you to PM OS didn't go through, and the platform owners have \
                         been told. You can request access again.",
                    ),
                )
                .await;
            }
        }
    }

    pub(crate) async fn open_handle_modal(&self, click: Click) {
        if self
            .admin(&click.user, &click.channel, ONLY_ADMINS)
            .await
            .is_none()
        {
            return;
        }
        let id = click.value.as_str();
        let request = match self.onboarding().request(id) {
            Some(r) => Some(r),
            None => self
                .toolserver()
                .pending_requests()
                .await
                .ok()
                .and_then(|rs| rs.into_iter().find(|r| r["id"] == id)),
        };
        let Some(request) = request else {
            self.private(
                &click.channel,
                &click.user,
                None,
                "This request isn't waiting any more. Say `requests` to see the ones that are.",
            )
            .await;
            return;
        };
        let view = mc::handle_modal(&request, &click.channel, &click.message_ts);
        if let Err(e) = self.slack().open_view(&click.trigger_id, view).await {
            tracing::warn!(error = %e, "views.open failed");
            self.private(
                &click.channel,
                &click.user,
                None,
                "Slack couldn't open the form. Tap *Approve with another handle* again.",
            )
            .await;
        }
    }

    /// The handle modal's submission. The handle is checked here so a bad one shows under
    /// the field; the approval itself runs after the ack.
    pub(crate) fn submit_handle(&self, payload: &Value) -> (Option<Value>, Option<FollowUp>) {
        let handle = input_value(payload, HANDLE_BLOCK).to_lowercase();
        let team = self.team();
        if !valid_new_handle(&handle) {
            return (
                Some(field_error(
                    HANDLE_BLOCK,
                    "Use lowercase letters, digits and hyphens, starting with a letter, up to \
                     24 characters.",
                )),
                None,
            );
        }
        if team.by_handle(&handle).is_some() {
            return (
                Some(field_error(
                    HANDLE_BLOCK,
                    "Someone already has that handle.",
                )),
                None,
            );
        }
        let meta: Value = payload["view"]["private_metadata"]
            .as_str()
            .and_then(|m| serde_json::from_str(m).ok())
            .unwrap_or(Value::Null);
        let user = str_of(&payload["user"], "id").to_string();
        let (Some(id), Some(channel), Some(ts)) = (
            meta["id"].as_str().map(String::from),
            meta["channel"].as_str().map(String::from),
            meta["ts"].as_str().map(String::from),
        ) else {
            tracing::warn!("handle submitted without its card's details; ignored");
            return (None, None);
        };
        let this = self.clone();
        (
            None,
            Some(Box::pin(async move {
                this.decide_on(&user, &channel, &ts, &id, "approve", Some(&handle))
                    .await
            })),
        )
    }

    // ---- Admin commands ----

    pub(crate) async fn list_requests(&self, dm: &Dm) {
        let thread = dm.in_thread.then_some(dm.thread_ts.as_str());
        match self.toolserver().pending_requests().await {
            Ok(requests) if requests.is_empty() => {
                self.post(
                    &dm.channel,
                    thread,
                    None,
                    cards::note("No access requests are waiting."),
                )
                .await;
            }
            Ok(requests) => {
                for request in requests.iter().take(20) {
                    if let Some(posted) = self
                        .post(&dm.channel, thread, None, mc::request_card(request))
                        .await
                    {
                        self.onboarding()
                            .track(request, &posted.channel, &posted.ts);
                    }
                }
            }
            Err(e) => {
                let note = match e {
                    ApiError::Unavailable => TOOLSERVER_DOWN.to_string(),
                    e => format!("PM OS couldn't list the requests: {}", cards_escape(&e)),
                };
                self.post(&dm.channel, thread, None, cards::note(&note))
                    .await;
            }
        }
    }

    pub(crate) async fn confirm_remove(&self, dm: &Dm, admin: &str, who: &str) {
        let thread = dm.in_thread.then_some(dm.thread_ts.as_str());
        let team = self.team();
        let note = if who == admin {
            Some("You can't remove yourself; ask another platform owner.".to_string())
        } else if team.by_handle(who).is_none() {
            Some(format!(
                "`{}` isn't a PM OS member.",
                crate::blocks::escape(who)
            ))
        } else {
            None
        };
        if let Some(note) = note {
            self.post(&dm.channel, thread, None, cards::note(&note))
                .await;
            return;
        }
        let slack_id = team
            .by_handle(who)
            .map(|m| m.slack_id.clone())
            .unwrap_or_default();
        self.post(
            &dm.channel,
            thread,
            None,
            mc::remove_confirm(who, &slack_id),
        )
        .await;
    }

    pub(crate) async fn remove_member(&self, click: Click, confirm: bool) {
        let refusal = "Only platform owners can remove members.";
        let Some(admin) = self.admin(&click.user, &click.channel, refusal).await else {
            return;
        };
        let handle = click.value.clone();
        if !confirm {
            self.update(
                &click.channel,
                &click.message_ts,
                mc::remove_closed(&handle, ":no_entry_sign: Cancelled; nothing was changed."),
            )
            .await;
            return;
        }
        let slack_id = self
            .team()
            .by_handle(&handle)
            .map(|m| m.slack_id.clone())
            .unwrap_or_default();
        // Take the buttons away at once, so a second tap can't remove twice.
        self.update(
            &click.channel,
            &click.message_ts,
            mc::remove_closed(&handle, ":hourglass_flowing_sand: Removing…"),
        )
        .await;
        let card = match self.toolserver().remove_member(&handle, &admin).await {
            Ok(done) => {
                tracing::info!(member = %handle, "member removed");
                mc::remove_closed(
                    &handle,
                    &mc::removed_line(&handle, &admin, str_of(&done, "commit")),
                )
            }
            Err(e) if e.status() == Some(404) => mc::remove_closed(
                &handle,
                &format!(
                    "`{}` isn't a PM OS member any more; nothing to remove.",
                    crate::blocks::escape(&handle)
                ),
            ),
            Err(e) if e.status() == Some(403) => mc::remove_retry(
                &handle,
                &slack_id,
                "The tool server refused: only platform owners can remove members, and not \
                 themselves.",
            ),
            Err(ApiError::Unavailable) => mc::remove_retry(&handle, &slack_id, TOOLSERVER_DOWN),
            Err(e) => {
                let why = match e.code() {
                    Some("github_not_linked") => {
                        "Removing commits to the content repo as you: link GitHub first (DM \
                         me `setup`)."
                            .to_string()
                    }
                    Some("no_push") => {
                        "Your GitHub account can't push to the content repo.".to_string()
                    }
                    _ => format!("That didn't go through: {e}"),
                };
                mc::remove_retry(&handle, &slack_id, &why)
            }
        };
        self.update(&click.channel, &click.message_ts, card).await;
    }

    // ---- The setup checklist ----

    /// What `handle` has set up. Anything PM OS can't find out shows as not done yet.
    pub(crate) async fn setup(&self, handle: &str) -> Setup {
        let state = match self.toolserver().onboarding(handle).await {
            Ok(state) => state,
            Err(e) => {
                tracing::warn!(error = %e, "onboarding state unavailable");
                Value::Null
            }
        };
        let mut links = Vec::new();
        for provider in PROVIDERS {
            let linked = state["linked"][provider].as_bool().unwrap_or(false);
            let url = if linked {
                None
            } else {
                self.toolserver().oauth_start(provider, handle).await.ok()
            };
            links.push((provider, linked, url));
        }
        let bot = personal_bot_name(handle);
        let bot_ready = match self.fresh_roster().await {
            Ok(roster) => roster.iter().any(|b| b.name == bot),
            Err(_) => state["personal_bot"].as_str() == Some(bot.as_str()),
        };
        Setup {
            handle: handle.to_string(),
            links,
            memory_repo: state["memory_repo"]
                .as_str()
                .filter(|r| !r.is_empty())
                .map(String::from),
            bot_ready,
        }
    }

    pub(crate) async fn post_setup(&self, dm: &Dm, handle: &str) {
        let setup = self.setup(handle).await;
        let thread = dm.in_thread.then_some(dm.thread_ts.as_str());
        self.post(&dm.channel, thread, None, mc::checklist(&setup))
            .await;
    }

    pub(crate) async fn refresh_setup(&self, click: Click) {
        let Some(member) = self.team().by_slack_id(&click.user).cloned() else {
            self.private(&click.channel, &click.user, None, NOT_A_MEMBER)
                .await;
            return;
        };
        let setup = self.setup(&member.handle).await;
        self.update(&click.channel, &click.message_ts, mc::checklist(&setup))
            .await;
    }

    pub(crate) async fn open_memory_modal(&self, click: Click) {
        let Some(member) = self.team().by_slack_id(&click.user).cloned() else {
            self.private(&click.channel, &click.user, None, NOT_A_MEMBER)
                .await;
            return;
        };
        let view = mc::memory_modal(
            &member.handle,
            self.onboarding().github_org.as_deref(),
            &click.channel,
            &click.message_ts,
        );
        if let Err(e) = self.slack().open_view(&click.trigger_id, view).await {
            tracing::warn!(error = %e, "views.open failed");
            self.private(
                &click.channel,
                &click.user,
                None,
                "Slack couldn't open the form. Tap *Set memory repo* again.",
            )
            .await;
        }
    }

    /// The memory-repo modal's submission: the tool server checks the repo while Slack waits
    /// for the ack, so a refusal shows under the field. Success closes the modal and
    /// refreshes the checklist.
    pub(crate) async fn submit_memory_repo(
        &self,
        payload: Value,
    ) -> (Option<Value>, Option<FollowUp>) {
        let user = str_of(&payload["user"], "id").to_string();
        let Some(member) = self.team().by_slack_id(&user).cloned() else {
            return (Some(field_error(REPO_BLOCK, NOT_A_MEMBER)), None);
        };
        let org = self.onboarding().github_org.clone();
        let repo = input_value(&payload, REPO_BLOCK);
        if !mc::valid_repo(&repo) {
            let example = format!(
                "{}/pm-os-memory-{}",
                org.as_deref().unwrap_or("your-org"),
                member.handle
            );
            return (
                Some(field_error(
                    REPO_BLOCK,
                    &format!("Use org/name, like {example}."),
                )),
                None,
            );
        }
        match self
            .toolserver()
            .set_memory_repo(&member.handle, &repo, MEMORY_ACK_WAIT)
            .await
        {
            Ok(_) => {
                tracing::info!(user = %member.handle, "memory repo set");
                let meta: Value = payload["view"]["private_metadata"]
                    .as_str()
                    .and_then(|m| serde_json::from_str(m).ok())
                    .unwrap_or(Value::Null);
                let this = self.clone();
                let handle = member.handle.clone();
                let work: FollowUp = Box::pin(async move {
                    let (Some(channel), Some(ts)) = (meta["channel"].as_str(), meta["ts"].as_str())
                    else {
                        return;
                    };
                    let setup = this.setup(&handle).await;
                    this.update(channel, ts, mc::checklist(&setup)).await;
                });
                (None, Some(work))
            }
            Err(ApiError::Unavailable) => (
                Some(field_error(
                    REPO_BLOCK,
                    "PM OS couldn't check that repo in time. Try again in a minute.",
                )),
                None,
            ),
            Err(e) => {
                let error = match &e {
                    ApiError::Refused { error, .. } => error.clone(),
                    ApiError::Unavailable => String::new(),
                };
                (
                    Some(field_error(
                        REPO_BLOCK,
                        &mc::memory_error(e.code(), &error, org.as_deref()),
                    )),
                    None,
                )
            }
        }
    }
}

/// An error for `mrkdwn`, escaped.
fn cards_escape(e: &ApiError) -> String {
    crate::blocks::escape(&crate::blocks::truncate(&e.to_string(), 300))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_typed_alone() {
        let team = TeamFile::from_yaml(
            "members:\n  - {handle: ana, slack-id: U0ANA, roles: [pm, admin]}\n  - {handle: ben, slack-id: U0BEN, roles: [pm]}\n",
        )
        .unwrap();
        for t in ["setup", "Status", " onboarding? "] {
            assert_eq!(dm_command(t, false, &team), Some(DmCommand::Setup), "{t}");
        }
        assert_eq!(dm_command("docs: setup", false, &team), None);
        assert_eq!(dm_command("setup my week", false, &team), None);
        assert_eq!(
            dm_command("requests", true, &team),
            Some(DmCommand::Requests)
        );
        assert_eq!(dm_command("requests", false, &team), None, "admins only");
        assert_eq!(
            dm_command("remove ben", true, &team),
            Some(DmCommand::Remove("ben".into()))
        );
        assert_eq!(
            dm_command("remove <@U0BEN>", true, &team),
            Some(DmCommand::Remove("ben".into()))
        );
        assert_eq!(dm_command("remove <@U0NOBODY>", true, &team), None);
        assert_eq!(dm_command("remove ben", false, &team), None);
        assert_eq!(dm_command("remove duplicate rows", true, &team), None);
    }
}
