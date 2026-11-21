# Content and harness admin

The web app's screens for the harness's content layer (scope item e): the roles, bots, skills,
playbooks, context library, templates, reviewer rubrics and master context the harness worker
loads, with their lint status; one file's text; a proposed change validated with the content
lint; and the change opened as a GitHub pull request, by the tool server, with the admin's own
linked GitHub account. Plus a read-only view of what the running harness uses. The API is
`docs/web-api.md`, **Admin: content and harness**; the code is `crates/web/src/app/content.rs`
and `crates/web/src/content_admin.rs`, sharing the proposal machinery with the Policy Admin
(`docs/policy-admin.md`; `crates/policy/src/admin/`, `crates/toolserver/src/proposals.rs`).

## What it shows

**The tree.** Every regular file under the content root (`PMOS_CONTENT_ROOT`), never a symlink
or a hidden file, with its kind (role, bot, persona (a marketplace bot's
`personas/<key>/PERSONA.md`), skill, playbook, template, reviewer, context, master, …), the metadata its front matter has (name, title, version, owner, sensitivity,
last-reviewed), its size, whether a proposal may change it, and its lint status from the
contract crate's lint (`pmos lint`, `docs/contract.md`) run now on the deployed tree. Beside it:
which roles use which skills, which bots are made from which role, and each skill's tools.

**One file.** Its text, its parsed front matter, and a `sha256` the admin sends back with a
change, so a file the server has redeployed since is refused (`409 stale_base`).

**The harness.** Each bot with its role, the model role that role uses, the gateway alias
(`pmos/<model role>`) and the served model name the gateway config maps it to
(`PMOS_WEB_GATEWAY_CONFIG`, read for names only: never an upstream address or a key), its
tools, and whether the bot runtime has it loaded (`GET /v1/bots`, as the `web` client). Worker
health and the content revision each worker loaded are **not shown**: the runtime gives the
`web` client no worker status, and workers don't report a revision.

## Validating and proposing

A change is a list of whole files to write or delete (at most 50). The server checks each path
and text first, then copies the deployed content tree to a scratch directory, applies the
change, and runs the content lint. Because a content change can change a sandbox (a skill's
tools, a role's skills, a bot's role, a new bot), it also regenerates every policy from the copy
and reports the policy files that change and every allowance that widens or narrows, exactly as
the Policy Admin does; a proposal then carries the regenerated policy files too, so CI's
`pmos policy check` passes on it.

Errors are every error in the changed tree (a deleted skill also breaks the role that lists it).
Warnings are those on a changed file and any that are new, so the content's existing warnings
elsewhere don't drown the change. A parser's line number is given when it has one.

Validating is for admins only: a validation's diff shows the current text of every file it
names, so an auditor validating a change to (or the deletion of) a restricted file would read
it.

A proposal is opened as described in `docs/policy-admin.md`, **How a proposal is opened**, with
kind `content`: the tool server checks it again (paths, caps, its own copy, the lint, the
regenerated policies), opens it as the admin, and records `admin.content_proposal`; branch
`pmos/proposal/content/…`, labels `pmos-proposal` and `pmos-proposal:content`. The same branch
protection is a deployment requirement (`docs/policy-admin.md`, **Nobody approves their own
change**).

### Safety

- **Paths:** repository paths inside the content tree (`content/…`), relative, made of letters,
  digits, `.`, `_` and `-`, no `..`, no empty or hidden components, at most 200 bytes and 8 deep.
  Each component on disk is checked with `symlink_metadata` and any symlink is refused, and the
  resolved path must be inside the resolved content root. Never `policies/` (the server
  regenerates those itself), `deploy/`, `.github/`, code, or anything outside. The tool server
  applies the same allow-list again before it opens anything, and neither server accepts the
  repository root as the content folder.
- **Files:** text only: `.md`, `.yaml`, `.yml`, `.txt`, valid UTF-8 (JSON strings always are),
  no NUL, at most 256 KiB. `content/team.yaml` can't be written here: it is retired (people and
  roles are in the user database, and `pmos lint` refuses the file).
