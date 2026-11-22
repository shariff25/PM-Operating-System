# OpenShell on any Linux host: the bots off the Spark

Status: **design spec; not built.** The reviewer checks it before any slice starts (CLAUDE.md,
scope item c). File references are to `claude/openshell-any-host-spec`, based on
`claude/web-app` at `d4df2eb` (which contains `2efb73f`).

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
real host**.

## 0. Goal and non-goals

**Goal.** One deployment kit (`deploy/`) and one set of reviewed sandbox policies (`policies/`)
that run PM OS's bots in OpenShell sandboxes on:

- the DGX Spark (DGX OS, arm64, local models on its GPU), as today;
- any single Linux host with Docker that meets OpenShell's requirements: a VM on any cloud or an
  on-prem server, amd64 or arm64, with or without a GPU.

What a host needs is checked by a command before anything is installed, and stated as pass or
fail per requirement.

**Non-goals.**

- **No per-cloud code.** No AWS, GCP or Azure APIs, images, templates or managed services. A cloud
  VM is just a Linux host.
- **No serverless container platforms** (§4, "Unsupported").
- **No multi-host PM OS.** One host runs the whole stack, as on the Spark. Kubernetes is Tier 2,
  designed in outline only (§4).
- **No model switching or fallback.** That is requirement i (CLAUDE.md): not started, not designed
  here. This spec only notes where a host without a GPU depends on it (§3.6).
