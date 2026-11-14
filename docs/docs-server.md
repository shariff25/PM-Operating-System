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
  private note saying who to ask. The file is re-read on every command, so a merged change
  applies without a restart.
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
   `chat:write.customize`, `im:history`, `users:read`).
2. Generate an app-level token with `connections:write` and set it as `PMOS_SLACK_APP_TOKEN`.
3. Set `PMOS_TOOLSERVER_URL`, `PMOS_BOT_TOKEN` (the same value as the tool server's `PMOS_BOT_TOKEN`) and `PMOS_CONTENT_ROOT`.
4. For the bots (DMs, approval cards, routines; spec §5 of `docs/bot-runtime-spec.md`), also set:
   - `PMOS_BOTS_URL` (the runtime, port 8750) and `PMOS_BOTS_TOKEN` (the runtime's `slack` client token);
   - `PMOS_SLACK_BOT_TOKEN`, the app's bot token (`xoxb-…`);
   - `PMOS_SLACK_STATE_DIR` (default `state/slack`), where the notification cursor is kept so a
     restart reposts nothing;
   - `PMOS_DEFAULT_TZ` (default `UTC`), for routines when Slack has no time zone for the PM.

   Without `PMOS_BOTS_URL` the bot serves `/docs` only.
5. Add each PM to `content/team.yaml` (handle, Slack member id, roles) by pull request.

## Network on the Spark

| Service | Needs to reach |
| --- | --- |
| `pmos-docs` | `support.acme.com` (HTTPS) |
| `pmos-slack` | `slack.com` (to open the connection), `wss-primary.slack.com` and `wss-backup.slack.com` (Socket Mode), `hooks.slack.com` (replies) |

Neither is reachable from agent sandboxes; the sandbox policy (0f) allows only the tool server
and the gateway.
