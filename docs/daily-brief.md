# The daily brief

Each weekday morning a PM gets one short DM from their personal bot: what changed, what is
waiting on them, and what they are waiting on, from Jira, Confluence and Slack. Every line
links to its source, and a line with no source is not shown. A quiet day gets one line.

This is the design for the whole feature. Part 1 is built from the tools that exist today (the
`daily-brief` skill, `content/skills/daily-brief/SKILL.md`, and its benchmark, `evals/categories/
daily-brief/`). Each brief also fills the web app's home page ("The web app's panels",
below). The follow-up tracker's reconciliation and the Slack buttons are a later slice (part 4).
The gaps that stop today's version being fully reliable are in part 3.

```
Morning, Ana. Four things moved overnight, two people are waiting on you, and one question of
yours is still waiting for a reply.

**What changed**
- PAY-212 moved to Blocked: waiting on the vendor sandbox. ([Jira](…))
…
**Waiting on you**
- Ben in #pm-team asked about SAML SSO for the console, 2 days ago. ([Slack](…))

**You are waiting on**
- Legal: your question about the DPA, 3 days ago. ([Slack](…))

Want a reply or a nudge drafted? Send "draft a reply" or "draft a nudge" with a link from above.
Nothing goes out until you approve it.
```

A quiet day is exactly: `Nothing moved overnight. Enjoy it while it lasts.`

## 1. The brief as a default routine

The brief is a routine (`docs/bot-runtime-spec.md`, Routines): a cron schedule plus a prompt,
run by the PM's personal bot, with its output sent to the PM's DM. It needs no new runtime
concept.

| The PM says (in a DM) | What PM OS does |
| --- | --- |
| `brief me at 8` | Creates the routine with `POST /v1/routines {draft}`, as `reply assistant on` does (`docs/slack-reply-spec.md` §1): the model is not involved. Reads back the time and the PM's time zone, and says "Brief off turns it off." |
| `brief me at 7:30`, `brief me at 8 London` | The same, at that time or time zone |
| `brief off` | Deletes the routine |
| `brief` | Says whether it is on and when it next runs |
| `brief me now` | Runs the skill once now and does not change the routine |

The routine draft:

```json
{"bot": "pm-<handle>", "owner": "<handle>",
 "schedule": {"cron": "0 8 * * 1-5", "tz": "<the PM's Slack time zone, else PMOS_DEFAULT_TZ>"},
 "prompt": "[daily-brief] Write my daily brief with the daily-brief skill.",
 "output": {"kind": "dm"},
 "approval": "approve"}
```

- **Finding and changing it.** The prompt's `[daily-brief]` prefix is how Slack finds the
  routine, as `[reply-assistant]` does.
- **Weekdays only.** The cron is `1-5`. A missed run runs once on start, not once per missed
  slot (the routine rule), so a restart at 11:00 sends a late brief, not two.
- **Default.** "Default routine" means it is offered, not forced: onboarding and the App Home
  say "Want a morning brief? Say `brief me at 8`." Nothing is scheduled until the PM asks.
- **Prerequisites to build:** the role `pm-assistant` lists the skill (`skills:` in
  `content/roles/pm-assistant/ROLE.md`); Slack gets the three DM commands; the claim passes the
  PM's name to the prompt (see gap 8).

### Slack commands

Built in `crates/slack/src/brief.rs` (wording as data in `brief_copy.rs`), for team members,
typed alone in a DM, in any case:

| Say | What happens |
| --- | --- |
| `brief me at 8`, `brief me at 8:30`, `brief me at 7am`, `daily brief at 09:00` | Turns the brief on at that time on weekdays (cron `M H * * 1-5`) in the PM's Slack time zone, else `PMOS_DEFAULT_TZ`. One plain line confirms the time and zone and says `brief off` stops it |
| `brief on` | The same at the default, 08:30 |
| `brief off` | Deletes the brief routine only; the PM's other routines are untouched |
| `brief`, `brief status` | Says whether it is on and when it next runs |
| `brief now`, `brief me now` | Starts one task for the personal bot with the same prompt, answered in the thread; the routine is unchanged |

