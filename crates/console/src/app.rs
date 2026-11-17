//! The four screens (bot runtime spec §6): Bots, Routines, Approvals and Activity (with a
//! page per task). Everything is read from the bot runtime and the tool server; the only
//! actions are pausing, resuming and deleting routines.

use std::fmt::Write;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{FromRequestParts, Path, Query, State};
use axum::http::request::Parts;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use futures_util::future::join_all;
use pmos_contract::model::TeamFile;
use serde::Deserialize;

use crate::audit;
use crate::auth::{self, AuthError, Csrf, Viewer};
use crate::config::{Config, Identity};
use crate::html::{self, badge, clip, empty, esc, page, table, when, Esc, Screen};
use crate::upstream::{BotDetail, BotSummary, Routine, Task, Upstream, UpstreamError};

/// How many recent tasks the Activity screen lists.
pub const RECENT_TASKS: u32 = 50;

/// What a list shows in place of the text of someone else's task or routine: the runtime sends
/// the console only its metadata.
pub const HIDDEN_TEXT: &str = "<span class=\"muted\">text hidden</span>";

struct Inner {
    identity: Identity,
    content_root: PathBuf,
    bots: Upstream,
    toolserver: Upstream,
    audit_log: Option<PathBuf>,
    slack_url: Option<String>,
    public_url: Option<String>,
    csrf: Csrf,
}

#[derive(Clone)]
pub struct AppState(Arc<Inner>);

pub fn router(config: Config) -> Router {
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("an HTTP client with default TLS");
    let state = AppState(Arc::new(Inner {
        bots: Upstream::new(
            http.clone(),
            &config.bots_url,
            &config.bots_token,
            "bot runtime",
        ),
        toolserver: Upstream::new(
            http,
            &config.toolserver_url,
            &config.toolserver_token,
            "tool server",
        ),
        identity: config.identity,
        content_root: config.content_root,
        audit_log: config.audit_log,
        slack_url: config.slack_url,
        public_url: config.public_url,
        csrf: Csrf::new(),
    }));
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/", get(|| async { Redirect::to("/bots") }))
        .route("/bots", get(bots))
        .route("/routines", get(routines))
        .route("/routines/{id}/{action}", post(routine_action))
        .route("/approvals", get(approvals))
        .route("/activity", get(activity))
        .route("/tasks/{id}", get(task))
        .fallback(|| async {
            message(
                StatusCode::NOT_FOUND,
                None,
                "Not found",
                "There's no such page.",
            )
        })
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            through_the_proxy,
        ))
        .layer(axum::middleware::map_response(security_headers))
        .with_state(state)
}

/// With `PMOS_CONSOLE_TRUST_PROXY=1`, every request but the container's health check must
/// carry the sign-in proxy's `X-Pmos-Proxy-Secret`, or it is refused with `401` before any
/// handler (or `X-Forwarded-User`) is looked at.
async fn through_the_proxy(
    State(state): State<AppState>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let health_check = req.method() == axum::http::Method::GET && req.uri().path() == "/healthz";
    if health_check || auth::from_proxy(&state.0.identity, req.headers()) {
        return next.run(req).await;
    }
    tracing::warn!(path = %req.uri().path(), "refused: no valid X-Pmos-Proxy-Secret");
    not_from_proxy()
}

fn not_from_proxy() -> Response {
    message(
        StatusCode::UNAUTHORIZED,
        None,
        "Not signed in",
        "Open the console through the PM OS sign-in.",
    )
}

/// Every response: no caching, no framing, no sniffing, and the page policy.
async fn security_headers(mut res: Response) -> Response {
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_str(&html::CSP).expect("an ASCII policy"),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("same-origin"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

fn html_response(status: StatusCode, body: String) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        body,
    )
        .into_response()
}

/// A page that only says something: an error, or that there's nothing here.
fn message(status: StatusCode, viewer: Option<&Viewer>, title: &str, text: &str) -> Response {
    html_response(
        status,
        page(
            title,
            None,
            viewer,
            &format!("<p class=\"error\">{}</p>", Esc(text)),
        ),
    )
}

