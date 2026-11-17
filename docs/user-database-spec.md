# User database: the source of users and roles

Status: **design spec, not built.** An independent security reviewer reviews it before any slice
starts. Owner's decision (scope item b in `CLAUDE.md`): the PM OS user database is the source
of users and roles, not `content/team.yaml`. Roles: Admin, Product Manager, Reviewer, Auditor.
A Reviewer recommends and the draft's owner confirms. An Auditor sees metadata only.

Each section gives a recommendation, then the alternatives considered. File references are to
`claude/web-app` at `25f7cc9`.

## 0. What reads `team.yaml` today

Every service reads the file from the shared content checkout
(`deploy/compose.yaml:107-114`: the tool server mounts it read-write and pulls it; the runtime,
Slack, console, web and workers mount it read-only, e.g. `:167`, `:201`). The model is
`TeamFile`/`TeamMember` (`crates/contract/src/model.rs:376-436`).

| Where | What it decides from the file | Reference |
| --- | --- | --- |
| Tool server `Team` | Who may have an MCP session, decide an approval, link an account. **Enforced only with live connectors**; with fake connectors anyone with a valid handle is in. | `crates/toolserver/src/team.rs:39-49`, `main.rs:73-75`, `service.rs:569,598,1765` |
| Tool server onboarding | Admin check for member decisions and removal; writes the new `team.yaml` | `onboarding.rs:198-200,453,711` |
| Tool server admin reads | Viewer's roles against the permission matrix (re-checked, not taken from the assertion) | `app/admin.rs:128-133` |
| Tool server account linking | Slack id and Atlassian email must match the member; GitHub login or verified email | `oauth.rs:397-434,462-493` |
| Tool server audience check | A restricted draft is blocked if anyone in the channel isn't in `team.yaml` | `service.rs:1886-1958` |
| Runtime catalog | Team bots serve members only; set level; routines pause when the owner may not use the bot. **No role check: an auditor-only member can ask team bots here.** | `crates/bots/src/catalog.rs:79,117,165-178`, `runtime.rs:860,1039` |
| Slack | Slack id → handle on every event (re-read per event; a parse error means an empty team); which admins get join cards | `crates/slack/src/commands.rs:55-66`, `members.rs:312`, 25 call sites |
| Web | Sign-in by handle (dev) or email (Entra); every request re-reads the file; `/api/me` roles and permissions; admin team page | `crates/web/src/auth.rs:101-120`, `app.rs:536-570,779,821`, `app/admin.rs:417-470` |
| Console | Viewer by handle or email; only `admin` vs not. **An auditor is treated as a PM there.** | `crates/console/src/auth.rs:76-110` |
| CLI | `pmos member add` writes `team.yaml` and the personal bot; `pmos lint` checks the `team-*` rules and bot owners | `crates/cli/src/main.rs:156-211`, `crates/contract/src/lint.rs:727-771,808-933` |
| Test stacks | `pmos member add` into a copy of the content | `scripts/claude-test-stack.sh:88`, `deploy/pmos-admin:133-145` (eval stack: `evals`, `ben`, `cy`) |
| Evals | `requested_by: evals` must be a member | `evals/config.yaml:6-8` |
| Harness | Nothing: it gets `requested_by` from the runtime | `harness/src/worker.ts:102` |

**How Slack onboarding writes today.** Not a pull request: on approval the tool server makes one
commit straight to the content branch with the approving admin's GitHub token (`create_commit`
then `update_ref`, `onboarding.rs:860-945`). The commit has the new `team.yaml`,
`bots/pm-<handle>/BOT.md` and the two generated sandbox policy files, with an `Approved-by`
trailer. The in-memory team is swapped at once (`onboarding.rs:621-624`). From the Slack bot
client an `admin` grant is refused `403 admin_by_pull_request` (`onboarding.rs:544-562`;
`docs/security-fixes-spec.md` S2). Removal commits `team.yaml` without the member, deletes the
bot files, unlinks every account and drops the memory-repo record (`onboarding.rs:703-842`).

**Member fields today.**

| Field | Stored in | Written by |
| --- | --- | --- |
| `handle`, `slack-id`, `roles` | `team.yaml` | Onboarding approval, `pmos member add`, hand edits by PR |
| `email` (Atlassian email *and* Entra sign-in name) | `team.yaml` | Onboarding (from Slack `users.info`), CLI, PR |
| `github` | `team.yaml` | CLI or PR only; onboarding writes none |
| Display name | `member_requests.name` only | Slack `users.info` at request time; never reaches `team.yaml` (`app.rs:817-820`) |
| Level | `pmos_bots.member_settings` | The PM, through the runtime |
| Personal bot | `content/bots/pm-<handle>/` and its policies | Onboarding commit |
| Memory repo, linked accounts | Tool server `memory_repos`, `credentials` | The PM |
| Timezone | Not per member: `PMOS_DEFAULT_TZ` | — |
| Entra `oid`/`tid` | Not stored; the ID token claims don't even parse `oid` (`entra.rs:368-380`) | — |

