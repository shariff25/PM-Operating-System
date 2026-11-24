# Policy Admin

The web app's screens for the NVIDIA OpenShell sandbox every bot runs in (scope item c). An
admin sees what each bot's sandbox is allowed, whether the checked-in policy files are current,
and what a proposed change would widen or narrow, then proposes it as a GitHub pull request,
**opened with the admin's own linked GitHub account**. The merge happens in GitHub, by another
person. The API is `docs/web-api.md`, **Admin: policy**; the routes are
`crates/web/src/app/policy.rs` and `crates/web/src/app/proposal.rs`; the logic both servers
share (evaluation, the change set and its re-check, the read rule) is
`crates/policy/src/admin/`; the tool server opens the pull request in
`crates/toolserver/src/proposals.rs`. `pmos-web` holds no GitHub token.

## What it shows

Policies are generated, never edited by hand (`policies/README.md`, `docs/sandbox-policies.md`).
`pmos policy generate` derives each bot's three files (`policies/bots/<bot>.yaml`,
`<bot>.sandbox.yaml`, and the shared `policies/providers/*.yaml`) from the content tree. So the
Policy Admin's **spec** is exactly what generation reads, and nothing more:

| Spec | From | What it changes in the sandbox |
| --- | --- | --- |
| `bots.<bot>.role` | `content/bots/<bot>/BOT.md` | Which role's skills are uploaded and which tools `tools/call` may name. |
| `bots.<bot>.scope` | the same | `personal` also uploads the owner's `master/people/<owner>/AGENTS.md`, if there is one. |
| `bots.<bot>.owner` | the same | For a personal bot, whose master file. |
| `roles.<role>.skills` | `content/roles/<role>/ROLE.md` | Which `skills/<skill>` folders are uploaded read-only, and so which tools are allowed. |
| `skills.<skill>.tools` | `content/skills/<skill>/SKILL.md` `metadata.tools` | The MCP tools `tools/call` may name (`docs.search` becomes `docs_search`). |

Generation reads one more input that the spec doesn't edit: the bot marketplace's catalog,
`content/personas/<key>/PERSONA.md` (`docs/bot-marketplace-spec.md` §4). A personal bot of
`pm-assistant` also uploads, read-only, the skills of every persona's role; its tools stay
`pm-assistant`'s, because the lint refuses a persona whose role has a tool `pm-assistant`
doesn't (`persona-exceeds-base`). So a change to a persona's role skills, made here or in a
content proposal, shows up as new read-only uploads on every `pm-*` bot, and one that would
reach a new tool is refused as invalid content. The catalog itself is changed in a content
proposal, not on this screen; the bots' facts list the uploaded skills, personas' included.

Everything else in a policy is fixed in `crates/policy/src/lib.rs` and shown as `fixed`: egress
only to the tool server (`toolserver.pmos.internal:8710`, MCP on `/mcp`), the gateway
(`gateway.pmos.internal:8740`, `POST /v1/chat/completions`) and the bot runtime
(`bots.pmos.internal:8750`, the worker routes); each of those names only at its own pinned
address (`allowed_ips`: `10.201.77.10/32`, `10.201.77.11/32` and `10.201.77.12/32`, the
constants `TOOLSERVER_ADDR`, `GATEWAY_ADDR` and `BOTS_ADDR`, equal to the `ipv4_address`
literals in `deploy/compose.yaml`); the four MCP methods and three MCP revisions; the
worker binary (`/usr/local/bin/node`); the read-only system paths, the writable workspace,
`/tmp` and `/dev/null`; the `sandbox` identity; Landlock `hard_requirement`; per-bot gateway and
worker providers, and none for the tool server. None of that can be changed from the web app:
it changes with code, reviewed as code.

For each bot the screen shows **allowances**, read field by field from the checked-in policy
file (or, when the server has no policies root, from what the content generates): every
destination, route, MCP method and tool, binary and credential; the read-only and writable
paths; Landlock and the process identity; the launch spec's providers and uploads; and the same
in sentences. Each destination says whether it is `pinned`: its `allowed_ips` exactly its own
service's address, as generated. A destination PM OS doesn't know (`service: null`), extra
ports, a destination that isn't pinned (no `allowed_ips`, another address, a range or a second
entry), a `tls` or `access` setting, or middleware can only come from a hand edit, and the
drift section names the rule it breaks (`net-allowed-ips` for the pin).

