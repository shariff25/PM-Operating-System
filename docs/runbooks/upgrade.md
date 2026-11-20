# Upgrade PM OS

A new version is a merge to `main` in the PM OS repo.

**Upgrades go through the benchmarks:** a change to a model, the harness or a skill doesn't
reach PMs until the affected category still meets its bar (D10).

1. **Back up:** `deploy/pmos-admin backup`.
2. **Get the new code:**
   ```sh
   cd /opt/pmos
   git fetch && git log --oneline HEAD..origin/main     # read what changed
   git pull --ff-only
   ```
   Read `CHANGELOG.md`, and any new variables in `deploy/env.example` or
   `deploy/live.env.example`. Add new settings to `deploy/.env` by hand, or run
   `deploy/setup.sh` again, which only fills in missing values.
3. **Build:** `deploy/pmos-admin build`.
4. **Restart with the new images:**
   ```sh
   deploy/pmos-admin up --models
   deploy/pmos-admin status
   ```
   The services run their own database migrations at start-up. Each service stops on SIGTERM
   only after finishing in-flight work, and a running task is retried once.
5. **If the worker image changed,** recreate each bot's sandbox so it runs the new one:
   ```sh
   openshell sandbox delete pmos-<bot>
   deploy/pmos-admin sandbox <bot>
   ```
6. **If a role's skills or tools changed** (the changelog says so), regenerate the bots'
   sandbox policies in your content repo, commit the result, then recreate each bot's
   sandbox as in step 5. A policy lists the tools its bot may call, so a bot keeps its old
   list until this is done:
   ```sh
   deploy/pmos-admin pmos policy generate --root content --out policies
   deploy/pmos-admin pmos policy check
   ```
7. **If the Slack app manifest changed** (`docs/slack-app-manifest.yaml`), update the app
   and reinstall it; see below.
8. **Check:** run `deploy/pmos-admin smoke <your handle>`, then the benchmarks for any category
   the change touches (`deploy/pmos-admin evals-run --category <c>`).

## The tool server validates first Microsoft sign-ins (review of `b469d5b`)

From this version the tool server checks the ID token of a person's **first** Microsoft
sign-in itself, instead of taking the web app's word for who it is
([../entra-sign-in.md](../entra-sign-in.md)). It needs the tenant id and client id:

- **With the deploy kit, nothing to add.** `deploy/compose.yaml` passes the web app's
  `PMOS_WEB_ENTRA_TENANT_ID`, `PMOS_WEB_ENTRA_CLIENT_ID` and `PMOS_WEB_ENTRA_AUTHORITY` to the
  tool server as `PMOS_ENTRA_TENANT_ID`, `PMOS_ENTRA_CLIENT_ID` and `PMOS_ENTRA_AUTHORITY`
  (never the client secret). The tool server now fetches the tenant's keys from
  `login.microsoftonline.com` itself, through the same outbound proxy as the web app.
- **Running the services another way:** set those three on the tool server, with the same
  values as the web app's. Without them a first Microsoft sign-in is refused
  (`reason=users_unavailable`) and the tool server logs it; people already linked keep signing
  in.
- **Check:** `deploy/pmos-admin status` shows the web app's `/readyz`; with Entra on it must
  include `"entra_link":"ok"`. `down` means the tool server has no Entra settings, or other ids
  than the web app (its log says which).

The same version also (all automatic, migration `0021`): only the Slack bot's token may decide
a join request or remove a member (an orchestrator script that did either now gets `403`);
`resolve` answers at most 600 hits a minute per client; the audit and role-history purges run
with a fixed search path and their triggers accept only the real function; and a user's old
sign-in name, Atlassian email or GitHub login can no longer be given to someone else. A
`PMOS_GITHUB_URL` (or any other connector base override) that is `http://` to anything but
loopback now stops the tool server at start-up: use `https://`.

## Upgrading to the user database (`team.yaml` retired)