**Findings that this spec fixes on the way.** (1) The Reviewer rule is not enforced anywhere:
`RecommendOnApproval` and `ViewOthersApprovals` exist only in the matrix
(`permissions.rs:44,78`); no route uses them. (2) The runtime and Slack don't check `AskBot`,
and the console knows only admin vs not, so an auditor can ask bots there. (3) An MCP session
outlives its member's removal by up to 60 minutes: `ToolService::session` re-checks the bot,
not the member (`service.rs:623-634`). (4) `skill-author` is a fifth role whose only permission,
`EditBotsAndSkills`, is checked nowhere.

## 1. Where the database lives and who owns it

**Recommendation: the tool server owns users and roles,** in its existing Postgres database
`pmos_toolserver`.

Why the tool server:

- It already makes the decisions that matter: approvals, account linking, the audience check
  for restricted drafts, admin and auditor reads, onboarding. Owning the data means those
  checks read it in the same database, in the same transaction as the action.
- It owns the append-only audit log (`migrations/0001_init.sql:70-94`), so a role change and its
  audit event are written together.
- It is the receiver of signed user assertions, not a signer. A compromised `pmos-web` or
  console cannot write roles directly; it can only call routes that re-check the actor
  (§6, §11).
- Every other service already holds a token for it.

| Alternative | Why not |
| --- | --- |
| `pmos-web` owns it (the sketch in `docs/web-client.md`) | The web backend is the most exposed service and signs assertions. Owning roles there means a web compromise is a role compromise. The tool server would also have to ask web for every decision. |
| A new small users service | Cleanest boundary, but one more service, database, token set and failure mode, and every tool server decision becomes a network hop. Worth it only with several tool server replicas or another product sharing users. Revisit then. |
| The runtime | It doesn't make the security decisions (approvals, linking, audit), and it has its own database. |

### Schema (migration `crates/toolserver/migrations/0012_users.sql`)

```sql
CREATE TABLE users (
    handle        text PRIMARY KEY
                  CHECK (handle ~ '^[a-z0-9][a-z0-9._-]{0,63}$' AND handle NOT LIKE 'bots.%'),
    display_name  text NOT NULL DEFAULT '' CHECK (length(display_name) <= 100),
    email         text CHECK (email = lower(email) AND length(email) <= 254),
    status        text NOT NULL CHECK (status IN ('active', 'deactivated')),
    created_at    timestamptz NOT NULL,
    created_by    text NOT NULL,           -- a handle, or 'import' / 'host'
    deactivated_at timestamptz,
    deactivated_by text
);
CREATE UNIQUE INDEX users_email ON users (email) WHERE email IS NOT NULL;

CREATE TABLE user_roles (
    handle     text NOT NULL REFERENCES users ON DELETE RESTRICT,
    role       text NOT NULL CHECK (role IN ('admin', 'pm', 'reviewer', 'auditor')),
    granted_at timestamptz NOT NULL,
    granted_by text NOT NULL,
    PRIMARY KEY (handle, role)
);
CREATE INDEX user_roles_role ON user_roles (role);

-- Keys that point at a person: sign-in, Slack, and the accounts a link must match.
CREATE TABLE user_identities (
    handle    text NOT NULL REFERENCES users ON DELETE RESTRICT,
    kind      text NOT NULL CHECK (kind IN ('slack', 'entra', 'atlassian', 'github')),
    issuer    text NOT NULL DEFAULT '',   -- entra: tid; slack: workspace id; else ''
    subject   text NOT NULL,              -- slack member id; entra oid; email; github login
    linked_at timestamptz NOT NULL,
    linked_by text NOT NULL,
    UNIQUE (kind, issuer, subject),       -- one person per key
    UNIQUE (handle, kind)                 -- one key of each kind per person
);

-- Authoritative history of every change above, written in the same transaction. Append-only
-- (same trigger as audit_events).
CREATE TABLE role_changes (
    id        bigserial PRIMARY KEY,
    at        timestamptz NOT NULL,
    actor     text NOT NULL,
    client    text NOT NULL CHECK (client IN ('web', 'bot', 'host', 'import')),
    target    text NOT NULL,
    change    text NOT NULL CHECK (change IN ('create', 'grant', 'revoke', 'deactivate',
              'reactivate', 'link', 'unlink', 'import', 'assign', 'unassign')),
    role      text,
    detail    text             -- fixed formats only (§10), never free text
);
CREATE INDEX role_changes_target ON role_changes (target, at DESC);

CREATE TABLE reviewer_assignments (           -- §8
    reviewer   text NOT NULL REFERENCES users,
    pm         text NOT NULL REFERENCES users,
    assigned_by text NOT NULL,
    assigned_at timestamptz NOT NULL,
    PRIMARY KEY (reviewer, pm),
    CHECK (reviewer <> pm)
);

CREATE TABLE approval_recommendations (       -- §8
    id          text PRIMARY KEY,
    approval_id text NOT NULL REFERENCES approvals ON DELETE CASCADE,
    reviewer    text NOT NULL REFERENCES users,
    verdict     text NOT NULL CHECK (verdict IN ('approve', 'reject', 'changes')),
    note        text CHECK (length(note) <= 1000),
    created_at  timestamptz NOT NULL,
    withdrawn_at timestamptz
);
CREATE UNIQUE INDEX one_live_recommendation
    ON approval_recommendations (approval_id, reviewer) WHERE withdrawn_at IS NULL;

-- Bumped by trigger on any change to users, user_roles, user_identities or
-- reviewer_assignments: the ETag of the directory (§2).
CREATE TABLE directory_version (one boolean PRIMARY KEY DEFAULT true CHECK (one), v bigint NOT NULL);
```

