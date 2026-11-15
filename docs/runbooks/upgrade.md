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
