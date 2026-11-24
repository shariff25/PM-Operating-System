# Public docs: `pmos-docs` and `/docs` in Slack

PMs can search your company's public product documentation from Slack. Typing
`/docs rotate a key` in any conversation returns the best-matching pages from your docs site
(`PMOS_DOCS_BASE_URL`), visible only to the person who asked. The same docs back the agent's
`docs.search` and `docs.read` tools, so `/ask` and `/prd-draft` cite them too.

```
Slack /docs ─► pmos-slack (Socket Mode) ─► tool server /internal (session for skill `docs`) ─► docs connector (MCP)
                                                                                              └► pmos-docs ─► Postgres ◄─ sync ◄─ your docs site
```

`pmos-docs` is its own MCP server, but it sits behind the tool server like every other
system. The tool server checks the skill's allowlist (`docs` can only search and read docs),
audits each call and wraps results as untrusted data. Sandboxes never reach `pmos-docs`
directly.

## What gets indexed

The docs site publishes an index at `/llms.txt` and a markdown version of every page (see
**Pointing the Docs bot at your docs** below). Only public pages are indexed.

| Section | Where | Indexed as |
| --- | --- | --- |
| Guides and knowledge base | `<prefix><slug>.md`, for each prefix in `PMOS_DOCS_PAGE_PREFIXES` | Front matter (`title`, `updated`) plus the markdown body, without a "Documentation Index" note if the site adds one |
| API reference | `<PMOS_DOCS_API_PREFIX><slug>.md`, if set | The prose, with each embedded OpenAPI document replaced by its endpoint line (`POST /v1/…`); the page link keeps the full schema |

- **Product:** each page gets a label for filtering, from the products file
  (`PMOS_DOCS_PRODUCTS`): the first product whose keywords appear in the page's title or URL
  path, else `general`. With no products file every page is `general`.
- **Login-only pages** (401 or 403) aren't indexed, and an earlier public copy is dropped.

## How the sync behaves

- **Robots:** reads `robots.txt` first, with full `*` and `$` pattern matching. If it's
  missing (404), everything is allowed. If the docs site can't answer (5xx) or is rate-limiting
  (429), nothing is fetched that run.
- **Pace:** one request per second across the whole sync, at most 2 in flight. A 429 holds
  every worker back for its `Retry-After` (60 seconds if absent, never more than 5 minutes),
  doubles the gap up to 10 seconds, and retries that page up to 3 times.
- **Speed:** at the default pace a full first sync takes about a second per page, so about
  30 minutes for 1,800 pages (measured 2026-11-14 against a real docs site: 1,798 pages, 0
  failures, no 429s). Later syncs send `If-None-Match`, so unchanged pages cost a 304.
- **Docs site content is untrusted:**
  - Redirects are followed only within the docs site (same scheme, host and port), at most 3.
  - Every body is read with a hard cap, whether or not it says its length: pages 2 MB (and
    skipped if larger), `llms.txt` 5 MB, `robots.txt` 512 KB.
  - A page must be served as markdown or plain text. An HTML answer, such as a login or
    error page, counts as a failed fetch.
- **Never empties the index:**
  - A failed fetch keeps the page's last good copy.
  - Pages leave the index only when they leave `llms.txt`, become disallowed, or need a login.
  - If more than 5 pages, or 2% of them, need a login in one run, that looks like the docs site
    blocking the crawler rather than pages going private. Every page is kept, and the run
    reports it.
  - If the index would shrink below half the stored pages, removals are skipped that run.
- **Schedule:** checked every 10 minutes. A sync runs when the index has never been synced,
  when the last successful sync is older than `PMOS_DOCS_SYNC_HOURS` (default 24), or an hour
  after a failed run. Each run is recorded in `docs_syncs`, so a restart doesn't re-crawl a
  fresh index.
- **Crashes:** a crash inside a run is recorded as a failed sync, and never leaves "sync
  running" stuck.

**Found in testing (2026-11-14):** a docs portal behind a CDN started answering 429 after
about 700 requests at 5 per second. That is why the default pace is 1 per second, with the
shared back-off above.

## MCP tools

| Tool | Input | Returns |
| --- | --- | --- |
| `search_docs` | `query`, `product?`, `limit` (1–10, default 5) | Title, page URL, product, section, updated date, snippet |
| `get_doc` | `url` (a page link or its `.md`) | The page's markdown (up to 12,000 characters, with `truncated` if cut) and metadata |
| `list_products` | none | The configured product ids, with page counts |

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
| `GET /readyz` | none | `200` when the index answers, has pages, and the last successful sync is within twice `PMOS_DOCS_SYNC_HOURS`; `503` otherwise, naming the failed check (`docs site not configured` when `PMOS_DOCS_BASE_URL` is unset) |

