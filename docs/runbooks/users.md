# Runbook: the user database

Who may use PM OS, and their roles, live in the tool server's own database
(`docs/user-database-spec.md`), and nowhere else. The tool server decides from it who may have
a session, decide a draft, link an account, read the admin screens, approve a join or
deactivate someone; the runtime, Slack, the web app and the console ask it. **`team.yaml` is
retired** (slice U7): no service reads one, onboarding commits only the new person's personal
bot, and `pmos lint` fails while a `team.yaml` is anywhere in the content
(`team-file-retired`). People are added in the web app (an admin's invite), through Slack's
join requests, or here on the host.

**Upgrading an install that still has a `team.yaml`: import it before you start the new
version** ([upgrade.md](upgrade.md), "Upgrading to the user database"). A tool server with an
empty user database lets nobody in, and logs `the user database is empty` at start-up.

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

Once, when upgrading an install that still has the old file. Read it at a reviewed commit,
keep a copy outside the content (the pull request that deletes it comes next), and always look
at the dry run first:

```sh
sha=$(git -C "$PMOS_CONTENT_CHECKOUT" rev-parse HEAD)     # the commit to record
git -C "$PMOS_CONTENT_CHECKOUT" show "$sha:content/team.yaml" > ~/team.yaml.$sha
deploy/pmos-admin users import --from ~/team.yaml.$sha --commit "$sha"
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

`--bootstrap` is for a test stack's empty database (CI imports `deploy/test/ci-users.yaml` this
way): it takes the file's admins without asking, all in one transaction, and is refused
(exit 3) if the users table has any row at all, deactivated people included. Development and
eval stacks without a database use a seed instead (`PMOS_USERS_SEED`, the same file shape),
which the tool server refuses if it is inside the content tree.

Then delete `content/team.yaml` by a pull request on the content repo: nothing reads it any
more, and `pmos lint` fails while it is there. To give someone a personal bot without the
Slack join flow, `deploy/pmos-admin pmos bot add-personal --root content --handle <handle>`
writes it into the content checkout; commit it like any content change.

Then, for **every Admin and Auditor**, link their Microsoft account on the host (imported admins
and auditors have none):

```sh
deploy/pmos-admin users set-entra <handle> --oid <their object id>
```

`--oid` uses the tenant in `PMOS_WEB_ENTRA_TENANT_ID` from `deploy/.env`.

## Day to day

**Most of this is the web app's now** (slice U5a; the Team screen itself is slice U5b): an
admin invites people, changes roles (never `admin`), sets or confirms sign-in names and the
Atlassian email or GitHub login, deactivates and reactivates, unlinks a Microsoft, Atlassian
or GitHub account, and assigns reviewers their PMs. Every change is signed by the admin's
session and recorded. The web app only works with signed user assertions
(`PMOS_ASSERTION_MODE=require`, as `deploy/setup.sh` sets it); a development stack with
`PMOS_ASSERTION_INSECURE_OFF=1` can read the Team page but changes nobody (`assertion_required`).

**Still the host's alone:** making an admin (`grant-admin`), the bootstrap admin, setting or
removing an **Admin's or Auditor's** sign-in name, Atlassian email or Microsoft link
(`set-sign-in-name`, `set-atlassian-email`, `set-entra`, `unset-entra`), the pilot exception,
deactivations past the hourly limit, the import and export. Nobody changes their own roles,
status or addresses in the web app, so an admin's own changes are another admin's or the
host's.

```sh
deploy/pmos-admin users list                                  # who, status, roles, flags
deploy/pmos-admin users grant-admin ana --oid <object id>     # the only way to make an admin
deploy/pmos-admin users deactivate ben                        # not capped; revokes admin
deploy/pmos-admin users reactivate ben                        # never restores admin
deploy/pmos-admin users export --format team-yaml > team.yaml # before a rollback
deploy/pmos-admin users password-setup ana                    # a one-time password setup code
```

**Password sign-in** (`docs/password-sign-in.md`, when `PMOS_WEB_PASSWORD_LOGIN=1`): an admin
makes a person's one-time setup code on the Team page; for an **Admin or Auditor** only the
host can, with `password-setup <handle>`, which prints the code once (24 hours, single use; a
new one replaces it). Give it to the person yourself, never by a channel others read. The same
command is the **reset** for someone who forgot their password or lost their phone: using the
code replaces their password, authenticator and recovery codes and signs them out everywhere.
Until it is used the old password still works: to shut someone out now, deactivate them. A
reset is never silent: the person sees "Your sign-in was reset by <name> on <date>" until they
dismiss it, and the admin overview lists the week's resets. When someone becomes an Admin or
Auditor, a password they set up with a code made in the web app stops working (the Team page
says "Password needs host"): give them a code with `password-setup <handle>`.

**Turning password sign-in on in the kit**: set `PMOS_WEB_PASSWORD_LOGIN=1`, the web app's
`https://` address in `PMOS_WEB_PUBLIC_URL`, and `PMOS_WEB_TRUSTED_PROXIES` to the address
your reverse proxy reaches the web app from (for a proxy on this host, the `pmos` network's
gateway: `deploy/pmos-admin up --web` prints it when it is blank, and refuses to start the web
app until it is set). The proxy must send `X-Forwarded-For`. Someone guessing from one address
only makes that address wait; the person still signs in from their own.

`grant-admin --keep-entra` reuses an Entra link the host already confirmed. A deactivated
admin who comes back is reactivated without `admin`; grant it again on purpose.

## Deactivating someone, and undoing it

Slack's `remove <handle>` and the web app's Team page (slice U5a) deactivate; they never
delete and commit nothing. In one step: their sessions end, `admin` is taken away if they
had it, and their linked Jira, Confluence, Slack and GitHub accounts are **suspended** (PM OS
won't use them). Their pending drafts are **held**. Then:

- **Reactivate within 24 hours** (the Team page, or `deploy/pmos-admin users reactivate
  <handle>`): the accounts work again and the held drafts can be decided. `admin` doesn't come back: `grant-admin` it
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
admin works, on the host or in the web app (the person then shows a badge, and the overview
says when the exception ends, from 14 days before). `off` is refused while anyone holds both,
naming them; `--revoke-auditor` takes `auditor` away from each in the same step and keeps
`admin`. If the date passes and nobody ran `off`, the exception counts as off: changes to
that person are refused until one of the two roles is gone. Revoking either role,
deactivating them and `off --revoke-auditor` still work (another admin can do the first two
in the web app), the person keeps their admin powers meanwhile (only their audit-log reads
are refused), and PM OS never lets this leave it without a working admin. The overview shows
a red notice naming them.

## Break glass: the web app is down or an admin is locked out

The host commands don't need the web app, the console or Slack, and they work whatever the
assertion mode in `deploy/.env`:

```sh
deploy/pmos-admin users grant-admin <handle> --oid <object id>   # make someone admin
deploy/pmos-admin users revoke-admin <handle>                    # take admin away
deploy/pmos-admin users unset-entra <handle>                     # a wrong Microsoft link
```

The web app can do the last two for anyone but an Admin or Auditor (and not on oneself); the
host does them for everyone.

PM OS always keeps at least one active admin: the last one can't be demoted or deactivated by
anyone, from anywhere, and the database itself enforces it. To replace the last admin,
`grant-admin` the new one first, then `revoke-admin` the old one.
