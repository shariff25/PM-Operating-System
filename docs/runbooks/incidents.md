# Incidents

Write down what happened, when, and what you did; it becomes the post-incident note.

## A bot did something it shouldn't (a guardrail incident)

**Examples:**
- a draft went out without approval;
- a bot read a channel or project outside its scope;
- a policy denial in a trace that you don't understand.

1. **Pause the bot:** `deploy/pmos-admin pause <bot>`.
2. **Find the task:**
   - in the console's Activity screen, open the task's trace and audit events;
   - or search the audit log:
     ```sh
     grep '"bot":"<bot>"' /srv/pmos/state/audit/audit.jsonl | tail -50
     ```
   - every write shows who approved it (`decided_by`). Every refusal shows as `denied` or as a
     `policy_denial` in the trace.
3. **If something was posted or created,** ask the PM whose token was used to delete it. Writes
   always use the approving PM's own account, so it's in their name.
4. **Report it to the other platform owners.** Gate 2 requires zero guardrail incidents, so
   every one is reviewed. Fix the cause (a role, a policy or a skill) by pull request, with a
   second owner's review, before resuming the bot.

## A token or secret leaked

**A PM's linked account:**
1. Run `deploy/pmos-admin revoke <handle> <provider>`.
2. Have them revoke the grant on the provider's side ([bots-and-members.md](bots-and-members.md)).

**A Slack app token:**
1. Regenerate it on the app's page.
2. Put the new token in `deploy/.env`.
3. Run `deploy/pmos-admin up`.

**A token between the services** (in `deploy/.env`):
1. Generate a new value: `openssl rand -hex 24`.
2. Replace it in `deploy/.env` and run `deploy/pmos-admin up`.
3. If it was a bot's worker or gateway token, recreate that bot's sandbox:
   `openshell sandbox delete pmos-<bot>`, then `deploy/pmos-admin sandbox <bot>`.

**The vault key (`PMOS_VAULT_KEY`):**
1. Put a new key first and keep the old one after it, comma-separated:
   ```
   k2:$(openssl rand -base64 32),k1:<old>
   ```
2. Run `deploy/pmos-admin up`. Stored tokens are re-sealed under the new key as they're read.
3. Once every PM has used PM OS again, remove the old key.
4. If the database may also have leaked, revoke every linked account instead.

## A service won't start

- **Find the cause:** run `deploy/pmos-admin logs <service>`. The last lines name it. Common
  causes:
  - a missing setting in `deploy/.env` (the message names it);
  - content that fails the lint (`deploy/pmos-admin lint`);
  - a full disk (`df -h`).
- **The tool server and bot runtime refuse to start on content that fails `pmos lint`.** A bad
  merge can't take PM OS down silently: fix the content by pull request. Until the fix lands,
  check out the last good commit in `/srv/pmos/content-repo`.
- **Out of disk:**
  1. Move old backups out of `deploy/backups/`.
  2. Run `docker image prune` to drop old images.
  3. Check `/srv/pmos/state`.

## The Spark is gone

Follow [backup-restore.md](backup-restore.md) on a new machine, using the latest off-Spark
backup and the `deploy/.env` from the password manager.
