# Tagging PM OS in a channel, and the App Home: build spec

Two pieces of the pilot's Slack experience (`docs/implementation-plan.md`, "Interface: the PM
OS Slack bot"):
- **Tag it in a thread.** A PM writes `@PM OS …` in a channel thread and gets a drafted reply,
  privately, that they can post as themselves.
- **The App Home.** One place for what's waiting (Inbox), what PM OS can do (Skills) and what
  it's connected to (Accounts).

**The rules every part follows** (unchanged):
- **Nothing posts without the PM's click.** A reply to a channel is a `slack.post_reply`
  approval, with the audience check and the 24-hour expiry it already has.
- **The app's own token never reads channels.** A thread is read with the PM's own token, by
  their personal bot, through `slack.read_thread`.
- **Only members can use it.** Membership is `team.yaml`.

## 1. Design decisions, and why

**A tagged answer is delivered in the PM's DM with PM OS, not as an ephemeral in the thread.**
The thread gets one ephemeral line saying so.
- An ephemeral message is gone on reload and can't be edited, so progress, the draft, edits
  and Snooze would each need a second code path through `response_url`.
- The DM is private, lasts, and already carries progress messages, the reply card (Post as
  you, Edit, Snooze, Reject), expiry and the ledger. One delivery path stays the only one.
- The draft is an approval item, so it is in the Inbox too, as the plan requires.

**The request reaches the bot as task text with a marker,** like the reply assistant's
`[reply-assistant]`. The worker only ever sees a task's text, and the runtime rejects unknown
task fields, so no runtime or harness change is needed.

**The personal bot answers,** because only it reads as the PM (`slack.read_thread`,
`slack.post_reply`). Team bots are never used for a tag.

**A tag is deduplicated per tagged message, not per thread.** The reply assistant's rule (one
draft per thread for 7 days) would refuse a PM who tags PM OS twice in one long thread.
`slack.post_reply` gains an optional `message_ts`; with it, the one-draft rule is per message.

**The Home is one view with three sections,** switched by buttons, because Slack gives an
app one Home tab. Decisions in the Inbox are tick-then-act: tick drafts, then **Approve
selected** or **Reject selected**. One mechanism covers one draft or ten, and an approval
stays a deliberate act. Editing, Snooze and confirming an external channel stay on the card in
the DM.

**Skills are grouped by a `track`** in each skill's front matter, so 27 skills read as a
catalog and not a wall.

## 2. Tagging PM OS in a channel (`crates/slack/src/mention.rs`, new)

**The event.** `app_mention` (bot scope `app_mentions:read`), dispatched from `Bots::event`.
Ignored: events with a `bot_id` or a `subtype`, channels whose id starts with `D`, and repeats
(the existing seen-events check). Without `Bots` configured, it is acked and dropped as today.

**The steps,** for an event `{user, text, channel, ts, thread_ts?}`:
1. **Member check.** `team.by_slack_id(user)`. A non-member gets one ephemeral in the thread:
   "PM OS is for the product team." No button: the request-access path lives in the DM, and a
   channel may include people from outside the company.
2. **Personal bot check.** The roster must hold `pm-<handle>`. If not, ephemeral: "Tagging
   needs your personal bot. DM me `setup` to finish setting up."
3. **Slack link check.** `GET /internal/onboarding?user=<handle>`; if `linked.slack` is false,
   ephemeral: "Connect your Slack account first, so I can read this thread as you: DM me
   `setup`." If the call fails, carry on: the tool reports it later.
4. **The ask.** Strip the app's own mention from `text` (the bot user id is
   `authorizations[0].user_id` in the envelope; fall back to stripping the first `<@…>`),
   trim, cap at 2,000 characters. Empty becomes "Draft a reply to this thread."
5. **Open the DM** (`conversations.open`) and post the thread's root message there, under the
   personal bot's name:
   "You tagged me in <permalink|a thread in #channel>. I'm drafting a reply here. Nothing
   posts there until you say so."
   The permalink comes from `chat.getPermalink {channel, message_ts}` (new wrapper); if that
   fails, use `<#channel>` with no link. The message never quotes the thread.
6. **Create the task** with `runtime.create_task(handle, "pm-<handle>", text, <DM channel>,
   <root message ts>)`, so progress, the answer and the reply card all land in that DM thread
   through the existing notifier. Remember thread → bot as `start_task` does. The text is:

   ```
   [tagged in Slack] channel=<C…> thread_ts=<ts> message_ts=<ts>
   <the ask>
   ```

   `thread_ts` is the event's `thread_ts`, or its `ts` when the tag is a top-level message.
   `message_ts` is the event's `ts`. Both ids are validated before use (`^[CG][A-Z0-9]{2,}$`,
   `^\d{1,12}\.\d{1,9}$`); an event that fails is dropped with a warning.
7. **Tell the thread,** as one ephemeral to the asker with `thread_ts` set: "On it. The draft
   will be in <DM link|our DM>. Nothing posts here until you say so." The DM link is
   `https://slack.com/app_redirect?channel=<DM channel id>`.

