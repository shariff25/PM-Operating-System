# The Slack reply assistant (SLK-1 to SLK-8): build spec

When someone in Slack asks a PM something, PM OS drafts a grounded reply. The PM approves,
edits, snoozes or rejects it on a card, and an approved reply posts **as the PM**, marked
"Drafted with PM OS". Decided 2026-11-15; the plan is in `docs/implementation-plan.md`.

**The rules every part follows:**
- **The bot token never reads channels.** Every read uses the PM's own token. `slack.search`
  is the Real-time Search API and stores nothing.
- **Nothing posts without the PM's click.** `slack.post_reply` is always `approve`.
- **No Slack message text is stored beyond 24 hours**: not in traces (already the case), not
  in approval items (§3 below), and never in the ledger.

## 1. The trigger (SLK-1): a routine on the PM's personal bot

**Turning it on.** A member DMs `reply assistant on`. Slack creates the routine directly with
`POST /v1/routines {draft}`; the model is not involved. The draft is:

```json
{"bot": "pm-<handle>", "owner": "<handle>",
 "schedule": {"cron": "*/30 8-17 * * 1-5", "tz": "<the PM's Slack time zone, else PMOS_DEFAULT_TZ>"},
 "prompt": "[reply-assistant] Check Slack for messages since the last run that ask me something or need my answer, and draft a reply to each with the slack-reply skill. Skip anything I already answered.",
 "output": {"kind": "dm"},
 "approval": "approve"}
```

The `output` is the routine output the runtime already uses: a DM to the owner. Check
`crates/bots/src/model.rs` for its exact shape.

**Finding and changing it.** The prompt's `[reply-assistant]` prefix is how Slack finds the
routine. Members can type these commands alone in a DM:
- `reply assistant on` creates the routine, or says it's already on.
- `reply assistant off` deletes it.
- `reply assistant` says whether it is on, and when it next runs.

**Personal bots only.** If the member has no personal bot, the commands explain that and do
nothing.

## 2. The skill (SLK-2 to SLK-4): `content/skills/slack-reply/SKILL.md`

**Front matter:**
- `data-class: restricted`;
- tools: `slack.search`, `slack.read_thread`, `slack.post_reply`, `docs.search`, `docs.read`,
  `jira.search`, `jira.read`, `confluence.search`, `confluence.read`, `context.search` and
  `context.read`;
- surface `slack`.

**Steps:**
1. **Search.** Use `slack.search` for messages since the last run that mention the PM or are
   in their DMs. The prompt says "since the last run"; with no date, search the past day.
2. **Read and filter.** Read each candidate thread with `slack.read_thread`. Skip threads
   where:
   - the PM already replied after the question;
   - the message isn't a question or request for the PM;
   - it is an FYI, an announcement or a bot message.
3. **Ground the answer.** Use the docs, Jira, Confluence and context tools. Cite every source
   as a link. If the sources don't answer it, the draft says so and suggests who might know,
   instead of guessing.
4. **Draft.** Call `slack.post_reply` once per thread, short and in the PM's voice. If the tool
   says the thread was already drafted, move on.
5. **Report.** Finish with a one-line summary for the PM: how many drafts are waiting, and which
   threads were skipped and why.

**The role.** `content/roles/pm-assistant/ROLE.md` gets `slack-reply` in its skills and
`slack.post_reply: approve` in its actions. Regenerate the policies with `pmos policy generate`.

## 3. Tool server

**One draft per thread.** `slack.post_reply` checks for an existing approval item for the same
user, `channel` and `thread_ts` from the last 7 days that is `pending`, `executing` or
`approved`.
- If there is one, the call returns success without creating a new item:
  `{"status": "already_drafted", "approval_id": "<existing>"}`, with the message "A reply to
  this thread is already waiting for <user> (or was posted); don't draft another."
- It is audited as `ok` with detail `already drafted`.
- **Tagging PM OS in a thread** adds `message_ts`, which makes the rule per message:
  `docs/slack-mentions-and-home-spec.md` §3.

**What the draft was built from.** Each session tracks the highest sensitivity it has read:
- `context.*` results count at their file's `sensitivity`;
- `jira.*`, `confluence.*` and `slack.*` reads count as `internal`;
- `docs.*` counts as `public`.

A new approval item stores it in `sources_sensitivity` (`public`, `internal` or `restricted`;
migration `0008_slack_reply.sql`).

**The audience check (SLK-6).** It runs on `approve` or `edit` of `slack.post_reply` and
`slack.send_message`, before the write, using the PM's token.
- **The calls:** `conversations.info` gives `is_ext_shared`, `is_shared` and `is_im`, and
  `conversations.members` gives the member ids, paged and capped at 1,000.
- **The scopes:** add the user scopes `channels:read`, `groups:read`, `mpim:read` and `im:read`
  to `docs/slack-app-manifest.yaml` and to the default `PMOS_SLACK_SCOPES`.
