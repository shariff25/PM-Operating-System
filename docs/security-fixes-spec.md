# Security fixes, batch 1: build spec

From the security and code-practices review of 2026-11-15. This batch fixes what must be
closed before the Spark's CI runner is registered or any real PM token is used. Finding
numbers (S1, S2, …) are the review's.

**Two principles behind every fix here:**
- **One token should not be able to act as everyone.** Each client gets only what it uses.
- **Untrusted input stays data.** A pull request, a model's output and text from Jira,
  Confluence or Slack never choose what runs or who sees it.

## S1. The eval gate must not run a pull request's code beside production secrets

**Today:** `evals-gate.yml` runs on `pull_request`, checks out the PR, links the Spark's
`deploy/.env` into it and runs the PR's own `deploy/pmos-admin`.

**Fix:**
- **Trigger:** `pull_request_target`, so the workflow file always comes from the base branch.
  Keep the `paths` filter, `permissions: contents: read`, the concurrency group and the
  fork guard.
- **Checkout:** the PR's head (`github.event.pull_request.head.sha`) into `pr/`, with
  `persist-credentials: false` and `fetch-depth: 0`. Nothing in `pr/` is executed.
- **Run the Spark's own script:**
  `"${PMOS_SPARK_DIR:-/opt/pmos}/deploy/pmos-admin" evals-gate --checkout "$GITHUB_WORKSPACE/pr" --base "origin/${BASE_REF}"`.
  Remove the step that links `deploy/.env` into the checkout.
