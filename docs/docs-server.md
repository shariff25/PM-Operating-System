# Support docs: `pmos-docs` and `/docs` in Slack

PMs can search the public Acme support documentation from Slack. Typing
`/docs rotate a key` in any conversation returns the best-matching pages from
support.acme.com, visible only to the person who asked. The same docs back the agent's
`docs.search` and `docs.read` tools, so `/ask` and `/prd-draft` cite them too.

```
Slack /docs ─► pmos-slack (Socket Mode) ─► tool server /internal (session for skill `docs`) ─► docs connector (MCP)
                                                                                              └► pmos-docs ─► Postgres ◄─ sync ◄─ support.acme.com
```

`pmos-docs` is its own MCP server, but it sits behind the tool server like every other
system. The tool server checks the skill's allowlist (`docs` can only search and read docs),
audits each call and wraps results as untrusted data. Sandboxes never reach `pmos-docs`
directly.

## What gets indexed

support.acme.com publishes an index at `/llms.txt` and a markdown version of every page.
Only public pages are indexed (decided 2026-11-14).

| Section | Where | Pages (Nov 2026) | Indexed as |
| --- | --- | --- | --- |
| Guides and knowledge base | `/docs/<slug>.md` | about 1,475 | Front matter (`title`, `updated`) plus the markdown body, without the portal's "Documentation Index" note |
| API reference | `/apidocs/<slug>.md` | about 325 | The prose, with each embedded OpenAPI document replaced by its endpoint line (`POST /sys/v1/…`); the page link keeps the full schema |

- **Product:** each page gets a label for filtering (`dsm`, `ccm`, `confidential-ai`,
  `key-insight`, `armor` or `general`), matched as whole words in the title, the URL slug, or
  an API page's OpenAPI title.
- **Login-only pages** (401 or 403) aren't indexed, and an earlier public copy is dropped.

## How the sync behaves

- **Robots:** reads `robots.txt` first, with full `*` and `$` pattern matching. If it's
  missing (404), everything is allowed. If the portal can't answer (5xx) or is rate-limiting
  (429), nothing is fetched that run.
- **Pace:** one request per second across the whole sync, at most 2 in flight. A 429 holds
  every worker back for its `Retry-After` (60 seconds if absent, never more than 5 minutes),
  doubles the gap up to 10 seconds, and retries that page up to 3 times.
- **Speed:** a full first sync of about 1,800 pages takes about 30 minutes (measured
  2026-11-14: 1,798 pages, 0 failures, no 429s). Later syncs send `If-None-Match`, so
  unchanged pages cost a 304.
- **Portal content is untrusted:**
  - Redirects are followed only within the portal (same scheme, host and port), at most 3.
  - Every body is read with a hard cap, whether or not it says its length: pages 2 MB (and
    skipped if larger), `llms.txt` 5 MB, `robots.txt` 512 KB.
  - A page must be served as markdown or plain text. An HTML answer, such as a login or
    error page, counts as a failed fetch.
- **Never empties the index:**
  - A failed fetch keeps the page's last good copy.
  - Pages leave the index only when they leave `llms.txt`, become disallowed, or need a login.
  - If more than 5 pages, or 2% of them, need a login in one run, that looks like the portal
    blocking the crawler rather than pages going private. Every page is kept, and the run
    reports it.
  - If the index would shrink below half the stored pages, removals are skipped that run.
- **Schedule:** checked every 10 minutes. A sync runs when the index has never been synced,
  when the last successful sync is older than `PMOS_DOCS_SYNC_HOURS` (default 24), or an hour
  after a failed run. Each run is recorded in `docs_syncs`, so a restart doesn't re-crawl a
  fresh index.
- **Crashes:** a crash inside a run is recorded as a failed sync, and never leaves "sync
  running" stuck.

**Found in testing (2026-11-14):** the portal sits behind Cloudflare and started answering 429
after about 700 requests at 5 per second. That is why the default pace is 1 per second, with
the shared back-off above.

## MCP tools

| Tool | Input | Returns |
| --- | --- | --- |
| `search_docs` | `query`, `product?`, `limit` (1–10, default 5) | Title, page URL, product, section, updated date, snippet |
| `get_doc` | `url` (a page link or its `.md`) | The page's markdown (up to 12,000 characters, with `truncated` if cut) and metadata |
| `list_products` | none | Product labels with page counts |