From this version **no service reads `content/team.yaml`**. Who may use PM OS, and their
roles, come only from the tool server's user database ([users.md](users.md)): sign-in, Slack,
asking bots, the console, the admin screens and approval decisions. Onboarding no longer
writes the file, `pmos member add` is gone, and `pmos lint` fails while a `team.yaml` is in the
content (`team-file-retired`). **Import the people in it before you start the new version.**

1. Back up (step 1 above), get the new code and build it (steps 2 and 3), but don't run `up`
   yet.
2. Note the content commit and keep a copy of the file outside the content (the pull request
   in step 5 deletes it):
   ```sh
   sha=$(git -C "$PMOS_CONTENT_CHECKOUT" rev-parse HEAD)
   git -C "$PMOS_CONTENT_CHECKOUT" show "$sha:content/team.yaml" > ~/team.yaml.$sha
   ```
3. Import it with the new image (the one-off container applies the tool server's migrations
   first). Read the dry run, then apply; admins come in with `--allow-admins`, or run
   `bootstrap-admin` first ([users.md](users.md), "Importing team.yaml"):
   ```sh
   deploy/pmos-admin users import --from ~/team.yaml.$sha --commit "$sha"
   deploy/pmos-admin users import --from ~/team.yaml.$sha --commit "$sha" --allow-admins --apply
   deploy/pmos-admin users list
   ```
   Then `users set-entra <handle> --oid <object id>` for each Admin and Auditor. On an install
   that already imported (any release since slice U3), the import is safe to run again: everyone
   is `unchanged`, and anyone added to the file since then is created.
4. Start the new version (step 4 above) and check (step 8).
5. Open a pull request on the content repo that deletes `content/team.yaml` (and its
   `CODEOWNERS` line, if you have one). Until it merges, CI's `pmos lint` fails on the file;
   the services only log a warning and ignore it.

**If you start the new version without importing,** nobody gets in, and the tool server says
so at start-up (`deploy/pmos-admin logs toolserver`): "the user database is empty: nobody can
use PM OS until `pmos-admin users import` (or bootstrap-admin) runs". The web app answers every sign-in with "not a member",
Slack offers everyone **Request access**, and the runtime refuses every ask. Nothing is lost:
run step 3; the services pick the users up within 30 seconds, without a restart.

**Rolling back past this version:** the previous version's Slack service, runtime, web app and
console may still read `team.yaml`. Write the current database back as one before you roll
back: `deploy/pmos-admin users export --format team-yaml > team.yaml`, commit it to the content
repo, then roll back as below.

## Upgrading to user assertions on by default

From this version the runtime and the tool server run in `require` whenever the web app's or
the console's tokens are set, and refuse to start in `off` or `verify` (`docs/user-assertion.md`,
Modes). On an existing install:

1. Run `deploy/setup.sh` again (same arguments). It writes `PMOS_ASSERTION_MODE=require` into
   `deploy/.env` if the line is blank, and `PMOS_ONBOARDING_V2=1` and `PMOS_NUDGES=1`.
2. If `deploy/.env` says `PMOS_ASSERTION_MODE=off` or `verify`, change it to `require`.
   `pmos-admin up`, `add-bot` and `restore` now refuse to go on otherwise, and say so.
3. `deploy/pmos-admin up --models` (with `--console` and `--web` if you run them). The mode now
   comes from `deploy/.env` on every path, so an `add-bot` or the `up` after a `restore` no
   longer starts the runtime with assertions off while the web app keeps running.

## Tool-server migrations renumbered (web-app branch databases only)

Which databases: only a tool-server database (`pmos_toolserver`) that was migrated by the
`claude/web-app` branch **before** it merged `main`'s security fixes (commit 5bb6de4). That
branch numbered its migrations `0009_audit_source_titles` and `0010_audit_admin_reads`; after
the merge they are `0010` and `0011`, and `main`'s `0009_note_sensitivity` is new. Such a
database fails at start-up with sqlx's "migration 9 was previously applied but has been
modified". Only dev and test stacks of that branch can have it: the Spark runs `main`, and
`scripts/claude-test-stack.sh` keeps everything in memory. Check with:

