# Nudges

A nudge is one short, gentle DM from PM OS about **your own work**, such as drafts that have
been waiting a while. It is built to be easy to ignore and easy to switch off.

**It is off unless the operator sets `PMOS_NUDGES=1`.** Without it nothing changes: no
messages, no state file, and `nudges on` / `nudges off` go to your bot like any other text.

## The rules

These are requirements, enforced in `crates/slack/src/nudges.rs` and covered by tests in
`crates/slack/tests/nudges.rs`.

1. **At most one nudge per PM per day.** The day is the PM's own calendar day.
2. **In the PM's DM only,** never in a channel or a thread.
3. **Only inside the PM's working hours,** in their own time zone: 09:00 to 17:00 on weekdays
   by default. The zone comes from Slack's profile for the PM; if Slack has none, it is
   `PMOS_DEFAULT_TZ`. Nothing is sent on Saturday or Sunday.
4. **Every nudge is about the PM's own work.** Never about other people, and never in public.
5. **Every nudge has a "Not now" button.** Tapping it counts as a dismissal and replaces the
   message with "Okay, not now."
6. **Three ignored or dismissed nudges in a row, and PM OS goes quiet for that PM for 14
   days.** A nudge is ignored when the next one is due and none of the things it was about
   (for drafts, none of those drafts) has been dealt with. Dealing with any of them resets the
   count to zero. After the 14 days nudges may return.
7. **`nudges off` and `nudges on`,** typed alone in a DM, stop and resume them. Each is
   answered with one plain line. `nudges on` also ends a quiet period and resets the count.
8. **Each wording line is shown to a given PM at most once.** When a PM has seen every line
   for a kind, that kind stays silent for them until the owner adds more lines.
9. **Another daily message counts as that day's nudge.** If PM OS already sent the PM its
   own daily message (the triage digest, say), no nudge goes out that day. Other modules call
   `nudges::record_daily_message(handle)` when they send one.
10. **State survives a restart.** The file `nudges.json` in the Slack state directory
    (`PMOS_SLACK_STATE_DIR`) holds, per PM: on or off, the last nudge day, the unanswered
    count, any quiet-until date, the line ids already shown and the open nudge. It is
    written to a temporary file and renamed, like `triage.sent`. The day is recorded
    **before** the message is posted, so a restart never sends a second nudge the same day;
    if the post fails, the day is given back.

## The kind: drafts waiting

Sent when the PM has pending drafts (the tool server's inbox, the same count as the App Home)
and **the oldest is more than 4 hours old**. No drafts, or only fresh ones, means no nudge.

The wording is true for the data:
- the count is the real total, as a word up to twelve ("Three") and digits above;
- singular and plural forms are separate text ("One draft is waiting...");
- **expiry is only claimed when every draft expires.** Slack drafts (`slack.post_reply`,
  `slack.send_message`) expire 24 hours after they were made, or at the item's `expires_at`.
  Other drafts, such as a Jira or Confluence write, do not, and then a line that mentions
  expiry is not used. The time shown is the soonest expiry.

## Editing the words

All wording is data in `crates/slack/src/nudge_copy.rs`: a list of lines with an id, a kind,
a singular form and a plural form. Placeholders are `{Count}`, `{count}` and `{expires}`. A
line that uses `{expires}` must set `expires: true`. Keep the voice gentle and lightly funny,
about the work, and never about a person, a missed deadline or customer data. Do not reuse
or rename an id after it has shipped: ids are how "shown once" is remembered.

## Adding a kind

Implement `NudgeKind` in `nudges.rs` (an id, and a function from what is known about the PM
to an optional candidate with a count and the references the nudge is about), add it to
`default_kinds()`, and add its lines to `nudge_copy.rs`. The rules above apply to every kind
without further work.

## Settings

| Setting | Default | |
|---|---|---|
| `PMOS_NUDGES` | unset (off) | `1` turns the engine on. |
| `PMOS_NUDGES_HOURS` | `09:00-17:00` | Working hours, local, `HH:MM-HH:MM`. |
| `PMOS_DEFAULT_TZ` | `UTC` | The zone for a PM Slack has none for (shared with the triage digest). |
| `PMOS_SLACK_STATE_DIR` | `state/slack` | Where `nudges.json` lives. |

The engine looks once a minute. The PM's Slack time zone is cached for six hours.

## Wiring

`main.rs` starts the engine with one call (`nudges::spawn`). Two hooks in `bots.rs` hand it
the DM command and the button; both do nothing when the engine is off:

```rust
// in on_dm, before the `level` command:
if crate::nudges::handle_dm(&dm.channel, &dm.thread_ts, &handle, &text).await {
    return;
}
// at the top of the `Some("block_actions")` arm of `interactive`, before `Click::from_payload`:
if let Some(work) = crate::nudges::action(payload) {
    return (None, Some(work));
}
```
