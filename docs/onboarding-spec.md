# Onboarding and personal memory: build spec

The contract for the onboarding and GitHub-memory work, built in parallel by the tool server and
the Slack bot. Decided 2026-11-14:
- **The bot handles onboarding.** A platform owner (`admin` in `team.yaml`) approves each new
  member in Slack. The tool server then commits the new `team.yaml` to the content repo, so git
  stays the source of truth and the commit records who approved.
- **Personal memory lives in each PM's own private repo in your GitHub org.**
- **Latency is not a priority.** The load test reports numbers but doesn't gate Gate 1.

Shared helpers are in `pmos_contract::onboarding`:
- `propose_handle`, `valid_new_handle`, `personal_bot_name`;
- `render_team_yaml`, `render_personal_bot`, `bot_path`.

`TeamMember` gains an optional `github` login. `TeamFile::admins()` lists the platform owners.

## 1. GitHub (tool server)

**Who acts:** a GitHub App, used only with *user* access tokens. Each PM links GitHub with
OAuth, as for Atlassian and Slack, and every GitHub call acts as a PM:
- **Memory reads and writes** act as the memory's owner.
- **Content-repo commits** act as the platform owner who approved them.

The tool server never uses an installation token.

**The App:**
- **Permissions:**
  - Repository: Contents read/write and Metadata read.
  - Account: Email addresses read.
  - Organization: Members read.
- **Installed on:** your GitHub org, on all repositories or on the repos PM OS uses.
- **Why user tokens are safe:** GitHub limits a user token to what that PM can already reach.

**Configuration** (live mode):

| Variable | Default | Notes |
| --- | --- | --- |
| `PMOS_GITHUB_CLIENT_ID`, `PMOS_GITHUB_CLIENT_SECRET` | required for GitHub | From the GitHub App; without them, GitHub linking and `PMOS_MEMORY=github` are off |
| `PMOS_GITHUB_ORG` | required with GitHub | Your GitHub org's login. Memory repos and the content repo must be in it. |
| `PMOS_GITHUB_URL` | `https://api.github.com` | API base, for mocks |
| `PMOS_GITHUB_AUTH_URL` | `https://github.com` | `/login/oauth/authorize` and `/login/oauth/access_token`, for mocks |
| `PMOS_CONTENT_REPO` | none | `org/name` of the content repo. Required for approving members. |
| `PMOS_CONTENT_BRANCH` | `main` | |
| `PMOS_CONTENT_DIR` | `content` | Where the content root sits in that repo |
| `PMOS_CONTENT_SYNC_SECS` | `300` | How often the content checkout is pulled; `0` turns it off |
| `PMOS_MEMORY` | `store` | `store` keeps memory in Postgres or in memory, as now. `github` puts each PM's memory in their repo and needs the GitHub settings. |

