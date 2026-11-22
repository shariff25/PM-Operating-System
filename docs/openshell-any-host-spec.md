# OpenShell on any Linux host: the bots off the Spark

Status: **design spec; not built.** The reviewer checks it before any slice starts (CLAUDE.md,
scope item c). Revised on 2026-11-22 after the reviewer's review of `9de1a65` ("sound after
changes"; §10 lists what changed). File references are to `claude/openshell-any-host-spec-r2`,
based on `claude/web-app` at `2544d01`.

**The owner's requirement, 2026-11-22:** "I want to deploy it in another platform apart from
spark", and, asked which cloud, "OpenShell on another platform generic implementation". So PM OS's
bots must be able to run in NVIDIA OpenShell sandboxes on hosts other than the DGX Spark, through
one host-independent implementation that is not tied to a single cloud. The Spark stays a
supported host.

**What has been verified, said first:** nothing in this repository has run on a real OpenShell
anywhere, the Spark included. Every policy file, launch command and red-team check was written
from OpenShell's documentation (`docs/sandbox-policies.md`, "Sources"). So this spec cannot be
"port what works on the Spark"; it makes the deployment generic and defines the checks that must
pass on a real host, the Spark or any other, before the docs may call that host verified. Every
statement about OpenShell's behaviour that this repo has not seen happen is marked **verify on a
real host**. The same holds for the fixes in slice H0: they are written from OpenShell's public
docs, and none of them counts as verified until the acceptance run (§6).

**Naming.** "Decision D1" to "D3" below are this spec's open decisions for the owner (§8).
`docs/bot-decisions.md` has its own D1 to D12; this spec writes those as "bot decision D2" and so
on.

## 0. Goal and non-goals

**Goal.** One deployment kit (`deploy/`) and one set of reviewed sandbox policies (`policies/`)
that run PM OS's bots in OpenShell sandboxes on:

- the DGX Spark (DGX OS, arm64, local models on its GPU), as today;
- any single Linux host with Docker that meets OpenShell's requirements: a VM on any cloud or an
  on-prem server, amd64 or arm64, with or without a GPU.

What a host needs is checked by a command before anything is installed and again at every start
(§3.2), and stated as pass or fail per requirement.

**Non-goals.**

- **No per-cloud code.** No AWS, GCP or Azure APIs, images, templates or managed services. A cloud
  VM is just a Linux host. The one place clouds differ is how the metadata check (H-METADATA,
  §3.2) reads the answer from `169.254.169.254`; it reads one address, with no SDK and no account.
- **No serverless container platforms** (§4, "Unsupported").
- **No multi-host PM OS.** One host runs the whole stack, as on the Spark. Kubernetes is Tier 2,
  designed in outline only (§4).
- **No model switching or fallback.** That is requirement i (CLAUDE.md): not started, not designed
  here. This spec only notes where a host without a GPU depends on it (§3.6).
- **No change to what a sandbox may reach.** The three PM OS endpoints, their routes, the MCP
  methods, the binaries, the credentials and the filesystem rules stay exactly as reviewed. The
  one policy change proposed (§3.5, pinning each of those three names to its own address)
  narrows, never widens.

## 1. What is known about OpenShell, and from where

Two sources, kept apart:

1. **This repo's own record** (`docs/sandbox-policies.md`), read from OpenShell's repository at
   `main` `2e5589b` (2026-11-13): the policy schema, default deny, credential placeholders,
   live versus start-up policy changes.
2. **Research for this spec** (OpenShell v0.1.9, 17 November 2026: the support matrix, README and
   manage-policies pages on docs.nvidia.com/openshell, reported by the conductor), and pages read
   for this spec from OpenShell's public repository on 2026-11-22 (links below).