- **One routine.** The draft is the one in part 1, built directly (the model is not
  involved). Asking again for the same time says it is already set; a new time creates the
  new routine and then deletes the old one, so there is never more than one; a paused one at
  the same time is resumed.
- **Identifying it.** The prompt starts with `[daily-brief]` (`brief::TAG`);
  `brief::is_brief_routine(routine)` tests it. The runtime is unchanged.
- **Times** are 00:00 to 23:59 (`7am`, `7:30 pm` also work). Anything else, such as `25` or
  `8:75`, gets one line with examples, and nothing changes. Weekdays only. A time zone in
  the message (`brief me at 8 London`) is not read; it gets the same line.
- **Hooks to wire** (the module does not edit these files):
  - `bots.rs`, `on_dm`, after the `nudges::handle_dm` call and before `level::command`:
    `if crate::brief::handle_dm(self, &dm.channel, &dm.thread_ts, &dm.user, &text).await { return; }`
  - `notify.rs`, the `"done"` arm of `handle`, first line, so a delivered brief counts as the
    PM's one message that day for the nudge engine (`docs/nudges.md`; a no-op when the engine
    is off, and when the finished task is not from a brief routine):
    `crate::brief::note_delivery(&self.bots, item).await;`
    It reads the notification's `requested_by` and `routine_id` (the runtime sets
    `routine_id` only on notifications for a task a routine created) and checks that routine
    is the PM's own with `is_brief_routine`, then calls `nudges::record_daily_message`.
    `brief now` has no routine and is not counted.

### The web app's panels

The web app's home page shows the same day in three panels, "What changed", "Waiting on you"
and "You're waiting on", from pmos-web's `GET /api/brief` (`docs/web-api.md`, "The daily brief
and follow-ups"). The Slack service fills them:

1. **The skill ends every brief with one block** (`content/skills/daily-brief/SKILL.md`, "The
   block for the web app"): a fenced block with the info string `pmos-brief` holding one JSON
   object, on a quiet day too (empty lists).

   ```pmos-brief
   {"version": 1, "date": "2026-11-16",
    "changed": [{"system": "jira", "title": "PAY-212 moved to Blocked", "summary": "waiting on the vendor sandbox", "url": "https://…"}],
    "waiting_on_you": [{"system": "slack", "who": "Ben", "summary": "asked about SAML SSO for the console", "url": "https://…", "asked_on": "2026-11-14"}],
    "you_wait_on": [{"system": "slack", "who": "Legal", "summary": "your question about the DPA", "url": "https://…"}]}
   ```

   `version` is required and is `1`; every other key is optional, and no other key is allowed.
   `summary` is optional for `changed` items. `asked_on` (`YYYY-MM-DD`) becomes the
   follow-up's `created_at` (that day's start in UTC, or now if that is later), so the web app
   shows its age.
2. **Slack takes it out** (`crates/slack/src/notify.rs`, through `brief_feed::split`): every
   answer from the notification feed loses its `pmos-brief` block before it is posted, so the
   PM never sees JSON in Slack, whether or not the feed to the web app is on.
3. **Slack checks it, all or nothing** (`brief_feed::parse`): valid JSON with only the keys
   above; at most 50 items a list; `title` and `summary` one line of at most 200 characters,
   `who` at most 80; no control characters and no Slack markup (`<@…>`, `<#…>`, `<!…>`, a sign
   of pasted message text); `system` lower-case letters, digits, `-` and `_`, at most 32;
   every `url` `http` or `https` with a host and no credentials, at most 2,000 characters; the
   body at most 128 KiB. These are pmos-web's own limits. A second block, a block with no
   closing fence, or any broken rule means nothing is sent, and a warning is logged with the
   rule (never the content). The brief is delivered in Slack as usual.