If step 5 or 6 fails, the ephemeral says: "I couldn't start on this. Try again in a minute, or
DM me." Every ephemeral failure is logged and otherwise ignored.

**The skill** (`content/skills/slack-reply/SKILL.md`) gains a section, "When the request
starts with `[tagged in Slack]`":
- Skip the search and the filtering. The PM asked; the thread is the one named.
- Read it: `slack.read_thread {channel, ts: thread_ts}`.
- **Decide what was asked.** If the PM asked for something for themselves (a summary, a
  status, where a document is), answer in the DM and draft nothing. Otherwise the ask is the
  question to answer, or instructions for the reply; draft it.
- Ground and cite as for any reply. If the sources don't answer, the draft says so.
- Call `slack.post_reply {channel, thread_ts, message_ts, text}` once.
- If the tool answers `already_drafted`, say a draft for that message is already waiting.

## 3. Tool server

**`slack.post_reply` gains `message_ts`** (optional string, pattern `^\d{1,12}\.\d{1,9}$`):
the message the reply answers.
- **One draft per message.** With `message_ts`, the existing-draft check matches on user,
  channel, `thread_ts` **and** `message_ts`. Without it, the check is unchanged (per thread).
- **Not sent to Slack.** The connector drops it before `chat.postMessage`.
- **Kept through expiry and purge,** like `channel` and `thread_ts`: it is an id, not text.
- **Not shown** on the reply card or in the edit modal.

**The Slack client may list a member's pending approvals.**
`GET /internal/approvals?user=<handle>` moves into what the bot client may call, limited for
that client: `user` is required and must be a team member; only `pending` items are returned
whatever `status` says; oldest first; at most 50, plus `total`. The orchestrator's access is
unchanged.

**The Slack client may unlink a member's account.**
`DELETE /internal/credentials/{user}/{provider}` is allowed for the bot client when `user` is
a team member. It is audited with the client, as every call is.

Both follow the trust the Slack client already has: it maps Slack ids to handles for
decisions, onboarding and OAuth starts.

## 4. The App Home (`crates/slack/src/home.rs`, new)

**Plumbing:**
- **Manifest:** `home_tab_enabled: true`; bot events `app_home_opened` and `app_mention`; bot
  scope `app_mentions:read`.
- **`views.publish {user_id, view}`**, a new wrapper in `slackapi.rs`.
- **`app_home_opened`** with `tab == "home"` publishes the view for `event.user`. The section
  is the one in the event's `view.private_metadata` (`{"tab": "inbox"|"skills"|"accounts"}`),
  default `inbox`.
- **Clicks.** A `block_actions` payload whose `view.type` is `home` goes to
  `home.action(payload)` before `Click::from_payload`, which needs a message and would drop
  it. Every Home action republishes the view; nothing in the Home uses `chat.update`.
- **Keeping it fresh.** The process remembers who has opened the Home (a bounded map). After
  the notifier handles `needs_approval` or `approval_closed` for one of them, it republishes
  their Home, best effort.
- **A non-member** sees: "PM OS is for the product team. DM me to ask for access."

**Layout.** A header, then three buttons (`home_tab_inbox`, `home_tab_skills`,
`home_tab_accounts`; the current one is `primary`), then the section.