- **`pmos-admin evals-gate` gains `--checkout <dir>`** (default: the script's own repo). It
  diffs, archives the baseline and starts the eval stack from that directory. From a
  checkout it reads only `content/`, `evals/` and `deploy/gateway.yaml`, as data. It never
  sources or executes a file from it.
- **Runbook** (`docs/runbooks/upgrade.md`, "Registering the Spark CI runner"): the gate runs
  the Spark's installed `pmos-admin`, so a change to the script takes effect only after an
  upgrade on the Spark. Add: restrict the runner to this repository, and set Actions to
  require approval for all outside collaborators.

## S2. Less power in the Slack bot's token

The Slack bot is the approval screen, so the tool server has to trust which PM it names;
Socket Mode gives the tool server nothing signed to check. The fix is to shrink what that
trust allows.

- **No admin grants.** A member decision from the bot client may grant `pm` and
  `skill-author` only. `admin` is refused (`403`); a new admin is made by a pull request to
  `team.yaml`.
- **An edit can't change where a draft goes.** For a decision of `edit` from the bot
  client, these arguments must equal the drafted ones, else `422`:
  `slack.post_reply` and `slack.send_message`: `channel`, `thread_ts`, `message_ts`;
  `confluence.create_draft`: `space`, `parent_id`; `confluence.update_page`: `page_id`,
  `version`; `jira.create_issue`: `project`, `issue_type`, `parent`; `bets.record`: `key`.
  (Use the argument names the schemas really have. The Slack edit window must not offer
  these fields; check `editable_fields` in `crates/slack/src/cards.rs`.) The orchestrator
  client is not limited.
- **Reads name their PM.** For the bot client, `GET /internal/approvals/{id}` needs
  `?user=<handle>` and answers `404` unless the item belongs to that member. The Slack
  client always passes the PM it is acting for.
- **The ledger hides approval ids from the bot client.** `GET /internal/ledger` omits `id`
  for that client. The triage digest uses `run_id`.

## S3. The benchmark client runs tasks only as the benchmark member

- **Runtime setting `PMOS_EVALS_AS`:** a comma-separated list of handles, default `evals`.
  A task from the `evals` client must have `requested_by` in that list, else `403`.
- **No Slack target:** a task with `source: eval` and a `slack` field is refused (`400`).
- **No Slack delivery:** the runtime creates no notifications for `eval` tasks.
- **Deploy:** `PMOS_EVALS_AS` in `deploy/env.example` (empty means the default) and passed
  to the runtime in both compose files. `pmos-admin evals` and `smoke` keep working: check
  which handle each uses and document it.

## S4. The console gets its own read-only token

- **Tool server:** a third client, `console`, with the token `PMOS_CONSOLE_TS_TOKEN`
  (optional; at least 32 characters when set). It may call `GET /internal/approvals` and
  `GET /internal/approvals/{id}` only; everything else is `403`.
- **Console:** `PMOS_TOOLSERVER_TOKEN` is given that token. `deploy/compose.yaml` stops
  passing `PMOS_SERVICE_TOKEN` to the console; `setup.sh` generates the new secret.

## S5. The console checks that a forwarded identity came from the proxy

- **`PMOS_CONSOLE_PROXY_SECRET`:** with `PMOS_CONSOLE_TRUST_PROXY=1` the console refuses to
  start without it (at least 32 characters). Every request must carry the header
  `X-Pmos-Proxy-Secret` equal to it (compared in constant time), else `401`.
  `X-Forwarded-User` is read only after that check.
- **Origin:** a state-changing request must carry `Origin` or `Referer` from the console's
  own origin; one with neither is refused.
- **Docs:** the install runbook's sign-in proxy section adds the header.

## S6. A note carries the sensitivity of what it was built from

- **`Note` gains `sensitivity`** (`public`, `internal` or `restricted`).
  - `memory.update` stores the writing session's highest sensitivity read so far.
  - `bets.record` stores the approval item's `sources_sensitivity`.
  - A note with none (older notes) reads as `internal`.
- **Reading raises the session to it.** `memory.read` raises to the highest among the notes
  returned. `bets.read` raises to that bet's; `bets.list` to the highest among the bets
  listed. Never below `internal`, as today.
- **Storage:** a nullable column on the Postgres notes table (the next migration), a field
  on the memory store, and `sensitivity:` in the front matter of a GitHub note.
- **Effect to document:** once a PM's memory holds a restricted note, a reply drafted in a
  session that read it is blocked in channels with people outside the team.

## S7. A Confluence update fails if the page changed since it was read

- **`confluence.read` returns the page's `version`** number in its content.
- **`confluence.update_page` requires `version`** (integer, 1 or more): the version the bot
  read. If the page's current version differs, the write is refused before any request that
  changes the page, with "the page changed since PM OS read it; read it again". Otherwise it
  sends `version + 1`, and a `409` from Confluence maps to the same message.
- The fake connector keeps a version per page and behaves the same way.

## Smaller fixes

- **S10:** `bets.read` and `bets.list` results are wrapped as untrusted, like connector
  results. `memory.update` refuses keys that start with `bet-`.
- **S11:** `stats.sample_size` refuses a result that isn't finite or exceeds 10^12 per arm,
  and multiplies with overflow checks.
- **S12:** the memory tools are refused for team bots, at the call and (for
  `memory.update`) wherever writes are checked, as the bets tools already are.
- **S13:** length limits on write arguments: Slack `text` 40,000; Confluence `body`
  200,000 and `title` 255; Jira `summary` 255 and `description` 32,000; `version_comment`
  500. The edit-distance sum runs on at most 10,000 characters of each text and outside the
  database transaction.
- **S14:** `Session` and `BotAccess` no longer print their tokens in debug output.
- **S17:** `.dockerignore` excludes `deploy/*.env`.
- **S24:** an internal failure of an approved write returns a generic message; the detail
  goes to the log.

## Tests

Each fix gets a test that fails without it:
- **S1:**
  - `evals-gate --checkout` uses the named directory, checked with `bash -n` and a dry
    argument parse;
  - a script test or a review check that nothing under `pr/` is sourced or executed.
- **S2:**
  - an `admin` grant from the bot client is refused;
  - an edit that changes a target is refused, and a text-only edit is accepted;
  - a read without `user`, or with the wrong one, is `404`;
  - the ledger has no `id` for the bot client and still has it for the orchestrator.
- **S3:**
  - another handle is refused;
  - a `slack` target on an eval task is refused;
  - an eval task produces no notification.
- **S4:** the console token reads approvals and is refused everywhere else.
- **S5:**
  - a start-up without the secret is refused;
  - a request without it, or with a wrong one, is refused;
  - a POST with neither `Origin` nor `Referer` is refused.
- **S6**, on every memory backend:
  - a restricted read followed by a write stores `restricted`;
  - a later session that reads the note is raised to `restricted`, and its Slack draft is
    blocked where outsiders are present;
  - an old note reads as `internal`.
- **S7:**
  - a stale version is refused with no write;
  - the right version writes `version + 1`;
  - a `409` maps to the message.
- **The smaller fixes:** one test each.

## Docs

`docs/toolserver.md`, `docs/bot-runtime-spec.md`, `docs/slack-home.md` (the ledger and reads),
the install and upgrade runbooks, `deploy/env.example`, the changelog ("Security fixes from
the November review"), and the "Review follow-ups" list in `docs/implementation-plan.md`:
mark these fixed and add the review's remaining findings (S8, S9, S15, S16, S18 to S23, and
the code-practices items P1 to P12) so they stay tracked.