**Drift** compares every generated file with the checked-in one (`same`, `differs`, `missing`,
`orphan`) and runs the policy crate's own rule checks (`check_policy`, `check_spec`: the rules of
`pmos policy check`, such as `net-egress`, `fs-writable`, `cred-binding`) on the checked-in
files.

## Validating and proposing

`POST /api/admin/policy/validate` (admins only: its diffs show the current text of the files it
changes) takes any part of the spec. The server:

1. turns it into edits of the bot, role and skill files' front matter, changing only the value
   lines it was given (`role: "…"`, `skills: "…"`, `tools: "…"`) and refusing a layout it can't
   change line by line (`spec-unsupported-format`: change that file as a content proposal);
2. applies them to a scratch copy of the deployed content tree (symlinks and hidden entries are
   not copied) and runs the content lint there;
3. regenerates every policy from the copy and runs the policy crate's rules on the result;
4. compares the new files with the checked-in ones: a text diff per file, and a structured
   diff of allowances per bot. Every addition is `wider`, every removal `narrower`, and a
   changed setting (enforcement, protocol, credential binding, `allowed_ips`, Landlock,
   identity) `changed`; `wider` and `changed` are both marked `widening`. `allowed_ips` is a
   setting, not a set: on an endpoint with a host it narrows where the host may resolve, so
   removing an address could widen, and any change to it is reviewed as one.

Nothing is saved. `POST /api/admin/policy/proposals` does the same again and sends the change to
the tool server, which opens the pull request.

### How a proposal is opened

`pmos-web` does the interactive part; the **tool server** decides, from nothing:

1. **`pmos-web` validates** (the steps above). Errors are `422 invalid` (recorded as a refusal);
   nothing is sent anywhere.
2. **`pmos-web` sends the change set** to the tool server, `POST
   /internal/admin/proposals/open?user=<admin>`, with its token and the admin's signed user
   assertion bound to the exact body (`docs/user-assertion.md`): `{kind, title, reason,
   base_sha, changes: [{path, text | delete, base_blob_sha}]}`, every file the pull request
   would write (the content files, then the regenerated policy files), each with the git blob
   id it was computed from (`null`: the file didn't exist). The tool server refuses the call
   without that assertion **in every assertion mode, `off` included**, and refuses any other
   client (`403`).
3. **Who.** The assertion's subject must be the `user=` named, and an admin in the tool
   server's own user database (`edit-policies` for a policy proposal, `edit-content` for a
   content one), else `403 forbidden`.
4. **Where** (A6). The tool server's `PMOS_CONTENT_DIR` and `PMOS_POLICIES_DIR` must be separate
   folders, neither the repository root nor inside the other: refused at start-up
   (`PMOS_PROPOSALS=off` keeps a root content folder for onboarding only), and again on every
   proposal (`501 not_configured`).
5. **What, checked again** (`pmos_policy::admin::changeset::recheck`), never trusting `pmos-web`:
   the path allow-list (content files under the content folder, and only the generated policy
   files `policies/bots/<bot>.yaml`, `policies/bots/<bot>.sandbox.yaml`,
   `policies/providers/<profile>.yaml`; never `.github/`, `deploy/`, code, `team.yaml`, a
   hidden file or anything at the repository root: `400 path_not_allowed` or `bad_path`); the
   caps (50 content files, 150 policy files, 256 KiB a file, text types, no NUL: `413` or
   `400`); a policy proposal changes only bot, role and skill definitions; each content file's
   `base_blob_sha` must match **the tool server's own** deployed copy (`409 stale_base`); the
   content lint and every policy rule on the changed tree (`422 invalid` with `errors`); and the
   policy files must be **exactly** what the tool server regenerates from the change, no more,
   no fewer, from the same base (`422 policy_mismatch` with `paths`).