- **Requests:** at most 50 files per proposal and 16 MiB per request; the title (120 characters)
  and reason (4,000) are cleaned of control characters; branch names are made by the server;
  every `POST` needs the session and its CSRF token, like every other.
- **Audit** (all at the tool server, never file text):
  - every proposal, before anything is written to GitHub (`started`, then `ok` with the pull
    request or `error`; the paths as a count);
  - every refusal, as `denied` with a fixed note: a role refusal on any admin content route
    (`admin.content_read`, `admin.content_file`, `admin.content_validate`,
    `admin.content_proposals`, `admin.content_proposal`, `admin.harness`; note `role`), a
    proposal `pmos-web` refused (`invalid`, `stale base`, with how many files), and the tool
    server's own refusals (`invalid`, `stale base`, `policy mismatch`, `rate limited`,
    `github not linked`, `not configured`);
  - an **admin reading a PM's personal master file or restricted context**, through the file
    route or by naming it in a validation or proposal (whose diff shows its text):
    `admin.content_sensitive_read`, outcome `ok`, note `personal file` or `restricted context`.
    It is recorded **before** the text is served; if it can't be, nothing is served (`502
    audit_unavailable`). The target is `content:personal:<handle>` for a personal file (whose
    it is matters to an auditor, and handles aren't secret); a restricted file's path is never
    recorded, since a file name can itself be sensitive (`Q3-acquisition-of-x.md`): only that
    one was read, and when, by whom.
  pmos-web's own log has the handle, kind, file count and pull request number, never a token or
  file text.
- **Auditors** read everything in the tree except a PM's own master file
  (`master/people/<handle>/AGENTS.md`) and context marked `sensitivity: restricted`, which they
  see listed but can't open, and whose lint messages they see replaced by "A finding on a file
  your role can't open." (a parser's message can quote the file). They never validate or
  propose. The same read rule (`pmos_policy::admin::access`) is applied to every path a
  validation or proposal names, whoever asks.

## Who may do what

| Permission | Roles | Allows |
| --- | --- | --- |
| `view-content` | admin, auditor | the tree, a file (with the auditor limit above), list proposals, the harness view |
| `edit-content` | admin | validate, open a proposal |

`edit-bots-and-skills` (admins and skill authors) is unchanged and not used here: a skill
author still changes skills through their own pull requests, and only an admin proposes from the
web app.

## Configuration

As in `docs/policy-admin.md`, **Configuration**: for `pmos-web` `PMOS_CONTENT_ROOT`,
`PMOS_WEB_CONTENT_DIR`, `PMOS_WEB_POLICIES_DIR`, `PMOS_WEB_CONTENT_REVISION` and
`PMOS_WEB_POLICIES_ROOT` (the policy files a content change regenerates), plus
`PMOS_WEB_GATEWAY_CONFIG` (the gateway's model config, `deploy/gateway.yaml` in the deploy kit)
for the harness view; `pmos-web` holds no GitHub token, and the old `PMOS_WEB_GITHUB_*` settings
refuse start-up. The tool server opens proposals with each admin's linked GitHub account (the
GitHub App's contents and pull requests write), `PMOS_CONTENT_REPO`, `PMOS_CONTENT_BRANCH`,
`PMOS_PROPOSALS` and `PMOS_PROPOSALS_PER_HOUR`.

## What it is not

- **Not a writer, and not live.** It shows and validates the server's deployed revision (which
  can be behind the default branch) and proposes changes as pull requests; the workers pick up
  content only after the merge and the next content sync or deployment.
- **Not built:** an in-app editor beyond whole-file text (the front end sends whole files);
  renaming (a delete and an add); binary files; worker health and the
  workers' loaded revision (nothing exposes them to `pmos-web`); review status and CI results of
  a proposal (link to the pull request); updating or closing a proposal from the app; a
  persisted rate limit (the tool server's is in memory).
