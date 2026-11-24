# Backup and restore

## What a backup holds

`deploy/pmos-admin backup` writes `deploy/backups/<time>/`, containing:
- **the three databases:**
  - tool server: approvals, sealed tokens, memory records, member requests and audit;
  - bot runtime: tasks, routines, traces and notifications;
  - docs index;
- **the audit and usage logs;**
- **`SHA256SUMS`**, which the restore checks before changing anything.

**Not in it:**
- **`deploy/.env`**, which is in the team password manager. Without its `PMOS_VAULT_KEY`, the
  backup's sealed tokens can't be read, and every PM would have to link their accounts again.
- **The content**, which is in git.
- **Each PM's memory**, which is in their own GitHub repo.

## Backups

Run one at least weekly, and before every upgrade:

```sh
cd /opt/pmos
deploy/pmos-admin backup
```

**Copy it off the host,** to the team's backup storage. A backup that only lives on the host
doesn't survive the host. To run it every Sunday night, add this to the platform user's
crontab (`crontab -e`):

```
15 2 * * 0  cd /opt/pmos && deploy/pmos-admin backup >> /srv/pmos/state/backup.log 2>&1
```

**Delete old backups** from `deploy/backups/` once they're copied; keep the last 4 on the host.

## Restore

Use this when a database is lost or damaged, when moving to a new host, or for the Gate 1 test.
It replaces all three databases. Anything newer than the backup is lost: tasks, approvals
decided since, and links PMs made since.

1. **On a new machine,** first do stage 1 of [install.md](install.md). Put the saved
   `deploy/.env` back before running `setup.sh`; it keeps the values already there. Then run
   `deploy/pmos-admin build`.
2. **Copy the backup** to `deploy/backups/` on the host, if it isn't there already.
3. **Restore it:**
   ```sh
   deploy/pmos-admin restore deploy/backups/<time>
   ```
   Type `RESTORE` when asked. The services are stopped while it runs; Postgres stays up.
   Before it stops anything it checks the user-assertion settings in `deploy/.env` (as `up`
   and `add-bot` do), so the `up` below starts the runtime and the tool server in `require`
   (`docs/user-assertion.md`, Modes).
4. **Start the stack again and check it:**
   ```sh
   deploy/pmos-admin up --models
   deploy/pmos-admin status
   deploy/pmos-admin smoke <your handle>
   ```
5. **Recreate the bot sandboxes,** if this is a new machine: stage 4 of
   [install.md](install.md).

**If the restore stops with "checksums don't match":** the backup was damaged in copying.
Nothing was changed. Copy it again, or use an older one.