```sh
docker compose -f deploy/compose.yaml --env-file deploy/.env exec -T postgres \
  psql -U postgres -d pmos_toolserver -c "SELECT version, description FROM _sqlx_migrations ORDER BY version"
```

Affected when version 9 reads `audit source titles`.

**Reset (verified path; loses the tool server's data: approvals, sessions, linked accounts,
audit):** a dev or test stack holds fake data only.

```sh
deploy/pmos-admin down
docker compose -f deploy/compose.yaml --env-file deploy/.env up -d postgres
docker compose -f deploy/compose.yaml --env-file deploy/.env exec -T postgres psql -U postgres \
  -v ON_ERROR_STOP=1 -c "DROP DATABASE pmos_toolserver WITH (FORCE)" \
  -c "CREATE DATABASE pmos_toolserver OWNER pmos_toolserver"
deploy/pmos-admin up
```

The tool server makes its tables again at start-up. For a local `cargo run` with its own
database, drop and recreate that database the same way.

**Repair (keeps the data). Dev and test stacks of that branch only; prefer the reset above.**
Never edit `_sqlx_migrations` by hand. The repair is one guarded script,
`scripts/repair-toolserver-migration-numbers.sql`. In one transaction, with the table locked,
it checks that the database is in exactly the affected state: version 9 is
`audit source titles` and version 10 `audit admin reads`, both applied, each with the
checksum of its file, and nothing after 10. Only then does it renumber them to 10 and 11 (the
SQL of both files is unchanged, so their checksums stay). In any other state (already
repaired, never affected, a different history) it changes nothing, rolls back and exits
non-zero with what it found. Take a backup first ([backup-restore.md](backup-restore.md)), then:

```sh
deploy/pmos-admin down
docker compose -f deploy/compose.yaml --env-file deploy/.env up -d postgres
docker compose -f deploy/compose.yaml --env-file deploy/.env exec -T postgres \
  psql -U postgres -d pmos_toolserver -v ON_ERROR_STOP=1 \
  -f - <scripts/repair-toolserver-migration-numbers.sql
deploy/pmos-admin up
```

`pmos-admin up` starts the tool server, which applies `0009_note_sensitivity` (it only adds a
nullable column) and anything newer. Verified on 2026-11-17 against a scratch Postgres 16
database: the branch's old migrations applied with sqlx, the current tool server refusing them
("migration 9 was previously applied but has been modified"), the script refusing a tampered
row and a second run, and after the repair the current migrations applying cleanly.

## Updating the Slack app

A new scope or event in `docs/slack-app-manifest.yaml` does nothing until the app in Slack has
it. **Tagging and the App Home** (`docs/slack-home.md`) need:

| What | Value |
| --- | --- |
| Bot scope | `app_mentions:read` |
| Bot events | `app_mention`, `app_home_opened` |
| App Home | the Home tab on |

1. **Update the app from the manifest.** At <https://api.slack.com/apps>, open the app, go to
   **App Manifest**, paste the new `docs/slack-app-manifest.yaml` and save.
2. **Reinstall it** (**Install App**, then **Reinstall to Workspace**). A workspace admin may
   need to approve the new scope. The bot token (`PMOS_SLACK_BOT_TOKEN`) usually stays the
   same; if Slack shows a new one, put it in `deploy/.env` and run `deploy/pmos-admin up`.
3. **Check:** open the app's **Home** tab in Slack, and tag `@PM OS` in a thread in a channel
   the app is in. Without the update, tags are ignored and the Home tab is missing.

PMs don't need to link Slack again: their own tokens' scopes are unchanged.

## Rolling back

```sh
git checkout <the previous commit>
deploy/pmos-admin build && deploy/pmos-admin up --models
```

If the new version changed a database (check the release notes, or new files under
`crates/*/migrations/`), restore the backup from step 1 as well
([backup-restore.md](backup-restore.md)). Migrations are not reversible.