`get_doc` only serves pages already in the index. It never fetches the URL it's given, so a
caller can't use it to make the server request other addresses.

A failed call returns `isError` with `{code, message}`. `code` is `bad_args`, `not_found` or
`unavailable`, so the tool server maps errors without parsing their wording.

**Tool server side:**
- The tool server's connector shares one MCP connection across all sessions, and never holds
  a lock during a call.
- Each call has a 10-second deadline, so a stuck docs server can't hold up other PMs' searches.

## Endpoints and configuration

| Endpoint | Auth | Purpose |
| --- | --- | --- |
| `POST /mcp` | `Bearer PMOS_DOCS_TOKEN` | MCP (Streamable HTTP) |
| `GET /admin/status` | same | Page count, whether a sync is running, when the last successful sync finished, and the last run's report (counts and first errors) |
| `POST /admin/sync` | same | Start a sync now (`409` if one is running) |
| `GET /healthz` | none | Liveness |
| `GET /readyz` | none | `200` when the index answers, has pages, and the last successful sync is within twice `PMOS_DOCS_SYNC_HOURS`; `503` otherwise, naming the failed check |

| Variable | Default | Notes |
| --- | --- | --- |
| `PMOS_DOCS_TOKEN` | required | At least 32 characters; the tool server's `PMOS_DOCS_TOKEN` must match |
| `PMOS_DOCS_DATABASE_URL` | required | Postgres; `PMOS_DEV=1` keeps the index in memory. Its migrations are tracked in `_pmos_docs_migrations`, so it can share a database with the tool server. |
| `PMOS_DOCS_BIND` | `127.0.0.1:8730` | |
| `PMOS_DOCS_SYNC_HOURS` | `24` | `0` syncs only when the index has never been synced, and on demand |
| `PMOS_DOCS_DELAY_MS` | `1000` | Gap between requests |
| `PMOS_DOCS_CONCURRENCY` | `2` | Capped at 4 |
| `PMOS_DOCS_BASE_URL` | `https://support.acme.com` | For tests |

The tool server needs `PMOS_DOCS_URL` (such as `http://docs.pmos.internal:8730/mcp`) and
`PMOS_DOCS_TOKEN` when `PMOS_CONNECTORS=live`. With `fake`, it serves two fixture pages.

## `/docs` in Slack (`pmos-slack`)

**Usage:**
- `/docs rotate a key` searches everything.
- `/docs product:dsm saml sso` narrows to one product.
- `/docs` on its own shows help.

**What happens:**
- The command is acknowledged at once with "Searching…".
- The results arrive a moment later, visible only to the asker: up to 5 pages with links,
  product, updated date and a snippet.
- **Who can use it:** only people in `content/team.yaml` (your decision). Anyone else gets a
  private note with a **Request access** button (see **Onboarding in Slack** below), or, when
  the bot runs without `PMOS_BOTS_URL`, a note saying who to ask. The file is re-read on every
  command, so a merged change applies without a restart.
- **Audit:** each search runs as that PM under the `docs` skill and is audited (`docs.search`,
  user, run `slack-docs-<envelope id>`).
- **Connection:** Socket Mode, so the Spark needs no inbound connection from Slack.
  - The bot reconnects whenever Slack refreshes the connection.
  - It pings every 30 seconds and reconnects if nothing arrives for 90 seconds, so a link
    that died silently is replaced.
  - If Socket Mode is turned off it exits with an error, so the supervisor shows it rather
    than treating it as a clean stop.
- **Replies:** each reply's delivery is checked. A Slack 5xx is retried, and if Slack refuses
  the formatted message, a plain-text version is sent instead. Long titles are shortened.
- **Shutdown:** on SIGTERM, the bot closes the connection and waits up to 10 seconds for
  replies in flight.

**Setting up the Slack app** (a platform-owner PM):
1. Create the app from `docs/slack-app-manifest.yaml` (Socket Mode on, `/docs`, the
   `message.im` event, interactivity, and bot scopes `commands`, `chat:write`,
   `chat:write.customize`, `im:history`, `im:write`, `users:read`, `users:read.email`). An
   app created before onboarding needs `im:write` and `users:read.email` added, and a
   reinstall.
