//! `pmos-toolserver users …`: the host user commands (spec §5), run by `pmos-admin users …`
//! in a one-off tool server container with the database's own access.
//!
//! Every command needs `PMOS_HOST_USER`, the OS user who ran it (`pmos-admin` passes
//! `${SUDO_USER:-$(id -un)}`), matching `^[a-z_][a-z0-9_.-]{0,31}$`; without it, or with
//! another value, nothing runs (exit 2). Writes record it in `role_changes.host_user` and in
//! the audit event's detail. It is what the host says, not a proof: the host is already
//! trusted with every secret. Nothing here prints a token; `list`, `export` and the import's
//! dry run show addresses, which the host already holds.
//!
//! Exit codes: 0 done; 1 failed (database, file); 2 usage (arguments, `PMOS_HOST_USER`,
//! `PMOS_DATABASE_URL`, `PMOS_ENTRA_TENANT_ID`); 3 refused by the rules (an admin exists
//! for `bootstrap-admin`, a non-empty table for `import --bootstrap`, the last admin, …).

use std::io::{BufRead, IsTerminal, Write};
use std::path::PathBuf;

use chrono::{DateTime, NaiveDate, Utc};
use clap::{Parser, Subcommand, ValueEnum};

use super::import::{export_team_yaml, ImportMode, ImportRequest};
use super::{
    detail, Actor, AdminEntra, EntraKey, Field, HostUser, NewUser, Op, UserError, UserStore,
};
use crate::pg::PgStore;

pub const DONE: i32 = 0;
pub const FAILED: i32 = 1;
pub const USAGE: i32 = 2;
pub const REFUSED: i32 = 3;

#[derive(Parser, Debug)]
#[command(
    name = "pmos-toolserver users",
    about = "PM OS user database: host commands (docs/runbooks/users.md)"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// The first admin: refused once any active admin exists.
    BootstrapAdmin {
        #[arg(long)]
        handle: String,
        /// The Entra sign-in name (UPN).
        #[arg(long)]
        sign_in_name: String,
        #[arg(long)]
        atlassian_email: String,
        #[arg(long)]
        slack_id: String,
        /// Their Entra object id (Entra admin center: Users, the user, Object ID).
        #[arg(
            long,
            required_unless_present = "no_entra",
            conflicts_with = "no_entra"
        )]
        oid: Option<String>,
        /// No Entra link (CI, or an install without Entra sign-in).
        #[arg(long)]
        no_entra: bool,
        /// Display name.
        #[arg(long, default_value = "")]
        name: String,
        #[arg(long)]
        github: Option<String>,
    },
    /// Grant admin (the only way to make an admin).
    GrantAdmin {
        handle: String,
        #[arg(
            long,
            required_unless_present = "keep_entra",
            conflicts_with = "keep_entra"
        )]
        oid: Option<String>,
        /// Keep the existing Entra link, which must be host-confirmed.
        #[arg(long)]
        keep_entra: bool,
    },
    /// Revoke admin (break glass when the web app is down).
    RevokeAdmin {
        handle: String,
    },
    /// Set or replace a host-confirmed Entra link.
    SetEntra {
        handle: String,
        #[arg(long)]
        oid: String,
    },
    /// Remove an Entra link.
    UnsetEntra {
        handle: String,
    },
    SetSignInName {
        handle: String,
        name: String,
    },
    SetAtlassianEmail {
        handle: String,
        email: String,
    },
    /// Not capped. Revokes admin, which reactivation never restores.
    Deactivate {
        handle: String,
    },
    Reactivate {
        handle: String,
    },
    /// The pilot exception: one person may hold Admin and Auditor.
    PilotException {
        #[command(subcommand)]
        which: ExceptionName,
    },
    /// Import an old team.yaml from the path the operator names (normally a reviewed
    /// commit): a dry run unless --apply.
    Import {
        #[arg(long)]
        from: PathBuf,
        /// The commit the file was read at (recorded with the file's SHA-256).
        #[arg(long)]
        commit: Option<String>,
        /// Also import the file's admins, after a confirmation on the terminal.
        #[arg(long, conflicts_with = "bootstrap")]
        allow_admins: bool,
        /// An empty users table only: everything, admins included, in one transaction.
        #[arg(long)]
        bootstrap: bool,
        #[arg(long)]
        apply: bool,
    },
    /// The database as a team.yaml, for a rollback.
    Export {
        #[arg(long, value_enum)]
        format: ExportFormat,
    },
    /// Every user: handle, status, roles, flags.
    List,
    /// A one-time password setup (or reset) code for a user, shown once
    /// (docs/password-sign-in.md). The only way to issue one for an Admin or Auditor.
    PasswordSetup {
        handle: String,
    },
}