6. **As whom.** The admin's own GitHub user token from the tool server's vault (linked like
   Atlassian and Slack, `docs/toolserver.md`, **GitHub**). Without one: `409
   github_not_linked`. So **the pull request's author is the admin.**
7. **Stale base.** Each file must be on the head of `PMOS_CONTENT_BRANCH` exactly as the tool
   server had it (by git blob id), else `409 stale_base` with `paths` and `base`. If GitHub
   truncates the file list (a very large repository), the check is skipped and the answer says
   `stale_check: "skipped"`.
8. **Rate limit:** at most `PMOS_PROPOSALS_PER_HOUR` (default 10) per admin per hour, in the
   tool server's memory (it runs as one replica; a restart forgets): `429 rate_limited` with
   `Retry-After`.
9. **Audit first.** The tool server records `admin.policy_proposal` `started`, with the admin,
   client `web` and the paths (served as a count). If it can't, nothing is written to GitHub
   (`502 audit_unavailable`).
10. **One commit** on the head (git data API: blobs, a tree with `base_tree`, a commit with the
    head as its only parent), **a new branch** `pmos/proposal/policy/<handle>-<time>-<random>`
    made by the tool server (`POST git/refs`, which can only create a ref: the default branch
    is never written), **the pull request** and **the labels** `pmos-proposal` and
    `pmos-proposal:policy`, every call with the admin's token.
11. **Audit again:** `ok` with the pull request (`github:<repo>#<n>` as the target), or `error`.

Every refusal from step 3 on is recorded as `denied`, with a fixed note (`role`, `invalid`,
`stale base`, `policy mismatch`, `rate limited`, `github not linked`, `not configured`).

