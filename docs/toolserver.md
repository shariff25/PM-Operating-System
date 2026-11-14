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
| A skill reads nothing above its data class | `context_search` drops results above the skill's `data-class`; `context_read` refuses them | Data classes |
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

## Endpoints

| Endpoint | Auth | Purpose |
| --- | --- | --- |
| `GET /healthz` | none | Liveness |
| `POST /mcp` (Streamable HTTP) | `Bearer <session token>` | MCP for the harness: `tools/list`, `tools/call` |
| `POST /internal/sessions` `{user, skill, run_id, ttl_minutes?}` | service token | Create a session; returns `token` and the session's tools |
| `DELETE /internal/sessions/{token}` | service token | Revoke a session (sandbox ended) |
| `POST /internal/grants` `{session_token, tool, args}` | service token | Record an approval given in the harness |
| `GET /internal/approvals?user=&status=` | service token | A PM's inbox, newest first |
| `GET /internal/approvals/{id}` | service token | One item |
| `POST /internal/approvals/{id}/decision` `{decided_by, decision, args?}` | service token | `approve`, `edit` (with `args`) or `reject` |
| `POST /internal/reindex` | service token | Rebuild the context index from the content checkout; `422` if it fails `pmos lint` |
| `GET /internal/oauth/{atlassian\|slack}/start?user=` | service token | The URL to send a PM to so they can link their account |
| `GET /oauth/{atlassian\|slack}/callback` | none (the PM's browser) | Finishes linking; shows a short page |
| `GET /internal/credentials?user=` | service token | A PM's linked accounts, without tokens |
| `DELETE /internal/credentials/{user}/{provider}` | service token | Unlink (the PM disconnects, or a platform owner revokes) |

Only the orchestrator and the Slack bot hold the service token. Sandboxes
get only their own session token. A tool error (refused, invalid arguments, not found) comes
back as an MCP tool result with `isError: true`, so the model can read why.

## Tools

Tool names in MCP are the registry names with dots turned into underscores
(`jira.create_issue` is `jira_create_issue`), the same names the harness's `interruptOn`
uses. The registry is `content/tools.yaml`; each tool's input schema is in
`crates/toolserver/src/schemas.rs`. The server refuses to start if a registry tool has no
schema or no connector.

## Connectors and account linking

With `PMOS_CONNECTORS=live`, every Jira, Confluence and Slack call acts as the PM, with the
PM's own token. There is no shared service identity, so a PM can only read and change what
they could in the product itself.

| System | Auth | Calls |
| --- | --- | --- |
| Jira | Atlassian OAuth 2.0 (3LO), via `api.atlassian.com/ex/jira/{cloudid}` | `GET /rest/api/3/search/jql`, `GET /rest/api/3/issue/{key}`, `POST /rest/api/3/issue` (description in Atlassian Document Format) |
| Confluence | same token, via `api.atlassian.com/ex/confluence/{cloudid}` | `GET /wiki/rest/api/search?cql=`, `GET /wiki/api/v2/pages/{id}`, `GET /wiki/api/v2/spaces?keys=`, `POST /wiki/api/v2/pages` (always `status: draft`), `PUT /wiki/api/v2/pages/{id}` (version + 1) |
| Slack | the PM's user token (`xoxp`), never a bot token | `assistant.search.context` (Real-time Search; results are never stored), `conversations.replies`, `chat.postMessage` |

These API shapes were checked against developer.atlassian.com and docs.slack.dev in November
2026, and tested against local mocks (`tests/live_connectors.rs`). The first real call
happens on the Spark after Gate 1.

**Linking an account.**
1. The Slack bot (its Accounts tab, or a first-use prompt) asks `/internal/oauth/{provider}/start` for a URL and sends the PM there.
2. Atlassian or Slack sends the PM's browser back to `/oauth/{provider}/callback`.
3. The tool server exchanges the code and seals the tokens in the vault.

The `state` value is HMAC-signed, names the user and provider, expires in 10 minutes and is
accepted once. Atlassian asks for `offline_access`, so it returns a refresh token, and
`accessible-resources` gives the site's `cloudid`. Set `PMOS_ATLASSIAN_SITE` if a grant covers
several sites.

**Refresh.** An Atlassian token within 60 seconds of expiry is refreshed under a per-user lock,
so concurrent calls make one refresh. Atlassian refresh tokens rotate: each refresh returns a
new one, which replaces the old. If Atlassian refuses the refresh (revoked, or unused for 90
days), the account is unlinked and the PM is asked to reconnect.

**Errors the model sees** are written for the PM:
- "connect Atlassian (Jira and Confluence) in PM OS first" when no account is linked.
- "reconnect" on a 401 or a revoked token.
- "permission denied" on a 403, audited as `denied`.
- A rate limit with its `Retry-After`.

**What gets written.**
- Every Slack post and Jira description ends with "Drafted with PM OS", and the Jira and
  Confluence ones also carry the run id, so a reader can trace them back to the run (SLK-7, OUT-1).
- Raw HTML in a Confluence draft's markdown is escaped, so a draft can't add macros the PM
  didn't see in the approval card.
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
| Sessions | `sessions` | Only the SHA-256 of each token is stored |
| Grants | `grants` | Taken with `DELETE … RETURNING` on a row locked `FOR UPDATE SKIP LOCKED`: one grant, one call, even under concurrency |
| Approvals | `approvals` | A decision is one conditional `UPDATE … WHERE status = 'pending' AND user_handle = …`: exactly one of several concurrent decisions wins |
| Audit | `audit_events` | Append-only: a trigger rejects `UPDATE` and `DELETE` |
| Credentials | `credentials` | Tokens sealed by the vault; the database never sees plaintext |

- **Migrations** run on start-up.
- **Pruning:** expired sessions and grants are deleted every 10 minutes.
- **Audit failures:** if an audit event can't be recorded, the server logs `AUDIT EVENT NOT RECORDED` at error level. The call itself is not undone.
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
| `PMOS_SERVICE_TOKEN` | required | At least 32 characters |
| `PMOS_CONNECTORS` | required | `live` for real Atlassian and Slack with each PM's own token; `fake` for fixture data (dev, tests, and everything before Gate 1) |
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
| `PMOS_ATLASSIAN_SCOPES` | Jira classic plus Confluence granular read/write and search | `offline_access` is always added |
| `PMOS_ATLASSIAN_SITE` | none | Needed only if a grant covers several sites |
| `PMOS_SLACK_CLIENT_ID`, `PMOS_SLACK_CLIENT_SECRET` | required | From the internal Slack app |
| `PMOS_SLACK_SCOPES` | search, history and `chat:write` user scopes | Requested as `user_scope` only |
| `PMOS_ATLASSIAN_AUTH_URL`, `PMOS_ATLASSIAN_API_URL`, `PMOS_SLACK_URL` | the real hosts | For pointing at mocks |

The OAuth `state` signing key is derived from `PMOS_SERVICE_TOKEN`, so rotating the service
token invalidates sign-in links in flight.

After a merge to the content repo, pull the checkout and call `POST /internal/reindex`:
- If the content fails `pmos lint`, the call returns `422` and the index keeps its last good state.
- Skill and tool-registry changes take effect on the next restart.

## Not yet

- **First real calls** with real tokens and data, on the Spark after Gate 1. The sandbox network policy (0f) must allow only the tool server and gateway, not these hosts.
- **Embeddings** in the context index, once the gateway (0d) serves the embedding model.
- **Paging** past the first page of results (`nextPageToken`, `cursor`). Skills ask for small result sets for now.
