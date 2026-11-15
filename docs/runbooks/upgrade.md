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
6. **Check:** run `deploy/pmos-admin smoke <your handle>`, then the benchmarks for any category
   the change touches (`deploy/pmos-admin evals run --category <c> --as <you>`).

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
owner runs `deploy/pmos-admin evals-gate --base origin/main` on the Spark from the PR's branch
and pastes the output into the PR.

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
   file (in its install directory) after step 3; the workflow links `deploy/.env` from there.
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
5. **Check:** Settings → Actions → Runners shows `spark-1` as Idle. Re-run a waiting "Eval
   gate" job, and read its summary.

**Day to day:** the runner updates itself. One job runs at a time, and a new push to the PR
cancels the previous run. If the Spark is busy or down, the job waits; `sudo ./svc.sh status`
in the runner directory shows whether the service is up.

**To remove it:** stop the service (`sudo ./svc.sh stop && sudo ./svc.sh uninstall`), then
`./config.sh remove --token <a removal token from the Runners page>`.