**OAuth provider `github`:**
- **Start:** `GET /internal/oauth/github/start?user=` returns `{url}` like the others. It uses
  `{auth}/login/oauth/authorize?client_id&redirect_uri&state`, with no scope parameter (a
  GitHub App's permissions apply).
- **Callback:** `/oauth/github/callback` exchanges the code at `{auth}/login/oauth/access_token`
  with `Accept: application/json`.
- **Tokens:** the user access token expires in 8 hours and the refresh token in about 6 months.
  Refresh uses `grant_type=refresh_token`.
- **Errors:** GitHub answers some errors as **200** with `{"error": …}`. `bad_refresh_token`
  unlinks the account, like `invalid_grant`; any other error is `Unavailable`, and the
  credentials are kept.
- **Identity check before storing anything:**
  1. `GET /user` gives the login.
  2. `GET /user/memberships/orgs/{org}` must say `state: active`.
  3. If the member has `github` in `team.yaml`, the login must equal it (ignoring case).
     Otherwise one **verified** address from `GET /user/emails` must equal their `email`
     (ignoring case).
  4. Store the login in the credential's metadata.

  On a mismatch nothing is stored and the page says why, as for the other providers.

**GitHub client** (`connectors/github.rs` or `github.rs`):
- **Requests:** with the PM's token from the vault, sending `Accept: application/vnd.github+json`,
  `X-GitHub-Api-Version: 2022-11-28` and a `User-Agent`.
- **Limits:** timeouts, plus 401 (reconnect) and 403/429 rate limits with `Retry-After` and
  `x-ratelimit-reset`.
- **Operations:**
  - get a repo (`private`, `archived`, `owner.login`, `default_branch`, `permissions.push`);
  - get a file (contents API, giving the blob sha);
  - list a tree recursively;
  - get a blob;
  - **commit several files at once** with the git data API: get the ref, then its commit and
    tree, post blobs, post the tree with `base_tree`, post the commit, and `PATCH` the ref with
    `force: false`. A `422` (not a fast-forward) means re-read and retry, up to 3 times.

## 2. Personal memory in GitHub (tool server)

With `PMOS_MEMORY=github`, `memory_read` and `memory_update` use the memory repo of the session's
user, the PM the task is for.

**Memory repo record** (new store, in memory and Postgres, migration `0006_onboarding.sql`): `user → repo`
(`org/name`), plus `verified_at`.

**Set and read the record:**
- `POST /internal/memory/repo {user, repo}` sets it. It checks, with the PM's GitHub token:
  - the owner is `PMOS_GITHUB_ORG`;
  - the repo is `private` and not `archived`;
  - `permissions.push` is true.

  The answer is `200 {repo, verified_at}`. `422 {error, code}` uses the codes `not_in_org`,
  `not_private`, `archived`, `no_push` and `not_found`. `409 {code: "github_not_linked"}`
  means the PM hasn't linked GitHub.
- `GET /internal/memory/repo?user=` returns `{repo, verified_at}` or `404`.

**Layout:** one file per note, `notes/<key>.md`, on the repo's default branch:
```markdown
---
updated: 2026-11-14T10:00:00Z
source: "pm-ana, task 0d3f…"   # the bot (or skill) and the run id that wrote it
---
<note text>
```

**How it works:**
- **Reads:** the tree is listed, and blobs are fetched only for shas that changed since the last
  read, using a per-repo cache.
- **Writes:** commit only that file, with the message `memory: <key> (<bot>, task <run>)`.
- **Re-verification:** the record is checked again if it is more than 10 minutes old. A repo
  that became public, was archived or lost push access refuses reads and writes, with a tool
  error naming the reason.
- **No repo set:** the tool error is `Set up your memory repo first: DM PM OS "setup".`
- **`MemoryStore` change:** `update` takes a `source: &str`, the bot or skill plus the run id.
  The Postgres and in-memory stores ignore it.

## 3. Members (tool server)

**Requests:** a new store in memory and Postgres (migration `0006_onboarding.sql`), with these fields:
- `id` (uuid), `slack_id`, `email`, `name`, `proposed_handle`;
- `status`: `pending`, `approved`, `rejected` or `failed`;
- `requested_at`, `decided_by`, `decided_at`, `handle` (the handle granted), `commit`, `error`.

**Endpoints:** the Slack bot client may call all of these, as may the orchestrator.

- **`POST /internal/members/requests {slack_id, email, name}`:**
  - **Answer:** `201 {request}`, or `200 {request}` with the existing request if one is still
    pending for that Slack id.
  - **Proposed handle:** `propose_handle(email, name, taken)`, where `taken` covers the handles
    in `team.yaml` and in pending requests.
  - **Refusals:**
    - `409 {code: "already_member"}` if the Slack id is in `team.yaml`;
    - `422 {code: "bad_request"}` for an invalid Slack id or email;
    - `429 {code: "too_many_pending"}` past 20 pending requests.
- **`GET /internal/members/requests?status=pending`** returns `{requests: [...]}`.
- **`POST /internal/members/requests/{id}/decision {decision: "approve"|"reject", decided_by,
  handle?, roles?}`:**
  - **Who may decide:** `decided_by` must be an `admin` in `team.yaml`, or the answer is `403`.
  - **Already decided:** `409 {code: "already_decided", request}`.
  - **`reject`:** the status becomes `rejected`.
  - **`approve`:**
    1. **Check the handle.** `handle` defaults to `proposed_handle` and must pass
       `valid_new_handle` and be unused; otherwise `422 {code: "bad_handle"}`. `roles`
       defaults to `[pm]`. From the Slack bot's token, `roles` may hold `pm` and
       `skill-author` only: `admin` is `403`, and a new admin is added by a pull request to
       `team.yaml`. The bot never sends `roles`, and its card says so.
    2. **Check the approver's GitHub.** The admin needs a linked GitHub account with push
       access to `PMOS_CONTENT_REPO`. If not, the answer is
       `409 {code: "github_not_linked"}` or `{code: "no_push"}`, and the request stays pending.
    3. **Commit two files in one commit, with the admin's token, on `PMOS_CONTENT_BRANCH`:**
       - `{dir}/team.yaml`: the current file at the branch head, rendered with
         `render_team_yaml` and the new member (`handle`, `slack-id`, `email`, `roles`);
       - `{dir}/bots/pm-<handle>/BOT.md` from `render_personal_bot`.

       - the bot's sandbox policy, `policies/bots/pm-<handle>.yaml` and
         `policies/bots/pm-<handle>.sandbox.yaml`, generated with `pmos_policy` from the content
         with the new files, under `PMOS_POLICIES_DIR` (default `policies`, from the content
         repo root). Without them CI's `pmos policy check` reports `policy-missing`.

         **POL-2:** the policy is generated unchanged from the already-reviewed `pm-assistant`
         role, and the approving platform owner is the reviewer of record, named in the
         `Approved-by` trailer. Hand changes still go by pull request with a second owner.

       Before committing, check that the new `team.yaml` parses and passes the team lint rules
       (`team-*`). The commit message is `Add <handle> to PM OS` with the trailer
       `Approved-by: <admin> (Slack, request <id>)`.
    4. **Answer** `200 {request}`: the status becomes `approved`, with `handle` and `commit` (the
       sha). The in-memory team already includes the new member when this answer is sent, so
       the welcome checklist's `oauth/.../start` and `onboarding` calls work at once. Then sync
       the content checkout (below).
    5. **If the commit fails** for any other reason, the status becomes `failed` with a readable
       `error`, and the answer is `502 {request}`.
- **`POST /internal/members/{handle}/remove {decided_by}`:** admin only, and an admin may not
  remove themselves.
  1. Commit `team.yaml` without the member, and delete their `pm-<handle>` `BOT.md` and its two
     policy files (git data API, with `sha: null` in the tree). The member is gone from the
     in-memory team when the answer is sent.
  2. Then unlink all their credentials and delete their memory-repo record. The repo itself is
     theirs and is left alone.
  3. Answer `200 {handle, commit}`, or `404` if they aren't a member.
- **`GET /internal/onboarding?user=<handle>`**, or `?slack_id=` for someone not yet a member,
  returns:
  ```json
  {"member": true, "handle": "ana", "admin": false,
   "linked": {"atlassian": true, "slack": false, "github": true},
   "memory_repo": "example-org/pm-os-memory-ana" | null,
   "personal_bot": "pm-ana" | null,          // the bot exists in the loaded content
   "pending_request": null | {request}}       // only for a non-member, by slack_id
  ```
- **OAuth start for the bot:** the Slack bot may also call
  `GET /internal/oauth/{provider}/start?user=`, for team members only. Linking stays
  orchestrator-or-bot; revoking stays orchestrator-only.

**Content sync:**
- **Every `PMOS_CONTENT_SYNC_SECS`, and right after a member commit,** the tool server runs
  `git -C <repo root of PMOS_CONTENT_ROOT> pull --ff-only`. The repo root comes from
  `git rev-parse --show-toplevel`.
- **If HEAD moved,** it reloads like `POST /internal/reindex`, with the same lint gate.
- **Failures** are logged and don't stop the server.
- **Credentials:** the checkout's own git setup is used, such as a deploy key on the Spark. The
  tool server holds no git credentials.
- **Other services** (`pmos-slack`, `pmos-bots`) read the same checkout, so they see the change
  as soon as the pull lands.

**Audit:** every decision and removal is an audit event:
- `client: "bot"`, `user`: the admin;
- `tool`: `members.add` or `members.remove`;
- outcome `approved`, `rejected`, `failed` or `ok`;
- `output_ref`: the commit sha.

## 4. Slack (`pmos-slack`)

**New bot scopes** in `docs/slack-app-manifest.yaml` (keep the existing ones): `users:read`,
`users:read.email` and `im:write`.

**Someone not in `team.yaml` DMs PM OS, or uses `/docs`:**
- **The note:** "PM OS is for the product team." with a **Request access** button
  (`action_id: pmos_request_access`).
- **Click:** call `users.info` for their `real_name` and `profile.email`. With no email, say so
  and stop. Otherwise `POST /internal/members/requests`, then reply "Sent to the platform owners.
  I'll DM you when it's decided."
- **An already-pending request:** say so instead.
- **Telling the admins:** for a new request, DM each admin (`conversations.open`) a card showing
  the name, email and proposed handle, with three buttons. The value of each is the request id:
  - **Approve** (`pmos_member_approve`);
  - **Approve with another handle** (`pmos_member_edit`), which opens a modal with a handle
    field, checked with `valid_new_handle`;
  - **Reject** (`pmos_member_reject`).
- **Card timestamps:** keep each card's `(channel, ts)` in memory by request id, so every
  admin's card can be updated once someone decides.

**An admin decides:**
- **Who may click:** the clicker must be an admin by Slack id in `team.yaml`; otherwise an
  ephemeral "Only platform owners can decide this."
- **Call** the decision endpoint with `decided_by` set to the admin's handle.
- **`github_not_linked` or `no_push`:** an ephemeral reply explains it. For `github_not_linked`
  it adds a **Connect GitHub** button (the OAuth start URL for the admin). Don't change the card.
- **`already_decided`:** update the card to show the outcome.
- **Success:**
  - update every admin's card to
    "✅ Approved by <admin>: `<handle>` added (commit `<short sha>`)", or "Rejected by <admin>";
  - DM the requester. If approved: a welcome and the setup checklist. If rejected: a short note
    saying a platform owner turned it down.

**`setup` (also `status` or `onboarding`), typed alone in a DM by a member:**
- **The checklist:** call `GET /internal/onboarding?user=` and post it:
  1. **Jira and Confluence:** ✅, or a **Connect** URL button (OAuth start, `atlassian`).
  2. **Slack** (so drafts post as you): ✅, or a **Connect** URL button (`slack`).
  3. **GitHub** (for your memory): ✅, or a **Connect** URL button (`github`).
  4. **Memory repo:** ✅ with `org/name`, or a **Set memory repo** button. It opens a modal
     prefilled with `<org>/pm-os-memory-<handle>`, with help text: "Create it in your
     org as a **private** repo first." The org comes from `PMOS_GITHUB_ORG` in the Slack config,
     or is left for the PM to type. On submit, call `POST /internal/memory/repo`. Show its
     `422` and `409` codes as plain sentences in the modal's field error.
  5. **Your bot `pm-<handle>`:** ✅ once it's in the roster. Otherwise "being set up (a minute or
     two)".
