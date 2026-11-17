# Audit retention

The owner's decision (`CLAUDE.md`, f): audit events are **purged at 90 days, with no
archive**. One controlled job deletes older events and records its own run; nothing else can
delete an event, and that job cannot delete a newer one. This page says what is deleted, when,
what never is, how the database enforces it, how to check, and what an auditor sees.

## What is deleted, and when

- **What:** rows of the tool server's `audit_events` table whose `at` is older than the
  retention: `PMOS_AUDIT_RETENTION_DAYS` days, **90 by default and at minimum**. A larger value
  (up to 3650) keeps events longer. A value under 90 stops the tool server at start-up with a
  message saying so.
- **When:** the tool server runs the purge 5 to 10 minutes after it starts (a random delay, so
  replicas started together don't all try at once), then once a day. Each run deletes the
  oldest eligible events in batches of 10,000, at most 100 batches; anything left waits for the
  next day. The job stops when the tool server shuts down. Two replicas never purge at the
  same time: the second finds the lock held and skips that run.
- **Recorded:** each batch writes one audit event, in the same transaction as the delete:
  action `audit.purge`, actor `pmos-retention`, client `system`, outcome `ok`, detail
  `purged 1200 events older than 2026-08-19T03:07:12Z` (the count and the cutoff, nothing
  else). A day with nothing to delete still records `purged 0 events …`. A purge that isn't
  recorded didn't happen: if the insert fails, the delete is rolled back with it.

## What is never deleted

- **No event newer than 90 days**, whatever `PMOS_AUDIT_RETENTION_DAYS` says and whoever asks
  (see the next section).
- **Nothing in any other table.** The function names `audit_events` only: approval items, the
  feedback ledger, and the planned `role_changes` table of the user database
  (`docs/user-database-spec.md`) are never touched by it.
- **The JSON-lines copy** (`PMOS_AUDIT_LOG`, `$PMOS_STATE_DIR/audit/audit.jsonl` in `deploy/`)
  is not purged by PM OS; it gets a line for each purge batch. If it is shipped off-box, its
  retention is that system's. The console reads this file.
- **No UPDATE and no TRUNCATE**, ever, by anyone.

## How the database enforces it

Migration `crates/toolserver/migrations/0013_audit_retention.sql` (its comments explain each
piece):

1. **The append-only triggers stay.** `audit_events_append_only` (BEFORE UPDATE OR DELETE,
   per row; BEFORE TRUNCATE, per statement) raises for everything except one case.
2. **The one exception is narrow:** a row-level `DELETE` passes only if
   (a) the row is older than 90 days, checked by the trigger itself against `now()`, with the
   90 days written into the trigger;
   (b) the transaction-local setting `pmos.audit_purge` is `on`, which only the purge function
   sets, and clears before it returns; and
   (c) the DELETE runs inside `purge_audit_events(interval, integer)` (the trigger reads the
   call stack, `PG_CONTEXT`).
3. **`purge_audit_events(retention, batch_size)`** is `SECURITY DEFINER` with a fixed
   `search_path` (its schema, then `pg_temp`). It raises for a retention under 90 days or a
   batch outside 1 to 100,000; takes a transaction-scoped advisory lock (and returns
   `ran = false`, doing nothing, if another purge holds it); deletes at most `batch_size` of
   the oldest rows before its cutoff (`FOR UPDATE SKIP LOCKED`), the cutoff being the older of
   `now() - retention` and `now() - 90 days`; and inserts the `audit.purge` event.
4. **Who may call it:** `EXECUTE` is revoked from `PUBLIC`. In `deploy/`, `postgres-init.sh`
   gives each service its own database and login role, and no other role may connect to
   `pmos_toolserver`; `pmos_toolserver` owns the database, runs the migrations, and is the only
   role that can call the function.

**Why a newer event can't be deleted by anyone:** (a) is checked for every row by the trigger,
with a constant no caller supplies. A direct `DELETE` fails (no setting). Forging the setting
(`SET LOCAL pmos.audit_purge = 'on'`) fails (not inside the function). A function written to
look like the purge (same name, another schema) gets past (b) and (c) but not (a): its DELETE
of a newer row raises, and the whole statement and transaction fail. Calling the real function
with a short retention raises before anything is deleted. The tests in
`crates/toolserver/tests/audit_retention.rs` try each of these against Postgres.

**Limits.** The role that owns `audit_events` can change the schema (drop or disable the
triggers, replace the function): that is DDL, not a DELETE, and no trigger can stop the owner.
In `deploy/` that role is the tool server's own, because the tool server runs its own
migrations. Separating a migration owner from the runtime role (the runtime role then holding
only `INSERT`/`SELECT` on `audit_events` and `EXECUTE` on the function) would close it; it is
not done yet. A Postgres superuser (`postgres`, used by `pmos-admin backup` and `restore`) can
do anything. The purge's own event can't be forged by the application: the tool server refuses
to record a `system` event itself.

## How to verify

- **What the next run would delete:** `deploy/pmos-admin audit purge --dry-run` prints the
  count and the oldest such event, read-only.
- **That it ran:** the admin overview (`GET /api/admin/overview`) shows `retention_days`,
  `oldest_event_at` (at most about `retention_days` + 1 days old) and `last_purge` (when, and
  how many). The audit list filtered to action `audit.purge` shows every batch.
- **That the guarantees hold:** `cargo test -p pmos-toolserver --test audit_retention` with
  `PMOS_TEST_DATABASE_URL` set: an old event is deleted and a newer one never is; a direct
  DELETE, a forged setting, a look-alike function, an UPDATE, a TRUNCATE and a short retention
  are all refused; each run is recorded; runs are idempotent and bounded; two at once neither
  double-delete nor deadlock.
- **By hand, in Postgres:** `DELETE FROM audit_events WHERE at > now() - interval '1 day'`
  fails with `audit_events is append-only`, as does `SELECT * FROM
  purge_audit_events(interval '30 days', 10)` with `audit retention must be at least 90 days`.

## What an auditor sees

The auditor's view is metadata only, as for every event: in the audit list a purge reads as
action `audit.purge`, actor `pmos-retention`, client `system`, outcome `ok`, and its detail
(the count and the cutoff). The overview shows the retention, the oldest event kept and the
last purge. Events older than the retention are gone for good: there is no archive to ask for.
