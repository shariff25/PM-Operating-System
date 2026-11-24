# Install PM OS on a host

Do the stages in order and check each one before starting the next. Stages 1–4 use no real
tokens and no real data, so you can go back and repeat them freely. Stage 6 is Gate 1.

Allow about half a day, most of it waiting for the first image build and model download.

## Before you start

- **The host:** a Linux host with Docker (the first host to verify on is an Ubuntu 24.04 x86
  virtual machine). You need a user that may run `docker`. `deploy/setup.sh` runs the host
  preflight (`deploy/preflight.sh`, stage `install`) before anything else and stops on any
  `fail`; nothing has yet been seen working on a real OpenShell host
  (`docs/openshell-any-host-spec.md` §6).
- **The model source** (`PMOS_MODEL_SOURCE`, chosen once with `setup.sh --model-source`; Stage 3):
  `local` needs an NVIDIA GPU and the NVIDIA container toolkit on a host the owner controls;
  `hosted-test` needs no GPU and takes fake data only. `hosted` (one hosted model the owner has
  approved, for real data) is not built yet and every service refuses it.
- **Tools:** `git`, `openssl` and `flock` (from `util-linux`, present on most Linux hosts;
  `setup.sh` checks for all three before it clones anything).
- **OpenShell:** installed on the host (`openshell --version`), following NVIDIA's own install
  page.
- **Network:** outbound HTTPS to:
  - GitHub, for the code and content repos;
  - Docker Hub and `nvcr.io`, for images;
  - Hugging Face, for models;
  - your docs site's host (`PMOS_DOCS_BASE_URL`), for the Docs bot;
  - Slack: `slack.com`, `wss-primary.slack.com` and `wss-backup.slack.com`;
  - from Gate 1: `api.atlassian.com`, `auth.atlassian.com` and `api.github.com`.

  If the host has an outbound proxy, you'll set it in stage 1.
- **The repo in your GitHub org.** The content repo must be in your company's GitHub org
  (`PMOS_CONTENT_REPO` must be in `PMOS_GITHUB_ORG`). If it still lives elsewhere, move it first.

## Stage 1: Code, content and settings

```sh
sudo mkdir -p /opt/pmos /srv/pmos && sudo chown "$USER" /opt/pmos /srv/pmos
git clone <the PM OS repo> /opt/pmos
cd /opt/pmos
deploy/setup.sh --content /srv/pmos/content-repo --clone <the content repo> --state /srv/pmos/state \
  --model-source local --exposure private     # or hosted-test, public: see below
```

`--model-source` defaults to `hosted-test` (fake data only) and `--exposure` to `public`, the
values that allow less; `setup.sh` never replaces a value already in `deploy/.env`. With
`PMOS_EXPOSURE=public` the web app refuses to start until slice H10 (the admin listener) is
built; `private` means the host is reached only from a network the company controls, which the
preflight's H-PRIVATE checks.

1. **Read `setup.sh`'s output.** Warnings about OpenShell or the NVIDIA runtime must be gone
   before stages 3 and 4.
2. **The deploy key.** The content checkout should pull with a read-only deploy key:
   - in GitHub, open the repo's Settings, then Deploy keys, and add the public key;
   - set `PMOS_CONTENT_DEPLOY_KEY=/srv/pmos/deploy_key` in `deploy/.env`;
   - use the `git@github.com:` URL for the checkout's remote.
3. **The proxy, if the host has one.** In `deploy/.env`, set `HTTPS_PROXY` and `NO_PROXY`.
   Also set `PMOS_BUILD_CA` if the proxy re-signs TLS.
4. **Your docs site (optional).** For the Docs bot, set `PMOS_DOCS_BASE_URL` (and, if you
   need them, `PMOS_DOCS_NAME`, `PMOS_DOCS_PAGE_PREFIXES`, `PMOS_DOCS_API_PREFIX` and
   `PMOS_DOCS_PRODUCTS`) in `deploy/.env`. The site must publish `llms.txt` and markdown
   pages: see "Pointing the Docs bot at your docs" in `docs/docs-server.md`. You can leave it
   blank and add it later; until then the Docs bot says no docs site is configured.