| Variable | Default | Notes |
| --- | --- | --- |
| `PMOS_DOCS_TOKEN` | required | At least 32 characters; the tool server's `PMOS_DOCS_TOKEN` must match |
| `PMOS_DOCS_DATABASE_URL` | required | Postgres; `PMOS_DEV=1` keeps the index in memory. Its migrations are tracked in `_pmos_docs_migrations`, so it can share a database with the tool server. |
| `PMOS_DOCS_BIND` | `127.0.0.1:8730` | |
| `PMOS_DOCS_SYNC_HOURS` | `24` | `0` syncs only when the index has never been synced, and on demand |
| `PMOS_DOCS_DELAY_MS` | `1000` | Gap between requests |
| `PMOS_DOCS_CONCURRENCY` | `2` | Capped at 4 |
| `PMOS_DOCS_BASE_URL` | none | Your docs site, `https://`; see below |
| `PMOS_DOCS_NAME` | `the public docs` | How tools and Slack name the site |
| `PMOS_DOCS_PAGE_PREFIXES` | `/` | URL path prefixes to index, comma-separated |
| `PMOS_DOCS_API_PREFIX` | none | The path prefix of API reference pages |
| `PMOS_DOCS_PRODUCTS` | none | Path to the products file |

The tool server needs `PMOS_DOCS_URL` (such as `http://docs.pmos.internal:8730/mcp`) and
`PMOS_DOCS_TOKEN` when `PMOS_CONNECTORS=live`. With `fake`, it serves two fixture pages for a
fictional product, Acme Vault, at `https://docs.example.com/docs/vault-saml-sso` and
`…/vault-key-rotation`.

## Pointing the Docs bot at your docs

PM OS ships with no docs site. You point it at your company's public docs with a few
settings in `deploy/.env`; the compose file passes them to the `docs` service.