fn upstream_failed(viewer: &Viewer, screen: Screen, err: UpstreamError) -> Response {
    let (service, why) = match err {
        UpstreamError::NotFound => ("upstream", "answered 404 Not Found".to_string()),
        UpstreamError::Failed { service, why } => (service, why),
    };
    tracing::warn!(service, why = %why, "upstream call failed");
    html_response(
        StatusCode::BAD_GATEWAY,
        page(
            "Not available right now",
            Some(screen),
            Some(viewer),
            &format!(
                "<p class=\"error\">The {} {}. Try again in a minute; if it keeps happening, \
                 tell a platform owner.</p>",
                Esc(service),
                Esc(&why)
            ),
        ),
    )
}

/// The person using the console, checked against `team.yaml` on every request.
pub struct Signed {
    pub viewer: Viewer,
    pub team: TeamFile,
}

impl FromRequestParts<AppState> for Signed {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Response> {
        let team = TeamFile::load(&state.0.content_root).map_err(|e| {
            tracing::error!(error = %e, "team.yaml can't be read");
            message(
                StatusCode::SERVICE_UNAVAILABLE,
                None,
                "Can't check who you are",
                "The team list can't be read. Tell a platform owner.",
            )
        })?;
        match auth::viewer(&state.0.identity, &parts.headers, &team) {
            Ok(viewer) => Ok(Self { viewer, team }),
            Err(AuthError::NotFromProxy) => Err(not_from_proxy()),
            Err(AuthError::NoUser) => Err(message(
                StatusCode::UNAUTHORIZED,
                None,
                "Not signed in",
                "Open the console through the PM OS sign-in.",
            )),
            Err(AuthError::NotMember(who)) => {
                tracing::info!(user = %who, "refused: not in team.yaml");
                Err(message(
                    StatusCode::FORBIDDEN,
                    None,
                    "Not on the team",
                    &format!(
                        "{who} isn't in team.yaml, so the console can't show you anything. \
                         Ask a platform owner to add you."
                    ),
                ))
            }
        }
    }
}

/// Ids from URLs before they go upstream: letters, digits, `_` and `-`.
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 80
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn list(items: &[String]) -> String {
    if items.is_empty() {
        return "<span class=\"muted\">none</span>".into();
    }
    items
        .iter()
        .map(|i| format!("<code>{}</code>", Esc(i)))
        .collect::<Vec<_>>()
        .join(", ")
}

// Bots.

async fn bots(State(state): State<AppState>, Signed { viewer, .. }: Signed) -> Response {
    let mut roster: Vec<BotSummary> = match state.0.bots.bots(&viewer.handle).await {
        Ok(b) => b,
        Err(e) => return upstream_failed(&viewer, Screen::Bots, e),
    };
    roster.retain(|b| viewer.sees_bot(&b.scope, &b.owner));
    roster.sort_by(|a, b| (a.scope != "team", &a.name).cmp(&(b.scope != "team", &b.name)));
    let details = join_all(
        roster
            .iter()
            .map(|b| state.0.bots.bot(&viewer.handle, &b.name)),
    )
    .await;

    let mut body = String::from(
        "<p class=\"muted\">Each bot's job, what it may read, and what happens when it \
         wants to write. Change a bot by asking it in Slack, or by pull request.</p>",
    );
    if roster.is_empty() {
        body.push_str(&empty("No bots yet."));
    } else {
        body.push_str("<div class=\"cards\">");
        for (bot, detail) in roster.iter().zip(details) {
            body.push_str(&bot_card(bot, detail));
        }
        body.push_str("</div>");
    }
    html_response(
        StatusCode::OK,
        page("Bots", Some(Screen::Bots), Some(&viewer), &body),
    )
}