5. **The console (optional).** `deploy/setup.sh` generates `PMOS_CONSOLE_PROXY_SECRET` in
   `deploy/.env`. Its only use is the proxy, so if the console sits behind a sign-in proxy on
   this host, do both of these:
   - set `PMOS_CONSOLE_TRUST_PROXY=1` in `deploy/.env`, after the secret is there (the console
     won't start with trust on and no secret);
   - configure the proxy to strip `X-Forwarded-User` and `X-Pmos-Proxy-Secret` from the
     browser's request, then set `X-Pmos-Proxy-Secret` itself to the same value. The Caddy
     example is in `docs/console.md`, under "Sign-in". Check it with a request that sends its
     own headers: neither may reach the console.

   **Existing installs:** run `deploy/setup.sh` again with the same arguments as above. It only
   fills in missing values, so it adds the secret and leaves the rest alone. Then restart with
   `deploy/pmos-admin up --models`, and set up the proxy as above before you turn trust on.
6. **User assertions.** `setup.sh` writes `PMOS_ASSERTION_MODE=require` into `deploy/.env` and
   generates the web app's and the console's tokens and signing keys. Keep it `require`: the
   runtime and the tool server refuse to start in `off` or `verify` while those tokens are
   set, and every `pmos-admin up` (with any options, including the `up --models` restarts in
   these runbooks), `add-bot` and `restore` checks it before it starts anything
   (`docs/user-assertion.md`, Modes). There is no override here: the development one
   (`PMOS_ASSERTION_INSECURE_OFF`) is for fake-data stacks only, `deploy/compose.yaml` doesn't
   pass it, and the services refuse it with live connectors or a database. `PMOS_ONBOARDING_V2` and `PMOS_NUDGES` (the
   Slack first-time flow and nudges) are written as `1`; set either to `0` to turn it off.
7. **Save the settings.** Copy `deploy/.env` into the team password manager now. It holds the
   vault key and the console proxy secret; without the vault key, stored tokens can't be read.

### Service uid (`PMOS_UID` and `PMOS_GID`)

The services and the bots' workers run as `PMOS_UID:PMOS_GID` in `deploy/.env`, which
`setup.sh` takes from the content checkout's owner. They must be plain numbers and never 0:
a root-owned checkout would make the services root. If `pmos-admin` or `setup.sh` refuses a
uid or gid of 0 (an older install can have one), give the checkout and state directory to a
service account and set its ids:

```sh
sudo useradd --system --no-create-home --shell /usr/sbin/nologin pmos
sudo chown -R pmos:pmos /srv/pmos/content-repo /srv/pmos/state
# then in deploy/.env: PMOS_UID=$(id -u pmos) and PMOS_GID=$(id -g pmos), as numbers
```

`pmos-admin` is the gate. Docker Compose itself accepts `PMOS_UID=0` or a name, so don't start
the stack with a bare `docker compose up`. The commands that don't start a container as that
user (`status`, `logs`, `backup`, `reindex`, `revoke` and `down`) still work with a bad value,
so an incident can be handled first; fix the value before the next `up`.

## Stage 2: First start, without models or Slack

```sh
deploy/pmos-admin build            # 15–30 minutes the first time
sudo deploy/pmos-admin host-names apply      # the service names (Stage 4, step 1 explains them)
sudo deploy/pmos-admin host-firewall apply   # keeps containers off the cloud metadata service
deploy/pmos-admin up --bringup
deploy/pmos-admin status
```

`up` runs the host preflight first, every time, and refuses (exit 2, nothing started) on any
`fail` (`docs/openshell-any-host-spec.md` §3.2). Two of its checks need the host prepared once
by the two `sudo` lines above: H-NAMES (the three service names resolve on this host to their
pinned addresses, through the managed block in `/etc/hosts`) and H-METADATA (no container can
reach the VM's cloud metadata service; `host-firewall apply` adds the block, its
`docker.service` drop-in and a timer that keeps it in place). `deploy/pmos-admin preflight`
shows every check by hand.

**Expect:**
- every container is `healthy`;
- `toolserver`, `gateway` and `bots` show `ok`;
- the gateway says `"stub":true`;
- `docs` turns ready once its first sync of your docs site finishes (about a second per page);
  with no `PMOS_DOCS_BASE_URL` it stays not ready (`docs site not configured`), which is expected.

**Add the first platform owners.** People and roles live in the tool server's user database
([users.md](users.md)), never in the content. Everyone after the first owners joins through
the bot, or an admin's invite in the web app.

1. Make yourself the first admin, then give yourself a personal bot and generate its sandbox
   policy:

   ```sh
   deploy/pmos-admin users bootstrap-admin --handle <handle> --name "<name>" \
     --sign-in-name <work sign-in name> --atlassian-email <atlassian email> \
     --slack-id <U…> --github <login> --no-entra        # or --oid <Entra object id>
   deploy/pmos-admin pmos bot add-personal --root content --handle <handle>
   deploy/pmos-admin pmos policy generate --root content --out policies
   deploy/pmos-admin lint
   ```

   Each other owner then requests access in Slack (or you invite them in the web app); once
   they are in, make them an admin on the host:
   `deploy/pmos-admin users grant-admin <handle> --oid <their Entra object id>`.

2. Commit the content changes in `/srv/pmos/content-repo` on a branch, and merge them by pull
   request.
3. Then pull the checkout and run `deploy/pmos-admin reindex`.
4. **Smoke test:** `deploy/pmos-admin smoke <your handle>` should end with
   `smoke test passed`. The answer comes from the stub model, so it reads like test text; this
   stage checks the plumbing, not the answers.

## Stage 3: The models

There are three paths, by `PMOS_MODEL_SOURCE`: **local models** on this host's GPU (below),
**a hosted test model** with fake data ("Without a GPU", below), and **`hosted`**, one hosted
model the owner has approved for real data, which comes only after slices H12 and H13 (the
owner's recorded approval of the provider, with its retention terms) and is refused until then.

### Local models (`PMOS_MODEL_SOURCE=local`)

1. **Check the model settings.** In `deploy/.env`, set `PMOS_VLLM_IMAGE`: a vLLM image you
   choose for this host's architecture and GPU, pinned by tag (it has no default; the
   preflight's H-NVIDIA checks it). Confirm `PMOS_MODEL_DEFAULT` and `PMOS_MODEL_EXTRACTION`, and
   check each model's id on Hugging Face. Set `HF_TOKEN` if a model needs one.
   Real data under `local` also needs a recorded attestation that the owner controls this GPU
   host, before live connectors (the preflight's H-LOCAL):
   `sudo deploy/pmos-admin host attest local-gpu --owner-controls --reason "<why>"`.
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

### Without a GPU: a hosted test model (testing only)

On a host with no GPU, the gateway can send every role to a hosted OpenAI-compatible model
instead (`PMOS_MODEL_SOURCE=hosted-test` in `deploy/.env`, with `PMOS_HOSTED_TEST_URL`,
`PMOS_HOSTED_TEST_MODEL`, `PMOS_HOSTED_TEST_LABEL` and `PMOS_GATEWAY_REMOTE_KEY`; see
`deploy/env.example`). Prompts then leave the host, so it is for **fake data only**: `pmos-admin
up` refuses live connectors and keeps the Slack bot off, and `status` says so on every run. Set
`PMOS_GATEWAY_STUB=0`, then `deploy/pmos-admin up` (never `--models`). Behind a proxy that
re-signs TLS, also set `PMOS_GATEWAY_CA_FILE` to the proxy's whole CA bundle. The gateway reports
and logs only the label. This is the gateway's part of slice H4; `hosted` (an approved provider)
is slice H12.

If another stack on the host already listens on 127.0.0.1:8710 to 8770, set
`PMOS_TOOLSERVER_PORT`, `PMOS_GATEWAY_PORT`, `PMOS_BOTS_PORT`, `PMOS_CONSOLE_PORT` and
`PMOS_WEB_PORT` in `deploy/.env`; `pmos-admin` follows them.

## Stage 4: Sandboxes (OpenShell)

The bring-up worker isn't isolated. Replace it with the Docs bot's real sandbox.

> **Not yet seen on a real OpenShell host.** Step 1 is how sandboxes find the three services
> (`docs/openshell-any-host-spec.md`, §3.3). That OpenShell resolves names through this host's
> `/etc/hosts`, and that a sandbox reaches an address on the `pmos` network, are checked in the
> acceptance run (§6, step 4: O-REACH, and H-1 from the Docs bot's sandbox). If a sandbox can't
> reach the services by name, stop and report: don't work around it (§3.3, step 6).

1. **Write the service names** (once, and again whenever `host-names check` fails). The tool
   server, gateway and runtime have fixed addresses on the `pmos` network, `10.201.77.10`, `.11`
   and `.12` (`deploy/compose.yaml`; the two model servers have `.13` and `.14`). A sandbox can't
   reach them on `127.0.0.1`: OpenShell never allows a loopback destination, and inside a sandbox
   `127.0.0.1` is the sandbox itself. So the host resolves the three names to those addresses,
   through a block `pmos-admin` keeps in `/etc/hosts`:
   ```sh
   sudo deploy/pmos-admin host-names apply   # writes or replaces only its own block
   deploy/pmos-admin host-names check        # each name resolves to its own address
   ```
   The block sits between `# BEGIN pmos (managed by pmos-admin)` and `# END pmos`; every other
   line of `/etc/hosts` is kept. It always holds the same three lines on every host, and takes no
   argument or setting. Don't edit it, and don't add a line for these names anywhere else:
   `check` fails on a missing, stale or doubled block, on any other line that names one of them
   (the old `127.0.0.1 toolserver.pmos.internal …` line included: remove it), and when the host's
   resolver answers anything else. `apply` keeps lines that aren't its own, so it exits 1 and
   names any such line for you to remove. `deploy/pmos-admin sandbox` refuses to start a sandbox
   while `check` fails, and `deploy/pmos-admin status` shows it every time.

   The addresses are reviewed code, not settings: `deploy/pmos-admin up` refuses a
   `PMOS_NETWORK_SUBNET` that is shorter than a `/24` or doesn't hold `10.201.77.10` to `.14`, and
   a `PMOS_NETWORK_IP_RANGE` (the addresses Docker hands every other container,
   `10.201.77.128/25` by default) that holds any of them. It also refuses a `pmos` network that
   was made with other settings, such as one made before these addresses existed; re-create it
   with `deploy/pmos-admin down && deploy/pmos-admin up` (no eval stack may be running).
2. **Turn on lingering** for the user OpenShell runs as (once). OpenShell's gateway and each
   bot's worker are that user's systemd services, and without lingering they stop when you log
   out and don't start after a reboot. Run it as that user (not root):
   ```sh
   sudo loginctl enable-linger "$USER"
   loginctl show-user "$USER" -p Linger    # Linger=yes
   ```
   `deploy/pmos-admin sandbox` and `resume` refuse to start a worker until this says `yes`.
   Run every `pmos-admin sandbox`, `pause` and `resume` as this same user, never with `sudo`.
3. **Load the provider profiles** (once). Each command is from `docs/sandbox-policies.md`:
   ```sh
   openshell provider profile import -f /srv/pmos/content-repo/policies/providers/pmos-gateway.yaml --global
   openshell provider profile import -f /srv/pmos/content-repo/policies/providers/pmos-worker.yaml --global
   ```
4. **Start the Docs bot's sandbox, without the bring-up worker:**
   ```sh
   deploy/pmos-admin down && deploy/pmos-admin up --models
   deploy/pmos-admin sandbox docs --dry-run    # what it will run; no tokens are printed
   deploy/pmos-admin sandbox docs
   systemctl --user status 'pmos-worker@docs.service'
   deploy/pmos-admin smoke <your handle>
   ```
   `sandbox` creates the sandbox from the worker image (`openshell sandbox create --from
   pmos-worker:<version>`), then starts the bot's worker as the systemd user unit
   `pmos-worker@<bot>.service` (from `deploy/systemd/pmos-worker@.service`), which restarts it
   if it fails and starts it again after a reboot. A name with a hyphen is escaped in the unit's
   name: `pm-ana`'s unit is `pmos-worker@pm\x2dana.service` (`systemd-escape -- pm-ana` prints
   it); quote it in the shell. Its log: `journalctl --user -u 'pmos-worker@docs.service'`.
   Not yet seen on a real OpenShell: whether `--from` takes the local image tag, and whether
   `sandbox exec` stays in the foreground for as long as the worker runs. Note in the record what
   you saw (`docs/openshell-any-host-spec.md` §6, step 2).
5. **Create each platform owner's personal bot sandbox:**
   ```sh
   deploy/pmos-admin add-bot pm-<handle>
   deploy/pmos-admin sandbox pm-<handle>
   ```
6. **Run the red-team checklist** in `policies/redteam/checklist.md`. Run it only on this host,
   and never on a host shared with other workloads. Work through the "Verify on a real host" list
   in `docs/sandbox-policies.md` as you go, and note anything that differed.

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
   `deploy/pmos-admin up --models`. Only with `PMOS_MODEL_SOURCE=local` and its attestation
   (Stage 3): under `hosted-test` every service refuses live connectors.
5. **Link your own accounts.** DM PM OS `setup` and link your accounts. Then ask your bot
   something that needs Jira.
6. **Run the benchmarks** (`docs/evals.md`):
   ```sh
   deploy/pmos-admin evals-run --category docs-qa --note "gate 1"
   deploy/pmos-admin evals load --tasks 5
   ```
   Commit the reports in `/srv/pmos/content-repo/evals/reports`. The load test runs on the
   live stack as the benchmark member `evals`, the only handle the benchmark token may use
   (`PMOS_EVALS_AS`). Add that user once: write a file outside the content holding
   `members: [{handle: evals, slack-id: U0EVALS, roles: [pm]}]`, import it with
   `deploy/pmos-admin users import --from <that file> --apply` (the database keeps no Slack id
   for `evals`; the import says so), then give it its bot with
   `deploy/pmos-admin pmos bot add-personal --root content --handle evals` and merge that.
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
behind a sign-in proxy on this host. `PMOS_CONSOLE_DEV_USER` is for running the console on your
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
   http://127.0.0.1:8760/bots` on the host answers `401`: without the proxy's secret, the
   user header counts for nothing. A request through the proxy that sends its own
   `X-Forwarded-User` and `X-Pmos-Proxy-Secret` must not get them through. Through the proxy,
   the Bots screen opens.

The console's tool-server token (`PMOS_CONSOLE_TS_TOKEN`) may only read approvals, so even a
compromised console can't decide one or call a tool.

## Optional: the web app

The web app is **off by default**. Turn it on only for a closed test network: development
sign-in lets anyone who can reach its port sign in as any member. Until slice H10 (the
admin-only listener and the public sign-in rules) is built, `pmos-web` refuses to start with
`PMOS_EXPOSURE=public`, so the web app, and password sign-in with it, runs only on a host
reached from networks the company controls. See
[docs/web-app-running.md](../web-app-running.md) before you start it.

With **password sign-in** (`PMOS_WEB_PASSWORD_LOGIN=1`, [docs/password-sign-in.md](../password-sign-in.md))
the web app also needs `PMOS_WEB_PUBLIC_URL` set to its `https://` address and
`PMOS_WEB_TRUSTED_PROXIES` set to the address the reverse proxy reaches it from: for a proxy on
this host, the `pmos` network's gateway, which the kit **pins**: `PMOS_NETWORK_SUBNET`
(`10.201.77.0/24`) and `PMOS_NETWORK_GATEWAY` (`10.201.77.1`) in `deploy/.env`, and
`env.example` already sets `PMOS_WEB_TRUSTED_PROXIES=10.201.77.1`. Change the pair only if
your site already routes that range. The proxy must send `X-Forwarded-For` (nginx:
`proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;`).

`deploy/pmos-admin up --web` refuses before anything starts, printing the fix, when:

| It says | What to do |
| --- | --- |
| `PMOS_WEB_TRUSTED_PROXIES is blank` | Put the printed `PMOS_WEB_TRUSTED_PROXIES=<gateway>` line in `deploy/.env`. |
| `The pmos network's gateway is <a>, but deploy/.env pins PMOS_NETWORK_GATEWAY=<b>` | The network was made before the kit pinned it (or with other settings). Run `deploy/pmos-admin down && deploy/pmos-admin up --web`: `down` removes the network and `up` makes it at the pinned address. Every service restarts. |
| `PMOS_WEB_TRUSTED_PROXIES names <x>, but ... the gateway, <g>` | The setting is stale (an old, unpinned gateway such as `172.18.0.1`). Set the printed line. A proxy in a container on the `pmos` network may be listed by its own address inside the subnet. |
| `PMOS_WEB_PUBLIC_URL must be the web app's https:// address` | Set it. |

**What a reverse proxy in front of the web app must not forward, and how big a body it takes.**
These rules hold on every host, private or not:

- `/healthz` and `/readyz`: answer `404` at the proxy. `/readyz` names which of the web app's
  dependencies are down, which tells an outsider what to attack and when. `deploy/pmos-admin
  status` and the container's health check reach both on `127.0.0.1:8770`.
- `/internal/`: answer `404` at the proxy. The daily brief route, `PUT /internal/brief/{handle}`,
  is served only on the web app's internal listener, `8771` (`PMOS_WEB_INTERNAL_BIND`, set by
  `deploy/compose.yaml`). `8771` is on the `pmos` network only and is **not published**: no host
  port, no proxy rule, and the preflight's H-EXPOSURE fails if anything on the host listens on it.
  The Slack service reaches it by name, so with the brief on set
  `PMOS_WEB_BRIEF_URL=http://web.pmos.internal:8771` (`deploy/env.example`). The browser-facing
  port answers `404` for the route, so a URL ending in `:8770` sends no brief.
- Body sizes: `client_max_body_size 2m;` on the web app's server block (nginx; axum's own default).
  The web app's limits are lower on most routes (the brief's route takes 128 KiB, password
  sign-in 4 KiB). The admin routes' 16 MiB bodies are not served on this port.

```nginx
location = /healthz    { return 404; }
location = /readyz     { return 404; }
location ^~ /internal/ { return 404; }
```

The full public example (rate limits, TLS, the admin listener) comes with slice H10.

**After it starts, if password sign-in answers "PM OS can't sign you in through this
address"** (`403 proxy_not_trusted`): the requests arrive from somewhere
`PMOS_WEB_TRUSTED_PROXIES` doesn't name, or the proxy sends no `X-Forwarded-For`. The web app's
`/readyz` counts them (`untrusted_proxy_refusals`, since it started:
`curl -s http://127.0.0.1:8770/readyz`) and its log says so at error level once a minute
(`deploy/pmos-admin logs web`). Check the proxy sets `X-Forwarded-For`, and that its requests
reach the published port from the gateway; fix `deploy/.env` and run `deploy/pmos-admin up
--web` again. Nothing is counted against a shared address meanwhile: every such request is
refused.