| Topic | What the docs say | Source | Status |
| --- | --- | --- | --- |
| Host OS and architecture | Linux (Debian/Ubuntu) x86_64 and aarch64 supported; macOS Apple Silicon through Docker Desktop; Windows WSL 2 experimental. The Linux gateway needs glibc 2.28 or newer. | [`about/support-matrix.mdx`](https://github.com/NVIDIA/OpenShell/blob/main/docs/about/support-matrix.mdx) | Reported; verify on a real host |
| Kernel | Landlock ABI 3 or newer (Linux 6.2+) with Landlock enabled; seccomp "usable under the runtime's existing seccomp profile without added capabilities"; "A kernel version alone does not establish support"; a disabled Landlock LSM or a blocking seccomp profile makes launch fail closed. | the same | Reported; verify on a real host |
| Runtimes | Docker Engine or Desktop 28.0+ ("local development and single-machine gateways"); Podman 5.x rootless with cgroups v2; MicroVM (KVM on Linux); Kubernetes through the Helm chart. The research said Kubernetes 1.29+; the support matrix read on 2026-11-22 says "1.33 (tested)" and "Older cluster versions are untested". | the same; [`sandboxes/runtimes.mdx`](https://github.com/NVIDIA/OpenShell/blob/main/docs/how-it-works/sandboxes/runtimes.mdx) | Reported |
| Docker driver networking | "`supervisor_network_mode` defaults to `host`"; "The workload always retains `network=none`"; in bridge mode, "on native Linux, the gateway and host services must listen on bridge-reachable addresses". | `sandboxes/runtimes.mdx` | Reported; verify on a real host |
| Kubernetes | "The cluster CNI must enforce `NetworkPolicy` in every sandbox namespace. Without it, sandbox pods can bypass OpenShell network policy." | the same | Reported |
| Addresses a policy can never allow | "Network policy never authorizes an outbound endpoint whose destination is loopback, link-local, or unspecified", "including the cloud metadata address `169.254.169.254`". Private addresses are blocked by default, but "an endpoint with an exact hostname can still reach the private addresses that its hostname resolves to". | [`policies/network-rules.mdx`](https://github.com/NVIDIA/OpenShell/blob/main/docs/how-it-works/policies/network-rules.mdx); also `docs/sandbox-policies.md` | Confirmed in docs |
| `allowed_ips` | "When an endpoint sets `allowed_ips`, every address its hostname resolves to must fall within the list". | `network-rules.mdx` | Confirmed in docs; PM OS's schema mirror already has the field (`crates/policy/src/openshell.rs:119`). Whether a single address must be written `/32` or may be bare: verify on a real host (this spec writes `/32`) |
| DNS inside the sandbox | OpenShell answers DNS for rule hosts with placeholder addresses and resolves the real destination "through its trusted resolver"; no search domains; AAAA answers are empty. **Which resolver that is (the host's, with its `/etc/hosts`?) is not stated.** | `network-rules.mdx` | Unknown; verify on a real host |
| Gateway install | A systemd **user** service at `https://127.0.0.1:17670` with an mTLS bundle made automatically; `loginctl enable-linger` keeps it running after logout. | [`about/installation.mdx`](https://github.com/NVIDIA/OpenShell/blob/main/docs/about/installation.mdx), [`gateways/authentication.mdx`](https://github.com/NVIDIA/OpenShell/blob/main/docs/how-it-works/gateways/authentication.mdx) | Reported |
| Plaintext gateway | "Intended for local port-forwarding or gateways behind a trusted reverse proxy or tunnel." The research reported that unauthenticated mode "must not be exposed to shared or public networks". The page read on 2026-11-22 says it for the Helm overlay only. | `gateways/authentication.mdx` | Reported |
| Remote gateway over SSH | `openshell gateway start --remote user@hostname` appeared in search snippets only; the page returned 404, and the installation page has no such command. | — | **Unconfirmed: not used by this design** |
| Custom images | `--from` takes an OCI image reference; "the target must declare an explicit numeric `USER UID:GID`" (said for Docker updates). | [`sandboxes/overview.mdx`](https://github.com/NVIDIA/OpenShell/blob/main/docs/how-it-works/sandboxes/overview.mdx) | Reported; see §2 surprises 2 and 3 |
| Resources | No GPU, RAM or disk requirement is stated. | support matrix | — |
| Cloud | Not named, except Amazon Linux 2023+ for the glibc requirement. Serverless platforms are not documented. | support matrix | — |

## 2. Inventory: where the repo assumes the Spark

Classes: **(a)** truly Spark-only (local GPU model serving); **(b)** a host-specific value that
should become a setting or a generated, checked value; **(c)** wording only. "Fixed" means the
value is not host-specific after all and stays a constant. Line numbers are at `2544d01`.

### Deployment kit

| Where | What it assumes | Class | Change (slice) |
| --- | --- | --- | --- |
| `deploy/compose.yaml:1`, `:12-16` | "PM OS on the DGX Spark"; sandboxes reach the services on 127.0.0.1 "(see /etc/hosts in the install runbook)" | (b) and (c) | Pinned service addresses (§3.3); wording (H2, H6) |
| `deploy/compose.yaml:31-34`, `:291` | "The Spark's outbound proxy" | (c) | "This host's outbound proxy" (H6) |
| `deploy/compose.yaml:143-162` (`gateway`) | No `<<: *proxy`, no `PMOS_GATEWAY_REMOTE_KEY`, no `PMOS_GATEWAY_CLOUD_TEST`: the gateway container cannot reach a hosted model at all today, through a proxy or not | (b) | Added with `PMOS_MODEL_SOURCE` (§3.6, H4; surprise 11) |
| `deploy/compose.yaml:284-341` (`web`) | `PMOS_WEB_INTERNAL_BIND` not passed, so the server-to-server route is on the browser-facing listener | (b) | Passed on its own port (§3.8, H9) |
| `deploy/compose.yaml:364-402` | `vllm-default`, `vllm-extraction`: `nvcr.io/nvidia/vllm:25.10-py3`, the NVIDIA device reservation, GPU memory shares sized for the Spark | (a) | Stays in the `models` profile; used only with `PMOS_MODEL_SOURCE=local` (§3.6, H4) |
| `deploy/compose.yaml:404-416` | The pinned `pmos` network (`10.201.77.0/24`), no `ip_range` | (b), already a setting | Gains an `ip_range` for unpinned containers; holds the three pinned addresses (§3.3, H2) |
| `deploy/compose.evals.yaml:32-37`, `:142-146` | The eval stack's containers join the live `pmos_pmos` network and take whatever address Docker hands out | (b) | Dynamic addresses come from the `ip_range` only, so they can never take a pinned one (§3.3, H2) |
| `deploy/env.example:1`, `:168-173`, `:177` | "PM OS on the Spark"; "Models (verify on the Spark)": `PMOS_VLLM_IMAGE`, model ids, GPU shares; "if the Spark has an outbound proxy" | (a) for the model block, (c) otherwise | Model block under `PMOS_MODEL_SOURCE=local`; new host-profile block (§3.1, H4) |
| `deploy/setup.sh:2`, `:37-41` | "on the Spark"; warns when there is no NVIDIA runtime | (b) | Warn only when `PMOS_MODEL_SOURCE=local`; run the preflight first (H1, H4) |
| `deploy/pmos-admin:2` | "PM OS operations on the Spark" | (c) | H6 |
| `deploy/pmos-admin:608-635` (`add-bot`), `:637-671` (`sandbox`) | The bot name is used unchecked: in the values written to `deploy/.env` (`:624-625`), which `pmos-admin` then sources as shell (`:47-49`); in file paths (`:643-644`); in a `sed` pattern (`:650-651`) | (b), a bug on every host | Validated before use (§3.7, H0; surprise 12) |
| `deploy/pmos-admin:637-671` (`sandbox`) | `--image` (`:662`), `/usr/local/bin/node`, `/sandbox/workspace`, `*.pmos.internal` URLs; the worker started with `&` from the admin's shell (`:669`) | node path and layout: fixed; flag and supervision: bugs | `--from` and a supervised worker (§3.7, H0; confirmed in H5) |
| `deploy/gateway.yaml:1-3`; `config/gateway.yaml:4-10` | Upstreams are the Spark's vLLM servers; "prompts never leave the Spark" | (a) and (c) | A hosted-test example config beside it (§3.6, H4) |
| `deploy/Dockerfile:3` | "Built on the Spark itself (arm64)" | (c) | "Built on the host, for its own architecture" (H6). CI's deploy job already builds both images on `ubuntu-latest`, which is amd64, but CI isn't running jobs at present (CLAUDE.md, "Waiting on the owner") |
| `deploy/Dockerfile.worker:19-25` | Node at `/usr/local/bin/node`; `USER node` (`:24`), a name, while the policy asks for `run_as_user: sandbox` (`crates/policy/src/lib.rs:121`, `RUN_AS`) | node path: fixed; `USER`: a bug | A `sandbox` account with a numeric `USER` (§3.7, H0; surprise 3) |
| `.github/workflows/evals-gate.yml:5-73` | Runner labels `self-hosted, pmos-spark`; `PMOS_SPARK_DIR` | (b) | Label `pmos-host`, variable `PMOS_HOST_DIR` with `PMOS_SPARK_DIR` still read (H6). The runner is not registered yet, so renaming now costs nothing |
| `.github/workflows/ci.yml:51` | "the Spark's eval gate" | (c) | H6 |

### Policies and their generator

| Where | What it assumes | Class | Change (slice) |
| --- | --- | --- | --- |
| `crates/policy/src/lib.rs:41-53` | `toolserver.pmos.internal:8710`, `gateway.pmos.internal:8740`, `bots.pmos.internal:8750` | Fixed | Names and ports stay; each name gets its own pinned address (§3.3, §3.5) |
| `crates/policy/src/lib.rs:86-88` | `WORKER_BINARY = /usr/local/bin/node`, "Verify on the Spark" | Fixed | The path comes from PM OS's own worker image (`node:22-bookworm-slim`), which is the same on every host. Verify once per architecture (P-2), not per host |
| `crates/policy/src/lib.rs:92-104` | `CONTENT_MOUNT = /sandbox`, `WORKSPACE = /sandbox/workspace` | Fixed | OpenShell's and the worker image's layout, not the host's. Verify once per OpenShell version and driver (RT-4) |
| `crates/policy/src/lib.rs:28`, `:69`, `:81` | "verify on the Spark" | (c) | "verify on a real host" (H3) |
| `crates/policy/src/generate.rs:300`, `:382` | Header text of every generated file: "Values still to verify on the Spark", "Import once on the Spark" | (c), but changing it changes every committed policy file | One regeneration, in H3, together with the `allowed_ips` change so policies change once |
| `policies/bots/docs.yaml:6`; `policies/providers/*.yaml:4` | Generated wording | (c) | Regenerated by H3 |
| `policies/README.md:4`, `:12`, `:49`, `:116-121` | "Applying on the Spark" | (c) | H6 |
| `policies/redteam/checklist.md:3`, `:24`, `:51` | "Run it on the Spark"; P-1 "kernel 6.2 on DGX OS" | (c), P-1 becomes the preflight | H7 |
| `policies/redteam/isolation.sh:4`, `:21` | "RUN THIS ON THE SPARK" | (c) | "Run this on the host, never on a machine shared with other work" (H7) |

### Services

| Where | What it assumes | Class | Change (slice) |
| --- | --- | --- | --- |
| `crates/gateway/src/config.rs:581-599` (`is_local_host`) | "Local" means loopback, **any** private IPv4 address, link-local (`169.254.0.0/16`), IPv6 unique-local, any name without a dot, `*.pmos.internal`, or a `local-hosts` entry. It checks the text of the host, not what it resolves to | (b): on the Spark's LAN a private address was the company's; on a cloud VM it is the provider's network, where hosted model endpoints can sit on private addresses | "On this host" redefined (§3.6, H4; surprise 13) |
| `crates/gateway/src/config.rs:452-480` | A remote test upstream needs `PMOS_GATEWAY_CLOUD_TEST=1`; anything else must pass `is_local_host` | (b) | Also bound to `PMOS_MODEL_SOURCE` (§3.6, H4) |
| `crates/toolserver/src/config.rs:313-321`, `:377-391` | `PMOS_CONNECTORS` and `PMOS_MEMORY` are chosen without regard to where prompts go | (b) | Refused with `hosted-test` (§3.6, H4) |
| `crates/web/src/config.rs:67-70`, `:308-317`; `crates/web/src/app.rs:372-394` | `PMOS_WEB_INTERNAL_BIND` exists and moves `PUT /internal/brief/{handle}` to its own listener; `/healthz` and `/readyz` stay on the browser-facing listener as well | — | Used by the kit (H9); the proxy forwards neither health route (§3.8) |

### Docs and runbooks

| Where | What it assumes | Class | Change (slice) |
| --- | --- | --- | --- |
| `docs/runbooks/install.md:1`, `:10-12` | "Install PM OS on the Spark"; DGX OS with the NVIDIA container toolkit | (b) | "Before you start" per host profile, the preflight first (H6) |
| `docs/runbooks/install.md:114-132` (Stage 3) | vLLM image, GPU shares | (a) | Stage 3 has two paths: local models, or the hosted test model (H6) |
| `docs/runbooks/install.md:138-141` | `/etc/hosts`: `127.0.0.1 toolserver.pmos.internal gateway.pmos.internal bots.pmos.internal` | A bug on every host (surprise 1) | Removed in H0; replaced by `pmos-admin host-names apply` (§3.3, H2) |
| `docs/runbooks/install.md:158-160` | "Run it on the Spark only, never on a shared machine" | (c) | "Only on this host, and never on a host shared with other workloads" (H6) |
| `docs/runbooks/install.md:229-264` | Console "only behind a sign-in proxy on the Spark" | (c), plus the exposure rule (§3.8) | H6 |
| `docs/runbooks/README.md:1-4`, `:25`, `:34` | "one DGX Spark"; ports "on the Spark"; "on the GPU" | (c) and (a) | H6 |
| `docs/runbooks/upgrade.md:217-280` | Registering the Spark CI runner (`ARM64`, `spark-1`, `pmos-spark`) | (b) | Any architecture; label `pmos-host` (H6) |
| `docs/runbooks/backup-restore.md:28-47`; `incidents.md:68-70` | Off-Spark copies; "The Spark is gone" | (c) | "Off the host"; encryption of the copy (§5, H6) |
| `docs/runbooks/daily.md:13`, `:26` | GPU queue; "a second Spark" | (a) for the GPU text | Unchanged meaning; "a second host" (H6) |
| `docs/sandbox-policies.md:38`, `:42`, `:180`, `:299-306`, `:320`, `:330-347` | "Verify on the Spark" table and "Run once on the Spark" | (c), and the table becomes the host acceptance list (§6) | H3 |
| `docs/gateway.md:3-9`, `:165`, `:193`; `crates/gateway/src/lib.rs:2-6`, `usage.rs:3` | ADM-3: "prompts never leave the Spark"; "restricted data sent off the Spark is zero by construction" | (a): a policy decision, not a host fact | Kept; where restricted data may be processed off the Spark is decision D1 (§8); the hosted path stays testing-only (§3.6, decision O4) |
| `docs/toolserver.md:1078` | "the context window on the Spark's local model" | (c) | H6 |
| `README.md:5`, `:69`, `:86`; `docs/implementation-plan.md`; `docs/bot-decisions.md:4`, `:21`, `:32` (bot decision D2) | "runs on the team's DGX Spark with open-weight models only"; bot decision D2 "Bots are long-lived services on the Spark" | (c), except "open-weight models only", which is ADM-3 | Wording in H6; bot decision D2 and ADM-3 keep their meaning ("on PM OS's host"), subject to D1 |
| `content/context/decisions/2026-10-product-agent-platform.md:10-12` | The team's platform decision names the Spark | Content, not code | Not changed by this spec; the owner may want to amend it |

### Surprises found while making the inventory

1. **The `/etc/hosts` line in the install runbook cannot work as written, on the Spark or
   anywhere.** It maps the three names to `127.0.0.1`. OpenShell's docs say a policy "never
   authorizes an outbound endpoint whose destination is loopback", and that sandbox-local
   loopback is the sandbox's own, so `127.0.0.1` would be the sandbox itself. With the Docker
   driver's default `host` supervisor networking it might be the host's loopback instead, which
   the same rule still refuses. The three services publish their ports on `127.0.0.1` only
   (`deploy/compose.yaml`), so no non-loopback address on the host serves them today. H0 removes
   the line; §3.3 and H2 replace it, for every host, the Spark included.
2. **`pmos-admin sandbox` passes `--image` (`deploy/pmos-admin:662`); OpenShell's sandbox page
   documents `--from`.** The script already says "verify on the Spark: the OpenShell command names
   and flags (`--image` …)". H0 switches to the documented flag; the acceptance run confirms it.
3. **The worker image ends with `USER node` (`deploy/Dockerfile.worker:24`, a name); OpenShell's
   page says a Docker target "must declare an explicit numeric `USER UID:GID`"**, and the policy
   asks for `run_as_user: sandbox` (`RUN_AS`), an account the `node` image does not have. Whether
   OpenShell creates or maps it is not documented. H0 adds the account with a numeric `USER`; RT-3
   and O-CANARY confirm it on a real host.
4. **The worker is started with `&` from the operator's shell** (`deploy/pmos-admin:669`), and
   nothing turns on lingering. Nothing restarts it after a reboot or after the operator's SSH
   session ends, which conflicts with bot decision D2 ("After a reboot, in-flight tasks resume").
   Cloud VMs reboot for maintenance more often than an office box, but this is a bug on the Spark
   today. H0 fixes it (§3.7).
5. **OpenShell's gateway is a systemd user service** that stops at logout unless lingering is
   enabled. The install runbook only says "OpenShell: installed on the Spark".
6. **The research said Kubernetes 1.29+; the support matrix now says 1.33 tested.** Tier 2 uses
   the matrix.
7. **ADM-3 (the gateway refuses a model server off the machine outside testing) means a host
   without a GPU has no production model today.** The research assumed the owner's hosted
   OpenAI-compatible model makes a GPU-less host usable. It does, but only in the gateway's
   testing-only mode (`PMOS_GATEWAY_CLOUD_TEST=1`), which is for fake data. Using a hosted model
   for real work is a change to ADM-3 and belongs with requirement i (decision O4).
8. **CLAUDE.md item i says "OpenShell holds one credential per provider".** Today the gateway is
   a Compose service, not in a sandbox, so OpenShell cannot hold its upstream key: the key is
   `PMOS_GATEWAY_REMOTE_KEY` in `deploy/.env`. Moving that key into OpenShell is part of
   requirement i's design, not this one (§3.6).
9. **OpenShell's install command points at `ai-openshell/openshell`** on GitHub
   (`about/installation.mdx`), not `NVIDIA/OpenShell`. The runbook should cite NVIDIA's own
   install page rather than copy the command.
10. **There is no Vercel deployment in this repo.** The web app is built into the service image
    and served by `pmos-web` (`deploy/Dockerfile`, `docs/web-app-running.md`). See decision O5.
11. **The kit's gateway cannot use a hosted model at all.** `deploy/compose.yaml:143-162` passes
    the gateway neither the proxy settings, nor `PMOS_GATEWAY_REMOTE_KEY`, nor
    `PMOS_GATEWAY_CLOUD_TEST`, so the "existing proxy setting" the first version of this spec relied
    on never reached it. H4 adds them.
12. **Bot names are never validated in `pmos-admin`.** `add-bot` writes `worker-<bot>=<token>` into
    `deploy/.env`, which every later `pmos-admin` run sources as shell (`set -a; . "$ENV"`,
    `deploy/pmos-admin:47-49`), so a name with a newline or `$(…)` in it runs as the operator; a
    comma or `=` adds or overrides a token entry. `sandbox` puts the name in file paths and a `sed` pattern. H0 validates it.
13. **"Local" in the gateway includes any private address.** `is_local_host`
    (`crates/gateway/src/config.rs:581-599`) accepts every RFC 1918 address, link-local and IPv6
    unique-local, and a dotless name, without resolving anything. On a cloud VM, a hosted model
    reached through a private endpoint (an address in the VPC) passes as local, needs no
    `PMOS_GATEWAY_CLOUD_TEST`, and so would be allowed with live connectors. §3.6 closes this.

## 3. Design

### 3.1 The host profile: four settings in `deploy/.env`

The smallest mechanism is not a new file format. It is four settings in `deploy/.env`, written by
`setup.sh`, read by `pmos-admin` and the preflight, and **never read by the policy generator**.
`PMOS_MODEL_SOURCE` and `PMOS_EXPOSURE` are also passed to the services that enforce them
(§3.6, §3.8), so the refusals hold however the stack is started.

| Setting | Values | Default | What it decides |
| --- | --- | --- | --- |
| `PMOS_HOST_PROFILE` | `spark`, `linux` | `spark` when `setup.sh` finds DGX OS (`/etc/dgx-release`), else `linux` | Which preflight checks are required; wording in `status`. `spark` = `linux` plus the NVIDIA runtime and local models required |
| `PMOS_MODEL_SOURCE` | `local`, `hosted-test` | `local` on `spark`; `hosted-test` on `linux` with no NVIDIA runtime | `local`: the vLLM profile, NVIDIA runtime required. `hosted-test`: no vLLM; the gateway runs a hosted test upstream; fake data only, enforced by the services (§3.6) |
| `PMOS_EXPOSURE` | `private`, `public` | `private` | `public` means the host has a public address or sits on a network you don't control. It turns on the exposure checks and refusals in §3.8 |
| `PMOS_OPENSHELL_DRIVER` | `docker` | `docker` | The OpenShell compute driver. Only `docker` is Tier 1; `podman` and `kubernetes` are refused until verified (§4) |

Existing installs: `setup.sh` fills in missing values only, so a Spark install gets `spark`,
`local`, `private`, `docker` and behaves as today.

What is deliberately **not** a setting:

- **The three host names and ports, the routes, the MCP methods.** They are the reviewed network
  policy (POL-2). They are the same on every host because the services are PM OS's own.
- **The three pinned service addresses** (§3.3). They are constants in the policy crate and
  literals in `deploy/compose.yaml`, checked to be equal (§3.5).
- **The worker binary path and the `/sandbox` layout.** They come from PM OS's own worker image
  and OpenShell's layout, which are identical on every host. A setting would let `deploy/.env`
  change a reviewed policy, and would make the committed `policies/` files differ per host. If
  OpenShell or an architecture ever needs a different value, the constant changes in
  `crates/policy/src/lib.rs` for every host, under POL-2 review.

So **the policy files stay host-independent**: one reviewed set in the content repo works on the
Spark and on any Tier 1 host.

### 3.2 The preflight: `deploy/preflight.sh` (and `pmos-admin preflight`)

A script that checks a host, and prints pass, fail or warn per requirement with the measured
value. It needs only `bash`, coreutils and `python3` (already needed by `pmos-admin ledger`). It
changes nothing on the host.

```
deploy/preflight.sh [--profile spark|linux] [--model-source local|hosted-test]
                    [--exposure private|public] [--stage install|run|quick]
                    [--openshell] [--json] [--root <dir>]
```

**When it runs.** Not only at setup: a kernel or Docker update can turn Landlock off, change the
seccomp profile or reset a firewall rule, and nothing else would notice until a sandbox fails.

| Where | Stage | On a fail |
| --- | --- | --- |
| `setup.sh`, first | `install` (every host check; H-NAMES and H-METADATA report "not set yet") | stops |
| `pmos-admin up` (every time), `restore`, `add-bot` | `run` (every host check, with the stack's own ports counted as in use by the stack) | refuses with exit 2, before anything starts |
| `pmos-admin sandbox`, `resume` | `run` plus `--openshell` | refuses |
| The worker unit's `ExecStartPre` (§3.7), at every start and boot | `quick`: H-LSM, H-LANDLOCK, H-SECCOMP, H-DOCKER-SECCOMP, H-NAMES, O-GATEWAY | the worker doesn't start; `StartLimit*` stops it retrying; `status` shows the failed check |
| `pmos-admin preflight`, by hand | any | prints |

`docker compose up` run by hand skips `pmos-admin` and so the preflight; what must hold however the
stack is started is enforced by the services themselves (§3.6, §3.8), and the preflight only
explains failures early. `--root <dir>` reads `/proc`, `/sys`, `/etc` and command outputs from a
fixture directory instead of the host, so every check is unit-tested (§6).

| Id | Requirement | How it is measured | Fail when |
| --- | --- | --- | --- |
| H-ARCH | x86_64 or aarch64 | `uname -m` | anything else |
| H-OS | Debian or Ubuntu (DGX OS is Ubuntu-based) | `/etc/os-release` `ID`, `ID_LIKE` | other distributions: **warn**, not fail ("not in OpenShell's support matrix") |
| H-GLIBC | glibc 2.28+ (OpenShell's gateway) | `getconf GNU_LIBC_VERSION` | older |
| H-KERNEL | Linux 6.2+ | `uname -r` | older. Passing this alone proves nothing, so it never stands in for H-LANDLOCK |
| H-LSM | Landlock enabled | `landlock` in `/sys/kernel/security/lsm` | absent (a kernel built with Landlock but not listed in `lsm=` fails) |
| H-LANDLOCK | Landlock ABI 3+ | a `python3 -I` probe calling `landlock_create_ruleset(NULL, 0, LANDLOCK_CREATE_RULESET_VERSION)` (syscall 444 on both architectures), which returns the ABI | below 3, or the call fails. Not measurable: **fail**, with the reason |
| H-SECCOMP | seccomp available | `Seccomp:` in `/proc/self/status`; `CONFIG_SECCOMP_FILTER` where `/proc/config.gz` or `/boot/config-*` exists | absent |
| H-CGROUP | cgroups v2 | `/sys/fs/cgroup/cgroup.controllers` exists | v1 only (needed by Podman; recorded for Docker) |
| H-DOCKER | Docker Engine 28.0+, daemon reachable, Compose v2 | `docker version --format '{{.Server.Version}}'`, `docker compose version` | missing, older, or the daemon not reachable by this user |
| H-DOCKER-SECCOMP | Docker runs containers under its default seccomp profile | `docker info --format '{{.SecurityOptions}}'` contains `seccomp` | `seccomp` missing or `unconfined` |
| H-NVIDIA | NVIDIA container runtime | `docker info` lists an `nvidia` runtime | missing, **only** when `PMOS_MODEL_SOURCE=local` (fail) |
| H-PORTS | 8710, 8740, 8750, 8760, 8770 free on 127.0.0.1 | `ss -ltnH` | in use by anything other than this stack |
| H-SUBNET | the `pmos` network as pinned | `ip route`; at stage `run`, `docker network inspect pmos_pmos` (subnet, gateway, `ip_range`) | a route overlapping `PMOS_NETWORK_SUBNET` other than Docker's own `pmos` bridge; a live network whose subnet, gateway or `ip_range` differs from `deploy/.env` (the fix: `pmos-admin down`, then `up`, which re-creates it); an `ip_range` that contains a pinned address (§3.3). The refusal of a subnet other than the policy's constant belongs to H3 (§3.5) |
| H-NAMES | **the host's view**: the three names resolve on the host to their pinned addresses, and nothing maps them to loopback | `getent ahostsv4` for each name, on the host; the managed block in `/etc/hosts` | a wrong or loopback address. At stage `install` this reports "not set yet", which passes. It does **not** show what a sandbox gets: OpenShell resolves through "its trusted resolver", which may not be the host's (§1). The sandbox's view is O-REACH |
| H-METADATA | PM OS's containers cannot get the VM's cloud credentials | after install, from a container on the `pmos` network (`docker run --rm --network pmos_pmos`), with a 2 s timeout: the rule per answer below | per the rule below: **warn** on `private`, **fail** on `public` (§5) |
| H-EXPOSURE | nothing PM OS runs listens on a non-loopback address except the reverse proxy | `ss -ltnH`: 8710, 8740, 8750, 8760, 8770, 8771 and OpenShell's 17670 only on `127.0.0.1` or the `pmos` bridge | otherwise, **fail** on `public` |
| H-DEVLOGIN | development sign-in off | `PMOS_WEB_ENABLE_DEV_LOGIN_CLOSED_TEST_NETWORK_ONLY` | `1` with `PMOS_EXPOSURE=public` (fail) |
| H-SIGNIN | sign-in on a public host as decision D2 says | `PMOS_WEB_PASSWORD_LOGIN`, `PMOS_WEB_PASSWORD_REACH` (§3.8) | per D2's answer (fail) |
| H-ASSERT | `PMOS_ASSERTION_MODE=require` | `deploy/.env` (the same rule as `assertion_checks`) | anything else while a web or console token is set |
| O-CLI | OpenShell CLI installed (with `--openshell`) | `openshell --version`, `openshell status` | missing, or the gateway not reachable |
| O-GATEWAY | OpenShell's gateway is local and authenticated | the gateway endpoint the CLI uses (`openshell status`) is loopback or the Docker bridge, over TLS | a non-loopback plaintext endpoint |
| O-LINGER | the gateway and the workers survive logout and reboot | `loginctl show-user "$USER" -p Linger` is `yes`, and the OpenShell gateway's user unit is enabled | not set (fail) |
| O-CANARY | a sandbox actually starts under PM OS's settings | creates `pmos-preflight` from the worker image with a minimal policy (no network rules, `landlock: hard_requirement`, `run_as_user: sandbox`), runs `/usr/local/bin/node -e 'process.exit(0)'`, reads the `Landlock ruleset built` event, deletes the sandbox | the sandbox does not start, Landlock was not applied, or node isn't at that path |
| O-REACH | **the sandbox's view**: a sandbox reaches the three services by name | a second canary, `pmos-preflight-net`, whose policy allows only `GET /healthz` on the three names and ports (with each name's `allowed_ips` once H3 is in), runs a `node` one-liner that fetches each `/healthz` | any of the three doesn't answer `200`. This is the automated form of H-1 (§3.3) |

**H-METADATA, by answer.** The first version said "reachable: fail". On GCP and Azure the
metadata server answers containers on any VM and no VM setting stops it, so that check failed
every `public` host there. The check now asks what matters (can a container get the VM's cloud
credentials?) and never requests a credential or token endpoint itself, so nothing secret passes
through it:

| What the probe sees at `169.254.169.254:80` | Passes when | Otherwise |
| --- | --- | --- |
| No answer | always (on-prem, a cloud with metadata off, or the block below in place) | — |
| AWS (`PUT /latest/api/token` is the only call made; its answer, if any, is discarded unread) | the `PUT` gets no answer from the container: IMDSv2 with a hop limit of 1 | fail: "set the instance's metadata to IMDSv2 required with hop limit 1, or apply the block" |
| GCP (an answer carrying `Metadata-Flavor: Google`; one read of `instance/service-accounts/`, which lists account names only) | no service account is attached | fail: "detach the service account, or apply the block" |
| Azure (`400` without the `Metadata: true` header), or any other answer | never: whether a managed identity exists can only be learned by asking for a token, which the check won't do | fail: "apply the block" |

**The block, one generic fix for every cloud:** `pmos-admin host-firewall apply` (with `sudo`)
adds rules to Docker's `DOCKER-USER` chain that drop traffic from `PMOS_NETWORK_SUBNET` to
`169.254.169.254`, `168.63.129.16` and `100.100.100.200`, and from the network's IPv6 range (if
any) to `fd00:ec2::254`. It is idempotent and tagged with a comment so `check` can find it. A
Docker or firewall restart can drop it; the `run` stage re-checks it at every `up`. With it, every
row above becomes "no answer". The rules do not affect the host itself, so the cloud's own agent
keeps working. Which form a GCP or Azure VM's answer takes, and that the block holds, are
**verify on a real host** (RT-16).

O-CANARY and O-REACH are the only checks that prove the kernel, the runtime and the network
together are enough, which is what OpenShell's "a kernel version alone does not establish
support" asks for. The other kernel checks exist to say *why* a host fails before an hour of
installing. The command names they use (`sandbox create --from`, `logs --source sandbox`,
`sandbox delete`) are from OpenShell's docs: verify on a real host.

Exit status: `0` all pass (warnings allowed), `1` any fail, `2` the script could not run.
`--json` prints `{"id", "status", "measured", "expected", "hint"}` per check, so a run can be
attached to the acceptance record (§6).

### 3.3 How sandboxes find the three services, on any host

**The problem** (surprise 1): the services publish only on `127.0.0.1`, and loopback is never an
allowed destination.

**The design:**

1. **Pin each service's own address on the `pmos` network.** `deploy/compose.yaml` gives
   `toolserver`, `gateway` and `bots` fixed `ipv4_address`es: `10.201.77.10`, `.11` and `.12`,
   written as literals equal to the policy crate's constants (§3.5). Each service already listens
   on `0.0.0.0` inside its container, so it answers there without publishing anything new. The
   ports stay published on `127.0.0.1` too, for `pmos-admin`.
2. **Reserve a range for everything else.** Docker hands out addresses in order from the whole
   subnet unless told otherwise, and it will not start a container whose pinned address another
   container already holds. Today the eval stack's containers join `pmos_pmos`
   (`deploy/compose.evals.yaml:142-146`), as do H-METADATA's probe and any operator's
   `docker run --network pmos_pmos`; one of them can take `.10` and then the tool server can't
   start. So the network's IPAM gains `ip_range: ${PMOS_NETWORK_IP_RANGE:-10.201.77.128/25}`:
   unpinned containers get `.128` to `.254` only, and `.2` to `.127` are for pinned addresses
   (the three services, and a reverse-proxy container pinned under
   `PMOS_WEB_TRUST_PROXY_IN_NETWORK`, `deploy/env.example:128-131`). H-SUBNET fails an `ip_range`
   that contains a pinned address or isn't inside the subnet. The gateway `.1` is unchanged, so
   `PMOS_WEB_TRUSTED_PROXIES` stays right.
3. **Moving an existing install.** Compose does not change the IPAM of a network that exists.
   `pmos-admin up` compares the live network (`docker network inspect pmos_pmos`) with
   `deploy/.env`, as `password_web_checks` already does for the gateway
   (`deploy/pmos-admin:165-180`), and refuses on a mismatch with the fix:
   `deploy/pmos-admin down && deploy/pmos-admin up` (which re-creates the network; no eval stack
   may be running). On the Spark this is a one-time step at the upgrade that brings H2.
4. **The names resolve to those addresses through a managed block in `/etc/hosts`**, written by
   `pmos-admin host-names apply` (with `sudo`), between `# BEGIN pmos (managed by pmos-admin)` and
   `# END pmos` markers. It replaces only its own block and is idempotent. `pmos-admin host-names
   check` and `status` report a missing, stale or loopback entry, and `pmos-admin sandbox` refuses
   to start a sandbox while the check fails. Nobody edits `/etc/hosts` by hand, and the Spark gets
   the same block.
5. **Why this should reach the services:** with the Docker driver's default `host` supervisor
   networking, the sandbox's egress leaves from the host's network namespace, which has a route
   to the `pmos` bridge. The addresses are private, which OpenShell allows for an exact host name.
   **Verify on a real host:** that OpenShell's "trusted resolver" is the host's resolver (so the
   managed block is what it reads), and that the supervisor reaches a bridge address. H-NAMES
   only shows the host's view; O-REACH and check H-1 in §6 show the sandbox's.
6. **If OpenShell does not use the host's `/etc/hosts`:** stop and report before changing
   anything. The fallbacks, each needing its own review, are a host name that the resolver OpenShell
   does use can answer (for example a local DNS server bound to the `pmos` bridge), or OpenShell's
   `bridge` mode with `host_gateway_ip`. This spec does not pick one without seeing OpenShell
   behave.

Nothing in this changes the policy's hosts, ports or routes.

### 3.4 Each host's other values

| Value | Where it lives | Safe default |
| --- | --- | --- |
| Service addresses | constants in `crates/policy/src/lib.rs`, literals in `deploy/compose.yaml` (§3.5) | `10.201.77.10`, `.11`, `.12` |
| Unpinned range | `PMOS_NETWORK_IP_RANGE` (new) | `10.201.77.128/25` |
| OpenShell gateway endpoint | OpenShell's own CLI configuration (`openshell status`); PM OS never stores it | the local package's `https://127.0.0.1:17670`, mTLS |
| Outbound proxy | `HTTPS_PROXY`, `NO_PROXY`, `PMOS_BUILD_CA` (exist); now also passed to the gateway (§3.6) | none |
| Model source | `PMOS_MODEL_SOURCE` (§3.6) | per profile |
| Eval runner | runner label `pmos-host`, `PMOS_HOST_DIR` (§2) | `/opt/pmos` |
| Content and state paths | `PMOS_CONTENT_CHECKOUT`, `PMOS_STATE_DIR` (exist) | `/srv/pmos/…` |

### 3.5 The policy generator, Policy Admin and drift

**The generator takes no host input.** `pmos policy generate` keeps reading only the content
tree. The host profile is invisible to it, so no setting, file or screen on a host can widen a
sandbox. The fixed parts stay in `crates/policy/src/lib.rs`, reviewed as code under POL-2, and
Policy Admin keeps showing them as `fixed` (`docs/policy-admin.md`). Policy Admin gains no host
field.

**One change to the fixed part, which narrows (decision O6):** each of the three endpoints gets
`allowed_ips` with **its own address only**: `toolserver.pmos.internal` →
`[10.201.77.10/32]`, `gateway.pmos.internal` → `[10.201.77.11/32]`, `bots.pmos.internal` →
`[10.201.77.12/32]`, from new constants beside the host names (`TOOLSERVER_ADDR`,
`GATEWAY_ADDR`, `BOTS_ADDR`). The first version pinned all three to `10.201.77.0/24`, which
includes the host's own bridge address `.1`, Postgres, the docs server, the console, the web app
and every eval container; a name pointed at any of them would have passed. With one `/32` each,
a name that resolves anywhere but its own service (a missing or stale `/etc/hosts` block, a VPC
resolver that answers `*.internal`, a tampered entry, or two names swapped) is refused by
OpenShell, not sent on. A link-local address is already refused whatever the policy says.

The cost: a site that has to change `PMOS_NETWORK_SUBNET` (because its network already routes
`10.201.77.0/24`) needs a code change to the three constants with a POL-2 review, not just a
setting. That is the intended trade: the address a sandbox may reach is reviewed, never
configured.

**Drift, and how each check treats a profile:**

| Check | Reads | A host can make it fail by |
| --- | --- | --- |
| `pmos policy check` (CI and `pmos-admin lint`) | content and `policies/` only | nothing: it never sees the host. A new rule, `net-allowed-ips`, requires each endpoint's `allowed_ips` to be exactly its own `/32` constant (another service's address, a wider range, a missing list, or a second entry fails, each with a failing and a passing unit test, like every rule) |
| A unit test in `crates/policy` | `deploy/compose.yaml` | nothing on a host: it fails CI when the three `ipv4_address` literals differ from the constants |
| `pmos-admin preflight` / `status` / `host-names check` | `deploy/.env`, the host | names that resolve elsewhere, a live network unlike `deploy/.env`, a profile check that fails |
| `pmos-admin up` and `sandbox` | the same | `up` refuses when `PMOS_NETWORK_SUBNET` does not contain the three constants, or `PMOS_NETWORK_IP_RANGE` contains one (the policies would refuse every connection, or a service couldn't start); `sandbox` refuses while `host-names check` fails |

The subnet refusal in `up` is built in H3, with the constants it compares against; H2 only checks
that the subnet contains the compose literals. The `PMOS_NETWORK_SUBNET` setting stays (the web
app's trusted proxy checks use it), but a value that doesn't hold the constants is refused with a
message that names the code change and POL-2.

Applying H3's change is a network change: a second platform owner reviews it, every bot's policy
file is regenerated once (the header wording changes in the same commit), and on each host
`openshell policy set` applies it live.

### 3.6 The model on a host without a GPU, fake data, and where the key is held

| `PMOS_MODEL_SOURCE` | Gateway config | Needs | Data allowed |
| --- | --- | --- | --- |
| `local` | `deploy/gateway.yaml` (vLLM upstreams), `up --models` | NVIDIA runtime and a GPU large enough for the two models | real data, after Gate 1, on a host decision D1 allows |
| `hosted-test` | `deploy/gateway.hosted-test.yaml`: each role an `openai+https://<host>/<base>` upstream (the existing remote test upstream, `docs/gateway.md`), `PMOS_GATEWAY_CLOUD_TEST=1` | an OpenAI-compatible API and its key | **fake data only**, as defined below |

**"Fake data only", defined.** With `hosted-test`, every prompt leaves the host, so nothing real may
reach a prompt by any path:

| Source | Real when | Under `hosted-test` |
| --- | --- | --- |
| Jira, Confluence, Slack through PMs' tokens | `PMOS_CONNECTORS=live` | refused |
| Memory repos (team and personal) | `PMOS_MEMORY=github`, or GitHub client settings set | refused |
| The content library | the deployed checkout (`PMOS_CONTENT_CHECKOUT`) is the team's own content repo, which can hold files marked `sensitivity: restricted` (the committed seed library has none today; 20 of its skills are `data-class: "restricted"` and may read them) | refused unless the content root carries a `TEST-CONTENT` marker file (committed in that checkout, stating it holds no company data) **and** no indexed file is `restricted`; a content sync that would bring a `restricted` file is not applied and is recorded as a failed sync |
| The tool server's database (stored memory notes, drafts, sealed tokens, the user database) | it was ever used with `PMOS_MODEL_SOURCE=local` and live connectors | refused: the tool server records the model source in the database at its first start, and a database first started under `local` with live connectors refuses `hosted-test` ("this database holds real data") |
| Slack, as a place people type | `PMOS_SLACK_APP_TOKEN` and `PMOS_SLACK_BOT_TOKEN` set (a real workspace; a test workspace can't be told apart) | `pmos-slack` refuses to start |
| The public docs site | `PMOS_DOCS_URL` (the company's **public** product docs, `docs/docs-server.md:3-5`) | allowed |
| What people type into the web app | always possible | can't be checked by code: the web app shows a "test stack: fake data only; prompts go to a hosted model" banner on every page, and the pilot never runs on `hosted-test` |

**Where each refusal lives.** The first version put the `hosted-test` plus live-connectors refusal
in `pmos-admin up` only; `docker compose up`, `pmos-admin add-bot` (which runs `compose up -d
bots gateway`) and `pmos-admin restart` skip that script. The refusals now live in the services
and run at their start-up, however they are started:

- **Setting:** `PMOS_MODEL_SOURCE`, written by `setup.sh` and passed by `deploy/compose.yaml` to
  `gateway`, `toolserver`, `slack`, `bots` and `web` as `${PMOS_MODEL_SOURCE:?run deploy/setup.sh
  first}`, so Compose itself fails without it. Each service also requires it (no default), like
  `PMOS_CONNECTORS` today, so the choice is always deliberate.
- **The gateway** refuses to start:
  - with `local` and `PMOS_GATEWAY_CLOUD_TEST=1`, or a remote test upstream, or an upstream that
    isn't on this host (below): `gateway config: PMOS_MODEL_SOURCE=local, but upstream
    `<upstream>` is not on this host; prompts would leave it (ADM-3)`;
  - with `hosted-test` and `PMOS_GATEWAY_CLOUD_TEST` not `1`: `PMOS_MODEL_SOURCE=hosted-test needs
    PMOS_GATEWAY_CLOUD_TEST=1`.
  Its `/readyz` and `GET /v1/pmos/roles` report the source (they already report `cloud_test` and
  `remote-test`, `crates/gateway/src/app.rs:255-264`).
- **The tool server** refuses to start with `hosted-test` and any row above marked refused, with
  one error per cause, for example: `PMOS_MODEL_SOURCE=hosted-test sends every prompt off this
  host, so it allows fake data only: PMOS_CONNECTORS must be fake (it is live)`, or
  `… the content root has no TEST-CONTENT marker`, or `… <path> is marked restricted`.
- **`pmos-slack`** refuses to start with `hosted-test` and Slack tokens set.
- **The runtime (`pmos-bots`)** cross-checks before taking tasks: its readiness fails while the
  gateway's reported source differs from its own `PMOS_MODEL_SOURCE`, so two containers started
  with different values can't run work together.
- **`pmos-admin up`** says the same before anything starts, and skips the Slack profile, so the
  operator sees one clear message rather than a container that exits.

**"On this host", redefined in the gateway.** Today any private address counts as local (surprise
13). Under `local`, a production upstream is on this host only when its host is a loopback
address, or a name (without a dot, or under `.pmos.internal`) that the gateway resolves at
start-up to loopback or addresses inside `PMOS_NETWORK_SUBNET` only, which it passes as a new
setting. The gateway pins the addresses it resolved for that upstream in its HTTP client, so a
later DNS answer can't move it. Link-local and IPv6 unique-local addresses, and private addresses
outside the subnet, are no longer local. The `local-hosts` list in the gateway config stays as the
one explicit exception (a model server on the company's own LAN next to the Spark), but the
gateway accepts entries in it only with `PMOS_GATEWAY_LAN_UPSTREAMS=1`, which `pmos-admin up`
refuses on `PMOS_HOST_PROFILE=linux`: on a cloud VM the private network is the provider's. The
Spark's own config (`vllm-default`, `vllm-extraction` on the `pmos` network) passes unchanged.
This narrows ADM-3's enforcement and changes no reviewed behaviour on the Spark.

**The gateway's container settings (H4).** `deploy/compose.yaml`'s `gateway` service gains
`<<: *proxy` (`HTTPS_PROXY`, and a `NO_PROXY` that already lists `vllm-default`,
`vllm-extraction`, `postgres` and `.pmos.internal`), `PMOS_GATEWAY_CLOUD_TEST:
${PMOS_GATEWAY_CLOUD_TEST:-0}`, `PMOS_GATEWAY_REMOTE_KEY: ${PMOS_GATEWAY_REMOTE_KEY:-}`,
`PMOS_MODEL_SOURCE` and `PMOS_NETWORK_SUBNET`, and its config path follows the model source
(`gateway.yaml` or `gateway.hosted-test.yaml`). The gateway's HTTP clients already use the system
proxy and trust store (`crates/gateway/src/app.rs:127-139`). A proxy that intercepts TLS needs
its CA in the gateway's trust store at run time, not only at build time (`PMOS_BUILD_CA`):
**verify on a real host** behind such a proxy, and add a CA mount in H4 if it is needed.

**The hosted model's key:**

- **Where:** `PMOS_GATEWAY_REMOTE_KEY` in `deploy/.env` (mode `600`, made by `setup.sh`), passed
  only to the `gateway` container. It is never in `policies/`, the content repo, the
  `gateway.*.yaml` files, a command line or a log. The gateway already never logs it and strips
  upstream addresses from errors (`docs/gateway.md`).
- **What a sandbox sees:** nothing of it. A worker holds only its own gateway client token's
  placeholder (`pmos-<bot>-gateway`), and its policy allows only `POST /v1/chat/completions` on
  the gateway. The hosted API's host is not in any sandbox policy, and `net-egress` keeps it out.
- **Not in this spec:** OpenShell holding the provider key for the gateway's own egress (CLAUDE.md
  item i). The gateway is not in a sandbox today; putting it in one, or giving it an OpenShell
  provider, is part of requirement i's design.

Eval reports from `hosted-test` already say so (`GET /v1/pmos/roles` reports `remote-test`), so
they never count towards a bar measured on the production role models.

### 3.7 Starting and keeping the worker running

- **Validate the bot name first.** `add-bot`, `sandbox`, `pause`, `resume` and the unit's install
  check the name against PM OS's own rule (`crates/contract/src/bots.rs:144-153`,
  `valid_name`): lowercase letters, digits and single hyphens, not starting or ending with one,
  at most 32 characters: in `bash`, `[[ $bot =~ ^[a-z0-9]+(-[a-z0-9]+)*$ ]] && [ ${#bot} -le 32 ]`.
  Anything else is refused with exit 2 before the name reaches `deploy/.env`, a path, a `sed`
  pattern or a unit name. The `sed` lookups (`deploy/pmos-admin:650-651`) are replaced by an exact
  field match (`awk -F= -v k="worker-$bot" '$1==k{print $2}'`), so even a valid name is never a
  pattern.
- **The flag:** `pmos-admin sandbox` uses `--from`, which OpenShell's sandbox page documents,
  instead of `--image` (H0). The O-CANARY code and `pmos-admin sandbox` share one function, so
  they cannot drift apart. That `--from` takes a local image tag (`pmos-worker:local`) without a
  registry is **verify on a real host** (acceptance step 2).
- **A supervised worker:** a systemd unit, `deploy/systemd/pmos-worker@.service` (instance = bot),
  installed by `pmos-admin sandbox <bot>` as a user unit of the OpenShell gateway's user, in place
  of the `&`. The instance name is made with `systemd-escape` (`pmos-worker@$(systemd-escape
  "$bot").service`), and the unit uses `%I`, the unescaped name, never `%i`. It runs
  `openshell sandbox exec … /usr/local/bin/node /opt/pmos/harness/dist/main.js` in the foreground
  with `Restart=on-failure`, `RestartSec=10`, `StartLimitIntervalSec=600` and
  `StartLimitBurst=5` (so a worker that fails at once stops after five tries in ten minutes, and
  `status` says so, instead of restarting forever), `ExecStartPre=` the `quick` preflight (§3.2),
  and `After=` and `Wants=` the OpenShell gateway's unit. It needs lingering (O-LINGER), which
  `pmos-admin sandbox` checks and the runbook turns on with `loginctl enable-linger`. `pause` and
  `resume` stop and start the unit as well as the sandbox. No token is in the unit file: the
  worker's credentials are the sandbox's providers. That `sandbox exec` stays in the foreground for
  the worker's life (so systemd supervises the worker, not a short-lived client), and the name of
  OpenShell's user unit, are **verify on a real host**.
- **The image's user (H0):** `deploy/Dockerfile.worker` creates a `sandbox` group and user with
  fixed numeric ids, makes `/sandbox/workspace` theirs, and ends with `USER <uid>:<gid>` in
  numbers. That meets OpenShell's documented "explicit numeric `USER UID:GID`" and gives the
  policy's `run_as_user: sandbox` (`RUN_AS`) an account, with no change to the policy. Whether
  OpenShell uses the image's `USER`, the policy's `run_as_user`, or needs the account at all, is
  **verify on a real host** (RT-3, O-CANARY). The eval stack's workers use the same image as plain
  containers; their tests must still pass.

### 3.8 Network exposure and TLS on a host that isn't on a private LAN

The Spark sat on the company's private LAN. A cloud VM may have a public address. Which port may
be reached from where:

| Listener | Bound to | Reached by | On `PMOS_EXPOSURE=public` |
| --- | --- | --- | --- |
| Reverse proxy (nginx or Caddy), 443 | the host's address | browsers | the only public listener. TLS with a publicly trusted certificate; HTTP only to redirect |
| `/oauth/*` on the tool server | through the proxy only (`install.md`, Stage 6) | PMs' browsers | as today: only `/oauth/`, `404` for everything else |
| Web app 8770 | `127.0.0.1` | the proxy | sign-in as decision D2 says; development sign-in refused (H-DEVLOGIN). `/healthz`, `/readyz` and `/internal/` never forwarded; `/api/admin/` as decision D3 says |
| Web app internal 8771 (`PMOS_WEB_INTERNAL_BIND`) | the `pmos` network only, not published | `pmos-slack` (the daily brief) | never public |
| Console 8760 | `127.0.0.1` | the proxy | **never on a public listener** (decision O7): reachable only over a private network, VPN or SSH tunnel, behind its SSO proxy |
| Tool server 8710, gateway 8740, runtime 8750 | `127.0.0.1` and their pinned `pmos` addresses | `pmos-admin`; the sandboxes | never public |
| Postgres, docs, vLLM | the `pmos` network only | the services | never public |
| OpenShell gateway 17670 | `127.0.0.1` (and the Docker bridge, which the driver adds) | the CLI on the host; the sandboxes' supervisors | never public; mTLS kept on; plaintext mode not used |
| SSH 22 | the host | operators | from the operators' addresses only |

**Health routes are not forwarded.** The web app's `/readyz` names which of its dependencies are
down (`store`, `bot_runtime`, `tool_server`, `entra_link`, and warnings such as
`untrusted_proxy_refusals`; `crates/web/src/app.rs`, `readyz`), which tells an outsider what to
attack and when. `/healthz` adds nothing a browser needs. The proxy answers `404` for both;
`pmos-admin status` and the container health check reach them on `127.0.0.1` as today.

**The internal route off the public listener.** `PMOS_WEB_INTERNAL_BIND` exists in `crates/web`
(`config.rs:67-70`, `:308-317`): with it set, `PUT /internal/brief/{handle}` is served only on that
second listener (`app.rs:382-394`). The kit doesn't set it today (`deploy/compose.yaml:284-341`).
H9 sets `PMOS_WEB_INTERNAL_BIND: 0.0.0.0:8771` in the `web` service, publishes nothing for it, and
changes `PMOS_WEB_BRIEF_URL`'s documented value to `http://web.pmos.internal:8771`
(`deploy/env.example:151-154`). No code change is needed in `crates/web`. The second listener
also serves `/healthz` and `/readyz`, and the public one keeps them, so the proxy rule above still
applies.

**A full reverse-proxy example** (nginx, because its body-size and rate limits are built in;
Caddy's rate limiting needs a plug-in). It goes in `docs/runbooks/install.md` (H9) with the
address and certificate paths left for the operator:

```nginx
# One key per client: an IPv4 address, or an IPv6 /64 (one host's usual allocation).
map $remote_addr $pmos_client {
    ~^(?<p>[0-9a-f]+:[0-9a-f]+:[0-9a-f]+:[0-9a-f]+): $p;
    default                                          $remote_addr;
}
limit_req_zone  $pmos_client zone=pmos_signin:10m rate=10r/m;
limit_req_zone  $pmos_client zone=pmos_api:10m    rate=20r/s;
limit_conn_zone $pmos_client zone=pmos_conn:10m;

server {
    listen 80;
    listen [::]:80;
    server_name pmos.example.com;
    return 301 https://$host$request_uri;
}

server {
    listen 443 ssl;
    listen [::]:443 ssl;
    http2 on;
    server_name pmos.example.com;
    ssl_certificate     /etc/ssl/pmos/fullchain.pem;
    ssl_certificate_key /etc/ssl/pmos/privkey.pem;
    ssl_protocols TLSv1.2 TLSv1.3;

    client_max_body_size  2m;      # axum's default; pmos-web's own limits are lower on most routes
    client_header_timeout 15s;
    client_body_timeout   15s;
    keepalive_timeout     30s;
    limit_conn pmos_conn 20;
    limit_req_status 429;
    limit_conn_status 429;

    # Never forwarded (§3.8).
    location = /healthz    { return 404; }
    location = /readyz     { return 404; }
    location ^~ /internal/ { return 404; }

    # Sign-in: Entra's redirect and callback, password sign-in and its second step.
    location ^~ /api/auth/ {
        limit_req zone=pmos_signin burst=5 nodelay;
        include /etc/nginx/pmos-proxy.conf;
    }

    # The admin routes: decision D3.
    #   Answer "not on the public listener": keep this block.
    location ^~ /api/admin/ { return 404; }
    #   Answer "served publicly": replace it with
    #   location ^~ /api/admin/ {
    #       client_max_body_size 16m;   # content and policy proposals (proposal.rs, MAX_REQUEST)
    #       limit_req zone=pmos_api burst=20 nodelay;
    #       include /etc/nginx/pmos-proxy.conf;
    #   }

    location / {
        limit_req zone=pmos_api burst=40 nodelay;
        include /etc/nginx/pmos-proxy.conf;
    }
}

# /etc/nginx/pmos-proxy.conf
#   proxy_pass http://127.0.0.1:8770;
#   proxy_http_version 1.1;
#   proxy_set_header Host $host;
#   proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;   # docs/password-sign-in.md
#   proxy_set_header X-Forwarded-Proto https;
#   proxy_set_header X-Forwarded-User "";                          # never from a browser
#   proxy_set_header X-Pmos-Proxy-Secret "";
#   proxy_read_timeout 300s;                                       # long answers stream
```

The proxy's limits are a coarse first layer against floods. The real sign-in limits stay where
they are (the tool server's per-account and per-address counters, `docs/password-sign-in.md`), and
`PMOS_WEB_TRUSTED_PROXIES` stays the `pmos` network's gateway, since nginx on the host reaches the
published port from there. The `/oauth/` block for the tool server (`install.md`, Stage 6) takes
the same `limit_req zone=pmos_signin` and `client_max_body_size 64k`. The example is not tested
against a real nginx in this repo: H9 adds `nginx -t` on it in CI's deploy job if the runner has
nginx, and the acceptance run checks it from outside (RT-14).

**Sign-in on a public host (decision D2).** For the reviewer's recommended answer, Microsoft
(Entra) sign-in only unless the web app is behind a VPN or an address allow-list:

- A new setting, `PMOS_WEB_PASSWORD_REACH`: `any` (default on `private`) or `allow-list`.
- With `PMOS_EXPOSURE=public` and `PMOS_WEB_PASSWORD_LOGIN=1`, `pmos-web` refuses to start unless
  `PMOS_WEB_PASSWORD_REACH=allow-list` **and** `PMOS_WEB_PASSWORD_CLIENTS` lists at least one CIDR
  (the VPN's or office's ranges). With it, `pmos-web` refuses every `/api/auth/password/*` request
  whose client address (from the trusted proxy's `X-Forwarded-For`) is outside the list, with
  `403 password_sign_in_not_from_here`, before the tool server is asked; Entra sign-in is
  unaffected. The error at start-up: `PMOS_EXPOSURE=public with password sign-in: set
  PMOS_WEB_PASSWORD_REACH=allow-list and PMOS_WEB_PASSWORD_CLIENTS (a VPN or office range), or turn
  password sign-in off and use Microsoft sign-in (decision D2)`.
- `pmos-web` gets `PMOS_EXPOSURE` from `deploy/.env` (required, like `PMOS_MODEL_SOURCE`), so the
  rule holds however it is started; `pmos-admin up --web` and H-SIGNIN say it first.
- None of these settings exist yet: they are added in `crates/web` in H10, after D2 is answered.
  If the owner answers differently, H10 builds that answer instead.

**The admin routes on a public host (decision D3).** The accepted limit from the user database
review is that a broken-into web backend can sign as any user to the tool server
(`docs/user-database-spec.md:931`, "Compromised web backend"). On a private LAN the attacker first
needs the LAN. On a public listener, the routes that change users, content and policy
(`/api/admin/users`, `/api/admin/content/proposals` with 16 MiB bodies, `/api/admin/policy`) are
served by the same internet-facing process as every PM's routes. The two answers:

- **Not on the public listener** (the recommendation, §8): `pmos-web` gains
  `PMOS_WEB_ADMIN_BIND`, a third listener for `/api/admin/*` (with the static app, so the admin
  screens load), bound to an address reached only over the VPN or an SSH tunnel; with it set, the
  public listener answers `404` for `/api/admin/*`. With `PMOS_EXPOSURE=public` and the web app on,
  `pmos-web` refuses to start unless `PMOS_WEB_ADMIN_BIND` is set. The proxy block above returns
  `404` as a second layer. The setting does not exist in `crates/web` today: H10 adds it. This
  does not remove the accepted limit (a break-in through the public listener still gets the whole
  process and its tokens); it keeps the admin handlers' code, and their large bodies, off the
  internet.
- **Served publicly, limit accepted:** no code change; the proxy uses the commented block, and
  the runbook records that the owner accepted the limit for a public host.

**Firewall expectations** (the operator sets them; the preflight checks what it can see):

- **Inbound:** allow 443 (and 80 for certificate issuance, if used) from where PMs are, and 22 from
  the operators' addresses. Deny everything else, at the cloud's network firewall (security group
  or equivalent) as well as on the host.
- **Docker and host firewalls:** Docker's own documentation says ports it publishes bypass `ufw`
  rules ([docs.docker.com, "Packet filtering and firewalls"](https://docs.docker.com/engine/network/packet-filtering-firewalls/)).
  That is why every PM OS port is published on `127.0.0.1` only and H-EXPOSURE fails any other
  bind. Never change a publish to `0.0.0.0`.
- **Container addresses from other machines:** a VPC neighbour must not reach `10.201.77.0/24`.
  Docker Engine 28 is reported to block direct access to unpublished container ports from other
  hosts; verify on a real host (RT-15, from a second machine).
- **Outbound:** as the install runbook lists (GitHub, registries, the docs site, Slack, Atlassian,
  and for `hosted-test` the hosted API). Restricting outbound egress on the host is optional;
  sandboxes are already default-deny.

**The signed user assertion stays required**, exactly as today: `setup.sh` writes
`PMOS_ASSERTION_MODE=require`, `assertion_checks` runs before every `up`, `add-bot` and `restore`,
and the runtime and tool server refuse to start otherwise. The host profile has no way to change
this, and H-ASSERT reports it.

## 4. Tiers of support

| Tier | What | Status after this spec's slices | Notes |
| --- | --- | --- | --- |
| **Spark** | DGX Spark, DGX OS, arm64, local models | Same kit as Tier 1 with `PMOS_HOST_PROFILE=spark` | Not verified yet: the same acceptance run (§6) applies to it |
| **Tier 1 (the generic target)** | One Linux host (Debian or Ubuntu), Docker Engine 28+, OpenShell's Docker driver, amd64 or arm64, any cloud VM or on-prem server, with `local` (NVIDIA GPU) or `hosted-test` models | Designed and built here | The generic implementation the owner asked for. Which of these hosts may hold real data is decision D1 |
| **Tier 2 (later)** | Kubernetes 1.33 through OpenShell's Helm chart, with a CNI that enforces `NetworkPolicy` | Outlined only; a later slice (H8) unless the owner wants it for the pilot (decision O2) | The three names would be cluster Services (or a DNS rewrite), PM OS would move from Compose to manifests, preflight gains a cluster mode (CNI `NetworkPolicy` enforcement, OpenShell's namespaces). That is a second deployment kit, not a small slice |
| **Not offered** | Podman rootless, MicroVM, macOS Docker Desktop, WSL 2 | OpenShell lists them; PM OS does not test them | `PMOS_OPENSHELL_DRIVER` refuses `podman` until someone verifies it. macOS and WSL 2 are development set-ups, not a host for PMs' work |
| **Unsupported** | Serverless container platforms: Vercel, Cloud Run, Fargate, Fly.io, Railway and similar | Never | OpenShell needs a host it controls: a Docker daemon (or cluster) to create a container per sandbox, Landlock and seccomp on a kernel it can check, and a long-running gateway. These platforms run your one container on their kernel and their runtime, give no Docker daemon, and don't document Landlock. PM OS also needs Postgres with durable disk and always-on bots (bot decision D2). None of that is documented for them, so they are unsupported, not "untested" |

**What "supported" will mean.** The docs may call a host or tier **verified** only after the
acceptance run in §6 has passed on a real host of that kind, with the record committed
(`docs/hosts/<date>-<host-kind>.md`: OpenShell version, kernel, Docker version, architecture,
this repo's commit, the preflight JSON, the red-team script's output). Until then the runbooks say
"designed for; not yet verified". One passed run verifies that host kind (for example "Ubuntu
24.04 amd64 VM, Docker 28, no GPU, hosted-test"), not every cloud; a second architecture or
kernel family needs its own run. Today **no host is verified, the Spark included**, and none of
H0's fixes has been seen working on OpenShell.

## 5. Security analysis

### What changes off the Spark

| Threat | On the Spark | On a cloud VM or shared server | Handled by |
| --- | --- | --- | --- |
| The host is reachable from the internet | Private LAN | Possibly a public IP | §3.8 exposure table and proxy example; H-EXPOSURE, H-DEVLOGIN, H-SIGNIN; console never public; decisions D2 and D3; RT-14 from outside |
| Other tenants | A box the team owns | The cloud's other customers share the hypervisor; on-prem servers may be shared with other workloads | The hypervisor boundary is the cloud's. Don't run PM OS on a host shared with other workloads: the red team already says "never on a shared machine". Whether restricted data may sit at a cloud provider at all is decision D1 |
| The cloud metadata service (`169.254.169.254`; `metadata.google.internal`, which resolves to it; AWS's IPv6 `fd00:ec2::254`) | None | Present, and may hand out the VM's cloud credentials | **From a sandbox:** link-local is never allowed by policy (OpenShell docs), the policy allows only three exact names, AAAA answers are empty, and `allowed_ips` (§3.5) pins each name to its own address. RT-11 checks each address. **From PM OS's own containers**, which are not sandboxed and hold the vault key: they *can* reach it. H-METADATA (§3.2) fails a `public` host where a container could get credentials; the generic fix is the `DOCKER-USER` block, and on AWS IMDSv2 with hop limit 1 also passes. Give the VM no cloud role, or one with no permissions, in any case |
| Metadata services outside link-local (Azure's `168.63.129.16`, Alibaba's `100.100.100.200`) | None | Possible | Not in any policy: default deny. RT-11 includes both, and the block covers them for PM OS's containers. Whether OpenShell treats `100.64.0.0/10` as private is not documented; it doesn't matter while no rule allows it |
| Name resolution | `/etc/hosts` on a LAN box | VPC resolvers answer `*.internal` names on some clouds | `.internal` is reserved for private use, the managed block decides the three names, and each name's `/32` refuses any other answer |
| A hosted model on a private address | None | A provider's private endpoint inside the VPC | The gateway's "on this host" rule (§3.6) no longer counts private addresses outside the `pmos` subnet as local |
| Secrets at rest | `deploy/.env` (vault key), Postgres (sealed tokens), backups on the Spark's disk | Same files on a cloud disk, plus snapshots and images the cloud account can copy | Encrypt the disk (the provider's encryption at rest, or LUKS on-prem). Treat VM snapshots and images as secrets: they hold the vault key and the sealed tokens together. Restrict who in the cloud account may snapshot, attach or read the disk |
| Backups | "Copy it off the Spark" | Copies go to object storage | Encrypt each backup before it leaves the host (for example `age` to the platform owners' keys), and never copy `deploy/.env` in the same place as the database dump. Restore test as Gate 1 asks |
| The hosted model | None (local only) | `hosted-test` sends prompts off the host | Fake data only, defined and enforced by the services at start-up (§3.6); the key stays in `deploy/.env`; no sandbox reaches the hosted API |
| Maintenance reboots | Rare | Routine | The supervised worker and lingering (§3.7, already a bug on the Spark); the runtime already retries a running task once; the preflight re-runs at every start |
| A bot name used as shell, path or pattern | Same | Same | Validated before use (§3.7, H0) |
| The OpenShell gateway | Local | Local | Kept on loopback with mTLS; O-GATEWAY. Plaintext mode, which the docs say is for local or tunnelled use only, is not used |
| Who can run `docker` | Platform owners on one box | The same, plus cloud console access | Membership of the `docker` group is root on the host: give it to platform owners only. Cloud console and serial-console access are equivalent to root |

The trust boundary itself does not move: the sandbox is still the boundary between a bot and
everything else, the three services are still the only things a bot reaches, and every service
that holds a real secret is still outside the sandboxes.

### Red-team checklist additions (H7)

Added to `policies/redteam/checklist.md`; the script items to `isolation.sh`, the outside probe in
a new `policies/redteam/exposure.sh` run from another machine.

| Id | Check | Expected |
| --- | --- | --- |
| P-0 | `deploy/preflight.sh --openshell --json` on the host | Every check passes, O-REACH included; the JSON is attached to the record. Replaces P-1's "kernel 6.2 on DGX OS" |
| P-7 | `pmos-admin host-names check` | The three names resolve on the host to `.10`, `.11`, `.12`; no loopback entry anywhere for them |
| RT-11 | From A: `169.254.169.254`, `metadata.google.internal`, `[fd00:ec2::254]`, `168.63.129.16`, `100.100.100.200`, each on 80 and 443 | Each blocked by OpenShell and logged as RT-10 describes |
| RT-12 | From A: the host's own addresses on 22, 17670 (OpenShell's gateway), 2375 and 2376 (Docker's API), and the Docker socket path | Each blocked; the socket is not in A's filesystem |
| RT-13 | From A: the console and Postgres by their `pmos` addresses (`<console address>:8760`, `<postgres address>:5432`), the network gateway `10.201.77.1` on 8770, and a pinned service address on a port that isn't its own (`10.201.77.10:8740`) | Each blocked: only the three exact names on their own ports are allowed |
| RT-13b | Point a name at another address for a moment (`host-names apply` with a test override), then call it from A: first `toolserver.pmos.internal` → `10.201.77.11` (the gateway's address, which a `/24` would have allowed), then → `10.201.78.10` | Both refused by `allowed_ips`; restore the block afterwards (only if O6 is adopted) |
| RT-14 | `exposure.sh <host's public address>` from outside the host's network | Only 443 (and 80 if used) and 22 from the operators' range answer; 5432, 8710, 8730, 8740, 8750, 8760, 8770, 8771, 17670 do not; through 443, `/healthz`, `/readyz` and `/internal/brief/x` answer `404`; `/api/admin/users` answers as decision D3 says |
| RT-15 | From a second machine on the same private network or VPC: `10.201.77.10:8710` (with a route added on that machine towards the host, if the operator can) | No answer |
| RT-16 | From a container on the `pmos` network: H-METADATA's probe, and a plain TCP connect to `169.254.169.254:80` | On a `public` host: H-METADATA passes; with the block applied, no answer at all |
| E-5 | Reboot the host | The OpenShell gateway, the stack and every bot's worker come back without anyone logging in (the units' `ExecStartPre` preflight passes); a Slack question is answered |

## 6. How to verify without a Spark

### In CI and in this build environment (no OpenShell)

| What | How | Slice |
| --- | --- | --- |
| Bot-name validation | `scripts/test-pmos-admin.sh`: `add-bot` and `sandbox` refuse `../x`, `a b`, `x$(id)`, a name with a newline, `x=y`, `a,b`, `-a`, `a--b`, `A`, a 33-character name, each with exit 2 and `deploy/.env` unchanged; `docs` and `pm-ana` pass | H0 |
| The worker unit | `systemd-analyze verify` on `deploy/systemd/pmos-worker@.service`; the instance name for `pm-ana` is made with `systemd-escape`; the command `sandbox` would run, printed with `--dry-run`, uses `--from` and no `&` | H0 |
| The worker image's user | `docker build` of `Dockerfile.worker`; `docker run --rm pmos-worker:local id` prints the numeric `sandbox` ids; `docker inspect` shows a numeric `Config.User`; the eval stack's workers still pass their tests | H0 |
| The preflight's every check, pass and fail | `scripts/test-preflight.sh`: fixture directories under `deploy/test/preflight/` (an Ubuntu 24.04 amd64 host, a DGX OS arm64 host, a 5.15 kernel, Landlock absent from `lsm`, an ABI-2 probe result, cgroups v1, Docker 27, no NVIDIA runtime with `local`, port 8710 taken, a public bind, dev sign-in on `public`, a live network unlike `deploy/.env`, an `ip_range` holding `.10`, each H-METADATA answer shape with and without the block); each asserts the exact status and exit code. Run in CI's `deploy` job | H1 |
| The preflight runs at every start | `scripts/test-pmos-admin.sh`: `up`, `restore`, `add-bot`, `sandbox` and `resume` run the right stage and refuse on a failing fixture | H1 |
| The Landlock probe itself | run on the CI runner and here: it prints an ABI or a clear failure, never a crash. Its result is not asserted (it depends on the runner's kernel) | H1 |
| The service refusals | `cargo test`: the gateway's config tests (`local` with `CLOUD_TEST=1`, a remote upstream, a `10.x` upstream outside the subnet, link-local, a dotless name resolving outside the subnet, `local-hosts` without `PMOS_GATEWAY_LAN_UPSTREAMS`; `hosted-test` without `CLOUD_TEST`; a missing `PMOS_MODEL_SOURCE`); the tool server's config tests (`hosted-test` with live connectors, `PMOS_MEMORY=github`, no `TEST-CONTENT`, a `restricted` file, a database first started under `local` with live connectors); `pmos-slack` with Slack tokens; the runtime's readiness on a mismatch. Each with its passing case | H4 |
| The refusals hold without `pmos-admin` | a stack test: `docker compose up` with `hosted-test` and `PMOS_CONNECTORS=live` leaves the tool server exited with its error; `pmos-admin restart toolserver` the same | H4 |
| The gateway's container settings | `docker compose config` shows `HTTPS_PROXY`, `NO_PROXY`, `PMOS_GATEWAY_CLOUD_TEST`, `PMOS_GATEWAY_REMOTE_KEY` and `PMOS_MODEL_SOURCE` on `gateway`; a stack test with a local mock of the hosted API behind a test proxy | H4 |
| The managed hosts block | `scripts/test-pmos-admin.sh` gains cases on a temporary hosts file: apply is idempotent, keeps other lines, replaces a stale block, refuses a loopback override, and `check` fails on each wrong state | H2 |
| Compose | `docker compose config -q` with the pinned addresses and `ip_range`; a container started on the network first gets an address in the range; CI's existing bring-up smoke test (it doesn't use sandboxes) and the eval stack still pass | H2 |
| The internal route and the proxy example | `docker compose config` shows `PMOS_WEB_INTERNAL_BIND`; a stack test: the brief `PUT` works on 8771 and `404`s on 8770; `nginx -t` on the example if CI has nginx | H9 |
| The policy change | `cargo test -p pmos-policy`: `net-allowed-ips` fails and passes (another service's `/32`, a `/24`, a missing list, two entries); the compose literals equal the constants; the generated files carry `allowed_ips`; `pmos policy check` green on the regenerated `policies/`; `up` refuses a subnet without the constants | H3 |
| Sign-in and admin routes on `public` | `cargo test -p pmos-web`: start-up refusals and the allow-list (D2), the admin listener and the public `404` (D3), each for the answer the owner gave | H10 |
| The workspace's checks | the full set in CLAUDE.md, unchanged | every slice |

None of this shows what OpenShell enforces. It shows that PM OS's side does what this spec says.

### On a real host with OpenShell (the acceptance run)

The first non-Spark host (decision O1). All with fake data and the `hosted-test` model, so no real
token or data is needed. Each step's output goes into the host record (§4).

1. **Before installing anything:** `deploy/preflight.sh --profile linux --model-source hosted-test
   --exposure public --json`. Every host check passes.
2. **Install OpenShell** from NVIDIA's install page; enable lingering;
   `deploy/preflight.sh --openshell`. O-CLI, O-GATEWAY, O-LINGER and O-CANARY pass. Record whether
   `--from` took the local image (surprise 2), how the canary's identity was set (surprise 3), and
   whether `sandbox exec` stays in the foreground.
3. **Stack, Stages 1–2 of the install runbook** with `hosted-test`: `status` all `ok`; the smoke
   test passes through the bring-up worker. Then the refusals: set `PMOS_CONNECTORS=live` and run
   `docker compose up -d toolserver` by hand; the tool server must exit with its error.
4. **Names:** `pmos-admin host-names apply`; P-7 passes; O-REACH passes. **H-1, the key unknown:**
   from the Docs bot's sandbox, a heartbeat on a made-up task id reaches the runtime and gets its
   `404` (RT-8's control). If it doesn't, stop at §3.3 step 6.
5. **Sandboxes:** `pmos-admin sandbox docs`, a personal bot and `pm-redteam`; the smoke test
   passes through the Docs bot's sandbox (E-1: no denials in its log).
6. **Red team:** `isolation.sh` both ways; RT-1 to RT-16 and P-0 to P-7 pass; the "verify on a
   real host" table in `docs/sandbox-policies.md` (today's "Verify on the Spark" table) filled
   in, with anything that differed.
7. **Reboot** (E-5).
8. **Write the record** and open a pull request with it; the reviewer session checks it before the
   docs call that host kind verified.

The same run on the Spark, with `--profile spark --model-source local --exposure private`, is
what verifies the Spark, H0's fixes included.

## 7. Slices, in build order

One agent, one worktree, one branch per slice (CLAUDE.md). "Before a host" means it can be built
and fully tested here; its claims about OpenShell are still checked in the acceptance run.
"Before D1–D3" means it doesn't wait for the owner's decisions in §8.

| # | Slice | Files | Tests | Model | Before D1–D3? | Before a host? |
| --- | --- | --- | --- | --- | --- | --- |
| H0 | **Spark fixes that don't depend on the owner's decisions** | `deploy/pmos-admin` (`add-bot`, `sandbox`, `pause`, `resume`: name validation, exact-field token lookup, `--from`, the unit instead of `&`, the lingering check), `deploy/systemd/pmos-worker@.service` (new: `systemd-escape`d instance, `%I`, `Restart=on-failure`, `StartLimit*`; `ExecStartPre` added once H1 exists), `deploy/Dockerfile.worker` (`sandbox` account, numeric `USER`), `docs/runbooks/install.md` (the `127.0.0.1` hosts line removed, with a "stop: sandboxes can't reach the services until H2" note; lingering), `docs/sandbox-policies.md` ("Creating a bot's sandbox") | §6 H0 rows | Strongest (shell injection, the sandbox's identity) | Yes | Yes to build; each fix is from OpenShell's public docs and unverified until the acceptance run |
| H1 | **Preflight** | `deploy/preflight.sh` (new), `deploy/test/preflight/**` (fixtures), `scripts/test-preflight.sh` (new), `.github/workflows/ci.yml` (deploy job: parse and run it), `deploy/pmos-admin` (`preflight` and `host-firewall` commands; the stages at `up`, `restore`, `add-bot`, `sandbox`, `resume`), `deploy/setup.sh` (run it first, stop on fail), the unit's `ExecStartPre` | §6 H1 rows | Strongest (it decides what may be called supported) | Yes (H-SIGNIN's rule for `public` lands with H10) | Yes; O-* checks are written now, proven in the acceptance run |
| H4 | **Model source and fake data, enforced by the services** | `crates/gateway` (`PMOS_MODEL_SOURCE`, "on this host" with resolution and pinning, `PMOS_GATEWAY_LAN_UPSTREAMS`), `crates/toolserver` (the `hosted-test` refusals, `TEST-CONTENT`, the restricted-file check at start and sync, the database's recorded source), `crates/slack` (refuse tokens), `crates/bots` (readiness cross-check), `crates/web` (the test banner), `deploy/compose.yaml` (gateway: `<<: *proxy`, `PMOS_GATEWAY_CLOUD_TEST`, `PMOS_GATEWAY_REMOTE_KEY`, `PMOS_MODEL_SOURCE`, `PMOS_NETWORK_SUBNET`, config path; `PMOS_MODEL_SOURCE` to the other four), `deploy/env.example` (host-profile block; model block under `local`), `deploy/setup.sh` (detect DGX OS, fill the four settings, NVIDIA warning only for `local`), `deploy/pmos-admin` (`up`'s early messages, `status` lines), `deploy/gateway.hosted-test.yaml` (new, example hosts only, no key), `docs/gateway.md` | §6 H4 rows | Strongest (security refusals, cross-crate) | Yes (keeps ADM-3 as it is; O4 and D1 only widen it later) | Yes |
| H2 | **Pinned addresses and managed host names** | `deploy/compose.yaml` (`ipv4_address` for three services; `ip_range`), `deploy/env.example` (`PMOS_NETWORK_IP_RANGE`), `deploy/pmos-admin` (`host-names apply`/`check`, the live-network check, `sandbox` refuses on a failed check, `status`), `scripts/test-pmos-admin.sh`, `docs/runbooks/install.md` Stage 4 step 1 | §6 H2 rows | Strongest (it decides what sandboxes reach) | Yes | Yes to build; H-1 needs a host |
| H9 | **Public exposure, decision-free part** | `deploy/compose.yaml` (`PMOS_WEB_INTERNAL_BIND`), `deploy/env.example` (`PMOS_WEB_BRIEF_URL` on 8771), `docs/runbooks/install.md` (the nginx example, health routes not forwarded, firewall), H-EXPOSURE's 8771 | §6 H9 rows | Medium | Yes (the admin block ships as "not public" until D3 is answered) | Yes |
| H3 | **Policy: `allowed_ips` per name and wording** (needs decision O6) | `crates/policy/src/lib.rs` (`TOOLSERVER_ADDR`, `GATEWAY_ADDR`, `BOTS_ADDR`; "verify on a real host"), `generate.rs` (emit `allowed_ips`; header wording), `check.rs` (`net-allowed-ips` in `RULES`), `crates/policy/tests/policies.rs` (and the compose-literals test), regenerated `policies/**`, `deploy/pmos-admin` (`up` refuses a subnet without the constants), `docs/sandbox-policies.md` (rule table, "verify on a real host" table), `docs/policy-admin.md` (the fixed list); the web Policy Admin's fixed facts if it lists endpoint fields (`crates/web/src/app/policy.rs`, `crates/policy/src/admin/`) | §6 H3 row; persona tests still show only skill-folder differences | Strongest (cross-crate, POL-2 network change) | Yes (waits for O6 only) | Yes to build and review; applied on hosts after the acceptance run's H-1 |
| H10 | **Sign-in and admin routes on a public host** (needs D2 and D3) | `crates/web` (`PMOS_EXPOSURE`, `PMOS_WEB_PASSWORD_REACH`, `PMOS_WEB_PASSWORD_CLIENTS`, `PMOS_WEB_ADMIN_BIND`, per the answers), `deploy/compose.yaml`, `deploy/pmos-admin` (`up --web`), `deploy/preflight.sh` (H-SIGNIN), `docs/web-app-running.md`, `docs/password-sign-in.md` | §6 H10 row | Strongest (authentication) | **No** | Yes |
| H11 | **Where real data may run** (needs D1) | per D1's answer: `deploy/env.example` and `deploy/pmos-admin` (a `PMOS_HOST_PLACE=company|cloud` declaration; `up` refuses live connectors where D1 forbids them, and warns when H-METADATA's probe finds a cloud on a host declared `company`), `crates/toolserver` (a data-class ceiling if D1 is "internal only"), `docs/gateway.md` and the runbooks | per D1's answer | Strongest | **No** | Yes |
| H7 | **Red-team additions** | `policies/redteam/checklist.md`, `policies/redteam/isolation.sh` (RT-11 to RT-13b), `policies/redteam/exposure.sh` (new; RT-14), wording | `bash -n`; `shellcheck` if CI has it; a dry run that prints the probes without OpenShell | Strongest | Yes (RT-14's admin-route line follows D3) | Yes to write; runs only on a host |
| H6 | **Docs and wording** | `docs/runbooks/install.md` (per-profile "Before you start", preflight first, two Stage 3 paths, OpenShell install and lingering), `README.md`, `docs/runbooks/README.md`, `upgrade.md` (runner label `pmos-host`, any architecture; the one-time network re-creation), `backup-restore.md` (encrypted off-host copies), `incidents.md`, `daily.md`, `deploy/compose.yaml`, `deploy/pmos-admin`, `deploy/Dockerfile` comments, `policies/README.md`, `.github/workflows/evals-gate.yml` (label, `PMOS_HOST_DIR` with `PMOS_SPARK_DIR` still read), `docs/toolserver.md:1078`, `docs/gateway.md` wording (ADM-3's meaning kept) | the docs lint, if any; CI's workflow syntax | Lighter | Yes, except the lines that state D1–D3's answers | Yes |
| — | **Acceptance run on the first host** (§6) | `docs/hosts/<date>-<kind>.md` (new) | the run itself | Operator with the conductor; reviewer checks the record | Yes (fake data, `hosted-test`) | Needs the host |
| H5 | **Launch confirmation** | whatever the acceptance run's step 2 showed: `deploy/pmos-admin` (`sandbox` flags if `--from` differs), `deploy/systemd/pmos-worker@.service` (if `sandbox exec` doesn't stay in the foreground, or OpenShell's unit has another name), `deploy/Dockerfile.worker` (if OpenShell maps the user differently), `docs/sandbox-policies.md` | `scripts/test-pmos-admin.sh` (`--dry-run`); `systemd-analyze verify`; E-5 on the host | Medium | Yes | After the acceptance run's step 2 |
| H8 | **Tier 2, Kubernetes** | a separate spec first | — | Strongest | — | Only if decision O2 says so |

**Order.** H0 first: it fixes bugs that affect the Spark today, and later slices build on its
`sandbox` function and unit. Then H1 and H4 in parallel (H1 is shell and fixtures; H4 is mostly the
crates, and both touch `deploy/pmos-admin`: the conductor merges H1 first). H2 after H4 (both edit
`compose.yaml`). H9 after H2 (both edit `compose.yaml`). H3 once the owner has answered O6, in
parallel with H2 and merged after it (its `up` refusal compares against H2's literals). H10 and
H11 once the owner has answered D2 and D3, and D1. H7 and H6 after H2, H3 and H9 (and H10 and H11
for the lines that describe them), since they describe them. Then the acceptance run, then H5.

**Before the owner answers D1 to D3:** H0, H1, H4, H2, H9, H3 (with O6), H7 and H6 (except the
answer-specific lines), and the acceptance run. **Before a real host exists:** every slice can be
built and its PM OS side tested; only the acceptance run and H5 need the host, and no fix in H0
counts as verified until then.

Each batch goes to the reviewer session as the security gate, as usual.

## 8. Open decisions for the owner

Neither the reviewer nor the conductor may answer these.

1. **O1. The first real host to verify on.** Recommendation: **an Ubuntu 24.04 LTS amd64 VM**
   with Docker Engine 28+, no GPU, on whichever cloud or server the company already has an account
   on, run as a closed test with fake data and `hosted-test`. Amd64 because the Spark is arm64, so
   the two runs together cover both architectures; no GPU because that is the case the generic
   path adds. Then the same run on the Spark.
2. **O2. Tier 2 (Kubernetes) for the pilot?** Recommendation: **no.** A 20-PM pilot fits one host
   (the gateway's queue, not the host, is the limit), and Kubernetes is a second deployment kit
   (§4). Revisit after the pilot if a second host is needed.
3. **O3. Must the local-model path work off the Spark, on other NVIDIA GPUs?** Recommendation:
   **not as a verified path for now.** The Compose services would run on any NVIDIA host with the
   container toolkit, but the models and memory shares are sized for the Spark's GB10, and the
   vLLM image's other-architecture build is unverified. Offer it as "designed for; not verified"
   until someone runs the acceptance run on such a host with `local`. Whether such a host may hold
   real data is D1.
4. **O4. A hosted model for real work on a host without a GPU?** Today ADM-3 says no: prompts
   never leave the host. Recommendation: **keep ADM-3** and keep `hosted-test` fake-data only,
   enforced (§3.6), until requirement i is planned; decide ADM-3 together with requirement i
   (which providers, which data classes may go to which provider, where the keys live).
5. **O5. The web front end on Vercel, with this back end?** The repo has no Vercel deployment:
   `pmos-web` serves the built app and is also its back end, holding its runtime and tool-server
   tokens and the keys that sign user assertions. Recommendation: **keep the web app on the PM OS
   host behind the reverse proxy.** A Vercel front end would need `pmos-web`'s API reachable from
   the internet with cross-site sessions, and would be a separate security review.
6. **O6. Pin each of the three names to its own address in the policy (`allowed_ips`, one `/32`
   each)?** Recommendation: **yes** (§3.5). It narrows every policy, fails closed on a wrong or
   swapped name, and costs a code change for the rare site that must move the subnet.
7. **O7. The console on a public host.** Recommendation: **never on a public listener**; reachable
   over a private network, VPN or SSH tunnel only, still behind its SSO proxy.
8. **O8. Worker supervision with systemd.** The reviewer confirmed that the unsupervised worker is
   a bug on the Spark today, against bot decision D2 ("After a reboot, in-flight tasks resume"),
   which the owner already decided. So H0 builds the systemd unit without waiting. What stays open
   is only whether the owner prefers another supervisor. Recommendation: **systemd user units**
   (§3.7), since OpenShell's own gateway is one.
9. **D1. May restricted data be stored or processed at a cloud provider at all?** What the repo
   records of the PRD: it "defines an internal agent workspace for the product team, running on one
   DGX Spark" (`docs/implementation-plan.md:4`), with "local vLLM routes only (ADM-3)" (`:116`) and
   a "Restricted data sent off the Spark" metric (`:118`); the gateway "keeps prompts on the
   Spark" (`docs/gateway.md:7-8`) and counts that metric as "zero by construction" (`:193`). The
   PRD's own text is not in this repository, so its exact line could not be cited. This spec's
   first version allowed real data with `local` models on any host, a cloud VM with a GPU
   included: the prompts stay on that VM, but the VM, its disk, snapshots and backups are at the
   provider.
   - **(a) No: real data only on hosts the company owns** (the Spark, on-prem servers); cloud
     VMs run `hosted-test` with fake data only. Consequence: the generic implementation serves
     cloud hosts for testing only; the pilot stays on the Spark or an on-prem server; H11
     enforces it through a `PMOS_HOST_PLACE` declaration that `up` checks, which code can't
     prove (H-METADATA's probe can only warn when a host declared `company` looks like a cloud).
   - **(b) Internal data yes, restricted no.** A cloud VM may run `local` models with live
     connectors, but the tool server caps every session at `internal` there. Consequence: all 20
     seed skills are `data-class: "restricted"`, so on such a host they lose every restricted
     source; the daily brief (`docs/daily-brief.md`, a `restricted` skill) is degraded; and the
     metric keeps its meaning.
   - **(c) Yes, all classes, under conditions** (the company's own cloud account, disk and
     backup encryption, the provider's data-processing terms, Security and Legal's sign-off).
     Consequence: the metric's "off the Spark" becomes "off PM OS's host"; the cloud account's
     administrators become people who can read restricted data; Legal and Security must sign off
     before the pilot uses it.
   **Recommendation: (a) for now**, the narrowest and the only one that keeps the PRD's metric as
   written; revisit with Security and Legal, as (b) or (c), if the pilot needs a cloud host.
10. **D2. What must a `PMOS_EXPOSURE=public` host require for sign-in?** The reviewer rated the
    password sign-in findings Medium for an internal network (a flood can block new sign-ins; one
    IPv6 /48 gets about 86,400 guesses a day at one account) and would rate them higher on the
    internet.
    - **(a) Microsoft (Entra) sign-in only on a public host; password sign-in only behind a VPN
      or an address allow-list** (the reviewer's recommendation). Consequence: PMs without Entra
      need the VPN; H10 builds the refusal and the allow-list (§3.8).
    - **(b) The web app only behind a VPN** on a public host, any sign-in. Consequence: the host
      is not really public for browsers; the proxy listens on the VPN address only; H-EXPOSURE
      checks that instead.
    - **(c) Password sign-in on the internet as built.** Consequence: the Medium findings rise
      and must be re-rated and fixed before the pilot (per-network limits that hold against a
      /48, a captcha or similar on new sign-ins).
    **Recommendation: (a)**, with (b) as the fallback if the company has no Entra tenant for some
    PMs.
11. **D3. Is the accepted limit "a broken-into web backend can act as any PM" still acceptable on
    a public listener, where the admin routes share that listener?**
    - **(a) Accept it, admin routes served publicly.** Consequence: no code change; the routes
      that change users, content and policy are reachable from the internet behind sign-in only.
    - **(b) Keep the limit, but take `/api/admin/*` off the public listener** (`PMOS_WEB_ADMIN_BIND`,
      §3.8). Consequence: admins use the admin screens over the VPN or an SSH tunnel; H10 adds the
      listener; the limit itself remains for the PM routes.
    - **(c) Not acceptable on a public host at all.** Consequence: no `public` web app until the
      web backend is split so that a break-in can't sign as any PM (a design of its own, not in
      this spec); D2's (b) becomes the only way to serve PMs from a cloud host.
    **Recommendation: (b).**

## 9. What this spec does not cover

- **Requirement i** (model providers, switching and fallback, OpenShell holding provider keys for
  the gateway): not started; the owner's go-ahead is pending. This spec only keeps the hosted
  model testing-only and notes where i will change that.
- **The OpenShell policy prover in Policy Admin:** a separate plan awaiting approval.
- **OpenShell's sandbox allow and deny decisions in the audit log:** a separate plan awaiting
  approval. The red-team script still reads OpenShell's logs directly.
- **MCP servers in place of connectors** (scope item g): a separate plan awaiting approval.
- **Multi-host PM OS** (the stack split across machines) and **high availability.**
- **Per-cloud automation** (Terraform, cloud images, managed Postgres, managed load balancers).
- **Changing the platform decision in the content repo**
  (`content/context/decisions/2026-10-product-agent-platform.md`), which names the Spark.
- **Splitting the web backend** so a break-in can't sign as any PM (D3's answer (c)).

## 10. Review of 2026-11-22

The reviewer's verdict on `9de1a65`: "sound after changes", no High or Critical in the sandbox
design. What changed:

1. **The `hosted-test` refusal moved into the services** (§3.6). It was in `pmos-admin up` only;
   `docker compose up`, `add-bot` and `restart` skipped it. The gateway, tool server and
   `pmos-slack` now refuse at start-up on `PMOS_MODEL_SOURCE`, and the runtime cross-checks it.
   "Fake data only" now covers the content library, the memory repos, the database and Slack
   tokens. The gateway's "local" no longer includes any private address (`is_local_host`, now at
   `crates/gateway/src/config.rs:581-599`).
2. **Each name pinned to its own `/32`** instead of the whole `10.201.77.0/24` (§3.5; RT-13b).
3. **An `ip_range` for unpinned containers** (§3.3), since the eval stack and probes share the
   network; the subnet refusal moved from H4 to H3, which brings the constants it checks.
4. **Public exposure** (§3.8): neither health route forwarded; `PMOS_WEB_INTERNAL_BIND` (which
   exists in `crates/web`) set by the kit in H9; a full nginx example with body-size and rate
   limits; the admin routes as decision D3, with both answers designed.
5. **Bot names validated** before they reach `deploy/.env`, paths, `sed` or a unit name;
   `systemd-escape` and `StartLimit*` on the unit (§3.7, H0).
6. **Host checks re-run** at every `up`, `sandbox`, `resume` and worker start (§3.2). H-NAMES is
   labelled as the host's view and O-REACH added for the sandbox's. H-METADATA redefined per
   answer, with one generic block that passes on every cloud.
7. **The gateway's container settings** (proxy, `PMOS_GATEWAY_REMOTE_KEY`,
   `PMOS_GATEWAY_CLOUD_TEST`) added in H4 (§3.6; surprise 11).

Also: slice H0 for the four Spark bugs the reviewer confirmed (plus the name validation); H9 to
H11 added; decisions D1 to D3 added; O8 narrowed, since bot decision D2 already requires
supervision. Still true: nothing in this spec has run on a real host with OpenShell, the Spark
included.
