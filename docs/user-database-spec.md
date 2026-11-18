# User database: the source of users and roles

Status: **design spec, revised after the first security review; not built.** The reviewer
checks this revision before any slice starts. Owner's decision (scope item b in `CLAUDE.md`):
the PM OS user database is the source of users and roles, not `content/team.yaml`. Roles:
Admin, Product Manager, Reviewer, Auditor. A Reviewer recommends and the draft's owner
confirms. An Auditor sees metadata only.

Each section gives a recommendation, then the alternatives considered. File references are to
`claude/web-app` at `52c45e3` and were re-read for this revision.

## Changes since the first review

The review of `476ef25` asked for nine changes and six smaller ones; the owner answered the
open questions. Where each is answered now:

| Review item | What changed | Where |
| --- | --- | --- |
| 1. Reactivation restores Admin from the web | Deactivation revokes `admin` (recorded); reactivation never restores it; only `grant-admin` on the host does. | §6 Deactivation, §5, U1/U3 tests |
| 2. Deactivation isn't reversible; Slack can do it | Credentials suspended at once, deleted after a 24-hour grace unless reactivated; pending drafts held, not rejected, for the grace. The bot client can't deactivate an Admin or Auditor. At most 3 deactivations an hour from web and Slack together; past that, host only. Threat table corrected. | §6 Deactivation, §4, §11 |
| 3. Entra first link by `preferred_username` or `email` | Claims checked against the code. First link by `preferred_username` only, never `email`. Guests refused (`acct` claim). Admin and Auditor never auto-link: the host sets `(tid, oid)`. A sign-in whose name is already linked to another identity is refused, audited and flagged. Microsoft's guidance stated, marked to confirm. | §7 |
| 4. Email on a Slack join comes from the unsigned bot client | Chosen: a user created through the bot client can't first-link Entra until an admin confirms their sign-in name in the web app. | §4, §7, §1 schema |
| 5. One value for the Entra sign-in name and the Atlassian email | Separate columns, `sign_in_name` and `atlassian_email`; who may change each, and when. | §1 schema, §6 "Who may change what" |
| 6. `resolve` rate limit | Misses (`404`) are limited, hits are not. | §2 Internal routes |
| 7. Stated bound vs last-known-good | Two bounds, reachable and unreachable. Slack-derived data is never served on the stale path. | §2 Stated bounds |
| 8. A recommendation isn't bound to the text read | Each recommendation stores the draft's revision and hash; the owner sees it marked stale after an edit; the audit says which version. Builds on the B1 fix; W6 is fixed. U6 waits for B1. | §8, §12 order |
| 9. Seed and import order | `import --bootstrap` for a fresh database (CI smoke); bootstrap and import order written down; the advisory lock is taken inside the trigger. Correction: the eval stack runs in memory, so the seed works there. | §3, §1 invariants, §12 |
| Smaller: Auditor-only person in a channel | Counts as an outsider for the restricted-draft audience check. | §6 Deactivation table, §8, U3 test |
| Smaller: the `evals` account | `pm` only; never an identity in the database. | §1 invariants |
| Smaller: display names and notes | Untrusted text: stored cleaned, rendered plain in web and Slack. | §1 schema, §8, §10 |
| Smaller: audit purge vs `role_changes` | `role_changes` has its own append-only trigger that the purge's exception can't reach; its own narrow retention rule. | §1 invariants, §10, §12 order |
| Smaller: host commands record the OS user | `host_user` on `role_changes` and in the audit event. | §1 schema, §5 |
| Smaller: Atlassian keyed on account id | After the first link, the Atlassian account id decides; GitHub likewise on its numeric id. | §1 schema, §6 |
| Owner Q1, Q2, Q4, Q5, Q6 | Defaults adopted; questions removed. | §5, §8, §3, §10, §8 |
| Owner Q3: Admin and Auditor together | Allowed for the pilot through a host-only, time-limited exception; flagged and marked. | §6 "Pilot exception: Admin and Auditor" |
| Code since `476ef25` | W6 fixed; assertions required whenever web or console tokens are set; console single-item reads need the item's PM or an admin. | §0, §2, §9 |

## 0. What reads `team.yaml` today

Every service reads the file from the shared content checkout (`deploy/compose.yaml:111,118`:
the tool server mounts the checkout read-write and pulls it; the runtime, Slack, console, web
and workers mount it read-only, e.g. `:173`, `:211`). The model is `TeamFile`/`TeamMember`
(`crates/contract/src/model.rs:376-436`).

| Where | What it decides from the file | Reference |
| --- | --- | --- |
| Tool server `Team` | Who may have an MCP session, decide an approval, link an account. **Enforced only with live connectors**; with fake connectors anyone with a valid handle is in. | `crates/toolserver/src/team.rs:21-50`, `main.rs:73-75`, `service.rs:584,631,1810` |
| Tool server onboarding | Admin check for member decisions and removal; writes the new `team.yaml` | `onboarding.rs:198-200,453,711` |
| Tool server admin reads | Viewer's roles against the permission matrix; overview, accounts and proposal screens list members from the file | `app/admin.rs:116-140,331-345,423-440,571` |
| Tool server single-item approval read | The console's admin view reads any item when the signed subject is an admin **in the tool server's `team.yaml`** | `app.rs:763-805` |
| Tool server account linking | Slack id and Atlassian email must match the member; GitHub login or verified email | `oauth.rs:392-440,464-525` |
| Tool server audience check | A restricted draft is blocked if anyone in the channel isn't in `team.yaml` | `service.rs:1937-2000` |
| Runtime catalog | Team bots serve members only; set level; routines pause when the owner may not use the bot. **No role check: an auditor-only member can ask team bots here.** | `crates/bots/src/catalog.rs:79,118-126,170-185`, `runtime.rs:303,917,937,1096` |
| Runtime console reads | One task's detail, events, trace and drafts under `require`: the task's PM or an admin **in the runtime's `team.yaml`** | `crates/bots/src/app.rs:556-565` |
| Slack | Slack id → handle on every event (re-read per event; a parse error means an empty team); which admins get join cards | `crates/slack/src/commands.rs:55-90`, `members.rs:310`, about 25 call sites |
| Web | Sign-in by handle (dev) or email (Entra); every request re-reads the file; `/api/me` roles and permissions; admin team page | `crates/web/src/auth.rs:101-125`, `app.rs:546-580,624,789,823-835`, `app/admin.rs:415-470` |
| Console | Viewer by handle or email; only `admin` vs not. **An auditor is treated as a PM there.** | `crates/console/src/auth.rs:76-110` |
| CLI | `pmos member add` writes `team.yaml` and the personal bot; `pmos lint` checks the `team-*` rules and bot owners | `crates/cli/src/main.rs:42-70,142`, `crates/contract/src/lint.rs:756-771,808-933` |
| Test stacks | `pmos member add` into a copy of the content | `scripts/claude-test-stack.sh:160`, `deploy/pmos-admin:183-205` (eval stack: `evals`, `ben`, `cy`), `.github/workflows/ci.yml:129` (CI smoke: `ci`) |
| Evals | `requested_by: evals` must be a member | `evals/config.yaml:6-8` |
| Harness | Nothing: it gets `requested_by` from the runtime | `harness/src/worker.ts:102` |

**Since the first review** (`52c45e3`): with a web or console token configured, the tool server
and the runtime run in assertion `require` and refuse to start in `off` or `verify` unless
`PMOS_ASSERTION_INSECURE_OFF=1` (development only; `crates/assertion/src/receiver.rs:18-100`,
`docs/user-assertion.md` "Modes"). The two "admin in its own `team.yaml`" rows above are the
console rule that came with it. W6 is fixed: the runtime records each task's highest data class
(`crates/bots/migrations/0006_task_sensitivity.sql`) and opens a follow-up's tool server session
at that level (`service.rs:571-597,615-660`, `docs/bot-runtime-spec.md` §3 Conversations), so a
draft's `sources_sensitivity` now includes what its conversation history read.

**How Slack onboarding writes today.** Not a pull request: on approval the tool server makes one
commit straight to the content branch with the approving admin's GitHub token (`commit_team`,
`onboarding.rs:859-946`). The commit has the new `team.yaml`, `bots/pm-<handle>/BOT.md` and the
two generated sandbox policy files, with an `Approved-by` trailer. The in-memory team is swapped
at once (`onboarding.rs:621-624`). From the Slack bot client an `admin` grant is refused
`403 admin_by_pull_request` (`onboarding.rs:544-562`; `docs/security-fixes-spec.md` S2).
Removal commits `team.yaml` without the member, deletes the bot files, unlinks every account
and drops the memory-repo record (`onboarding.rs:703-842`).

**Member fields today.**