#[derive(Subcommand, Debug)]
enum ExceptionName {
    AdminAndAuditor {
        #[command(subcommand)]
        action: ExceptionAction,
    },
}

#[derive(Subcommand, Debug)]
enum ExceptionAction {
    /// On until a date (YYYY-MM-DD, at 00:00 UTC) at most 90 days ahead.
    On {
        #[arg(long)]
        until: String,
    },
    /// Off; refused while anyone holds both, unless --revoke-auditor.
    Off {
        #[arg(long)]
        revoke_auditor: bool,
    },
    Status,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ExportFormat {
    TeamYaml,
}

/// The process environment and terminal, for [`run`].
pub struct Io<'a> {
    pub env: &'a dyn Fn(&str) -> Option<String>,
    pub out: &'a mut dyn Write,
    pub err: &'a mut dyn Write,
    /// Ask on the terminal; `None` when there is none.
    pub ask: &'a mut dyn FnMut(&str) -> Option<String>,
}

/// `pmos-toolserver users <args>`, from the real process.
pub async fn main(args: Vec<String>) -> i32 {
    let env = |k: &str| std::env::var(k).ok();
    let mut out = std::io::stdout();
    let mut err = std::io::stderr();
    let mut ask = |prompt: &str| -> Option<String> {
        if !std::io::stdin().is_terminal() {
            return None;
        }
        eprint!("{prompt}");
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line).ok()?;
        Some(line.trim().to_string())
    };
    run(
        &args,
        Io {
            env: &env,
            out: &mut out,
            err: &mut err,
            ask: &mut ask,
        },
    )
    .await
}

macro_rules! say {
    ($w:expr, $($arg:tt)*) => {{
        let _ = writeln!($w, $($arg)*);
    }};
}

/// `--until`: a date (00:00 UTC that day) or an RFC 3339 time.
fn parse_until(s: &str) -> Option<DateTime<Utc>> {
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Some(d.and_hms_opt(0, 0, 0)?.and_utc());
    }
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// What one command does, once its arguments are checked.
enum Action {
    Write(Actor, Box<Op>),
    Import {
        req: Box<ImportRequest>,
        apply: bool,
    },
    List,
    Export,
    ExceptionStatus,
    PasswordSetup(String),
}

impl Action {
    fn write(actor: Actor, op: Op) -> Self {
        Self::Write(actor, Box::new(op))
    }
}

