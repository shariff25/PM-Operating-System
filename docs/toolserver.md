# Tool server

`pmos-toolserver` (Rust, `crates/toolserver`) is the only path from an agent sandbox to the
context library, memory, and the systems of record: Jira and Confluence (Atlassian Cloud) and
Slack. The sandbox network policy allows only this server and the model gateway. The team
service and personal agents go through the same server, so they follow the same rules
(MOD-3).

## Rules it enforces

| Rule | How | PRD |
| --- | --- | --- |
| A skill calls only the tools it declares | `tools/list` returns the session skill's tools only; any other call is refused and audited as `denied` | Authorization |
| A bot calls only its role's tools, and never a `never` action | A bot session's `tools/list` is its role's tools less any write set to `never`; anything else is refused and audited as `denied` (see **Bot sessions**) | D7 |
| A skill reads nothing above its data class | `context_search` drops results above the skill's `data-class` (a bot session's role's); `context_read` refuses them | Data classes |
| A team bot reads only what its owner chose | Its Jira, Confluence and Slack reads use its own account and stay inside its `reads` list; anything else is `403`, audited as `denied` | Decision 26 |
| External content is data, not instructions | Every Jira, Confluence and Slack item comes back wrapped as `{"untrusted": true, "source": …, "content": …}` | Untrusted content |
| Nothing is written outside PM OS without approval | A write runs only with a matching grant or an inbox approval by its own user; otherwise it returns `pending_approval` and does nothing | OUT-4 |
| Everything is audited | One event per call and per decision: user, skill, run, tool, outcome, sources read, argument hash, output reference. Never message bodies | ADM-2, SLK-8 |
| Arguments match the tool's schema | Required keys, no unknown keys, types and enums checked before any handler runs | — |

## How a write gets approved

```
harness ──tool call──► interruptOn pause ──► PM approves / edits / rejects (Slack card or web)
                                                   │ approve or edit
                                                   ▼
                     orchestrator ──POST /internal/grants {session, tool, args}──► tool server
harness ──same call (edited args if edited)──► tool server: grant matches → runs once
```