- **Refresh:** a **Refresh** button (`pmos_setup_refresh`) re-renders the message in place.
- **The trailer:** "All set" when every step is done.

**Admin commands:** both work in a DM, typed alone, from admins only.
- **`requests`** lists the pending requests with the same cards.
- **`remove <handle>`** posts a confirmation card with **Remove** and **Cancel**
  (`pmos_member_remove_confirm` and `pmos_member_remove_cancel`). **Remove** calls the remove
  endpoint and updates the card.

**Precedence:** these exact commands come before the bot routing. `docs: setup` still goes to
the `docs` bot.

## 5. Bot runtime (`pmos-bots`)

**Reloading the catalog:**
- **When:** every 30 seconds, if a fingerprint of `team.yaml`, `bots/**` and `roles/**` under
  the content root changed (paths, sizes and mtimes, or a hash).
- **On success:** the new catalog replaces the old one atomically.
- **If it fails the lint:** the old catalog stays and an error is logged.
- **Running tasks** keep the bot settings they were claimed with.
- **New bots** show up in `GET /v1/bots`, so a new member's `pm-<handle>` appears without a
  restart.
- **Worker tokens** stay per-bot in config (`PMOS_BOTS_TOKENS`). Starting a new member's worker
  sandbox, and giving it a token, is an operations step for now: document it in
  `docs/bots-runtime.md`.

