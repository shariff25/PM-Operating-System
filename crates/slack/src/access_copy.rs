//! The words the Slack service uses about who someone is and what they may do, from the user
//! database (`docs/user-database-spec.md` §2 and §6, slice U4b), as data, so the owner can
//! edit them without touching the code. Plain: no jokes about access.
//!
//! **Placeholders:** `{handle}` is a handle, already checked and shown as code.

/// The directory snapshot is too old (or there is none yet) to say who someone is.
pub const CANT_CHECK: &str = "PM OS can't check who you are right now. Try again in a few \
                              minutes.";

/// An admin action on a snapshot older than 30 seconds: nothing was sent.
pub const CANT_CHECK_ADMIN: &str = "PM OS can't check your access right now, so nothing was \
                                    changed. Try again in a minute.";

/// A deactivated person.
pub const DEACTIVATED: &str = "Your PM OS access was turned off. If you think that's a \
                               mistake, ask a platform owner.";

/// Someone in the directory whose roles don't use PM OS in Slack (an Auditor only).
pub const NO_ACCESS: &str = "Your PM OS role doesn't include using PM OS in Slack. Ask a \
                             platform owner if you need it.";

/// What approving grants. The card never offers anything but `pm`: the tool server refuses
/// any other role from the Slack bot.
pub const ROLE_LINE: &str = "Approving adds them to PM OS as a PM and gives them a personal \
                             bot. Other roles are given in the web app; admins only by \
                             someone with host access.";

/// The `remove` confirmation's explanation of what deactivating does.
pub const REMOVE_EXPLAINED: &str = "This deactivates them: their sessions end at once and their \
     linked accounts are suspended. Reactivating them within 24 hours (in the web app) restores \
     their accounts and held drafts; after that the accounts are deleted.";

/// `remove` of someone holding Admin or Auditor (`403 protected_not_from_slack`).
pub const REMOVE_PROTECTED: &str = "{handle} is a platform owner or an auditor, so PM OS won't \
     deactivate them from Slack. Use the web app's Team page, or ask someone with host access.";

/// `remove` past the hourly cap (`429 deactivation_limit`).
pub const REMOVE_LIMIT: &str = "PM OS has stopped deactivations for an hour. Someone with host \
     access can continue with `pmos-admin users deactivate`.";

/// `remove` of oneself (`403 self_removal`).
pub const REMOVE_SELF: &str = "You can't remove yourself; ask another platform owner.";

/// `remove` that would leave no active admin (`409 last_admin`).
pub const REMOVE_LAST_ADMIN: &str = "That would leave PM OS without an active platform owner, so \
                                     nothing was changed.";

/// `remove` of someone already deactivated (`409 already_deactivated`).
pub const REMOVE_ALREADY: &str = "{handle} is already deactivated; nothing to do.";

/// `remove` of someone who still owns a team bot (`409 owns_bots`).
pub const REMOVE_OWNS_BOTS: &str = "{handle} still owns a team bot. Give it another owner \
                                    first (by a pull request to its `BOT.md`).";

/// `remove` of a handle nobody has (`404 not_a_member`).
pub const REMOVE_NOBODY: &str = "{handle} isn't a PM OS member.";

/// `remove` by someone the tool server doesn't count as an admin (`403 not_admin`).
pub const REMOVE_NOT_ADMIN: &str = "Only platform owners can remove members.";