- A grant matches one exact call: the same session, the same tool, and the same arguments
  (the SHA-256 of their canonical JSON, so key order doesn't matter). It is single-use and
  expires after 15 minutes.
- A write with no grant never runs. This covers a personal agent without approval pauses, a
  model that skips the pause, or edited arguments that don't match. The write becomes a
  pending item in the PM's inbox instead.
- Only the item's own user can approve it there (`403` for anyone else). Approve and edit
  run the write as that user.
- **Edits are checked before anything changes.** Edited arguments are checked against the
  tool's schema like any call (`400` if they fail), and the item's skill must still declare
  the tool (`409` if not). A refused decision leaves the item pending.
- **An item moves through states:** `pending`, then `executing` while the write runs, then
  `approved` (with the result) or `failed`.
  - The write runs in its own task, so an approver who disconnects can't cancel it halfway.
  - If the process dies mid-write, the item stays `executing`. On the next start, and every
    10 minutes for anything stuck longer than that, it becomes `failed` with "it may or may
    not have happened: check Jira before trying again". Nothing is retried automatically.
- **No write runs unless the audit log records it starting** (outcome `started`). If that
  record fails, the write doesn't run, and a harness grant is put back.
- **A timeout or 5xx on a write** is reported as "may or may not have happened", never as
  "unavailable", so neither the model nor the PM retries into a duplicate.

## Endpoints

| Endpoint | Auth | Purpose |
| --- | --- | --- |
| `GET /healthz` | none | Liveness: the process answers |
| `GET /readyz` | none | Readiness: `200` when storage answers within a second and every connector's own dependency (the support docs server) answers; `503` otherwise, naming the check that failed |
| `POST /mcp` (Streamable HTTP) | `Bearer <session token>` | MCP for the harness: `tools/list`, `tools/call` |
| `POST /internal/sessions` `{user, skill, run_id, ttl_minutes?}` | service or bot token | Create a session; returns `token` and the session's tools. `403` if the user isn't in `team.yaml` (live connectors), or the bot asks for a skill outside `PMOS_BOT_SKILLS`. |
| `POST /internal/sessions` `{user, bot, run_id, ttl_minutes?}` | service token | Create a bot session (see **Bot sessions**). Send `skill` or `bot`, never both (`400`). `404` for an unknown bot; `403` if `user` may not use it. |
| `DELETE /internal/session` + `X-Pmos-Session` | service or bot token | Revoke a session (sandbox ended) |
| `POST /internal/call` + `X-Pmos-Session` `{tool, args}` | service or bot token | One tool call for the session, with the same rules as MCP: allowlist, argument checks, approvals for writes, audit (which records the caller). |
| `POST /internal/grants` `{session_token, tool, args}` | service token | Record an approval given in the harness |
| `GET /internal/approvals?user=&status=` | service token | A PM's inbox, newest first |
| `GET /internal/approvals/{id}` | service or bot token | One item |
| `POST /internal/approvals/{id}/decision` `{decided_by, decision, args?}` | service or bot token | `approve`, `edit` (with `args`) or `reject`. `decided_by` must be the item's user (`403`), and in `team.yaml` when it is enforced (`403`). The audit records which token decided. |
| `POST /internal/reindex` | service token | Rebuild the context index from the content checkout and re-read `team.yaml`, the bots and their roles; `422` if it fails `pmos lint` |
| `GET /internal/oauth/{atlassian\|slack}/start?user=` | service token | The URL to send a PM to so they can link their account. `user=bots.<name>` gives the URL for a team bot's own account (`404` unless `<name>` is a team bot). |
| `GET /oauth/{atlassian\|slack}/callback` | none (the PM's browser) | Finishes linking; shows a short page |
| `GET /internal/credentials?user=` | service token | A PM's linked accounts, without tokens |
| `DELETE /internal/credentials/{user}/{provider}` | service token | Unlink (the PM disconnects, or a platform owner revokes) |

**Who holds which token:**
- **Service token** (`PMOS_SERVICE_TOKEN`): the orchestrator, the bot runtime and platform-owner tools. Every route, and the only token that starts bot sessions.
- **Bot token** (`PMOS_BOT_TOKEN`): the Slack bot. Only sessions for the skills in `PMOS_BOT_SKILLS` and calls within them, plus reading and deciding one approval item (`GET /internal/approvals/{id}` and its `decision`), for Slack approval cards. A session started for another skill, or any bot session, looks to the bot like one that doesn't exist, and every other route returns `403`.
- **Session token:** each sandbox gets only its own.

Session tokens are sent in headers, never in a URL, so access logs and proxies don't record them. A tool error (refused, invalid arguments, not found) comes
back as an MCP tool result with `isError: true`, so the model can read why.

## Bot sessions

A named bot (`content/bots/<name>/BOT.md`, made from one role in `content/roles/`) gets a
session per task: the bot runtime sends `POST /internal/sessions {user, bot, run_id}` with the
service token. Bots and roles load with the skills at start-up and reload on
`POST /internal/reindex`; a live session follows the new settings on its next call, and a
session whose bot is gone (or whose personal bot changed owner) stops working.
Spec: `docs/bot-runtime-spec.md` §2.

| Session property | Rule |
| --- | --- |
| Tools | `role_tools(role, skills)`: the union of the role's skills' tools |
| Data class | the role's `data-class` |
| A write set to `never` (`bot.action_setting(role, tool)`, the bot's override, else the role's) | not in `tools/list`; a call is refused (`403` on `/internal/call`, an MCP tool error), audited as `denied`, and never reaches the inbox. A harness grant can't make it run. |
| A write set to `approve` | the usual grant-or-inbox path. The item records the bot, and is checked again when decided: if the bot is gone, its role lost the tool or it is now `never`, the decision is `409` and the item stays pending. |
| `user` | the PM the task is for. A personal bot serves only its owner (`403` otherwise); a team bot, any team member (`team.yaml`, when enforced). Never a `bots.*` identity. |

Sessions, approval items and audit events record either `skill` or `bot`, never both; a bot
session's `skill` is `null`.

**Who a bot reads and writes as.**
- **Personal bots** read as their owner, with the owner's linked accounts and no scope.
- **Team bots** read Jira, Confluence and Slack as the bot's own service identity,
  `bots.<name>`. A platform owner links its read-only accounts with
  `GET /internal/oauth/{provider}/start?user=bots.<name>` (service token), then opens the
  URL signed in as the bot's account. The `team.yaml` identity check is skipped only for
  `bots.<valid bot name>`; such a handle is never a PM, so it gets no session and can't
  decide approvals. `GET /internal/credentials?user=bots.<name>` and
  `DELETE /internal/credentials/bots.<name>/{provider}` list and unlink it. If it isn't
  linked, a read fails with "the `<name>` bot has no working … account", never falling back
  to the PM's.
- **Writes** from any bot session run as `user`, with the approving PM's own token, never as
  the bot.

**A team bot's read scope** is its `reads` list in `BOT.md`, enforced in the connectors (the
fake ones apply it too):

| Tool | How |
| --- | --- |
| `jira.search` | the JQL becomes `project in ("A", "B") AND (<jql>)`; a trailing `ORDER BY` stays outside the group |
| `jira.read` | the key's project must be listed, checked before the call and again on the issue returned (a moved issue changes key) |
| `confluence.search` | the CQL becomes `space in ("A") AND (<cql>)` |
| `confluence.read` | the page's space (by id, from `GET /wiki/api/v2/spaces?keys=`) must be listed, checked after the fetch and before anything is returned; a page with no space is refused |
| `slack.search` | results outside the listed channels are dropped |
| `slack.read_thread` | the channel must be listed |

- An empty list means no access to that system. An out-of-scope read is `403`, audited as
  `denied`.
- **No query injection.** Project and space keys must match `^[A-Z][A-Z0-9_]{0,19}$` and
  channels must be ids (`C…` or `G…`); an entry that doesn't is refused at read time (and
  logged at start-up), never put in a query. A query is wrapped only if its quotes are
  closed, its parentheses balanced, it has no backslash outside a quote and its `ORDER BY` is
  a plain list of fields, so nothing in it can reach outside the `( … )`. Otherwise it is
  `400`.
