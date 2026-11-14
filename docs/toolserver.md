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

Only the orchestrator, the Slack app and the web workspace hold the service token. Sandboxes
get only their own session token. A tool error (refused, invalid arguments, not found) comes
back as an MCP tool result with `isError: true`, so the model can read why.

## Tools

Tool names in MCP are the registry names with dots turned into underscores
(`jira.create_issue` is `jira_create_issue`), the same names the harness's `interruptOn`
uses. The registry is `content/tools.yaml`; each tool's input schema is in
`crates/toolserver/src/schemas.rs`. The server refuses to start if a registry tool has no
schema or no connector.

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
| `PMOS_CONNECTORS` | required | Only `fake` until the real connectors land (no real tokens before Gate 1) |
| `PMOS_DATABASE_URL` | required | Postgres for every store. `PMOS_DEV=1` without it keeps everything in memory. |
| `PMOS_AUDIT_LOG` | none | Also writes audit events to this JSON-lines file, for shipping off-box |
| `PMOS_CONTENT_ROOT` | `content` | Must pass `pmos lint`, or the server won't start |
| `PMOS_BIND` | `127.0.0.1:8710` | |
| `PMOS_ALLOWED_HOSTS` | `toolserver.pmos.internal,localhost,127.0.0.1` | `Host` values `/mcp` accepts |

After a merge to the content repo, pull the checkout and call `POST /internal/reindex`:
- If the content fails `pmos lint`, the call returns `422` and the index keeps its last good state.
- Skill and tool-registry changes take effect on the next restart.

## Not yet

- **Real connectors** behind `Connector`:
  - Jira and Confluence on Atlassian Cloud REST with each user's OAuth 2.0 (3LO) token.
  - Slack with each PM's own user token. Real-time Search results are never stored.
- **Per-user token storage:** encrypted, and attached only to that user's sandbox through
  OpenShell credential placeholders.
- **Embeddings** in the context index, once the gateway (0d) serves the embedding model.