## Tests (each side, against mocks)

**Tool server, against a mock GitHub built with axum:**
- **OAuth:**
  - linking succeeds, and the refresh rotates the tokens;
  - `bad_refresh_token` unlinks;
  - a mismatched login or email is refused;
  - a non-member of the org is refused.
- **Memory:**
  - writing then reading notes;
  - the cache does no blob fetch when nothing changed;
  - a repo that turns public is refused;
  - a repo that isn't private, isn't in the org, or has no push access is refused when set.
- **Members:**
  - a request, then an approval: the mock repo gets one commit with both files, the YAML parses
    back with the new member, and the trailer names the admin;
  - a non-admin gets `403`;
  - an admin without GitHub gets `409`;
  - a ref conflict retries;
  - removal commits and unlinks;
  - the requests and memory-repo stores work on both backends;
  - the content sync pulls and reindexes (a temp git repo with a bare remote).

**Slack:**
- with a fake Slack API, fake runtime and fake tool server:
  - a non-member clicks Request access, which creates the request and posts the admin cards;
  - an admin approves, which updates the cards and DMs the welcome checklist;
  - a non-admin click is refused;
  - `github_not_linked` gets the Connect button;
  - `setup` renders each state;
  - the memory-repo modal turns errors into field errors;
  - `remove` works with its confirmation.