**Invariants in the database, not only in code:**

- **The last admin** (§5): a deferred constraint trigger on `users` and `user_roles` refuses a
  commit that leaves users in the table but no `active` user with `admin`. Every write to these
  tables first takes `pg_advisory_xact_lock(hashtext('pmos.users'))`, so two concurrent
  demotions can't both pass the check under READ COMMITTED.
- **Admin and Auditor together** (§6): the same trigger refuses a user holding both.
- **No deletes.** Users are deactivated, never deleted, in the MVP. `ON DELETE RESTRICT` keeps
  history intact. Hard deletion (erasure) is out of scope (§12).
- `email` is stored lower-case and unique across all users, active or not, so a deactivated
  person's email can't be silently re-used for an Entra first link (§7).

**Migrations** run at tool server start, as today (`pg.rs:89`). The in-memory store
(`PMOS_DEV=1` without a database URL) gets the same `UserStore` trait and enforces the same
invariants in code; the Postgres tests prove the trigger versions.

### What stays in content, what moves

| Stays in content (reviewed by pull request) | Moves to the database |
| --- | --- |
| Bot definitions, including personal bots `bots/pm-<handle>/BOT.md`, their roles and generated sandbox policies | Who exists, their status, roles, display name, email |
| `master/people/<handle>/` context folders | Slack id, Entra `oid`+`tid`, Atlassian email, GitHub login |
| A bot's `owner` field (a handle) | Reviewer assignments, recommendations |

