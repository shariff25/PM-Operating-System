# Bots and members

## A new member

New members join through Slack. They tap **Request access**, and a platform owner approves on
the card (`docs/onboarding-spec.md`). The approval commits:
- their entry in `team.yaml`;
- their personal bot `pm-<handle>`;
- that bot's sandbox policy.

Once the content sync pulls the commit, within about 5 minutes, give their bot a sandbox:

```sh
cd /opt/pmos
deploy/pmos-admin add-bot pm-<handle>        # their bot's own runtime and gateway tokens
deploy/pmos-admin sandbox pm-<handle>        # its OpenShell sandbox and worker
```

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
deploy/pmos-admin pause <bot>      # stops its sandbox; its tasks wait
deploy/pmos-admin resume <bot>
```

To stop it for good, remove its `BOT.md` by pull request and delete its sandbox with
`openshell sandbox delete pmos-<bot>`.

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
