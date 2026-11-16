//! The words of the new first-time flow (`PMOS_ONBOARDING_V2=1`), as data, so the owner can
//! edit them without touching the code.
//!
//! **Voice:** friendly, and lightly funny in the greeting only. Anything about accounts,
//! permissions or errors is plain. No joke about a person, a deadline or customer data.
//!
//! **Placeholders:** `{name}` is the member's first name (escaped before it is shown) and
//! `{label}` a starter's button label.

/// The two-line welcome. With no usable first name, [`HELLO_NO_NAME`] is used.
pub const HELLO: &str = "Hi {name}, I'm PM OS. I read Jira so you don't have to.\n\
                         Connect it and I'll prove it.";
pub const HELLO_NO_NAME: &str = "Hi, I'm PM OS. I read Jira so you don't have to.\n\
                                 Connect it and I'll prove it.";
/// The one primary button of the welcome.
pub const CONNECT_BUTTON: &str = "Connect Jira and Confluence";
/// Added when PM OS can't make the link right now, in place of the button.
pub const NO_LINK: &str = "I couldn't make the connect link just now. Say `setup` in a minute \
                           and it will be there.";

/// The offer after the account is linked. It claims no number of tickets.
pub const OFFER: &str =
    "Found your account. Want the short version of your week, or shall I start on something?";

/// One starter: the button, and the task it starts.
pub struct Starter {
    pub id: &'static str,
    pub label: &'static str,
    /// The text of the task, as if the PM had typed it in the DM.
    pub task: &'static str,
}

pub const STARTERS: &[Starter] = &[
    Starter {
        id: "week",
        label: "Summarize my week",
        task: "Summarize my week: what moved, what shipped and what is at risk in the Jira work \
               I own or follow. Cite the tickets.",
    },
    Starter {
        id: "status",
        label: "Draft a status update",
        task: "Draft a status update from my Jira work this week. Cite the tickets.",
    },
    Starter {
        id: "blocked",
        label: "What's blocked?",
        task: "What is blocked in my Jira work right now, and by what? Cite the tickets.",
    },
];

/// Replaces the offer once a starter is tapped, so it can't be tapped twice.
pub const STARTED: &str =
    "On it: {label}. The answer will arrive in this thread, with its sources.";

/// The one closing line, after the first completed task.
pub const CLOSING: &str = "That's the basics. There is more under the Skills section of my \
                           Home tab, and `nudges off` stops my occasional nudges.";

pub fn starter(id: &str) -> Option<&'static Starter> {
    STARTERS.iter().find(|s| s.id == id)
}