- **No change to what a sandbox may reach.** The three PM OS endpoints, their routes, the MCP
  methods, the binaries, the credentials and the filesystem rules stay exactly as reviewed. The
  one policy change proposed (§3.5, pinning the addresses those three names may resolve to)
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
| `allowed_ips` | "When an endpoint sets `allowed_ips`, every address its hostname resolves to must fall within the list". | `network-rules.mdx` | Confirmed in docs; PM OS's schema mirror already has the field (`crates/policy/src/openshell.rs:119`) |
| DNS inside the sandbox | OpenShell answers DNS for rule hosts with placeholder addresses and resolves the real destination "through its trusted resolver"; no search domains; AAAA answers are empty. **Which resolver that is (the host's, with its `/etc/hosts`?) is not stated.** | `network-rules.mdx` | Unknown; verify on a real host |
| Gateway install | A systemd **user** service at `https://127.0.0.1:17670` with an mTLS bundle made automatically; `loginctl enable-linger` keeps it running after logout. | [`about/installation.mdx`](https://github.com/NVIDIA/OpenShell/blob/main/docs/about/installation.mdx), [`gateways/authentication.mdx`](https://github.com/NVIDIA/OpenShell/blob/main/docs/how-it-works/gateways/authentication.mdx) | Reported |
| Plaintext gateway | "Intended for local port-forwarding or gateways behind a trusted reverse proxy or tunnel." The research reported that unauthenticated mode "must not be exposed to shared or public networks". The page read on 2026-11-22 says it for the Helm overlay only. | `gateways/authentication.mdx` | Reported |
| Remote gateway over SSH | `openshell gateway start --remote user@hostname` appeared in search snippets only; the page returned 404, and the installation page has no such command. | — | **Unconfirmed: not used by this design** |
| Custom images | `--from` takes an OCI image reference; "the target must declare an explicit numeric `USER UID:GID`" (said for Docker updates). | [`sandboxes/overview.mdx`](https://github.com/NVIDIA/OpenShell/blob/main/docs/how-it-works/sandboxes/overview.mdx) | Reported; see §2 surprises |
| Resources | No GPU, RAM or disk requirement is stated. | support matrix | — |
| Cloud | Not named, except Amazon Linux 2023+ for the glibc requirement. Serverless platforms are not documented. | support matrix | — |

## 2. Inventory: where the repo assumes the Spark

Classes: **(a)** truly Spark-only (local GPU model serving); **(b)** a host-specific value that
should become a setting or a generated, checked value; **(c)** wording only. "Fixed" means the
value is not host-specific after all and stays a constant.

### Deployment kit

| Where | What it assumes | Class | Change (slice) |
| --- | --- | --- | --- |
| `deploy/compose.yaml:1`, `:12-16` | "PM OS on the DGX Spark"; sandboxes reach the services on 127.0.0.1 "(see /etc/hosts in the install runbook)" | (b) and (c) | Pinned service addresses (§3.3); wording (H2, H6) |
| `deploy/compose.yaml:32-34`, `:291` | "The Spark's outbound proxy" | (c) | "This host's outbound proxy" (H6) |
| `deploy/compose.yaml:364-402` | `vllm-default`, `vllm-extraction`: `nvcr.io/nvidia/vllm:25.10-py3`, the NVIDIA device reservation, GPU memory shares sized for the Spark | (a) | Stays in the `models` profile; used only with `PMOS_MODEL_SOURCE=local` (§3.6, H4) |
| `deploy/compose.yaml:404-416` | The pinned `pmos` network (`10.201.77.0/24`) | (b), already a setting | Also the base of the pinned service addresses (§3.3) |
| `deploy/env.example:1`, `:168-173`, `:177` | "PM OS on the Spark"; "Models (verify on the Spark)": `PMOS_VLLM_IMAGE`, model ids, GPU shares; "if the Spark has an outbound proxy" | (a) for the model block, (c) otherwise | Model block under `PMOS_MODEL_SOURCE=local`; new host-profile block (§3.1, H4) |
| `deploy/setup.sh:2`, `:37-41` | "on the Spark"; warns when there is no NVIDIA runtime | (b) | Warn only when `PMOS_MODEL_SOURCE=local`; run the preflight first (H1, H4) |
| `deploy/pmos-admin:2` | "PM OS operations on the Spark" | (c) | H6 |
| `deploy/pmos-admin:637-670` (`sandbox`) | `--image`, `/usr/local/bin/node`, `/sandbox/workspace`, `*.pmos.internal` URLs; the worker started with `&` from the admin's shell | node path and layout: fixed; flags and supervision: (b) | Flags as verified; a supervised worker (§3.7, H5) |
| `deploy/gateway.yaml:1-3`; `config/gateway.yaml:4-10` | Upstreams are the Spark's vLLM servers; "prompts never leave the Spark" | (a) and (c) | A hosted-test example config beside it (§3.6, H4) |
| `deploy/Dockerfile:3` | "Built on the Spark itself (arm64)" | (c) | "Built on the host, for its own architecture" (H6). CI's deploy job already builds both images on `ubuntu-latest`, which is amd64, but CI isn't running jobs at present (CLAUDE.md, "Waiting on the owner") |
| `deploy/Dockerfile.worker:2-3`, `:19-25` | Node at `/usr/local/bin/node`; `USER node` | node path: fixed; `USER`: verify (surprise 3 below) | H5 |
| `.github/workflows/evals-gate.yml:5-73` | Runner labels `self-hosted, pmos-spark`; `PMOS_SPARK_DIR` | (b) | Label `pmos-host`, variable `PMOS_HOST_DIR` with `PMOS_SPARK_DIR` still read (H6). The runner is not registered yet, so renaming now costs nothing |
| `.github/workflows/ci.yml:51` | "the Spark's eval gate" | (c) | H6 |

### Policies and their generator

| Where | What it assumes | Class | Change (slice) |
| --- | --- | --- | --- |
| `crates/policy/src/lib.rs:41-53` | `toolserver.pmos.internal:8710`, `gateway.pmos.internal:8740`, `bots.pmos.internal:8750` | Fixed | Names and ports stay; how they resolve becomes generated and checked (§3.3) |
| `crates/policy/src/lib.rs:86-88` | `WORKER_BINARY = /usr/local/bin/node`, "Verify on the Spark" | Fixed | The path comes from PM OS's own worker image (`node:22-bookworm-slim`), which is the same on every host. Verify once per architecture (P-2), not per host |
| `crates/policy/src/lib.rs:92-104` | `CONTENT_MOUNT = /sandbox`, `WORKSPACE = /sandbox/workspace` | Fixed | OpenShell's and the worker image's layout, not the host's. Verify once per OpenShell version and driver (RT-4) |
| `crates/policy/src/lib.rs:28`, `:69`, `:81` | "verify on the Spark" | (c) | "verify on a real host" (H3) |
| `crates/policy/src/generate.rs:300`, `:382` | Header text of every generated file: "Values still to verify on the Spark", "Import once on the Spark" | (c), but changing it changes every committed policy file | One regeneration, in H3, together with the `allowed_ips` change so policies change once |
| `policies/bots/docs.yaml:6`; `policies/providers/*.yaml:4` | Generated wording | (c) | Regenerated by H3 |
| `policies/README.md:4`, `:12`, `:49`, `:116-121` | "Applying on the Spark" | (c) | H6 |
| `policies/redteam/checklist.md:3`, `:24`, `:51` | "Run it on the Spark"; P-1 "kernel 6.2 on DGX OS" | (c), P-1 becomes the preflight | H7 |
| `policies/redteam/isolation.sh:4`, `:21` | "RUN THIS ON THE SPARK" | (c) | "Run this on the host, never on a machine shared with other work" (H7) |

### Docs and runbooks

| Where | What it assumes | Class | Change (slice) |
| --- | --- | --- | --- |
| `docs/runbooks/install.md:1`, `:10-12` | "Install PM OS on the Spark"; DGX OS with the NVIDIA container toolkit | (b) | "Before you start" per host profile, the preflight first (H6) |
| `docs/runbooks/install.md:114-132` (Stage 3) | vLLM image, GPU shares | (a) | Stage 3 has two paths: local models, or the hosted test model (H6) |
| `docs/runbooks/install.md:138-141` | `/etc/hosts`: `127.0.0.1 toolserver.pmos.internal gateway.pmos.internal bots.pmos.internal` | (b), and contradicts OpenShell's docs (surprise 1) | Replaced by `pmos-admin host-names apply` (§3.3, H2) |
| `docs/runbooks/install.md:158-160` | "Run it on the Spark only, never on a shared machine" | (c) | "Only on this host, and never on a host shared with other workloads" (H6) |
| `docs/runbooks/install.md:229-264` | Console "only behind a sign-in proxy on the Spark" | (c), plus the exposure rule (§3.8) | H6 |
| `docs/runbooks/README.md:1-4`, `:25`, `:34` | "one DGX Spark"; ports "on the Spark"; "on the GPU" | (c) and (a) | H6 |
| `docs/runbooks/upgrade.md:217-280` | Registering the Spark CI runner (`ARM64`, `spark-1`, `pmos-spark`) | (b) | Any architecture; label `pmos-host` (H6) |
| `docs/runbooks/backup-restore.md:28-47`; `incidents.md:68-70` | Off-Spark copies; "The Spark is gone" | (c) | "Off the host"; encryption of the copy (§5, H6) |
| `docs/runbooks/daily.md:13`, `:26` | GPU queue; "a second Spark" | (a) for the GPU text | Unchanged meaning; "a second host" (H6) |
| `docs/sandbox-policies.md:38`, `:42`, `:180`, `:299-306`, `:320`, `:330-347` | "Verify on the Spark" table and "Run once on the Spark" | (c), and the table becomes the host acceptance list (§6) | H3 |
| `docs/gateway.md:3-9`, `:165`; `crates/gateway/src/config.rs:421-423`, `lib.rs:2-6`, `usage.rs:3` | ADM-3: "prompts never leave the Spark"; the gateway refuses a non-local upstream outside testing | (a): a policy decision, not a host fact | Unchanged by this spec; the hosted path is testing-only until the owner decides otherwise (§3.6, decision O4) |
| `docs/toolserver.md:1078` | "the context window on the Spark's local model" | (c) | H6 |
| `README.md:5`, `:69`, `:86`; `docs/implementation-plan.md`; `docs/bot-decisions.md:4`, `:21`, `:32` (D2) | "runs on the team's DGX Spark with open-weight models only"; D2 "Bots are long-lived services on the Spark" | (c), except "open-weight models only", which is ADM-3 | Wording in H6; D2 and ADM-3 keep their meaning ("on PM OS's host") |
| `content/context/decisions/2026-10-product-agent-platform.md:10-12` | The team's platform decision names the Spark | Content, not code | Not changed by this spec; the owner may want to amend it |

### Surprises found while making the inventory

1. **The `/etc/hosts` line in the install runbook cannot work as written, on the Spark or
   anywhere.** It maps the three names to `127.0.0.1`. OpenShell's docs say a policy "never
   authorizes an outbound endpoint whose destination is loopback", and that sandbox-local
   loopback is the sandbox's own, so `127.0.0.1` would be the sandbox itself. With the Docker
   driver's default `host` supervisor networking it might be the host's loopback instead, which
   the same rule still refuses. The three services publish their ports on `127.0.0.1` only
   (`deploy/compose.yaml`), so no non-loopback address on the host serves them today. §3.3 fixes
   this for every host, the Spark included.
2. **`pmos-admin sandbox` passes `--image`; OpenShell's sandbox page documents `--from`.** The
   script already says "verify on the Spark: the OpenShell command names and flags (`--image` …)".
   Settle it on the first real host (H5).
3. **The worker image ends with `USER node` (a name); OpenShell's page says a Docker target
   "must declare an explicit numeric `USER UID:GID`"**, and the policy asks for `run_as_user:
   sandbox`, an account the `node` image does not have. Whether OpenShell creates or maps it is
   not documented. Verify on a real host (RT-3), and fix the image if needed (H5).
4. **The worker is started with `&` from the operator's shell** (`deploy/pmos-admin:669`).
   Nothing restarts it after a reboot or after the operator's SSH session ends, which conflicts
   with D2 ("After a reboot, in-flight tasks resume"). Cloud VMs reboot for maintenance more often
   than an office box, so this matters more off the Spark (§3.7).
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

## 3. Design

### 3.1 The host profile: four settings in `deploy/.env`

The smallest mechanism is not a new file format. It is four settings in `deploy/.env`, written by
`setup.sh`, read by `pmos-admin` and the preflight, and **never read by the policy generator**.

| Setting | Values | Default | What it decides |
| --- | --- | --- | --- |
| `PMOS_HOST_PROFILE` | `spark`, `linux` | `spark` when `setup.sh` finds DGX OS (`/etc/dgx-release`), else `linux` | Which preflight checks are required; wording in `status`. `spark` = `linux` plus the NVIDIA runtime and local models required |
| `PMOS_MODEL_SOURCE` | `local`, `hosted-test` | `local` on `spark`; `hosted-test` on `linux` with no NVIDIA runtime | `local`: the vLLM profile, NVIDIA runtime required. `hosted-test`: no vLLM; the gateway runs a hosted test upstream (§3.6) |
| `PMOS_EXPOSURE` | `private`, `public` | `private` | `public` means the host has a public address or sits on a network you don't control. It turns on the exposure checks and refusals in §3.8 |
| `PMOS_OPENSHELL_DRIVER` | `docker` | `docker` | The OpenShell compute driver. Only `docker` is Tier 1; `podman` and `kubernetes` are refused until verified (§4) |

Existing installs: `setup.sh` fills in missing values only, so a Spark install gets `spark`,
`local`, `private`, `docker` and behaves as today.

What is deliberately **not** a setting:

- **The three host names and ports, the routes, the MCP methods.** They are the reviewed network
  policy (POL-2). They are the same on every host because the services are PM OS's own.
- **The worker binary path and the `/sandbox` layout.** They come from PM OS's own worker image
  and OpenShell's layout, which are identical on every host. A setting would let `deploy/.env`
  change a reviewed policy, and would make the committed `policies/` files differ per host. If
  OpenShell or an architecture ever needs a different value, the constant changes in
  `crates/policy/src/lib.rs` for every host, under POL-2 review.

So **the policy files stay host-independent**: one reviewed set in the content repo works on the
Spark and on any Tier 1 host.

### 3.2 The preflight: `deploy/preflight.sh` (and `pmos-admin preflight`)

A script that checks a host before anything is installed, and prints pass, fail or warn per
requirement with the measured value. It needs only `bash`, coreutils and `python3` (already
needed by `pmos-admin ledger`). It changes nothing on the host.

```
deploy/preflight.sh [--profile spark|linux] [--model-source local|hosted-test]
                    [--exposure private|public] [--openshell] [--json] [--root <dir>]
```

`setup.sh` runs it first and stops on any `fail`. `pmos-admin preflight` runs it with the
settings from `deploy/.env`. `--root <dir>` reads `/proc`, `/sys`, `/etc` and command outputs from
a fixture directory instead of the host, so every check is unit-tested (§6).

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
| H-SUBNET | the `pmos` subnet is not already routed | `ip route` has no route overlapping `PMOS_NETWORK_SUBNET` other than Docker's own `pmos` bridge | overlap |
| H-NAMES | the three names resolve to the pinned addresses (§3.3), and nothing maps them to loopback | `getent ahostsv4` for each name; the managed block in `/etc/hosts` | wrong or loopback address. Before install this reports "not set yet", which is a pass for the install stage |
| H-METADATA | the cloud metadata address is not reachable from a container on the `pmos` network | after install: `docker run --rm --network pmos_pmos` a probe to `169.254.169.254:80` with a 2 s timeout | reachable: **warn** on `private`, **fail** on `public` (§5) |
| H-EXPOSURE | nothing PM OS runs listens on a non-loopback address except the reverse proxy | `ss -ltnH`: 8710, 8740, 8750, 8760, 8770 and OpenShell's 17670 only on `127.0.0.1` or the `pmos` bridge | otherwise, **fail** on `public` |
| H-DEVLOGIN | development sign-in off | `PMOS_WEB_ENABLE_DEV_LOGIN_CLOSED_TEST_NETWORK_ONLY` | `1` with `PMOS_EXPOSURE=public` (fail) |
| H-ASSERT | `PMOS_ASSERTION_MODE=require` | `deploy/.env` (the same rule as `assertion_checks`) | anything else while a web or console token is set |
| O-CLI | OpenShell CLI installed (with `--openshell`) | `openshell --version`, `openshell status` | missing, or the gateway not reachable |
| O-GATEWAY | OpenShell's gateway is local and authenticated | the gateway endpoint the CLI uses (`openshell status`) is loopback or the Docker bridge, over TLS | a non-loopback plaintext endpoint |
| O-LINGER | the gateway survives logout and reboot | `loginctl show-user "$USER" -p Linger` is `yes`, and the user unit is enabled | not set (fail) |
| O-CANARY | a sandbox actually starts under PM OS's settings | creates `pmos-preflight` from the worker image with a minimal policy (no network rules, `landlock: hard_requirement`, `run_as_user: sandbox`), runs `/usr/local/bin/node -e 'process.exit(0)'`, reads the `Landlock ruleset built` event, deletes the sandbox | the sandbox does not start, Landlock was not applied, or node isn't at that path |

O-CANARY is the only check that proves the kernel and the runtime together are enough, which is
what OpenShell's "a kernel version alone does not establish support" asks for. The other kernel
checks exist to say *why* a host fails before an hour of installing. The command names it uses
(`sandbox create --from`, `logs --source sandbox`, `sandbox delete`) are from OpenShell's docs:
verify on a real host.

Exit status: `0` all pass (warnings allowed), `1` any fail, `2` the script could not run.
`--json` prints `{"id", "status", "measured", "expected", "hint"}` per check, so a run can be
attached to the acceptance record (§6).

### 3.3 How sandboxes find the three services, on any host

**The problem** (surprise 1): the services publish only on `127.0.0.1`, and loopback is never an
allowed destination.

**The design:**

1. **Pin the three services' addresses on the `pmos` network.** `deploy/compose.yaml` gives
   `toolserver`, `gateway` and `bots` fixed `ipv4_address`es inside `PMOS_NETWORK_SUBNET`: by
   default `10.201.77.10`, `.11` and `.12`. Each service already listens on `0.0.0.0` inside its
   container, so it answers there without publishing anything new. Docker never hands a pinned
   address to another container. The ports stay published on `127.0.0.1` too, for `pmos-admin`.
2. **The names resolve to those addresses through a managed block in `/etc/hosts`**, written by
   `pmos-admin host-names apply` (with `sudo`), between `# BEGIN pmos (managed by pmos-admin)` and
   `# END pmos` markers. It is generated from `deploy/.env`, replaces only its own block,
   and is idempotent. `pmos-admin host-names check` and `status` report a missing, stale or
   loopback entry, and `pmos-admin sandbox` refuses to start a sandbox while the check fails.
   Nobody edits `/etc/hosts` by hand, and the Spark gets the same block.
3. **Why this should reach the services:** with the Docker driver's default `host` supervisor
   networking, the sandbox's egress leaves from the host's network namespace, which has a route
   to the `pmos` bridge. The addresses are private, which OpenShell allows for an exact host name.
   **Verify on a real host:** that OpenShell's "trusted resolver" is the host's resolver (so the
   managed block is what it reads), and that the supervisor reaches a bridge address. Check H-1
   in §6.
4. **If OpenShell does not use the host's `/etc/hosts`:** stop and report before changing
   anything. The fallbacks, each needing its own review, are a host name that the resolver OpenShell
   does use can answer (for example a local DNS server bound to the `pmos` bridge), or OpenShell's
   `bridge` mode with `host_gateway_ip`. This spec does not pick one without seeing OpenShell
   behave.

Nothing in this changes the policy's hosts, ports or routes.

### 3.4 Each host's other values

| Value | Where it lives | Safe default |
| --- | --- | --- |
| Service addresses | derived from `PMOS_NETWORK_SUBNET` (`.10`, `.11`, `.12`) | `10.201.77.0/24`, already pinned |
| OpenShell gateway endpoint | OpenShell's own CLI configuration (`openshell status`); PM OS never stores it | the local package's `https://127.0.0.1:17670`, mTLS |
| Outbound proxy | `HTTPS_PROXY`, `NO_PROXY`, `PMOS_BUILD_CA` (exist) | none |
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
`allowed_ips: [10.201.77.0/24]`, a new constant `SERVICE_SUBNET` beside the host names. Then a
name that resolves anywhere else (a missing or stale `/etc/hosts` block, a VPC resolver that
answers `*.internal`, a tampered entry) is refused by OpenShell, not sent on. A link-local address
is already refused whatever the policy says; this also covers public and other private addresses.
The cost: a site that has to change `PMOS_NETWORK_SUBNET` (because its network already routes
`10.201.77.0/24`) needs a code change to `SERVICE_SUBNET` with a POL-2 review, not just a
setting. That is the intended trade: the address a sandbox may reach is reviewed, never
configured.

**Drift, and how each check treats a profile:**

| Check | Reads | A host can make it fail by |
| --- | --- | --- |
| `pmos policy check` (CI and `pmos-admin lint`) | content and `policies/` only | nothing: it never sees the host. A new rule, `net-allowed-ips`, requires each endpoint's `allowed_ips` to be exactly `SERVICE_SUBNET` (wider, missing, or a second range fails, each with a failing and a passing unit test, like every rule) |
| `pmos-admin preflight` / `status` / `host-names check` | `deploy/.env`, the host | a subnet that isn't `SERVICE_SUBNET`, names that resolve elsewhere, a profile check that fails |
| `pmos-admin up` and `sandbox` | the same | `up` refuses when `PMOS_NETWORK_SUBNET` differs from `SERVICE_SUBNET` (the policies would refuse every connection); `sandbox` refuses while `host-names check` fails |

The `PMOS_NETWORK_SUBNET` setting stays (the web app's trusted proxy checks use it), but any
value other than the constant is refused with a message that names the code change and POL-2.

Applying H3's change is a network change: a second platform owner reviews it, every bot's policy
file is regenerated once (the header wording changes in the same commit), and on each host
`openshell policy set` applies it live.

### 3.6 The model on a host without a GPU, and where its key is held

| `PMOS_MODEL_SOURCE` | Gateway config | Needs | Data allowed |
| --- | --- | --- | --- |
| `local` | `deploy/gateway.yaml` (vLLM upstreams), `up --models` | NVIDIA runtime and a GPU large enough for the two models | real data, after Gate 1 |
| `hosted-test` | `deploy/gateway.hosted-test.yaml`: each role an `openai+https://<host>/<base>` upstream (the existing remote test upstream, `docs/gateway.md`), `PMOS_GATEWAY_CLOUD_TEST=1` | an OpenAI-compatible API and its key | **fake data only** |

Rules for `hosted-test`, enforced by `pmos-admin up` (H4):

- `--models` is refused (there are no local model servers).
- `PMOS_CONNECTORS=live` is refused while `PMOS_GATEWAY_CLOUD_TEST=1`: prompts would carry real
  Jira, Confluence and Slack data off the host, which ADM-3 forbids. This holds until the owner
  changes ADM-3 (decision O4). The test stack's rule "fake data only" (CLAUDE.md) is then enforced
  rather than remembered.
- Eval reports from `hosted-test` already say so (`GET /v1/pmos/roles` reports `remote-test`), so
  they never count towards a bar measured on the production role models.

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

The host needs outbound HTTPS to the hosted API's host only from the gateway container. With an
outbound proxy, it goes through the existing `HTTPS_PROXY` setting.

### 3.7 Starting and keeping the worker running

- **Flags as verified:** `pmos-admin sandbox` uses the flags O-CANARY proved on the host
  (`--from` or `--image`, surprise 2). The O-CANARY code and `pmos-admin sandbox` share one
  function, so they cannot drift apart.
- **A supervised worker:** a systemd unit, `deploy/systemd/pmos-worker@.service` (instance = bot),
  installed by `pmos-admin sandbox <bot> --install-unit` as a user unit of the OpenShell gateway's
  user. It runs `openshell sandbox exec … /usr/local/bin/node /opt/pmos/harness/dist/main.js`
  with `Restart=on-failure` and `After=` the OpenShell gateway's unit, and needs lingering
  (O-LINGER). `pause` and `resume` stop and start the unit as well as the sandbox. No token is in
  the unit file: the worker's credentials are the sandbox's providers.
- **The image's user:** if RT-3 or O-CANARY shows OpenShell needs a numeric `USER` or a `sandbox`
  account in the image, `deploy/Dockerfile.worker` changes accordingly, and the policy's
  `run_as_user` follows only through a POL-2-reviewed change to `RUN_AS`.

### 3.8 Network exposure and TLS on a host that isn't on a private LAN

The Spark sat on the company's private LAN. A cloud VM may have a public address. Which port may
be reached from where:

| Listener | Bound to | Reached by | On `PMOS_EXPOSURE=public` |
| --- | --- | --- | --- |
| Reverse proxy (Caddy or nginx), 443 | the host's address | browsers | the only public listener. TLS with a publicly trusted certificate; HTTP only to redirect |
| `/oauth/*` on the tool server | through the proxy only (`install.md`, Stage 6) | PMs' browsers | as today: only `/oauth/`, `respond 404` for everything else |
| Web app 8770 | `127.0.0.1` | the proxy | only with Entra sign-in or password sign-in with its required authenticator code; development sign-in refused (H-DEVLOGIN) |
| Console 8760 | `127.0.0.1` | the proxy | **never on a public listener** (decision O7): reachable only over a private network, VPN or SSH tunnel, behind its SSO proxy |
| Tool server 8710, gateway 8740, runtime 8750 | `127.0.0.1` and their pinned `pmos` addresses | `pmos-admin`; the sandboxes | never public |
| Postgres, docs, vLLM | the `pmos` network only | the services | never public |
| OpenShell gateway 17670 | `127.0.0.1` (and the Docker bridge, which the driver adds) | the CLI on the host; the sandboxes' supervisors | never public; mTLS kept on; plaintext mode not used |
| SSH 22 | the host | operators | from the operators' addresses only |

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
| **Tier 1 (the generic target)** | One Linux host (Debian or Ubuntu), Docker Engine 28+, OpenShell's Docker driver, amd64 or arm64, any cloud VM or on-prem server, with `local` (NVIDIA GPU) or `hosted-test` models | Designed and built here | The generic implementation the owner asked for |
| **Tier 2 (later)** | Kubernetes 1.33 through OpenShell's Helm chart, with a CNI that enforces `NetworkPolicy` | Outlined only; a later slice (H8) unless the owner wants it for the pilot (decision O2) | The three names would be cluster Services (or a DNS rewrite), PM OS would move from Compose to manifests, preflight gains a cluster mode (CNI `NetworkPolicy` enforcement, OpenShell's namespaces). That is a second deployment kit, not a small slice |
| **Not offered** | Podman rootless, MicroVM, macOS Docker Desktop, WSL 2 | OpenShell lists them; PM OS does not test them | `PMOS_OPENSHELL_DRIVER` refuses `podman` until someone verifies it. macOS and WSL 2 are development set-ups, not a host for PMs' work |
| **Unsupported** | Serverless container platforms: Vercel, Cloud Run, Fargate, Fly.io, Railway and similar | Never | OpenShell needs a host it controls: a Docker daemon (or cluster) to create a container per sandbox, Landlock and seccomp on a kernel it can check, and a long-running gateway. These platforms run your one container on their kernel and their runtime, give no Docker daemon, and don't document Landlock. PM OS also needs Postgres with durable disk and always-on bots (D2). None of that is documented for them, so they are unsupported, not "untested" |

**What "supported" will mean.** The docs may call a host or tier **verified** only after the
acceptance run in §6 has passed on a real host of that kind, with the record committed
(`docs/hosts/<date>-<host-kind>.md`: OpenShell version, kernel, Docker version, architecture,
this repo's commit, the preflight JSON, the red-team script's output). Until then the runbooks say
"designed for; not yet verified". One passed run verifies that host kind (for example "Ubuntu
24.04 amd64 VM, Docker 28, no GPU, hosted-test"), not every cloud; a second architecture or
kernel family needs its own run. Today **no host is verified, the Spark included**.

## 5. Security analysis

### What changes off the Spark

| Threat | On the Spark | On a cloud VM or shared server | Handled by |
| --- | --- | --- | --- |
| The host is reachable from the internet | Private LAN | Possibly a public IP | §3.8 exposure table; H-EXPOSURE, H-DEVLOGIN; console never public; RT-14 from outside |
| Other tenants | A box the team owns | The cloud's other customers share the hypervisor; on-prem servers may be shared with other workloads | The hypervisor boundary is the cloud's. Don't run PM OS on a host shared with other workloads: the red team already says "never on a shared machine" |
| The cloud metadata service (`169.254.169.254`; `metadata.google.internal`, which resolves to it; AWS's IPv6 `fd00:ec2::254`) | None | Present, and may hand out the VM's cloud credentials | **From a sandbox:** link-local is never allowed by policy (OpenShell docs), the policy allows only three exact names, AAAA answers are empty, and `allowed_ips` (§3.5) pins those names to `10.201.77.0/24`. RT-11 checks each address. **From PM OS's own containers**, which are not sandboxed and hold the vault key: they *can* reach it. Give the VM no cloud role, or one with no permissions; on AWS require IMDSv2 with a hop limit of 1 so containers can't reach it; H-METADATA fails a `public` host where a container reaches it |
| Metadata services outside link-local (Azure's `168.63.129.16`, Alibaba's `100.100.100.200`) | None | Possible | Not in any policy: default deny. RT-11 includes both. Whether OpenShell treats `100.64.0.0/10` as private is not documented; it doesn't matter while no rule allows it |
| Name resolution | `/etc/hosts` on a LAN box | VPC resolvers answer `*.internal` names on some clouds | `.internal` is reserved for private use, the managed block decides the three names, and `allowed_ips` refuses any other answer |
| Secrets at rest | `deploy/.env` (vault key), Postgres (sealed tokens), backups on the Spark's disk | Same files on a cloud disk, plus snapshots and images the cloud account can copy | Encrypt the disk (the provider's encryption at rest, or LUKS on-prem). Treat VM snapshots and images as secrets: they hold the vault key and the sealed tokens together. Restrict who in the cloud account may snapshot, attach or read the disk |
| Backups | "Copy it off the Spark" | Copies go to object storage | Encrypt each backup before it leaves the host (for example `age` to the platform owners' keys), and never copy `deploy/.env` in the same place as the database dump. Restore test as Gate 1 asks |
| The hosted model | None (local only) | `hosted-test` sends prompts off the host | Fake data only, enforced (§3.6); the key stays in `deploy/.env`; no sandbox reaches the hosted API |
| Maintenance reboots | Rare | Routine | The supervised worker and lingering (§3.7); the runtime already retries a running task once |
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
| P-0 | `deploy/preflight.sh --openshell --json` on the host | Every check passes; the JSON is attached to the record. Replaces P-1's "kernel 6.2 on DGX OS" |
| P-7 | `pmos-admin host-names check` | The three names resolve to `.10`, `.11`, `.12` in `PMOS_NETWORK_SUBNET`; no loopback entry anywhere for them |
| RT-11 | From A: `169.254.169.254`, `metadata.google.internal`, `[fd00:ec2::254]`, `168.63.129.16`, `100.100.100.200`, each on 80 and 443 | Each blocked by OpenShell and logged as RT-10 describes |
| RT-12 | From A: the host's own addresses on 22, 17670 (OpenShell's gateway), 2375 and 2376 (Docker's API), and the Docker socket path | Each blocked; the socket is not in A's filesystem |
| RT-13 | From A: the console and Postgres by their `pmos` addresses (`<console address>:8760`, `<postgres address>:5432`), and a pinned service address on a port that isn't its own (`10.201.77.10:8740`) | Each blocked: only the three exact names on their own ports are allowed |
| RT-13b | Point a name at another address for a moment (`host-names apply` with a test override to `10.201.78.10`), then call it from A | Refused by `allowed_ips`; restore the block afterwards (only if O6 is adopted) |
| RT-14 | `exposure.sh <host's public address>` from outside the host's network | Only 443 (and 80 if used) and 22 from the operators' range answer; 5432, 8710, 8730, 8740, 8750, 8760, 8770, 17670 do not |
| RT-15 | From a second machine on the same private network or VPC: `10.201.77.10:8710` (with a route added on that machine towards the host, if the operator can) | No answer |
| RT-16 | From a container on the `pmos` network: `169.254.169.254` | No answer on a `public` host (the H-METADATA rule) |
| E-5 | Reboot the host | The OpenShell gateway, the stack and every bot's worker come back without anyone logging in; a Slack question is answered |

## 6. How to verify without a Spark

### In CI and in this build environment (no OpenShell)

| What | How | Slice |
| --- | --- | --- |
| The preflight's every check, pass and fail | `scripts/test-preflight.sh`: fixture directories under `deploy/test/preflight/` (an Ubuntu 24.04 amd64 host, a DGX OS arm64 host, a 5.15 kernel, Landlock absent from `lsm`, an ABI-2 probe result, cgroups v1, Docker 27, no NVIDIA runtime with `local`, port 8710 taken, a public bind, dev sign-in on `public`); each asserts the exact status and exit code. Run in CI's `deploy` job | H1 |
| The Landlock probe itself | run on the CI runner and here: it prints an ABI or a clear failure, never a crash. Its result is not asserted (it depends on the runner's kernel) | H1 |
| The managed hosts block | `scripts/test-pmos-admin.sh` gains cases on a temporary hosts file: apply is idempotent, keeps other lines, replaces a stale block, refuses a loopback override, and `check` fails on each wrong state | H2 |
| Compose | `docker compose config -q` with the pinned addresses; CI's existing bring-up smoke test (it doesn't use sandboxes) still passes | H2 |
| The policy change | `cargo test -p pmos-policy`: `net-allowed-ips` fails and passes; the generated files carry `allowed_ips`; `pmos policy check` green on the regenerated `policies/` | H3 |
| The host-profile refusals | `scripts/test-pmos-admin.sh`: `up --models` with `hosted-test`, live connectors with `PMOS_GATEWAY_CLOUD_TEST=1`, a subnet other than the constant, an unknown driver, each refused with exit 2 before anything starts | H4 |
| The workspace's checks | the full set in CLAUDE.md, unchanged | every slice |

None of this shows what OpenShell enforces. It shows that PM OS's side does what this spec says.

### On a real host with OpenShell (the acceptance run)

The first non-Spark host (decision O1). All with fake data and the `hosted-test` model, so no real
token or data is needed. Each step's output goes into the host record (§4).

1. **Before installing anything:** `deploy/preflight.sh --profile linux --model-source hosted-test
   --exposure public --json`. Every host check passes.
2. **Install OpenShell** from NVIDIA's install page; enable lingering;
   `deploy/preflight.sh --openshell`. O-CLI, O-GATEWAY, O-LINGER and O-CANARY pass. Record which
   `sandbox create` flag worked (surprise 2) and how the canary's identity was set (surprise 3).
3. **Stack, Stages 1–2 of the install runbook** with `hosted-test`: `status` all `ok`; the smoke
   test passes through the bring-up worker.
4. **Names:** `pmos-admin host-names apply`; P-7 passes. **H-1, the key unknown:** from the Docs
   bot's sandbox, a heartbeat on a made-up task id reaches the runtime and gets its `404`
   (RT-8's control). If it doesn't, stop at §3.3 step 4.
5. **Sandboxes:** `pmos-admin sandbox docs`, a personal bot and `pm-redteam`; the smoke test
   passes through the Docs bot's sandbox (E-1: no denials in its log).
6. **Red team:** `isolation.sh` both ways; RT-1 to RT-16 and P-0 to P-7 pass; the "verify on a
   real host" table in `docs/sandbox-policies.md` (today's "Verify on the Spark" table) filled
   in, with anything that differed.
7. **Reboot** (E-5).
8. **Write the record** and open a pull request with it; the reviewer session checks it before the
   docs call that host kind verified.

The same run on the Spark, with `--profile spark --model-source local --exposure private`, is
what verifies the Spark.

## 7. Slices, in build order

One agent, one worktree, one branch per slice (CLAUDE.md). "Before a host" means it can be built
and fully tested here; its claims about OpenShell are still checked in the acceptance run.

| # | Slice | Files | Tests | Model | Before a host? |
| --- | --- | --- | --- | --- | --- |
| H1 | **Preflight** | `deploy/preflight.sh` (new), `deploy/test/preflight/**` (fixtures), `scripts/test-preflight.sh` (new), `.github/workflows/ci.yml` (deploy job: parse and run it), `deploy/pmos-admin` (`preflight` command), `deploy/setup.sh` (run it first, stop on fail) | §6 fixtures, every check both ways, exit codes, `--json` shape | Strongest (it decides what may be called supported) | Yes; O-* checks are written now, proven in the acceptance run |
| H4 | **Host-profile settings and refusals** | `deploy/env.example` (host-profile block; model block under `local`), `deploy/setup.sh` (detect DGX OS, fill the four settings, NVIDIA warning only for `local`), `deploy/pmos-admin` (`up` refusals, `status` lines), `deploy/gateway.hosted-test.yaml` (new, example hosts only, no key), `deploy/compose.yaml` (gateway config path from the model source) | `scripts/test-pmos-admin.sh` refusal cases (§6) | Strongest (security refusals) | Yes |
| H2 | **Pinned addresses and managed host names** | `deploy/compose.yaml` (`ipv4_address` for three services), `deploy/pmos-admin` (`host-names apply`/`check`, `sandbox` refuses on a failed check, `status`), `scripts/test-pmos-admin.sh`, `docs/runbooks/install.md` Stage 4 step 1 | §6 hosts-file cases; compose config | Strongest (it decides what sandboxes reach) | Yes to build; H-1 needs a host |
| H3 | **Policy: `allowed_ips` and wording** (needs decision O6) | `crates/policy/src/lib.rs` (`SERVICE_SUBNET`; "verify on a real host"), `generate.rs` (emit `allowed_ips`; header wording), `check.rs` (`net-allowed-ips` in `RULES`), `crates/policy/tests/policies.rs`, regenerated `policies/**`, `docs/sandbox-policies.md` (rule table, "verify on a real host" table), `docs/policy-admin.md` (the fixed list); the web Policy Admin's fixed facts if it lists endpoint fields (`crates/web/src/app/policy.rs`, `crates/policy/src/admin/`) | each rule's pass and fail; persona tests still show only skill-folder differences; `policy check` green | Strongest (cross-crate, POL-2 network change) | Yes to build and review; applied on hosts after the acceptance run's H-1 |
| H7 | **Red-team additions** | `policies/redteam/checklist.md`, `policies/redteam/isolation.sh` (RT-11 to RT-13b), `policies/redteam/exposure.sh` (new; RT-14), wording | `bash -n`; `shellcheck` if CI has it; a dry run that prints the probes without OpenShell | Strongest | Yes to write; runs only on a host |
| H6 | **Docs and wording** | `docs/runbooks/install.md` (per-profile "Before you start", preflight first, two Stage 3 paths, OpenShell install and lingering, exposure and firewall), `README.md`, `docs/runbooks/README.md`, `upgrade.md` (runner label `pmos-host`, any architecture), `backup-restore.md` (encrypted off-host copies), `incidents.md`, `daily.md`, `deploy/compose.yaml`, `deploy/pmos-admin`, `deploy/Dockerfile` comments, `policies/README.md`, `.github/workflows/evals-gate.yml` (label, `PMOS_HOST_DIR` with `PMOS_SPARK_DIR` still read), `docs/toolserver.md:1078`, `docs/gateway.md` wording (ADM-3's meaning kept) | the docs lint, if any; CI's workflow syntax | Lighter | Yes |
| — | **Acceptance run on the first host** (§6) | `docs/hosts/<date>-<kind>.md` (new) | the run itself | Operator with the conductor; reviewer checks the record | Needs the host |
| H5 | **Launch fixes** | `deploy/pmos-admin` (`sandbox` flags as verified, shared with O-CANARY; `--install-unit`; `pause`/`resume` drive the unit), `deploy/systemd/pmos-worker@.service` (new), `deploy/Dockerfile.worker` (numeric `USER` if needed), `docs/sandbox-policies.md` ("Creating a bot's sandbox") | `scripts/test-pmos-admin.sh` (the command it would run, printed with `--dry-run`); `systemd-analyze verify` on the unit; E-5 on the host | Medium | After the acceptance run's step 2 (the flags must be known); the unit can be drafted before |
| H8 | **Tier 2, Kubernetes** | a separate spec first | — | Strongest | Only if decision O2 says so |

Order: H1 and H4 can run in parallel (different parts of `deploy/pmos-admin`: the conductor merges
H1 first). H2 after H4 (both edit `compose.yaml` and `pmos-admin`). H3 after the owner's O6 and
in parallel with H2. H7 and H6 after H2 and H3, since they describe them. Then the acceptance run,
then H5.

Each batch goes to the reviewer session as the security gate, as usual.

## 8. Open decisions for the owner

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
   until someone runs the acceptance run on such a host with `local`.
4. **O4. A hosted model for real work on a host without a GPU?** Today ADM-3 says no: prompts
   never leave the host. Recommendation: **keep ADM-3** and keep `hosted-test` fake-data only,
   enforced (§3.6), until requirement i is planned; decide ADM-3 together with requirement i
   (which providers, which data classes may go to which provider, where the keys live).
5. **O5. The web front end on Vercel, with this back end?** The repo has no Vercel deployment:
   `pmos-web` serves the built app and is also its back end, holding its runtime and tool-server
   tokens and the keys that sign user assertions. Recommendation: **keep the web app on the PM OS
   host behind the reverse proxy.** A Vercel front end would need `pmos-web`'s API reachable from
   the internet with cross-site sessions, and would be a separate security review.
6. **O6. Pin the three names' addresses in the policy (`allowed_ips`)?** Recommendation: **yes**
   (§3.5). It narrows every policy, fails closed on a wrong name, and costs a code change for the
   rare site that must move the subnet.
7. **O7. The console on a public host.** Recommendation: **never on a public listener**; reachable
   over a private network, VPN or SSH tunnel only, still behind its SSO proxy.
8. **O8. Worker supervision with systemd.** Recommendation: **yes** (§3.7): without it, a reboot
   leaves bots stopped until someone logs in, which D2 doesn't allow.

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