| Field | Stored in | Written by |
| --- | --- | --- |
| `handle`, `slack-id`, `roles` | `team.yaml` | Onboarding approval, `pmos member add`, hand edits by PR |
| `email` (Atlassian email *and* Entra sign-in name) | `team.yaml` | Onboarding (from Slack `users.info`, read by the Slack service, `crates/slack/src/members.rs:217-252`), CLI, PR |
| `github` | `team.yaml` | CLI or PR only; onboarding writes none |
| Display name | `member_requests.name` only | Slack `users.info` at request time; never reaches `team.yaml` (`crates/web/src/app.rs:827-829`) |
| Level | `pmos_bots.member_settings` | The PM, through the runtime |
| Personal bot | `content/bots/pm-<handle>/` and its policies | Onboarding commit |
| Memory repo, linked accounts | Tool server `memory_repos`, `credentials` (which already keeps the provider's `external_id`, `migrations/0002_credentials.sql`) | The PM |
| Timezone | Not per member: `PMOS_DEFAULT_TZ` | — |
| Entra `oid`/`tid` | Not stored; the ID token claims don't parse `oid` (`crates/web/src/entra.rs:369-380`) | — |

**Findings that this spec fixes on the way.** (1) The Reviewer rule is not enforced anywhere:
`RecommendOnApproval` and `ViewOthersApprovals` exist only in the matrix
(`permissions.rs:44,46,157-160`); no route uses them. (2) The runtime and Slack don't check
`AskBot`, and the console knows only admin vs not, so an auditor can ask bots there. (3) An MCP
session outlives its member's removal by up to 60 minutes: `ToolService::session` re-checks the
bot, not the member (`service.rs:663-674`). (4) `skill-author` is a fifth role whose only
permission, `EditBotsAndSkills`, is checked nowhere.

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

### Schema (migration `crates/toolserver/migrations/NNNN_users.sql`)

`NNNN` is the next free number when U1 merges: `0012` on this branch, but the audit purge and
the B1 fix may each take one first (§12 order).

```sql
CREATE TABLE users (
    handle        text PRIMARY KEY
                  CHECK (handle ~ '^[a-z0-9][a-z0-9._-]{0,63}$' AND handle NOT LIKE 'bots.%'),
    -- Untrusted (often from Slack): cleaned on write, rendered as plain text (§10).
    display_name  text NOT NULL DEFAULT ''
                  CHECK (length(display_name) <= 100 AND display_name !~ '[[:cntrl:]<>]'),
    -- The Entra sign-in name (UPN) a first link must match (§7). Not the Atlassian email.
    sign_in_name  text CHECK (sign_in_name = lower(sign_in_name) AND length(sign_in_name) <= 254),
    -- Set when an admin (web) or the host confirmed sign_in_name; NULL for a value that came
    -- through the Slack bot client (§4). An unconfirmed name never links.
    sign_in_name_confirmed_at timestamptz,
    sign_in_name_confirmed_by text,
    -- What an Atlassian link must match until the first link; after it, the account id
    -- in user_identities decides (§6).
    atlassian_email text CHECK (atlassian_email = lower(atlassian_email)
                                AND length(atlassian_email) <= 254),
    -- What a GitHub link must match until the first link (a login); then the numeric id.
    github_login  text CHECK (github_login ~ '^[A-Za-z0-9-]{1,39}$'),
    status        text NOT NULL CHECK (status IN ('active', 'deactivated')),
    created_at    timestamptz NOT NULL,
    created_by    text NOT NULL,           -- a handle, or 'import' / 'host'
    created_client text NOT NULL CHECK (created_client IN ('web', 'bot', 'host', 'import')),
    deactivated_at timestamptz,
    deactivated_by text,
    CHECK ((sign_in_name_confirmed_at IS NULL) = (sign_in_name_confirmed_by IS NULL))
);
CREATE UNIQUE INDEX users_sign_in_name ON users (sign_in_name) WHERE sign_in_name IS NOT NULL;
CREATE UNIQUE INDEX users_atlassian_email ON users (atlassian_email)
    WHERE atlassian_email IS NOT NULL;
CREATE UNIQUE INDEX users_github_login ON users (lower(github_login))
    WHERE github_login IS NOT NULL;

CREATE TABLE user_roles (
    handle     text NOT NULL REFERENCES users ON DELETE RESTRICT,
    role       text NOT NULL CHECK (role IN ('admin', 'pm', 'reviewer', 'auditor')),
    granted_at timestamptz NOT NULL,
    granted_by text NOT NULL,
    PRIMARY KEY (handle, role)
);
CREATE INDEX user_roles_role ON user_roles (role);

-- Keys that point at a person and were proven by a sign-in or an OAuth link (or set on the host).
CREATE TABLE user_identities (
    handle    text NOT NULL REFERENCES users ON DELETE RESTRICT,
    kind      text NOT NULL CHECK (kind IN ('slack', 'entra', 'atlassian', 'github')),
    issuer    text NOT NULL DEFAULT '',   -- entra: tid; slack: workspace id; else ''
    subject   text NOT NULL,              -- slack member id; entra oid; Atlassian account id;
                                          -- GitHub numeric user id
    linked_at timestamptz NOT NULL,
    linked_by text NOT NULL,
    linked_client text NOT NULL CHECK (linked_client IN ('web', 'bot', 'host', 'import', 'link')),
    -- entra only: true when the host set or confirmed it (§7: required for Admin and Auditor).
    host_confirmed boolean NOT NULL DEFAULT false,
    UNIQUE (kind, issuer, subject),       -- one person per key
    UNIQUE (handle, kind)                 -- one key of each kind per person
);

-- Authoritative history of every change above, written in the same transaction. Append-only
-- through its OWN trigger function (not audit_events'), with one narrow retention rule (below).
CREATE TABLE role_changes (
    id        bigserial PRIMARY KEY,
    at        timestamptz NOT NULL,
    actor     text NOT NULL,           -- a handle, or 'host' / 'import'
    client    text NOT NULL CHECK (client IN ('web', 'bot', 'host', 'import', 'system')),
    host_user text CHECK (host_user ~ '^[a-z_][a-z0-9_.-]{0,31}$'),  -- the OS user (§5)
    target    text NOT NULL,           -- a handle, or '*' for import and pilot exceptions
    change    text NOT NULL CHECK (change IN ('create', 'grant', 'revoke', 'deactivate',
              'reactivate', 'link', 'unlink', 'import', 'assign', 'unassign', 'set_field',
              'confirm_sign_in_name', 'suspend_credentials', 'delete_credentials',
              'exception_on', 'exception_off')),
    role      text,
    detail    text,                    -- fixed formats only (§10), never free text
    CHECK (client <> 'host' OR host_user IS NOT NULL)
);
CREATE INDEX role_changes_target ON role_changes (target, at DESC);
CREATE INDEX role_changes_recent ON role_changes (change, at DESC);

-- Owner Q3: Admin and Auditor on one person, for the pilot only (§6).
CREATE TABLE pilot_exceptions (
    id          bigserial PRIMARY KEY,
    name        text NOT NULL CHECK (name = 'admin_and_auditor'),
    enabled_at  timestamptz NOT NULL,
    enabled_by  text NOT NULL,         -- host_user
    expires_at  timestamptz NOT NULL CHECK (expires_at <= enabled_at + interval '90 days'),
    ended_at    timestamptz,
    ended_by    text
);
CREATE UNIQUE INDEX one_open_exception ON pilot_exceptions (name) WHERE ended_at IS NULL;

CREATE TABLE reviewer_assignments (           -- §8
    reviewer   text NOT NULL REFERENCES users,
    pm         text NOT NULL REFERENCES users,
    assigned_by text NOT NULL,
    assigned_at timestamptz NOT NULL,
    PRIMARY KEY (reviewer, pm),
    CHECK (reviewer <> pm)
);

CREATE TABLE approval_recommendations (       -- §8
    id             text PRIMARY KEY,
    approval_id    text NOT NULL REFERENCES approvals ON DELETE CASCADE,
    reviewer       text NOT NULL REFERENCES users,
    verdict        text NOT NULL CHECK (verdict IN ('approve', 'reject', 'changes')),
    note           text CHECK (length(note) <= 1000 AND note !~ '[\x00-\x08\x0b-\x1f]'),
    draft_revision integer NOT NULL,   -- approvals.revision the reviewer read (B1 fix)
    draft_sha256   text NOT NULL CHECK (draft_sha256 ~ '^[0-9a-f]{64}$'),
    created_at     timestamptz NOT NULL,
    withdrawn_at   timestamptz
);
CREATE UNIQUE INDEX one_live_recommendation
    ON approval_recommendations (approval_id, reviewer) WHERE withdrawn_at IS NULL;

-- Bumped by trigger on any change to users, user_roles, user_identities, pilot_exceptions or
-- reviewer_assignments: the ETag of the directory (§2).
CREATE TABLE directory_version (one boolean PRIMARY KEY DEFAULT true CHECK (one), v bigint NOT NULL);
```

U3 adds one column to an existing table: `credentials.suspended_at timestamptz` (§6
Deactivation). Linked tokens with it set are never unsealed.

**Invariants in the database, not only in code.** One deferred constraint trigger,
`users_invariants()`, runs at commit for any change to `users`, `user_roles`,
`user_identities` or `pilot_exceptions`. Its first statement is
`PERFORM pg_advisory_xact_lock(hashtext('pmos.users'))`; it raises if
`current_setting('transaction_isolation') <> 'read committed'`, so every check after the lock
runs on a fresh snapshot that includes any transaction that committed while it waited. Two
concurrent demotions are therefore serialized **in the database**, whatever the code does.
(Application code takes the same lock first too, to fail fast; it is re-entrant.) Then:

- **The last admin** (§5): refuses a commit that leaves users in the table but no `active` user
  holding `admin`.
- **Admin and Auditor together** (§6): refuses a user holding both unless a
  `pilot_exceptions` row is open and `expires_at > now()`.
- **The `evals` account:** the handle `evals` may hold `pm` only, and never a row in
  `user_identities` (no Slack, Entra, Atlassian or GitHub key, so nobody can sign in or link
  as it). The eval stack's fake Slack id lives only in its in-memory seed (§3).
- **No deletes.** Users are deactivated, never deleted, in the MVP. `ON DELETE RESTRICT` keeps
  history intact. Hard deletion (erasure) is out of scope (§12).
- `sign_in_name` and `atlassian_email` are stored lower-case and unique across all users,
  active or not, so a deactivated person's address can't be silently re-used for a first link.

**`role_changes` is append-only by its own trigger,** `role_changes_append_only()`, separate
from `audit_events_append_only()` (`migrations/0001_init.sql:86-94`). It refuses `UPDATE`
always. It refuses `DELETE` unless the session setting `pmos.role_changes_retention` is `on`
**and** the row's target is a user that is `deactivated` with `deactivated_at` more than 90
days ago (owner Q5: kept while the user is active, and 90 days after deactivation); rows whose
target is `*` are never deleted. The 90-day audit purge (in flight) uses its own exception for
`audit_events`; this trigger doesn't read it, so the purge can't reach `role_changes` (test in
U1, order in §12).

**Migrations** run at tool server start, as today (`pg.rs:90`). The in-memory store
(`PMOS_DEV=1` without a database URL) gets the same `UserStore` trait and enforces the same
invariants in code; the Postgres tests prove the trigger versions.

### What stays in content, what moves

| Stays in content (reviewed by pull request) | Moves to the database |
| --- | --- |
| Bot definitions, including personal bots `bots/pm-<handle>/BOT.md`, their roles and generated sandbox policies | Who exists, their status, roles, display name |
| `master/people/<handle>/` context folders | Sign-in name, Atlassian email, GitHub login; Slack id, Entra `oid`+`tid`, Atlassian account id, GitHub id |
| A bot's `owner` field (a handle) | Reviewer assignments, recommendations, the pilot exception |

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
| `GET /internal/directory` | Orchestrator (`PMOS_SERVICE_TOKEN`, used by the runtime), Slack bot (`PMOS_BOT_TOKEN`) | Every user: `handle`, `display_name`, `status`, `roles`, and for the Slack bot only `slack_id`. `ETag: "<directory_version>"`; `304` on `If-None-Match`. Never an email or sign-in name, `oid`, Atlassian or GitHub keys. |
| `GET /internal/users/resolve?by=entra&tid=&oid=` / `?by=sign_in_name&name=` / `?by=handle&handle=` | Web (`PMOS_WEB_TOKEN`), console (`PMOS_CONSOLE_TS_TOKEN`) | One user: `handle`, `display_name`, `status`, `roles`, `entra` (`none`, `linked`, `needs_host`), `sign_in_name_confirmed` (bool), `dual_role` (bool). `404` if none. `by=sign_in_name` is how web does an Entra first link and how the console matches `X-Forwarded-User`. |
| `POST /internal/users/{handle}/identities/entra` | Web | First Entra link (§7). |
| `GET /internal/admin/users`, writes in §6 | Web, with an assertion | Admin screens. |

Per-client projections are fixed in code and tested: a token that may not see a field never
gets it. `resolve` can't carry an assertion (there is no signed-in subject yet at sign-in), so a
stolen web or console token can look people up one at a time. It returns no address. **Misses
are rate-limited, hits are not:** each client may get at most 20 `404` answers a minute
(a token bucket per client); past that, a miss answers `429 resolve_misses` and is logged by
kind, while hits keep being answered. A hit names a real, signed-in user and is cached by the
caller for 10 seconds, so a busy team never reaches the limit; a token used to guess names or
`oid`s does. Every miss is logged with its kind (never the value), and more than 100 misses in
an hour from one client raises an admin overview warning.

### Caching and invalidation

| Service | Reads | Cache | Allow-decision bound while the tool server is reachable |
| --- | --- | --- | --- |
| Tool server | Its tables | None for decisions. MCP sessions re-check `status` on each call through a 5-second in-process cache keyed by `directory_version`. | Immediate (≤ 5 s for an open MCP session) |
| Web | `resolve` per handle | 10 s per handle | ≤ 10 s |
| Console | `resolve` | 10 s per handle | ≤ 10 s |
| Runtime | `directory` | Poll every 10 s with `If-None-Match`; a snapshot older than 30 s is not used to allow | ≤ 30 s |
| Slack | `directory` | Same as the runtime | ≤ 30 s |
| Workers, harness, gateway, docs, evals runner | Nothing | — | — |
| CLI (`pmos`) | Nothing; host user commands run inside the tool server container (§5) | — | — |

No push invalidation in the MVP: polling a version is cheap and has no new moving parts.

### Stated bounds

There are two, and the spec states both.

- **Tool server reachable** (the normal case): a deactivation or a removed role takes effect at
  the tool server at once (the same transaction revokes its MCP sessions), in web and the
  console within 10 seconds, and in the runtime and Slack within 30 seconds. Every task runs
  through a tool server session and every write through a tool server decision, so a removed
  person can *act* for at most 30 seconds anywhere and *write* nowhere after the change
  commits.
- **Tool server unreachable from web:** a signed-in web session may keep reading **pmos-web's
  own store** for at most **5 minutes** on its last-known-good answer (below). It never reads
  Slack-derived data on that path, never reaches another service (those calls fail anyway), and
  never writes. So the worst case for a person deactivated during an outage is 5 minutes of
  reading their own non-Slack follow-ups, settings, practice drafts and suggestions in web. The
  console, runtime and Slack have no such allowance for reads of their own data beyond what
  the table below says.

### When the store is unavailable

The tool server's database being down already stops sessions, approvals and audit. The table
says what each service does when it can't get a fresh answer.

| Action | Service | Behaviour |
| --- | --- | --- |
| Sign-in (new session) | Web, console | **Fail closed:** `503 users_unavailable`. |
| A request on an existing web session | Web | Last-known-good for that handle for at most **5 minutes**, for `GET` routes served from pmos-web's own store only. **Excluded:** follow-ups whose `system` is `slack` (`crates/web/src/followups.rs:4-5`) and the daily brief (`web_brief_days`, built partly from Slack), which answer with the Slack items withheld and the note "Slack items are hidden until PM OS can check your access". Any write, and any route that proxies to the tool server or runtime, fails closed. |
| Decide an approval, recommend | Tool server | **Fail closed** (`503 unavailable`). |
| Admin and auditor reads, any user or role write | Tool server | **Fail closed.** |
| Ask a bot | Runtime | May queue on a snapshot up to 5 minutes old; the task then needs a tool server session, which fails closed. Net: nothing runs on stale data. |
| Console reads of another PM's task | Runtime | Only on a snapshot ≤ 30 s old; otherwise the viewer is treated as not an admin (their own tasks only). |
| A routine falls due | Runtime | Snapshot older than 5 minutes: skip this run (don't pause the routine), log it. |
| Slack id → handle | Slack | Last-known-good up to 5 minutes; after that, answer "PM OS can't check who you are right now". Onboarding decisions and deactivation fail closed. |
| Restricted-draft audience check | Tool server | Fail closed (already: `audience_unavailable`). |

A service never falls back to `team.yaml`.

## 3. Migrating existing members

**Recommendation: a one-time, idempotent import run on the host, then the file is deleted from
the content repo.**

- **Command:** `pmos-admin users import --from <path> [--commit <sha>] [--allow-admins | --bootstrap] [--apply]`,
  which runs `pmos-toolserver users import` in the tool server container with its database
  access. Without `--apply` it prints what it would do (create, unchanged, conflict) and changes
  nothing.
- **Idempotent:** keyed by handle. A user that exists with the same fields is unchanged. A field
  that differs is a **conflict**, reported and not overwritten: after cut-over the database wins.
  Re-running never removes anyone and never revokes a role.
- **Mapping:** `slack-id` → `slack` identity; `email` → **both** `sign_in_name` and
  `atlassian_email` (the file has one value; the dry run lists every user so the operator can
  correct either afterwards with `set-sign-in-name` / `set-atlassian-email`, §5); imported
  sign-in names count as confirmed (a reviewed pull request put them there); `github` →
  `github_login`; `pm`, `reviewer`, `auditor`, `admin` as they are; `skill-author` → dropped,
  the person keeps `pm` (owner Q4), reported. An unknown role, a duplicate key, a `bots.*`
  handle, an `evals` entry with any role but `pm`, or a person holding `admin` and `auditor`
  while no pilot exception is open stops the import before any write.
- **Admins** are imported only with `--allow-admins`, which lists them and asks for
  confirmation on the terminal. The file's admins were granted by reviewed pull requests, so
  this is the one time a file grants admin. Imported Admins and Auditors have **no** Entra link;
  the host sets each one (`set-entra`, §5, §7).
- **Tamper evidence:** the import records one `role_changes` row with `change = 'import'`,
  `target = '*'` and `detail = "team.yaml sha256 <hex> commit <sha>"`, plus a `users.import`
  audit event. It reads the file from a path the operator names (normally the checkout at a
  reviewed commit), never from a URL.
- **Then:** a content pull request deletes `team.yaml` and its `CODEOWNERS` line; the same
  release ships the `team-file-retired` lint error.

**Order on a real install** (written into `docs/runbooks/upgrade.md` by U7):

1. Deploy the release; the tool server runs the migration at start. The users table is empty;
   nothing reads it yet for decisions (the cut-over flag, U3, is off).
2. Either `pmos-admin users bootstrap-admin …` (§5) and then `import` without
   `--allow-admins`, or `import --allow-admins` directly. An import that would leave users but
   no active admin is refused by the command with that reason before it writes (the trigger
   would refuse the commit anyway).
3. `pmos-admin users set-entra <handle> --oid <oid>` for each Admin and Auditor.
4. Turn on the cut-over (U3's `PMOS_USERS_SOURCE=database`, which becomes the only behaviour in
   U7), then delete `team.yaml` by pull request.

**Rollback.** The previous release's binaries still read `team.yaml`. Before rolling back,
`pmos-admin users export --format team-yaml > team.yaml` writes the current database as a
`team.yaml` (active users, their roles, Slack id, GitHub login; the `email` field gets the
Atlassian email, and the export warns for each user whose sign-in name differs, because the old
file can't hold both). Commit it, roll back. The tables stay; rolling forward again re-runs the
import, which reports no conflicts except those warned about.

**Alternatives.** *Keep `team.yaml` read-only as a seed* — rejected: two places that look like
the truth, and a PR to the seed would look like a role grant. *Keep it only for lint* — rejected
for the same reason. *Dual-read during a transition* — rejected: the two disagree the moment
anyone changes a role.

### Test stacks

Two paths, by whether the stack has a database.

| Stack | Store | How users get there |
| --- | --- | --- |
| `scripts/claude-test-stack.sh` | In memory (`PMOS_DEV=1`, no database URL; `:238`) | `PMOS_USERS_SEED=$STATE/users.seed.yaml` with the tester as `pm, admin` |
| Eval stack (`deploy/pmos-admin eval_stack_up`, `deploy/compose.evals.yaml`) | In memory (`PMOS_DEV: "1"`, no `PMOS_DATABASE_URL`; `compose.evals.yaml:44`) | Seed with `evals` (`U0EVALS`), `ben`, `cy`, so the fake channel audiences mean what the eval tasks say. `evals` is `pm` only. |
| CI smoke (`.github/workflows/ci.yml:117-135`, the full `deploy/compose.yaml` stack with Postgres) | **Postgres** | `deploy/pmos-admin users import --from deploy/test/ci-users.yaml --bootstrap --apply` |
| A real install | Postgres | §3 order above |

(The first review said the eval stack runs with a database; it doesn't on this branch, so the
seed covers it. CI smoke does, which is why `--bootstrap` exists.)

- **The seed** (`PMOS_USERS_SEED=<file>`, the same YAML shape as `team.yaml`, parsed by the
  import code) is **honoured only with `PMOS_DEV=1` and in-memory stores**; with
  `PMOS_DATABASE_URL` or live connectors the tool server refuses to start if it is set. A seed
  may give `evals` a fake Slack id: the in-memory store applies the `evals` rule to Entra,
  Atlassian and GitHub keys only, and only with `PMOS_CONNECTORS=fake`.
- **`import --bootstrap`** is the import-and-bootstrap path for a database-backed stack. It is
  accepted only when the users table is **empty**; it takes the file's admins without the
  terminal prompt, writes everything in one transaction (so the last-admin trigger sees the
  admin and the PMs together) and records `detail = "bootstrap sha256 <hex>"`. On a table with
  any row it exits `3` without writing. It is safe on a real install for the same reason
  `bootstrap-admin` is (§5): only the host can run it, and only once.
- **The personal bot** half of `pmos member add` becomes `pmos bot add-personal --root <content>
  --handle <h>` (content only, no user). CI smoke runs it after the import.
- **Transition:** between U3 and U7 some services still read the file. U3 makes the stacks do
  both (the old `pmos member add` and the new seed or `--bootstrap` import); U7 removes the
  file half.

The "open" team for fake connectors (`team.rs:21-28`, anyone with a valid handle) goes away in
release binaries: dev and eval stacks use a seed. An open directory stays available to unit
tests only, behind a `test-support` feature.

## 4. Slack onboarding

**Today:** a request goes to `member_requests` (`migrations/0006_onboarding.sql:19-35`); an
admin's approval commits `team.yaml`, the bot and its policies.

**After:**

1. Someone not in the directory taps **Request access**. Unchanged: a `member_requests` row,
   status `pending`, with the name and email the Slack service read from `users.info`
   (`crates/slack/src/members.rs:217-252`). The request **is** the pending record; no user row
   exists yet, and the person has no role and no access. A Slack id that belongs to a
   *deactivated* user gets `409 deactivated` ("your access was turned off; ask a platform
   owner"), not a new request.
2. An admin approves (Slack card, or the web admin screen). In one transaction the tool server
   creates the user (`active`, `display_name` from the request, cleaned; `slack` identity;
   `atlassian_email` and `sign_in_name` both proposed from the request's email) with roles
   **`pm` only from Slack**; from web, `pm` plus optionally `reviewer`. The request becomes
   `approved`. The person can use PM OS in Slack at once.
3. **Where the email came from decides what it can do** (review item 4). The bot client is
   unsigned, so a stolen bot token can invent a request's email and name a real admin as the
   approver. A user approved **through the bot client** gets `sign_in_name_confirmed_at = NULL`:
   they can use Slack, link Atlassian (the OAuth sign-in proves the account) and GitHub, but
   **cannot first-link Entra** until an admin confirms the sign-in name in the web app (Team
   page: "Confirm Microsoft sign-in name", editable before confirming; a signed web write,
   `POST /internal/admin/users/{handle}/sign-in-name/confirm`, recorded as
   `confirm_sign_in_name`). A join approved **in the web app** is confirmed in the same
   signed transaction, because the admin sees and can edit both addresses on the approval
   screen. The admin overview lists "joined from Slack, Microsoft sign-in not confirmed".
4. Then the tool server commits the personal bot files (`BOT.md` and the two policy files) as
   today, minus `team.yaml`, with the approving admin's GitHub token and the `Approved-by`
   trailer. If that commit fails, the user stays active, the setup checklist shows the bot as
   missing, and an admin can retry with `POST /internal/admin/users/{handle}/personal-bot`.
   (Trade-off: access no longer waits for GitHub. The bot is what needs review; the person's
   access is the admin's decision, which is already recorded.)

*Why confirmation rather than the tool server fetching the email itself* (the reviewer's other
option): fetching needs Slack's bot token (`xoxb`, with `users:read.email`) inside the tool
server, which today holds only the Slack OAuth client for PMs' own links
(`crates/toolserver/src/config.rs:446-448`). That widens where the bot token lives, and a Slack
profile email is still not proof of the Microsoft account. The admin's confirmation is a signed
web action by a real admin, costs one click per joiner on a small team, and keeps the Slack
token where it is.

**Role limits by channel.** The Slack bot's token names the deciding admin without a signature
(`docs/security-fixes-spec.md` S2), so from the bot client any role but `pm` is refused:
`admin` keeps `403 admin_by_pull_request` renamed to `403 admin_not_from_slack`, and `reviewer`
or `auditor` get `403 role_not_from_slack`. Onboarding never grants admin, from any channel.
Every admin's join card already shows the outcome and who decided (`members.rs:1-15`), so a
forged approval is visible to every admin at once.

`remove <handle>` in Slack becomes **deactivate** (§6) and stays admin-only; it no longer
commits anything. From the bot client it is refused for a target holding `admin` or `auditor`
(`403 protected_not_from_slack`) and counts toward the hourly cap (§6).

## 5. The first admin

**Recommendation: a host command, refused once any active admin exists.**

```text
pmos-admin users bootstrap-admin --handle raj --sign-in-name raj@contoso.com \
  --atlassian-email raj@contoso.com --slack-id U0RAJ (--oid <guid> | --no-entra) [--name "Raj"]
```

It runs `pmos-toolserver users bootstrap-admin` inside the tool server container with direct
database access. It creates the user with `pm, admin`, a host-confirmed `entra` identity
(`tid` = the configured tenant, `oid` as given) unless `--no-entra` (CI, or a dev install
with no Entra), writes `role_changes` (`client = 'host'`) and an audit event, and refuses
(`exit 3`) if an active admin already exists. Host access to the Spark is already the highest
trust in PM OS (it holds every secret), so this adds no new trust. The admin finds their `oid`
in the Entra admin center (Users → the user → Object ID); `docs/entra-sign-in.md` says how.

**Every host user command records the invoking OS user.** `pmos-admin` passes
`PMOS_HOST_USER="${SUDO_USER:-$(id -un)}"` into the container; `pmos-toolserver users …`
refuses to run without it (exit 2) or with a value that doesn't match the `host_user`
pattern, and writes it to `role_changes.host_user` and the audit event's detail
(`host_user <name>`). It is what the host says, not a proof; the host is already trusted.

**Host user commands** (all `pmos-admin users …`, all recorded, none capped):

| Command | Does |
| --- | --- |
| `bootstrap-admin` | Above. |
| `grant-admin <handle> (--oid <guid> \| --keep-entra)` | Grants `admin`. The person must be active. `--oid` sets or confirms the host-confirmed Entra link in the same transaction; with an existing automatic link whose `oid` differs, it refuses. `--keep-entra` only when the existing link is already host-confirmed. Owner Q1: the only way to make an admin. |
| `revoke-admin <handle>` | Break glass if the web is down; same rules as the web route. |
| `set-entra <handle> --oid <guid>` / `unset-entra <handle>` | Sets or clears a host-confirmed Entra link. Needed for every Admin and Auditor (§7). |
| `set-sign-in-name <handle> <name>` / `set-atlassian-email <handle> <email>` | The only way to change these for an Admin or Auditor (§6). |
| `deactivate <handle>` / `reactivate <handle>` | Not subject to the hourly cap. Reactivation never restores `admin`. |
| `pilot-exception admin-and-auditor on --until <date>` / `off [--revoke-auditor]` / `status` | §6. |
| `import`, `export`, `list` | §3. |

**Losing the last admin.** Refused in three places: the API (`409 last_admin` on demoting or
deactivating the last active admin), the self-action rule (an admin can't change their own roles
or deactivate themselves), and the database trigger (§1), which takes the advisory lock itself
and so also covers concurrent changes.

**Alternative rejected:** an env-named bootstrap admin (`PMOS_BOOTSTRAP_ADMIN=raj`) honoured
while no admin exists. It is a standing latent grant in `deploy/.env`: if the admins are ever
all gone (or the trigger has a bug), the variable silently makes someone admin again, and the
variable outlives the person who set it.

## 6. Role administration

**Who may change what.**

| Change | Who | Channel |
| --- | --- | --- |
| Add a user (invite), approve a join request | Admin | Web (assertion), or Slack for `pm`-only approvals (sign-in name then unconfirmed, §4) |
| Grant or revoke `pm`, `reviewer`, `auditor` | Admin | Web. Granting `auditor` to an admin needs the pilot exception (below). |
| **Grant `admin`** | Someone with host access | **Host CLI only** (owner Q1) |
| Revoke `admin` | Admin, not on themselves, not the last | Web; host |
| Deactivate | Admin, not themselves; capped (below) | Web; Slack (`remove`) only for a target holding neither `admin` nor `auditor`; host uncapped |
| Reactivate | Admin, not themselves | Web; host. Never restores `admin`. |
| Assign reviewers to PMs | Admin | Web |
| Confirm or change a sign-in name; change an Atlassian email or GitHub login | Admin, for a target holding neither `admin` nor `auditor` | Web |
| The same for an Admin or Auditor | Someone with host access | Host CLI only |
| Unlink an Entra identity | Admin, for a target holding neither `admin` nor `auditor` | Web; host for anyone |
| Switch the Admin + Auditor pilot exception | Someone with host access | Host CLI only |

No one changes their own roles, status or addresses, in any direction. The tool server checks
the actor in its own tables on every write; the assertion's `roles` are never used for this.

**When addresses may change** (review item 5).

- `sign_in_name`: an admin may set it in web while the target has no Entra link; setting it is
  also the confirmation. Once linked, sign-in uses `(tid, oid)` only and the field is
  informational; changing it then is allowed and recorded but moves nothing. For an Admin or
  Auditor: host only.
- `atlassian_email` and `github_login`: an admin may change them in web only while the target
  has no linked account of that kind. **After the first link the account id decides**: the
  Atlassian `accountId` (already returned by `/me` and kept as `credentials.external_id`) and
  the GitHub numeric user id are stored as the `atlassian` and `github` identities. A re-link
  must present the same id (`409 different_account` otherwise); the email or login is then
  ignored. To move a person to another Atlassian or GitHub account, an admin unlinks first
  (which deletes the credential) and then changes the field. For an Admin or Auditor: host
  only.
- `display_name`: an admin in web, for anyone. Untrusted text either way (§10).

**Admin API** (tool server; `pmos-web` exposes the same under `/api/admin/users…`):

| Route | Body | Notes |
| --- | --- | --- |
| `GET /internal/admin/users?user=<viewer>` | — | `ManageMembers` or `ViewAuditLog`. Status, roles, display name, flags (`dual_role`, `sign_in_unconfirmed`, `entra_needs_host`), and booleans: `has_sign_in_name`, `slack_linked`, `entra_linked`, accounts linked. Never an address, `oid` or Slack id to an auditor. |
| `POST /internal/admin/users` | `{handle, display_name, sign_in_name, atlassian_email, github_login?, slack_id?, roles}` | Creates `active`, sign-in name confirmed. `roles` must not contain `admin`. |
| `PUT /internal/admin/users/{handle}/roles` | `{roles, expected_version}` | Whole set; `409 stale` if the directory moved. |
| `PUT /internal/admin/users/{handle}/fields` | `{sign_in_name?, atlassian_email?, github_login?, display_name?, expected_version}` | Rules above; `403 host_only` for an Admin or Auditor target. |
| `POST /internal/admin/users/{handle}/sign-in-name/confirm` | `{sign_in_name}` | §4. Must equal the stored value or replace it. |
| `POST /internal/admin/users/{handle}/deactivate` / `reactivate` | `{}` | Semantics below. |
| `DELETE /internal/admin/users/{handle}/identities/entra` | — | For a reassigned mailbox (§7). |
| `PUT /internal/admin/reviewers/{reviewer}` | `{pms: [handle]}` | §8. |

**Write routes need an assertion, and never run on the development override.** Since
`52c45e3` the tool server is in `require` whenever the web token is set (§0). These routes
additionally answer `503 assertion_required` if the service started with
`PMOS_ASSERTION_INSECURE_OFF=1`. The subject must equal `user=` (the actor). Only the web token
reaches them; the orchestrator and console tokens get `403`; the Slack bot token reaches only
the `pm`-only join approval and the capped `remove` → deactivate. Every write, allowed or
refused, writes `role_changes` (when allowed) and an audit event (always), with who, what and
when, and no secrets.

**Separation of duties.**

- **Admin and Auditor:** refused for one person by the database trigger, except under the
  pilot exception below (owner Q3). Today only a lint warning (`lint.rs:906-916`).
- **Reviewer and owner of the same draft:** a reviewer never recommends on their own draft
  (`403 own_draft`); they decide it as its owner.
- Reviewer and Admin together: allowed. Admins already see more.

### Pilot exception: Admin and Auditor (owner Q3)

The owner allows one person to hold both for the pilot, because the team is small. The design
keeps it visible and makes it end.

- **On:** only `pmos-admin users pilot-exception admin-and-auditor on --until <date>` on the
  host. `--until` is required and at most 90 days ahead (the table's `CHECK`). It inserts a
  `pilot_exceptions` row, a `role_changes` row (`exception_on`, `target = '*'`,
  `detail = "admin_and_auditor until <date>"`, `host_user`) and an audit event
  `users.pilot_exception`. No web, console or Slack route reads or writes the table. Renewing
  is another `on` after `off` (recorded twice).
- **While on:** the trigger allows a person to hold both. Granting `auditor` to an admin happens
  in web as any other grant, recorded.
- **Flagged:** every person holding both appears on the admin overview in a list headed "Admin
  and Auditor (pilot exception, ends <date>)", with a badge on the Team page, and `dual_role`
  in `resolve` and the admin user list. Fourteen days before `expires_at` the overview shows
  "The pilot exception ends on <date>".
- **Their auditor reads are marked.** Every audit event the tool server writes for a read of
  the audit log or its export (`ViewAuditLog`, `ExportAudit`; the read events of
  `migrations/0011_audit_admin_reads.sql`) by a person holding both gets
  `detail = "dual_role admin+auditor"`. The audit page can filter on it, and the auditor's view
  marks rows whose actor is the viewer as "your own action".
- **Off:** `… off` ends the row (`ended_at`, `ended_by`), recorded `exception_off`. It is
  refused while anyone holds both, listing them, unless `--revoke-auditor` is given, which
  revokes `auditor` from each in the same transaction (recorded per person). Admin is the role
  kept, because the pilot needs it to run.
- **Lapsed:** past `expires_at` with nobody running `off`, the exception counts as off. The
  trigger then refuses any change that touches such a person until one role is gone, their
  audit-log reads answer `403 role_conflict` (admin powers stay), and the overview shows a red
  notice naming them. `off --revoke-auditor` clears it.

### Deactivation

One transaction, then a grace period, then clean-up; each step recorded.

| What | At deactivation (same transaction) | After 24 hours, unless reactivated |
| --- | --- | --- |
| Status | `deactivated` | — |
| `admin` role | **Revoked**, recorded `revoke` with `detail = "on deactivation"` (review item 1). Other role rows are kept but grant nothing. | — |
| Tool server MCP sessions | Revoked | — |
| Web and console sessions | Ended on the next request (≤ 10 s), as removal does today (`crates/web/src/app.rs:560-580`) | — |
| Linked accounts | **Suspended:** `credentials.suspended_at` set; the vault refuses to unseal them, so no connector call can use them (`suspend_credentials`) | **Deleted** by the tool server sweeper, as removal does today (`onboarding.rs:816-826`), recorded `delete_credentials` |
| Pending approvals | **Held:** still `pending`, but `decide` refuses because the owner isn't active; Slack drafts keep expiring on their own TTL | Closed by the system as `rejected` with ledger reason `owner_deactivated`; no write runs |
| Routines | The runtime pauses them when its snapshot shows the change (≤ 30 s), with the existing "paused" note | — |
| Personal bot | Refused by `may_use` (owner not active). The sandbox keeps idling until `pmos-admin pause pm-<handle>`; the API reply says so. Bot files stay in content. | — |
| Restricted-draft audience | Their Slack id counts as an **outsider** (§2 of `docs/slack-reply-spec.md`) | — |
| Team bots they own | Refused `409 owns_bots` until reassigned, as today (`onboarding.rs:746-766`) | — |
| Reviewer assignments, memory-repo record | Kept, inert | — |

**Audience insiders,** for the restricted-draft check, are active users holding `pm`,
`reviewer` or `admin`. A person holding only `auditor` is an **outsider** (the reviewer's
suggestion, adopted): an auditor reads metadata only, so restricted text posted where they can
read it is a leak. A deactivated person is an outsider.

**Reactivation** within the grace: status `active`, kept roles grant again (never `admin`;
§5 `grant-admin`), credentials un-suspended, held approvals decidable again if they haven't
expired. After the grace: the same, but accounts must be linked again and held drafts are
gone. Recorded `reactivate`.

**The hourly cap.** At most **3 deactivations in any rolling hour** from the web and bot
clients together, counted from `role_changes` under the advisory lock (so concurrent requests
can't both slip under). The fourth answers `429 deactivation_limit` ("PM OS has stopped
deactivations for an hour. Someone with host access can continue with `pmos-admin users
deactivate`"), writes a denied audit event, and shows on the overview. The host command isn't
capped. `PMOS_USERS_DEACTIVATIONS_PER_HOUR` (1–10, default 3) changes it.

The sweeper runs beside the existing draft sweeper (`crates/toolserver/src/main.rs:291-300`),
every `PRUNE_EVERY`; it fails closed (does nothing) if the database is unavailable.

## 7. Entra

### What the code does today (checked)

The reviewer's description matches the code. `crates/web/src/entra.rs` validates the ID token
(RS256, issuer, audience, time, nonce, `tid`, `:644-700`) and then names the person by
`preferred_username`, or else `email`, the first that parses as an email (`:701-706`). The
claims struct has no `oid`, `acct` or `idp` (`:369-380`). `crates/web/src/app.rs:789-795` looks
that address up in `team.yaml` (`auth.rs:115-125`, unique match only). The single-tenant `tid`
check doesn't exclude guests: a B2B guest signs in to this tenant and gets this tenant's
`tid`. `docs/entra-sign-in.md:49-63` documents email matching and the shared Atlassian/Entra
email.

### Microsoft's guidance, as I know it

**Confirmed for U4c** (2026-11-17) against Microsoft's ID token claims reference, optional
claims reference and claims-validation guidance, with quotes and links in
`docs/entra-sign-in.md` ("What Microsoft says about the claims"). The points below hold; for
`idp`, an ordinary member's token carries none, or the issuer's value.

- `oid` with `tid` is the stable, immutable identifier for a user in a tenant, and is what an
  app should key on. `sub` is pairwise per app and also stable, but `oid` is what other
  Microsoft APIs use.
- `preferred_username` is the sign-in name (usually the UPN in v2.0 tokens). It is mutable, can
  be reassigned, and Microsoft says not to use it for authorization decisions.
- `email` is optional, may be absent, and its value can be one the domain owner never verified
  (for example a guest's or an admin-set address). Microsoft added the `xms_edov` optional claim
  to say whether the domain owner verified it, and stopped emitting unverified emails by default
  for multi-tenant apps. It is not meant to identify a user.
- Guests: the `acct` optional claim is `0` for a member of the tenant and `1` for a guest. The
  `idp` claim is present when the user signed in with another identity provider (a guest's home
  tenant, or a Microsoft account).

### Identity mapping

The stable key is `(tid, oid)`, stored as an `entra` identity. Add `oid`, `acct` and `idp` to
`IdClaims`. The app registration adds the optional `acct` claim to the ID token
(`docs/entra-sign-in.md`, "Token configuration", updated in U4c).

**Refused before any lookup:** a token without `oid`; a token without `acct` (`403
entra_misconfigured`, with the setup hint, so a missing optional claim fails closed); `acct` ≠
`0` (guest: `403 not_a_member`); an `idp` claim that isn't this tenant's issuer.

1. **Known identity.** `resolve?by=entra&tid=&oid=`. Found and active: signed in, unless the
   user holds `admin` or `auditor` and the link isn't `host_confirmed`, which answers `403
   entra_needs_host` ("ask someone with host access to confirm your Microsoft account"). Names
   in the token are ignored from then on, so a renamed or reassigned mailbox can't move the
   sign-in to another person.
2. **First link.** Not found by `oid`: `resolve?by=sign_in_name` with `preferred_username`
   only. **No fallback to `email`.** It links only if all of these hold: exactly one user has
   that `sign_in_name`; the user is `active`; their sign-in name is **confirmed** (§4); they
   hold neither `admin` nor `auditor` (those are linked on the host only, §5); and they have
   **no** Entra identity yet. Then `POST …/identities/entra` stores `(tid, oid)` in one
   transaction that re-checks every condition. Audit `users.identity_link`; the user is told on
   their first page ("You're signed in with Microsoft for the first time") and admins see it on
   the overview.
3. **Conflict.** Not found by `oid`, and the `preferred_username` matches a user who **already
   has a different** Entra identity: refused `403 not_a_member`, audited
   `users.identity_conflict` (denied; detail `entra`, never the address or `oid`), and flagged
   on the admin overview ("Someone signed in with Microsoft as <handle>'s sign-in name using a
   different Microsoft account"). The same flag fires when a known `oid` signs in with a
   `preferred_username` that is another user's sign-in name.
4. Anything else: `403 not_a_member` with the existing wording.

If the attacker links first (a mailbox reassigned before its owner's first sign-in), the real
person gets the conflict path, which is now audited and flagged instead of silent; an admin
unlinks the wrong identity.

**Granting `auditor` to someone with an automatic link** keeps the link but marks it
`needs_host` until the host runs `set-entra <handle> --oid <same oid>`; `grant-admin` needs
`--oid` (§5). So every Admin's and Auditor's Microsoft account was named on the host.

**Why still auto-link PMs and Reviewers:** an admin entered or confirmed the sign-in name (web
invite, web confirmation, or import from a reviewed file), never the bot client alone, and it
matches only `preferred_username`. Confirming every PM's `oid` on the host would make the host a
bottleneck for routine joins. The residual risk is a sign-in name reassigned before the first
link; the answer is the conflict flag and the admin unlink.

**Entra app roles and groups: ignored for the MVP.** The database is the only source of roles.
A one-way sync would make tenant admins (outside PM OS's audit log) able to grant PM OS admin,
and would need a precedence rule against admin edits. Revisit with SCIM if the team grows.

**Just-in-time user creation: no.** A tenant member who isn't in the directory is refused.
Joining stays an admin decision (Slack request or web invite).

The dev provider (`PMOS_WEB_DEV_LOGIN=1`, `crates/web/src/config.rs:43,93`; `deploy/pmos-admin`
enables it only with `PMOS_WEB_ENABLE_DEV_LOGIN_CLOSED_TEST_NETWORK_ONLY=1`) keeps resolving by
handle, closed test networks only.

## 8. The Reviewer rule, enforced

**Who reviews whom: assigned per PM** (`reviewer_assignments`, set by an admin; owner Q2). A
Reviewer sees and recommends on the pending drafts of the PMs assigned to them, and no one
else's. Rejected alternatives: *any Reviewer sees every draft* (a much wider read of draft text
and sources, which can come from restricted Jira or Slack content); *assigned per draft* (the
owner picks a reviewer each time; flexible, but more UI and an extra step on every draft — a
later addition).

**A draft has one version everywhere.** The B1 fix (in flight) saves a PM's web edit on the
approval item in the tool server, through a new owner route, instead of only in pmos-web
(`web_draft_edits`), so Slack and web approve the same text. This spec relies on it adding a
`revision` (an integer bumped by each saved edit) to `approvals`; if the B1 fix lands without
one, U6 adds `approvals.revision integer NOT NULL DEFAULT 1`, bumped by that owner route.
`draft_sha256` is the SHA-256 of the item's current arguments, canonical JSON.

**What a recommendation is.** A row in `approval_recommendations`: `verdict` `approve`,
`reject` or `changes`, an optional note (≤ 1000 characters, plain text), the `draft_revision`
and `draft_sha256` the reviewer read, one live recommendation per reviewer per draft (a new one
withdraws the old). Only on `pending` drafts.

**Route:** `POST /internal/approvals/{id}/recommendation` with
`{recommended_by, verdict, note?, draft_revision}`. Web token only, assertion required (subject
= `recommended_by`). The check lives in a new `ToolService::recommend`, beside
`decide_verdict` (`service.rs:1777`):

1. `recommended_by` is active and has `RecommendOnApproval` **in the database**;
2. the item is pending and `item.user` is assigned to them;
3. `recommended_by != item.user` (`403 own_draft`);
4. `draft_revision` equals the item's current `revision` (`409 draft_changed` otherwise, with
   the new revision, so the reviewer re-reads);
5. insert with the current hash, then audit `approvals.recommend` (metadata: actor, item id,
   verdict, revision; never the note).

**It can never publish.** The route has no path to `decide_verdict` or a connector. `decide`
ignores recommendations entirely and still requires `decided_by == item.user`. A test asserts
that after any sequence of recommendations the item is still `pending` and no connector call
was made, and that a reviewer's `decision` call on someone else's item is `403`.

**What the owner sees.** Web: the draft page shows "Ana (reviewer) recommends approving" with
the note, above the owner's own buttons. When the draft has changed since (the item's revision
is past the recommendation's), it reads "Ana recommended approving an earlier version" and the
note is shown greyed. Slack: the approval card gets one context line with the reviewer's name
and verdict, and "(earlier version)" when stale; the note is in the web view only, to keep
content out of more Slack messages. The owner's decision is recorded with a fixed detail
`"after a recommendation to <verdict> on this version"` or `"… on an earlier version"`, so the
audit shows whether the advice was followed and on what.

**What a Reviewer may read** — wider than today, stated plainly:

- `GET /internal/approvals?reviewer=<handle>`: assigned PMs' pending drafts (tool, target,
  draft text at the current revision, created time), and `GET /internal/approvals/{id}/sources`
  for those items (today owner-only, `app.rs:815-850`).
- **Restricted drafts** (`sources_sensitivity` `restricted` or unrecorded): metadata only, "this
  draft uses restricted sources; its owner can share it with you" (owner Q6). Since W6 is fixed
  (§0), an item's `sources_sensitivity` includes what its conversation history read, so a
  follow-up built on restricted content is labelled restricted, not internal.
- **Slack drafts** keep their text only until `PMOS_SLACK_DRAFT_TTL_HOURS` (default 24). A
  reviewer can read and recommend only while the text exists; notes on a Slack draft are removed
  when its text is. Slack message text never reaches the audit log or a report.
- No traces, no tasks, no routines of others (`ViewOthersTasks` stays admin-only).

**Untrusted text.** The note and the reviewer's display name are rendered as plain text: React
text nodes in web (no Markdown, no HTML, no links made from them), and in Slack only as escaped
`plain_text` (with `<`, `>` and `&` escaped, so `<@U…>`, `<!channel>` and links can't be
formed). Tests feed both a `<!channel>` and an `<img onerror>` string.

**Assertion changes:** none to the wire format. The new routes are added to the assertion
binding rules (subject must equal `recommended_by`, and `reviewer=` on the list).

**Matrix change:** none to the Reviewer row (`permissions.rs:157-160`: it already has
`ViewOthersApprovals` and `RecommendOnApproval`). The *scope* of "others" (assigned PMs only)
is the caller's check, documented in the module docs like the `*Own*` rules.

## 9. The signed user assertion

- **When it is required:** since `52c45e3`, whenever a web or console token is configured, both
  receivers (tool server, runtime) run in `require` unless the development override is set
  (§0). Nothing in this spec relaxes that; user writes refuse the override outright (§6).
- **Where roles come from:** `pmos-web` and the console put the roles they read from `resolve`
  (cached ≤ 10 s) into each assertion they sign. The assertion lives 60 s
  (`docs/user-assertion.md`, "Clocks"). So a role shown in an assertion can be up to **10 s +
  60 s + 5 s skew = 75 s** stale.
- **Sessions hold no roles.** Web sessions store only the handle (as today); roles are looked up
  per request. There is nothing to refresh.
- **Receivers re-check what matters.** The tool server checks roles in its own tables for every
  admin read, user write, approval decision and recommendation. The runtime checks `AskBot` and
  membership from its directory snapshot (≤ 30 s), not from the assertion. The assertion's
  `roles` field stays informational.
- **The console's single-item reads,** which today need an assertion for the item's PM or "an
  admin in the receiver's own `team.yaml`": at the tool server (`app.rs:784-795`) the admin
  check reads `users`/`user_roles` in the same request (active and holding `admin`); at the
  runtime (`crates/bots/src/app.rs:556-565`) it reads the directory snapshot and counts an admin
  only on a snapshot ≤ 30 s old. A person holding only `auditor` gets no console at all
  (`403`, "the console is for PMs and admins; the audit log is in the web app").
  `docs/user-assertion.md` "Not addressed" and the two "own `team.yaml`" sentences change to
  "the directory".
- **`skill-author` in the wire format:** `Role::KNOWN` keeps it parseable for one release so
  mixed-version services don't refuse each other's assertions; nothing signs it after cut-over.

## 10. Auditor

**Unchanged promise:** metadata only. The auditor sees the user list (handles, display names,
roles, status, flags, booleans), never an address, Slack id, `oid`, a recommendation note,
draft text or a token.

**New audit events** (actor, client, target handle in `output_ref`, outcome):

| `action` | When | `detail` (fixed formats, added to `safe_detail`, `app/admin.rs:225-248`) |
| --- | --- | --- |
| `users.create` | Invite or join approval (replaces `members.add`) | `roles <list>`; `sign_in unconfirmed` for a bot-client join |
| `users.roles` | Roles changed | `+reviewer -pm` style, known role names only; `on deactivation` |
| `users.fields` | Sign-in name, Atlassian email, GitHub login, display name changed; sign-in name confirmed | field names only, never values |
| `users.deactivate`, `users.reactivate` | Status | `credentials suspended`, `credentials deleted` |
| `users.identity_link`, `users.identity_unlink` | Entra first link, admin or host unlink | `entra`, `atlassian`, `github` |
| `users.identity_conflict` | §7 conflict | `entra` |
| `users.import`, `users.bootstrap_admin`, `users.grant_admin`, `users.set_entra`, `users.pilot_exception` | Host commands | `host_user <name>`; `sha256 <hex>` for import; `on until <date>` / `off` |
| `reviewers.assign` | Assignments changed | `<n> pms` |
| `approvals.recommend` | A recommendation | `verdict <v> revision <n>` |
| Audit-log reads by a dual-role person | §6 pilot exception | `dual_role admin+auditor` |
| `users.*` with `denied` | Any refused write | the refusal code (`last_admin`, `self_change`, `role_not_from_slack`, `protected_not_from_slack`, `deactivation_limit`, `host_only`, …) |

The audit `client` check constraint gains `host` (the `migrations/0011_audit_admin_reads.sql:12`
pattern).

**Retention.** Audit events purge at 90 days (`CLAUDE.md` f; the purge job is in flight).
`role_changes` follows owner Q5: kept while the user is active, and deleted 90 days after
deactivation, by a daily tool server job that sets `pmos.role_changes_retention = on` for one
transaction; the trigger (§1) checks each row itself, so neither a bug in that job nor the
audit purge can delete a live user's history or the import and pilot-exception rows.

**Display names are untrusted** wherever they appear (web Team page, overview, Slack cards):
cleaned on write (control characters and `<`, `>` removed, trimmed, ≤ 100), rendered as plain
text as in §8.

## 11. Threats

| Threat | How the design answers it |
| --- | --- |
| Privilege escalation through onboarding | A request grants nothing. The Slack bot client may create `pm` only; admin is host-only; tested per role and per client. |
| Stolen Slack bot token | It can approve `pm`-only joins naming a real admin (visible on every admin's card), but those users can't sign in to web until an admin confirms their sign-in name (§4). **Residual:** a user approved through the bot client can use Slack and the team bots (and their own personal bot) at once, before any signed confirmation; the confirmation gates only Microsoft sign-in to the web app. It can deactivate non-Admin, non-Auditor users, at most 3 an hour (shared with web); credentials are suspended, not deleted, for 24 hours, and drafts are held, so a reactivation undoes it. It can't grant roles, touch addresses, or deactivate an Admin or Auditor. |
| Escalation through a pull request | No file grants anything after cut-over; `team.yaml` is a lint error; the import is a one-time host action. |
| Compromised web backend | It can sign as any user to the tool server (a known limit of assertions). It **can't grant admin** (host-only), can't change an Admin's or Auditor's addresses or Entra link (host-only), can't remove the last admin (trigger), can't switch the pilot exception, and every change is in `role_changes` and the audit log with `client = web`. It can change other users' roles and deactivate up to 3 people an hour, reversible within the grace (§6); after that, host only. |
| Compromised console | No write route accepts its token; single-item reads need a signed admin or the item's PM, checked against the database. |
| Entra account takeover at first link | Links on `preferred_username` only, a confirmed name, members only (no guests), never for an Admin or Auditor (host sets their `oid`); a conflicting sign-in is refused, audited and flagged (§7). |
| Stale cache after demotion | Bounded: tool server immediate, web/console ≤ 10 s, runtime/Slack ≤ 30 s while reachable; ≤ 5 min of web's own non-Slack data if the tool server is unreachable (§2). Writes are decided only at the tool server from the database. |
| Database unavailable | Fail closed for sign-in, decisions, admin; last-known-good ≤ 5 min only for web's own non-Slack reads and for queueing (§2). No fallback to a file. |
| Import tampering | Dry run first, conflicts never overwrite, admins only with `--allow-admins` and a terminal confirmation (or `--bootstrap` on an empty table, host only), file hash and commit recorded. |
| Last-admin lockout | API check, self-change ban, deferred trigger that takes the advisory lock itself, and the host break-glass commands. |
| Address reassignment | Sign-in keys on `(tid, oid)` after the first link, Atlassian on the account id, GitHub on its numeric id; addresses unique across all users; admin unlink. |
| Audit purge reaching role history | `role_changes` has its own trigger; the purge's exception doesn't apply to it; tested (§12). |
| Confused deputy between services | Per-client route allow-lists and field projections; user writes need an assertion whose subject is the actor; the orchestrator token can read the directory but write nothing. |
| Two sources of truth | One owner; no service reads `team.yaml`; the proof is a hostile-file test (§12). |

## 12. Build plan

Each slice is one agent, one worktree, one branch. All need the workspace checks in `CLAUDE.md`.

### Order against other work in flight

1. **The 90-day audit purge job** goes first. U1 then adds `role_changes` with its own trigger
   and a Postgres test that runs the purge's real delete path (whatever exception it uses for
   `audit_events`) and proves `role_changes` rows, old ones included, survive it. If the purge
   slips and U1 merges first, the purge slice must carry that same test before it merges.
2. **The B1 fix** (owner edits saved on the approval item) must merge before **U6**. U1–U5 don't
   depend on it. Whichever of the purge, B1 and U1 merges later renumbers its migration.
3. **W6** is done (`52c45e3`); U6 relies on it and adds a test that a follow-up's draft built on
   restricted history is metadata-only to a reviewer.
4. **Admin screens that read the team from the file** today: in the tool server
   `app/admin.rs` (`viewer` `:116-140`, `accounts` `:331`, `overview` `:423`, `proposal` `:571`)
   and the console approval read (`app.rs:784-795`); in web `app/admin.rs:415-470` and the
   sign-in and session code in `app.rs`. Admin-screen work already in flight merges **before U3**
   and is converted by U3 (tool server) and U4c/U5 (web). From U3's start, new code reads the
   `Directory`, never `TeamFile`; U7's workspace test enforces it.

### Slices

| # | Slice | Changes | Depends on | Key tests |
| --- | --- | --- | --- | --- |
| U1 | **Store** | Users migration; `UserStore` (Postgres + memory); `users_invariants()` (lock inside, isolation check, last admin, admin+auditor with the pilot exception, `evals` rule); `role_changes_append_only()` with the retention rule; `pilot_exceptions`; seed loader (`PMOS_USERS_SEED`, dev only); `pmos-toolserver users …` and `pmos-admin users …` (§5 table) with `PMOS_HOST_USER`; `import --bootstrap`; `pmos bot add-personal` | Audit purge (order 1) | Postgres (`PMOS_TEST_DATABASE_URL`): last admin, incl. two concurrent transactions **issued as raw SQL without the application lock**; a `REPEATABLE READ` write refused; admin+auditor refused with no exception, allowed while open, refused after `expires_at`; `--until` > 90 days refused; `off` refused while someone holds both, `--revoke-auditor` works; `evals` with `reviewer` or any identity refused; unique keys; `role_changes` `UPDATE` refused, `DELETE` refused for an active user, for a user deactivated 89 days ago, for `*` rows, and under the audit purge's exception; allowed for a user deactivated 91 days ago with the setting on; host commands refuse without `PMOS_HOST_USER` and record it; import idempotent, conflicts reported, unknown role aborts with no writes, no-admin import refused with a clear message; `--bootstrap` refused on a non-empty table; seed refused with a database URL or live connectors |
| U2 | **Read API** | `/internal/directory` (ETag), `/internal/users/resolve` (`entra`, `sign_in_name`, `handle`), `/identities/entra`; per-client projections; miss-only rate limit | U1 | Each token sees exactly its fields; web/console can't list; `304`; 21st miss in a minute is `429`, while hits keep answering; first-link conditions: unconfirmed name, Admin, Auditor, existing link, deactivated, two users racing |
| U3 | **Tool server cut-over** | `Team` → `Directory` in service, OAuth (link keyed on Atlassian account id and GitHub id after the first link), onboarding (DB write then bot commit; bot-client joins unconfirmed), deactivation (admin revoked, credentials suspended, approvals held, 24-hour sweeper, cap, bot client refused on Admin/Auditor), audience check (deactivated and auditor-only = outsider), MCP session re-check, admin reads incl. the four screens in order 4 and overview flags, console item read from the database, dual-role marking; `credentials.suspended_at`; CI and stacks seed/import alongside `member add` | U1 | Existing tool server suites green; `admin_not_from_slack`, `role_not_from_slack`, `protected_not_from_slack`; 4th deactivation in an hour `429` and the host command still works; deactivate then reactivate within 24 h restores credentials and drafts, after 24 h they're gone; **reactivating a former admin leaves them without `admin`**; a suspended credential is never unsealed; an open MCP session dies on deactivation; auditor-only member in the channel blocks a restricted draft; re-link with another Atlassian account id `409`; dual-role audit read carries the marker |
| U4a | **Runtime** | Catalog membership and `AskBot` from the directory snapshot; routine pause on deactivation; console task reads' admin check from a ≤ 30 s snapshot | U2 | Auditor-only member can't ask; stale-snapshot rules; stale snapshot → console admin sees own tasks only |
| U4b | **Slack** | Directory snapshot replaces `load_team`; admins from roles; `remove` → deactivate with the new refusals; display names escaped | U2, U3 | Socket tests with a seeded directory; `remove` of an admin refused; `<!channel>` in a display name renders inert |
| U4c | **Web sign-in** | `oid`, `acct`, `idp` claims; guest and missing-`acct` refusal; `preferred_username` only; resolve; conflict path; per-handle cache; last-known-good with the Slack exclusion; `/api/me` from the directory with `display_name`; `docs/entra-sign-in.md` | U2 | Entra first link; no fallback to `email`; guest refused; Admin/Auditor without a host link refused; conflict audited and flagged; reassigned name; deactivated user; `503 users_unavailable`; on the stale path Slack follow-ups and the brief are withheld and writes fail |
| U4d | **Console** | Resolve; role-aware viewer (auditor isn't a PM; auditor-only gets no console) | U2 | Proxy identity by sign-in name and handle; auditor-only `403` |
| U5 | **Admin writes and Team screen** | Write routes; `pmos-web` `/api/admin/users…`; Team page (invite with both addresses, roles, fields, confirm sign-in name, deactivate/reactivate with the grace and cap shown, reviewers, unlink, dual-role badge); overview lists (unconfirmed sign-ins, conflicts, dual-role, exception end date, resolve-miss warning); copy | U3, U4c | Every self/last-admin/role/host-only refusal; override → `503 assertion_required`; confirm then first link works; web unit, e2e and a11y tests |
| U6 | **Reviewer** | Assignments, recommendation route and store bound to revision and hash, reviewer inbox, owner view (stale marking), Slack card line, decision detail | U3, U5, **B1** | Recommendation never changes status or calls a connector; unassigned `404`; restricted metadata-only, incl. a W6 follow-up; `409 draft_changed` on an old revision; owner edit after a recommendation shows "earlier version" in web and Slack and the decision's detail says so; note removed with Slack text; note and name render as plain text |
| U7 | **Retire the file** | `team-file-retired` lint; remove `TeamFile` use from every service (parser stays only in the import/seed module); remove `pmos member add` (CI smoke and stacks keep only the import/seed and `pmos bot add-personal`); docs (`entra-sign-in.md`, `web-api.md`, `onboarding-spec.md`, `user-assertion.md`, upgrade runbook, `content/team.yaml` deleted) | U3–U6 | **The proof** (below); CI smoke green on `--bootstrap` alone |

**Status (2026-11-17): U1 is built** on `claude/u1-user-store` (migration `0015_users.sql`,
`crates/toolserver/src/users/`, `docs/runbooks/users.md`). Where it differs from the text above:
the advisory lock is keyed `(hashtext('pmos.users'), hashtext(current_schema()))`, the audit
purge's pattern, and every trigger function has a fixed `search_path`; the directory version is
bumped inside `users_invariants()`, after the lock; a `role_changes` DELETE needs the setting
**and** must run inside `purge_role_changes()` (the audit purge's call-stack check), which the
daily retention job calls; the handles `host`, `import` and `system` are reserved; a
`users.deactivated_at` check keeps it set exactly while deactivated; `reviewer_assignments` and
`approval_recommendations` are left to U6. A seed, like any import, needs an active admin, so
the eval stack's seed (U3) must name one besides `evals`, `ben` and `cy`.

**U2 is built** on `claude/u2-user-read-api` (`crates/toolserver/src/app/directory.rs`,
`crates/toolserver/src/users/link.rs`, `tests/directory_routes.rs`; no migration;
`docs/toolserver.md` "The user database's read API"). Where it differs from the text above:
the miss limit is a rolling minute per client (at most 20 `404`s in any 60 seconds) rather
than a refilling token bucket, so "the 21st miss in a minute" holds exactly; the first-link
route's `404`s count against `pmos-web`'s same window; the first link takes no assertion
(like `resolve`, there is no subject yet) and its refusals are `404 not_found`,
`403 not_active`, `403 sign_in_unconfirmed`, `403 host_only`, `403 evals_account`,
`409 linked`, `409 taken`; the same key again answers `200` with `first_link: false`. The
tool server doesn't check `tid` against a configured tenant (it has none outside the host
commands); `pmos-web` validates it (U4c). The more-than-100-misses-an-hour count is kept in
process and logged; U5 puts it on the overview.

**U3 is built** on `claude/u3-toolserver-cutover` (migration `0017_users_cutover.sql`:
`credentials.suspended_at` and the `owner_deactivated` ledger reason; `src/users/access.rs`;
`tests/users_cutover.rs`; `docs/toolserver.md` "The tool server decides from the user
database" and "Deactivation"). The tool server reads no `team.yaml` for any decision; the
runtime, Slack, `pmos-web` and the console still do (U4a–d). Where it differs from the text
above:
- **No cut-over flag.** §3's `PMOS_USERS_SOURCE=database` was not built: there is no live
  install with users, and two code paths in every check would be the larger risk. The
  database is the only source in the tool server from U3; an empty database refuses everyone
  (the server logs a warning at start-up), so step 4 of the real-install order is "import,
  then deploy U3" rather than "turn on the flag".
- **Onboarding still commits `team.yaml`** with the personal bot until U7: the other services
  read it, and `pmos lint`'s `bot-owner-unknown` checks bot owners against it. The database
  write comes first and decides; a failed commit leaves the user active and the request
  `approved` with `commit: null` and the error (the retry route is U5's).
- **The orchestrator token is unsigned too:** a join it approves is `pm` only, like the Slack
  bot's, and its `remove` is capped and recorded with client `bot`.
- **Who may use PM OS** is "active and holding `pm`, `reviewer` or `admin`" for sessions,
  decisions and links, the same set as the audience insiders: an auditor-only person gets no
  MCP session.
- **A deactivation's sessions** are deleted in its transaction (so a host command ends them
  too), and an open session also re-checks the user on each call through a cache of at most
  5 seconds that a write through the server empties; the cache is not keyed by
  `directory_version`.
- **Reactivation after the grace** deletes the suspended credentials and closes the held drafts
  itself, whether or not the sweeper ran first.
- **The overview's flags** are in `users` on `GET /internal/admin/overview`: conflicts count
  `users.identity_conflict` events of the last 7 days; the resolve-miss warning counts U2's
  misses per client in the tool server process (`ResolveMisses`, fed by U2's routes).
- **Atlassian and GitHub unlink** (removing the `atlassian`/`github` identity so another
  account can be linked) is not built: the orchestrator's credential unlink deletes the token
  but keeps the identity, by design. U5's admin unlink must remove the identity.
- **Dev stacks:** `scripts/claude-test-stack.sh` seeds from the stack's own `team.yaml`; the
  eval stack adds `eval-admin` (U0EVALADM, in no fake channel) to its `team.yaml` and seeds from
  it; CI smoke imports `deploy/test/ci-users.yaml` with `--bootstrap` beside `pmos member add`.

**U4a is built** on `claude/u4a-runtime-directory`: the shared crate `crates/directory`
(`pmos-directory`, `src/snapshot.rs`: the snapshot client), and in the bot runtime
(`crates/bots`, package `pmos-bots`; `crates/runtime` is the small shared-helpers crate)
`src/people.rs`, the catalog without the team, and `tests/directory.rs`; no migration;
`docs/bots-runtime.md` "Who may ask". The runtime reads no `team.yaml` for any decision and
never falls back to it; its reloader still watches the file only because the lint's
bot-owner check reads it until U7. Where it differs from the text above:
- **Bounds are named reads**, not a "stale ok" flag: `fresh()` (≤ 30 s, every allow),
  `for_queueing_ask()` (≤ 5 min), `for_due_routine()` (≤ 5 min) and, for U4b,
  `for_slack_identity()` (≤ 5 min). Each bound is inclusive (exactly 30 s still allows). The
  age is measured from when the last `200` or `304` was *requested*, on a monotonic clock; a
  failed poll keeps the old snapshot and its old age. A `304` for a snapshot the client
  doesn't hold is an error, not a refresh.
- **Before the first answer nobody may ask** (`503`); `/readyz` gained a fourth check,
  `directory` (a snapshot ≤ 30 s old).
- **Allows the table doesn't name** use the 30-second bound: parsing a routine (it calls the
  model as the owner), creating one, and setting a PM's level; past it they answer `503`. An
  ask past 5 minutes is `503`, never a guess from the old snapshot.
- **Routines pause for any owner who may no longer ask**, not only on deactivation: removed
  from the directory, deactivated, or left without `pm`, `reviewer` or `admin` (an Auditor
  only). The sweep runs on each new snapshot and on every 30-second tick, only on a snapshot
  ≤ 30 s old, and uses the existing guarded run (a failed task with "Your routine is paused:
  …" and a notification). A routine whose bot is gone still pauses at its next due run.
- **A skipped run** (snapshot older than 5 minutes when a routine falls due) moves the
  routine's next run to its next slot through a new guarded store call
  (`skip_routine_run`); it stays enabled, records no run and queues nothing. A due routine on
  a snapshot between 30 s and 5 min old runs (its session decides afresh) or pauses.
- **The console admin check** (`Runtime::is_admin`: active, `admin`, snapshot ≤ 30 s) also
  governs the console's routine list, routine changes and cancels, which used the same
  `team.yaml` check. An auditor-only console viewer still sees their own tasks: that rule is
  U4d's, in the console.
- **Stacks:** no new setting. The runtime already holds `PMOS_SERVICE_TOKEN` and
  `PMOS_TOOLSERVER_URL`, and every stack already seeds or imports the users the runtime is
  asked as (U3); CI smoke waits 35 s after its import, longer than one poll. The in-process
  stack test (`crates/stack-tests`) now seeds the tool server's user store and no longer
  opens the directory, so the runtime and the tool server both decide from the seed.

**U4c is built** on `claude/u4c-web-signin` (`crates/directory/src/resolve.rs`, the shared
resolve client the console's U4d reuses; `crates/web/src/app.rs` `Signed`/`SignedRead` and
`entra_person`; `crates/web/src/entra.rs`; `crates/web/tests/{web,entra}.rs` with a fake of
the read API in `tests/support/mod.rs`; a stack test against the real tool server;
`docs/entra-sign-in.md`, `docs/web-api.md`). `pmos-web` reads no `team.yaml` for sign-in,
sessions, roles, `/api/me`, admin access or the brief producer's membership check. Where it
differs from the text above:
- **`idp` accepts this tenant in either form:** absent, equal to the token's `iss`, or equal
  to the tenant's v1 issuer `https://sts.windows.net/<tid>/` (Microsoft documents `idp` as
  equal to the issuer for a member; the v1 form names the same tenant, accepted so a format
  difference can't lock members out). `acct` must be `0` (a number, or the string `"0"`);
  a missing `acct` is `entra_misconfigured`, any other value `not_a_member`. A token without
  a GUID `oid` is `failed`. The `email` scope is no longer asked for.
- **New signed-out reasons** beyond §7's `not_a_member`: `deactivated`,
  `sign_in_unconfirmed`, `entra_needs_host`, `entra_misconfigured`, `users_unavailable`, so
  a person is told what to ask for. A conflict, a guest and another identity provider stay
  `not_a_member` as §7 says. The tool server's sign-in events route accepts these reasons.
- **The second conflict flavour** (a known `oid` presenting another user's sign-in name) is
  recorded through the sign-in events route as a new action, `users.identity_conflict`
  (`member` = that user, detail `entra`), the same shape the first-link route writes, so the
  overview counts both; the person still signs in by `oid`. It costs one more `resolve` per
  such sign-in (a miss when the name is nobody's counts toward the miss limit).
- **An Entra session is checked by its `(tid, oid)`**, not its handle, on every request (so
  an unlinked or re-linked account ends it); the 10-second cache is per lookup key. Sessions
  from before U4c (no `oid`) end on their next request.
- **The stale path** serves `GET /api/me` and `GET /api/brief` only; the brief withholds
  every item whose `system` is `slack` from `changed` and both follow-up lists, and adds
  `slack_withheld: true` and the note. A last-known answer that says deactivated reads
  nothing. Development sign-in accepts a handle only (no longer an email).
- **The admin Team page's member list still comes from `team.yaml`** until U5 replaces it
  with `GET /internal/admin/users`; it decides nothing (access to the page is the viewer's
  roles from the directory), so `team_unavailable` stays for that page only. `/readyz` no
  longer checks `team.yaml`, and `pmos-web` starts without it.
- **Assertion roles** come from the directory answer for the request (`for_user`); a
  sign-out's come from whatever answer is cached, never a call.
- **Not built:** the "signed in with Microsoft for the first time" message (the link is
  logged and audited only). **Not verified:** a real Entra tenant's tokens (none available).

**U4b is built** on `claude/u4b-slack-directory` (`crates/slack/src/people.rs`, the copy in
`access_copy.rs`, `blocks::name`; `crates/slack/tests/socket_onboarding.rs` against a mock
tool server's directory over HTTP, `socket_docs.rs` against the real tool server's,
`no_team_file.rs`; no migration, no new setting). The Slack service reads no `team.yaml`
for any decision, never falls back to it, and starts without one. It polls
`GET /internal/directory` every 10 s with `PMOS_BOT_TOKEN` (`pmos-directory`'s
`SnapshotClient`, unchanged). Where it differs from the text above:
- **Who may use PM OS in Slack** is the tool server's and the runtime's set: an active user
  holding `pm`, `reviewer` or `admin`. Someone the directory doesn't have gets **Request
  access**; a deactivated person, an Auditor only, or a status this build doesn't know gets a
  plain line instead (no button), in DMs, tags, `/docs`, the App Home, buttons and modals.
- **Two bounds, by who decides.** The 5-minute identity read (`for_slack_identity`) maps a
  Slack id to a handle *and* says whether that person may use PM OS; it is enough for
  anything the runtime or the tool server decides again (a DM's task, a tag, an approval card,
  settings, routines, `/docs`), so §2's "≤ 30 s" allow bound for Slack holds where Slack
  itself decides: admin actions (deciding a join, the handle check, `requests`, `remove` and
  its target) and who gets a join card, plus proactive sends (nudges, the triage digest),
  all on `fresh()`. Past 30 s an admin action answers "PM OS can't check your access right
  now" and sends nothing; a filed request then tells no admin (they see it in `requests`).
  Past 5 minutes everyone, strangers included, is told "PM OS can't check who you are right
  now" (no **Request access** while it can't check); a snoozed reply card waits rather than
  being dropped; a notification for a PM is dropped and logged, as for a non-member before.
- **After an approval or a deactivation** the service polls the directory at once, so the
  change shows in Slack without waiting for the next poll.
- **`remove`** checks the admin and the target on a fresh snapshot before showing the card,
  and says why not in its own words (an Admin or Auditor, already deactivated, oneself, no
  such member); the confirmation names the directory's display name and says it deactivates.
  The tool server's refusals as U3 built them (`crates/toolserver/src/onboarding.rs`) each
  have a line: `403 protected_not_from_slack`, `429 deactivation_limit` (the card closes and
  points to `pmos-admin users deactivate`), `409 already_deactivated`, `409 last_admin`,
  `409 owns_bots`, `403 self_removal`/`self_change`, `403 not_admin`, `404 not_a_member`. A
  request from a deactivated Slack id is answered locally, and `409 deactivated` too.
- **Display names render inert** through `blocks::name` wherever the service shows one (the
  join card and its notification text, the handle modal, the welcome and first-time hello,
  the `remove` card): control characters become spaces, `&<>` are escaped, and the `mrkdwn`
  characters `*`, `_`, `~` and the backtick, and the auto-link characters `.`, `/`, `:`
  and `@`, become look-alike
  characters, so a name can't mention, link or format. The name reads the same; those
  characters are not the originals.
- **The App Home's skills catalog** still runs `pmos_contract::lint` over the content root,
  which reads `team.yaml` for its own team rules; only its `skills` list is used. That goes
  with the lint rules in U7.
- **Not verified:** the live Slack test stack (not run); a real Slack workspace's rendering
  of the look-alike characters.

U4a–U4d run in parallel (U4b after U3, since `remove` calls U3's deactivation). U5 and U6 can
overlap once the routes in U5 are merged.

**Proving no service decides from the file.** (1) An integration test per service starts it
with a content root holding a **hostile** `team.yaml` that lists `mallory` as `admin` with the
tester's Slack id and email, and a seeded directory without `mallory`: sign-in, Slack, a bot
ask, an admin read, a console item read and an approval decision as `mallory` are all refused,
and the real user is unaffected. (2) The same services start with no `team.yaml` at all. (3) A
workspace test fails if any crate other than the import/seed module names `team.yaml` or
`TeamFile`.

**Out of scope for the MVP:** Entra group or app-role sync; SCIM; JIT creation; self-service
join from the web; hard deletion and erasure; per-draft reviewer choice; recommendations from
Slack; a Slack DM to admins on every role change; step-up re-authentication for admin writes
(the assertion has no `auth_time`); more than one tool server replica; multiple Slack
workspaces or Entra tenants.

## 13. Open questions for the owner

The owner answered the first six (Q1 host only, Q2 assigned PMs, Q3 pilot exception, Q4 drop
`skill-author`, Q5 keep `role_changes` until 90 days after deactivation, Q6 metadata only);
they are folded in above. New ones from this revision, each with the default the build uses:

1. **The deactivation cap.** Default: **3 an hour** from web and Slack together, then host only.
   Higher is easier during a reorganization; lower limits a compromised web or bot token more.
2. **The grace before linked accounts are deleted.** Default: **24 hours**. Longer gives more
   time to undo a mistaken or malicious deactivation; shorter keeps a departed person's tokens
   (sealed and unusable) on disk for less time.
3. **Slack joiners wait for one web click before Microsoft sign-in.** Default: **yes** (§4).
   They can use Slack at once; only web sign-in waits for an admin to confirm their sign-in
   name.
