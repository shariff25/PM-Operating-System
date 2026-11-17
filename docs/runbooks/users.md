# Runbook: the user database

PM OS is moving who-may-use-it from `content/team.yaml` to the tool server's own database
(`docs/user-database-spec.md`). **Today (slice U1) nothing reads the database for a decision:**
every service still reads `team.yaml`. Filling the database now is safe and is the first step
of the cut-over.

Everything here runs on the Spark through `deploy/pmos-admin users …`. Each command runs in a
one-off tool server container with the database's own access, and records **the OS user who
ran it** (your login, or the user behind `sudo`) with every change. Nothing here prints a
token.

| Exit code | Means |
| --- | --- |
| 0 | Done |
| 1 | Failed (the database, a file) |
| 2 | Wrong arguments, or an OS user name PM OS can't record (use a named login, not a shared one) |
| 3 | Refused by a rule; the message says which (`admin_exists`, `last_admin`, `dual_role`, …) |

## The first admin

Once, on a fresh install, before anyone else:

```sh
deploy/pmos-admin users bootstrap-admin --handle raj --name "Raj" \
  --sign-in-name raj@contoso.com --atlassian-email raj@contoso.com \
  --slack-id U0RAJ --oid <their Entra object id>
```

Find the object id in the Entra admin center: Users, the person, **Object ID**. Without Entra
sign-in set up yet, use `--no-entra` instead of `--oid`, and run `set-entra` later. It is
refused (exit 3) once any active admin exists; after that, `grant-admin` makes admins.

## Importing team.yaml

Read the file at a reviewed commit, and always look at the dry run first:

```sh
git -C "$PMOS_CONTENT_CHECKOUT" rev-parse HEAD          # the commit to record
deploy/pmos-admin users import --from "$PMOS_CONTENT_CHECKOUT/content/team.yaml" --commit <sha>
```

The dry run lists every person as `create`, `unchanged` or `conflict`, with the addresses it
would store. Then add `--apply`. What it does:

- **Idempotent.** Run it again and everyone is `unchanged`. It never removes anyone and never
  revokes a role.
- **Conflicts are reported, never overwritten:** the database wins. Correct either side.
- **`email` goes to both** the Microsoft sign-in name and the Atlassian email. If they differ for
  someone, fix it afterwards: `users set-sign-in-name <handle> <name>` or
  `users set-atlassian-email <handle> <email>`.
- **`skill-author`** is dropped; the person keeps `pm`. `entra-oid` is ignored: the host sets
  Entra links (below).
- **It stops before writing anything** on an unknown role, a key listed twice, a `bots.*`
  handle, an `evals` entry with a role other than `pm`, or someone holding admin and auditor
  while no pilot exception is on.
- **Admins** are not imported unless you add `--allow-admins`, which lists them and asks you to
  type `ADMINS`. Two ways to start, either is fine:
  1. `bootstrap-admin` for yourself, then `import` without `--allow-admins`, then
     `grant-admin` for each other admin; or
  2. `import --allow-admins --apply` directly.
  An import that would leave people but no active admin is refused with that reason.
- The file's SHA-256 and the commit you named are recorded.

`--bootstrap` is for a test stack's empty database (CI): it takes the file's admins without
asking, all in one transaction, and is refused (exit 3) if the users table has any row at all,
deactivated people included.

Then, for **every Admin and Auditor**, link their Microsoft account on the host (imported admins
and auditors have none):

```sh
deploy/pmos-admin users set-entra <handle> --oid <their object id>
```

`--oid` uses the tenant in `PMOS_WEB_ENTRA_TENANT_ID` from `deploy/.env`.

## Day to day

```sh
deploy/pmos-admin users list                                  # who, status, roles, flags
deploy/pmos-admin users grant-admin ana --oid <object id>     # the only way to make an admin
deploy/pmos-admin users deactivate ben                        # not capped; revokes admin
deploy/pmos-admin users reactivate ben                        # never restores admin
deploy/pmos-admin users export --format team-yaml > team.yaml # before a rollback
```

`grant-admin --keep-entra` reuses an Entra link the host already confirmed. A deactivated
admin who comes back is reactivated without `admin`; grant it again on purpose.

## The pilot exception: one person as Admin and Auditor

Normally refused. For the pilot only, and for at most 90 days:

```sh
deploy/pmos-admin users pilot-exception admin-and-auditor on --until 2027-02-01
deploy/pmos-admin users pilot-exception admin-and-auditor status
deploy/pmos-admin users pilot-exception admin-and-auditor off --revoke-auditor
```

`--until` is a date (it ends at 00:00 UTC that day). While it is on, granting `auditor` to an
admin works (from the web app in a later release). `off` is refused while anyone holds both,
naming them; `--revoke-auditor` takes `auditor` away from each in the same step and keeps
`admin`. If the date passes and nobody ran `off`, the exception counts as off: changes to
that person are refused until one of the two roles is gone. Revoking either role,
deactivating them and `off --revoke-auditor` still work, and PM OS never lets this leave it
without a working admin.

## Break glass: the web app is down or an admin is locked out

The host commands don't need the web app, the console or Slack, and they work whatever the
assertion mode in `deploy/.env`:

```sh
deploy/pmos-admin users grant-admin <handle> --oid <object id>   # make someone admin
deploy/pmos-admin users revoke-admin <handle>                    # take admin away
deploy/pmos-admin users unset-entra <handle>                     # a wrong Microsoft link
```

PM OS always keeps at least one active admin: the last one can't be demoted or deactivated by
anyone, from anywhere, and the database itself enforces it. To replace the last admin,
`grant-admin` the new one first, then `revoke-admin` the old one.