/// Run one host command; returns the exit code.
pub async fn run(args: &[String], io: Io<'_>) -> i32 {
    let Io { env, out, err, ask } = io;
    let cli = match Cli::try_parse_from(
        std::iter::once("pmos-toolserver users".to_string()).chain(args.iter().cloned()),
    ) {
        Ok(cli) => cli,
        Err(e) => {
            use clap::error::ErrorKind;
            let help = matches!(e.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion);
            if help {
                say!(out, "{e}");
                return DONE;
            }
            say!(err, "{e}");
            return USAGE;
        }
    };
    let host_user = match env("PMOS_HOST_USER").filter(|s| !s.is_empty()) {
        None => {
            say!(
                err,
                "PMOS_HOST_USER is required: run this through `deploy/pmos-admin users …`, which passes the OS user who ran it"
            );
            return USAGE;
        }
        Some(raw) => match HostUser::parse(&raw) {
            Some(h) => h,
            None => {
                say!(
                    err,
                    "PMOS_HOST_USER must be an OS user name (^[a-z_][a-z0-9_.-]{{0,31}}$)"
                );
                return USAGE;
            }
        },
    };
    let Some(database_url) = env("PMOS_DATABASE_URL").filter(|s| !s.is_empty()) else {
        say!(
            err,
            "users commands need PMOS_DATABASE_URL: the in-memory development store is filled from PMOS_USERS_SEED instead"
        );
        return USAGE;
    };
    let tenant = || -> Result<String, String> {
        let raw = env("PMOS_ENTRA_TENANT_ID").filter(|s| !s.is_empty()).ok_or(
            "--oid needs PMOS_ENTRA_TENANT_ID (the web app's PMOS_WEB_ENTRA_TENANT_ID in deploy/.env)",
        )?;
        super::rules::guid(&raw, "tenant id (PMOS_ENTRA_TENANT_ID)").map_err(|e| e.to_string())
    };
    let entra = |oid: &str| -> Result<EntraKey, String> {
        Ok(EntraKey {
            tid: tenant()?,
            oid: oid.to_string(),
        })
    };
    let host = Actor::host(host_user.clone());
    let action = match cli.command {
        Command::BootstrapAdmin {
            handle,
            sign_in_name,
            atlassian_email,
            slack_id,
            oid,
            no_entra: _,
            name,
            github,
        } => {
            let key = match oid.as_deref().map(entra).transpose() {
                Ok(k) => k,
                Err(m) => {
                    say!(err, "{m}");
                    return USAGE;
                }
            };
            Action::write(
                host,
                Op::BootstrapAdmin {
                    user: NewUser {
                        handle,
                        display_name: name,
                        sign_in_name: Some(sign_in_name),
                        atlassian_email: Some(atlassian_email),
                        github_login: github,
                        slack_id: Some(slack_id),
                        roles: Default::default(),
                    },
                    entra: key,
                },
            )
        }
        Command::GrantAdmin {
            handle,
            oid,
            keep_entra: _,
        } => {
            let choice = match oid {
                Some(oid) => match entra(&oid) {
                    Ok(k) => AdminEntra::Oid(k),
                    Err(m) => {
                        say!(err, "{m}");
                        return USAGE;
                    }
                },
                None => AdminEntra::Keep,
            };
            Action::write(
                host,
                Op::GrantAdmin {
                    handle,
                    entra: choice,
                },
            )
        }
        Command::RevokeAdmin { handle } => Action::write(host, Op::RevokeAdmin { handle }),
        Command::SetEntra { handle, oid } => match entra(&oid) {
            Ok(key) => Action::write(host, Op::SetEntra { handle, key }),
            Err(m) => {
                say!(err, "{m}");
                return USAGE;
            }
        },
        Command::UnsetEntra { handle } => Action::write(host, Op::UnsetEntra { handle }),
        Command::SetSignInName { handle, name } => Action::write(
            host,
            Op::SetField {
                handle,
                field: Field::SignInName,
                value: name,
            },
        ),
        Command::SetAtlassianEmail { handle, email } => Action::write(
            host,
            Op::SetField {
                handle,
                field: Field::AtlassianEmail,
                value: email,
            },
        ),
        Command::Deactivate { handle } => Action::write(host, Op::Deactivate { handle }),
        Command::Reactivate { handle } => Action::write(host, Op::Reactivate { handle }),
        Command::PilotException {
            which: ExceptionName::AdminAndAuditor { action },
        } => match action {
            ExceptionAction::On { until } => match parse_until(&until) {
                Some(until) => Action::write(host, Op::ExceptionOn { until }),
                None => {
                    say!(
                        err,
                        "--until must be a date (YYYY-MM-DD) or an RFC 3339 time"
                    );
                    return USAGE;
                }
            },
            ExceptionAction::Off { revoke_auditor } => {
                Action::write(host, Op::ExceptionOff { revoke_auditor })
            }
            ExceptionAction::Status => Action::ExceptionStatus,
        },
        Command::Import {
            from,
            commit,
            allow_admins,
            bootstrap,
            apply,
        } => {
            let bytes = match std::fs::read(&from) {
                Ok(b) => b,
                Err(e) => {
                    say!(err, "can't read {}: {e}", from.display());
                    return USAGE;
                }
            };
            let mode = if bootstrap {
                ImportMode::Bootstrap
            } else if allow_admins {
                ImportMode::AllowAdmins
            } else {
                ImportMode::Members
            };
            match ImportRequest::parse(&bytes, commit.as_deref(), mode) {
                Ok(req) => Action::Import {
                    req: Box::new(req),
                    apply,
                },
                Err(e) => return refusal(err, &e),
            }
        }
        Command::Export {
            format: ExportFormat::TeamYaml,
        } => Action::Export,
        Command::List => Action::List,
        Command::PasswordSetup { handle } => Action::PasswordSetup(handle),
    };

    let store = match PgStore::connect(&database_url).await {
        Ok(s) => s,
        Err(e) => {
            say!(err, "can't reach the tool server's database: {e}");
            return FAILED;
        }
    };
    if let Err(e) = store.migrate().await {
        say!(err, "applying the tool server's migrations failed: {e}");
        return FAILED;
    }
    let now = Utc::now();
    match action {
        Action::Write(actor, op) => match store.apply(&actor, *op, now).await {
            Ok(applied) => {
                for line in applied.lines {
                    say!(out, "{line}");
                }
                say!(out, "recorded as host user {}", host_user.as_str());
                DONE
            }
            Err(e) => refusal(err, &e),
        },
        Action::Import { req, apply } => {
            let actor = Actor::import(Some(host_user.clone()));
            let admins = req.admins();
            let asks = apply && req.mode == ImportMode::AllowAdmins && !admins.is_empty();
            let op = Op::Import(req);
            match store.preview(&actor, &op, now).await {
                Ok(preview) => {
                    for line in preview.lines {
                        say!(out, "{line}");
                    }
                }
                Err(e) => return refusal(err, &e),
            }
            if !apply {
                say!(out, "dry run: nothing was changed; add --apply to write");
                return DONE;
            }
            if asks {
                let prompt = format!(
                    "The file makes these people admins: {}.\nType ADMINS to import them as admins: ",
                    admins.join(", ")
                );
                match ask(&prompt) {
                    None => {
                        say!(
                            err,
                            "--allow-admins asks for a confirmation on a terminal: run it from an interactive shell (or bootstrap-admin, then import without --allow-admins)"
                        );
                        return USAGE;
                    }
                    Some(answer) if answer == "ADMINS" => {}
                    Some(_) => {
                        say!(out, "stopped; nothing changed");
                        return FAILED;
                    }
                }
            }
            match store.apply(&actor, op, Utc::now()).await {
                Ok(_) => {
                    say!(
                        out,
                        "imported; recorded as host user {}",
                        host_user.as_str()
                    );
                    DONE
                }
                Err(e) => refusal(err, &e),
            }
        }
        Action::List => match store.directory(now).await {
            Ok(dir) => {
                say!(
                    out,
                    "{:<24} {:<12} {:<26} flags",
                    "handle",
                    "status",
                    "roles"
                );
                for u in dir.users.values() {
                    let mut flags = vec![format!("entra:{}", u.entra_state())];
                    if u.sign_in_name.is_some() && u.sign_in_name_confirmed_at.is_none() {
                        flags.push("sign_in_unconfirmed".into());
                    }
                    if u.dual_role() {
                        flags.push("dual_role".into());
                    }
                    if u.identity(super::IdentityKind::Slack).is_some() {
                        flags.push("slack".into());
                    }
                    // U3 (review at c78b5fb): spelled as another user's handle.
                    let clash = dir.spelling_clash_of(&u.handle);
                    if !clash.is_empty() {
                        flags.push(format!("spelling_clash:{}", clash.join(",")));
                    }
                    say!(
                        out,
                        "{:<24} {:<12} {:<26} {}",
                        u.handle,
                        u.status.as_str(),
                        super::role_list(&u.role_set()),
                        flags.join(" ")
                    );
                }
                say!(
                    out,
                    "{} users (directory version {})",
                    dir.users.len(),
                    dir.version
                );
                for w in dir.spelling_clash_warnings() {
                    say!(err, "warning: {w}");
                }
                DONE
            }
            Err(e) => {
                say!(err, "{e}");
                FAILED
            }
        },
        Action::Export => match store.directory(now).await {
            Ok(dir) => {
                let (yaml, warnings) = export_team_yaml(&dir);
                for w in warnings {
                    say!(err, "warning: {w}");
                }
                let _ = write!(out, "{yaml}");
                DONE
            }
            Err(e) => {
                say!(err, "{e}");
                FAILED
            }
        },
        Action::PasswordSetup(handle) => {
            let actor = Actor::host(host_user.clone());
            match crate::password::issue_setup_code(store.passwords(), &actor, &handle, now).await {
                Ok(issued) => {
                    say!(
                        out,
                        "{} code for `{}` (shown once; it works once, until {}):",
                        issued.kind.as_str(),
                        issued.handle,
                        issued.expires_at.format("%Y-%m-%d %H:%M UTC")
                    );
                    say!(out, "{}", issued.code.expose());
                    say!(
                        out,
                        "give it to them yourself; they open the web app's set-up page with their sign-in name and this code"
                    );
                    say!(out, "recorded as host user {}", host_user.as_str());
                    DONE
                }
                Err(e @ crate::password::PwError::Unavailable(_)) => {
                    say!(err, "{}", e.message());
                    FAILED
                }
                Err(e) => {
                    say!(err, "refused ({}): {}", e.code(), e.message());
                    REFUSED
                }
            }
        }
        Action::ExceptionStatus => match store.directory(now).await {
            Ok(dir) => {
                match &dir.exception {
                    None => say!(out, "pilot exception admin-and-auditor: off"),
                    Some(e) => say!(
                        out,
                        "pilot exception admin-and-auditor: {} until {} (switched on by {} on {})",
                        if e.in_force(now) { "on" } else { "LAPSED" },
                        e.expires_at.format("%Y-%m-%d %H:%M UTC"),
                        e.enabled_by,
                        detail::date(e.enabled_at)
                    ),
                }
                let holders: Vec<&str> = dir
                    .dual_role_holders()
                    .iter()
                    .map(|u| u.handle.as_str())
                    .collect();
                if holders.is_empty() {
                    say!(out, "nobody holds admin and auditor");
                } else {
                    say!(out, "holding admin and auditor: {}", holders.join(", "));
                }
                DONE
            }
            Err(e) => {
                say!(err, "{e}");
                FAILED
            }
        },
    }
}

fn refusal(err: &mut dyn Write, e: &UserError) -> i32 {
    match e {
        UserError::Refused { refusal, message } => {
            say!(err, "refused ({}): {message}", refusal.code());
            REFUSED
        }
        UserError::Store(s) => {
            say!(err, "{s}");
            FAILED
        }
    }
}