The pull request's body says who proposed it (their PM OS handle, in code, so GitHub notifies
nobody) and that it was opened with their own account, the reason (in a fenced block, so it
can't format the page or mention anyone), the base branch and commit, the web app's content
revision when known, every allowance change with widenings marked **WIDER** and a reminder that
network changes need a second platform owner's review (POL-2), and the files. The first line is
a marker (`<!-- pmos-proposal kind=policy by=<handle> base=<sha> -->`) the proposals list reads
back.

PM OS never merges, never approves, never enables auto-merge and never pushes to the default
branch. CI's `pmos policy check` runs on the pull request like on any other.

### Nobody approves their own change: a deployment requirement

Because the admin is the pull request's author, **GitHub itself refuses their approval** once
the default branch requires a review. That is what enforces POL-2 ("a second platform owner,
never the author"), so it is a deployment requirement, not an option. On `PMOS_CONTENT_BRANCH`
(classic branch protection or a ruleset):

- **Require a pull request before merging, with at least one approving review.**
- **Dismiss stale approvals when new commits are pushed.**
- **Restrict who can push to the branch** (no direct pushes; nobody may bypass the rules,
  administrators included: "Do not allow bypassing the above settings" / no bypass list).
  Onboarding's member commits (`docs/toolserver.md`, **Members**) go straight to this branch
  as the approving platform owner: give those platform owners a bypass for a new personal
  bot's files only (`content/bots/pm-*/BOT.md` and `policies/bots/pm-*`) through a ruleset, or
  add members' bots by pull request instead (`pmos bot add-personal`). There is no
  `team.yaml` to commit since slice U7.
- **Require status checks:** the content lint and `pmos policy check` (the `ci` workflow).
- Optionally **require review from Code Owners**, with CODEOWNERS on `policies/**` and
  `crates/policy/**` naming the platform owners.

`GET /api/admin/policy` and `GET /api/admin/content` carry `protection: {required_reviews,
verified}`: what the tool server could read of this with the viewing admin's own account
(classic protection needs the GitHub App's **Administration: read** permission; rulesets are
read with **Metadata: read**). It is best effort and never blocks anything; the front end
warns when `verified` is `false` or `required_reviews` is `0`.

## Who may do what

| Permission | Roles | Allows |
| --- | --- | --- |
| `view-policies` | admin, auditor | `GET /api/admin/policy`, list proposals |
| `edit-policies` | admin | validate, open a proposal |

Every role refusal on these routes is recorded at the tool server (`admin.policy_read`,
`admin.policy_validate`, `admin.policy_proposals`, `admin.policy_proposal`, outcome `denied`,
note `role`). An auditor never sees a lint message on a file they can't open
(`generate_errors` on a PM's personal master file or restricted context say only "A finding on a
file your role can't open.").

`approve-policy-change` (POL-2: a second platform owner, never the author) stays in GitHub:
the reviewer approves the pull request there, and branch protection makes GitHub refuse the
author's own approval (above).

## Configuration

**`pmos-web`** (it reads and validates; it holds no GitHub token):

| Variable | Default | |
| --- | --- | --- |
| `PMOS_CONTENT_ROOT` | `content` | The deployed content tree. |
| `PMOS_WEB_POLICIES_ROOT` | `policies` beside the content root, if it exists | The checked-in policy files. Without one, drift is `null` and allowances come from generation. |
| `PMOS_WEB_CONTENT_REVISION` | none | The deployed content's commit, when the content root has no `.git` (a container image). |
| `PMOS_WEB_CONTENT_DIR`, `PMOS_WEB_POLICIES_DIR` | `content`, `policies` | Where those trees are in the repository. Neither may be the repository root (set but empty is refused) nor inside the other; match the tool server's. |

**Removed** (each refuses start-up if set, so none lingers): `PMOS_WEB_GITHUB_TOKEN`,
`PMOS_WEB_GITHUB_REPO`, `PMOS_WEB_GITHUB_API`, `PMOS_WEB_GITHUB_BASE`,
`PMOS_WEB_PROPOSALS_PER_HOUR`, and `PMOS_WEB_GITHUB_CONTENT_DIR` /
`PMOS_WEB_GITHUB_POLICIES_DIR` (renamed as above). Revoke the token they used.

**The tool server** (it opens the pull request):

| Variable | Default | |
| --- | --- | --- |
| `PMOS_GITHUB_CLIENT_ID`, `PMOS_GITHUB_CLIENT_SECRET`, `PMOS_GITHUB_ORG` | none | The GitHub App each PM links (`docs/toolserver.md`, **GitHub**). |
| `PMOS_CONTENT_REPO` | none | `org/name`: where proposals go. With it, proposals are on. |
| `PMOS_CONTENT_BRANCH` | `main` | The base every proposal is made on and checked against. |
| `PMOS_CONTENT_DIR`, `PMOS_POLICIES_DIR` | `content`, `policies` | Separate folders, neither the repository root (A6). |
| `PMOS_PROPOSALS` | `on` with `PMOS_CONTENT_REPO` | `off` turns proposals off (and allows a root content folder for onboarding). |
| `PMOS_PROPOSALS_PER_HOUR` | `10` | Per admin, 1 to 100, in memory. |
| `PMOS_POLICIES_ROOT` | the policies folder of the checkout `PMOS_CONTENT_ROOT` is in | The checked-in policy files the re-check compares with. |
| `PMOS_WEB_TOKEN`, `PMOS_ASSERTION_KEYS` (`web:toolserver`) | none | `pmos-web`'s token and assertion key: proposals need both. |

### The admin's GitHub account: what it needs

Proposals act with the admin's own **GitHub App user token** (never an installation token, and
never a shared one), so GitHub limits it to what that admin can already do, and GitHub's own
rules apply to them as the author. The GitHub App (`docs/toolserver.md`, **Setting up the GitHub
App**) needs these repository permissions:

- **Contents: read and write** (blobs, trees, the commit and the proposal branch);
- **Pull requests: read and write** (open, list and label pull requests; the labels endpoint is
  the issues one, which GitHub accepts for a pull request with pull requests write. If refused,
  the pull request still opens, with `labels_added: false`, and the list still finds it by its
  branch);
- **Metadata: read** (always granted; also reads rulesets for the protection check);
- **Administration: read**, optional: only to read classic branch protection for the
  `protection` field. Without it, `verified` is `false` unless a ruleset requires reviews.

Each admin who proposes links GitHub (`setup` in a Slack DM, the App Home's **Accounts**) and
needs write access to the content repository. Branch protection (above) stops that access from
reaching the default branch without someone else's review.

## OpenShell's prover and the sandbox's denials

Built before review (owner's instruction of 2026-11-24); design and status in
`docs/openshell-prover-and-sandbox-audit-spec.md`. Nothing here has run against a real OpenShell.

- **Prover row**, on every bot card: the newest proof `deploy/pmos-admin policy prove` recorded
  on the host for that bot's running sandbox (`GET /internal/admin/policy/proofs`, admins and
  auditors). It reads **Proved (5 of 5 domains)** only for a proof whose status is `proved` and
  which is current: its `candidate_sha256` is the checked-in policy's (the SHA-256 of its
  canonical form) and it is not older than the sandbox's last `sandbox.policy_loaded`. Anything
  else says what it is: **Stale**, **Exceeds boundary: …**, **Not checked: unsupported (…)**,
  **Not checked: inconclusive**, **Prover error**, **Prover not available on the host**,
  **Prover version too old (or different)**, or that no proof is recorded, or that the tool
  server didn't answer. None of them is a pass. A proof is of the **network view** (the
  tool-server endpoint's MCP rules removed, which only widens), and every card lists what a
  proof never covers (readable paths, the MCP tool allowlist, credentials, uploads, name
  resolution, what a running sandbox enforces).
- **Not run here:** `pmos-web` runs no prover; the propose and proposals parts say "The prover
  runs on the pull request (CI check `policy-prover`). Not run here." Reading that check into
  the proposals list (slice A6) needs the GitHub App's **Checks: read** and is not built.
- **Sandbox denials**, on every bot card: its latest five denials, findings and deny summaries
  OpenShell recorded in the last 7 days (rule, destination, time), from
  `GET /internal/admin/sandbox`. An Admin sees the host, port, method and path; an Auditor the
  destination class (`pmos service`, `private`, `public`, `always blocked`, `unknown`) and port.
  The card says when the collector is off, silent (no record is not proof of none), or unknown.

## MCP servers

Built before review (owner's instruction of 2026-11-24); design in `docs/mcp-servers-spec.md`
§5. A read-only section of the overview, after the bot cards, for Admins and Auditors alike
(the `mcp` field of `GET /api/admin/policy`, metadata only). One card per server in use: the
systems it serves (and any its record lists that stay on the built-in connector), who runs it
(the vendor's hosted server, or PM OS, self-hosted: the record does not tell an open-source
server from one PM OS built), the host it is reached at, how PMs sign in, the data class it may
receive, the permissions it asks for, its last tool check (passed, failed with the reason, or
not checked yet) and each approved tool with read or write, a short digest and who approved it.
A tool changed or gone at the server is flagged as unusable until an Admin approves the new
definition; a card with a write tool says every write still waits for the PM's approval. With
no server configured it says that the built-in connectors serve everything; when the tool
server doesn't answer, that it is not known. It says it cannot show what a server does with
what it is sent. Nothing here adds or approves a server: that is a content pull request on the
record (`content/mcp/<server>.yaml`); an Admin gets a link to the Content page.

## What it is not

- **Not a writer.** It never edits a file on the server, applies a policy to OpenShell
  (`openshell policy set` stays a step on the host after the merge, `docs/sandbox-policies.md`)
  or restarts a sandbox.
- **Not live.** It shows the server's deployed revision, which can be behind the default branch
  until the next deployment. Proposals made from a stale copy are refused (`409 stale_base`).
- **Not built:** editing the fixed parts (hosts, routes, binaries: those are code); creating or
  removing bots, roles or skills from the policy screen (use a content proposal); review status
  and CI results in the proposals list (link to the pull request); closing or updating a
  proposal from the app (do it in GitHub); a persisted rate limit (it is in the tool server's
  memory, and a restart resets it); enforcing branch protection from PM OS (it is read and
  reported, best effort; GitHub enforces it).
