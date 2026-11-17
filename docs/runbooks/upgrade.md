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

**Repair (keeps the data; not exercised in CI, so take a backup first):** move the two
applied rows to their new numbers with their new checksums, then start the tool server, which
applies `0009_note_sensitivity` (it only adds a nullable column):

```sh
c10=$(sha384sum crates/toolserver/migrations/0010_audit_source_titles.sql | cut -d' ' -f1)
c11=$(sha384sum crates/toolserver/migrations/0011_audit_admin_reads.sql | cut -d' ' -f1)
docker compose -f deploy/compose.yaml --env-file deploy/.env exec -T postgres \
  psql -U postgres -d pmos_toolserver -v ON_ERROR_STOP=1 -1 \
  -c "UPDATE _sqlx_migrations SET version = 11, description = 'audit admin reads', checksum = decode('$c11', 'hex') WHERE version = 10" \
  -c "UPDATE _sqlx_migrations SET version = 10, description = 'audit source titles', checksum = decode('$c10', 'hex') WHERE version = 9"
deploy/pmos-admin up
```

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
