# The console: `pmos-console`

A small web console for the bots (D1, bot runtime spec §6). Slack stays the main surface:
PMs ask bots, create routines and decide approvals there. The console is where you look
things up. It has four screens and nothing else, so there's never a screen for one feature.

```
browser ──SSO proxy──► pmos-console :8760 ──► pmos-bots :8750 (client "console")
                                         └──► pmos-toolserver :8710 (approvals, service token)
                                         └──► tool server audit file (read-only, optional)
```

## The four screens

| Screen | What it shows | Actions |
| --- | --- | --- |
| **Bots** | Each bot's role, scope, owner and description; what a team bot may read (Jira projects, Confluence spaces, Slack channels); its tools; and each write tool's setting (`approve` or `never`) | none: change a bot by asking it, or by pull request |
| **Routines** | Each routine's prompt, bot, owner, schedule (cron and time zone), next and last run, and whether it's paused | **Pause**, **Resume**, **Delete** |
| **Approvals** | Pending writes from the tool server: when, whose, the tool, the bot or skill, the arguments, and a link to the task | none: approvals are decided on the Slack card, with a link to Slack |
| **Activity** | The 50 most recent tasks. A task's page shows its status, ask, checklist, result or failure reason, its events, its full trace (D6), its audit events, and a link to its Slack thread | none |

Pages are plain server-rendered HTML with one inline stylesheet: no frontend build and no
script. They follow the browser's light or dark setting and turn tables into cards at phone
width.

## Who sees what

| | Admin (`admin` in `team.yaml`) | PM |
| --- | --- | --- |
| Bots | every bot | team bots, and their own personal bots |
| Routines | everyone's, and may pause, resume or delete any | their own |
| Approvals | every member's pending items | their own |
| Activity and task pages | every task | tasks they asked for (`requested_by`) |

Someone else's task or routine answers `404`, so the console doesn't confirm it exists. The
console's runtime token may read and change everything, so these checks are made in the
console before any call: a PM's routine action is checked against their own routines first.

## Sign-in

- **Behind the Spark's SSO proxy:** with `PMOS_CONSOLE_TRUST_PROXY=1`, the proxy's
  `X-Forwarded-User` names the person. It may be a PM OS handle or the member's email.
  Without that setting the header is ignored.
- **The proxy proves itself on every request.** With `PMOS_CONSOLE_TRUST_PROXY=1`, a request
  counts only if it also carries `X-Pmos-Proxy-Secret` equal to `PMOS_CONSOLE_PROXY_SECRET`
  (at least 32 characters, different from both tokens). The two are hashed and compared in
  constant time. A request with the secret missing, wrong, or sent twice is `401`, and its
  `X-Forwarded-User` isn't read at all. The console logs only that a request was refused,
  never either header's value. The console won't start with proxy trust on and no secret,
  and says which variable is missing.
- **Development:** `PMOS_CONSOLE_DEV_USER=<handle>` makes every request that person. The
  console won't start with it unless `PMOS_CONSOLE_BIND` is a loopback address (`127.0.0.1`
  or `::1`; the default is `127.0.0.1:8760`). So it can't be used in the deploy kit, where
  the container listens on `0.0.0.0`: run the console on your own machine for that.
- Set exactly one of the two; the console won't start with both or neither.
- **The person must be in `team.yaml`.** It's re-read on every request, so removing someone
  takes effect at once. Nobody signed in is `401`; someone not on the team is `403`.

### What the reverse proxy must do

The proxy is the only thing that may set these two headers. On every request to the console it
must:

1. **Sign the person in** (SSO, or until then something like basic auth for the platform
   owners). Never forward an anonymous request with a user.
2. **Strip** any `X-Forwarded-User` and `X-Pmos-Proxy-Secret` the browser sent.
3. **Set both itself:** the signed-in person, and the secret from `deploy/.env`.

For example, with Caddy on the Spark (put the secret in Caddy's environment, not in the
Caddyfile; `forward_auth` stands for whatever does the sign-in):

