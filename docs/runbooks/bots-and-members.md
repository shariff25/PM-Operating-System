# Bots and members

## A new member

New members join through Slack. They tap **Request access**, and a platform owner approves on
the card (`docs/onboarding-spec.md`). The approval first adds them to the user database (they
can use PM OS at once; [users.md](users.md)), then commits to the content repo:
- their personal bot `pm-<handle>`;
- that bot's sandbox policy.

There is no `team.yaml` to edit: people and roles are only in the user database. Someone an
admin invites in the web app gets no personal bot automatically; write one with
`deploy/pmos-admin pmos bot add-personal --root content --handle <handle>` and
`deploy/pmos-admin pmos policy generate --root content --out policies`, and commit both by pull
request.

A bot's `owner` in its `BOT.md` must be a PM OS handle (`pmos lint`: `bot-owner-handle`).
Whether that person is still an active user is the database's to say: the admin overview lists
bots whose owner can't use PM OS (`bots_without_active_owner`), a personal bot serves only its
owner while they are active, and Slack and the web app won't deactivate someone who owns a
team bot.

Once the content sync pulls the commit, within about 5 minutes, give their bot a sandbox:

```sh
cd /opt/pmos
deploy/pmos-admin add-bot pm-<handle>        # their bot's own runtime and gateway tokens
deploy/pmos-admin sandbox pm-<handle>        # its OpenShell sandbox and worker
systemctl --user status "pmos-worker@$(systemd-escape -- pm-<handle>).service"
```

Run them as the user OpenShell runs as, not with `sudo`. A bot name is lowercase letters, digits
and single hyphens, at most 32 characters; `pmos-admin` refuses anything else before it writes a
token or creates a sandbox. The worker runs as that user's systemd unit, which restarts it if it
fails (at most five times in ten minutes, then it stays failed until `resume`) and starts it
again after a reboot, as long as lingering is on (`install.md`, Stage 4 step 2).

Until you do, their bot's tasks wait in the queue. Their `setup` checklist shows the bot as
"being set up".

## A new team bot

A team bot is a `BOT.md` under `content/bots/` with `scope: team`.

1. Add it by pull request.
2. Generate its policy (`pmos policy generate`) in the same pull request.
3. Get it reviewed by a second platform owner. A new bot is a sandbox policy change (POL-2).
4. Once it's merged and pulled, run `add-bot <bot>` and `sandbox <bot>`, as for a member.

## Pausing a bot

When a bot misbehaves, pause it before you investigate:

```sh
deploy/pmos-admin pause <bot>      # stops and disables its worker unit, then its sandbox; its tasks wait
                                   # (not as root; exit 1 and a list of what is left if a step failed)
deploy/pmos-admin resume <bot>     # starts the sandbox, then the worker unit (clearing a failed one)
```

A paused bot stays paused after a reboot: `pause` disables the unit, and only `resume` enables
it again.

To stop it for good, remove its `BOT.md` by pull request, `pause` it, and delete its sandbox
with `openshell sandbox delete pmos-<bot>`.

## A member leaves

Use `remove <handle>` in a DM to PM OS, as a platform owner. It **deactivates** them in the
user database (nothing is committed; see [users.md](users.md) "Deactivating someone"): their
sessions end and their linked accounts are suspended, then deleted after 24 hours unless they
are reactivated. Slack won't deactivate an Admin or an Auditor, and stops after 3
deactivations an hour; use the host commands for those. Then delete their bot's sandbox:

```sh
openshell sandbox delete pmos-pm-<handle>
```

Also remove their `worker-pm-<handle>` and `bot-pm-<handle>` entries from
`PMOS_RT_WORKER_TOKENS` and `PMOS_GW_BOT_TOKENS` in `deploy/.env`, then run
`deploy/pmos-admin up`.

## Revoking one linked account

For example, if someone's laptop is lost:

```sh
deploy/pmos-admin revoke <handle> atlassian      # or slack, github
```

This deletes the stored tokens. Also revoke the grant on the provider's side:
- **Atlassian:** "Connected apps" in their account settings.
- **Slack:** the app's page under "Authorized users".
- **GitHub:** Settings, then Applications.
