# Daily check, and "PM OS is slow or down"

## Daily check (2 minutes)

```sh
cd /opt/pmos
deploy/pmos-admin status
```

All is well when:
- **every container** says `healthy`;
- **`toolserver`, `gateway`, `bots` and `docs`** show `ok`;
- **the gateway's line** shows `"waiting"` near 0. A queue that stays high means the GPU is
  overloaded.

Once a week, also run `deploy/pmos-admin smoke <your handle>`.

## Someone says PM OS is slow or not answering

1. **Run `deploy/pmos-admin status`.** Find the first service that isn't `ok`:

   | What you see | What to do |
   | --- | --- |
   | `toolserver` storage not ok | Postgres is down or full: `deploy/pmos-admin logs postgres`; check free disk with `df -h` |
   | `gateway` upstream not ok | A model server is down: `deploy/pmos-admin logs vllm-default` (or `vllm-extraction`), then `deploy/pmos-admin restart vllm-default` |
   | `gateway` `"waiting"` high | Too many requests for the GPU. Interactive work goes first; scheduled routines wait. If it stays high for days, that's the signal for a second Spark (implementation plan, Phase 3). |
   | `bots` not ok | Read its `checks`: it names the service it can't reach |
   | Slack bot not listed | Its tokens are missing, or it's crashing: `deploy/pmos-admin logs slack` |
   | Everything ok, but one bot doesn't answer | Its sandbox and worker: `openshell sandbox list` and `systemctl --user list-units 'pmos-worker@*'` (as the user OpenShell runs as), then `deploy/pmos-admin pause <bot>` and `resume <bot>` |

2. **Look at the console** (Activity), or ask the PM for the task's thread. A task stuck in
   `queued` means no worker is claiming for that bot. One that `failed` gives its reason.

3. **Restarting one service is safe.** Tasks keep their leases, and a task that was running is
   retried once (D2).

   ```sh
   deploy/pmos-admin restart <service>
   ```

## Where the logs are

- `deploy/pmos-admin logs <service> [-f]`: the last 200 lines, or follow along.
- **Audit:** `/srv/pmos/state/audit/audit.jsonl`. Every tool call, with no message text.
- **Model usage:** `/srv/pmos/state/usage/usage.jsonl`. Who, which role and how many tokens,
  with no text.
- **Retention:** logs never hold tokens. Slack message text in traces is removed after 24 hours,
  and traces after 30 days.
