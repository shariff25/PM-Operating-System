//! The first-time flow of a new member (`PMOS_ONBOARDING_V2=1`). Off, none of this runs and
//! the welcome and checklist are exactly as before (`docs/slack-home.md`, "First time").
//!
//! 1. **Hello:** two short lines and one button, **Connect Jira and Confluence**.
//! 2. **Offer:** once the account is linked (seen on the PM's next DM or Home open, by reading
//!    the onboarding state again; nothing polls), one message with three starter buttons. Each
//!    starts a task for the PM's personal bot, as a typed DM would.
//! 3. **Closing:** after the first completed task, one line about the Skills view and
//!    `nudges off`.
//!
//! Each step is shown once. Which steps a PM has seen is kept in `<state dir>/onboarding.json`
//! (written before the message is posted, then renamed into place), so a restart never
//! repeats one. Slack and the memory repo are not asked for here: they are offered when a
//! request first needs them, by the paths that already do that.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::bots::{choose_bot, Bots, Click, Dm};
use crate::cards;
use crate::nudges;
use crate::onboard_copy;

pub const STATE_FILE: &str = "onboarding.json";
pub const FLAG: &str = "PMOS_ONBOARDING_V2";

/// `PMOS_ONBOARDING_V2=1` turns the flow on; anything else leaves it off.
pub fn enabled(value: Option<&str>) -> bool {
    value.map(str::trim) == Some("1")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    Hello,
    Offer,
    Closing,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
struct Steps {
    hello: bool,
    offer: bool,
    closing: bool,
}

impl Steps {
    fn get(&mut self, step: Step) -> &mut bool {
        match step {
            Step::Hello => &mut self.hello,
            Step::Offer => &mut self.offer,
            Step::Closing => &mut self.closing,
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct State {
    pms: BTreeMap<String, Steps>,
}

/// Which steps each PM has seen, and whether the flow is on.
pub(crate) struct Flow {
    on: bool,
    path: PathBuf,
    state: Mutex<State>,
}

impl Flow {
    pub(crate) fn open(state_dir: &Path, on: bool) -> Self {
        let path = state_dir.join(STATE_FILE);
        let state = if on {
            match std::fs::read_to_string(&path) {
                Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                    tracing::error!(error = %e, "onboarding state unreadable; starting empty");
                    State::default()
                }),
                Err(_) => State::default(),
            }
        } else {
            State::default()
        };
        Self {
            on,
            path,
            state: Mutex::new(state),
        }
    }

    pub(crate) fn on(&self) -> bool {
        self.on
    }

    pub(crate) fn seen(&self, handle: &str, step: Step) -> bool {
        let mut state = self.state.lock().expect("onboarding state");
        state
            .pms
            .get_mut(handle)
            .map(|s| *s.get(step))
            .unwrap_or(false)
    }

    /// Marks `step` seen and saves; `false` if it already was (or the flow is off).
    pub(crate) fn claim(&self, handle: &str, step: Step) -> bool {
        if !self.on {
            return false;
        }
        let mut state = self.state.lock().expect("onboarding state");
        let flag = state.pms.entry(handle.to_string()).or_default().get(step);
        if *flag {
            return false;
        }
        *flag = true;
        self.save(&state);
        true
    }

    /// Takes a claim back when the message could not be posted.
    pub(crate) fn release(&self, handle: &str, step: Step) {
        let mut state = self.state.lock().expect("onboarding state");
        if let Some(s) = state.pms.get_mut(handle) {
            *s.get(step) = false;
        }
        self.save(&state);
    }

    fn save(&self, state: &State) {
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = self.path.with_extension("tmp");
        let body = serde_json::to_string_pretty(state).unwrap_or_default();
        if let Err(e) = std::fs::write(&tmp, body).and_then(|_| std::fs::rename(&tmp, &self.path)) {
            tracing::error!(error = %e, "saving the onboarding state failed");
        }
    }
}

impl Bots {
    /// Whether the new first-time flow is on.
    pub(crate) fn onboarding_v2(&self) -> bool {
        self.flow().on()
    }

    /// An onboarding message went to the PM: that day counts as their nudge day.
    fn onboarding_posted(&self, handle: &str) {
        nudges::record_daily_message(handle);
    }

    /// Step 1: the welcome to a newly approved member, in their DM.
    pub(crate) async fn onboarding_hello(&self, channel: &str, name: &str, handle: &str) {
        if !self.flow().claim(handle, Step::Hello) {
            return;
        }
        let url = self
            .toolserver()
            .oauth_start("atlassian", handle)
            .await
            .ok();
        let first = name.split_whitespace().next().unwrap_or_default();
        let message = cards::onboard_hello(first, url.as_deref());
        match self.post(channel, None, None, message).await {
            Some(_) => self.onboarding_posted(handle),
            None => self.flow().release(handle, Step::Hello),
        }
    }

    /// Whether the offer could be due: the welcome was shown and the offer was not.
    pub(crate) fn offer_due(&self, handle: &str) -> bool {
        self.flow().on()
            && self.flow().seen(handle, Step::Hello)
            && !self.flow().seen(handle, Step::Offer)
    }

    /// Step 2: when the PM has linked Jira and Confluence, offer the three starters, once.
    /// Called on their next DM and on a Home open.
    pub(crate) async fn onboarding_offer(&self, channel: &str, handle: &str) {
        if !self.offer_due(handle) {
            return;
        }
        let linked = match self.toolserver().onboarding(handle).await {
            Ok(state) => state["linked"]["atlassian"].as_bool().unwrap_or(false),
            Err(e) => {
                tracing::warn!(error = %e, "onboarding state unavailable for the offer");
                false
            }
        };
        if !linked || !self.flow().claim(handle, Step::Offer) {
            return;
        }
        match self.post(channel, None, None, cards::onboard_offer()).await {
            Some(_) => self.onboarding_posted(handle),
            None => self.flow().release(handle, Step::Offer),
        }
    }

    /// The offer on a Home open: opens the DM only when the offer is due and the account linked.
    pub(crate) async fn onboarding_offer_for_slack_user(&self, slack_id: &str) {
        let Some(handle) = self
            .people()
            .identify(slack_id)
            .into_member()
            .map(|m| m.handle)
        else {
            return;
        };
        if !self.offer_due(&handle) {
            return;
        }
        match self.slack().open_dm(slack_id).await {
            Ok(channel) => self.onboarding_offer(&channel, &handle).await,
            Err(e) => tracing::warn!(error = %e, "couldn't open the DM for the offer"),
        }
    }

    /// A tap on a starter: a task for the PM's personal bot, through the path a typed DM takes.
    /// The answer arrives in the thread under the offer.
    pub(crate) async fn onboarding_start(&self, click: Click) {
        let Some(handle) = self.clicker(&click).await else {
            return;
        };
        if !self.onboarding_v2() {
            return;
        }
        let Some(starter) = onboard_copy::starter(&click.value) else {
            return;
        };
        let thread = click.message_ts.clone();
        let dm = Dm {
            user: click.user.clone(),
            channel: click.channel.clone(),
            thread_ts: thread.clone(),
            in_thread: false,
            text: starter.task.to_string(),
        };
        let roster = match self.roster().await {
            Ok(roster) => roster,
            Err(_) => {
                self.post(
                    &dm.channel,
                    Some(&thread),
                    None,
                    cards::note(crate::bots::UNAVAILABLE),
                )
                .await;
                return;
            }
        };
        let bot = choose_bot(None, None, &handle, &roster);
        let shown = crate::bots::named(&roster, &bot);
        if self
            .start_task(&dm, &handle, &bot, &shown, starter.task)
            .await
        {
            self.update(
                &click.channel,
                &click.message_ts,
                cards::onboard_started(starter.label),
            )
            .await;
        }
    }

    /// Step 3: after the first answer in a PM's thread, one closing line, once.
    pub(crate) async fn onboarding_closing(
        &self,
        handle: &str,
        channel: &str,
        thread: Option<&str>,
    ) {
        if !self.flow().on()
            || !self.flow().seen(handle, Step::Hello)
            || !self.flow().claim(handle, Step::Closing)
        {
            return;
        }
        match self
            .post(channel, thread, None, cards::onboard_closing())
            .await
        {
            Some(_) => self.onboarding_posted(handle),
            None => self.flow().release(handle, Step::Closing),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_reads_only_one() {
        assert!(enabled(Some("1")) && enabled(Some(" 1 ")));
        assert!(!enabled(None) && !enabled(Some("0")) && !enabled(Some("true")));
    }

    #[test]
    fn a_claim_is_kept_across_restarts_and_never_repeats() {
        let dir = tempfile::tempdir().unwrap();
        let flow = Flow::open(dir.path(), true);
        assert!(flow.claim("ben", Step::Hello));
        assert!(!flow.claim("ben", Step::Hello));
        assert!(flow.claim("cal", Step::Hello));
        flow.release("cal", Step::Hello);
        let flow = Flow::open(dir.path(), true);
        assert!(flow.seen("ben", Step::Hello) && !flow.seen("ben", Step::Offer));
        assert!(!flow.claim("ben", Step::Hello));
        assert!(
            flow.claim("cal", Step::Hello),
            "a released claim is free again"
        );
    }

    #[test]
    fn off_nothing_is_claimed_or_written() {
        let dir = tempfile::tempdir().unwrap();
        let flow = Flow::open(dir.path(), false);
        assert!(!flow.claim("ben", Step::Hello) && !flow.seen("ben", Step::Hello));
        assert!(!dir.path().join(STATE_FILE).exists());
    }
}