fn bot_card(bot: &BotSummary, detail: Result<BotDetail, UpstreamError>) -> String {
    let mut out = format!(
        "<section class=\"card\" id=\"bot-{name}\"><h3>{name} {scope}</h3>",
        name = Esc(&bot.name),
        scope = badge(&bot.scope),
    );
    if !bot.description.is_empty() {
        let _ = write!(out, "<p class=\"muted\">{}</p>", Esc(&bot.description));
    }
    let _ = write!(
        out,
        "<dl><dt>Role</dt><dd>{}</dd><dt>Owner</dt><dd>{}</dd>",
        Esc(&bot.role),
        Esc(&bot.owner)
    );
    match detail {
        Ok(d) => {
            let reads = match (&d.reads, bot.scope.as_str()) {
                (Some(r), "team") => format!(
                    "<ul class=\"plain\"><li>Jira projects: {}</li><li>Confluence spaces: {}</li>\
                     <li>Slack channels: {}</li></ul>",
                    list(&r.jira_projects),
                    list(&r.confluence_spaces),
                    list(&r.slack_channels)
                ),
                (_, "personal") => "As its owner, with their linked accounts".into(),
                _ => "<span class=\"muted\">not listed</span>".into(),
            };
            let _ = write!(
                out,
                "<dt>Reads</dt><dd>{reads}</dd><dt>Tools</dt><dd>{}</dd><dt>Writes</dt><dd>",
                list(&d.tools)
            );
            if d.actions.is_empty() {
                out.push_str("<span class=\"muted\">no write tools</span>");
            } else {
                out.push_str("<ul class=\"plain\">");
                for (tool, setting) in &d.actions {
                    let _ = write!(
                        out,
                        "<li><code>{}</code> {}</li>",
                        Esc(tool),
                        badge(setting)
                    );
                }
                out.push_str("</ul>");
            }
            out.push_str("</dd>");
        }
        Err(e) => {
            tracing::warn!(bot = %bot.name, error = ?e, "bot details unavailable");
            out.push_str("<dt>Details</dt><dd class=\"muted\">not available right now</dd>");
        }
    }
    out.push_str("</dl></section>");
    out
}

// Routines.

#[derive(Deserialize)]
struct RoutinesQuery {
    #[serde(default)]
    done: String,
}

async fn routines(
    State(state): State<AppState>,
    Signed { viewer, .. }: Signed,
    Query(q): Query<RoutinesQuery>,
) -> Response {
    let mut items = match visible_routines(&state, &viewer).await {
        Ok(r) => r,
        Err(e) => return upstream_failed(&viewer, Screen::Routines, e),
    };
    items.sort_by(|a, b| {
        (!a.enabled, a.next_run_at.as_deref().unwrap_or("~"))
            .cmp(&(!b.enabled, b.next_run_at.as_deref().unwrap_or("~")))
    });
    let mut body = String::new();
    let notice = match q.done.as_str() {
        "pause" => Some("Routine paused."),
        "resume" => Some("Routine resumed."),
        "delete" => Some("Routine deleted."),
        _ => None,
    };
    if let Some(n) = notice {
        let _ = write!(body, "<p class=\"notice\" role=\"status\">{n}</p>");
    }
    body.push_str(
        "<p class=\"muted\">Create or change a routine by asking in Slack (\"every weekday \
         at 8am, …\"). Here you can pause, resume or delete one.</p>",
    );
    if items.is_empty() {
        body.push_str(&empty("No routines."));
    } else {
        let token = state.0.csrf.token(&viewer.handle);
        let rows: Vec<Vec<String>> = items.iter().map(|r| routine_row(r, &token)).collect();
        body.push_str(&table(
            &[
                "Routine", "Bot", "Owner", "Schedule", "Next run", "Last run", "Status", "",
            ],
            &rows,
        ));
    }
    html_response(
        StatusCode::OK,
        page("Routines", Some(Screen::Routines), Some(&viewer), &body),
    )
}

/// The routines this person may see and act on: their own, or everyone's for an admin.
async fn visible_routines(
    state: &AppState,
    viewer: &Viewer,
) -> Result<Vec<Routine>, UpstreamError> {
    let owner = (!viewer.admin).then_some(viewer.handle.as_str());
    let mut items = state.0.bots.routines(&viewer.handle, owner).await?;
    items.retain(|r| viewer.sees_own(&r.owner));
    Ok(items)
}