**What the site must publish.** The crawler reads two kinds of file, both public over
`https://`:
- **An index at `<base>/llms.txt`** (the [llms.txt](https://llmstxt.org) convention). Every
  page to index is a markdown list line `- [Title](https://docs.example.com/docs/<slug>.md)`,
  optionally followed by `: one-line description`. Links must be on the same host as
  `PMOS_DOCS_BASE_URL`, end in `.md`, and sit under one of the page prefixes; nested paths
  such as `/docs/guides/sso.md` are fine (query strings, fragments, `..` and other hosts are
  skipped). `## ` headings group the lines;
  a heading containing "api" marks the lines under it as API reference.
- **A markdown version of each page** at that `.md` URL, served as markdown or plain text (an
  HTML answer counts as a failed fetch). Optional YAML front matter gives `title` and
  `updated` (an ISO 8601 date); without it the title comes from the index line. The page
  people open is the same URL without `.md`.
- **`robots.txt`** is honoured if present. Many hosted docs platforms publish `llms.txt` and
  `.md` pages already; check yours by opening `<base>/llms.txt` and one `.md` link from it.

**The settings** (`deploy/env.example` has them, with comments):

| Variable | Default | Meaning |
| --- | --- | --- |
| `PMOS_DOCS_BASE_URL` | none | The docs site to crawl, such as `https://docs.example.com`. Unset: nothing is crawled (below). |
| `PMOS_DOCS_NAME` | `the public docs` | How tools and Slack name the site, such as "the Acme Help Center". |
| `PMOS_DOCS_PAGE_PREFIXES` | `/` | Comma-separated URL path prefixes whose pages are indexed, such as `/docs/,/apidocs/`. |
| `PMOS_DOCS_API_PREFIX` | none | A path prefix whose pages are API reference (section `api`, OpenAPI documents stripped), such as `/apidocs/`. |
| `PMOS_DOCS_PRODUCTS` | none | Path to a YAML products file. In the deployed stack the repo's `config/` folder is mounted read-only at `/etc/pmos/config`, so this is `/etc/pmos/config/docs-products.yaml`. With none, every page is product `general`. |

**The products file** lets PMs and the Docs bot narrow a search to one product
(`/docs product:vault saml sso`). Copy `config/docs-products.example.yaml` to
`config/docs-products.yaml` and list your products:

```yaml
# A page belongs to the first product whose keywords appear in its title or URL path;
# otherwise it is `general`.
products:
  - id: vault
    name: Acme Vault
    keywords: [vault]
  - id: cloud
    name: Acme Cloud
    keywords: [cloud]
```

`docs.search` takes `product` as a plain string; an id that isn't in the file is refused
with `bad_args`, naming the configured ids. Changing the file needs a restart of the `docs`
service and a sync to relabel pages.

**With no site configured** (`PMOS_DOCS_BASE_URL` blank), the docs service still starts:
- the index stays empty and syncing is off; the log says "no docs site configured";
- `/readyz` answers `503` with `docs site not configured`, so `pmos-admin status` shows docs
  as not ready;
- `search_docs` returns no results with a note saying no docs site is configured, and the
  Docs bot tells the asker so instead of guessing.

**Checking it:**
1. Set the variables in `deploy/.env`, then `deploy/pmos-admin restart docs`.
2. `deploy/pmos-admin logs docs -f` shows the first sync: the index fetched, then pages.
   It starts within 10 minutes of the restart, as the index has never been synced.
3. `deploy/pmos-admin status` shows docs as ready once the first sync has stored pages.
   `GET /admin/status` (with `PMOS_DOCS_TOKEN`) gives the page count and the last run's
   report; `POST /admin/sync` starts a sync now.
4. In Slack, `/docs <something your docs cover>`, or `deploy/pmos-admin smoke <handle>` for
   one Docs-bot question through the whole stack.

(`pmos-admin reindex` reloads the *content* library after a merge; it doesn't touch the docs
index.)

## `/docs` in Slack (`pmos-slack`)

**Usage:**
- `/docs rotate a key` searches everything.
- `/docs product:vault saml sso` narrows to one product (an id from the products file).
- `/docs` on its own shows help.

**What happens:**
- The command is acknowledged at once with "Searching…".
- The results arrive a moment later, visible only to the asker: up to 5 pages with links,
  product, updated date and a snippet.
- **Who can use it:** only active users of PM OS, from the tool server's user database
  (`docs/user-database-spec.md`; the Slack service polls its directory every 10 seconds).
  Anyone else gets a private note with a **Request access** button (see **Onboarding in
  Slack** below), or, when the bot runs without `PMOS_BOTS_URL`, a note saying who to ask.
- **Audit:** each search runs as that PM under the `docs` skill and is audited (`docs.search`,
  user, run `slack-docs-<envelope id>`).
- **Connection:** Socket Mode, so PM OS's host needs no inbound connection from Slack.
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
   - `PMOS_SLACK_STATE_DIR` (default `state/slack`), where the notification cursor, the
     triage digest's sent dates and the reply cards' snoozes are kept, so a restart reposts
     nothing and loses no snooze;
   - `PMOS_DEFAULT_TZ` (default `UTC`), for routines (the reply assistant's too) and **Snooze**
     when Slack has no time zone for the PM, and for the triage digest. With the digest on, it must be an IANA name (`Europe/London`) or
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
5. Make at least one platform owner (`admin`) in the user database on the host
   (`deploy/pmos-admin users bootstrap-admin`, `docs/runbooks/users.md`). After that, new PMs
   ask for access in Slack (below), or an admin invites them in the web app.

## Onboarding in Slack (`pmos-slack`)

The contract is `docs/onboarding-spec.md` (§4 for Slack, §3 for the tool server endpoints it
calls). It needs the bots (`PMOS_BOTS_URL`).

**Asking for access:**
1. Someone not in the user database DMs PM OS, or uses `/docs`, and gets "PM OS is for the
   product team." with a **Request access** button.
2. A tap reads their name and email from Slack (`users.info`). With no email in their Slack
   profile they are told so, and nothing is sent.
3. The request goes to the tool server, and they are told "Sent to the platform owners. I'll
   DM you when it's decided." Asking again while it waits says it is already pending.

**Deciding:**
- Every platform owner (an active `admin` in the user database) gets a DM card with the name, email and
  proposed handle, and **Approve**, **Approve with another handle** and **Reject**.
- Only an admin's tap counts; anyone else is told "Only platform owners can decide this."
- **Approve with another handle** opens a form. A handle that isn't valid, or is taken, is
  shown under the field before anything is sent.
- **Approving adds them, then commits as the admin:** the tool server adds the new member to
  the user database (they can use PM OS at once), then commits their `bots/pm-<handle>/BOT.md`
  and its sandbox policy to the content repo with the admin's GitHub account.
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
  **Remove** has the tool server **deactivate** them in the user database: their sessions end
  and their linked accounts are suspended, then deleted after 24 hours unless they are
  reactivated; nothing is committed (`docs/runbooks/users.md`, "Deactivating someone"). An
  admin can't remove themselves, and Slack never removes an Admin or Auditor.

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
  their bots. A bot with no owner, or whose owner isn't an active user, is skipped and logged.
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

## The reply assistant in Slack (`pmos-slack`)

The contract is `docs/slack-reply-spec.md` (§1 and §5 for Slack; §3 for the tool server's
audience check and expiry). It needs the bots (`PMOS_BOTS_URL`) and the member's personal
bot `pm-<handle>`.

**The commands,** typed alone in a DM:
- `reply assistant on` creates the member's routine directly with `POST /v1/routines`; the
  model isn't involved. The routine:
  - runs on `pm-<handle>`, every 30 minutes from 08:00 to 17:30 on weekdays
    (`*/30 8-17 * * 1-5`), in the member's Slack time zone, else `PMOS_DEFAULT_TZ`;
  - has a prompt starting `[reply-assistant]`, which is how Slack finds it again;
  - sends its result to the member's DM, and its writes need approval.

  If it is already on, the bot says so with the next run; if it was paused, it is resumed.
- `reply assistant off` deletes it. Drafts already waiting stay until they're decided or
  expire.
- `reply assistant` (or `reply assistant status`) says whether it is on, and when it next
  runs.
- A member without a personal bot is told the assistant runs on `pm-<handle>` and that
  nothing was changed. Someone not in the user database gets the usual **Request access** note.
- The member's other routines are never touched; "my routines" still lists them all.

**The reply card.** Each `slack.post_reply` draft gets its own card in the member's DM:
- **Header:** "Reply drafted for #channel" (or "for your DM"), with a link to the thread.
  - The channel is a Slack mention (`<#C…>`), so Slack shows its name to whoever can see it,
    and the bot never reads the channel.
  - The thread link is built as `https://slack.com/archives/<channel>/p<thread_ts without
    the dot>`, the format the tool server's Slack connector uses for its own links, rather
    than asked of `chat.getPermalink`, which the bot token can't answer for channels the bot
    isn't in.
- **The draft,** quoted, as Slack will show it: its `https://` links and `@person` mentions
  are kept, and everything else is escaped (an `@here` in a draft shows as text).
- **The question's text is never on the card.** It isn't in the approval item, and the card
  shows only the draft.
- **A context line:** who it is for, that it posts as you marked "Drafted with PM OS", what
  it was built from (`public`, `internal` or `restricted` sources), and the audience when the
  item knows it ("Visible to 14 people", "External channel").
- **The buttons:**
  - **Post as you** approves it;
  - **Edit** opens the usual editor, whose submit button reads **Post as you**;
  - **Snooze** (below);
  - **Reject** asks why, with the usual six reasons.

  As with every card, only the member the draft is for can use them; each tap is
  acknowledged at once, and a card decided meanwhile shows its outcome.
- **Once posted:** "Posted by Ana — see the reply", linking the reply in Slack.

**What the tool server can answer** when a draft is posted, each with its own card:
- `audience_blocked`: "This draft uses restricted sources and 3 people in #channel aren't on
  the product team" (or "#channel is shared outside the company"), "so nothing was posted", with
  only **Edit** and **Reject**. The draft stays waiting.
- `needs_external_confirm`: the card says the channel is shared with people outside the company
  and adds **Post to external channel**, which sends the decision again with
  `confirm_external: true`. After an edit it resends the edit, so the edited text is what
  posts; if the bot restarted in between, it asks for the edit again instead.
- `expired`: "Expired after 24 hours; nothing was posted", with no buttons. A draft the tool
  server already marked `expired` shows the same.
- `audience_unavailable`: Slack didn't answer the channel check, so nothing was posted; the
  buttons stay, to try again.

**Snooze:**
- **The choices:** 1 hour, 4 hours, or tomorrow at 09:00 in the member's Slack time zone
  (else `PMOS_DEFAULT_TZ`), with **Back**.
- **The expiry:** a choice that would end after the draft expires isn't offered. The expiry
  is the item's `expires_at`, else 24 hours after it was drafted. With no choice left, the
  card says it's too late to snooze.
- **The card** becomes "Snoozed until Mon 16 Nov 14:00 GMT", with the header and no draft.
- **At that time,** if the draft is still waiting, the card is posted again as a new message
  in the DM, and the snoozed message says it's back. A draft decided or expired meanwhile
  isn't posted again; its snoozed message shows the outcome instead.
- **Restarts:** snoozes are kept in `PMOS_SLACK_STATE_DIR/snoozes.json` (approval ids,
  message ids and times, never message text), so a restart keeps them. The bot looks for
  due snoozes every 30 seconds.

**Slack scopes:** the audience check reads a channel's sharing and members with each PM's
own token, so the user scopes now include `channels:read`, `groups:read`, `mpim:read` and
`im:read` (`docs/slack-app-manifest.yaml`). PMs who linked Slack before this need to link it
again to grant them.

## Network on PM OS's host

| Service | Needs to reach |
| --- | --- |
| `pmos-docs` | your docs site's host, from `PMOS_DOCS_BASE_URL` (HTTPS) |
| `pmos-slack` | `slack.com` (to open the connection), `wss-primary.slack.com` and `wss-backup.slack.com` (Socket Mode), `hooks.slack.com` (replies) |

Neither is reachable from agent sandboxes; the sandbox policy (0f) allows only the tool server
and the gateway.
