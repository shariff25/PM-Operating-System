# Install PM OS on the Spark

Do the stages in order and check each one before starting the next. Stages 1–4 use no real
tokens and no real data, so you can go back and repeat them freely. Stage 6 is Gate 1.

Allow about half a day, most of it waiting for the first image build and model download.

## Before you start

- **The Spark:** DGX OS, with Docker and the NVIDIA container toolkit (both come with DGX OS).
  You need a user that may run `docker`.
- **OpenShell:** installed on the Spark (`openshell --version`).
- **Network:** outbound HTTPS to:
  - GitHub, for the code and content repos;
  - Docker Hub and `nvcr.io`, for images;
  - Hugging Face, for models;
  - your docs site's host (`PMOS_DOCS_BASE_URL`), for the Docs bot;
  - Slack: `slack.com`, `wss-primary.slack.com` and `wss-backup.slack.com`;
  - from Gate 1: `api.atlassian.com`, `auth.atlassian.com` and `api.github.com`.

  If the Spark has an outbound proxy, you'll set it in stage 1.
- **The repo in your GitHub org.** The content repo must be in your company's GitHub org
  (`PMOS_CONTENT_REPO` must be in `PMOS_GITHUB_ORG`). If it still lives elsewhere, move it first.

## Stage 1: Code, content and settings

```sh
sudo mkdir -p /opt/pmos /srv/pmos && sudo chown "$USER" /opt/pmos /srv/pmos
git clone <the PM OS repo> /opt/pmos
cd /opt/pmos
deploy/setup.sh --content /srv/pmos/content-repo --clone <the content repo> --state /srv/pmos/state
```

1. **Read `setup.sh`'s output.** Warnings about OpenShell or the NVIDIA runtime must be gone
   before stages 3 and 4.
2. **The deploy key.** The content checkout should pull with a read-only deploy key:
   - in GitHub, open the repo's Settings, then Deploy keys, and add the public key;
   - set `PMOS_CONTENT_DEPLOY_KEY=/srv/pmos/deploy_key` in `deploy/.env`;
   - use the `git@github.com:` URL for the checkout's remote.
3. **The proxy, if the Spark has one.** In `deploy/.env`, set `HTTPS_PROXY` and `NO_PROXY`.
   Also set `PMOS_BUILD_CA` if the proxy re-signs TLS.
4. **Your docs site (optional).** For the Docs bot, set `PMOS_DOCS_BASE_URL` (and, if you
   need them, `PMOS_DOCS_NAME`, `PMOS_DOCS_PAGE_PREFIXES`, `PMOS_DOCS_API_PREFIX` and
   `PMOS_DOCS_PRODUCTS`) in `deploy/.env`. The site must publish `llms.txt` and markdown
   pages: see "Pointing the Docs bot at your docs" in `docs/docs-server.md`. You can leave it
   blank and add it later; until then the Docs bot says no docs site is configured.