fn action_form(id: &str, action: &str, token: &str, label: &str, class: &str) -> String {
    format!(
        "<form class=\"inline\" method=\"post\" action=\"/routines/{}/{action}\">\
         <input type=\"hidden\" name=\"csrf\" value=\"{}\">\
         <button type=\"submit\"{class}>{label}</button></form>",
        Esc(id),
        Esc(token),
    )
}

fn routine_row(r: &Routine, token: &str) -> Vec<String> {
    let toggle = if r.enabled {
        action_form(&r.id, "pause", token, "Pause", "")
    } else {
        action_form(&r.id, "resume", token, "Resume", "")
    };
    let delete = format!(
        "<details><summary>Delete</summary>{}</details>",
        action_form(
            &r.id,
            "delete",
            token,
            "Delete for good",
            " class=\"danger\""
        )
    );
    let prompt = if r.hidden {
        HIDDEN_TEXT.to_string()
    } else {
        esc(&clip(&r.prompt, 160))
    };
    vec![
        format!(
            "{prompt}<br><span class=\"muted\"><code>{}</code></span>",
            Esc(&r.id)
        ),
        esc(&r.bot),
        esc(&r.owner),
        format!(
            "<code>{}</code> {}",
            Esc(&r.schedule.cron),
            Esc(&r.schedule.tz)
        ),
        if r.enabled {
            when(r.next_run_at.as_deref().unwrap_or(""))
        } else {
            "<span class=\"muted\">paused</span>".into()
        },
        when(r.last_run_at.as_deref().unwrap_or("")),
        badge(if r.enabled { "enabled" } else { "paused" }),
        format!("<div class=\"actions\">{toggle}{delete}</div>"),
    ]
}

#[derive(Deserialize)]
struct ActionForm {
    #[serde(default)]
    csrf: String,
}

async fn routine_action(
    State(state): State<AppState>,
    Signed { viewer, .. }: Signed,
    headers: HeaderMap,
    Path((id, action)): Path<(String, String)>,
    form: Result<Form<ActionForm>, axum::extract::rejection::FormRejection>,
) -> Response {
    if !auth::same_origin(&headers, state.0.public_url.as_deref()) {
        tracing::warn!(user = %viewer.handle, "refused a form from another origin");
        return message(
            StatusCode::FORBIDDEN,
            Some(&viewer),
            "Refused",
            "That form didn't come from the console.",
        );
    }
    let token = form.map(|Form(f)| f.csrf).unwrap_or_default();
    if !state.0.csrf.verify(&viewer.handle, &token) {
        tracing::warn!(user = %viewer.handle, "refused a form with a bad token");
        return message(
            StatusCode::FORBIDDEN,
            Some(&viewer),
            "Refused",
            "The form has expired. Go back, reload the page and try again.",
        );
    }
    if !valid_id(&id) || !matches!(action.as_str(), "pause" | "resume" | "delete") {
        return message(
            StatusCode::NOT_FOUND,
            Some(&viewer),
            "Not found",
            "There's no such routine action.",
        );
    }
    // The console's runtime token may change any routine, so check this person may.
    match visible_routines(&state, &viewer).await {
        Ok(items) if items.iter().any(|r| r.id == id) => {}
        Ok(_) => {
            return message(
                StatusCode::NOT_FOUND,
                Some(&viewer),
                "Not found",
                "There's no such routine, or it isn't yours.",
            )
        }
        Err(e) => return upstream_failed(&viewer, Screen::Routines, e),
    }
    let done = match action.as_str() {
        "pause" => {
            state
                .0
                .bots
                .set_routine_enabled(&viewer.handle, &id, false)
                .await
        }
        "resume" => {
            state
                .0
                .bots
                .set_routine_enabled(&viewer.handle, &id, true)
                .await
        }
        _ => state.0.bots.delete_routine(&viewer.handle, &id).await,
    };
    match done {
        Ok(()) => {
            tracing::info!(user = %viewer.handle, routine = %id, action = %action, "routine changed");
            Redirect::to(&format!("/routines?done={action}")).into_response()
        }
        Err(UpstreamError::NotFound) => message(
            StatusCode::NOT_FOUND,
            Some(&viewer),
            "Not found",
            "That routine is gone.",
        ),
        Err(e) => upstream_failed(&viewer, Screen::Routines, e),
    }
}