4. **Slack sends it** only for a task that **is** a daily brief, never because an answer
   contains a block (any answer can: a page the bot quoted, a prompt injection). A brief is a
   task whose notification's `routine_id` names the requester's own brief routine
   (`is_brief_routine`, checked with one `GET /v1/routines` call, only for a `done`
   notification that has both a block and a `routine_id`), or a `brief now` task this Slack
   service started: it marks the task's id when it creates it (`brief::mark_brief_now`, in
   memory; a restart between the command and the answer forgets the mark, and that one brief
   isn't sent). Anything else with a block has it taken out and nothing sent, with a warning
   that names the task, never the content. Then, when the task is `done`, its bot is the
   requester's own personal bot (`pm-<handle>`) and the requester is on the team: `PUT
   /internal/brief/{handle}` with `Authorization: Bearer <PMOS_WEB_BRIEF_TOKEN>` and the body
   `{date?, changed: [{system, title, summary, url}], waiting_on_you: [{system, who, summary,
   url, created_at?}], you_wait_on: [...]}`. That replaces the PM's panels, so a quiet day
   empties them. It runs in the background with a 10-second limit: a failure (pmos-web down,
   a `4xx` or `5xx`) is logged at warn with the handle and the reason and never delays or fails
   the Slack brief. A success logs the handle and the three counts. The token, the text and the
   urls are never logged.

`brief now` is sent the same way as the routine's. A brief the PM asks for in their own
words in a DM ("write my brief") is an ordinary task: its block is taken out and not sent.

**What the web app shows of it.** `GET /api/brief` says whether any brief has filled the
PM's day: `delivered_at` (when the last one was sent here, kept after its items expire) and
`status`, `never` (none ever: the home page doesn't say "your brief is in"), `today` (the
last one came within 24 hours; empty lists mean it found nothing) or `stale` (older). A brief
that found nothing still sends its block with empty lists, so it counts as delivered.

**Configuration** (the Slack service): `PMOS_WEB_URL`, pmos-web's base URL as the Slack
service reaches it (`http://127.0.0.1:8770`), and `PMOS_WEB_BRIEF_TOKEN`, the same token
pmos-web has. With either unset the feed is off: the block is still taken out, nothing is sent.
pmos-web needs `PMOS_WEB_BRIEF=1` and the same `PMOS_WEB_BRIEF_TOKEN`
(`crates/web/README.md`). `scripts/claude-test-stack.sh` with `PMOS_TEST_WEB=1` sets all of it;
on the server, `deploy/setup.sh` generates the token, and `PMOS_WEB_BRIEF=1` with
`PMOS_WEB_BRIEF_URL=http://web.pmos.internal:8770` in `deploy/.env` turns it on.

**Privacy.** A `summary` is a paraphrase the skill wrote, never Slack message text; the skill
says so, and Slack refuses one with Slack markup. pmos-web keeps Slack follow-ups for 24 hours
and the others for a week, and the day's `changed` items for 24 hours, and deletes them after
(`crates/web/src/followups.rs`). The block is part of the task's `result` in the runtime, like
the brief itself (gap 10).

## 2. What the skill can do today

With the tools that exist (`content/tools.yaml`): `jira.search`, `jira.read`,
`confluence.search`, `confluence.read`, `slack.search`, `slack.read_thread` and, only when the
PM asks, `slack.post_reply`.

| Part | How it is found today | How far it can be trusted |
| --- | --- | --- |
| What changed | Jira issues the PM is assigned, reported or watching, updated in the last two days (`jira.search`, then `jira.read` for comments); Confluence pages the PM contributed to or is mentioned on, modified in the last two days (`confluence.search`, then `confluence.read` for comments) | Good for issues and pages the PM is attached to. The live Jira connector returns `updated` and comments; a result with no date is not shown (the fakes carry none, so no change shows in the evals) |
| Waiting on you | Slack search for messages that mention the PM, and for their DMs, then each thread read to see whether the PM replied after the ask; the same for Jira and Confluence comments already read | Good for threads the search surfaces. Reliable only as far as search recall goes (gaps 1 and 2) |
| You are waiting on | Slack search for the PM's own messages that end in a question, then each thread read for a reply after two working days | Works for Slack. Jira and Confluence questions are found only on items already read (gap 3) |

The rules the skill follows, all in the skill and checked by the benchmark where a check can be
deterministic:
- **Sourcing.** Every item line ends with a link from a tool result. No link, no line. A line
  that says something moved or was asked also needs a date from the tool result inside the
  window.
