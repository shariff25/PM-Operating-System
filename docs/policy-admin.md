# Policy Admin

The web app's screens for the NVIDIA OpenShell sandbox every bot runs in (scope item c). An
admin sees what each bot's sandbox is allowed, whether the checked-in policy files are current,
and what a proposed change would widen or narrow, then proposes it as a GitHub pull request. The
merge happens in GitHub, by a person. The API is `docs/web-api.md`, **Admin: policy**; the code is
`crates/web/src/app/policy.rs` and `crates/web/src/policy_admin.rs`, with the shared proposal
machinery in `crates/web/src/app/proposal.rs`, `crates/web/src/proposals.rs` and
`crates/web/src/github.rs`.

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

Everything else in a policy is fixed in `crates/policy/src/lib.rs` and shown as `fixed`: egress
only to the tool server (`toolserver.pmos.internal:8710`, MCP on `/mcp`), the gateway
(`gateway.pmos.internal:8740`, `POST /v1/chat/completions`) and the bot runtime
(`bots.pmos.internal:8750`, the worker routes); the four MCP methods and three MCP revisions; the
worker binary (`/usr/local/bin/node`); the read-only system paths, the writable workspace,
`/tmp` and `/dev/null`; the `sandbox` identity; Landlock `hard_requirement`; per-bot gateway and
worker providers, and none for the tool server. None of that can be changed from the web app:
it changes with code, reviewed as code.

For each bot the screen shows **allowances**, read field by field from the checked-in policy
file (or, when the server has no policies root, from what the content generates): every
destination, route, MCP method and tool, binary and credential; the read-only and writable
paths; Landlock and the process identity; the launch spec's providers and uploads; and the same
in sentences. A destination PM OS doesn't know (`service: null`), extra ports or IP ranges, a
`tls` or `access` setting, or middleware can only come from a hand edit, and the drift section
names the rule it breaks.

**Drift** compares every generated file with the checked-in one (`same`, `differs`, `missing`,
`orphan`) and runs the policy crate's own rule checks (`check_policy`, `check_spec`: the rules of
`pmos policy check`, such as `net-egress`, `fs-writable`, `cred-binding`) on the checked-in
files.

## Validating and proposing

`POST /api/admin/policy/validate` takes any part of the spec. The server:

1. turns it into edits of the bot, role and skill files' front matter, changing only the value
   lines it was given (`role: "…"`, `skills: "…"`, `tools: "…"`) and refusing a layout it can't
   change line by line (`spec-unsupported-format`: change that file as a content proposal);
2. applies them to a scratch copy of the deployed content tree (symlinks and hidden entries are
   not copied) and runs the content lint there;
3. regenerates every policy from the copy and runs the policy crate's rules on the result;
4. compares the new files with the checked-in ones: a text diff per file, and a structured
   diff of allowances per bot. Every addition is `wider`, every removal `narrower`, and a
   changed setting (enforcement, protocol, credential binding, Landlock, identity) `changed`;
   `wider` and `changed` are both marked `widening`.

Nothing is saved. `POST /api/admin/policy/proposals` does the same again and then opens the pull
request.

### How a proposal is opened

1. **Validate again.** Errors are `422 invalid`; nothing is sent anywhere.
2. **The base:** `PMOS_WEB_GITHUB_BASE`, or the repository's default branch, at its head now.
3. **Stale base.** Each file the pull request writes (the content files and the regenerated
   policy files) must be on that commit exactly as the server had it when it computed the change
   (compared by git blob id). If not, `409 stale_base` with the paths: the deployed content is
   behind (or ahead of) the branch, and the change would be made on the wrong text. If GitHub
   truncates the file list (a very large repository), the check is skipped and the answer says
   `stale_check: "skipped"`.
4. **Rate limit:** at most `PMOS_WEB_PROPOSALS_PER_HOUR` (default 10) per admin per hour.
5. **Audit first.** The tool server records `admin.policy_proposal` with outcome `started`, the
   admin, client `web` and the paths (`POST /internal/admin/proposals`, signed for the admin;
   the tool server checks their role itself). If it can't, nothing is written to GitHub
   (`502 audit_unavailable`).
6. **One commit** on the base (git data API: a tree with `base_tree`, then a commit with the base
   as its only parent), **a new branch** `pmos/proposal/policy/<handle>-<time>-<random>` made by
   the server (`POST git/refs`, which can only create a ref: the default branch is never
   written), **the pull request**, and **the labels** `pmos-proposal` and
   `pmos-proposal:policy`.
