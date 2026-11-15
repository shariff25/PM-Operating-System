# The Slack reply assistant

When someone in Slack asks you something, your PM OS bot drafts a reply from the team's
sources and shows it to you. Nothing posts until you tap **Post as you**. Then it posts in the
thread, as you, marked "Drafted with PM OS". The build spec is `docs/slack-reply-spec.md`
(SLK-1 to SLK-8).

## Turning it on

DM PM OS:

| You type | What happens |
| --- | --- |
| `reply assistant on` | Starts a check every 30 minutes, weekdays 08:00–18:00 in your time zone, by your personal bot `pm-<you>` |
| `reply assistant` | Says whether it's on and when it next runs |
| `reply assistant off` | Stops it |

**Before turning it on:** link Slack with `setup`. If you linked Slack before this feature,
link it again: the audience check below needs four more read permissions.

## What happens on each check

1. **Search.** Your bot searches Slack for messages since the last check that mention you or
   are in your DMs. It searches with your own Slack account, through Slack's search API, which
   PM OS doesn't store. The PM OS app's own token never reads channels.
2. **Skip.** It reads each thread and skips anything that doesn't need you:
   - threads you already answered;
   - FYIs and announcements;
   - bot messages;
   - questions meant for someone else.
3. **Find the answer.** It looks in the team's context, Confluence, Jira and the support docs,
   and cites them. If they don't answer it, the draft says so instead of guessing, and it never
   promises dates, prices or scope for you.
4. **Draft.** You get one card per thread, and never two for the same thread.

## The card

- **What it shows:**
  - where the reply would go;
  - a link to the thread (it never copies the question into the card);
  - the draft;
  - what kind of sources it used;
  - who would see it.
- **Post as you** posts it in the thread.
- **Edit** opens the draft for changes; you post the edited text.
- **Snooze** brings the card back in 1 hour, in 4 hours, or tomorrow at 9:00, within the
  draft's 24 hours.
- **Reject** asks why in one tap. Your reasons feed the improvement loop
  (`docs/runbooks/quality.md`).

## Who will see it (the audience check)

When you tap **Post as you**, PM OS checks the channel first:

| Situation | What happens |
| --- | --- |
| The draft used **restricted** sources, and someone in the channel isn't on the product team, or the channel is shared outside Fortanix | **Blocked.** Nothing posts. The card offers only Reject: answer in the thread yourself if it still needs a reply. |
| The channel is shared with another company (Slack Connect) | **Confirm first.** The card asks again before posting to an external channel. |
| Otherwise | It posts. |

**Who counts as the product team:** `team.yaml`.

**What counts as restricted:** a draft is restricted if your bot read any restricted context
while writing it. Jira, Confluence and Slack reads count as internal, and the support docs as
public.

## What PM OS keeps

- **The draft text:** an undecided draft expires after 24 hours and its text is deleted; the
  card says "Expired after 24 hours". A decided draft's text is deleted 24 hours after the
  decision; the posted reply lives in Slack.
- **Everything else, kept:** that a draft existed, its channel and thread ids, and the outcome.
  The feedback ledger and the audit log have no message text.
- **Traces:** Slack text is removed from your bot's traces after 24 hours, and the traces after
  30 days.

## Settings (platform owners)

| Variable | Service | Default | Notes |
| --- | --- | --- | --- |
| `PMOS_SLACK_DRAFT_TTL_HOURS` | tool server | 24 | When undecided Slack drafts expire. It can't be set above 24. |
| `PMOS_SLACK_SCOPES` | tool server | includes the four `:read` scopes | Keep `channels:read`, `groups:read`, `mpim:read` and `im:read`: the audience check needs them |
