# Running the web app

The web app has two parts. `web/` is the single-page app (TypeScript, built with Vite into
`web/dist`). `pmos-web` (`crates/web`) is its backend: it holds the browser session, serves
`/api` (the contract is `docs/web-api.md`) and serves the built app's static files. The product
plan is `docs/web-client.md`.

```
browser ──HTTPS──► reverse proxy ──► pmos-web :8770 ──► pmos-bots :8750        (client "web")
                                                   └──► pmos-toolserver :8710  (approvals, users)
                                                   └──► content/team.yaml      (Team page list only)
```

**Not ready for production.** See [Not ready for production](#not-ready-for-production). It is
off by default everywhere, and every way of turning it on is named for what it is.

## Run it locally

**With the mock only (no backend).** From the repo root:

```sh
pnpm install
pnpm -C web dev          # http://localhost:5173
```

`dev` uses the mock client, so state lives in the page and a reload resets it (`web/README.md`
lists the mock's sample drafts and switches).

**With the test stack.** `scripts/claude-test-stack.sh` can also run `pmos-web` against the test
stack's real runtime and tool server (fake Jira, Confluence and Slack fixtures; see
`docs/testing-with-claude.md` for the keys it needs):

```sh
PMOS_TEST_WEB=1 scripts/claude-test-stack.sh
```

With `PMOS_TEST_WEB=1` the script also:
- builds `web/` (`pnpm -C web build`) and `pmos-web`;
- adds a `web` client token to the runtime's `PMOS_BOTS_TOKENS` (fresh for this run);
- starts `pmos-web` on `127.0.0.1:8770` with development sign-in, after the runtime and tool
  server are healthy, and waits for its `/healthz`;
- prints the address. Sign in as your test handle (`PMOS_TEST_HANDLE`, default `tester`).

Without the variable, nothing about the web app runs. The session cookie is `Secure`, so
browsers keep it on `http://127.0.0.1` and `http://localhost` only; any other address needs TLS.

## Build it

```sh
pnpm install
pnpm -C web build            # web/dist
cargo build --release -p pmos-web
PMOS_WEB_STATIC_DIR=web/dist ...   # see the table below
```

The deployment image (`deploy/Dockerfile`) does both: a `webbuild` stage runs the pnpm build,
the Rust stage builds `pmos-web` with the other services, and the final image holds the binary and
the app's files at `/usr/share/pmos/web`.

## Environment variables

Names and defaults were read from the backend's in-progress `config.rs` (not yet on this branch,
where `crates/web` is only a stub): confirm them against `crates/web/src/config.rs` once it lands.

| Variable | Default | What it is |
| --- | --- | --- |
| `PMOS_WEB_BIND` | `127.0.0.1:8770` | Address to listen on. The container uses `0.0.0.0:8770` and publishes the port on `127.0.0.1` only. |
| `PMOS_WEB_DEV_LOGIN` | off | `1` turns on development sign-in. The server needs it or Entra ID sign-in; with neither it refuses to start. |
| `PMOS_WEB_ENTRA_TENANT_ID`, `PMOS_WEB_ENTRA_CLIENT_ID`, `PMOS_WEB_ENTRA_CLIENT_SECRET` | none | Entra ID sign-in, with `PMOS_WEB_PUBLIC_URL` (`docs/entra-sign-in.md`). `PMOS_WEB_ENTRA_AUTHORITY` is optional. The deploy kit passes all four from `deploy/.env`. |
| `PMOS_WEB_DATABASE_URL` | none | Postgres for sessions, settings and follow-ups. Without it they are in memory and a restart signs everyone out. In the deploy kit: the kit's Postgres, a `pmos_web` database and role the operator creates (`deploy/env.example`). |
| `PMOS_BOTS_URL` | `http://127.0.0.1:8750` | The bot runtime. |
| `PMOS_WEB_BOTS_TOKEN` | required | The runtime token for client `web`: at least 32 characters, and the same value as the `web=<token>` entry in the runtime's `PMOS_BOTS_TOKENS`. |
| `PMOS_TOOLSERVER_URL` | `http://127.0.0.1:8710` | The tool server, for approvals. |
| `PMOS_WEB_TOOLSERVER_TOKEN` | required | The tool server's service token (its `PMOS_SERVICE_TOKEN`): at least 32 characters, different from `PMOS_WEB_BOTS_TOKEN`. |
| `PMOS_CONTENT_ROOT` | `content` | The deployed content tree the admin screens read. Who may sign in, and their roles, come from the user database at the tool server (`PMOS_TOOLSERVER_URL`), not `team.yaml`. |
| `PMOS_WEB_STATIC_DIR` | none | The built app (`web/dist`). Without it only `/api` and `/healthz` answer. |
| `PMOS_WEB_PUBLIC_URL` | none | The origin browsers use, such as `https://pmos.example.com` (no path). Writes must come from it. Without it, `Origin` must match `Host`. |
| `PMOS_WEB_SESSION_IDLE_MINUTES` | `480` | Idle expiry. |
| `PMOS_WEB_SESSION_MAX_MINUTES` | `1440` | Expiry from sign-in. |

