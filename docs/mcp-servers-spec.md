# MCP servers in place of the built-in connectors

Status: **design spec; not built.** Revision r1, 2026-11-24, written from `claude/web-app` at
`0e1316b`. The independent security reviewer reads it before any slice starts (CLAUDE.md, "How
to work"). No connector code is removed or written until the reviewer has answered.

Source of the requirement: CLAUDE.md, scope item g. The owner, 2026-11-18: drop the built-in
connectors, use MCP servers instead; "the connector code is to be removed and an MCP feature
added in its place". The owner, 2026-11-24: go ahead and build to the PRD, starting with this
spec. Which MCP servers the pilot uses is still the owner's choice (§10).

Line numbers are not cited; file paths are. Where this spec is unsure of a fact it says
"to be confirmed".

## Owner's decisions of 2026-11-24

These answer open questions 1, 2 and 6 of §10. Where the text below differs, these decide; the
sections they change are to be revised before the build, and the reviewer has not yet read
them.

- **Which servers (question 1): all three kinds.** The design must support, for each of Jira,
  Confluence and Slack, the vendor's own hosted server, a self-hosted open-source server and a
  server PM OS builds. Which kind a deployment uses for a system is chosen per deployment in
  the server's record, not fixed in code. The product facts in §10's comparison are still to be
  confirmed for each server before it is used.
- **Who approves (question 2): an Admin.** A server and each of its tools are approved by a
  user with the Admin role. The record still reaches the content repository through a pull
  request, as every content change does. Whether that pull request also needs a second
  reviewer, as §2 proposes, is for the reviewer to weigh and the owner to confirm.
- **Removal (question 6): each connector's removal needs the owner's explicit go-ahead,** in
  addition to the tests, the comparison and the eval run.
- **The side-by-side comparison (question 5): fake data only.** The old connector and the new
  route are compared on fake data, reads only. No real data reaches an MCP server during the
  comparison. The comparison therefore shows agreement on the fixtures only; differences that
  appear only on real projects are found after the switch, so the switch for each system keeps
  a quick way back to the built-in connector until its removal is approved.

## 0. Summary

- The tool server (`crates/toolserver`) stays the only thing a sandboxed bot talks to. It
  becomes an MCP **client** of external MCP servers. A sandbox never reaches an MCP server, and
  its network policy does not change.
- Every guarantee the tool server gives today stays in the tool server: the approval gate,
  attribution, per-user tokens in the vault, data classes, untrusted-data wrapping, audit,
  ledger, fakes. A third-party server is treated as a backend that executes calls, never as
  something that decides.
- A server is usable only through a reviewed **server record**, and a tool only when its exact
  name and a digest of its definition have been approved. Any change to a tool makes it a new,
  unapproved tool.
- The first phase keeps today's registry names (`jira.create_issue` and the rest): each is
  **mapped** to one upstream tool. The model sees PM OS's own schema and description, never the
  server's.
- Old connector code is removed last, one connector at a time, only after its MCP replacement
  passes the same tests and an eval run.

## 1. What exists today

### 1.1 The registry and the dispatch

- The tool registry is `content/tools.yaml` (format: `docs/contract.md`, "Tool registry"). Each
  tool has `name` (`<system>.<action>`), `system`, `kind` (`read`, `internal`, `write`) and
  `description`. Input schemas live in `crates/toolserver/src/schemas.rs`; `ToolKind` is in
  `crates/contract/src/model.rs`.
- At start the tool server refuses a registry tool with no schema or no connector
  (`ToolService::new` in `crates/toolserver/src/service.rs`). Local systems with no connector
  are `context`, `memory`, `stats`, `bets` (`LOCAL_SYSTEMS`, same file).
- Connectors implement the `Connector` trait (`crates/toolserver/src/connectors/mod.rs`):
  `read`, `write`, `audience`, `conversation`, `ready`. `main.rs` builds one per system: fakes
  for `PMOS_CONNECTORS=fake`, live ones for `live`.
- `ToolService::call_as` and `dispatch` (`service.rs`) do, in order: the model-source check
  (slice H4), the invalid-draft limit, the session policy check (skill or role tools, persona
  intersection, `never` writes), `check_args` against the schema, the team-bot rules, then the
  connector call. Reads are wrapped by `connectors::untrusted`; writes go to `write`.
- The MCP surface for sandboxes is `crates/toolserver/src/mcp.rs` (rmcp, Streamable HTTP).
  Harness names replace dots with underscores. Writes are annotated and described as needing
  approval. The harness client is `harness/src/mcp-tools.ts`; it reads tool kinds from the
  registry copy in the sandbox (`harness/src/registry.ts`).

### 1.2 The connectors and their tools

| System | File | Reads | Writes | Auth |
| --- | --- | --- | --- | --- |
| Jira | `connectors/atlassian.rs`, `connectors/rollup.rs`, `connectors/format.rs` | `jira.search`, `jira.read`, `jira.rollup` (computed by PM OS over search) | `jira.create_issue` | Atlassian OAuth 2.0 (3LO), PM's token |
| Confluence | `connectors/atlassian.rs`, `connectors/format.rs` | `confluence.search`, `confluence.read` | `confluence.create_draft`, `confluence.update_page` | same Atlassian token |
| Slack | `connectors/slack.rs` | `slack.search` (Real-time Search, never stored), `slack.read_thread` | `slack.post_reply`, `slack.send_message` | PM's user token, never a bot token |
| Docs | `connectors/docs.rs` | `docs.search`, `docs.read` | none | none for the PM; `PMOS_DOCS_TOKEN` to `pmos-docs` |

Shared code: `connectors/http.rs` (status mapping, 30 s timeout, 429 with `Retry-After`, id
checks), `connectors/scope.rs` (a team bot's read scope: JQL and CQL wrapping, channel filter),
`connectors/registry.rs` (the provider table: Atlassian, Slack, GitHub), `connectors/fake.rs`.

GitHub is a provider but serves **no registry tool** (`systems: []` in `connectors/registry.rs`).
It backs personal memory (`github_memory.rs`), member approvals and admin proposals
(`github.rs`, `proposals.rs`). It is out of scope here (§9).

The docs connector is already an MCP client: it reaches PM OS's own `pmos-docs` server over
Streamable HTTP with a bearer token, shares one connection, holds no lock during a call, has a
10 s deadline, reconnects once on a transport error, and maps `{code, message}` tool errors
(`connectors/docs.rs`, `docs/docs-server.md`, "MCP tools").

### 1.3 The guarantees around them

| Guarantee | Where today |
| --- | --- |
| A write never runs without a human decision: a single-use grant for the exact arguments (SHA-256 of canonical JSON, 15 min), else a pending item the PM approves, edits or rejects | `approvals.rs`, `service.rs` `write`, `docs/toolserver.md` "How a write gets approved" |
| Edits cannot change where a draft goes (`EDIT_IMMUTABLE_ARGS`); `expected_args_hash` is required from Slack and web | `schemas.rs`, `docs/toolserver.md` |
| No write runs unless the audit log records it `started`; a timeout or 5xx on a write is "may or may not have happened"; nothing is retried | `service.rs`, `approvals.rs` |
| Writes act as the PM with the PM's token; drafts carry "Drafted with PM OS" and the run id (SLK-7, OUT-1); source markers are stripped before writing; persona notices are added | `connectors/mod.rs` `DRAFTED_MARKER`, `service.rs` `execute_write`, `crates/contract/src/cite.rs` |
| Confluence HTML is escaped; updates keep status and name the version read | `connectors/format.rs`, `connectors/atlassian.rs` |
| Slack audience check before a post: members vs the user database and the draft's sensitivity; Slack Connect needs a second confirmation (SLK-6) | `Connector::audience`, `docs/toolserver.md` "Slack drafts" |
| Slack text is gone after 24 hours (SLK-8); Slack reads record no titles | `docs/toolserver.md`, `audit.rs` `TITLED_SYSTEMS` |
| The signed user assertion on the approvals routes from web and console | `assertion.rs`, `docs/user-assertion.md` |
| Per-user linking: HMAC `state`, account identity checked against the user database before sealing; refresh under a per-user lock; only `invalid_grant` unlinks | `oauth.rs`, `docs/toolserver.md` "Connectors and account linking" |
| Tokens sealed with AES-256-GCM bound to user, provider and field; never in logs, events, responses or `Debug` | `vault.rs` |
| Requested scopes are fixed defaults, overridable per deployment | `config.rs` `DEFAULT_ATLASSIAN_SCOPES`, `DEFAULT_SLACK_SCOPES` |
| Data classes: a session is raised by what it reads (`docs` public, other connectors `internal`); `context` refuses above the ceiling | `service.rs` `connector_class`, `note_read` |
| Team bots read with their own read-only accounts inside their read scope; writes still act as the PM | `connectors/scope.rs`, `connectors/mod.rs` `Caller` |
| External content is untrusted: wrapped with a note and a `ref`; bounded (bodies 12,000 characters, 20 results, 50 thread messages) | `connectors/mod.rs` `untrusted`, `MAX_BODY_CHARS` |
| One audit event per call and decision: metadata only, an args hash, never bodies; Auditors see metadata only | `audit.rs`, `app/admin.rs` `event_json`, `docs/user-database-spec.md` |
| Audit purged at 90 days by one job | `retention.rs`, `docs/audit-retention.md` |
| A feedback-ledger row per approval item, no text | `ledger.rs` |
| Rate limits: the tool server passes on the provider's 429 with its wait; it has no per-tool rate limit of its own for connector calls (checked: the only limits found are on directory and team routes) | `connectors/http.rs`, `github.rs`, `app/directory.rs` |
| Fake-data mode refuses live connectors, GitHub memory, real content and a database that has held real data | `docs/openshell-any-host-spec.md` §3.6 "`hosted-test`, and fake-data mode" |
| Fakes: in-process fixtures (PM-101 to PM-103, Acme Vault docs pages, Slack threads, Confluence page versions) used by tests, the test stack and the 103 eval tasks | `connectors/fake.rs`, `evals/categories/*/tasks` |

## 2. Target design

### 2.1 Shape

```
sandbox (harness) ──MCP──► tool server ──policy, schema, approval, audit──► backend for the system
                                                                    ├─ built-in connector (until removed)
                                                                    ├─ in-process fake (fake-data mode)
                                                                    └─ MCP client ──HTTPS──► MCP server
```

A new module `crates/toolserver/src/mcp_client/` implements the `Connector` trait for every
system an MCP server serves. `dispatch` and `write` stay as they are; they still call a
`Connector`. So the policy checks, the approval gate, the audit event and the ledger happen
before and after the MCP call exactly as today.

### 2.2 The server registry

One reviewed file per server, `content/mcp/<server>.yaml`, in the content repo:

| Field | Meaning |
| --- | --- |
| `id` | Lowercase, unique. Also the vault provider id and the audit `server` value. |
| `operator` | `pmos` (PM OS runs it), `system_vendor` (the vendor of the system of record), `third_party` |
| `transport` | `streamable_http` only in production (§2.3) |
| `url` | Exact HTTPS URL, or an internal `http://<name>.pmos.internal:<port>/mcp` for a server PM OS runs |
| `identity` | Remote: the TLS host name, and where possible a pinned public-key hash (SPKI). Internal: a pinned address and a bearer token held by the tool server |
| `auth` | `oauth_per_user` (MCP authorization), `service_token` (PM OS's own servers), `none` (public data only) |
| `oauth` | Client id reference (never the secret), authorization server issuer, exact scopes, `resource` value |
| `identity_check` | How a linked account is matched to the PM (a mapped "who am I" tool or the issuer's user info). Required for `oauth_per_user` |
| `data_class` | The highest class its reads return; default `internal` |
| `systems` | Registry systems it serves, such as `[jira, confluence]` |
| `tools` | The approved tools (§2.4) |
| `approved_by`, `pull` | Who approved, and the pull request that added it |

Rules:
- **Added by a proposal, never by a setting.** An Admin proposes a server or tool change
  through Policy Admin, which opens a pull request (`docs/policy-admin.md`, "How a proposal is
  opened"). It merges only with a second reviewer under CODEOWNERS ("Nobody approves their own
  change"). A deployment cannot add a server with an environment variable.
- **Per deployment.** Each team's content repo holds its own records. Secrets (client secrets,
  bearer tokens) are per deployment in `deploy/secrets/`, named by the record, never in it.
- **A `third_party` operator needs the owner's recorded approval** (§3, threat 8).
- **Lint.** `pmos-admin lint` (the content lint in `crates/contract/src/lint`) checks every record:
  known fields, HTTPS for remote URLs, no wildcard hosts, no IP literals for remote servers,
  scopes listed, every tool classified, every mapped registry name present in `tools.yaml`,
  and no registry name served by two servers.

### 2.3 Transports

- **Streamable HTTP over HTTPS** for remote servers; certificate checked against the system
  roots plus the SPKI pin when the record has one. Redirects are refused.
- **Internal HTTP** on the `pmos` Docker network only for servers PM OS runs (`pmos-docs` and
  any self-hosted server), each at a pinned address like the services in
  `crates/policy/src/lib.rs`, with a bearer token.
- **stdio is refused in production.** A stdio server would run inside the tool server's
  container with its environment, which holds `PMOS_VAULT_KEY` and the service tokens. A
  self-hosted server runs as its own container instead. stdio is allowed only for test fakes,
  started with an empty environment.
- **The legacy HTTP+SSE transport is refused.**

### 2.4 Tool discovery and the allowlist

- On connect, the client calls `initialize` and `tools/list`. For each tool it computes a
  **definition digest**: SHA-256 of the canonical JSON of `name`, `description`, `inputSchema`,
  `outputSchema` and `annotations`.
- A tool is usable only if the record lists its exact upstream `name` and `digest`. Every other
  tool is ignored, never listed to a bot.
- **A changed tool is a new, unapproved tool.** If a listed tool's digest differs, or the tool is
  gone, every registry tool mapped to it is unavailable at once: a call answers "this tool
  changed at its server and needs an Admin's review", is audited `denied` with detail
  `tool_changed: <server>.<tool>`, and the Admin view shows it. It comes back only with a new
  approved digest in a merged pull request.
- `notifications/tools/list_changed` triggers a re-list. A re-list can only take tools away.
  The client also re-lists on every new connection and every 10 minutes.
- `serverInfo` (name, version) is recorded and shown to Admins. It is self-declared, so it is
  never used as identity.

### 2.5 Mapping to the existing registry names

Phase 1 keeps every registry name, schema and description. Each approved tool in a record
maps one registry tool to one upstream tool:

```yaml
tools:
  - registry: jira.search          # must exist in content/tools.yaml
    upstream: <upstream tool name>  # to be confirmed per server
    digest: sha256:…
    class: read                    # read | write; absent means write
    args: { … }                    # declarative: rename, fixed values, PM OS-added text
    result: { items: …, id: …, title: …, url: …, body: … }   # where an item's fields are
```

- The model sees PM OS's schema and description from `tools.yaml` and `schemas.rs`, never the
  upstream description. Skills, roles, personas, `never` settings, the persona intersection,
  the policy generator and `interruptOn` keep working unchanged, because they all use registry
  names.
- **The upstream arguments are a pure function** of the approved PM OS arguments plus fixed
  values (the "Drafted with PM OS" marker, the run id, a persona notice). No model text is
  added after approval. A unit test per mapping checks this.
- PM OS-side logic stays in the tool server: `jira.rollup` is computed over the mapped
  search; team-bot JQL and CQL wrapping (`scope.rs`) is applied to the arguments before the
  call, and results outside scope are dropped after it; Confluence's version check and HTML
  escaping run before the write. **A tool whose rule cannot be applied on the arguments or the
  result is not mappable**, and its system stays on the built-in connector.
- The mapping is declarative data, reviewed with the record. If a system needs more than the
  mapping language can say, a small adapter function in `mcp_client/adapters/` is allowed,
  reviewed as code.
- **Phase 2 (slice M11, later):** new tools that have no registry name today, named
  `<server>.<action>` in `tools.yaml`, with a description written by the Admin. Not built
  until phase 1 is done and reviewed.

### 2.6 Read or write

- `class` comes only from the approved record. An upstream `readOnlyHint` or
  `destructiveHint` is ignored for classification: it is the server's claim.
- **An unclassified tool is a write.** Lint refuses a record whose mapped registry tool is a
  `read` in `tools.yaml` but has no `class: read`.
- Every write follows today's path: grant or inbox item, the exact PM OS arguments shown to
  the PM, `EDIT_IMMUTABLE_ARGS`, `expected_args_hash`, `started` audit before the call. The
  item also stores `upstream_args_hash`, the hash of what is sent, so the audit can show what
  left PM OS.
- The approval card names the server ("via `<server id>`").
- The Slack audience check needs channel information and members read with the PM's token.
  Slack writes move to MCP only if the chosen server exposes both as approved reads; until
  then, Slack writes stay on the built-in connector (§6, M8).

### 2.7 Per-user authentication

- **OAuth per PM**, following the MCP authorization specification (OAuth 2.1 with PKCE, the
  server's protected-resource metadata, and a `resource` indicator binding the token to that
  server; to be confirmed against the revision each chosen server supports).
- The authorization server's issuer and endpoints must match the record. Metadata pointing to
  another host is refused. **Dynamic client registration is refused**; each server uses a
  client registered in advance by an Admin (to be confirmed that each candidate allows that).
- Linking reuses today's flow (`/internal/oauth/{provider}/start`, `/oauth/{provider}/callback`,
  HMAC `state`, 10 minutes, once). The callback runs the record's `identity_check` and seals the
  tokens only if the account is the PM's in the user database. A server with no identity
  check can't be linked.
- Tokens live in the vault, bound to (user, `mcp:<server id>`, field); refresh under the
  per-user lock; only a refused grant unlinks. Tokens never reach a sandbox, a log, an audit
  event, a tool result, or another server. The tool server never forwards a token issued for
  one server to another, and never sends its own service or bot tokens to a third party.
- Team bots' read accounts (`bots.<name>`) link to a server the same way, read tools only.
- The requested scopes are the record's, exactly. A grant with fewer scopes than requested
  shows in the link status (`link_statuses`, `connectors/registry.rs`); a write that needs a
  missing scope fails with "reconnect".

### 2.8 Output handling

- A response is capped at 256 KiB before parsing (`Unavailable` above that) and 1,000 items.
- `structuredContent` is preferred; text blocks are used otherwise. Images, audio, resource
  links, embedded resources and `_meta` are dropped.
- The mapping turns the result into `Item`s (`connectors/mod.rs`): source id, title, URL and
  content, so `sources`, `source_titles`, `ref` and citations work as today. Then the existing
  caps apply (12,000 characters a body, 20 results, 50 messages of 2,000).
- Every item is wrapped by `untrusted()`. Nothing in a result changes what the tool server does:
  no field of a result can name a tool to call, a URL to fetch or an approval to make.
- Ids from a result are shape-checked (`sources.rs`, `http.rs`) before they reach a link or
  a later call.
- Reads raise the session to the record's `data_class` (replacing `connector_class`).

### 2.9 Audit and ledger

- The audit event keeps every field and adds `server` (record id) and `tool_digest`. Both are
  metadata, so `event_json` (`app/admin.rs`) serves them to Admins and Auditors. Arguments
  are still only hashed.
- New events: `mcp.connect` (server, outcome, `serverInfo`), `mcp.tool_changed`,
  `mcp.server_request_refused` (§3, threat 9). Detail is fixed text plus ids, never content.
- The ledger adds `served_by` (`builtin`, `fake` or the server id), so the side-by-side period
  can compare outcomes by backend. Nothing else changes; rows hold no text.
- The 90-day purge covers the new events through the existing job; no new delete path.

### 2.10 Rate limits, timeouts, errors

- Per call: 3 s to connect, 10 s for a read, 30 s for a write (today's HTTP timeout). The
  deadline covers connecting, like `docs.rs`.
- New: a per-server concurrency cap (default 8 in flight) and a per-user, per-server rate cap
  (default 60 calls a minute). Over it, the call answers `RateLimited` without reaching the
  server. A server's 429, or a tool error coded as a rate limit, is passed on with its wait.
- Reads retry once after a transport error, on a fresh connection. **Writes are never retried.**
  A write that times out or breaks is "may or may not have happened", as today.
- Connections: one per (user, server) for `oauth_per_user` (the auth header differs), one shared
  for `service_token` and `none`; idle ones close after 5 minutes.
- Errors map to `ConnectorError` as `docs.rs` does: by an error code in structured content
  where the mapping names one, else by HTTP status (401 reconnect, 403 forbidden, 404 not
  found, 429 rate limited, 5xx unavailable). Error text from a server is clipped to 500
  characters and wrapped as untrusted when shown to the model.

## 3. MCP-specific threats

| # | Threat | Control | Test |
| --- | --- | --- | --- |
| 1 | Prompt injection in a tool description | Phase 1: upstream descriptions never reach the model; PM OS's own are used. Phase 2: an Admin-written description; the upstream one is shown only to the Admin as untrusted text | A fake server whose description says "ignore previous instructions"; `tools/list` to the sandbox shows PM OS's description; the string appears in no model-facing output |
| 2 | A server changes a tool after approval ("rug pull") | Digest pinning; re-list on connect, on `list_changed` and every 10 minutes; a mismatch disables the mapped tools | The fake server changes a schema mid-session; the next call is refused, audited `tool_changed`, and no upstream call is made |
| 3 | Tool-name shadowing between servers | Mappings name the server explicitly; a registry name maps to one server (lint); upstream names are never exposed | Two fake servers both offer `search`; each registry tool reaches only its own server; a record mapping a name another record maps fails lint |
| 4 | Over-broad OAuth scopes | Exact scopes in the reviewed record; no write scope without an approved write tool (lint); granted scopes shown in link status | Lint refuses a write scope with read-only tools; a grant with a missing scope makes the write answer "reconnect" |
| 5 | A server exfiltrates arguments | Only mapped arguments are sent (no session token, user database data or other context); size caps; the operator class decides who may receive what (threat 8). A server can still misuse what it receives: that is the trust decision in the record | A mapping test asserts the exact upstream body; a property test that no PM OS token or session id appears in any upstream request |
| 6 | A compromised or impersonated server | TLS with host check and, where possible, an SPKI pin; no redirects; internal servers at pinned addresses with a bearer token; tokens bound to the server by `resource`; results untrusted; writes approved. Not controlled: what a compromised server does with an approved write. The approval gate guarantees what PM OS sends, not what the server does | Wrong certificate, wrong pin, a redirect, and an internal name resolving elsewhere are each refused before any request carries a token |
| 7 | Confused deputy via the tool server's credentials | Every call uses the session user's own token for that server (or a team bot's read account, reads only); never a service token, a bot token or another PM's token; tokens never forwarded between servers | A call for user A with only user B linked fails "connect first"; a team bot's write uses the PM's token; no request to a remote server carries `PMOS_*` tokens |
| 8 | Data leaving the host to a third party | `system_vendor`: data goes to the vendor that already holds it (the same as today's REST calls). `pmos`: stays on the host, apart from the server's own egress. `third_party`: refused unless the owner's recorded approval names the operator and the data classes, kept like the provider record (`docs/openshell-any-host-spec.md` §3.9, conditions 1 to 3, including retention no longer than PM OS's own: Slack text 24 hours). A record's `data_class` above the owner's approval refuses to load | A `third_party` record without the approval reference fails lint; the tool server refuses to start with one |
| 9 | Server-initiated requests: sampling, elicitation, roots | The client declares no capabilities (to confirm that rmcp's `ClientConfig::default()` declares none). Any server request is answered with "method not found", audited `mcp.server_request_refused` with the method name only | The fake server sends `sampling/createMessage` and `elicitation/create`; both are refused and audited; the gateway receives nothing |
| 10 | Resources and prompts features | Not used. `resources/*` and `prompts/*` are never called; server-sent `resources/updated` notifications are dropped. Server log notifications are dropped (they may hold data) | A fake server advertising both; no call is made; dropped notifications leave no trace in logs beyond a count |
| 11 | A server answers a huge or slow response | Size cap, item cap, deadlines, concurrency and rate caps (§2.8, §2.10) | 10 MB response and a server that never answers: both fail within the deadline and other PMs' calls continue |
| 12 | SSRF through OAuth metadata or result URLs | Metadata hosts must match the record; private and link-local addresses refused for remote servers; result URLs are only shown, never fetched | Metadata pointing to `169.254.169.254` or another host is refused |

## 4. Fake-data mode, tests and evals

- **Today's in-process fakes stay** (`connectors/fake.rs`) and stay the default for
  `PMOS_CONNECTORS=fake`. So CI, the test stack and the eval fixtures keep working without any
  change, and the 103 eval tasks stay valid: their fixtures (PM-101 to PM-103, the Acme Vault
  pages, the Slack threads) are the same data.
- **A fake MCP server is added**: `pmos-fake-mcp` (a test binary in `crates/toolserver`, or its
  own crate), serving the same fixtures from the same source as `FakeConnector`, behind the
  same mapping records the live servers use, with upstream tool names chosen for the fakes. A
  new mode `PMOS_CONNECTORS=fake-mcp` routes every mapped system through the MCP client to it.
- **Parity test:** for every registry tool and every fixture-backed eval call recorded in a
  run, `fake` and `fake-mcp` return the same wrapped items, the same audit `sources` and the
  same errors. This runs in CI and needs no model.
- **Fake-data mode** (`PMOS_MODEL_SOURCE=hosted-test`) allows only `fake` and `fake-mcp`, plus
  `pmos-docs` (public docs, as today). The tool server refuses to start with any record whose
  `auth` is `oauth_per_user`, or any `system_vendor` or `third_party` record, and the
  `held_real_data` marker is written the first time any such server is enabled.
- **The first eval after the change** runs on the test stack with `fake-mcp`, all 103 tasks, the
  same settings as the last run, and must show: every task ran (none lost to the MCP layer); no
  category that met its bar in the last run falls below it; tool error rates no higher than
  with `fake`. It is not a benchmark (the judges still have no human labels); it shows that the
  MCP path does not change behaviour.

## 5. Sandbox policy and egress

- **The sandbox policy does not change.** Sandboxes reach only the tool server, the gateway and
  the runtime (`crates/policy/src/lib.rs`). The MCP methods a sandbox may send stay
  `initialize`, `notifications/initialized`, `tools/list`, `tools/call` (`MCP_METHODS`). The
  generated tool allowlist still comes from registry names.
- **The tool server's egress is new.** Today it reaches the internet through the host's
  `HTTPS_PROXY` (`deploy/compose.yaml`, `x-proxy`), and PM OS itself enforces no host list for
  it. With MCP:
  - The MCP client connects only to the hosts in loaded records (the server and its
    authorization server). This is enforced in code, before any proxy.
  - `pmos-admin preflight` gains `H-MCP`: for each record, the host resolves, TLS verifies (and
    the pin matches), the protected-resource metadata names the expected issuer, and an internal
    server's name resolves to its pinned address. It warns when a site's outbound proxy allows
    more than these hosts (it can't check that the proxy enforces anything).
  - A self-hosted server's own egress (to the system of record) is configured where it runs,
    and listed in its record for review.
- **Policy Admin shows it**, read-only: per server, its operator, URL host, auth kind, scopes,
  data class, approved tools with their class and digest, and its last `tools/list` check; and
  per bot, which of its tools are served by which server. The fixed sandbox egress stays shown
  as `fixed`.

## 6. Migration in slices, in build order

Each slice: one agent, one worktree, one branch; the conductor merges and re-runs every check.

| Slice | What it changes | Tests | Reviewer first |
| --- | --- | --- | --- |
| M1 | MCP client layer (`mcp_client/`): server records, lint, loading, discovery, digests, mapping language, output handling, error mapping, caps, refused server requests. A `Connector` for mapped systems. No production record | Threats 1, 2, 3, 9, 10, 11, 12 against in-process fake servers; mapping purity tests | Yes: this spec, then the slice's commit |
| M2 | `pmos-fake-mcp` and `PMOS_CONNECTORS=fake-mcp`; fake records for Jira, Confluence, Slack | Parity test (§4) in CI | No (test-only), reviewed with M1's batch |
| M3 | `docs` on the generic layer: `pmos-docs` as a `pmos` record with `service_token`; `docs.rs` replaced | `docs_e2e.rs` unchanged and passing; `docs-qa` evals unchanged | Light: sent with the batch |
| M4 | Per-user OAuth to MCP servers: metadata checks, pre-registered clients, `resource`, identity check, vault provider `mcp:<id>`, link status, admin connectors page | Threats 4, 6, 7; `live_connectors.rs`-style mocks of an authorization server | Yes |
| M5 | Egress: code allowlist, `H-MCP` preflight, Policy Admin view | Preflight tests with stand-ins; lint rules with a failing and a passing test each | Yes |
| M6 | **Jira first**, behind a per-system switch `PMOS_MCP_SYSTEMS=jira` (default empty): `jira.search`, `jira.read`, `jira.create_issue`, and `jira.rollup` over the mapped search | Every Jira test in `service.rs`, `jira_rollup.rs`, `live_connectors.rs` run against the MCP backend; `jira-draft` evals with `fake-mcp` | Yes (the mapping record and the owner's server choice) |
| M7 | Side by side for Jira on the test stack and, once the owner allows, the pilot: reads called on both backends and compared (counts, ids, field differences recorded as metadata); writes go to one backend only, never both | A comparison report per day; ledger `served_by` split | No; results reported |
| M8 | Confluence, then Slack, each as M6 and M7. Slack writes only if the server exposes channel info and members; else Slack writes stay built-in | The same per system; the Slack audience tests (`slack_reply.rs`) against the MCP backend | Yes, each |
| M9 | Eval run with `fake-mcp` (§4); then the first real-host run with the switch on | §4's criteria | Results sent |
| R-jira, R-confluence, R-slack | **Remove** that system's built-in connector, per system, last | The full workspace checks with the system on MCP only; the eval run | Yes, and the owner's go-ahead |
| M11 | Phase 2: new tools from approved servers under new registry names | Threat 1 phase 2 tests | Yes |

**Why Jira first.** Its reads and its single write have no rule that needs extra upstream data:
the targets are `project`, `issue_type`, `epic`; there is no audience check and no 24-hour text
rule. Its team-bot scope is argument wrapping, which the tool server can apply. Confluence adds
the version check and HTML escaping, and Slack adds the audience check and SLK-8, so they
follow. `docs` (M3) moves before Jira because it is already MCP and PM OS's own server: it proves
the generic layer with no third party.

**The removal step (R-\*), per system, only when all of these hold:** the MCP backend has passed
every test the built-in one passes; the parity test passes; an eval run with the system on MCP
has met §4's criteria; the side-by-side period showed no unexplained difference; the reviewer
has passed the batch; the owner has said go. It removes:
- that system's REST code in `connectors/atlassian.rs` or `connectors/slack.rs`, and
  `connectors/format.rs` and `connectors/http.rs` once no system uses them;
- that provider's entry in `connectors/registry.rs` and its OAuth code in `oauth.rs`, with
  `DEFAULT_*_SCOPES` and the `PMOS_ATLASSIAN_*` or `PMOS_SLACK_*` client settings in `config.rs`;
- that system's arm of the `live` construction in `main.rs`.

**Kept, always:** the registry and schemas, the approval gate and its store, grants,
`EDIT_IMMUTABLE_ARGS`, the vault, the OAuth linking flow (made generic), the audit log and
retention, the ledger, `scope.rs` (as argument and result rules), `rollup.rs`, the source refs and
citations, persona notices, the `Connector` trait, the in-process fakes, `pmos-fake-mcp`, and
GitHub (§9). Git history keeps the removed code; a revert brings it back.

## 7. Requirements

| Id | Requirement | Acceptance check |
| --- | --- | --- |
| MCP-1 | A sandbox never reaches an MCP server; its policy is unchanged | `pmos policy check` passes with no new endpoint; the generated policies are byte-identical before and after M1 to M8 |
| MCP-2 | A server is usable only through a reviewed record in the content repo | No environment variable adds a server; a test with an unlisted URL makes no connection |
| MCP-3 | A tool is usable only with its exact name and digest approved | Unit test: an unlisted tool never appears in `tools/list`; a call to it is `UnknownTool` |
| MCP-4 | A changed or missing tool is unapproved at once | Threat 2's test |
| MCP-5 | Classification comes from the record; unclassified is write | Lint test; a server's `readOnlyHint: true` on an unclassified tool still goes to the inbox |
| MCP-6 | Every write passes the approval gate with the exact PM OS arguments shown | The approval tests in `service.rs` and `approval_draft.rs` pass with the MCP backend |
| MCP-7 | The upstream arguments are a pure function of the approved arguments and fixed values; their hash is stored | Mapping purity tests; `upstream_args_hash` on items and audit |
| MCP-8 | Registry names, schemas and descriptions are unchanged in phase 1 | `content/tools.yaml` and `schemas.rs` unchanged by M1 to M8 |
| MCP-9 | Upstream descriptions never reach the model | Threat 1's test |
| MCP-10 | Per-user OAuth with tokens in the vault, never in a sandbox, log, event, result or other server | Vault tests for `mcp:<id>`; a log and event scan in the M4 tests finds no token |
| MCP-11 | A linked account must be the PM's | Callback test: another person's account is refused and nothing is sealed |
| MCP-12 | Requested scopes are exactly the record's | OAuth start URL test; lint rule for write scopes |
| MCP-13 | Results are untrusted, capped and mapped to items with sources | Output tests in M1; `approval_sources.rs` passes with the MCP backend |
| MCP-14 | Reads raise the session to the server's data class | Test: a `restricted` record's read makes a draft `restricted` |
| MCP-15 | Team-bot read scopes hold on the MCP backend | The `scope.rs` tests re-run through the MCP backend |
| MCP-16 | Audit events add `server` and `tool_digest`, metadata only; Auditors see them | `admin_events.rs` checks the fields and that no argument text appears |
| MCP-17 | The ledger records `served_by` | `ledger.rs` test |
| MCP-18 | Per-server concurrency and per-user rate caps; deadlines; no write retry | Threat 11's test; a write timeout reads "may or may not have happened" |
| MCP-19 | Sampling, elicitation and roots refused; resources and prompts unused | Threats 9 and 10 |
| MCP-20 | stdio and legacy SSE refused in production | Config test: a stdio record refuses to load outside tests |
| MCP-21 | Server identity pinned: TLS host, SPKI where given, internal pinned address | Threat 6's test |
| MCP-22 | `third_party` needs the owner's recorded approval; fake-data mode allows fakes and `pmos-docs` only | Threat 8's test; a fake-data start with a live record refuses |
| MCP-23 | The tool server connects only to record hosts; preflight checks them; Policy Admin shows them | M5 tests |
| MCP-24 | In-process fakes stay; `fake-mcp` gives the same results | Parity test in CI |
| MCP-25 | Old code goes last, per system, after tests, eval, side by side, reviewer and owner | Each R-slice's pull request links the evidence |

## 8. Non-goals

- Giving sandboxes or the harness direct MCP access to anything but the tool server.
- Using MCP servers for the model (requirement i stays in the gateway) or for OpenShell's
  sandbox decisions.
- MCP resources, prompts, sampling, elicitation, roots.
- Moving GitHub memory, member approvals or admin proposals to MCP (§9).
- Automatic approval of any write, or retrying writes.
- Choosing the pilot's servers: that is the owner's (§10).

## 9. What this spec does not decide

- **GitHub** serves PM OS itself (memory repos, content commits, proposals), not bot tools. It
  stays built-in unless the owner wants bots to have GitHub tools; that would be a phase 2
  server.
- **`pmos-docs`** is recommended to stay: PM OS runs it, its data is public, and it already
  speaks MCP. M3 only moves its client onto the generic layer.

## 10. Open questions for the owner

1. **Which MCP servers for the pilot.** Candidates per system, with trade-offs. None has been
   tested by PM OS; every product fact below is to be confirmed.

   | System | Candidate | Who operates it | Where data goes | Auth |
   | --- | --- | --- | --- | --- |
   | Jira and Confluence | The vendor's own hosted MCP server (one has been announced; its tools, scopes, client registration and data residency are to be confirmed) | The vendor | The vendor, which already holds the data | OAuth per user (to be confirmed: pre-registered clients, `resource`) |
   | Jira and Confluence | A self-hosted open-source server (community projects exist; which one, its licence, maintenance and auth support are to be confirmed) | PM OS's operators, on the host | The host, then the vendor's API | Depends on the project: OAuth pass-in or API tokens (API tokens would be per-user secrets to manage; to be confirmed) |
   | Slack | The vendor's own MCP server, if one is offered (to be confirmed it exists and exposes search, threads, channel info and members as user-token tools) | The vendor | The vendor | To be confirmed |
   | Slack | A self-hosted open-source server (to be confirmed which; check Real-time Search support and the members read the audience check needs) | PM OS's operators | The host, then the vendor's API | To be confirmed |
   | Any | A PM OS-built MCP server wrapping today's connector code | PM OS | As today | As today |

   The trade: a vendor-hosted server needs the least operation and sends data nowhere new, but
   PM OS controls neither its tools nor their changes (digest pinning makes changes stop the
   tool, not slip through). A self-hosted server is under PM OS's control but must be patched
   and its egress reviewed. A PM OS-built server meets "an MCP feature" but moves the connector
   code rather than removing it.
2. **Who approves a server and a tool.** Proposed: an Admin proposes; a second Admin approves
   under CODEOWNERS; the owner approves every `third_party` operator and every data-class
   widening. Who besides the owner may approve is open, as for the provider record.
3. **Whether `pmos-docs` stays.** Recommended yes (§9).
4. **Slack writes without an audience check.** If the chosen Slack server can't list channel
   members with the PM's token, should Slack writes stay on the built-in connector (recommended)
   or be turned off?
5. **Side by side with real data.** M7 compares reads on both backends, which doubles reads to
   the vendor during the period. Allowed on the pilot, and for how long?
6. **Removal.** Confirm that each R-slice needs the owner's explicit go-ahead.

## 11. For the reviewer

Check before M1 starts: that §1 matches the code; that no path lets a server's text change
what the tool server does (§2.4, §2.5, §2.8); that §2.6 keeps every write behind the gate with
the arguments the PM saw; the OAuth rules in §2.7 (metadata pinning, no dynamic registration,
`resource`, identity check); threat 6's stated limit; threat 8 against the any-host spec's
§3.9; and that the R-slices are last and per system.