```
console.<your domain> {
  # Strip what the browser sent (this runs before the sign-in and the proxying).
  request_header -X-Forwarded-User
  request_header -X-Pmos-Proxy-Secret
  # Sign in: the sign-in service's answer names the person.
  forward_auth <your SSO> { copy_headers X-Forwarded-User }
  # Add the secret.
  reverse_proxy 127.0.0.1:8760 {
    header_up X-Pmos-Proxy-Secret {env.PMOS_CONSOLE_PROXY_SECRET}
  }
}
```

Whatever proxy you use, check it with a request that sends its own `X-Forwarded-User` and
`X-Pmos-Proxy-Secret`: neither may reach the console. Then set `PMOS_CONSOLE_PUBLIC_URL` to the
proxy's origin.

**Upgrading an existing install:** run `deploy/setup.sh` again (it only fills in what's
missing, including `PMOS_CONSOLE_PROXY_SECRET`), give the secret to the reverse proxy as above,
and clear `PMOS_CONSOLE_DEV_USER` if it was set. Until then, `pmos-admin up --console` and the
console itself stop with an error naming the variable.

### The network

The deploy kit publishes the console on the host's `127.0.0.1:8760` only, for the reverse proxy
on the same machine. Inside its container it listens on `0.0.0.0:8760` (a published port needs
that), on the shared `pmos` network. Every container on that network, such as the bring-up
worker, can reach it directly, and so can anything on the host that can reach its loopback.
The proxy secret is what keeps them from naming a user. Keep it out of every bot's
environment and sandbox.

### What this does not fix

- **The console still holds powerful tokens:** the tool server's full service token (which can
  also decide approvals and link accounts) and a runtime token that may read and change
  everything. Anyone who gets the proxy secret, or code running in the console, can act as any
  member. The per-viewer checks are made in the console, not upstream.
- **The console is not on its own network.** Separating it would take a network shared only
  with `bots` and `toolserver` (and the console taken off `pmos`). That changes other
  services' settings, so it's left for a stack change.
- **The secret is shared, not per person.** It proves a request came through the proxy, not
  who sent it; the proxy's sign-in decides that. Real per-user sign-in, and the signed user
  assertion so that the runtime and tool server check the person themselves, are separate work
  (`docs/user-assertion.md`).
- **No rotation without a restart.** To change the secret, change it in `deploy/.env` and the
  proxy, then restart both.

## Forms

Pause, resume and delete are `POST` forms. Each one is checked twice before anything runs:
- **A form token** bound to the person: an HMAC of their handle under a key made at start-up.
  A restart invalidates open forms; reloading the page gives a fresh one.
- **Its origin:** `Sec-Fetch-Site` must not be another site, and `Origin` (or else `Referer`)
  must be the console. That's `PMOS_CONSOLE_PUBLIC_URL` when set, or else the request's
  `Host`. Behind a proxy that rewrites `Host`, set `PMOS_CONSOLE_PUBLIC_URL`.

A refused form is `403` and changes nothing. A done action redirects back to Routines with a
short notice.

Every page also carries a strict Content-Security-Policy (no script, no frames, forms only
to the console, and only the stylesheet by its hash), `X-Frame-Options: DENY`,
`Cache-Control: no-store` and `Referrer-Policy: same-origin`. Every value from the runtime,
the tool server or the audit file is HTML-escaped, including task text, trace bodies and
results.

## What it calls

| Call | For |
| --- | --- |
| `GET /v1/bots`, `GET /v1/bots/{name}` | Bots |
| `GET /v1/routines` (admin), `GET /v1/routines?owner=<handle>` (PM) | Routines, and checking an action is allowed |
| `PATCH /v1/routines/{id}` `{"enabled": false}` or `{"enabled": true}` | Pause, Resume |
| `DELETE /v1/routines/{id}` | Delete |
| `GET /v1/tasks?limit=50` (admin), `…&requested_by=<handle>` (PM) | Activity |
| `GET /v1/tasks/{id}`, `/events`, `/trace` | A task's page |
| Tool server `GET /internal/approvals?user=<handle>&status=pending` | Approvals: once for a PM, once per `team.yaml` member for an admin |
| The tool server's audit file | A task's audit events: lines whose `run_id` is the task id, at most 500 |