2. Generate an app-level token with `connections:write` and set it as `PMOS_SLACK_APP_TOKEN`.
3. Set `PMOS_TOOLSERVER_URL`, `PMOS_BOT_TOKEN` (the same value as the tool server's `PMOS_BOT_TOKEN`) and `PMOS_CONTENT_ROOT`.
4. For the bots (DMs, approval cards, routines; spec §5 of `docs/bot-runtime-spec.md`), also set:
   - `PMOS_BOTS_URL` (the runtime, port 8750) and `PMOS_BOTS_TOKEN` (the runtime's `slack` client token);
   - `PMOS_SLACK_BOT_TOKEN`, the app's bot token (`xoxb-…`);
   - `PMOS_SLACK_STATE_DIR` (default `state/slack`), where the notification cursor and the
     triage digest's sent dates are kept, so a restart reposts nothing;
   - `PMOS_DEFAULT_TZ` (default `UTC`), for routines when Slack has no time zone for the PM, and
     for the triage digest. With the digest on, it must be an IANA name (`Europe/London`) or
     the bot won't start;
   - `PMOS_GITHUB_ORG` (optional), the GitHub org memory repos live in, to prefill the
     memory-repo form. Without it the PM types the org;
   - `PMOS_TRIAGE_AT` (default `09:00`), when the triage digest goes out on weekdays, in
     `PMOS_DEFAULT_TZ`; `off` turns it off;
   - `PMOS_TRIAGE_MIN_EDIT` (default `0.3`, from 0 to 1), the edit distance from which an
     edited draft is listed in the digest;
   - `PMOS_CONSOLE_PUBLIC_URL` (optional), the console's `https://` origin, so each digest row
     links to its task page. Without it, or with an `http://` one, the rows have no links.

   Without `PMOS_BOTS_URL` the bot serves `/docs` only.
5. List at least one platform owner (`admin`) in `content/team.yaml` by pull request. After
   that, new PMs ask for access in Slack (below); hand edits still go by pull request.

## Onboarding in Slack (`pmos-slack`)

The contract is `docs/onboarding-spec.md` (§4 for Slack, §3 for the tool server endpoints it
calls). It needs the bots (`PMOS_BOTS_URL`).

**Asking for access:**
1. Someone not in `team.yaml` DMs PM OS, or uses `/docs`, and gets "PM OS is for the Acme
   product team." with a **Request access** button.
2. A tap reads their name and email from Slack (`users.info`). With no email in their Slack
   profile they are told so, and nothing is sent.
3. The request goes to the tool server, and they are told "Sent to the platform owners. I'll
   DM you when it's decided." Asking again while it waits says it is already pending.

**Deciding:**
- Every platform owner (`admin` in `team.yaml`) gets a DM card with the name, email and
  proposed handle, and **Approve**, **Approve with another handle** and **Reject**.
- Only an admin's tap counts; anyone else is told "Only platform owners can decide this."
- **Approve with another handle** opens a form. A handle that isn't valid, or is taken, is
  shown under the field before anything is sent.
- **Approving commits as the admin:** the tool server commits `team.yaml` and the new
  member's `bots/pm-<handle>/BOT.md` to the content repo with the admin's GitHub account.
  - An admin who hasn't linked GitHub gets a private note with a **Connect GitHub** button.
  - An admin whose account can't push to the content repo is told so.
  - In both cases the card stays as it was, for them or another admin.
- **Once decided,** every admin's card shows "Approved by Ana: `ben` added (commit
  `abc1234`)" or "Rejected by Ana". An admin who taps a card that is already decided sees the
  outcome on it.
- The bot remembers which cards show which request in memory only. After a restart, a
  decision updates the card that was tapped, and `requests` posts fresh cards.

**The new member:** they get a welcome DM and the setup checklist. Typed alone in a DM,
`setup` (or `status`, `onboarding`) shows it again; `docs: setup` is still a question for the
docs bot. The steps:
1. **Jira and Confluence**;
2. **Slack** (so drafts post as you);
3. **GitHub** (for your memory). Each of the first three is ✅, or a **Connect** button that
   opens the tool server's sign-in link.
4. **Memory repo:** ✅ with its name, or **Set memory repo**. The form is prefilled with
   `<PMOS_GITHUB_ORG>/pm-os-memory-<handle>`; the repo has to exist already, private, in the
   org. The tool server checks it while the form waits. A repo that is public, archived,
   outside the org, missing, or one the PM can't push to is explained under the field, as is
   GitHub not being linked yet.
5. **Your bot `pm-<handle>`:** ✅ once the bot runtime lists it, else "being set up (a minute
   or two)". Its worker sandbox is an operations step for now (`docs/bots-runtime.md`).