7. **Audit again:** `opened` with the pull request (`github:<repo>#<n>` as the target), or
   `failed`.

The pull request's body says who proposed it (their PM OS handle, in code, so GitHub notifies
nobody: the GitHub author is the token's identity), the reason (in a fenced block, so it can't
format the page or mention anyone), the base branch and commit, the server's content revision
when known, every allowance change with widenings marked **WIDER** and a reminder that network
changes need a second platform owner's review (POL-2), and the files. The first line is a marker
(`<!-- pmos-proposal kind=policy by=<handle> base=<sha> -->`) the proposals list reads back.

PM OS never merges, never approves, never enables auto-merge and never pushes to the default
branch. CI's `pmos policy check` runs on the pull request like on any other.

## Who may do what

| Permission | Roles | Allows |
| --- | --- | --- |
| `view-policies` | admin, auditor | `GET /api/admin/policy`, validate, list proposals |
| `edit-policies` | admin | open a proposal |

`approve-policy-change` (POL-2: a second platform owner, never the author) stays in GitHub:
the reviewer approves the pull request there, and branch protection on `policies/**` and
`crates/policy/**` should require it (`policies/README.md`).

## Configuration

| Variable | Default | |
| --- | --- | --- |
| `PMOS_CONTENT_ROOT` | `content` | The deployed content tree. |
| `PMOS_WEB_POLICIES_ROOT` | `policies` beside the content root, if it exists | The checked-in policy files. Without one, drift is `null` and allowances come from generation. |
| `PMOS_WEB_CONTENT_REVISION` | none | The deployed content's commit, when the content root has no `.git` (a container image). |
| `PMOS_WEB_GITHUB_REPO` | none | `owner/name`. With the token, turns proposals on; without both, proposals answer `501 not_configured` and everything else works. |
| `PMOS_WEB_GITHUB_TOKEN` | none | See below. Never logged; `Debug` prints `<redacted>`; sent only in `Authorization` to the API base; redirects are not followed. |
| `PMOS_WEB_GITHUB_API` | `https://api.github.com` | GitHub Enterprise, or a mock in tests. `https` unless the host is loopback. |
| `PMOS_WEB_GITHUB_BASE` | the default branch | The branch proposals target. |
| `PMOS_WEB_GITHUB_CONTENT_DIR`, `PMOS_WEB_GITHUB_POLICIES_DIR` | `content`, `policies` | Where those trees are in the repository. |
| `PMOS_WEB_PROPOSALS_PER_HOUR` | `10` | Per admin, 1 to 100. |

### The GitHub token: least privilege

Use a **fine-grained personal access token or a GitHub App installation token restricted to the
one repository**, with exactly:

- **Contents: read and write** (create blobs, trees, commits and the proposal branch);
- **Pull requests: read and write** (open and list pull requests, and label them: the labels
  endpoint is the issues one, which GitHub documents as accepting issues or pull requests write.
  If a GitHub version refuses it, the pull request still opens, with `labels_added: false`, and
  the list still finds it by its branch);
- **Metadata: read** (always granted).

Nothing else: no administration, workflows, actions, secrets or organization access. Protect
the default branch (required reviews, no direct pushes, CODEOWNERS on `policies/**` and
`crates/policy/**`) so that even this token can't bypass review. A classic token with `repo`
scope works but reaches every repository its user can; don't.

## What it is not

- **Not a writer.** It never edits a file on the server, applies a policy to OpenShell
  (`openshell policy set` stays a step on the Spark after the merge, `docs/sandbox-policies.md`)
  or restarts a sandbox.
- **Not live.** It shows the server's deployed revision, which can be behind the default branch
  until the next deployment. Proposals made from a stale copy are refused (`409 stale_base`).
- **Not built:** editing the fixed parts (hosts, routes, binaries: those are code); creating or
  removing bots, roles or skills from the policy screen (use a content proposal); review status
  and CI results in the proposals list (link to the pull request); closing or updating a
  proposal from the app (do it in GitHub); a rate limit shared across `pmos-web` processes (it is
  in memory, per process, and a restart resets it); checking that the token has exactly the
  scopes above (GitHub doesn't report fine-grained permissions to the token itself).