## Registering the Spark CI runner

The eval gate (`.github/workflows/evals-gate.yml`, `docs/evals.md` "CI") runs on pull requests
that change roles, skills, bots, the evals or `deploy/gateway.yaml`. It needs the stack and the
models, so it runs on a GitHub Actions self-hosted runner on the Spark with the labels
`self-hosted` and `pmos-spark`. Until one is registered the job waits in the queue; a platform
owner checks out the PR's branch somewhere on the Spark (not in `/opt/pmos`), runs
`deploy/pmos-admin evals-gate --checkout <that checkout> --base origin/main` from `/opt/pmos`,
and pastes the output into the PR.

**What the runner runs.** The workflow runs on `pull_request_target`, so its own file always
comes from the base branch. It checks out the PR's head into `pr/` without credentials and
runs the Spark's installed script, `/opt/pmos/deploy/pmos-admin evals-gate --checkout pr`. The
PR is only read as data (its `content/`, `evals/` and `deploy/gateway.yaml`); nothing in it is
run, and no secret file is linked into it. So:
- **A change to `pmos-admin` or the eval stack's compose file takes effect only after an
  upgrade on the Spark** (the steps above). A PR that changes them is measured with the
  Spark's current copies.
- **The images are the Spark's,** built by the last `deploy/pmos-admin build`. A PR's code
  changes are not benchmarked by the gate until they are merged and the Spark is upgraded.

**Once, to register it** (a GitHub repo admin and a Spark platform owner together):
1. **On GitHub:** the repo's Settings → Actions → Runners → New self-hosted runner, Linux,
   ARM64. Keep the page open: it shows the download commands and a registration token that
   lasts an hour.
2. **On the Spark, a user for the runner** that can run Docker (`pmos-admin` needs it) and
   read the stack's settings. Docker access is root-equivalent, which is why only branches of
   this repository, never forks, run there.
   ```sh
   sudo useradd --create-home --groups docker gh-runner
   sudo setfacl -m u:gh-runner:r /opt/pmos/deploy/.env
   ```
   If PM OS isn't in `/opt/pmos`, set `PMOS_SPARK_DIR=<that path>` in the runner's `.env`
   file (in its install directory) after step 3; the workflow runs that copy's
   `deploy/pmos-admin`, which reads its own `deploy/.env`.
3. **Install and configure it** as that user, with the commands from step 1 and these
   options:
   ```sh
   sudo -iu gh-runner
   mkdir actions-runner && cd actions-runner
   # download and unpack, as the GitHub page shows
   ./config.sh --url https://github.com/<org>/<repo> --token <token from the page> \
     --name spark-1 --labels pmos-spark --unattended
   exit
   ```
   GitHub adds `self-hosted`, `Linux` and `ARM64` itself.
4. **Run it as a service,** so it survives reboots:
   ```sh
   cd ~gh-runner/actions-runner
   sudo ./svc.sh install gh-runner && sudo ./svc.sh start
   ```
5. **Restrict who can use it,** on GitHub:
   - **The runner serves this repository only.** Register it on the repository, as in step 1,
     not on the organization. If it must be an organization runner, put it in a runner group
     whose repository access is this repository only (organization Settings → Actions →
     Runner groups).
   - **Outside collaborators need an approval.** The repo's Settings → Actions → General →
     "Approval for running fork pull request workflows from contributors": choose "Require
     approval for all outside collaborators".
6. **Check:** Settings → Actions → Runners shows `spark-1` as Idle. Re-run a waiting "Eval
   gate" job, and read its summary.

**Day to day:** the runner updates itself. One job runs at a time, and a new push to the PR
cancels the previous run. If the Spark is busy or down, the job waits; `sudo ./svc.sh status`
in the runner directory shows whether the service is up.

**To remove it:** stop the service (`sudo ./svc.sh stop && sudo ./svc.sh uninstall`), then
`./config.sh remove --token <a removal token from the Runners page>`.