5. **The console (optional).** `deploy/setup.sh` generates `PMOS_CONSOLE_PROXY_SECRET` in
   `deploy/.env`. Its only use is the proxy, so if the console sits behind the Spark's sign-in
   proxy, do both of these:
   - set `PMOS_CONSOLE_TRUST_PROXY=1` in `deploy/.env`, after the secret is there (the console
     won't start with trust on and no secret);
   - configure the proxy to strip `X-Forwarded-User` and `X-Pmos-Proxy-Secret` from the
     browser's request, then set `X-Pmos-Proxy-Secret` itself to the same value. The Caddy
     example is in `docs/console.md`, under "Sign-in". Check it with a request that sends its
     own headers: neither may reach the console.

   **Existing installs:** run `deploy/setup.sh` again with the same arguments as above. It only
   fills in missing values, so it adds the secret and leaves the rest alone. Then restart with
   `deploy/pmos-admin up --models`, and set up the proxy as above before you turn trust on.
6. **Save the settings.** Copy `deploy/.env` into the team password manager now. It holds the
   vault key and the console proxy secret; without the vault key, stored tokens can't be read.

## Stage 2: First start, without models or Slack

```sh
deploy/pmos-admin build            # 15–30 minutes the first time
deploy/pmos-admin up --bringup
deploy/pmos-admin status
```

**Expect:**
- every container is `healthy`;
- `toolserver`, `gateway` and `bots` show `ok`;
- the gateway says `"stub":true`;
- `docs` turns ready once its first sync of your docs site finishes (about a second per page);
  with no `PMOS_DOCS_BASE_URL` it stays not ready (`docs site not configured`), which is expected.

**Add the first platform owners.** Everyone after them joins through the bot.

1. Add each owner and generate their bots' sandbox policies:

   ```sh
   deploy/pmos-admin pmos member add --root content --handle <handle> --slack-id <U…> \
     --email <atlassian email> --github <login> --roles pm,admin
   deploy/pmos-admin pmos policy generate --root content --out policies
   deploy/pmos-admin lint
   ```

2. Commit the changes in `/srv/pmos/content-repo` on a branch, and merge them by pull request.
3. Then pull the checkout and run `deploy/pmos-admin reindex`.
4. **Smoke test:** `deploy/pmos-admin smoke <your handle>` should end with
   `smoke test passed`. The answer comes from the stub model, so it reads like test text; this
   stage checks the plumbing, not the answers.

## Stage 3: The models

1. **Check the model settings.** In `deploy/.env`, confirm `PMOS_VLLM_IMAGE` (NVIDIA's vLLM
   image for the Spark), `PMOS_MODEL_DEFAULT` and `PMOS_MODEL_EXTRACTION`. Check each model's
   id on Hugging Face. Set `HF_TOKEN` if a model needs one.
2. **Turn off the stub:** set `PMOS_GATEWAY_STUB=0`.
3. **Start the models:**

   ```sh
   deploy/pmos-admin up --bringup --models
   deploy/pmos-admin logs vllm-default -f     # the first start downloads the model: wait for "ready"
   deploy/pmos-admin status                   # the gateway's readiness lists both upstreams as ok
   deploy/pmos-admin smoke <your handle>      # now a real answer from the local model
   ```

4. **If a model server runs out of GPU memory,** lower `PMOS_MODEL_DEFAULT_GPU_SHARE` or
   `PMOS_MODEL_EXTRACTION_GPU_SHARE`. The two shares must add up to less than 0.9.
5. **Check the served names.** Confirm them in `deploy/gateway.yaml`: they must match each
   server's `--served-model-name`.

## Stage 4: Sandboxes (OpenShell)

The bring-up worker isn't isolated. Replace it with the Docs bot's real sandbox.

1. **Let sandboxes find the services.** Add this line to `/etc/hosts` on the Spark:
   ```
   127.0.0.1 toolserver.pmos.internal gateway.pmos.internal bots.pmos.internal
   ```
2. **Load the provider profiles** (once). Each command is from `docs/sandbox-policies.md`:
   ```sh
   openshell provider profile import -f /srv/pmos/content-repo/policies/providers/pmos-gateway.yaml --global
   openshell provider profile import -f /srv/pmos/content-repo/policies/providers/pmos-worker.yaml --global
   ```
3. **Start the Docs bot's sandbox, without the bring-up worker:**
   ```sh
   deploy/pmos-admin down && deploy/pmos-admin up --models
   deploy/pmos-admin sandbox docs
   deploy/pmos-admin smoke <your handle>
   ```
4. **Create each platform owner's personal bot sandbox:**
   ```sh
   deploy/pmos-admin add-bot pm-<handle>
   deploy/pmos-admin sandbox pm-<handle>
   ```
5. **Run the red-team checklist** in `policies/redteam/checklist.md`. Run it on the Spark only,
   never on a shared machine. Work through the "Verify on the Spark" table in
   `docs/sandbox-policies.md` as you go, and note anything that differed.

## Stage 5: Slack

1. **Create the app.** At <https://api.slack.com/apps>, create an app from
   `docs/slack-app-manifest.yaml` in your company's workspace, and install it. A workspace admin
   may need to approve it.
2. **Add the tokens to `deploy/.env`:**
   - `PMOS_SLACK_BOT_TOKEN` (`xoxb-…`);
   - `PMOS_SLACK_APP_TOKEN` (`xapp-…`, scope `connections:write`);
   - `PMOS_CONSOLE_SLACK_URL`.
3. **Start Slack:** run `deploy/pmos-admin up --models`. The Slack bot starts once its tokens
   are set.
4. **Check it:** DM the app `docs:` and a question your docs site answers. You should see a progress message, then
   an answer with links.
5. **Check tagging and the Home.** The manifest turns on the Home tab, the events
   `app_mention` and `app_home_opened`, and the bot scope `app_mentions:read`. Open the app in
   Slack: its **Home** tab shows your Inbox. Once you have a personal bot and Slack linked
   (Stage 6), tag `@PM OS` in a thread in a channel the app is in; the draft arrives in your DM
   (`docs/slack-home.md`, `docs/slack-reply.md`).

## Stage 6: Live connectors (Gate 1)

Only after the red-team checklist passes.

1. **The apps.** Create the Atlassian OAuth 2.0 (3LO) app, add OAuth to the Slack app, and
   create the GitHub App. For the GitHub App, follow "GitHub App setup" in `docs/toolserver.md`.
2. **The settings.** Copy `deploy/live.env.example` to `deploy/live.env` and fill it in. Delete
   the lines you don't use.
3. **The OAuth callback.** PMs' browsers must reach `PMOS_PUBLIC_URL/oauth/...` on the tool
   server, and nothing else of it. Put a reverse proxy in front that forwards only `/oauth/`.
   For example, with Caddy:
   ```
   pmos.<your domain> {
     handle /oauth/* {
       reverse_proxy 127.0.0.1:8710
     }
     respond 404
   }
   ```
   Register `PMOS_PUBLIC_URL/oauth/{atlassian,slack,github}/callback` with each app.
4. **Switch to live:** set `PMOS_CONNECTORS=live` in `deploy/.env`, then
   `deploy/pmos-admin up --models`.
5. **Link your own accounts.** DM PM OS `setup` and link your accounts. Then ask your bot
   something that needs Jira.
6. **Run the benchmarks** (`docs/evals.md`):
   ```sh
   deploy/pmos-admin evals-run --category docs-qa --note "gate 1"
   deploy/pmos-admin evals load --tasks 5
   ```
   Commit the reports in `/srv/pmos/content-repo/evals/reports`. The load test runs on the
   live stack as the benchmark member `evals`, the only handle the benchmark token may use
   (`PMOS_EVALS_AS`). Add that member once, with no real Slack account, and merge it:
   `deploy/pmos-admin pmos member add --root content --handle evals --slack-id U0EVALS --roles pm`.
   To run it as yourself instead, set `PMOS_EVALS_AS=<your handle>` in `deploy/.env`,
   `deploy/pmos-admin restart bots`, pass `--as <your handle>`, then empty it and restart
   again.
7. **Test a restore** by following [backup-restore.md](backup-restore.md). Gate 1 asks that a
   platform owner can do it from the runbook alone.

Gate 1 is passed when:
- the red-team checklist passes;
- the benchmarks have met their bar two weeks in a row;
- every task in the load test finishes;
- a restore has been done from this runbook.

## The console behind a sign-in proxy (optional)

The console (`docs/console.md`) starts with `deploy/pmos-admin up --models --console`, and only
behind a sign-in proxy on the Spark. `PMOS_CONSOLE_DEV_USER` is for running the console on your
own machine: it needs a loopback bind, and the kit's container listens on `0.0.0.0`, so the
console refuses it here. Step 5 above made the secret; to put the proxy in front:

1. **The settings.** In `deploy/.env`, set `PMOS_CONSOLE_TRUST_PROXY=1`, empty
   `PMOS_CONSOLE_DEV_USER`, and set `PMOS_CONSOLE_PUBLIC_URL` to the address browsers use.
   `setup.sh` has already made `PMOS_CONSOLE_PROXY_SECRET`; the console refuses to start
   without it (or with one under 32 characters).
2. **The proxy** signs the person in, strips any `X-Forwarded-User` and `X-Pmos-Proxy-Secret`
   the browser sent, then forwards every request with both set itself: their handle or email
   in `X-Forwarded-User`, and `X-Pmos-Proxy-Secret` set to `PMOS_CONSOLE_PROXY_SECRET`. For
   example, with Caddy and an SSO add-on that sets `{http.auth.user.id}` (a fuller example is
   in `docs/console.md`, under "Sign-in"):
   ```
   console.pmos.<your domain> {
     request_header -X-Forwarded-User
     request_header -X-Pmos-Proxy-Secret
     # your SSO plugin's directive here
     reverse_proxy 127.0.0.1:8760 {
       header_up X-Forwarded-User {http.auth.user.id}
       header_up X-Pmos-Proxy-Secret {env.PMOS_CONSOLE_PROXY_SECRET}
     }
   }
   ```
   Give the proxy the secret through its environment or secret store, never in a file in git.
3. **Check:** `curl -s -o /dev/null -w '%{http_code}\n' -H 'X-Forwarded-User: <your handle>'
   http://127.0.0.1:8760/bots` on the Spark answers `401`: without the proxy's secret, the
   user header counts for nothing. A request through the proxy that sends its own
   `X-Forwarded-User` and `X-Pmos-Proxy-Secret` must not get them through. Through the proxy,
   the Bots screen opens.

The console's tool-server token (`PMOS_CONSOLE_TS_TOKEN`) may only read approvals, so even a
compromised console can't decide one or call a tool.

## Optional: the web app

The web app is **off by default**. Turn it on only for a closed test network: development
sign-in lets anyone who can reach its port sign in as any member. See
[docs/web-app-running.md](../web-app-running.md) before you start it.