- `docs.*` and `context.*` have no read scope: the docs are public, and context follows the
  data class.

## Tools

Tool names in MCP are the registry names with dots turned into underscores
(`jira.create_issue` is `jira_create_issue`), the same names the harness's `interruptOn`
uses. The registry is `content/tools.yaml`; each tool's input schema is in
`crates/toolserver/src/schemas.rs`. The server refuses to start if a registry tool has no
schema or no connector.

## Connectors and account linking

With `PMOS_CONNECTORS=live`, every Jira, Confluence and Slack call acts as the PM, with the
PM's own token, so a PM can only read and change what they could in the product itself. The
one exception is a team bot's reads, which use the bot's own read-only accounts inside its
read scope (see **Bot sessions**); its writes still act as the PM.

| System | Auth | Calls |
| --- | --- | --- |
| Jira | Atlassian OAuth 2.0 (3LO), via `api.atlassian.com/ex/jira/{cloudid}` | `GET /rest/api/3/search/jql`, `GET /rest/api/3/issue/{key}`, `POST /rest/api/3/issue` (description in Atlassian Document Format) |
| Confluence | same token, via `api.atlassian.com/ex/confluence/{cloudid}` | `GET /wiki/rest/api/search?cql=`, `GET /wiki/api/v2/pages/{id}`, `GET /wiki/api/v2/spaces?keys=`, `POST /wiki/api/v2/pages` (always `status: draft`), `PUT /wiki/api/v2/pages/{id}` (version + 1) |
| Slack | the PM's user token (`xoxp`), never a bot token | `assistant.search.context` (Real-time Search; results are never stored), `conversations.replies`, `chat.postMessage` |
| Support docs | none (public docs); the tool server's `PMOS_DOCS_TOKEN` to `pmos-docs` | `search_docs`, `get_doc` over MCP; see `docs/docs-server.md` |

These API shapes were checked against developer.atlassian.com and docs.slack.dev in November
2026, and tested against local mocks (`tests/live_connectors.rs`). The first real call
happens on the Spark after Gate 1.

**Linking an account.**
1. The Slack bot (its Accounts tab, or a first-use prompt) asks `/internal/oauth/{provider}/start` for a URL and sends the PM there.
2. Atlassian or Slack sends the PM's browser back to `/oauth/{provider}/callback`.
3. The tool server exchanges the code and checks the account belongs to that PM in
   `team.yaml`: the Slack user must be the member's `slack-id`, and the Atlassian account's
   email (from `/me`, scope `read:me`) must be the member's `email`. Only then does it seal the
   tokens in the vault. A link forwarded to someone else attaches nothing.

The `state` value is HMAC-signed, names the user and provider, expires in 10 minutes and is
accepted once. Atlassian asks for `offline_access`, so it returns a refresh token, and
`accessible-resources` gives the site's `cloudid`. Set `PMOS_ATLASSIAN_SITE` if a grant covers
several sites.

**Refresh.** An Atlassian token within 60 seconds of expiry is refreshed under a per-user lock,
so concurrent calls make one refresh. Atlassian refresh tokens rotate: each refresh returns a
new one, which replaces the old.
- **Only a refused grant unlinks.** If Atlassian answers `invalid_grant` (revoked, or unused for
  90 days), the account is unlinked and the PM is asked to reconnect.