// Approvals.

async fn approvals(State(state): State<AppState>, Signed { viewer, team }: Signed) -> Response {
    let users: Vec<String> = if viewer.admin {
        team.members.iter().map(|m| m.handle.clone()).collect()
    } else {
        vec![viewer.handle.clone()]
    };
    let mut items = Vec::new();
    // Inboxes the tool server cut at its safety cap: (PM, how many are pending in all).
    let mut cut: Vec<(String, usize)> = Vec::new();
    for (user, result) in users.iter().zip(
        join_all(
            users
                .iter()
                .map(|u| state.0.toolserver.pending_approvals(u)),
        )
        .await,
    ) {
        match result {
            Ok(mut page) => {
                if page.truncated {
                    cut.push((user.clone(), page.total_pending));
                }
                items.append(&mut page.items);
            }
            Err(e) => return upstream_failed(&viewer, Screen::Approvals, e),
        }
    }
    cut.retain(|(u, _)| viewer.sees_own(u));
    items.retain(|a| viewer.sees_own(&a.user));
    items.sort_by(|a, b| b.created_at.cmp(&a.created_at));

    let slack = match &state.0.slack_url {
        Some(url) => format!(" <a href=\"{}\">Open Slack</a>.", Esc(url)),
        None => String::new(),
    };
    let mut body = format!(
        "<p class=\"note\">Approvals are decided in Slack: each one has a card with \
         <strong>Approve</strong>, <strong>Edit</strong> and <strong>Reject</strong> in its \
         task's thread, and only its own PM can decide it. This queue is read-only.{slack}</p>"
    );
    for (user, total) in &cut {
        let _ = write!(
            body,
            "<p class=\"error\" role=\"status\">{}'s inbox has {total} pending drafts; the tool \
             server listed only the newest. The rest are still pending.</p>",
            Esc(user)
        );
    }
    if items.is_empty() {
        body.push_str(&empty("Nothing is waiting for approval."));
    } else {
        let rows: Vec<Vec<String>> = items
            .iter()
            .map(|a| {
                // As it will be written: without the draft's source markers (`[[src-…]]`).
                let args = serde_json::to_string(&pmos_contract::cite::strip_json(&a.args))
                    .unwrap_or_default();
                let task = if valid_id(&a.run_id) {
                    format!("<a href=\"/tasks/{id}\">{id}</a>", id = Esc(&a.run_id))
                } else {
                    esc(&a.run_id)
                };
                vec![
                    when(&a.created_at),
                    esc(&a.user),
                    format!("<code>{}</code>", Esc(&a.tool)),
                    esc(a.bot.as_deref().or(a.skill.as_deref()).unwrap_or("")),
                    format!("<code>{}</code>", Esc(&clip(&args, 240))),
                    task,
                    badge(&a.status),
                ]
            })
            .collect();
        body.push_str(&table(
            &["Asked", "PM", "Tool", "Bot", "What", "Task", "Status"],
            &rows,
        ));
    }
    html_response(
        StatusCode::OK,
        page("Approvals", Some(Screen::Approvals), Some(&viewer), &body),
    )
}

// Activity.

async fn activity(State(state): State<AppState>, Signed { viewer, .. }: Signed) -> Response {
    let who = (!viewer.admin).then_some(viewer.handle.as_str());
    let mut tasks = match state.0.bots.tasks(&viewer.handle, who, RECENT_TASKS).await {
        Ok(t) => t,
        Err(e) => return upstream_failed(&viewer, Screen::Activity, e),
    };
    tasks.retain(|t| viewer.sees_own(&t.requested_by));
    let mut body = String::from(
        "<p class=\"muted\">Recent tasks, newest first. Open one for its progress, its full \
         trace and its audit events.</p>",
    );
    if tasks.is_empty() {
        body.push_str(&empty("No tasks yet."));
    } else {
        let rows: Vec<Vec<String>> = tasks
            .iter()
            .map(|t| {
                // Someone else's task comes as metadata only: its ask isn't shown here, and
                // its page answers only its own PM or an admin.
                let ask = if t.hidden {
                    HIDDEN_TEXT.to_string()
                } else {
                    esc(&clip(&t.text, 140))
                };
                vec![
                    when(&t.created_at),
                    esc(&t.bot),
                    esc(&t.requested_by),
                    esc(&t.source),
                    badge(&t.status),
                    if valid_id(&t.id) {
                        format!("<a href=\"/tasks/{}\">{ask}</a>", Esc(&t.id))
                    } else {
                        ask
                    },
                ]
            })
            .collect();
        body.push_str(&table(
            &["When", "Bot", "Asked by", "From", "Status", "Ask"],
            &rows,
        ));
    }
    html_response(
        StatusCode::OK,
        page("Activity", Some(Screen::Activity), Some(&viewer), &body),
    )
}