### Inbox
- **Data:** `toolserver.pending_approvals(handle)` (the route in §3).
- **Top line:** "N drafts waiting for you", or "Nothing is waiting for you."
- **Buttons:** **Approve selected** (`home_approve`, with a Block Kit `confirm`: "Approve the
  selected drafts? Each one is created or posted as you."), **Reject selected**
  (`home_reject`), **Refresh** (`home_refresh`).
- **Rows:** the 10 oldest (Slack drafts expire first). Each row is one section: what it is and
  where it goes (reuse the wording the cards use for a Jira issue, a Confluence draft, a Slack
  reply or message), which bot drafted it, how long ago, and a preview of the text cut to 200
  characters. Its accessory is a one-option `checkboxes` element (`home_pick`, block id
  `pick_<approval id>`). A tick fires `block_actions`; it is acked and ignored.
- **More than 10:** "and N more; they appear as you clear these."
- **Footer:** "To edit or snooze a draft, use its card in our DM."

**Acting.** `home_approve` or `home_reject` reads the ticked ids from
`view.state.values`, checks each id's shape, and decides them one at a time with the existing
`decide(id, handle, "approve"|"reject", None, None, false)`. None ticked: "Tick the drafts
first." Then the view is republished with a notice at the top that counts each outcome in
plain words:
- approved or posted;
- rejected;
- **blocked** (`audience_blocked`): "built from restricted sources, and people in the channel
  aren't on the product team. Reject it, or answer in the thread yourself";
- **needs your confirmation** (`needs_external_confirm`): "it goes to a channel shared outside
  the company. Confirm on its card in our DM";
- expired, or already decided;
- failed: "PM OS couldn't reach the tool server" or the write's error.

The notice lives only in that publish; nothing is stored.

### Skills
- **Data:** the content repo (`PMOS_CONTENT_ROOT`, already read for `team.yaml`), through
  `pmos_contract::lint`'s `Report.skills`, cached for 60 seconds. Shown: skills with the
  `slack` surface that the member's personal bot's role lists (`GET /v1/bots/pm-<handle>`,
  a new runtime-client call); with no personal bot, the Docs bot's.
- **Layout:** a line on how to use them ("DM me and ask in your own words, or name a skill:
  `strategy-doc: our plan for small teams`"), then one header per track in the order below,
  and one line per skill: the name in bold and the first sentence of its description, cut to
  150 characters.
- **Track order:** Everyday, Product sense, Strategy and decisions, Metrics and experiments,
  Spec and review, Customer and market, AI products, Delivery and launch, then any other
  track alphabetically, then "Other" for skills with none.

### Accounts
- **Data:** `GET /internal/onboarding?user=` (`linked`, `memory_repo`).
- **Rows:** Jira and Confluence (one Atlassian link), Slack, GitHub (shown only when the
  onboarding answer includes it). Each says Connected or Not connected and what PM OS uses it
  for.
  - **Not connected:** a **Connect** URL button from `oauth_start`, as the setup checklist
    does. The link lasts 10 minutes; **Refresh** makes new ones.
  - **Connected:** **Disconnect** (`home_unlink_<provider>`), with a `confirm` that says what
    stops working. For Slack: "The reply assistant, tagging and thread reading stop until you
    connect again."
- **Disconnect** calls `toolserver.unlink(handle, provider)` (new client method for the route
  in §3) and republishes with "Disconnected <name>." or the error.
- **Memory:** the repo name, when set.

## 5. The `track` field

- **Contract:** `metadata.track` in `SKILL.md`: optional, a string of at most 40 characters.
  `Skill.track: Option<String>`. Lint rule `skill-track` for a longer or empty value.
- **Content:** every skill gets one:

| Track | Skills |
| --- | --- |
| Everyday | `ask`, `docs`, `meeting-notes`, `slack-reply` |
| Product sense | `product-critique`, `problem-framing` |
| Strategy and decisions | `strategy-doc`, `decision-doc`, `prioritize`, `pre-mortem` |
| Metrics and experiments | `metrics-tree`, `experiment-design`, `experiment-readout` |
| Spec and review | `prd-draft`, `review` |
| Customer and market | `call-synth`, `competitive-brief`, `opportunity-sizing`, `poc-plan`, `partner-brief` |
| AI products | `ai-feature-spec`, `eval-plan` |
| Delivery and launch | `release-notes`, `status-update`, `launch-checklist`, `jira-draft`, `advisory-draft` |

## 6. Tests

**Slack** (the fake Socket Mode server and mocks in `crates/slack/tests/`; the fake Web API
learns `views.publish` and `chat.getPermalink`):
- **Tagging:**
  - a member's tag: an ephemeral in the thread, a root message in their DM, and a task for
    `pm-<handle>` whose `slack` is the DM channel and root ts, and whose text has the marker,
    the three ids and the ask with the app's mention stripped;
  - a top-level tag uses its own `ts` as `thread_ts`;
  - a non-member gets the plain note and no task;
  - no personal bot, and Slack not linked, each get their note and no task;
  - a repeated event, a bot's message and a DM-channel mention do nothing;
  - a failed task creation tells the asker.
- **Home:**
  - opening it publishes the Inbox for a member, and the note for a non-member;
  - the section buttons switch sections, and the section survives a republish;
  - ticking two drafts and approving decides both and shows the count;
  - mixed outcomes (posted, blocked, needs confirmation, expired) each show their line;
  - nothing ticked asks to tick first;
  - Skills groups by track and shows only the role's Slack skills;
  - Accounts shows Connect for an unlinked account and Disconnect for a linked one, and
    Disconnect calls the tool server;
  - a `needs_approval` notification republishes an open Home.

**Tool server:**
- **`message_ts`:**
  - the same `message_ts` returns `already_drafted`;
  - a different one in the same thread makes a new item;
  - none keeps the per-thread rule;
  - it isn't sent to Slack, and it survives expiry and purge.
- **Pending list for the bot client:**
  - a member's pending items only, oldest first, capped;
  - a non-member is refused;
  - `status` is ignored;
  - the orchestrator's access is unchanged.
- **Unlink by the bot client:** works for a member, refused for a non-member, audited.

**Contract:** `track` parsed, optional, and the lint rule.

**Benchmarks:** one `slack-reply` task whose ask is a `[tagged in Slack]` request on a fixture
thread; it expects one draft for that channel and thread.

## 7. Docs

- `docs/slack-reply.md`: a section on tagging PM OS in a thread.
- `docs/slack-home.md` (new): the three sections, for PMs.
- `docs/slack-app-manifest.yaml`, and `docs/runbooks/install.md` and `upgrade.md`: the new
  scope and events mean the Slack app must be updated and reinstalled.
- `docs/toolserver.md` (the two routes, `message_ts`), `docs/contract.md` (`track`),
  `docs/slack-reply-spec.md` (a pointer here), the plan status and the changelog.