- **Any other refusal keeps the account.** For example `invalid_client` after a mistyped
  client secret, or a firewall page. The call fails as unavailable and an error is logged for
  the platform owner, rather than making every PM reconnect.
- **Saving the new token is conditional.** It is written only over the row it was refreshed
  from, and retried if storage blips. Unlinking waits for any refresh in flight, so an
  unlinked account never comes back. Re-sealing after a key rotation is conditional the
  same way.

**Errors the model sees** are written for the PM:
- "connect Atlassian (Jira and Confluence) in PM OS first" when no account is linked.
- "reconnect" on a 401 or a revoked token.
- "permission denied" on a 403, audited as `denied`.
- A rate limit with its `Retry-After`.

**What the model gets back** is bounded, because the context window on the Spark's local model is shared by every session:
- **Bodies:** a Jira description, a Confluence page or a docs page is cut at 12,000 characters and marked `truncated`.
- **Jira comments:** at most 20, each cut at 1,000 characters.
- **Slack threads:** at most 50 messages, each cut at 2,000 characters.
- **Searches:** at most 20 results.
- **Input:** query strings are at most 500 characters (JQL and CQL 2,000), and a search uses at most 32 distinct words.

**What gets written.**
- Every Slack post and Jira description ends with "Drafted with PM OS", and the Jira and
  Confluence ones also carry the run id, so a reader can trace them back to the run (SLK-7, OUT-1).
- Raw HTML in a Confluence draft's markdown is escaped, so a draft can't add macros the PM
  didn't see in the approval card.
- **Updating a Confluence page keeps its status**, so updating a draft doesn't publish it.
  If someone saved a newer version first, the update fails with "page changed since PM OS
  read it". MCP marks `confluence_update_page` as destructive, because it replaces the body.
- Caller-supplied ids (issue keys, page ids, Slack channels and timestamps) are validated
  before they reach a URL.

### The vault

Tokens are sealed with AES-256-GCM before any store sees them (`src/vault.rs`, table
`credentials`).
- Each ciphertext is bound to its user, provider and field, so a sealed token copied into
  another user's row won't decrypt.
- `PMOS_VAULT_KEY` is a comma-separated list of `key_id:base64key` (32-byte keys). The first key
  seals; every listed key can unseal.
- **To rotate:** put the new key first and keep the old one listed. Rows are re-sealed under the
  new key on their next read, and the old key can be dropped once nothing uses it.
- Tokens never appear in logs, audit events, API responses or `Debug` output.

## Storage

Every store sits behind an async trait (`crates/toolserver/src/store.rs`). Each has an in-memory
implementation for tests and `PMOS_DEV=1`, and a Postgres one for real use
(`crates/toolserver/src/pg.rs`, schema in `crates/toolserver/migrations/`).

| Store | Postgres table | Notes |
| --- | --- | --- |
| Context index | `context_docs` | Full-text search (title weighted over body), rebuilt from the content repo on start-up and on `POST /internal/reindex`. Embeddings are added once the gateway serves the embedding model. |
| Memory | `memory_notes` | One row per user and key |
| Sessions | `sessions` | Only the SHA-256 of each token is stored. Each names a `skill` or a `bot` (a check constraint keeps it to one) |
| Grants | `grants` | Taken with `DELETE … RETURNING` on a row locked `FOR UPDATE SKIP LOCKED`: one grant, one call, even under concurrency |
| Approvals | `approvals` | A decision is one conditional `UPDATE … WHERE status = 'pending' AND user_handle = …`: exactly one of several concurrent decisions wins |
| Audit | `audit_events` | Append-only: triggers reject `UPDATE`, `DELETE` and `TRUNCATE`. Each event records the caller (`mcp`, `orchestrator` or `bot`) and the session's skill or bot. |
| Credentials | `credentials` | Tokens sealed by the vault; the database never sees plaintext |

- **Migrations** run on start-up.
- **Pruning:** expired sessions and grants are deleted every 10 minutes.
- **Shutdown:** SIGTERM (what `docker compose stop` sends) or Ctrl-C stops new requests and lets in-flight ones finish. Give the container a stop grace period of at least 30 seconds.
- **MCP sessions** stay open while idle for up to the session TTL (60 minutes), so a long model step between tool calls doesn't drop the connection.
- **One replica.** OAuth sign-in states, refresh locks and MCP sessions are in process memory, so run a single tool server. Running two needs those moved to Postgres first; this is tracked in the implementation plan.
- **Audit failures:**
  - **Writes fail closed:** they don't run unless their `started` event is recorded.
  - **Reads:** a missing event is logged as `AUDIT EVENT NOT RECORDED` at error level, and the read isn't undone.
