# Runbook: the user database

PM OS is moving who-may-use-it from `content/team.yaml` to the tool server's own database
(`docs/user-database-spec.md`). **Since slice U3 the tool server decides from the database:**
who may have a session, decide a draft, link an account, read the admin screens, approve a
join or deactivate someone. The runtime (U4a), Slack (U4b) and the web app's sign-in (U4c)
read the database too; the console and the web app's Team page still read `team.yaml` until
slices U4d and U5, so **keep both equal**: a join approved in Slack writes both;
anything you add by hand to `team.yaml`, add here too (`import` picks up new people).

**Before deploying a release with U3, import.** A tool server with an empty user database lets
nobody in (it logs `the user database is empty` at start-up). On an install that already has
people in `team.yaml`: deploy, run the import below (with `--allow-admins`, or
`bootstrap-admin` first), then everything works as before.

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

## Deactivating someone, and undoing it

Slack's `remove <handle>` (and, from slice U5, the web app's Team page) deactivates; it never
deletes and commits nothing. In one step: their sessions end, `admin` is taken away if they
had it, and their linked Jira, Confluence, Slack and GitHub accounts are **suspended** (PM OS
won't use them). Their pending drafts are **held**. Then:

- **Reactivate within 24 hours** (`deploy/pmos-admin users reactivate <handle>`): the accounts
  work again and the held drafts can be decided. `admin` doesn't come back: `grant-admin` it
  on purpose.
- **After 24 hours** the tool server deletes the accounts and closes the drafts
  (`owner_deactivated`, nothing is posted). A reactivation then brings the person back, but
  they link their accounts again.
- Their personal bot's sandbox keeps idling until `deploy/pmos-admin pause pm-<handle>`.

**The hourly limit.** The web app and Slack together may deactivate at most 3 people in any
hour (`PMOS_USERS_DEACTIVATIONS_PER_HOUR`, 1 to 10, in `deploy/.env`). The fourth answers
"PM OS has stopped deactivations for an hour": if that is a real need (a reorganization, a
compromised account), continue on the host with `deploy/pmos-admin users deactivate`, which
isn't limited. A sudden run of deactivations you didn't expect is a sign the web app or the
Slack bot token is compromised: check the overview and the audit log (`users.deactivate`).
Slack can never deactivate an Admin or Auditor.

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