`team.yaml` is removed from the content repo at cut-over, and `pmos lint` then refuses it
(`team-file-retired`, an error), so it can't come back as a second source. Lint rules that
needed the member list (`bot-owner-unknown`, `people-unknown`; `lint.rs:756-771,919-933`)
become tool server checks shown on the admin overview ("bots whose owner isn't an active
user"), because lint runs without the database.

## 2. How every service reads users and roles

**The tool server reads its own tables directly** for every decision. Other services call it.

### Internal routes

| Route | Callers (token) | Returns |
| --- | --- | --- |
| `GET /internal/directory` | Orchestrator (`PMOS_SERVICE_TOKEN`, used by the runtime), Slack bot (`PMOS_BOT_TOKEN`) | Every user: `handle`, `display_name`, `status`, `roles`, and for the Slack bot only `slack_id`. `ETag: "<directory_version>"`; `304` on `If-None-Match`. Never email, `oid`, Atlassian or GitHub keys. |
| `GET /internal/users/resolve?by=entra&tid=&oid=` / `?by=email&email=` / `?by=handle&handle=` | Web (`PMOS_WEB_TOKEN`), console (`PMOS_CONSOLE_TS_TOKEN`) | One user: `handle`, `display_name`, `status`, `roles`, `entra_linked` (bool). `404` if none. `by=email` is how the console matches `X-Forwarded-User` and how web does an Entra first link. |
| `POST /internal/users/{handle}/identities/entra` | Web | First Entra link (§7). |
| `GET /internal/admin/users`, writes in §6 | Web, with an assertion | Admin screens. |

Per-client projections are fixed in code and tested: a token that may not see a field never
gets it. `resolve` can't carry an assertion (there is no signed-in subject yet at sign-in), so a
stolen web or console token can look people up one at a time; it returns no email, and the
route is rate-limited (60/min per client) and logged by kind.

### Caching and invalidation

| Service | Reads | Cache | Allow-decision bound |
| --- | --- | --- | --- |
| Tool server | Its tables | None for decisions. MCP sessions re-check `status` on each call through a 5-second in-process cache keyed by `directory_version`. | Immediate (≤ 5 s for an open MCP session) |
| Web | `resolve` per handle | 10 s per handle | ≤ 10 s |
| Console | `resolve` | 10 s per handle | ≤ 10 s |
| Runtime | `directory` | Poll every 10 s with `If-None-Match`; a snapshot older than 30 s is not used to allow | ≤ 30 s |
| Slack | `directory` | Same as the runtime | ≤ 30 s |
| Workers, harness, gateway, docs, evals runner | Nothing | — | — |
| CLI (`pmos`) | Nothing; host user commands run inside the tool server container (§5) | — | — |

No push invalidation in the MVP: polling a version is cheap and has no new moving parts.

**Stated bound:** a deactivation or a removed role takes effect at the tool server at once
(the same transaction revokes its MCP sessions), in web and the console within 10 seconds, and
in the runtime and Slack within 30 seconds. Because every task runs through a tool server
session and every write through a tool server decision, a removed person can *act* for at most
30 seconds anywhere and *write* nowhere after the change commits.

### When the store is unavailable

The tool server's database being down already stops sessions, approvals and audit. The table
says what each service does when it can't get a fresh answer.

| Action | Service | Behaviour |
| --- | --- | --- |
| Sign-in (new session) | Web, console | **Fail closed:** `503 users_unavailable`. |
| A request on an existing web session | Web | Last-known-good for that handle for at most **5 minutes**, for reads only. Any write goes to the tool server, which decides from the database. |
| Decide an approval, recommend | Tool server | **Fail closed** (`503 unavailable`). |
| Admin and auditor reads, any user or role write | Tool server | **Fail closed.** |
| Ask a bot | Runtime | May queue on a snapshot up to 5 minutes old; the task then needs a tool server session, which fails closed. Net: nothing runs on stale data. |
| A routine falls due | Runtime | Snapshot older than 5 minutes: skip this run (don't pause the routine), log it. |
| Slack id → handle | Slack | Last-known-good up to 5 minutes; after that, answer "PM OS can't check who you are right now". Onboarding decisions fail closed. |
| Restricted-draft audience check | Tool server | Fail closed (already: `audience_unavailable`). |

A service never falls back to `team.yaml`.

## 3. Migrating existing members

**Recommendation: a one-time, idempotent import run on the host, then the file is deleted from
the content repo.**

- **Command:** `pmos-admin users import --from <path> [--commit <sha>] [--allow-admins] [--apply]`,
  which runs `pmos-toolserver users import` in the tool server container with its database
  access. Without `--apply` it prints what it would do (create, unchanged, conflict) and changes
  nothing.
- **Idempotent:** keyed by handle. A user that exists with the same fields is unchanged. A field
  that differs is a **conflict**, reported and not overwritten: after cut-over the database wins.
  Re-running never removes anyone and never revokes a role.
- **Mapping:** `slack-id` → `slack` identity; `email` → `users.email` and the `atlassian`
  identity; `github` → `github` identity; `pm`, `reviewer`, `auditor`, `admin` as they are;
  `skill-author` → dropped (the person keeps `pm`; reported) — see open question 4. An unknown
  role, a duplicate key or a `bots.*` handle stops the import before any write.
- **Admins** are imported only with `--allow-admins`, which lists them and asks for
  confirmation on the terminal. The file's admins were granted by reviewed pull requests, so
  this is the one time a file grants admin.
- **Tamper evidence:** the import records one `role_changes` row with `change = 'import'` and
  `detail = "team.yaml sha256 <hex> commit <sha>"`, plus a `users.import` audit event. It reads
  the file from a path the operator names (normally the checkout at a reviewed commit), never
  from a URL.
- **Then:** a content pull request deletes `team.yaml` and its `CODEOWNERS` line; the same
  release ships the `team-file-retired` lint error.

**Rollback.** The previous release's binaries still read `team.yaml`. Before rolling back,
`pmos-admin users export --format team-yaml > team.yaml` writes the current database as a
`team.yaml` (active users, their roles, Slack id, email, GitHub; reviewer roles kept), so changes
made after cut-over aren't lost. Commit it, roll back. The tables stay; rolling forward again
re-runs the import, which reports no conflicts.

**Alternatives.** *Keep `team.yaml` read-only as a seed* — rejected: two places that look like
the truth, and a PR to the seed would look like a role grant. *Keep it only for lint* — rejected
for the same reason. *Dual-read during a transition* — rejected: the two disagree the moment
anyone changes a role.

**Test stacks.** The in-memory user store can be seeded from `PMOS_USERS_SEED=<file>` (the
same YAML shape as `team.yaml`, parsed by the import code), **honoured only with `PMOS_DEV=1`
and in-memory stores**; with `PMOS_DATABASE_URL` or live connectors the tool server refuses to
start if it is set. `scripts/claude-test-stack.sh` writes `$STATE/users.seed.yaml` with the
tester as `pm, admin` instead of calling `pmos member add`; `eval_stack_up`
(`deploy/pmos-admin:133-145`) writes `evals`, `ben` and `cy` the same way, which keeps the fake
channel audiences meaning what the eval tasks say. Postgres tests seed through the store API.

The "open" team for fake connectors (`team.rs:21-28`, anyone with a valid handle) goes away in
release binaries: dev and eval stacks use a seed. An open directory stays available to unit
tests only, behind a `test-support` feature.

## 4. Slack onboarding

**Today:** a request goes to `member_requests` (`migrations/0006_onboarding.sql:18-35`); an
admin's approval commits `team.yaml`, the bot and its policies.

**After:**

1. Someone not in the directory taps **Request access**. Unchanged: a `member_requests` row,
   status `pending`. The request **is** the pending record; no user row exists yet, and the
   person has no role and no access. A Slack id that belongs to a *deactivated* user gets
   `409 deactivated` ("your access was turned off; ask a platform owner"), not a new request.
2. An admin approves (Slack card, or the web admin screen). In one transaction the tool server
   creates the user (`active`, `display_name` from the request, `email`, `slack` and `atlassian`
   identities) with roles **`pm` only from Slack**; from web, `pm` plus optionally `reviewer`.
   The request becomes `approved`. The person can use PM OS at once.
3. Then the tool server commits the personal bot files (`BOT.md` and the two policy files) as
   today, minus `team.yaml`, with the approving admin's GitHub token and the `Approved-by`
   trailer. If that commit fails, the user stays active, the setup checklist shows the bot as
   missing, and an admin can retry with `POST /internal/admin/users/{handle}/personal-bot`.
   (Trade-off: access no longer waits for GitHub. The bot is what needs review; the person's
   access is the admin's decision, which is already recorded.)

**Role limits by channel.** The Slack bot's token names the deciding admin without a signature
(`docs/security-fixes-spec.md` S2), so from the bot client any role but `pm` is refused:
`admin` keeps `403 admin_by_pull_request` renamed to `403 admin_not_from_slack`, and `reviewer`
or `auditor` get `403 role_not_from_slack`. Onboarding never grants admin, from any channel.

`remove <handle>` in Slack becomes **deactivate** (§6) and stays admin-only; it no longer
commits anything.

## 5. The first admin

**Recommendation: a host command, refused once any active admin exists.**

```text
pmos-admin users bootstrap-admin --handle raj --email raj@example.com --slack-id U0RAJ [--name "Raj"]
```

It runs `pmos-toolserver users bootstrap-admin` inside the tool server container with direct
database access. It creates the user with `pm, admin`, writes `role_changes` (`client = 'host'`)
and an audit event, and refuses (`exit 3`) if an active admin already exists. Host access to
the Spark is already the highest trust in PM OS (it holds every secret), so this adds no new
trust.

**Break glass.** `pmos-admin users grant-admin <handle>` on the host works at any time and is
audited the same way. It is how a second admin is made in the MVP (§6, open question 1), and
how access is restored if the only admin leaves the company.

**Losing the last admin.** Refused in three places: the API (`409 last_admin` on demoting or
deactivating the last active admin), the self-action rule (an admin can't change their own roles
or deactivate themselves), and the database trigger (§1), which also covers concurrent changes.

**Alternative rejected:** an env-named bootstrap admin (`PMOS_BOOTSTRAP_ADMIN=raj`) honoured
while no admin exists. It is a standing latent grant in `deploy/.env`: if the admins are ever
all gone (or the trigger has a bug), the variable silently makes someone admin again, and the
variable outlives the person who set it.

## 6. Role administration

**Who may change what.**

| Change | Who | Channel |
| --- | --- | --- |
| Add a user (invite), approve a join request | Admin | Web (assertion), or Slack for `pm`-only approvals |
| Grant or revoke `pm`, `reviewer`, `auditor` | Admin | Web |
| **Grant `admin`** | Admin with host access | **Host CLI only** in the MVP (open question 1) |
| Revoke `admin` | Admin, not on themselves, not the last | Web |
| Deactivate, reactivate | Admin, not themselves | Web; deactivate also from Slack (`remove`) |
| Assign reviewers to PMs | Admin | Web |
| Unlink an Entra identity | Admin | Web |

No one changes their own roles or status, in any direction. The tool server checks the actor
in its own tables on every write; the assertion's `roles` are never used for this.

**Admin API** (tool server; `pmos-web` exposes the same under `/api/admin/users…`):

| Route | Body | Notes |
| --- | --- | --- |
| `GET /internal/admin/users?user=<viewer>` | — | `ManageMembers` or `ViewAuditLog`. Status, roles, display name, and booleans: `has_email`, `slack_linked`, `entra_linked`, accounts linked. Never an email, `oid` or Slack id to an auditor. |
| `POST /internal/admin/users` | `{handle, display_name, email, slack_id?, roles}` | Creates `active`. `roles` must not contain `admin`. |
| `PUT /internal/admin/users/{handle}/roles` | `{roles, expected_version}` | Whole set; `409 stale` if the directory moved. |
| `POST /internal/admin/users/{handle}/deactivate` / `reactivate` | `{}` | §6 semantics below. |
| `DELETE /internal/admin/users/{handle}/identities/entra` | — | For a reassigned mailbox (§7). |
| `PUT /internal/admin/reviewers/{reviewer}` | `{pms: [handle]}` | §8. |

**Write routes need an assertion whatever `PMOS_ASSERTION_MODE` says.** If the tool server is
not in `require` they answer `503 assertion_required`; the subject must equal `user=` (the
actor). Only the web token reaches them; the orchestrator, Slack bot and console tokens get
`403`. Every write, allowed or refused, writes `role_changes` (when allowed) and an audit event
(always), with who, what and when, and no secrets.

**Separation of duties.**

- **Admin and Auditor: refused** for one person (database trigger; today only a lint warning,
  `lint.rs:906-916`). An auditor reviews what admins do. See open question 3.
- **Reviewer and owner of the same draft:** a reviewer never recommends on their own draft
  (`403 own_draft`); they decide it as its owner.
- Reviewer and Admin together: allowed. Admins already see more.

**Deactivation semantics** (one transaction, then best-effort clean-up, each step logged):

| What | Happens |
| --- | --- |
| Status | `deactivated`; role rows kept for reactivation but grant nothing |
| Tool server MCP sessions | Revoked in the same transaction |
| Web and console sessions | Ended on the next request (≤ 10 s), as removal does today (`app.rs:554-569`) |
| Pending approvals | Closed by the system as `rejected` with ledger reason `owner_deactivated`; no write runs. Slack draft text is removed as at expiry. |
| Linked accounts | Unlinked and tokens deleted, as removal does today (`onboarding.rs:816-826`) |
| Routines | The runtime pauses them when its snapshot shows the change (≤ 30 s), with the existing "paused" note |
| Personal bot | Refused by `may_use` (owner not active). The sandbox keeps idling until `pmos-admin pause pm-<handle>`; the API reply says so. Bot files stay in content. |
| Restricted-draft audience | A deactivated person's Slack id counts as an **outsider** (§2 of `docs/slack-reply-spec.md`) |
| Team bots they own | Refused `409 owns_bots` until reassigned, as today (`onboarding.rs:746-766`) |
| Reviewer assignments, memory-repo record | Kept, inert |

## 7. Entra

**Identity mapping.** The stable key is `(tid, oid)`, stored as an `entra` identity. Add `oid`
to the ID token claims (`entra.rs:368-380`); a token without `oid` is refused.

1. Sign-in validates the token as today, then calls `resolve?by=entra&tid=&oid=`. Found and
   active: signed in. Email in the token is ignored from then on, so a renamed or reassigned
   mailbox can't move the sign-in to another person.
2. Not found: **first link.** `resolve?by=email` with `preferred_username` (or `email`). It
   links only if exactly one user has that email, the user is `active`, and the user has **no**
   Entra identity yet. Then `POST …/identities/entra` stores `(tid, oid)` in one transaction that
   re-checks all three conditions. Audit `users.identity_link`; the user is told on their first
   page ("You're signed in with Microsoft for the first time") and admins see it on the overview.
3. Anything else: `403 not_a_member` with the existing wording.

**Why auto-link on email rather than admin-confirmed link:** the email was put there by an admin
(invite, import) or came from Slack and was seen by the approving admin. An extra confirmation
for every PM's first sign-in is friction for a small team. The residual risk is a mailbox
reassigned *before* the first link; the fix is the admin's unlink action and the notice above.

**Entra app roles and groups: ignored for the MVP.** The database is the only source of roles.
A one-way sync would make tenant admins (outside PM OS's audit log) able to grant PM OS admin,
and would need a precedence rule against admin edits. Revisit with SCIM if the team grows.

**Just-in-time user creation: no.** A tenant member who isn't in the directory is refused.
Joining stays an admin decision (Slack request or web invite).

The dev provider (`PMOS_WEB_DEV_LOGIN`) keeps resolving by handle, dev setups only.

## 8. The Reviewer rule, enforced

**Who reviews whom: assigned per PM** (`reviewer_assignments`, set by an admin). A Reviewer sees
and recommends on the pending drafts of the PMs assigned to them, and no one else's. Rejected
alternatives: *any Reviewer sees every draft* (a much wider read of draft text and sources,
which can come from restricted Jira or Slack content); *assigned per draft* (the owner picks a
reviewer each time; flexible, but more UI and an extra step on every draft — a later addition).

**What a recommendation is.** A row in `approval_recommendations`: `verdict` `approve`,
`reject` or `changes`, an optional note (≤ 1000 characters, plain text), one live
recommendation per reviewer per draft (a new one withdraws the old). Only on `pending` drafts.

**Route:** `POST /internal/approvals/{id}/recommendation` with
`{recommended_by, verdict, note?}`. Web token only, assertion required (subject =
`recommended_by`). The check lives in a new `ToolService::recommend`, beside `decide_verdict`
(`service.rs:1723-1770`):

1. `recommended_by` is active and has `RecommendOnApproval` **in the database**;
2. the item is pending and `item.user` is assigned to them;
3. `recommended_by != item.user` (`403 own_draft`);
4. insert, then audit `approvals.recommend` (metadata: actor, item id, verdict; never the note).

**It can never publish.** The route has no path to `decide_verdict` or a connector. `decide`
ignores recommendations entirely and still requires `decided_by == item.user`. A test asserts
that after any sequence of recommendations the item is still `pending` and no connector call
was made, and that a reviewer's `decision` call on someone else's item is `403`.

**What the owner sees.** Web: the draft page shows "Ana (reviewer) recommends approving" with
the note, above the owner's own buttons. Slack: the approval card gets one context line with the
reviewer's name and verdict (the note is in the web view, to keep content out of more Slack
messages). The owner's decision is recorded with a fixed detail `"after a recommendation to
<verdict>"` so the audit shows whether the advice was followed.

**What a Reviewer may read** — wider than today, stated plainly:

- `GET /internal/approvals?reviewer=<handle>`: assigned PMs' pending drafts (tool, target,
  draft text, created time), and `GET /internal/approvals/{id}/sources` for those items (today
  owner-only, `app.rs:775-800`).
- **Restricted drafts** (`sources_sensitivity` `restricted` or unrecorded): metadata only, "this
  draft uses restricted sources; its owner can share it with you". See open question 6.
- **Slack drafts** keep their text only until `PMOS_SLACK_DRAFT_TTL_HOURS` (default 24). A
  reviewer can read and recommend only while the text exists; notes on a Slack draft are removed
  when its text is. Slack message text never reaches the audit log or a report.
- No traces, no tasks, no routines of others (`ViewOthersTasks` stays admin-only).

**Assertion changes:** none to the wire format. The new routes are added to the assertion
binding rules (subject must equal `recommended_by`, and `reviewer=` on the list).

**Matrix change:** none to the Reviewer row (`permissions.rs`: it already has
`ViewOthersApprovals` and `RecommendOnApproval`). The *scope* of "others" (assigned PMs only)
is the caller's check, documented in the module docs like the `*Own*` rules.

## 9. The signed user assertion

- **Where roles come from:** `pmos-web` and the console put the roles they read from `resolve`
  (cached ≤ 10 s) into each assertion they sign. The assertion lives 60 s (`docs/user-assertion.md`,
  "Clocks"). So a role shown in an assertion can be up to **10 s + 60 s + 5 s skew = 75 s**
  stale.
- **Sessions hold no roles.** Web sessions store only the handle (as today); roles are looked up
  per request. There is nothing to refresh.
- **Receivers re-check what matters.** The tool server checks roles in its own tables for every
  admin read, user write, approval decision and recommendation. The runtime checks `AskBot` and
  membership from its directory snapshot (≤ 30 s), not from the assertion. The assertion's
  `roles` field stays informational. `docs/user-assertion.md` "Not addressed" changes from "check
  against `team.yaml`" to "check against the directory".
- **`skill-author` in the wire format:** `Role::KNOWN` keeps it parseable for one release so
  mixed-version services don't refuse each other's assertions; nothing signs it after cut-over.

## 10. Auditor

**Unchanged promise:** metadata only. The auditor sees the user list (handles, display names,
roles, status, booleans), never an email, Slack id, `oid`, a recommendation note, draft text or
a token.

**New audit events** (actor, client, target handle in `output_ref`, outcome):

| `action` | When | `detail` (fixed formats, added to `safe_detail`, `app/admin.rs:207-246`) |
| --- | --- | --- |
| `users.create` | Invite or join approval (replaces `members.add`) | `roles <list>` |
| `users.roles` | Roles changed | `+reviewer -pm` style, known role names only |
| `users.deactivate`, `users.reactivate` | Status | — |
| `users.identity_link`, `users.identity_unlink` | Entra first link, admin unlink | `entra` |
| `users.import`, `users.bootstrap_admin`, `users.grant_admin` | Host commands | `sha256 <hex>` for import |
| `reviewers.assign` | Assignments changed | `<n> pms` |
| `approvals.recommend` | A recommendation | `verdict <v>` |
| `users.*` with `denied` | Any refused write | the refusal code (`last_admin`, `self_change`, `role_not_from_slack`, …) |

The audit `client` check constraint gains `host` (`migrations/0011_audit_admin_reads.sql`
pattern). Audit events purge at 90 days (`CLAUDE.md`, f); `role_changes` is kept as the record
of who holds which grant (open question 5).

## 11. Threats

| Threat | How the design answers it |
| --- | --- |
| Privilege escalation through onboarding | A request grants nothing. The Slack bot client may create `pm` only; admin is host-only; tested per role and per client. |
| Escalation through a pull request | No file grants anything after cut-over; `team.yaml` is a lint error; the import is a one-time host action. |
| Compromised web backend | It can sign as any user to the tool server (a known limit of assertions). It **can't grant admin** (host-only), can't remove the last admin (trigger), and every change is in `role_changes` and the audit log with `client = web`. It can change other roles and deactivate people; both reversible. |
| Compromised console or Slack bot | No write route accepts their tokens, except Slack's `pm`-only join approval. |
| Stale cache after demotion | Bounded: tool server immediate, web/console ≤ 10 s, runtime/Slack ≤ 30 s; writes are decided only at the tool server from the database. |
| Database unavailable | Fail closed for sign-in, decisions, admin; last-known-good ≤ 5 min only for reads and queueing (§2). No fallback to a file. |
| Import tampering | Dry run first, conflicts never overwrite, admins only with `--allow-admins` and a terminal confirmation, file hash and commit recorded. |
| Last-admin lockout | API check, self-change ban, deferred trigger under an advisory lock, and the host break-glass command. |
| Email reassignment | Sign-in keys on `(tid, oid)` after the first link; email is used once; unique across all users; admin unlink. |
| Confused deputy between services | Per-client route allow-lists and field projections; user writes need an assertion whose subject is the actor; the orchestrator token can read the directory but write nothing. |
| Two sources of truth | One owner; no service reads `team.yaml`; the proof is a hostile-file test (§12). |

## 12. Build plan

Each slice is one agent, one worktree, one branch. All need the workspace checks in `CLAUDE.md`.

| # | Slice | Changes | Depends on | Key tests |
| --- | --- | --- | --- | --- |
| U1 | **Store** | Migration 0012; `UserStore` (Postgres + memory); triggers; `role_changes`; seed loader (`PMOS_USERS_SEED`, dev only); `pmos-toolserver users import/export/bootstrap-admin/grant-admin/list`; `pmos-admin users …` wrapper | — | Postgres (`PMOS_TEST_DATABASE_URL`): last admin incl. two concurrent transactions; admin+auditor refused; unique keys; append-only `role_changes`; import idempotent, conflicts reported, unknown role aborts with no writes; seed refused with a database URL or live connectors |
| U2 | **Read API** | `/internal/directory` (ETag), `/internal/users/resolve`, `/identities/entra`; per-client projections; rate limit | U1 | Each token sees exactly its fields; web/console can't list; `304`; first-link conditions incl. a race |
| U3 | **Tool server cut-over** | `Team` → `Directory` in service, OAuth, onboarding (DB write then bot commit), admin reads, audience check (deactivated = outsider), MCP session re-check | U1 | Existing toolserver suites green; `admin_not_from_slack`, `role_not_from_slack`; an open MCP session dies on deactivation |
| U4a | **Runtime** | Catalog membership and `AskBot` from the directory snapshot; routine pause on deactivation | U2 | Auditor-only member can't ask; stale-snapshot rules |
| U4b | **Slack** | Directory snapshot replaces `load_team`; admins from roles; `remove` → deactivate | U2 | Socket tests with a seeded directory |
| U4c | **Web sign-in** | `oid` claim; resolve; per-handle cache; `/api/me` from the directory with `display_name` | U2 | Entra first link, reassigned email, deactivated user, `503 users_unavailable` |
| U4d | **Console** | Resolve; role-aware viewer (auditor isn't a PM) | U2 | Proxy identity by email and handle |
| U5 | **Admin writes and Team screen** | Write routes; `pmos-web` `/api/admin/users…`; Team page edits (invite, roles, deactivate, reviewers, unlink); copy | U3, U4c | Every self/last-admin/role refusal; assertion required; web unit, e2e and a11y tests |
| U6 | **Reviewer** | Assignments, recommendation route and store, reviewer inbox, owner view in web, Slack card line | U3, U5 | Recommendation never changes status or calls a connector; unassigned `404`; restricted metadata-only; note removed with Slack text |
| U7 | **Retire the file** | `team-file-retired` lint; remove `TeamFile` use from every service (parser stays only in the import/seed module); remove `pmos member add`; test-stack and eval-stack seeds; docs (`entra-sign-in.md`, `web-api.md`, `onboarding-spec.md`, `user-assertion.md`, `content/team.yaml` deleted) | U3–U6 | **The proof** (below) |

U4a–U4d run in parallel. U5 and U6 can overlap once the routes in U5 are merged.

**Proving no service decides from the file.** (1) An integration test per service starts it
with a content root holding a **hostile** `team.yaml` that lists `mallory` as `admin` with the
tester's Slack id and email, and a seeded directory without `mallory`: sign-in, Slack, a bot
ask, an admin read and an approval decision as `mallory` are all refused, and the real user
is unaffected. (2) The same services start with no `team.yaml` at all. (3) A workspace test
fails if any crate other than the import/seed module names `team.yaml` or `TeamFile`.

**Out of scope for the MVP:** Entra group or app-role sync; SCIM; JIT creation; self-service
join from the web; hard deletion and erasure; per-draft reviewer choice; recommendations from
Slack; a Slack DM to admins on every role change; step-up re-authentication for admin writes
(the assertion has no `auth_time`); more than one tool server replica; multiple Slack
workspaces or Entra tenants.

## 13. Open questions for the owner

1. **Granting admin only from the host CLI?** Recommended default: **yes** for the MVP. It
   keeps a compromised web backend from creating admins. Cost: a new admin needs someone with
   Spark access. Alternative: web, with a second admin's confirmation (weaker, because a
   compromised web can sign as both admins).
2. **Reviewers assigned per PM, or any Reviewer sees every PM's drafts?** Default: **per PM,
   assigned by an admin.**
3. **May one person be both Admin and Auditor?** Default: **no**, refused by the database.
   With a very small pilot team this may need a temporary exception; say so if it does.
4. **`skill-author`:** it grants nothing that is checked today. Default: **drop it**; people
   keep `pm`, and bot and skill changes stay gated by pull request review.
5. **Retention of `role_changes`:** default: **keep while the user exists, and 90 days after
   deactivation**, longer than the 90-day audit purge, because it is the record of who was
   granted what. Alternative: purge with the audit log at 90 days.
6. **Restricted drafts and Reviewers:** default: **metadata only** to the reviewer; the owner
   can share. Alternative: assigned reviewers read restricted drafts too, which widens who sees
   restricted Jira, Confluence and Slack content.