**Bot runtime:** a content change adds a bot to `GET /v1/bots`; a lint failure keeps the old
catalog.

## As built (2026-11-14)

Differences from the text above, and codes added while building:
- **Migration** `0006_onboarding.sql` (`0005` was taken). It also allows `github` as a
  credentials provider.
- **More error codes:**
  - `403 not_admin` and `403 self_removal`;
  - `404 not_a_member` on removal;
  - `409 owns_bots`: a member who still owns a team bot can't be removed until it has a new
    owner, or the content would stop passing the lint;
  - `409 not_configured`: no content repo or GitHub App;
  - `422 bad_request`;
  - memory repo: `502` or `504 github_unavailable` (GitHub gets 1.8 s, inside the Slack
    modal's ack), and `404 github_off`.
- **`MemoryStore`** returns `MemoryError` (store failure, refusal or unavailable), so a refusal
  reaches the model as itself.
- **Content sync defaults to `0` with fake connectors**, so a developer's checkout isn't pulled.
- **GitHub links PMs only:** a `bots.*` identity is refused.
- **The first note in an empty repo** goes through the contents API, because the git data API
  can't commit to an empty repo.
- **Slack:**
  - the memory-repo modal answers inside its ack, with a 2.5 s cap;
  - `requests` and `remove …` from a non-admin go to their bot as ordinary text;
  - the catalog reload also watches `tools.yaml` and `skills/**`;
  - admin cards are tracked in memory, so after a restart a decision updates only the tapped
    card.

### Slack first-time flow (`PMOS_ONBOARDING_V2=1`)

Off by default; with it off the Slack welcome and checklist (§4) are unchanged. On, a newly
approved member's welcome is two lines and one **Connect Jira and Confluence** button, with no
checklist. Once the onboarding state shows Atlassian linked (read on their next DM or Home open),
PM OS posts one offer with three starter buttons that each create a task for their personal bot
through the normal DM path. Slack and the memory repo are offered when a request first needs them.
After the first finished task, one closing line (Skills view, `nudges off`). Each step is shown
once, kept in `<state dir>/onboarding.json`. `setup` is unchanged. See `docs/slack-home.md`
("First time").
