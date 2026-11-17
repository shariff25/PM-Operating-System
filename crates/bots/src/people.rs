//! Who may use the runtime's bots: the user directory (`docs/user-database-spec.md` §2,
//! slice U4a), read from the tool server's `GET /internal/directory` with the orchestrator
//! token (`PMOS_SERVICE_TOKEN`) and held by a [`SnapshotClient`]. **Never `team.yaml`.**
//!
//! | Decision | Snapshot | Without one |
//! | --- | --- | --- |
//! | Queue an ask (`POST /v1/tasks`) | ≤ 5 min ([`SnapshotClient::for_queueing_ask`]); the task still needs a tool server session, which fails closed | `503` |
//! | A routine falls due: run it, or pause it if its owner may no longer ask | ≤ 5 min ([`SnapshotClient::for_due_routine`]) | Skip this run (don't pause), logged |
//! | Pause the routines of someone who may no longer ask, when the snapshot shows it | ≤ 30 s ([`SnapshotClient::fresh`]) | Wait for a fresh one |
//! | Parse or create a routine; set a PM's level | ≤ 30 s | `503` |
//! | The console's admin check (another PM's task or routine) | ≤ 30 s | Not an admin: own tasks only |
//!
//! May ask bots ([`may_ask`]): an **active** user whose roles allow `ask-bot`
//! (`pmos_contract::permissions`): `pm`, `reviewer` or `admin`. An Auditor-only person can't
//! ask; a deactivated person's kept roles grant nothing.

use pmos_contract::bots::Bot;
use pmos_contract::permissions::Permission;
pub use pmos_directory::snapshot::{Snapshot, SnapshotClient};

use crate::catalog::Catalog;

/// What a PM reads when the directory can't be checked.
pub const UNAVAILABLE: &str =
    "PM OS can't check who may use the bots right now; try again in a minute";

/// May `handle` ask bots at all? The error says why not, for the PM.
pub fn may_ask_bots(people: &Snapshot, handle: &str) -> Result<(), String> {
    let Some(user) = people.user(handle) else {
        return Err(format!("`{handle}` isn't a PM OS user"));
    };
    if !user.is_active() {
        return Err(format!("`{handle}`'s PM OS account is deactivated"));
    }
    if !user.allows(Permission::AskBot) {
        return Err(format!(
            "`{handle}` has no role that may ask bots (an Auditor sees audit metadata only)"
        ));
    }
    Ok(())
}

/// May `handle` use `bot`: may they ask bots ([`may_ask_bots`]), and does the bot's scope
/// serve them ([`Catalog::may_use`])?
pub fn may_ask(
    people: &Snapshot,
    catalog: &Catalog,
    handle: &str,
    bot: &Bot,
) -> Result<(), String> {
    may_ask_bots(people, handle)?;
    catalog.may_use(handle, bot)
}