fn slack_thread(slack_url: &str, task: &Task) -> Option<String> {
    let s = task.slack.as_ref()?;
    let ok_channel = !s.channel.is_empty() && s.channel.chars().all(|c| c.is_ascii_alphanumeric());
    let ok_ts =
        !s.thread_ts.is_empty() && s.thread_ts.chars().all(|c| c.is_ascii_digit() || c == '.');
    (ok_channel && ok_ts).then(|| {
        format!(
            "{slack_url}/archives/{}/p{}",
            s.channel,
            s.thread_ts.replace('.', "")
        )
    })
}

async fn task(
    State(state): State<AppState>,
    Signed { viewer, .. }: Signed,
    Path(id): Path<String>,
) -> Response {
    let not_found = || {
        message(
            StatusCode::NOT_FOUND,
            Some(&viewer),
            "Not found",
            "There's no such task, or it isn't yours.",
        )
    };
    if !valid_id(&id) {
        return not_found();
    }
    let task = match state.0.bots.task(&viewer.handle, &id).await {
        Ok(t) if viewer.sees_own(&t.requested_by) => t,
        Ok(_) | Err(UpstreamError::NotFound) => return not_found(),
        Err(e) => return upstream_failed(&viewer, Screen::Activity, e),
    };
    let audit = async {
        match &state.0.audit_log {
            Some(path) => Some(audit::events_for_run(path.clone(), task.id.clone()).await),
            None => None,
        }
    };
    let (events, trace, audit) = tokio::join!(
        state.0.bots.task_events(&viewer.handle, &task.id),
        state.0.bots.task_trace(&viewer.handle, &task.id),
        audit
    );

    let mut body = String::from("<p><a href=\"/activity\">← Activity</a></p>");
    let _ = write!(
        body,
        "<dl><dt>Status</dt><dd>{}</dd><dt>Bot</dt><dd>{}</dd><dt>Asked by</dt><dd>{}</dd>\
         <dt>From</dt><dd>{}</dd><dt>Priority</dt><dd>{}</dd><dt>Attempts</dt><dd>{}</dd>\
         <dt>Created</dt><dd>{}</dd><dt>Updated</dt><dd>{}</dd><dt>Task id</dt><dd><code>{}</code></dd>",
        badge(&task.status),
        Esc(&task.bot),
        Esc(&task.requested_by),
        Esc(&task.source),
        Esc(&task.priority),
        task.attempts,
        when(&task.created_at),
        when(&task.updated_at),
        Esc(&task.id),
    );
    if let Some(r) = &task.routine_id {
        let _ = write!(body, "<dt>Routine</dt><dd><code>{}</code></dd>", Esc(r));
    }
    if !task.approvals.is_empty() {
        let _ = write!(body, "<dt>Approvals</dt><dd>{}</dd>", list(&task.approvals));
    }
    if let Some(link) = state
        .0
        .slack_url
        .as_deref()
        .and_then(|u| slack_thread(u, &task))
    {
        let _ = write!(
            body,
            "<dt>Slack</dt><dd><a href=\"{}\">Open the thread</a> (approval cards are there)</dd>",
            Esc(&link)
        );
    }
    body.push_str("</dl>");

    let _ = write!(body, "<h2>Ask</h2><pre>{}</pre>", Esc(&task.text));
    if !task.checklist.is_empty() {
        body.push_str("<h2>Checklist</h2><ul class=\"plain\">");
        for c in &task.checklist {
            let _ = write!(
                body,
                "<li>{} {}</li>",
                if c.done { "✓" } else { "○" },
                Esc(&c.item)
            );
        }
        body.push_str("</ul>");
    }
    if let Some(result) = &task.result {
        let result = pmos_contract::cite::strip(result);
        let _ = write!(body, "<h2>Result</h2><pre>{}</pre>", Esc(&result));
    }
    if let Some(error) = &task.error {
        let _ = write!(
            body,
            "<h2>Why it failed</h2><p class=\"error\">{}</p>",
            Esc(error)
        );
    }

    body.push_str("<h2>Events</h2>");
    match events {
        Ok(ev) if ev.is_empty() => body.push_str(&empty("No events.")),
        Ok(ev) => {
            let rows: Vec<Vec<String>> = ev
                .iter()
                .map(|e| {
                    let mut text = esc(e.text.as_deref().unwrap_or(""));
                    if let Some(items) = &e.checklist {
                        let done = items.iter().filter(|c| c.done).count();
                        let _ = write!(
                            text,
                            " <span class=\"muted\">({done}/{} done)</span>",
                            items.len()
                        );
                    }
                    vec![when(&e.at), esc(&e.kind), text]
                })
                .collect();
            body.push_str(&table(&["At", "Kind", "Text"], &rows));
        }
        Err(e) => body.push_str(&section_error("events", e)),
    }

    body.push_str(
        "<h2>Trace</h2><p class=\"muted\">Every prompt, model reply, tool call and result, \
         kept 30 days. Slack text is removed after 24 hours.</p>",
    );
    match trace {
        Ok(tr) if tr.is_empty() => body.push_str(&empty("No trace entries.")),
        Ok(tr) => {
            let rows: Vec<Vec<String>> = tr
                .iter()
                .map(|t| {
                    vec![
                        when(&t.at),
                        badge(&t.kind),
                        format!("<pre>{}</pre>", Esc(&html::json_text(&t.body))),
                    ]
                })
                .collect();
            body.push_str(&table(&["At", "Kind", "Body"], &rows));
        }
        Err(e) => body.push_str(&section_error("trace", e)),
    }

    body.push_str("<h2>Audit events</h2>");
    match audit {
        None => body.push_str(&empty(
            "Not available here: the console isn't given the tool server's audit log \
             (PMOS_CONSOLE_AUDIT_LOG).",
        )),
        Some(Err(e)) => {
            tracing::warn!(error = %e, "audit log can't be read");
            body.push_str("<p class=\"error\">The audit log can't be read right now.</p>");
        }
        Some(Ok((ev, _))) if ev.is_empty() => {
            body.push_str(&empty("No audit events for this task."))
        }
        Some(Ok((ev, more))) => {
            let rows: Vec<Vec<String>> = ev
                .iter()
                .map(|a| {
                    vec![
                        when(&a.at),
                        format!("<code>{}</code>", Esc(&a.tool)),
                        badge(&a.outcome),
                        esc(&a.user),
                        esc(&a.client),
                        list(&a.sources),
                        a.output_ref.as_deref().map(esc).unwrap_or_default(),
                        a.detail.as_deref().map(esc).unwrap_or_default(),
                    ]
                })
                .collect();
            body.push_str(&table(
                &[
                    "At", "Tool", "Outcome", "As", "Client", "Read", "Wrote", "Detail",
                ],
                &rows,
            ));
            if more {
                let _ = write!(
                    body,
                    "<p class=\"muted\">Showing the first {} events.</p>",
                    audit::MAX_EVENTS
                );
            }
        }
    }

    html_response(
        StatusCode::OK,
        page(
            &format!("Task: {}", clip(&task.text, 60)),
            Some(Screen::Activity),
            Some(&viewer),
            &body,
        ),
    )
}

fn section_error(what: &str, e: UpstreamError) -> String {
    tracing::warn!(what, error = ?e, "task section unavailable");
    format!(
        "<p class=\"error\">The task's {} can't be read right now.</p>",
        Esc(what)
    )
}