Related settings on other services:

| Where | Variable | What it is |
| --- | --- | --- |
| Runtime (`pmos-bots`) | `PMOS_BOTS_TOKENS` | Must list `web=<token>`. |
| Slack service | `PMOS_WEB_PUBLIC_URL` | An `https://` address. Set, the Slack cards link to the app; unset, they don't. |
| Slack service | `PMOS_WEB_URL`, `PMOS_WEB_BRIEF_TOKEN` | pmos-web's address as the Slack service reaches it, and the same token as pmos-web's `PMOS_WEB_BRIEF_TOKEN`. Both set: each daily brief fills the home page's brief panels (`docs/daily-brief.md`, "The web app's panels"); pmos-web also needs `PMOS_WEB_BRIEF=1`. In `deploy/.env`: `PMOS_WEB_BRIEF=1` and `PMOS_WEB_BRIEF_URL=http://web.pmos.internal:8770`. |

## Deploy it (the Spark)

The deploy kit (`deploy/`) knows the web app, **off by default**:

- **A Compose profile.** `web` is in the `web` profile, so `deploy/pmos-admin up` does not start
  it. `deploy/pmos-admin up --web` does, and `down`, `status`, `logs` and `restart` know it.
- **Entra sign-in, or a loudly named opt-in.** `up --web` refuses unless `deploy/.env` has
  Entra sign-in set (`PMOS_WEB_ENTRA_TENANT_ID`, `PMOS_WEB_ENTRA_CLIENT_ID`,
  `PMOS_WEB_ENTRA_CLIENT_SECRET` and `PMOS_WEB_PUBLIC_URL`; `docs/entra-sign-in.md`) or
  `PMOS_WEB_ENABLE_DEV_LOGIN_CLOSED_TEST_NETWORK_ONLY=1`. That second setting is for a **closed
  test network only**: with development sign-in, anyone who can reach the port signs in as any
  team member. Leave it `0` anywhere else.
- **Sessions in Postgres, if you create the database.** Blank `PMOS_WEB_DATABASE_URL` keeps
  them in memory. `deploy/env.example` has the commands that make a `pmos_web` database and role
  in the kit's Postgres (its init script runs only when the volume is first made); `pmos-admin
  backup` doesn't dump it.
- **A token like the others.** `deploy/setup.sh` generates `PMOS_RT_WEB_TOKEN` (`openssl rand
  -hex 24`, so 48 characters), and Compose lists it in the runtime's `PMOS_BOTS_TOKENS` as
  `web=…`. Run `deploy/setup.sh` again on an existing install to fill it in, then restart the
  runtime. The web app's tool server token is the existing `PMOS_SERVICE_TOKEN`; no new secret.
- **Static files in the image.** `deploy/Dockerfile` builds `web/` in its own stage and copies
  the result to `/usr/share/pmos/web`; Compose sets `PMOS_WEB_STATIC_DIR` to it. Rebuild with
  `deploy/pmos-admin build`.
- **Only through the reverse proxy.** The port is published on `127.0.0.1:8770`, like the
  console's, and the session cookie is `Secure`, so the app works only behind a TLS proxy on the
  Spark. Never publish the port on another address. Put the proxy's address in
  `PMOS_WEB_PUBLIC_URL` in `deploy/.env`; Compose passes it to `pmos-web` (for its origin check)
  and to the Slack service (for the cards' links), and blank means unset for both. The proxy
  must pass the request's `Host`, `Origin` and `Cookie` headers unchanged.
- **Health.** The container's health check is `GET /healthz`; `deploy/pmos-admin status` shows
  it while the service runs.

## Not ready for production

- **Development sign-in is still there.** With
  `PMOS_WEB_ENABLE_DEV_LOGIN_CLOSED_TEST_NETWORK_ONLY=1` anyone who can reach the port signs in
  as any member, so it is for a closed test network behind the proxy, never for the company
  network or the internet. Use Entra sign-in anywhere else.
- **Sessions and settings are in memory unless the database is set up.** Without
  `PMOS_WEB_DATABASE_URL`, a restart signs everyone out and resets what the app keeps for
  people.
- **No real identity in the deploy kit.** With development sign-in nothing ties a browser to a
  person but the handle chosen at sign-in.
- **A powerful token.** The backend holds a tool server token (`PMOS_SERVICE_TOKEN`) that can
  decide approvals for any user. It checks ownership itself before deciding, but the token is
  not limited to one person, so anyone who can run code in or reach the container can act for
  anyone.

Until those are fixed, keep `PMOS_WEB_ENABLE_DEV_LOGIN_CLOSED_TEST_NETWORK_ONLY` at `0` on the
production Spark.
