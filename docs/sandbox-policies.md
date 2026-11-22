# Sandbox policies (0f)

Every bot runs in its own NVIDIA OpenShell sandbox (D3). Its policy is generated from the bot's
definition and its role (D12), kept in git under `policies/`, and checked in CI. This page covers
the format, the rules, how to change a policy, and what has been confirmed against OpenShell
versus what still has to be verified on the Spark. The spec is section 7 of
[`bot-runtime-spec.md`](bot-runtime-spec.md).

- **Code:** `crates/policy` (`pmos-policy`).
- **Commands:**
  - `pmos policy generate [--root content] [--out policies]`;
  - `pmos policy check [--root content] [--policies policies] [--format text|json]`, which exits non-zero on any violation.
- **Files:** [`policies/`](../policies/README.md), including the review rule (POL-2).
- **Red team:** [`policies/redteam/`](../policies/redteam/checklist.md).

## Sources, and what is confirmed

Everything marked **confirmed** below was read from OpenShell's own repository,
[github.com/NVIDIA/OpenShell](https://github.com/NVIDIA/OpenShell), at `main` commit `2e5589b`
(2026-11-13):

| Topic | Source |
| --- | --- |
| Policy schema: fields, types, defaults, limits, unknown fields rejected | [`docs/how-it-works/policies/schema.mdx`](https://github.com/NVIDIA/OpenShell/blob/main/docs/how-it-works/policies/schema.mdx) and the serde types in [`crates/openshell-policy-schema/src/lib.rs`](https://github.com/NVIDIA/OpenShell/blob/main/crates/openshell-policy-schema/src/lib.rs) |
| Baseline paths, default deny | [`docs/how-it-works/policies/default-policy.mdx`](https://github.com/NVIDIA/OpenShell/blob/main/docs/how-it-works/policies/default-policy.mdx) |
| Network rules, MCP and REST inspection | [`docs/how-it-works/policies/network-rules.mdx`](https://github.com/NVIDIA/OpenShell/blob/main/docs/how-it-works/policies/network-rules.mdx) |
| Applying changes; what is live and what needs a restart | [`docs/how-it-works/policies/manage-policies.mdx`](https://github.com/NVIDIA/OpenShell/blob/main/docs/how-it-works/policies/manage-policies.mdx) |
| Policy advisor (off by default) | [`docs/how-it-works/policies/advisor.mdx`](https://github.com/NVIDIA/OpenShell/blob/main/docs/how-it-works/policies/advisor.mdx) |
| Providers, placeholders, endpoint binding, `stable_placeholder` | [`docs/how-it-works/providers/overview.mdx`](https://github.com/NVIDIA/OpenShell/blob/main/docs/how-it-works/providers/overview.mdx) and [`profiles.mdx`](https://github.com/NVIDIA/OpenShell/blob/main/docs/how-it-works/providers/profiles.mdx) |
| Denial log format | [`docs/observability/logging.mdx`](https://github.com/NVIDIA/OpenShell/blob/main/docs/observability/logging.mdx) |
| Non-root, `no_new_privs`, seccomp | [`docs/security/best-practices.mdx`](https://github.com/NVIDIA/OpenShell/blob/main/docs/security/best-practices.mdx) |
| `sandbox create --upload`, `sandbox exec` | [`docs/how-it-works/sandboxes/overview.mdx`](https://github.com/NVIDIA/OpenShell/blob/main/docs/how-it-works/sandboxes/overview.mdx) |

The published copy is at
[docs.nvidia.com/openshell](https://docs.nvidia.com/openshell/latest/reference/policy-schema.html).
It wasn't reachable from the build environment, so the repository docs above are what was read.

Before production, re-check against the OpenShell version installed on the Spark.
`crates/policy/src/openshell.rs` rejects unknown fields as OpenShell does, so a schema change
there shows up as a failing parse.

**Verify on the Spark** marks a value PM OS chose that depends on the Spark's image or on how the
harness behaves. The schema field it sits in is still confirmed. The full list is at the end.

## The files

`pmos policy generate` writes three kinds of file under `policies/`:

| File | Format | Used by |
| --- | --- | --- |
| `bots/<bot>.yaml` | OpenShell sandbox policy, `version: 1` | `openshell sandbox create --policy` and `openshell policy set` |
| `bots/<bot>.sandbox.yaml` | PM OS launch spec | the sandbox launcher: `--provider` and `--upload` flags |
| `providers/pmos-gateway.yaml`, `providers/pmos-worker.yaml` | OpenShell provider profiles | `openshell provider profile import` (once) |

OpenShell rejects unknown fields (confirmed), so the policy file can't carry PM OS metadata such
as which providers to attach or which content to upload. The launch spec holds that, and it is
generated and checked the same way.

The output is deterministic: keys are sorted, lists are sorted, there are no timestamps, and
strings a YAML 1.1 reader could misread (such as `"2025-11-25"`) are quoted. CI compares the files
byte for byte.

## The policy, field by field

The seeded Docs bot's policy is [`policies/bots/docs.yaml`](../policies/bots/docs.yaml).

### `filesystem_policy` (Landlock; applied at startup)

| Field | Value | Why | Status |
| --- | --- | --- | --- |
| `include_workdir` | `false` | `true` would make the whole working directory writable, including any content uploaded under it. | Confirmed field and default. |
| `read_write` | `/sandbox/workspace`, `/tmp`, `/dev/null` | The bot's own workspace. `/tmp` and `/dev/null` are OpenShell's baseline read-write paths, which it adds anyway to any policy with a network rule. `/tmp` is inside the sandbox, not the host's. | Field and baseline confirmed. The path `/sandbox/workspace` needs verifying on the Spark. |
| `read_only` | `/app /dev/urandom /etc /lib /proc /usr /var/log`, plus `/sandbox/skills/<skill>` for each of the role's skills (and, for a personal bot of `pm-assistant`, each persona's skills: "Personas" below), `/sandbox/master/AGENTS.md`, and `/sandbox/templates`, `/sandbox/reviewers` and `/sandbox/playbooks` | The system paths are exactly OpenShell's baseline read-only set. They are listed so the file says what the sandbox can read. Content is the role's skills, the bot's master context, and the team's templates, reviewer rubrics and playbooks (shared by every bot; none of them holds customer data); a personal bot also gets `/sandbox/master/people/<owner>/AGENTS.md` if its owner wrote one. | Field and baseline confirmed. The `/sandbox` layout needs verifying on the Spark. |

**Nothing else is mounted.** OpenShell has no host bind mounts in the policy. Content gets in
only by `--upload`, which copies into the sandbox's own filesystem, and the launch spec lists the
only uploads allowed. Bot B's files are never in bot A's sandbox, and paths that aren't listed
are inaccessible (confirmed).

### `landlock`

| Field | Value | Why | Status |
| --- | --- | --- | --- |
| `compatibility` | `hard_requirement` | The default, `best_effort`, runs the sandbox *without* the filesystem rules if Landlock fails to apply them. `hard_requirement` refuses to start instead. | Confirmed. The kernel must support Landlock ABI v3 (Linux 6.2 or later); check that on the Spark (checklist P-1). |

### `process`

| Field | Value | Why | Status |
| --- | --- | --- | --- |
| `run_as_user`, `run_as_group` | `sandbox` | OpenShell's non-root account. It rejects root and requires a numeric id to be between 1 and 4294967294. The runtime runs with no capabilities, with `no_new_privs`, and under seccomp. | Confirmed. |

### `network_policies` (applied live)

Anything not allowed is denied (confirmed). There are three rules, and each lists the worker
binary as the only binary allowed to connect.

| Rule (also its name in logs) | Endpoint | Inspection | Allowed | Credential |
| --- | --- | --- | --- | --- |
| `pmos_toolserver` | `toolserver.pmos.internal:8710`, `path: /mcp` | `protocol: mcp`, `enforcement: enforce`, `mcp.versions: [2025-03-26, 2025-06-18, 2025-11-25]` | `initialize`, `notifications/initialized`, `tools/list`; `tools/call` only for the role's tools by exact MCP name (`docs_read`, `docs_search`) | none: the per-task session token comes in the claim response |
| `pmos_gateway` | `gateway.pmos.internal:8740` | `protocol: rest`, `enforcement: enforce` | `POST /v1/chat/completions` | `credential_binding.provider: pmos-<bot>-gateway` |
| `pmos_runtime` | `bots.pmos.internal:8750` | `protocol: rest`, `enforcement: enforce` | `POST /v1/worker/claim`, and `POST /v1/worker/tasks/*/heartbeat`, `/events`, `/trace`, `/complete`, `/fail` | `credential_binding.provider: pmos-<bot>-worker` |

What each part does:

- **Runtime routes:** the worker claims tasks and reports on them (spec sections 3 and 4), and
  nothing else: no `GET /v1/tasks`, routines, notifications or bot listings, which belong to
  other clients. In OpenShell's REST path globs `*` matches exactly one path segment
  (confirmed), so `/v1/worker/tasks/*/heartbeat` admits one task id and nothing deeper. The
  worker token can claim only its own bot's tasks; the runtime enforces that, not the policy.

- **Tool calls:** the MCP tool allowlist is the same set the tool server gives the session
  (`role_tools`, with dots turned into `_` as `harness_tool_name` does). A role's tool set to
  `never` is still listed, so the tool server can refuse it and audit the refusal as `denied`
  (spec section 2).
- **Enforcement:** `enforcement: enforce` blocks a request the rules don't allow. OpenShell's
  default, `audit`, only logs the request and sends it anyway (confirmed).
- **Binary:** the binary is `/usr/local/bin/node`. Verify on the Spark; see the list at the end.
- **MCP methods and revisions:** OpenShell's own MCP example allows the same methods.
  OpenShell's default revision list is only `2025-11-25`, and a request with no
  `MCP-Protocol-Version` header is treated as `2025-03-26`, so all three 2025 revisions are
  listed (confirmed). Which revision the harness and tool server actually negotiate, and whether
  the client sends any other method, needs verifying on the Spark.
- **Always blocked:** OpenShell never allows loopback, link-local (including `169.254.169.254`)
  or control-plane ports, whatever the policy says. Exact internal hostnames may resolve to
  private addresses (confirmed).
- **Middleware:** `network_middlewares` is not used.

## Credentials

A sandbox holds three credentials, all of them its own bot's:

| Credential | How it gets in | Used for |
| --- | --- | --- |
| Gateway client token | provider `pmos-<bot>-gateway` (profile `pmos-gateway`), placeholder in `PMOS_GATEWAY_TOKEN` | `gateway.pmos.internal:8740` |
| Runtime worker token | provider `pmos-<bot>-worker` (profile `pmos-worker`), placeholder in `PMOS_WORKER_TOKEN` | `bots.pmos.internal:8750` |
| Tool-server session token | the claim response, one per task | `toolserver.pmos.internal:8710/mcp` |

**One token per bot, per service.** Each bot needs its own entry in the gateway's
`PMOS_GATEWAY_TOKENS` (for example `bot-docs=…`) and in the runtime's `PMOS_BOTS_TOKENS`
(`worker-docs=…`). The runtime lets a worker token claim only its own bot's tasks. Never give two
bots' providers the same value.

**How OpenShell handles the two providers (confirmed):**

- **Placeholders:** the sandbox sees only an opaque placeholder, `openshell:resolve:env:…`. The
  proxy replaces it with the real value just before forwarding a request, and only when:
  1. the network policy allows the binary and destination;
  2. the credential is bound to that host, port and path;
  3. the placeholder is in the `Authorization` header (both profiles use `auth_style: bearer`,
     which is what the gateway and runtime read).
- **Endpointless profiles:** both profiles are endpointless, so a credential resolves only where
  a policy names that exact instance with `credential_binding.provider`. That is how "only this
  bot's credentials" becomes a checkable line in the policy. A placeholder used anywhere else
  gets `credential_endpoint_mismatch`.
- **Unknown placeholders:** a placeholder from another sandbox, or an expired one, fails closed.
- **No other credentials:** there is no service or Slack-bot token in a sandbox. Credentials are
  never rewritten into bodies or WebSocket messages, never signed, and never sent uninspected.

### The tool-server session token (v1 decision, 2026-11-14)

The per-task session token comes in the claim response, as the runtime and harness already do
(spec sections 3 and 4). So the tool-server endpoint binds **no** provider, and `pmos policy
check` refuses one there (`cred-binding`). The reasons this is acceptable in v1:

- **Short-lived:** the token lives for one task.
- **Narrow:** it is scoped to that one bot, and the tool server refuses anything outside the
  bot's role.
- **Same channel:** it reaches the sandbox only over the bot's own, credentialed runtime
  connection.

Unlike the gateway and worker tokens, it is a real value inside the sandbox, not a placeholder.

**Future option:** a per-bot session provider (`pmos-<bot>-session`, endpointless, with
`stable_placeholder: true`), bound to the tool-server endpoint. The runtime would set its value
for each task with `openshell provider update` instead of returning it in the claim. The worker
would then see only a placeholder, and OpenShell would keep the token from being sent anywhere
but `/mcp`. OpenShell supports this (`stable_placeholder` exists for "a host service rotates the
value"); it needs runtime and harness changes, and a POL-2 review.

**Run once on the Spark:**

```sh
openshell provider profile lint   -f policies/providers/pmos-gateway.yaml
openshell provider profile import -f policies/providers/pmos-gateway.yaml --global
openshell provider profile import -f policies/providers/pmos-worker.yaml --global
```

**Then, per bot:**

```sh
openshell provider create --name pmos-docs-gateway --type pmos-gateway --credential PMOS_GATEWAY_TOKEN
openshell provider create --name pmos-docs-worker  --type pmos-worker  --credential PMOS_WORKER_TOKEN
```

`--credential <NAME>` with no value reads the token from that environment variable; never write
`--credential NAME=<value>`, which OpenShell's providers page says "exposes it in shell history
and the host process table". A provider is its own resource, attached to sandboxes and deleted
separately, so it outlives `openshell sandbox delete`. `pmos-admin sandbox <bot>` therefore runs
`openshell provider get <name>` first and, for a provider that exists, `openshell provider update
<name> --credential <NAME>` instead of creating it again; that is also how a rotated token
reaches it (`docs/runbooks/incidents.md`). From OpenShell's public docs
(`how-it-works/providers/overview.mdx`, read 2026-11-22); verify on a real host that `provider
get` fails only for a provider that doesn't exist.

The command names come from OpenShell's docs and its example profile headers. The docs show both
`openshell profile …` and `openshell provider profile …`; check which one the installed CLI uses.

## Personas: what the bot marketplace adds

The marketplace's bots (Chief, Sprout, Echo) are **personas**: roles a PM's own personal bot
runs as, chosen per ask, with no sandbox of their own (`docs/bot-marketplace-spec.md` §0 and §4).
So the generator reads one more input, `content/personas/*/PERSONA.md`, and uses it in one place:

- For a bot with `scope: personal` whose role is the personal role (`pm-assistant`,
  `PERSONAL_ROLE`), the skill folders uploaded read-only are the role's skills **and** the
  skills of every persona's role, offered or not (so offering or withdrawing a persona changes no
  policy). The header then lists `personas/*/PERSONA.md` among its sources.
- Nothing else depends on the catalog: the `tools/call` allowlist stays the personal role's
  tools, and the network rules, binaries, credentials, writable paths, process and Landlock
  settings are byte for byte what they were. Team bots and personal bots of any other role are
  untouched.

Why that holds: a persona's role may not have a tool the personal role doesn't
(`persona-exceeds-base`, a lint error), nor a higher data class (`persona-data-class`), nor
re-enable a write the personal role sets to `never` (`persona-action`); `pmos policy generate`
refuses content with lint errors. The tests in `crates/policy/tests/policies.rs` generate the
same content with and without a persona and assert that the only difference is the persona's
new skill folder in `filesystem_policy.read_only` and in the launch spec's uploads, and that a
persona reaching a new tool is refused before generation.

A catalog whose personas add no skill folder (today's Nudge alone: its role is the personal
role) leaves every policy file byte for byte as it was.

**Operations.** A pull request that adds a persona with a new skill regenerates every `pm-*`
policy in the same pull request (`policy-stale` fails CI otherwise), and the diff shows the new
read-only folders. That is a filesystem change, not a network change: it gets the content
repo's review plus a platform owner (POL-2 applies only if `pm-assistant`'s own tools change).
OpenShell applies filesystem rules only at startup, so after it merges, each `pm-*` sandbox is
recreated once.

## Creating a bot's sandbox

The launcher (`deploy/pmos-admin sandbox <bot>`) checks the bot name first (PM OS's
`valid_name`: lowercase letters, digits and single hyphens, at most 32 characters), reads
`policies/bots/<bot>.sandbox.yaml` and runs, for the Docs bot (`sandbox docs --dry-run` prints
it, without the tokens):

```sh
openshell sandbox create --name pmos-docs --from pmos-worker:local --policy policies/bots/docs.yaml \
  --provider pmos-docs-gateway --provider pmos-docs-worker \
  --upload content/master/AGENTS.md:/sandbox/master/AGENTS.md \
  --upload content/skills/docs:/sandbox/skills/docs
systemctl --user enable --now pmos-worker@docs.service
```

The worker is the systemd user unit `deploy/systemd/pmos-worker@.service`, installed for the user
whose OpenShell gateway runs. Its `ExecStart` is
`openshell sandbox exec -n pmos-%I --workdir /sandbox/workspace -- env … /usr/local/bin/node
/opt/pmos/harness/dist/main.js`, with `Restart=on-failure` and a start limit of five tries in ten
minutes. The instance name is made with `systemd-escape` (`pm-ana` → `pm\x2dana`) and the unit
reads it back with `%I`. It holds no token: the two credentials are the sandbox's providers.
`pmos-admin sandbox` and `resume` refuse to start it unless lingering is on
(`loginctl show-user <user> -p Linger` is `yes`), since without it the user's services stop at
logout and don't start at boot. `pause` disables and stops the unit, then stops the sandbox
(even when the unit step fails); it refuses to run as root, and exits 1, listing what is left
to do, when the unit isn't installed, can't be stopped or the sandbox can't be stopped.

The worker image (`deploy/Dockerfile.worker`) has a `sandbox` account (uid and gid 10100) and
ends with `USER 10100:10100`, so the policy's `run_as_user: sandbox` names an account that exists
and the image declares the numeric identity OpenShell's docs ask for.

What each part rests on (slice H0, `docs/openshell-any-host-spec.md` §3.7):

| Part | From OpenShell's docs | Verify on a real host |
| --- | --- | --- |
| `--from pmos-worker:<version>` (it was `--image`, which isn't documented) | `--from` takes "an explicit OCI image reference", built and tagged "with the container engine used by your local gateway" (`sandboxes/overview.mdx`) | that the local tag is taken without a registry |
| A numeric `USER` and a `sandbox` account | a Docker target "must declare an explicit numeric `USER UID:GID`"; "symbolic or implicit identities are rejected" (said for sandbox updates) | whether OpenShell runs the workload as the image's `USER`, as `run_as_user`, or makes the account itself (RT-3) |
| The worker under systemd, not `&` | `sandbox exec -n … --workdir …` runs a command in a running sandbox; the docs call it "one-shot" | that `sandbox exec` stays in the foreground for the worker's life, and that stopping it stops the worker inside |
| `After=`/`Wants=openshell-gateway.service`; lingering | the Linux package runs the gateway as the user service `openshell-gateway`; `sudo loginctl enable-linger $USER` keeps it after logout (`about/installation.mdx`) | the unit's name on the installed version; that the gateway and every worker come back after a reboot (E-5) |

None of these has run on a real OpenShell. If one differs, slice H5 changes it.

- `--upload` can't be combined with a trailing command yet, so the worker starts with
  `sandbox exec` (confirmed).
- Uploads honour `.gitignore` inside a git checkout (confirmed).
- Whether uploading into paths that Landlock makes read-only works at create time needs
  verifying on the Spark. If it doesn't, build each role's skills into its image instead and drop
  the uploads; the policy stays the same.

## The rules `pmos policy check` enforces

The check runs the rules on each file's contents and, separately, compares each file with what
`generate` writes. A hand edit therefore fails with the rule it breaks as well as with
`policy-stale`. A policy file for a bot the content doesn't define is checked as a bot with no
skills, tools or credentials, and is also reported as `policy-orphan`.

| Rule | Requires |
| --- | --- |
| `policy-parse` | The file is a valid OpenShell policy: YAML, known fields only, right types. |
| `policy-version` | `version` is 1. |
| `policy-missing` | Every bot has a policy and a launch spec, and every provider profile exists. |
| `policy-stale` | Each generated file is exactly what `pmos policy generate` writes. |
| `policy-orphan` | No generated file is left for a bot that no longer exists. |
| `content-invalid` | The content passes `pmos lint`, so policies can be generated. |
| `net-egress` | Egress only to `toolserver.pmos.internal:8710`, `gateway.pmos.internal:8740` and `bots.pmos.internal:8750`, each host on exactly its port: no other host or port, no wildcard, no `ports` list, no host-less IP range. Direct Slack, Jira, Confluence or support-docs hosts get a hint that those go through the tool server. |
| `net-inspection` | Every endpoint is inspected (`mcp` on `/mcp` for the tool server, `rest` for the gateway and runtime) and enforced; no `tls: skip`, no access preset. |
| `net-tools` | MCP allows only the four methods above, `tools/call` only for the role's tools by exact name, and neither `allow_all_known_mcp_methods` nor `strict_tool_names: false`. |
| `net-gateway-routes` | The gateway allows only `POST /v1/chat/completions`. |
| `net-runtime-routes` | The runtime allows only `POST /v1/worker/claim` and `POST /v1/worker/tasks/*/heartbeat`, `/events`, `/trace`, `/complete` and `/fail`, each exactly as written. `GET /v1/tasks`, `/v1/routines`, a wider glob such as `/v1/worker/**`, or another method all fail. |
| `net-binaries` | Only the worker binary may open connections. |
| `net-middleware` | No network middleware. |
| `fs-workdir` | `filesystem_policy` is present and `include_workdir` is false. |
| `fs-writable` | Writable: the workspace, `/tmp` and `/dev/null`, or paths inside them, only. Any content path, or a parent of one (such as `/sandbox`), is reported as making content writable. A path with `..` never counts as inside an allowed one. |
| `fs-readable` | Readable: the system paths, the role's skills (plus the personas' for a personal bot of `pm-assistant`) and the bot's master context, or paths inside them, only. Another role's skill, another PM's master file, other content and wider paths are each named. |
| `fs-landlock` | `landlock.compatibility` is `hard_requirement`. |
| `fs-uploads` | The launch spec uploads only the role's skills (plus the personas', as above) and the bot's master context, each to its read-only path and never under a writable one. |
| `cred-binding` | The gateway and runtime endpoints bind exactly this bot's own provider (`pmos-<bot>-gateway`, `pmos-<bot>-worker`), and the tool-server endpoint binds none. Another bot's provider is named as such. No credential rewriting, signing or uninspected credentials. |
| `cred-providers` | The launch spec attaches this bot's gateway and worker providers and nothing else (no session provider in v1). |
| `proc-identity` | `run_as_user` and `run_as_group` are `sandbox` (or a numeric id of at least 1000); never root. |
| `spec-parse` | The launch spec is valid: known fields only. |
| `spec-identity` | The launch spec names its own bot, role, scope, sandbox and policy, with the workspace as the working directory. |

The list lives in `pmos_policy::check::RULES`, and every rule has a unit test that fails it and
one that passes it. Fewer grants than generated, such as a missing rule or provider, is reported
only as `policy-stale`: it is a broken bot, not a leak.

## Changing a policy

1. **Change the source.** Edit the bot (`content/bots/<bot>/BOT.md`), its role, the catalog
   (`content/personas/`, for the skill folders of personal bots), or, for a platform-wide
   change, the constants in `crates/policy/src/lib.rs`. Never edit
   `policies/bots/*` by hand.
2. **Regenerate and check.** Run `cargo run -q -p pmos -- policy generate`, then
   `cargo run -q -p pmos -- policy check`, and commit the source and the regenerated files
   together.
3. **Review.** A network change needs approval from a second platform-owner PM who isn't the
   author (POL-2, in [`policies/README.md`](../policies/README.md)). That includes a new bot or
   new role tools, since both change `tools/call`.
4. **Apply on the Spark.**
   - **Network rules** apply live:
     `openshell policy set pmos-<bot> --policy policies/bots/<bot>.yaml --wait`. Exit status 0
     means loaded, 1 means rejected, 124 means timed out. Open connections close and reconnect
     under the new rules (confirmed).
   - **Filesystem, Landlock and process changes** apply only at startup. OpenShell rejects
     removing paths from a running sandbox, so delete and recreate the sandbox (confirmed).
   - **Afterwards,** run `policies/redteam/isolation.sh`.

Agents can't change policies themselves: the OpenShell policy advisor is off by default, and the
red team checks that it stays off (RT-2).

## Isolation tests

- **Here:**
  - `cargo test -p pmos-policy`: the generator, the YAML writer, and every checker rule, each
    with a passing and a failing case;
  - integration tests over a copy of the real content with a second, personal bot: bots see
    neither each other's content nor each other's credentials; hand edits, planted files,
    removed bots and invalid content are all caught.
- **CI:** runs `pmos policy check` next to the content lint.
- **On the Spark:** [`policies/redteam/isolation.sh`](../policies/redteam/isolation.sh) and
  [the checklist](../policies/redteam/checklist.md) check what OpenShell actually enforces:
  - bot A can't read bot B's files or processes, hold B's tokens, or use B's placeholders;
  - every blocked call is logged with its destination and the policy that blocked it.

  In OpenShell's log, a connection no rule matches shows `[policy:- …] [reason:no matching
  policy]`, meaning the sandbox's default deny, and a request refused by an endpoint's rules
  names the rule, such as `[policy:pmos_toolserver …]`. The script records the sandbox's active
  policy revision with each run.

## Verify on the Spark

Each of these is a PM OS choice, or an assumption about the image or the harness, that couldn't
be checked without a live OpenShell. Each one is also marked in the code comments
(`crates/policy/src/lib.rs`) or in the red-team script.

| What | Assumed | Check |
| --- | --- | --- |
| The worker's Node binary | `/usr/local/bin/node` (official Node image), by real path, not a symlink | P-2 in the checklist |
| Sandbox working directory and layout | `/sandbox` (seen in OpenShell's examples), with `/sandbox/workspace`, `/sandbox/skills`, `/sandbox/master` under it | RT-4 |
| Uploading into read-only paths | `sandbox create --upload` writes them before Landlock applies to the workload | RT-4 (can read its own content) |
| MCP revisions and methods | the harness and tool server use a 2025 revision, and send only `initialize`, `notifications/initialized`, `tools/list` and `tools/call` | E-1: a real task with no denials in the log |
| MCP over Streamable HTTP | OpenShell's MCP inspection passes the `POST`s the client sends to `/mcp` (and any `GET` or `DELETE` it sends) | E-1 |
| Requests to other paths on the tool server's port | refused, since the only endpoint there is `path: /mcp` | RT-9 (`/internal/call`) |
| Runtime path globs | `/v1/worker/tasks/*/heartbeat` matches a real task id (`tsk_<hex>`), and nothing with more segments | RT-9, E-1 |
| Node's `fetch` | goes through the sandbox proxy with no proxy settings | RT-8, RT-9 |
| CLI command names | `openshell provider profile import`, `policy set --wait`, `logs --source sandbox`, `settings get` | P-3, RT-2 |
| Creating from the local image | `sandbox create --from pmos-worker:<version>` takes a local tag | O-CANARY (`docs/openshell-any-host-spec.md` §3.2) |
| The workload's identity | OpenShell runs the worker as `sandbox` (uid 10100, the image's numeric `USER`) | RT-3, O-CANARY |
| The worker unit | `sandbox exec` stays in the foreground while the worker runs; OpenShell's gateway unit is `openshell-gateway.service`; the workers come back after a reboot | E-5 |
| Landlock ABI | kernel 6.2 or later on DGX OS | P-1 |

## Decisions on the first version's gaps (2026-11-14)

1. **The worker reaches the bot runtime.** The worker runs inside the sandbox and claims from
   `bots.pmos.internal:8750`. The policy allows only the worker routes (`pmos_runtime` above),
   with the bot's own worker token (`worker-<bot>` in `PMOS_BOTS_TOKENS`, provider
   `pmos-<bot>-worker`). The runtime lets that token claim only its own bot's tasks.
2. **The session token comes in the claim response** (see "The tool-server session token"
   above). The tool-server endpoint binds no provider. Rotating a per-bot session provider is
   the recorded future option.
3. **One gateway client token per bot:** `bot-<bot>` in `PMOS_GATEWAY_TOKENS`, held by
   `pmos-<bot>-gateway`.