All runtime calls carry the `console` client token, and tool server calls the service token.
Each call times out after 10 seconds. If an upstream is down, the screen says which one, with
`502`. On a task page, a missing events list, trace or audit file only blanks that section.

**Shapes it accepts.** The console follows the spec's shapes and ignores fields it doesn't
show. Where the spec leaves a shape open, it accepts either form:
- **Lists:** bare (`[…]`) or wrapped (`{"items": […]}`).
- **`GET /v1/bots/{name}`:** `role` as a name or `{name}`; `reads` with snake_case or
  kebab-case keys (`jira_projects` or `jira-projects`); `tools` as a list of names; and the
  write settings as `actions` (also `action_settings` or `settings`), either a map
  `{tool: setting}` or a list of `{tool, setting}`.

## Running it

```sh
PMOS_CONSOLE_DEV_USER=ana \
PMOS_BOTS_TOKEN=<the runtime's console token> \
PMOS_TOOLSERVER_TOKEN=<the tool server's service token> \
cargo run -p pmos-console
```

| Variable | Default | Notes |
| --- | --- | --- |
| `PMOS_CONSOLE_BIND` | `127.0.0.1:8760` | |
| `PMOS_CONSOLE_TRUST_PROXY` | off | `1` to trust `X-Forwarded-User` from the SSO proxy |
| `PMOS_CONSOLE_PROXY_SECRET` | none | Required with `PMOS_CONSOLE_TRUST_PROXY=1`: the secret the proxy sends in `X-Pmos-Proxy-Secret`. At least 32 characters, different from both tokens. `deploy/setup.sh` generates it |
| `PMOS_CONSOLE_DEV_USER` | none | A handle in `team.yaml`; every request is that person. Development only, and only with a loopback `PMOS_CONSOLE_BIND` |
| `PMOS_BOTS_URL` | `http://127.0.0.1:8750` | The bot runtime |
| `PMOS_BOTS_TOKEN` | required | The `console` entry of the runtime's `PMOS_BOTS_TOKENS`. At least 32 characters |
| `PMOS_TOOLSERVER_URL` | `http://127.0.0.1:8710` | |
| `PMOS_TOOLSERVER_TOKEN` | required | The tool server's `PMOS_SERVICE_TOKEN`, which the approvals list needs. At least 32 characters, and different from `PMOS_BOTS_TOKEN` |
| `PMOS_CONTENT_ROOT` | `content` | Where `team.yaml` is. The console won't start if it doesn't parse |
| `PMOS_CONSOLE_AUDIT_LOG` | none | The tool server's `PMOS_AUDIT_LOG` file, mounted read-only. Without it, task pages say audit events aren't available |
| `PMOS_CONSOLE_SLACK_URL` | none | The workspace, such as `https://acme.slack.com`, for links to Slack and to a task's thread |
| `PMOS_CONSOLE_PUBLIC_URL` | none | The origin browsers use; forms must come from it |

- **Health:** `GET /healthz` answers `ok` with no sign-in.
- **Shutdown:** SIGTERM or Ctrl-C stops new requests and lets in-flight ones finish.
- **State:** none. The console stores nothing; everything is read when a page loads.

## Not yet

- **Audit events come from a file.** The tool server has no endpoint that reads its audit log
  back, so the console reads the JSON-lines file the tool server already writes. Each task
  page scans the whole file. A `GET /internal/audit?run_id=` on the tool server would replace
  both the mount and the scan.
- **The console holds the tool server's service token,** which may also decide approvals and
  link accounts, only to list approvals. A read-only token for `GET /internal/approvals`
  would narrow that.
- **An admin's approvals queue** makes one call per `team.yaml` member, because the tool
  server lists one PM's inbox at a time.