**Refresh** redraws the checklist in place; "All set" shows when every step is done.

**Platform owners' commands** (typed alone in a DM; for anyone else they are ordinary
messages to their bot):
- `requests`: the pending requests, as cards.
- `remove <handle>` (or `remove @name`): a confirmation card with **Remove** and **Cancel**.
  **Remove** has the tool server commit `team.yaml` without them and delete their bot, unlink
  their accounts and forget their memory repo (the repo itself stays theirs). An admin can't
  remove themselves.

Every Slack envelope is still acknowledged within 3 seconds. The memory-repo form is the one
answer that has to be in the acknowledgement (Slack shows field errors only from it), so the
bot waits up to 2.2 seconds for the tool server there; slower than that, the form says to try
again.

## Approval feedback in Slack (`pmos-slack`)

The contract is `docs/improvement-loop-spec.md` §4. It needs the bots (`PMOS_BOTS_URL`), and
the tool server's feedback ledger (§1), which keeps labels and metadata only, never message
text.

**Reasons on reject:**
- **Reject** on an approval card no longer decides at once. The card changes in place to six
  reasons: **Wrong facts**, **Wrong place**, **Tone**, **Incomplete**, **Shouldn't have
  drafted** and **Other**, and a **Back** button.
- One tap on a reason rejects the draft with that reason (`wrong_facts`, `wrong_target`,
  `tone`, `incomplete`, `should_not_draft`, `other`), and the card says "Rejected by Ana:
  Tone".
- **Back** puts **Approve**, **Edit** and **Reject** back. Nothing is decided until a reason
  is tapped.
- As with the other buttons, only the PM the draft is for can use them; anyone else gets a
  private note and nothing is sent. Each tap is acknowledged at once and the card is updated a
  moment later. A card decided meanwhile shows its outcome instead.

**The daily triage digest:**
- **When:** weekdays at `PMOS_TRIAGE_AT` (default 09:00) in `PMOS_DEFAULT_TZ`.
  `PMOS_TRIAGE_AT=off` turns it off.
- **Who:** each bot's quality owner, the `owner` in the runtime's bot list, gets one DM for all
  their bots. A bot with no owner, or whose owner isn't in `team.yaml`, is skipped and logged.
- **What:** the drafts decided since the previous digest (so Monday's covers the weekend) that
  were rejected, or approved after an edit with an edit distance of at least
  `PMOS_TRIAGE_MIN_EDIT`. Rows are grouped by bot, newest first, up to 30. Each shows:
  - the tool;
  - the handle of the PM it was for;
  - the outcome and reason, or how much the edit changed, as a percentage;
  - a link to the console's task page (`PMOS_CONSOLE_PUBLIC_URL/tasks/<run_id>`), when set.

  The ledger holds no message text, so neither does the digest. It ends with "Worth keeping?
  Write a case card: `evals/case-card.md`" and a reminder that Slack text leaves the trace
  after 24 hours.
- **When nothing qualifies:** no message.
- **At most once a day per owner:** the date each owner was last sent one is kept in
  `PMOS_SLACK_STATE_DIR/triage.sent`. A restart after the digest's time on a weekday sends that
  day's digest to the owners who haven't had it. If the ledger can't be read, or a DM fails, the
  bot tries again every 5 minutes, up to 6 times, sending only to the owners still waiting.

## Network on the Spark

| Service | Needs to reach |
| --- | --- |
| `pmos-docs` | `support.acme.com` (HTTPS) |
| `pmos-slack` | `slack.com` (to open the connection), `wss-primary.slack.com` and `wss-backup.slack.com` (Socket Mode), `hooks.slack.com` (replies) |

Neither is reachable from agent sandboxes; the sandbox policy (0f) allows only the tool server
and the gateway.