- **The rules,** in order:
  1. **Restricted sources reaching outsiders are blocked.** If `sources_sensitivity` is
     `restricted` and any member isn't a PM OS insider (since slice U3: an active user holding
     `pm`, `reviewer` or `admin` in the user database), or the channel is externally shared,
     answer `409 {code: "audience_blocked", error, outsiders: <count>}`. Nothing is written
     and the item stays `pending`. An edit doesn't unblock it: the draft was built from
     restricted sources whatever its text now says, so the card offers only Reject, and the
     PM answers in the thread themselves.
  2. **External channels need a second confirmation.** If the channel is externally shared
     (Slack Connect) and the decision lacks `confirm_external: true`, answer
     `409 {code: "needs_external_confirm", error}`.
  3. **Otherwise** the write goes ahead.
- **The decision API:** `POST /internal/approvals/{id}/decision` gains an optional
  `confirm_external`.
- **Failures:** when Slack's channel calls fail, the decision fails closed with
  `503 {code: "audience_unavailable"}`. Each block or confirmation request is audited as
  `denied` with its code.

**The 24-hour expiry (SLK-8).** A sweeper runs every 10 minutes.
- **Undecided drafts expire.** A `pending` `slack.post_reply` or `slack.send_message` item
  older than `PMOS_SLACK_DRAFT_TTL_HOURS` (default 24) becomes `expired`, a new status added to
  the CHECK in migration 0008.
- **Text is removed.** Every string argument except `channel` and `thread_ts` becomes
  `"[expired]"`, and the args hash is kept.
- **Decided drafts lose their text too,** 24 hours after the decision. The posted copy lives
  in Slack.
- **The ledger** records `expired` as a stored outcome, and the expired item is audited.
- **Deciding an expired item** answers `409 {code: "expired"}`.

## 4. Bot runtime

`expired` is a closed approval state. A task waiting on approvals settles when its last item is
`expired`, rejected or decided, and Slack gets `approval_closed` for it, as for the other
closed states.

## 5. Slack: the reply card

**What the card shows.** An approval item for `slack.post_reply` gets its own card:
- **Header:** "Reply drafted for #channel" (or "for your DM with @name"), with a link to the
  thread (the Slack permalink). The question's text is never shown.
- **The draft,** with its cited links.
- **The audience,** when known from the item:
  - "Visible to N people" (from `conversations.members`);
  - "External channel" when shared.

  Slack may get these from the tool server on decision errors, or show only the channel.
- **The buttons:** **Post as you**, **Edit** (the existing modal), **Snooze**, and **Reject**
  (with the existing reasons).

**Snooze.** It opens three choices: 1 hour, 4 hours, or tomorrow at 09:00 in the PM's time zone.
- Choices that would pass the item's expiry (created + 24 hours) are not offered.
- The card is replaced by "Snoozed until …". At that time Slack posts the card again, if the
  item is still `pending`.
- Snoozes are kept in the state directory, so they survive restarts.

**Answers from the tool server:**
- `audience_blocked`: the card shows the reason in plain words ("This draft was built from
  restricted sources and N people in #channel aren't on the product team"), with only
  **Reject**: an edit can't unblock it.
- `needs_external_confirm`: the card adds **Post to external channel**, which sends the
  decision again with `confirm_external: true`.
- `expired`: the card reads "Expired after 24 hours; nothing was posted".

**Commands:** `reply assistant on`, `off` and status (§1).

## 6. Content and benchmarks

**Fake-connector fixtures** (`crates/toolserver/src/connectors/fake.rs`):
- search results that include a mention of the PM with a question, a thread where the PM
  already answered, and an FYI;
- `conversations.info` and `members` fixtures for:
  - an all-team channel;
  - a channel with an outsider;
  - a Slack Connect channel.

**The `slack-reply` category.**
- **Its tasks:** re-point them at the new skill through the personal bot, with at least 5 seed
  tasks:
  - an answerable question, drafted with a citation;
  - an already-answered thread, which gets no draft;
  - an FYI, which gets no draft;
  - a question the sources can't answer, where the draft says so;
  - two mentions in one thread, which get one draft.

  Use `drafts:` checks (tool, count, `channel`, `thread_ts`, `must_include`), and keep "seed:
  replace with case cards".
- **Its launch bar:** a starting bar of 80% over 3 repeats since 2026-11-16, on seed tasks; to be tightened once real case cards exist.

## 7. Docs

- `docs/slack-reply.md` covers:
  - how it works, from the routine to the skill, the card and the post;
  - what it reads and with whose token;
  - the audience rules;
  - retention.
- The Slack docs (`docs/docs-server.md`) and `docs/toolserver.md`: routes, statuses,
  variables.
- `docs/slack-app-manifest.yaml`: the new scopes.
- `docs/runbooks/quality.md`: reply drafts appear in the ledger and the digest.
- The plan status and the changelog.