- **Database roles (recommended):** run migrations as an owner role, and give the tool server's role only `SELECT, INSERT` on `audit_events`. The triggers already stop the owner from rewriting history; the separate role also stops anyone from dropping them through the app's connection.
- **Tests:** `tests/stores.rs` runs the same contract cases against both backends, including the concurrency cases.
  - Set `PMOS_TEST_DATABASE_URL` to run the Postgres side. CI does.
  - Without it, the Postgres cases are skipped locally and fail in CI.

## Running it

```sh
PMOS_SERVICE_TOKEN=$(openssl rand -hex 32) \
PMOS_CONNECTORS=fake \
PMOS_DATABASE_URL=postgresql://pmos@localhost/pmos \
cargo run -p pmos-toolserver
```

| Variable | Default | Notes |
| --- | --- | --- |
| `PMOS_SERVICE_TOKEN` | required | The orchestrator's token. At least 32 characters |
| `PMOS_BOT_TOKEN` | none | The Slack bot's own token: at least 32 characters and different from the service token |
| `PMOS_BOT_SKILLS` | `docs` | Skills the bot may start sessions for |
| `PMOS_CONNECTORS` | required | `live` for real Atlassian and Slack with each PM's own token, and only people in `team.yaml` may have sessions; `fake` for fixture data (dev, tests, and everything before Gate 1), with `team.yaml` not enforced |
| `PMOS_DATABASE_URL` | required | Postgres for every store. `PMOS_DEV=1` without it keeps everything in memory. |
| `PMOS_AUDIT_LOG` | none | Also writes audit events to this JSON-lines file, for shipping off-box |
| `PMOS_CONTENT_ROOT` | `content` | Must pass `pmos lint`, or the server won't start |
| `PMOS_BIND` | `127.0.0.1:8710` | |
| `PMOS_ALLOWED_HOSTS` | `toolserver.pmos.internal,localhost,127.0.0.1` | `Host` values `/mcp` accepts |

With `PMOS_CONNECTORS=live`:

| Variable | Default | Notes |
| --- | --- | --- |
| `PMOS_VAULT_KEY` | required | `key_id:base64key[,…]`; generate a key with `openssl rand -base64 32` |
| `PMOS_PUBLIC_URL` | required | `https://` URL PMs' browsers reach; callbacks are `{url}/oauth/{provider}/callback`, registered with each app |
| `PMOS_ATLASSIAN_CLIENT_ID`, `PMOS_ATLASSIAN_CLIENT_SECRET` | required | From the Atlassian developer console (OAuth 2.0 integration) |
| `PMOS_ATLASSIAN_SCOPES` | `read:me`, Jira classic, Confluence granular read/write and search | `offline_access` is always added; keep `read:me`, which the link check needs |
| `PMOS_ATLASSIAN_SITE` | none | Needed only if a grant covers several sites |
| `PMOS_SLACK_CLIENT_ID`, `PMOS_SLACK_CLIENT_SECRET` | required | From the internal Slack app |
| `PMOS_SLACK_SCOPES` | search, history and `chat:write` user scopes | Requested as `user_scope` only |
| `PMOS_ATLASSIAN_AUTH_URL`, `PMOS_ATLASSIAN_API_URL`, `PMOS_SLACK_URL` | the real hosts | For pointing at mocks |
| `PMOS_DOCS_URL` | required | The `pmos-docs` MCP endpoint, such as `http://docs.pmos.internal:8730/mcp` |
| `PMOS_DOCS_TOKEN` | required | The bearer token `pmos-docs` expects |

`PMOS_DEV=1` (everything in memory) is refused with `PMOS_CONNECTORS=live`, so real tokens and the audit log can't end up in a process that forgets them.

The OAuth `state` signing key is derived from `PMOS_SERVICE_TOKEN`, so rotating the service
token invalidates sign-in links in flight.

After a merge to the content repo, pull the checkout and call `POST /internal/reindex`:
- If the content fails `pmos lint`, the call returns `422` and the index keeps its last good state.
- Bot and role changes (tools, action settings, read scope) take effect at once.
- Skill and tool-registry changes take effect on the next restart.

## Not yet

- **First real calls** with real tokens and data, on the Spark after Gate 1. The sandbox network policy (0f) must allow only the tool server and gateway, not these hosts.
- **Embeddings** in the context index, once the gateway (0d) serves the embedding model.
- **Paging** past the first page of results (`nextPageToken`, `cursor`). Skills ask for small result sets for now.