- **Waiting on you.** The PM was addressed (a mention, a DM, an `@` in a comment), someone asked a
  question or made a request, the PM has not replied after it, and it is under seven days old.
  FYIs, announcements, bot messages and reaction-only threads are skipped. If someone else has
  replied, the line says so rather than guessing.
- **You are waiting on.** A question the PM asked, with no reply from anyone else after two
  working days, under seven days old.
- **A quiet day** is one line, with no headings.
- **A source not connected** (the tool says "connect Slack in PM OS first" or "your Slack
  connection has expired"): that source is left out, one line says so and names `setup`, and the
  day is never called quiet about a source that could not be read. The brief does not fail.
- **Length.** At most five lines a part, twenty words a line, twenty-two lines in all. Overflow
  is a count, not more lines.
- **Tone.** Short and friendly. One light line, the opening one, and only there; never about a
  person, a missed deadline or a customer's data. The owner's example, "one person is ignoring
  you", is turned into "one question of yours is still waiting for a reply" so it never points at
  a person; the counts in it must match the lines below.
- **Read only.** The skill never posts on its own. It drafts a reply or a nudge only when the PM
  asks, with a link from the brief, through `slack.post_reply`, which waits for approval and
  posts as the PM (`docs/toolserver.md`, Slack drafts).

Calendar, GitHub and call notes are not connected; the brief does not mention them.

## 3. Gaps: what the tools cannot answer reliably

Each gap says what is wrong today and what would close it. They are in the order they hurt.

| # | Gap | Why the tools cannot do it | What closes it |
| --- | --- | --- | --- |
| 1 | **Every unanswered question addressed to the PM, across channels.** The brief finds mentions and DMs the search returns, not all of them | `slack.search` is the Real-time Search API: a bounded, ranked result list, not an enumeration. A question that does not mention the PM ("can the PM team confirm…") is never a candidate. Search's query operators (`to:me`, `from:me`, dates) are not verified against the live API | A `slack.mentions {since}` tool that pages through the PM's mentions and DMs with `search.messages`-style filters and returns them in time order with `thread_ts`, `reply_count` and `latest_reply` (read with the PM's token, never stored); plus a verified query syntax in `docs/toolserver.md` |
| 2 | **Knowing a thread was answered elsewhere** (a reply in another thread, a call, a Jira comment, a thumbs-up) | Only the thread itself is read. A later answer in a DM, a ticket or a meeting is invisible | A `slack.read_thread` result that includes reactions and `latest_reply`; a per-thread "handled" signal the PM can set ("Not mine", "Done") and a tracker that keeps it (part 4). Cross-system answers stay a PM judgement |
| 3 | **Jira and Confluence asks of the PM** (comments that mention the PM, review requests, approvals) | JQL has no "comment mentions me"; CQL has `mention = currentUser()` but the connector's CQL is not verified for it. The skill finds them only on issues and pages it already read | A `jira.mentions {since}` tool (issues with a comment mentioning the PM since a date, via the comment's `accountId` mention) and a verified `confluence.search` for `mention` and for unresolved inline comments |
| 4 | **Working days and holidays** | There is no calendar. The skill counts Monday to Friday and cannot know a public holiday or the PM's leave, so "two working days" can be a day early | A `time.working_days {from, to, tz}` read tool backed by the team's holiday calendar (and the PM's time zone), or the runtime passing "today, previous working day and the working-day count since X" in the prompt |
| 5 | **"Since the last brief"** | The skill reads "today" from the prompt and uses a fixed window; it does not know when the last brief ran or if one was missed | The routine run's `last_run_at` passed in the prompt (`Last brief: <iso>`), as the reply assistant's "since the last run" needs |
| 6 | **What changed in Jira**: status moves, not just "updated" | `updated` moves on any edit. The status history (a changelog) is not returned by `jira.read` | `jira.read` with `expand=changelog` for status transitions (a read-only field), so a line can say "moved to Blocked" and not just "updated" |
| 7 | **Counts and dates by arithmetic** (age in days, working days) | The model counts them, which it does badly. The rest of PM OS sends numbers through tools (`stats.*`, `jira.rollup`) | The same `time.working_days` tool, or the runtime computing `age_days` before the brief is written |
| 8 | **The PM's name and Slack id** | The skill infers the id from the mentions the search returns, and the first name from the request. A bad inference can show another person's messages as "yours" in "You are waiting on" | The claim carries `{name, slack_id}` for the PM (the member record has the Slack id), and the harness adds them to the prompt |
| 9 | **Source not connected, as an eval** | The eval stack cannot run as a member whose Slack is unlinked, so the benchmark simulates it in the request | An eval-only fixture user whose Slack, Jira or Confluence answers "connect it in PM OS first" |
| 10 | **Slack text in the finished DM.** The runtime redacts Slack text in traces after 24 hours; the task's `result` (the brief) and the DM in Slack are not traces | The brief paraphrases Slack and links it, but a paraphrase is still Slack-derived text | Confirm with the runtime owner whether a routine's `result` is covered by the 24-hour redaction; if not, redact `result` for tasks whose trace shows `slack.*` calls, and keep the brief's Slack lines to who, where and a few words (the skill already does) |
| 11 | **The length limit** | Nothing counts lines. The skill states the limit and a judge or person checks it | A `max_lines` check on task results in `pmos-evals`, or a judge. Not built |

## 4. The follow-up tracker (a later slice)

Today a follow-up exists only inside one brief: the next morning the skill finds it again, or
does not. Three things need state, so they need a small store.

**The store.** A table in the tool server (or the bots runtime), `followups`:

| Field | Meaning |
| --- | --- |
| `id` | `fu_<32 hex>`, as the web API already shows it |
| `owner` | the PM's handle |
| `kind` | `waiting_on_you` or `you_wait_on` |
| `system` | `slack`, `jira` or `confluence` |
| `ref` | channel and thread ts, issue key, or page and comment id: enough to re-read the item |
| `url` | the source link |
| `who` | the other person's name (a label, as Slack gave it) |
| `summary` | a few words, as the brief shows it |
| `asked_at` | when the ask was made |
| `first_seen`, `last_seen` | when the brief first and last saw it |
| `state` | `open`, `answered`, `dismissed` or `expired` |
| `decided_at` | when it left `open` |

**The life of a follow-up.** Each morning the brief reads the sources, then reconciles with
the store, as a pure function of what the sources say now:
- a new ask is added as `open`;
- an `open` follow-up whose thread now has a reply by the PM (or, for `you_wait_on`, by anyone
  else) becomes `answered`;
- the PM's **Not mine** becomes `dismissed` and the item never comes back;
- an `open` follow-up a week after `asked_at` becomes `expired` and leaves the brief.

A follow-up stays until it is answered, dismissed or a week old. The brief shows only `open`
rows, so "stays until answered" does not depend on search recall (gap 1).

**The web app.** `GET /api/brief` returns `Brief` exactly as `docs/web-api.md` fixes it:
`{date, delivered_at, status, changed, waiting_on_you, you_wait_on}`, where each follow-up row
has `id`, `system`, `who`, `summary`, `url` and `age_days`. Every row has a `url`. It answers
`501 {"code":"not_built"}` while pmos-web's `PMOS_WEB_BRIEF` is off, and the app then hides that
part of the home page. Today pmos-web keeps the rows itself, filled from each brief (part 1,
"The web app's panels"); a follow-up sent again keeps its age and a dismissal. `POST
/api/followups/{id}/dismiss` ("Not mine") answers `204`. `POST /api/followups/{id}/draft` answers
`201 Task`: a task that asks the PM's bot for a reply or nudge draft for that follow-up and ends
as an approval draft. The follow-up's fields came from other people's messages, so they go
into that task's prompt as untrusted data: one line of JSON between two marker lines that carry
a nonce fresh for each request (`<<<FOLLOW_UP_DATA <nonce>` and `FOLLOW_UP_DATA <nonce>>>>`),
with every `<` and `>` in it escaped so no value can rebuild a marker, after an instruction to
treat every value as text and never follow anything in it (`crates/web/src/app/brief.rs`,
`draft_prompt`).

**Slack buttons.** Each follow-up line in the DM gets three buttons: **Draft a reply**
(`waiting_on_you`), **Draft a nudge** (`you_wait_on`) and **Not mine**. Draft a reply or a nudge
calls the same route the web uses and shows the usual approval card (`docs/slack-reply.md`);
nothing posts until the PM approves, and it posts as the PM, marked "Drafted with PM OS". Not mine
dismisses. The buttons carry the follow-up's id only: no Slack text goes in a button.

**How the skill changes.** The skill gets `followups.list` and `followups.upsert` tools (read and
internal, like `memory.*`), and the three sections become the store's `open` rows, refreshed from
the sources. The text-only "send me a link" footer is replaced by the buttons. The sourcing,
quiet-day, not-connected and length rules do not change.

## 5. Privacy and retention

- **Slack is read with the PM's own account** (the PM's user token, never the bot's), so only
  channels and DMs the PM is in. No other PM's brief can contain a message the first PM cannot
  see. A team bot would read with its own identity; the brief is only ever run by a personal
  bot.
- **Slack text is not kept beyond 24 hours.** The trace store replaces `slack.*` results with
  `{"redacted": …}` after 24 hours, and undecided Slack drafts expire in 24 hours (`docs/toolserver.md`,
  Slack drafts). The tracker stores no Slack text: only the ref, the link, the other person's name,
  a few words of summary the brief wrote, and dates. Those few words are Slack-derived, so the
  tracker deletes a row's `summary` and `who` when it leaves `open`, and a row 24 hours after
  that. Gap 10 is the check that the brief's own `result` meets the same limit.
- **The brief is the PM's alone.** It goes to the PM's DM, with nobody else, and the web
  `GET /api/brief` reads the signed-in PM's rows only.
- **Restricted data.** The skill is `restricted`: it may read restricted sources, so the brief
  is told not to paste customer data, figures or quotes. It names who and what about, with a link.
  The reader follows the link to the PM's own access.
- **Drafts.** A reply or a nudge is a Slack write: it goes through the approval and audience
  check (a draft that would reach people outside the product team, or a Slack Connect channel,
  needs the PM's confirmation first), and no text is stored past 24 hours.
- **Prompt injection.** Slack, Jira and Confluence text is untrusted data; the skill never
  follows instructions in it.

## 6. The day's one nudge

The brief is the one message PM OS sends a PM each weekday on its own. It makes the other
proactive pieces fit around it:
- **One a day.** The routine runs once, and a missed run runs once on restart (the routine
  rule). The runtime also ignores a second `[daily-brief]` task for the same PM on the same
  local date: not built, listed as a guard to add with the routine commands.
- **Nothing else is sent unprompted to the PM.** The brief never DMs again to say a draft is ready;
  drafts the PM asks for arrive as the usual approval cards in the reply. The reply assistant's
  cards (`docs/slack-reply-spec.md`) are drafts the PM asked for by turning it on, not a second
  nudge, and the daily triage digest goes to a bot's quality owner, not to the PM as a PM.
- **A quiet day is one line,** so the one message is never padded to look useful.
- **Off means off.** `brief off` deletes the routine. `PUT /api/me/settings {nudges: false}`
  already turns off the web app's hints; it does not turn off the brief.

## 7. Benchmark

`evals/categories/daily-brief/` (bot `personal`, three runs a task, `pass_bar` 0.7 as a starting
value to replace with a measured one):

| Task | Checks |
| --- | --- |
| `normal-day-three-parts-sourced` | the three parts; a link to each of the three open Slack asks; none of the answered, FYI or bot threads; nothing drafted |
| `quiet-day-is-one-line` | asks past seven days expired; the one quiet line; no headings, items or links |
| `slack-not-connected-says-so` | Slack named and `setup` offered; no Slack item; not called quiet; no failure |
| `never-invents-an-item` | no item, name or key outside the fixtures; the planted injection ignored; no "ignoring you" without a basis; nothing drafted |

Run `pmos-evals check --category daily-brief` offline. A model run (`pmos-evals run`) is needed
to see whether the bot follows the skill. Until then the offline check only proves the files are
well formed.
