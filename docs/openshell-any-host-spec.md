# OpenShell on any Linux host: one platform-agnostic deployment

Status: **design spec; not built.** The reviewer checks it before any slice starts (CLAUDE.md,
scope item c). Revised on 2026-11-22 after the reviewer's review of `9de1a65` ("sound after
changes"; §10 lists what changed), again after the reviewer's re-review the same day (§11), and
a fourth time for the owner's change of direction later the same day: **the DGX Spark is no
longer a deployment target**, real data may be processed by the owner's hosted model as well
as by a local model, and decisions D2, D3, O1 and O6 are answered (§12 summarises it all and
ends with the points the reviewer must check). Text new in this revision cites `claude/web-app`
at `57e2e72`; text kept from earlier revisions cites `6bb806f` unless it says `2544d01` (the §2
tables are the inventory as it was made, kept as the record of why each slice exists). Line
numbers may have moved since; the slice that edits a file re-checks them.

**Where this revision overrides earlier text.** §10 and §11 are the record of earlier reviews
and are not rewritten; where they say "the Spark", `spark`, `PMOS_HOST_PROFILE`, H-PROFILE,
H11, ADM-3 or "fake-data mode off the Spark", §12 and the sections it names apply instead.

**Slice H0 status (2026-11-22): built on `claude/any-host-h0`, not yet reviewed or merged.**
What it changed: `pmos-admin sandbox` creates with `--from`; the worker runs as the systemd user
unit `deploy/systemd/pmos-worker@.service` (instance made with `systemd-escape`, `%I`,
`Restart=on-failure`, `StartLimitIntervalSec=600`, `StartLimitBurst=5`, `After=`/`Wants=`
`openshell-gateway.service`), installed only with lingering on and never as root; `pause`
disables and stops it, `resume` reinstalls, clears and starts it; `sandbox --dry-run` prints the
commands without tokens; the worker image has a `sandbox` account (10100:10100) and ends with
`USER 10100:10100`; the `127.0.0.1` hosts line is gone from the install runbook, with a "stop
until H2" note; bot names are checked as `valid_name` in `add-bot`, `sandbox`, `pause` and
`resume`, and tokens are found by exact key, not a `sed` pattern. The same flaw was looked for
elsewhere, and fixed: `deploy/.env` is now read as data (`load_env`), not sourced as shell, so
no value in it is ever run; `set_env` refuses any character outside a plain set; `setup.sh`
refuses a `--content` or `--state` that isn't an absolute path of plain characters, and passes
`--clone` to git after `--`; `evals-gate` refuses a `--base` that starts with `-`. Tested in
`scripts/test-pmos-admin.sh` with stand-ins for `openshell`, `systemctl`, `loginctl` and `id`
(the unit also through `systemd-analyze verify`). **Not verified:** the worker image was not
built here (no Docker daemon), and nothing ran on OpenShell: `--from` with a local tag, which
identity OpenShell uses, whether `sandbox exec` stays in the foreground and the gateway unit's
name are still for the acceptance run (§6, step 2) and H5. The `ExecStartPre=` preflight comes
with H1.

**Slice H0b status (2026-11-22): built on `claude/any-host-h0b`, not yet reviewed or merged.**
The fixes for the security review of `6bb806f`: no token on a command line (`pmos-admin`'s
curl calls read the bearer header with `--config -`; `evals` and `build` pass `-e NAME` and
`--build-arg NAME` with the value in the environment; the test stack's scripts export their
services' settings instead of passing them to `env`; `setup.sh` refuses a clone URL with
credentials); `pause` refuses root, says plainly when the unit is missing or can't be
stopped, always stops the sandbox and exits 1 with what is left; `pause` and `down` go on, with
a warning, when `deploy/.env` can't be parsed or has the wrong mode; `deploy/.env` is refused
when every account can read it or its group can write it, warned about when its group (or an
ACL) can read it or another account owns it, and rewritten by a rename under a `flock` lock,
keeping owner and ACL; `revoke`, `smoke`, `logs` and `restart` take only a handle, a known
provider or a known service (and `smoke` writes its JSON with jq or python3); the eval gate
passes `--base` after `--end-of-options`; the bring-up and eval workers run as the checkout's
owner; `sandbox` updates a provider that already exists (`provider get`, then `provider update`
or `provider create`, from OpenShell's providers page). **Not verified on OpenShell:** the
provider commands' exit codes and that `provider create` refuses an existing name, and
`openshell sandbox stop`/`start`, which `pause` and `resume` use but OpenShell's current
public docs don't show (they show `sandbox delete`).

**The owner's requirement, 2026-11-22:** "I want to deploy it in another platform apart from
spark", and, asked which cloud, "OpenShell on another platform generic implementation". So PM OS's
bots must be able to run in NVIDIA OpenShell sandboxes on hosts other than the DGX Spark, through
one host-independent implementation that is not tied to a single cloud. Later the same day:
**"Deploy on spark scrap that idea I want to make it platform agnostic."** So PM OS runs on any
suitable Linux host with OpenShell, and **the DGX Spark is no longer a deployment target**: no
host profile, check, default or runbook is specific to it. A Spark is just one possible GPU
host, with no special status.

**What in H0 and H0b was Spark-worded, and is generalised in slice H6 (no behaviour change):**
the comment heads of `deploy/pmos-admin` ("PM OS operations on the Spark") and `deploy/setup.sh`
("on the Spark"), the install runbook's "OpenShell: installed on the Spark" line, the
`pmos-admin` comment that names "the Spark's eval-gate runner" and "Registering the Spark CI
runner" (`deploy/pmos-admin:82-83`), and the eval gate's `PMOS_SPARK_DIR`, which `evals-gate`
and the workflow still read. Nothing H0 or H0b decides depends on the host being a Spark: name
validation, `--from`, the unit, the numeric `USER`, `load_env`, `set_env` and the token
handling hold on every host as written.

**What has been verified, said first:** nothing in this repository has run on a real OpenShell
anywhere. Every policy file, launch command and red-team check was written from OpenShell's
documentation (`docs/sandbox-policies.md`, "Sources"). So this spec defines the checks that must
pass on a real host before the docs may call that kind of host verified. Every
statement about OpenShell's behaviour that this repo has not seen happen is marked **verify on a
real host**. The same holds for the fixes in slice H0: they are written from OpenShell's public
docs, and none of them counts as verified until the acceptance run (§6).

**Naming.** "Decision D1" to "D3" and "O1" to "O8" are this spec's decisions for the owner
(§8; all answered on 2026-11-22). `docs/bot-decisions.md` has its own D1 to D12; this spec
writes those as "bot decision D2" and so on. "The owner's hosted model" is the owner's
connected model provider, an OpenAI-compatible endpoint, the same model used for the eval runs.
This spec does not name the vendor or its host, and the design keeps the host out of every
committed file and every report (§3.6), since whether the provider's host name may appear in
committed eval reports is still the owner's question (CLAUDE.md, "Waiting on the owner").

## 0. Goal and non-goals

**Goal.** One deployment kit (`deploy/`) and one set of reviewed sandbox policies (`policies/`)
that run PM OS's bots in OpenShell sandboxes on any single Linux host with Docker that meets
OpenShell's requirements: a VM on any cloud or an on-prem server, amd64 or arm64, with or
without a GPU. Its model is either a local model server on the same host (a GPU host the owner
controls) or the owner's hosted model reached through PM OS's gateway (§3.6); both may serve
real data. There is no host-specific profile.

What a host needs is checked by a command before anything is installed and again at every start
(§3.2), and stated as pass or fail per requirement.

**Non-goals.**

- **No per-cloud code.** No AWS, GCP or Azure APIs, images, templates or managed services. A cloud
  VM is just a Linux host. The one place clouds differ is how the metadata check (H-METADATA,
  §3.2) reads the answer from `169.254.169.254`; it reads one address, with no SDK and no account.
- **No serverless container platforms** (§4, "Unsupported").
- **No multi-host PM OS.** One host runs the whole stack. Kubernetes is Tier 2, designed in
  outline only (§4), and not for the pilot (O2).
- **No model switching, fallback or second provider.** That is requirement i (CLAUDE.md): not
  started, not designed here. This spec designs only the minimum for **one** configured hosted
  model, from an owner-approved list, to serve real data through a `class: production` gateway
  (§3.6). One gateway serves one model source; mixing sources across roles is requirement i.
- **No change to what a sandbox may reach.** The three PM OS endpoints, their routes, the MCP
  methods, the binaries, the credentials and the filesystem rules stay exactly as reviewed. The
  one policy change (§3.5, each of those three names pinned to its own address, decision O6)
  narrows, never widens. No sandbox ever reaches the hosted provider or holds its key.

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
| Custom images | `--from` takes an OCI image reference; "the target must declare an explicit numeric `USER UID:GID`" (said for Docker updates). Re-read for H0 on 2026-11-22: `--from` takes "an explicit OCI image reference or a rootfs tar archive", "does not build local Dockerfiles or directories", and a local image is built and tagged "with the container engine used by your local gateway"; `sandbox exec` is "a one-shot command inside a running sandbox", with nothing said about long-running commands. | [`sandboxes/overview.mdx`](https://github.com/NVIDIA/OpenShell/blob/main/docs/how-it-works/sandboxes/overview.mdx) | Reported; see §2 surprises 2 and 3 |
| Gateway unit | The Linux package's gateway is the user service `openshell-gateway` (`systemctl --user status openshell-gateway`); `sudo loginctl enable-linger $USER` keeps it running after logout. Read for H0 on 2026-11-22. | [`about/installation.mdx`](https://github.com/NVIDIA/OpenShell/blob/main/docs/about/installation.mdx) | Reported; verify on a real host |
| Resources | No GPU, RAM or disk requirement is stated. | support matrix | — |
| Cloud | Not named, except Amazon Linux 2023+ for the glibc requirement. Serverless platforms are not documented. | support matrix | — |

## 2. Inventory: where the repo assumes the Spark

Classes: **(a)** truly Spark-only (local GPU model serving); **(b)** a host-specific value that
should become a setting or a generated, checked value; **(c)** wording only. "Fixed" means the
value is not host-specific after all and stays a constant. Line numbers are at `2544d01`, except
the `crates/gateway` rows, re-checked at `6bb806f`.

**Read with the change of direction (§12).** These tables were made while the Spark was still a
target. Where a row's "Change" says the Spark keeps a behaviour (the `spark` profile, ADM-3 "kept",
`local` only on the Spark, the hosted path testing-only, decision O4), §3.6 and §12 replace it:
there is no `spark` profile, ADM-3 is replaced by the approved-hosted-model rule, and O4 is
withdrawn. Every other row stands. The complete, current list of the word "Spark" in the repo,
with what happens to each, is H6's inventory (§7.1), which supersedes the "(c) wording" rows
here.

### Deployment kit

| Where | What it assumes | Class | Change (slice) |
| --- | --- | --- | --- |
| `deploy/compose.yaml:1`, `:12-16` | "PM OS on the DGX Spark"; sandboxes reach the services on 127.0.0.1 "(see /etc/hosts in the install runbook)" | (b) and (c) | Pinned service addresses (§3.3); wording (H2, H6) |
| `deploy/compose.yaml:31-34`, `:291` | "The Spark's outbound proxy" | (c) | "This host's outbound proxy" (H6) |
| `deploy/compose.yaml:143-162` (`gateway`) | No `<<: *proxy`, no `PMOS_GATEWAY_REMOTE_KEY`, no `PMOS_GATEWAY_CLOUD_TEST`: the gateway container cannot reach a hosted model at all today, through a proxy or not | (b) | Added with `PMOS_MODEL_SOURCE` (§3.6, H4; surprise 11) |
| `deploy/compose.yaml:284-341` (`web`) | `PMOS_WEB_INTERNAL_BIND` not passed, so the server-to-server route is on the browser-facing listener | (b) | Passed on its own port (§3.8, H9) |
| `deploy/compose.yaml:364-402` | `vllm-default`, `vllm-extraction`: `nvcr.io/nvidia/vllm:25.10-py3`, the NVIDIA device reservation, GPU memory shares sized for the Spark | (a) | Stays in the `models` profile; used only with `PMOS_MODEL_SOURCE=local` (§3.6, H4); each gets a pinned `ipv4_address` (§3.3, H2) |
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
| `crates/gateway/src/config.rs:671-689` (`is_local_host`) | For a `class: test` gateway, "local" means loopback, **any** private IPv4 address, link-local (`169.254.0.0/16`), IPv6 unique-local, any name without a dot, `*.pmos.internal`, or a `local-hosts` entry. It checks the text of the host, not what it resolves to | (b): on the Spark's LAN a private address was the company's; on a cloud VM it is the provider's network, where hosted model endpoints can sit on private addresses | Allowed only with `hosted-test`, where every source is fake (§3.6, H4; surprise 13) |
| `crates/gateway/src/config.rs:375-380`, `:543-557`, `:691-711` (`is_production_host`) | A `class: production` gateway (`deploy/gateway.yaml`) already refuses `local-hosts`, testing-only upstreams and any model server not named `<label>.pmos.internal` (review finding F3); it does not check what such a name resolves to | (b), mostly already done | `local` requires `class: production`; the names' answers checked at every resolution (§3.6, H4) |
| `crates/gateway/src/config.rs:470-515` | A remote test upstream needs `PMOS_GATEWAY_CLOUD_TEST=1`; anything else must pass `is_local_host` | (b) | Also bound to `PMOS_MODEL_SOURCE` (§3.6, H4) |
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
   **Superseded on 2026-11-22:** the owner decided that the hosted model may process real data
   (§8, D1), and the minimum for one approved hosted model is designed here (§3.6), not in
   requirement i.
8. **CLAUDE.md item i says "OpenShell holds one credential per provider".** Today the gateway is
   a Compose service, not in a sandbox, so OpenShell cannot hold its upstream key: the key is
   `PMOS_GATEWAY_REMOTE_KEY` in `deploy/.env`. Moving that key into OpenShell is part of
   requirement i's design, not this one (§3.6, "Where the hosted model's key is held").
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
13. **"Local" in a test gateway includes any private address; a production gateway is already
    narrower.** `is_local_host` (`crates/gateway/src/config.rs:671-689`) accepts every RFC 1918
    address, link-local and IPv6 unique-local, and a dotless name, without resolving anything. On
    a cloud VM, a hosted model reached through a private endpoint (an address in the VPC) passes
    it, with no `PMOS_GATEWAY_CLOUD_TEST`. Since review finding F3, a `class: production` gateway
    (`deploy/gateway.yaml`) takes none of that: it refuses `local-hosts`, testing-only upstreams
    and any host not named `<label>.pmos.internal` (`config.rs:375-380`, `:543-557`,
    `is_production_host` at `:691-711`). What production still doesn't check is what such a name
    *resolves to*: a VPC resolver, a search domain or an `/etc/hosts` line that answers
    `vllm-default.pmos.internal` with an address off the host would pass. So the gap is two
    things: nothing ties live data to a `production` gateway (a `class: test` config with a
    private upstream runs with live connectors today), and nothing checks a production name's
    answer. §3.6 closes both.

## 3. Design

### 3.1 The host settings: three in `deploy/.env`

The smallest mechanism is not a new file format. It is three settings in `deploy/.env`, written
by `setup.sh`, read by `pmos-admin` and the preflight, and **never read by the policy
generator**. `PMOS_MODEL_SOURCE` and `PMOS_EXPOSURE` are also passed to the services that enforce
them (§3.6, §3.8), so the refusals hold however the stack is started.

| Setting | Values | Default written by `setup.sh` | What it decides |
| --- | --- | --- | --- |
| `PMOS_MODEL_SOURCE` | `local`, `hosted`, `hosted-test` | `hosted-test` unless `setup.sh --model-source` names another | Where prompts go (§3.6). `local`: a model server on this host (the `models` profile, NVIDIA runtime required), a `class: production` gateway, real data allowed. `hosted`: the owner's hosted model, which must be an entry on the approved list, through a `class: production` gateway, real data allowed once the entry's conditions are recorded. `hosted-test`: any OpenAI-compatible endpoint, not approved, through a `class: test` gateway, **fake data only** |
| `PMOS_EXPOSURE` | `private`, `public` | `public` unless `setup.sh --exposure private` | Who can reach the sign-in page. `public`: the host has a public address or sits on a network you don't control; password sign-in only from an allow-list (decision D2 (a), §3.8) and development sign-in refused. `private`: the host is reachable only from a network the company controls; H-PRIVATE checks that the host looks so (§3.2) |
| `PMOS_OPENSHELL_DRIVER` | `docker` | `docker` | The OpenShell compute driver. Only `docker` is Tier 1; `podman` and `kubernetes` are refused until verified (§4) |

**What became of `PMOS_HOST_PROFILE`.** The previous revision had a fourth setting,
`PMOS_HOST_PROFILE` (`spark` or `linux`), which chose the Spark-only preflight checks and,
until decision D1, was the only thing that allowed real data. With no Spark target and D1
answered, it decides nothing: every host gets the same checks, and whether real data is allowed
follows from the model source alone (§3.6). So it is **not added**, and neither is the
`PMOS_HOST_PLACE` declaration H11 would have added. No generic value replaces it: a setting
that every host sets to the same thing is not a setting.

**Defaults that allow less.** Each default is the value that allows less: `hosted-test`
refuses every real source (§3.6), and `public` applies the stricter sign-in rules (§3.8). The
values that allow more (`local`, `hosted`, `private`) must be chosen, and are checked:
`local` needs the NVIDIA runtime (H-NVIDIA), `hosted` needs an approved entry whose conditions
are recorded (the gateway refuses otherwise, §3.6), and `private` must match what the host shows
(H-PRIVATE, §3.2). `setup.sh` fills in missing values only; no host was ever installed with an
earlier revision, so there is nothing to migrate.

**These settings are declarations, not measurements.** Nothing in a service can prove that a host
is not reachable from the internet: an operator can write `private` on a host with a public
address. H-PRIVATE checks it against what the host shows, and a mismatch fails until an
operator records an acknowledgement that `status` shows every time (§3.2). That makes a wrong
declaration visible and deliberate, not impossible, because the check runs from `pmos-admin`
and the worker unit only, and the services see only the declaration (§5). `PMOS_MODEL_SOURCE`
is different in kind: what it allows is enforced by the gateway against reviewed code and a
reviewed list, not by a declaration (§3.6).

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
- **Which hosted endpoints may see real data.** That is the approved list, a reviewed file
  compiled into the gateway (§3.6), never an environment variable.

So **the policy files stay host-independent**: one reviewed set in the content repo works on
every Tier 1 host.

### 3.2 The preflight: `deploy/preflight.sh` (and `pmos-admin preflight`)

A script that checks a host, and prints pass, fail or warn per requirement with the measured
value. It needs only `bash`, coreutils and `python3` (already needed by `pmos-admin ledger`). It
changes nothing on the host.

```
deploy/preflight.sh [--model-source local|hosted|hosted-test]
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
| H-OS | Debian or Ubuntu | `/etc/os-release` `ID`, `ID_LIKE` | other distributions: **warn**, not fail ("not in OpenShell's support matrix") |
| H-GLIBC | glibc 2.28+ (OpenShell's gateway) | `getconf GNU_LIBC_VERSION` | older |
| H-KERNEL | Linux 6.2+ | `uname -r` | older. Passing this alone proves nothing, so it never stands in for H-LANDLOCK |
| H-LSM | Landlock enabled | `landlock` in `/sys/kernel/security/lsm` | absent (a kernel built with Landlock but not listed in `lsm=` fails) |
| H-LANDLOCK | Landlock ABI 3+ | a `python3 -I` probe calling `landlock_create_ruleset(NULL, 0, LANDLOCK_CREATE_RULESET_VERSION)` (syscall 444 on both architectures), which returns the ABI | below 3, or the call fails. Not measurable: **fail**, with the reason |
| H-SECCOMP | seccomp available | `Seccomp:` in `/proc/self/status`; `CONFIG_SECCOMP_FILTER` where `/proc/config.gz` or `/boot/config-*` exists | absent |
| H-CGROUP | cgroups v2 | `/sys/fs/cgroup/cgroup.controllers` exists | v1 only (needed by Podman; recorded for Docker) |
| H-DOCKER | Docker Engine 28.0+, daemon reachable, Compose v2 | `docker version --format '{{.Server.Version}}'`, `docker compose version` | missing, older, or the daemon not reachable by this user |
| H-DOCKER-SECCOMP | Docker runs containers under its default seccomp profile | `docker info --format '{{.SecurityOptions}}'` contains `seccomp` | `seccomp` missing or `unconfined` |
| H-NVIDIA | NVIDIA container runtime | `docker info` lists an `nvidia` runtime; `PMOS_VLLM_IMAGE` set (it has no default any more, §3.6) | missing, **only** when `PMOS_MODEL_SOURCE=local` (fail). Not checked for `hosted` or `hosted-test` |
| H-EGRESS | the gateway can reach the hosted endpoint (`hosted` and `hosted-test` only) | the gateway's own `/readyz` label for its hosted upstream (§3.6), never a request made by the preflight with the key. Only once the gateway runs: `pmos-admin status` and `preflight` by hand; at `up` and `install` it reports "not started", which passes | `down`: **fail** for `hosted`, **warn** for `hosted-test`; the hint names `HTTPS_PROXY` and `NO_PROXY` |
| H-PORTS | 8710, 8740, 8750, 8760, 8770 free on 127.0.0.1 | `ss -ltnH` | in use by anything other than this stack |
| H-SUBNET | the `pmos` network as pinned | `ip route`; at stage `run`, `docker network inspect pmos_pmos` (subnet, gateway, `ip_range`, bridge name) | `PMOS_NETWORK_SUBNET` shorter than a `/24` (a `/8` or `/16` would hold the pinned addresses and much else besides); a route overlapping it other than Docker's own `pmos0` bridge; a live network whose subnet, gateway, `ip_range` or bridge name differs from `deploy/.env` and `deploy/compose.yaml` (the fix: `pmos-admin down`, then `up`, which re-creates it); an `ip_range` that contains a pinned address (§3.3), or a `PMOS_WEB_TRUSTED_PROXIES` in-network address that is one. The refusal of a subnet other than the policy's constant belongs to H3 (§3.5) |
| H-NAMES | **the host's view**: the three names resolve on the host to their pinned addresses, and nothing maps them to loopback | `getent ahostsv4` for each name, on the host; the managed block in `/etc/hosts` | a wrong or loopback address. At stage `install` this reports "not set yet", which passes. It does **not** show what a sandbox gets: OpenShell resolves through "its trusted resolver", which may not be the host's (§1). The sandbox's view is O-REACH |
| H-METADATA | PM OS's containers cannot get the VM's cloud credentials | after install, from a container on the `pmos` network (`docker run --rm --network pmos_pmos`), with a 2 s timeout: the rule per answer below; and, where `host-firewall apply` was run, that its rules, its `docker.service` drop-in and its timer are in place | per the rule below: **fail**, on every host (§5). The previous revision only warned on `private`; that existed to spare an office box on a LAN, which answers "no answer" and passes anyway, so the one rule for every host is the stricter one |
| H-EXPOSURE | nothing PM OS runs listens on a non-loopback address except the reverse proxy | `ss -ltnH` on the host: 8710, 8740, 8750, 8760, 8770, 8771 only on `127.0.0.1`; 8772 (the admin listener, §3.8) only on `PMOS_WEB_ADMIN_PUBLISH`; OpenShell's 17670 only on `127.0.0.1` or the host's address on the Docker bridge OpenShell's driver uses (`docker0` by default; which one it binds is **verify on a real host**). The containers' own addresses on `pmos0` are not host listeners and don't appear here | otherwise, **fail**, on every host (the previous revision failed only on `public` and on an acknowledged `private`; no host needs a PM OS port on another address, so the rule is the same everywhere) |
| H-DEVLOGIN | development sign-in off | `PMOS_WEB_ENABLE_DEV_LOGIN_CLOSED_TEST_NETWORK_ONLY` | `1` with `PMOS_EXPOSURE=public` or an acknowledged `private` (fail). `pmos-web` refuses the same itself from H10 (§3.8) |
| H-SIGNIN | sign-in as decision D2 (a) says | `PMOS_EXPOSURE`, `PMOS_WEB_PASSWORD_LOGIN`, `PMOS_WEB_PASSWORD_CLIENTS` (§3.8) | until H10 is merged: `public` with the web profile on (fail: "pmos-web does not serve a public host yet"). From H10: `public` with password sign-in on and `PMOS_WEB_PASSWORD_CLIENTS` empty, or holding an entry shorter than `/16` (IPv4) or `/48` (IPv6) (fail) |
| H-ADMIN | the admin routes on their own listener (decision D3 (b), every host) | `PMOS_WEB_ADMIN_BIND`, `PMOS_WEB_ADMIN_PUBLISH` (§3.8); from H10 | the web profile on with `PMOS_WEB_ADMIN_BIND` unset, or `PMOS_WEB_ADMIN_PUBLISH` outside loopback, RFC 1918, `100.64.0.0/10` and IPv6 unique-local (fail) |
| H-PRIVATE | a host declared `private` looks private | any interface address outside RFC 1918, `100.64.0.0/10`, loopback, link-local and IPv6 unique-local and link-local (`ip -j addr`); and H-METADATA's probe getting any answer. With the acknowledgement, also `ss -ltnH` for anything on the host listening on 80 or 443 | `PMOS_EXPOSURE=private` with either, unless acknowledged (below): **fail**. With the acknowledgement it passes as **warn**, and H-DEVLOGIN applies at `public` strength (H-METADATA and H-EXPOSURE already fail on every host); and it **fails** if any process (PM OS's or not) listens on 80 or 443 on an address outside the private ranges above, or on the unspecified address (`0.0.0.0`, `::`) while the host has a public address, so an operator's own proxy in front of 8770 can't put a sign-in page that accepts passwords from anywhere on the internet |
| H-ASSERT | `PMOS_ASSERTION_MODE=require` | `deploy/.env` (the same rule as `assertion_checks`) | anything else while a web or console token is set |
| O-CLI | OpenShell CLI installed (with `--openshell`) | `openshell --version`, `openshell status` | missing, or the gateway not reachable |
| O-GATEWAY | OpenShell's gateway is local and authenticated | the gateway endpoint the CLI uses (`openshell status`) is loopback or the Docker bridge, over TLS | a non-loopback plaintext endpoint |
| O-LINGER | the gateway and the workers survive logout and reboot | `loginctl show-user "$USER" -p Linger` is `yes`, and the OpenShell gateway's user unit is enabled | not set (fail) |
| O-CANARY | a sandbox actually starts under PM OS's settings | creates `pmos-preflight` from the worker image with a minimal policy (no network rules, `landlock: hard_requirement`, `run_as_user: sandbox`), runs `/usr/local/bin/node -e 'process.exit(0)'`, reads the `Landlock ruleset built` event, deletes the sandbox | the sandbox does not start, Landlock was not applied, or node isn't at that path |
| O-REACH | **the sandbox's view**: a sandbox reaches the three services by name | a second canary, `pmos-preflight-net`, whose policy allows only `GET /healthz` on the three names and ports (with each name's `allowed_ips` once H3 is in), runs a `node` one-liner that fetches each `/healthz` | any of the three doesn't answer `200`. This is the automated form of H-1 (§3.3) |

**H-METADATA, by answer.** The first version said "reachable: fail". On GCP and Azure the
metadata server answers containers on any VM and no VM setting stops it, so that check failed
every `public` host there. The check now asks what matters (can a container get the VM's cloud
credentials?) and never asks for a credential. Exactly three requests can be sent, and nothing
any of them returns is stored, logged, printed or put in the `--json` output; only the status
line, the `Metadata-Flavor` header's presence and whether any account is listed are kept:

- `GET /` with no headers (every cloud);
- on AWS, `PUT /latest/api/token` with `X-aws-ec2-metadata-token-ttl-seconds: 1`. This asks for
  an **IMDSv2 session token**: a token that only lets its holder read the metadata service for
  one second. It is not a cloud credential (those are at `/latest/meta-data/iam/security-credentials/`,
  which the probe never requests), but it is still a token, so the probe reads only the status
  line and discards the body unread;
- on GCP, `GET /computeMetadata/v1/instance/service-accounts/` with `Metadata-Flavor: Google`,
  which lists account names; the probe keeps only whether the list is empty. It never requests
  `…/token`.

Azure's token endpoint (`/metadata/identity/oauth2/token`) is never requested on any cloud.

| What the probe sees at `169.254.169.254:80` | Passes when | Otherwise |
| --- | --- | --- |
| No answer | always (on-prem, a cloud with metadata off, or the block below in place) | — |
| AWS (the `PUT` above; its body, if any, is discarded unread) | the `PUT` gets no answer from the container: IMDSv2 with a hop limit of 1 | fail: "set the instance's metadata to IMDSv2 required with hop limit 1, or apply the block" |
| GCP (an answer carrying `Metadata-Flavor: Google`; one read of `instance/service-accounts/`, which lists account names only) | no service account is attached | fail: "detach the service account, or apply the block" |
| Azure (`400` without the `Metadata: true` header), or any other answer | never: whether a managed identity exists can only be learned by asking for a token, which the check won't do | fail: "apply the block" |

**The block, one generic fix for every cloud:** `pmos-admin host-firewall apply` (with `sudo`)
adds rules to Docker's `DOCKER-USER` chain that drop traffic from `PMOS_NETWORK_SUBNET` to
`169.254.169.254`, `168.63.129.16` and `100.100.100.200`, and from the network's IPv6 range (if
any) to `fd00:ec2::254`. Each rule matches the input interface as well as the source: `-i pmos0`
(the `pmos` network's bridge, named in H2 so the name doesn't change when the network is
re-created) and `-i docker0` (Docker's default bridge, which an operator's plain `docker run`
uses). It is idempotent and tagged with a comment so `check` can find it.

**Keeping the block in place.** A Docker daemon restart (an upgrade, a crash, `systemctl restart
docker`) rebuilds Docker's chains and brings the containers back through their restart policy
without `pmos-admin up`, so a check at `up` alone would miss the gap. `host-firewall apply`
therefore also installs, with the same `sudo`:

- a drop-in, `/etc/systemd/system/docker.service.d/pmos-metadata-block.conf`, whose
  `ExecStartPost=` runs `/usr/local/sbin/pmos-metadata-block apply` (the same rules, from a
  root-owned copy of the script, reading the subnet from a root-owned file written at `apply`, not
  from `deploy/.env`);
- a timer, `pmos-metadata-block.timer`, that re-applies the rules every minute, for a firewall
  reload (`ufw reload`, `firewall-cmd --reload`, `nft flush ruleset`) that doesn't restart Docker.

So the gap after a Docker restart is the time until `ExecStartPost` runs (seconds; containers may
start before it), and after a firewall reload at most a minute. `host-firewall check`, H-METADATA
and `status` report a missing rule, drop-in or timer. `host-firewall remove` takes all three
away. With the block, every row above becomes "no answer". The rules do not affect the host
itself, so the cloud's own agent keeps working. Which form a GCP or Azure VM's answer takes, that
the block holds, and that the drop-in runs after Docker has made `DOCKER-USER`, are **verify on
a real host** (RT-16).

**Acknowledging a mismatch (H-PRIVATE only).** A `private` declaration that doesn't match the
host fails until an operator runs `pmos-admin host acknowledge H-PRIVATE --reason "<text>"` (with
`sudo`, so it needs the same rights as `host-firewall`), for example on a closed test VM whose
public address is blocked inbound at the cloud's firewall. H-PRIVATE is the only check that takes
an acknowledgement: `pmos-admin host acknowledge` refuses every other check id with exit 2, and a
`PMOS_HOST_ACK_<CHECK>` line for any other check in `deploy/.env` is itself a fail. (H-PROFILE,
which the previous revision made unacknowledgeable, is gone with the Spark target.) An
acknowledgement records:

- in `deploy/.env`, `PMOS_HOST_ACK_H_PRIVATE=<fingerprint>`, where the fingerprint is a SHA-256
  of what the check saw (the sorted public addresses and whether the probe answered). If the
  host later shows something else (a new public address), the fingerprint no longer matches and
  the check fails again;
- in a root-owned, append-only log, `/var/log/pmos/host-acknowledgements.log` (`chattr +a` where
  the filesystem supports it), one line per acknowledgement: time, the operator's login
  (`SUDO_USER`), the check, what it saw, the fingerprint and the reason.

`pmos-admin status` prints every acknowledgement in force on its own line, every time
(`host: PMOS_EXPOSURE=private acknowledged on 2026-11-23 by ana: public address 203.0.113.7;
"closed test, inbound blocked at the security group"`), the preflight's `--json` includes it, and
the acceptance record (§4) must list it. `pmos-admin host acknowledge --clear <check>` removes the
`deploy/.env` line and logs that too. There is no way to acknowledge a check from `deploy/.env`
alone: a line there without its log entry fails the check.

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
   ports stay published on `127.0.0.1` too, for `pmos-admin`. The two vLLM services get
   `10.201.77.13` (`vllm-default`) and `.14` (`vllm-extraction`) the same way, because the
   gateway checks the address their names resolve to (§3.6): with a pinned address, no other
   container can hold it after a vLLM container restarts. `.10` to `.14` are listed as reserved in
   `deploy/env.example`, beside `PMOS_WEB_TRUST_PROXY_IN_NETWORK`, so an operator pinning a
   reverse-proxy container picks another.
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
   `PMOS_WEB_TRUSTED_PROXIES` stays right. The network's bridge is named `pmos0`
   (`driver_opts: com.docker.network.bridge.name: pmos0`), so the metadata block (§3.2) can match
   it by name.
   `PMOS_NETWORK_SUBNET` must be a `/24` or longer: `pmos-admin up` and H-SUBNET refuse a shorter
   prefix, since "contains the pinned addresses" alone would pass a `/8`. A prefix longer than
   `/24` must still hold the pinned addresses and an `ip_range` that holds none of them, which the
   same checks test.
3. **Moving an existing install.** Compose does not change the IPAM of a network that exists.
   `pmos-admin up` compares the live network (`docker network inspect pmos_pmos`) with
   `deploy/.env`, as `password_web_checks` already does for the gateway
   (`deploy/pmos-admin:165-180`), and refuses on a mismatch with the fix:
   `deploy/pmos-admin down && deploy/pmos-admin up` (which re-creates the network; no eval stack
   may be running). No host has been installed yet, so in practice this only matters to a test
   stack made before H2.
4. **The names resolve to those addresses through a managed block in `/etc/hosts`**, written by
   `pmos-admin host-names apply` (with `sudo`), between `# BEGIN pmos (managed by pmos-admin)` and
   `# END pmos` markers. It replaces only its own block, is idempotent, and takes no override: it
   always writes the pinned addresses (RT-13b's test swap is done by the red-team script, §5).
   `pmos-admin host-names
   check` and `status` report a missing, stale or loopback entry, and `pmos-admin sandbox` refuses
   to start a sandbox while the check fails. Nobody edits `/etc/hosts` by hand, and every host
   gets the same block.
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
| vLLM addresses | literals in `deploy/compose.yaml` only (no sandbox reaches them, so not in the policy crate) | `10.201.77.13`, `.14` |
| Unpinned range | `PMOS_NETWORK_IP_RANGE` (new) | `10.201.77.128/25` |
| OpenShell gateway endpoint | OpenShell's own CLI configuration (`openshell status`); PM OS never stores it | the local package's `https://127.0.0.1:17670`, mTLS |
| Outbound proxy | `HTTPS_PROXY`, `NO_PROXY`, `PMOS_BUILD_CA` (exist); now also passed to the gateway (§3.6) | none |
| Model source | `PMOS_MODEL_SOURCE` (§3.6) | `hosted-test` (fake data only) |
| Hosted endpoint and key | `PMOS_GATEWAY_HOSTED_URL` in `deploy/.env`; the key in `deploy/secrets/hosted-model.key` (§3.6) | none |
| Local model server image | `PMOS_VLLM_IMAGE` (exists; its default is removed, §3.6) | none: required with `local` |
| Eval runner | runner label `pmos-host`, `PMOS_HOST_DIR` (§2) | `/opt/pmos` |
| Content and state paths | `PMOS_CONTENT_CHECKOUT`, `PMOS_STATE_DIR` (exist) | `/srv/pmos/…` |

### 3.5 The policy generator, Policy Admin and drift

**The generator takes no host input.** `pmos policy generate` keeps reading only the content
tree. The host settings are invisible to it, so no setting, file or screen on a host can widen a
sandbox. The fixed parts stay in `crates/policy/src/lib.rs`, reviewed as code under POL-2, and
Policy Admin keeps showing them as `fixed` (`docs/policy-admin.md`). Policy Admin gains no host
field.

**One change to the fixed part, which narrows (decision O6, decided by the owner on 2026-11-22:
yes):** each of the three endpoints gets
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

**Drift, and how each check treats a host:**

| Check | Reads | A host can make it fail by |
| --- | --- | --- |
| `pmos policy check` (CI and `pmos-admin lint`) | content and `policies/` only | nothing: it never sees the host. A new rule, `net-allowed-ips`, requires each endpoint's `allowed_ips` to be exactly its own `/32` constant (another service's address, a wider range, a missing list, or a second entry fails, each with a failing and a passing unit test, like every rule) |
| A unit test in `crates/policy` | `deploy/compose.yaml` | nothing on a host: it fails CI when the three `ipv4_address` literals differ from the constants |
| `pmos-admin preflight` / `status` / `host-names check` | `deploy/.env`, the host | names that resolve elsewhere, a live network unlike `deploy/.env`, a host check that fails |
| `pmos-admin up` and `sandbox` | the same | `up` refuses when `PMOS_NETWORK_SUBNET` does not contain the three constants, or `PMOS_NETWORK_IP_RANGE` contains one (the policies would refuse every connection, or a service couldn't start); `sandbox` refuses while `host-names check` fails |

The subnet refusal in `up` is built in H3, with the constants it compares against; H2 checks that
the subnet is a `/24` or longer and contains the compose literals, and that the `ip_range` holds
none of them. The `PMOS_NETWORK_SUBNET` setting stays (the web
app's trusted proxy checks use it), but a value that doesn't hold the constants is refused with a
message that names the code change and POL-2.

Applying H3's change is a network change: a second platform owner reviews it, every bot's policy
file is regenerated once (the header wording changes in the same commit), and on each host
`openshell policy set` applies it live.

### 3.6 Model sources, real data, and where the hosted model's key is held

Revised on 2026-11-22 for decision D1 (§8): real data may be processed by **both** a local model
on a GPU host the owner controls **and** the owner's hosted model. This replaces ADM-3 ("prompts
never leave the host"; restricted data only on the Spark's local models). Model sources become
production options. Requirement i (many providers, switching, automatic fallback) is **not**
started and not designed here: this is the minimum for **one** configured hosted model to serve
real data through a `class: production` gateway, and one gateway serves exactly one source.

| `PMOS_MODEL_SOURCE` | Gateway config (`class`) | Upstream form | Needs | Data allowed |
| --- | --- | --- | --- | --- |
| `local` | `deploy/gateway.yaml` (`production`), `up --models` | `http://<name>.pmos.internal:<port>/v1` (today's rule) | an NVIDIA GPU and runtime, `PMOS_VLLM_IMAGE` | **real data**, after Gate 1 |
| `hosted` (new) | `deploy/gateway.hosted.yaml` (`production`, new) | `hosted:<label>` (new), `<label>` an entry on the approved list | an approved entry; `PMOS_GATEWAY_HOSTED_URL`; the key file | **real data**, after Gate 1 and the conditions in §3.9 |
| `hosted-test` | `deploy/gateway.hosted-test.yaml` (`test`) | `openai+https://<host>/<base>` (today's remote test upstream, `docs/gateway.md`), `PMOS_GATEWAY_CLOUD_TEST=1` | any OpenAI-compatible API and its key | **fake data only** (below) |

`hosted` and `hosted-test` may point at the same endpoint. The difference is who decided: a
`hosted` endpoint is on a reviewed list with the owner's recorded approval; a `hosted-test` one
is whatever an operator typed, so it never sees real data.

#### The local model path on a generic GPU host

`local` is unchanged in what it enforces: a `class: production` gateway, upstreams named only
`<label>.pmos.internal` (since review finding F3; `crates/gateway/src/config.rs:543-557`,
`is_production_host` at `:695-709` at `57e2e72`), and, from H4, a resolver that refuses any
answer for those names outside the pinned part of the `pmos` subnet (below). What changes is
that it is no longer sized or defaulted for one machine.

**What in `deploy/` is specific to NVIDIA's vLLM image or to one GPU** (`deploy/compose.yaml`,
the `vllm-default` and `vllm-extraction` services; `deploy/env.example`, "Models"):

- `image: ${PMOS_VLLM_IMAGE:-nvcr.io/nvidia/vllm:25.10-py3}`: NVIDIA's own build of vLLM, the
  one the Spark's arm64 GPU needed. **H4 removes the default**: `PMOS_VLLM_IMAGE` becomes
  required with `local` (`${PMOS_VLLM_IMAGE:?…}` in Compose; H-NVIDIA in the preflight), so a
  host's operator chooses the image for that host's architecture and GPU. The install runbook
  (H6) names two candidates, NVIDIA's build and the vLLM project's own published image, each
  **designed for; not verified** (decision O3) until an acceptance run with `local` has passed
  on such a host. Whichever is chosen, its tag is pinned in `deploy/.env`, never `latest`.
- `deploy.resources.reservations.devices: [{driver: nvidia, count: all, capabilities: [gpu]}]`
  and H-NVIDIA: the NVIDIA container runtime. Other GPU vendors are not offered.
- `--gpu-memory-utilization` shares (`0.55` and `0.30`), sized for two servers on **one** GPU.
  They stay settings (`PMOS_MODEL_*_GPU_SHARE`); a host with another GPU layout changes them, or
  runs one server for both roles, in `deploy/.env` and `deploy/gateway.yaml` (a model swap,
  `docs/runbooks/model-swap.md`).
- `ipc: host`, the `/health` health check, the `vllm serve` command line and the Hugging Face
  cache path are vLLM's own, the same in both images.

**How a generic host chooses its model server.** The gateway does not care which server it is:
it needs an OpenAI-compatible endpoint answering at `vllm-default.pmos.internal` and
`vllm-extraction.pmos.internal` on their pinned addresses (`.13`, `.14`, §3.3). The kit ships
only vLLM; another server would be a Compose change under review, not a setting.

#### The hosted model as a production source (`hosted`)

**The approved list.** A new file, `crates/gateway/approved-hosted.yaml`, compiled into the
gateway with `include_str!`, so changing it needs a reviewed, merged commit and a rebuilt image.
It is not read from `deploy/`, a mount or an environment variable. Each entry:

```yaml
# Owner-approved hosted models (docs/openshell-any-host-spec.md §3.6). Reviewed under
# CODEOWNERS; compiled into pmos-gateway. Never a host name, a URL or a key in this file.
- label: owner-hosted            # lowercase letters, digits, hyphens; what every report shows
  url_sha256: "<64 hex digits>"  # SHA-256 of the normalised base URL (below)
  models: ["<model id>"]         # the only model ids a role may name with this label
  record: docs/model-providers/owner-hosted.md   # the owner's approval and conditions (§3.9)
```

The entry holds a digest of the URL, not the URL: the endpoint is pinned without its host name
appearing in any committed file while the owner's question about provider host names in
committed files is open (CLAUDE.md, "Waiting on the owner"). The digest is not a secret (a
host name can be guessed and hashed); it is a pin. The URL itself is
`PMOS_GATEWAY_HOSTED_URL` in `deploy/.env`, and the gateway refuses to start unless its
normalised form hashes to the entry's digest. **So editing an environment variable can only
break the gateway, never point real data at another endpoint.** Normalised form:
`https://` + the host in lowercase + `:<port>` only when not 443 + the path without a trailing
`/`; a URL with user information, a query, a fragment, or a scheme other than `https` is
refused, not normalised.

**Who may change the list.** A CODEOWNERS line for `crates/gateway/approved-hosted.yaml` and
`docs/model-providers/`, owned by the platform owners, with "Require review from Code Owners" on
the default branch, the same mechanism as `policies/README.md`'s lines for bot instructions.
Those lines wait on the owner (CLAUDE.md, "Waiting on the owner"); **until they are in force, no
entry may be added**, and the slice that adds the first entry (§7, H13) is not merged before
then. A unit test in `crates/gateway` checks every entry: a valid label, a 64-hex digest, at
least one model id, and a `record` file that exists and has the three sections §3.9 requires,
each saying "confirmed by the owner on <date>". The list ships **empty** with H12.

**The gateway config for `hosted`.** `deploy/gateway.hosted.yaml`, `class: production`, every
upstream `hosted:<label>` with one label, every role's `model` one of that entry's `models`, no
`local-hosts`. H12 extends the production rule in `config.rs` by exactly this one form: a
`class: production` gateway accepts `<name>.pmos.internal` hosts (as today) **or** `hosted:<label>`
upstreams, never both in one config, and never `openai+https://` or `anthropic+https://`.

**The gateway refuses to start** with `PMOS_MODEL_SOURCE=hosted` and any of: a config whose
class isn't `production` (the stub may be on, as for `local`, and then serves as class `test`);
an upstream that isn't `hosted:<label>`, or two labels; a label not on the compiled list; a
`PMOS_GATEWAY_HOSTED_URL` that is missing, not normalisable or whose digest differs; a model id
not in the entry; `PMOS_GATEWAY_CLOUD_TEST=1`; an empty or unreadable key file. And with `local`
or `hosted-test`, it refuses any `hosted:` upstream. Each message names the setting, never the
URL.

**Where the hosted model's key is held.** In one file, `deploy/secrets/hosted-model.key` (the
directory mode `700`, the file mode `600`, owned by the platform user; `deploy/secrets/` in
`.gitignore`), mounted by Compose's `secrets:` into the **gateway container only**, at
`/run/secrets/pmos-hosted-model-key`, and read through `PMOS_GATEWAY_HOSTED_KEY_FILE`. It is
never in `deploy/.env`, so it is not in the gateway's environment, `docker inspect`,
`/proc/<pid>/environ` or any other container; never in `policies/`, the content repo, a
`gateway.*.yaml` file, a report or a log; and never on a command line: `pmos-admin hosted-key
set` reads it from standard input and writes it by a rename, as `set_env` does, and refuses a
file or directory with group or world bits, as `deploy/.env` is refused (H0b). `setup.sh` always
creates the file, empty, so Compose can mount it on every host; the gateway refuses an empty key
only under `hosted`. `pmos-admin backup` never copies it into a backup, and the backup runbook
keeps it apart from the database dump, as for `deploy/.env` (§5). Whether the gateway image's
user can read the mounted file at mode `600` is a Docker detail, tested in H12's stack test,
not an OpenShell one. The existing `PMOS_GATEWAY_REMOTE_KEY` in `deploy/.env` stays for
`hosted-test` only.

**How this relates to CLAUDE.md's "OpenShell holds one credential per provider".** That note
belongs to requirement i's design, where the gateway might run in a sandbox and OpenShell would
inject the provider key into its egress. Today the gateway is a Compose service, not in a
sandbox, so OpenShell cannot hold its key; this spec gives the key to the gateway's container
alone. **No sandbox ever holds the provider's key or reaches the provider**: a worker holds only
its own gateway token's placeholder, its policy allows only `POST /v1/chat/completions` on
`gateway.pmos.internal`, and `net-egress` keeps the provider's host out of every policy. If
requirement i later moves the key into an OpenShell provider, the key file goes away; nothing
here prevents that.

**Egress.** The gateway's `environment:` gains `<<: *proxy` (H4), so with `HTTPS_PROXY` set the
hosted calls go through the host's proxy; the hosted host must not match `NO_PROXY`, which
`pmos-admin up` checks under `hosted`. Without a proxy, the gateway resolves the hosted host
itself, and its resolver (the one H4 adds for `local`) refuses for a `hosted:` upstream any
answer that is loopback, link-local, unspecified, one of the metadata addresses of §3.2, inside
`PMOS_NETWORK_SUBNET`, or private (RFC 1918, `100.64.0.0/10`, IPv6 unique-local): so a DNS answer
can't send the key and the prompts to a service inside the host or the VPC. A provider reached
over a private endpoint is therefore not supported by this minimum; it would be a reviewed change
to that rule. With a proxy, the proxy resolves the name and the gateway cannot see the address:
the proxy is then part of the data path (§3.9). TLS is verified against the system trust store;
there is no setting that turns verification off; a proxy that re-signs TLS needs its CA mounted
into the gateway at run time (`PMOS_BUILD_CA` covers build time only), which H4 adds if the
acceptance run behind such a proxy shows it is needed (**verify on a real host**). The hosted
client follows **no redirects** (`reqwest::redirect::Policy::none()`): a `3xx` is a `502` for
that call, so the key is never sent on to another host.

**Timeouts and retries,** the remote test upstream's today (`crates/gateway/src/app.rs:132-145`,
`docs/gateway.md`, "How long a model server may take"): connect 10 s; a streamed answer silent
for 120 s is cut off; a whole answer gets 600 s; a stream at most `limits.stream-max-secs` (900
s); `429` and `503` retried as `retry.rs` says. One change: `/readyz` probes the hosted
endpoint's `/models` with the key (as it does for a remote test upstream today, `app.rs:292-352`)
at most once per 30 seconds, from a cached result, with a 5 s timeout, because `/readyz` needs no
token and a sandbox can call it: no caller can make the gateway send keyed requests to the
provider faster than that.

**What `/readyz`, `/v1/pmos/roles` and the logs report: labels only, never the host.**

- `/readyz` (`200`, `503`, stub): `class`, `stub`, `model_source` (`local`, `hosted` or
  `hosted-test`), `hosted_label` under `hosted`, and `upstreams` keyed by `<kind>:<role>`, for
  example `{"hosted:default": "ok"}`, never by URL (H4 replaces today's URL-keyed map).
- `/v1/pmos/roles`: as today, plus `model_source`, `hosted_label`, and the upstream kind `hosted`
  beside `local`, `claude-test` and `remote-test`. The model id is the provider's model name, as
  for every role today.
- The usage log: today each event carries the upstream's host (`role.upstream_host()`,
  `app.rs:680`, `:752`), which under `hosted` would put the provider's host in a file shipped
  off-box. H12 writes `upstream: "hosted:<label>"` for `hosted` and `upstream: "remote-test"`
  for `hosted-test`; `local` keeps its `*.pmos.internal` name.
- Errors returned to callers already name the role, never the URL (`docs/gateway.md`).

Tests assert that no `/readyz`, `/v1/pmos/roles`, usage line or start-up message under `hosted`
contains the configured URL, its host, `pmos` addresses or the key (H12).

#### `hosted-test`, and fake-data mode

`hosted-test` is kept as the separate **testing-only** path for unapproved endpoints: the
existing remote test upstream, a `class: test` gateway, `PMOS_GATEWAY_CLOUD_TEST=1`, and
`PMOS_GATEWAY_REMOTE_KEY`. Its eval reports never count, as today.

**Fake-data mode** is now exactly `PMOS_MODEL_SOURCE=hosted-test`. The previous revision also
put every host whose `PMOS_HOST_PROFILE` wasn't `spark` in fake-data mode until D1; with D1
answered and no profile, that condition is gone (the table "What this removes", below). In
fake-data mode nothing real may reach a prompt or a database by any path:

| Source | Real when | In fake-data mode |
| --- | --- | --- |
| Jira, Confluence, Slack through PMs' tokens | `PMOS_CONNECTORS=live` | refused |
| Memory repos (team and personal) | `PMOS_MEMORY=github`, or GitHub client settings set | refused |
| The content library | the deployed checkout (`PMOS_CONTENT_CHECKOUT`) is the team's own content repo, which can hold files marked `sensitivity: restricted` (the committed seed library has none today; 45 of its 46 skills are `data-class: "restricted"`, all but `docs`, and may read them) | refused unless the content root carries a `TEST-CONTENT` marker file (committed in that checkout, stating it holds no company data) **and** no file under it is `restricted`. Checked by every reader of the checkout, not only the tool server (below); a content sync that would bring a `restricted` file is not applied and is recorded as a failed sync |
| The databases (the tool server's memory notes, drafts, sealed tokens and user database; the runtime's tasks; the web app's sessions and follow-ups) | the stack ever ran with `PMOS_CONNECTORS=live` or `PMOS_MEMORY=github` | refused: each of the three services writes a permanent `held_real_data` marker into its own database the first time it starts with live connectors or GitHub memory, whatever the model source; no code path clears it (a fresh database is the only way), a restore carries it, and a service in fake-data mode refuses to start on a database that has it ("this database has held real data") |
| Slack, as a place people type | `PMOS_SLACK_APP_TOKEN` and `PMOS_SLACK_BOT_TOKEN` set (a real workspace; a test workspace can't be told apart) | `pmos-slack` refuses to start |
| The public docs site | `PMOS_DOCS_BASE_URL` on the `docs` service (the company's **public** product docs, `docs/docs-server.md:3-5`) | allowed |
| What people type into the web app | always possible | can't be checked by code: the web app shows a "test stack: fake data only" banner on every page, with "prompts go to an unapproved hosted model", and the pilot never runs in fake-data mode |

**Where each refusal lives** (unchanged from the previous revision except that
`PMOS_HOST_PROFILE` is gone). The refusals live in the services and run at their start-up,
however they are started:

- **Settings:** `PMOS_MODEL_SOURCE`, written by `setup.sh` and passed by `deploy/compose.yaml`
  (and `deploy/compose.evals.yaml`) to `gateway`, `toolserver`, `slack`, `bots` and `web` as
  `${PMOS_MODEL_SOURCE:?run deploy/setup.sh first}`, so Compose itself fails without it. Each
  service also requires it (no default), like `PMOS_CONNECTORS` today. `bots` and `web` also get
  `PMOS_CONNECTORS`, for their `held_real_data` marker only.
- **The gateway** refuses to start: with `local` or `hosted` and a config whose `class` isn't
  `production`, or with `PMOS_GATEWAY_CLOUD_TEST=1`; with `hosted` as above; with `hosted-test`
  and `PMOS_GATEWAY_CLOUD_TEST` not `1`, unless the stub is on.
  (Status, 2026-11-22: done ahead of H4 in the gateway's gate fixes (G2, G8b): `/readyz` now
  has `class` and `stub` in every body and keys `upstreams` by role, each with its kind label
  and `ok` or `down`; tests check that its `200`, `503` and stub bodies carry no upstream URL,
  host, port, path, server name, model id or key. H4 still adds `model_source`.)
- **The tool server** refuses to start in fake-data mode with any row above marked refused, with
  one error per cause, for example: `PMOS_MODEL_SOURCE=hosted-test sends prompts to an endpoint
  nobody approved, so it allows fake data only: PMOS_CONNECTORS must be fake (it is live)`,
  `… the content root has no TEST-CONTENT marker`, `… <path> is marked restricted`, or `… this
  database has held real data`.
- **The content check runs wherever the checkout is read**: one function in `crates/contract`
  (beside `Sensitivity`, `model.rs`), run by the tool server, the runtime and the web app at
  start-up and at every content reload in fake-data mode, and by `pmos-admin sandbox` before it
  uploads skills and master context (`pmos content check-test`). The bring-up worker starts only
  after `bots` is healthy, so the runtime's refusal stops it too.
- **`pmos-slack`** refuses to start in fake-data mode with Slack tokens set.
- **`pmos-admin up`** says the same before anything starts, and skips the Slack profile.

**The runtime and the tool server cross-check the gateway.** As in the previous revision: the
runtime's readiness fails while the gateway's reported `model_source` differs from its own; the
tool server reads the gateway's `/readyz` (`PMOS_GATEWAY_URL`, passed in H4) and refuses `POST
/internal/sessions` when the answer is missing, unreadable or differs from its own
`PMOS_MODEL_SOURCE`, failing closed, at every session open or from a cache of at most 10 seconds
dropped on any failed read. It reads `/readyz` because `/v1/pmos/roles` needs a client token that
would also let the tool server call the model. What is new (H12): it keeps the gateway's
`hosted_label` with the session, for the audit log (§3.9).

**Which gateway may serve `local`: the names' answers (H4).** The gateway's HTTP clients get a
resolver of their own (reqwest's `dns_resolver`) that resolves each upstream name at **every**
connection. For a `*.pmos.internal` upstream it refuses any answer not inside
`PMOS_NETWORK_SUBNET` and outside `PMOS_NETWORK_IP_RANGE` (both passed to the gateway): only
pinned containers have addresses there (§3.3). For a `hosted:` upstream it applies the opposite
rule given under "Egress". A refused answer is a `502` for that call and a warning naming the
upstream's label, never the address. The `local-hosts` exception stays gone: a LAN model server
on another machine is out of scope (§9). `class: test` keeps `is_local_host` as it is
(`config.rs:674-689` at `57e2e72`) and is allowed only with `hosted-test` (or the stub).

**The gateway's container settings (H4, H12).** `deploy/compose.yaml`'s `gateway` service gains,
under `environment:`, `<<: *proxy` (`HTTPS_PROXY`, and a `NO_PROXY` that already lists
`vllm-default`, `vllm-extraction`, `postgres` and `.pmos.internal`), `PMOS_GATEWAY_CLOUD_TEST:
${PMOS_GATEWAY_CLOUD_TEST:-0}`, `PMOS_GATEWAY_REMOTE_KEY: ${PMOS_GATEWAY_REMOTE_KEY:-}`,
`PMOS_MODEL_SOURCE`, `PMOS_NETWORK_SUBNET` and `PMOS_NETWORK_IP_RANGE` (H4), and
`PMOS_GATEWAY_HOSTED_URL`, `PMOS_GATEWAY_HOSTED_KEY_FILE` and the `secrets:` entry (H12); its
config path follows the model source (`gateway.yaml`, `gateway.hosted.yaml` or
`gateway.hosted-test.yaml`).

**Benchmarks under `hosted`.** Today a report counts towards a bar only from a `class:
production` gateway with every upstream `local`, on the models of `deploy/gateway.yaml`
(`docs/evals.md`, "Which runs count"; `benchmark.production_gateway` in `evals/config.yaml`).
H12 lets a run count from a `production` gateway whose upstreams are all `hosted:<label>` with a
label on the approved list, when `benchmark.production_gateway` names
`../deploy/gateway.hosted.yaml`: which config holds the production role models stays one
reviewed line in `evals/config.yaml`, not a host setting. Reports carry the label, never the host.

#### What this removes, and what replaces each safeguard

| Earlier safeguard | Now | What replaces it |
| --- | --- | --- |
| **ADM-3**: outside testing, the gateway refuses any model server that isn't on the host ("prompts never leave the Spark"; `crates/gateway/src/lib.rs:6`, `config.rs:673`) | **Removed for `hosted`.** Prompts with real data, restricted included, leave the host for the approved provider | The approved list: compiled into the gateway, the endpoint pinned by digest, changed only by a CODEOWNERS-reviewed commit with the owner's recorded approval and conditions (§3.9). An endpoint not on it is reachable only as `hosted-test`, which is fake-data mode |
| **Real data only on the Spark** until D1: fake-data mode on every `PMOS_HOST_PROFILE` but `spark`; H-PROFILE a hard fail with no acknowledgement; H11's `PMOS_HOST_PLACE` | **Removed.** Real data is allowed on any host, with `local` or `hosted` | Nothing host-based, by the owner's D1 answer. The protections every host now gets: H-METADATA and H-EXPOSURE fail on every host (§3.2), disk and backup encryption and snapshot control (§5), and, for `local`, the operator's statement that the GPU host is one the owner controls, recorded in the host record (§4) since code can't check it |
| **Fake-data mode tied to `hosted-test`** because every prompt there leaves the host | **Kept, with a narrower reason**: because nobody approved the endpoint | The same refusals (the table above) |
| **The `held_real_data` marker**: a database that ever held real data never runs where prompts leave the host | **Purpose narrowed**: it now only stops such a database running under `hosted-test`. A database used with `local` may later be used with `hosted`, so data that so far stayed on the host can reach the provider | The tool server records every change of source: at start, and at a session open whose gateway answer differs from the last recorded `model_source`/`hosted_label`, it writes an audit event `system.model_source` (`from`, `to`, labels only), and `pmos-admin status` shows the current source and when it last changed |
| **The tool server's cross-check** as the second line keeping real data on the host | **Kept**; it now keeps the tool server and the gateway agreeing on the source, and gives the audit log its label | — |
| **The metric "restricted data sent off the Spark: zero by construction"** (`docs/implementation-plan.md:118`, `docs/gateway.md:231`, `usage.rs:3`) | **Removed** | Two figures: "real data sent to an endpoint not on the approved list", zero by construction (fake-data mode); and tasks served per source and label, from the audit log (§3.9) |
| **`class: production` means local model servers only** | **Loosened by one form**: `hosted:<label>` | The label's entry on the compiled list; no `openai+https://` in production, ever |
| **The usage log's upstream host "lets anyone check"** that prompts stayed local | **Replaced** | The label, checked against the approved list |

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

### 3.8 Network exposure and TLS on any host

Any host may have a public address or sit on a network the company doesn't control.

**Until H10 is merged, no public web app.** H4 makes `pmos-web` read `PMOS_EXPOSURE` (required,
no default) and **refuse to start with `public`**: `PMOS_EXPOSURE=public: this build of
pmos-web does not serve a public host yet (spec slice H10). Reach it over a VPN or an SSH tunnel
with PMOS_EXPOSURE=private, or leave the web app off`. With D2 and D3 answered (below), H10 no
longer waits for the owner and is buildable now; the refusal stays only so that no build between
H4 and H10 serves a public sign-in page with neither answer built. H10 lifts it together with
D2's and D3's rules, never alone. `pmos-admin up --web` and H-SIGNIN say it first. The other
services start on `public`; the console never serves publicly (O7). On a host declared `private`
with H-PRIVATE acknowledged, H-PRIVATE also fails while anything listens on 80 or 443 on a
non-private address (§3.2), so an operator's own proxy in front of 8770 can't put a public
sign-in page there.

The tables and the proxy example below describe every host **as H10 builds it**. Which port
may be reached from where:

| Listener | Bound to | Reached by | On `PMOS_EXPOSURE=public` |
| --- | --- | --- | --- |
| Reverse proxy (nginx or Caddy), 443 | the host's address | browsers | the only public listener. TLS with a publicly trusted certificate; HTTP only to redirect |
| `/oauth/*` on the tool server | through the proxy only (`install.md`, Stage 6) | PMs' browsers | as today: only `/oauth/`, `404` for everything else |
| Web app 8770 | `127.0.0.1` | the proxy | Microsoft (Entra) sign-in; password sign-in only from `PMOS_WEB_PASSWORD_CLIENTS` (D2 (a)); development sign-in refused (H-DEVLOGIN). `/healthz`, `/readyz` and `/internal/` never forwarded; `/api/admin/` answers `404` here on **every** host (D3 (b)) |
| Web app internal 8771 (`PMOS_WEB_INTERNAL_BIND`) | the `pmos` network only, not published | `pmos-slack` (the daily brief) | never public |
| Web app admin 8772 (`PMOS_WEB_ADMIN_BIND`, D3 (b), H10) | published on `PMOS_WEB_ADMIN_PUBLISH` only: loopback or a private address (below) | admins over the VPN, the company's private network, or an SSH tunnel | never public |
| Console 8760 | `127.0.0.1` | the proxy | **never on a public listener** (decision O7): reachable only over a private network, VPN or SSH tunnel, behind its SSO proxy |
| Tool server 8710, gateway 8740, runtime 8750 | `127.0.0.1` and their pinned `pmos` addresses | `pmos-admin`; the sandboxes | never public |
| Postgres, docs, vLLM | the `pmos` network only | the services | never public |
| OpenShell gateway 17670 | `127.0.0.1` (and possibly the host's address on the Docker bridge the driver uses, `docker0` by default: **verify on a real host**, H-EXPOSURE) | the CLI on the host; the sandboxes' supervisors | never public; mTLS kept on; plaintext mode not used |
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
Caddy's rate limiting needs a plug-in). It goes in `docs/runbooks/install.md` **with H10**, not
before, since before H10 `pmos-web` refuses a public host; H9 puts only the rules that hold on a
private host there too (the health and internal routes not forwarded, the body sizes). The address
and certificate paths are left for the operator:

```nginx
# One key per client address. For IPv6 this is one key per address, not per /64: a client
# with a /64 can use many addresses and get a bucket for each. nginx has no built-in way to
# key on a prefix, and a regex on $remote_addr fails on compressed addresses (2001:db8::1).
# If that matters more than IPv6 reach, drop the two `listen [::]:…` lines below.
limit_req_zone  $binary_remote_addr zone=pmos_signin:10m rate=10r/m;
limit_req_zone  $binary_remote_addr zone=pmos_api:10m    rate=20r/s;
limit_conn_zone $binary_remote_addr zone=pmos_conn:10m;

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

    # The admin routes: decision D3 (b). They are served only on the admin listener (8772),
    # never through this proxy; pmos-web answers 404 for them on 8770 as well.
    location ^~ /api/admin/ { return 404; }

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

The proxy's limits are a coarse first layer against floods; per-address keys for IPv6 make them
coarser still (the comment above), which is acceptable only because they are not the sign-in
defence. The real sign-in limits stay where
they are (the tool server's per-account and per-address counters, `docs/password-sign-in.md`), and
`PMOS_WEB_TRUSTED_PROXIES` stays the `pmos` network's gateway, since nginx on the host reaches the
published port from there. The `/oauth/` block for the tool server (`install.md`, Stage 6) takes
the same `limit_req zone=pmos_signin` and `client_max_body_size 64k`. The example is not tested
against a real nginx in this repo: H10 adds `nginx -t` on it in CI's deploy job if the runner has
nginx, and a run on a public host after H10 checks it from outside (RT-14).

**Sign-in on a public host (decision D2 (a), decided by the owner on 2026-11-22).** Microsoft
(Entra) sign-in only on a public host; password sign-in only from a VPN or an allow-listed
range:

- With `PMOS_EXPOSURE=public` and `PMOS_WEB_PASSWORD_LOGIN=1`, `pmos-web` refuses to start unless
  `PMOS_WEB_PASSWORD_CLIENTS` lists at least one CIDR (the VPN's or office's ranges). With it set
  (on any host), `pmos-web` refuses every `/api/auth/password/*` request whose client address
  (from the trusted proxy's `X-Forwarded-For`) is outside the list, with `403
  password_sign_in_not_from_here`, before the tool server is asked; Entra sign-in is unaffected.
  The error at start-up: `PMOS_EXPOSURE=public with password sign-in: set PMOS_WEB_PASSWORD_CLIENTS
  (a VPN or office range), or turn password sign-in off and use Microsoft sign-in (decision D2)`.
- **What counts as an allow-list** (the bounds D2 (a) accepted): every entry in
  `PMOS_WEB_PASSWORD_CLIENTS` is a CIDR no shorter than `/16` for IPv4 and no shorter than `/48`
  for IPv6. `pmos-web` refuses to start on `0.0.0.0/0`, `::/0`, `::ffff:0:0/96` or any shorter
  prefix, naming the entry: a list that holds the whole internet is not a list. A site whose VPN
  really hands out a `/12` would need H10's constant changed under review, which is the intended
  cost.
- **One setting fewer than the previous revision.** `PMOS_WEB_PASSWORD_REACH` (`any` or
  `allow-list`) is not added: a non-empty `PMOS_WEB_PASSWORD_CLIENTS` is the allow-list, and on
  `public` it is required. On `private` it is optional.
- **Development sign-in** (`PMOS_WEB_DEV_LOGIN`) is refused by `pmos-web` itself on `public`, not
  only by H-DEVLOGIN, so the rule holds however it is started.
- **Consequence:** PMs without an Entra account sign in over the VPN or from an allow-listed
  range. Until the owner's Entra app registration exists (CLAUDE.md, "Waiting on the owner"), a
  public host has no sign-in for anyone outside the allow-list.
- `pmos-web` gets `PMOS_EXPOSURE` from `deploy/.env` (required; added in H4, which refuses
  `public` until H10), so the rule holds however it is started; `pmos-admin up --web` and
  H-SIGNIN say it first.

**The admin routes (decision D3 (b), decided by the owner on 2026-11-22): a separate admin-only
listener.** The accepted limit from the user database review stays: a broken-into web backend can
sign as any user to the tool server (`docs/user-database-spec.md:931`, "Compromised web
backend"). D3 (b) keeps the routes that change users, content and policy (`/api/admin/users`,
`/api/admin/content/proposals` with 16 MiB bodies, `/api/admin/policy`) off the browser-facing
listener:

- `pmos-web` gains `PMOS_WEB_ADMIN_BIND`, a third listener for `/api/admin/*` (with the static
  app, so the admin screens load), on port 8772; the browser-facing listener answers `404` for
  `/api/admin/*`. **This applies on every host, not only on `public`:** with the web app on,
  `pmos-web` refuses to start without `PMOS_WEB_ADMIN_BIND`. One rule for every host is simpler
  than two, and on a private network the admin listener is simply published on a private
  address. (A slight widening of the owner's answer, which named the public listener; the
  reviewer should confirm it, §12.)
- Its address is checked in two places, because the process can't see where Docker publishes
  its port:
  - `pmos-web` refuses a `PMOS_WEB_ADMIN_BIND` whose address is not the unspecified address (the
    container's own `0.0.0.0`, as for 8770), loopback, RFC 1918, `100.64.0.0/10` (VPN overlays
    such as WireGuard set-ups commonly use it) or IPv6 unique-local;
  - `deploy/compose.yaml` publishes 8772 on `${PMOS_WEB_ADMIN_PUBLISH:-127.0.0.1}:8772`, and
    `pmos-admin up --web`, H-ADMIN and H-EXPOSURE refuse a `PMOS_WEB_ADMIN_PUBLISH` outside
    loopback and those private ranges, and fail if 8772 listens anywhere else. RT-14 checks from
    outside that 8772 doesn't answer.
- The proxy block above returns `404` as a second layer.
- **Consequence:** admins use the admin screens over the VPN, the company's private network or an
  SSH tunnel. This does not remove the accepted limit (a break-in through the public listener
  still gets the whole process and its tokens); it keeps the admin handlers' code, and their
  large bodies, off the internet.
- **Not added:** `PMOS_WEB_ADMIN_ON_PUBLIC_LISTENER=owner-accepted`, which the previous revision
  designed for D3's answer (a). With (b) decided, there is no way to serve the admin routes on the
  browser-facing listener.

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
  and for `hosted` or `hosted-test` the hosted endpoint, from the gateway only). Restricting
  outbound egress on the host is optional; sandboxes are already default-deny.

**The signed user assertion stays required**, exactly as today: `setup.sh` writes
`PMOS_ASSERTION_MODE=require`, `assertion_checks` runs before every `up`, `add-bot` and `restore`,
and the runtime and tool server refuse to start otherwise. No host setting has a way to change
this, and H-ASSERT reports it.

### 3.9 Data handling with a hosted model

This section says plainly what the owner's D1 answer allows, what the spec requires before real
data flows to the hosted model, and what PM OS does not verify.

**What now leaves the host under `hosted`.** Every model call: the system prompt (the bot's
persona, role and skill text, and the master context from the content library), the PM's
messages, and every tool result the harness puts into the conversation. That includes text read
from Jira, Confluence and Slack with the PM's own tokens, memory notes, drafts, and content
files marked `sensitivity: restricted`, which 45 of the 46 seed skills may read (§3.6). Nothing
filters it by data class: this design has no data-class ceiling for the hosted model, and adding
one (for example "internal only") would be a later slice the owner would have to ask for. What
does **not** leave the host: the databases, the PMs' sealed tokens, the vault key, the user
database, `deploy/.env`, and the audit log, except as far as their contents appear in a prompt.
Under `local`, nothing leaves the host through the model; the host's disks, snapshots and
backups are wherever the host is (§5).

**What the spec requires before real data flows to the hosted model** (each a condition the
first approved entry, slice H13, must meet; none is checked by code except where it says so):

1. **The owner's approval of that provider and model, recorded** in
   `docs/model-providers/<label>.md`: who approved, when, which model ids, and for which data
   (all classes, as D1 says, unless the owner narrows it). The gateway's unit test refuses an
   entry without this record (§3.6).
2. **A data-processing agreement with the provider, and its retention terms** (how long prompts
   and answers are kept, whether they are used for training, where they are processed), each
   **confirmed by the owner** in the same record. These are the owner's to confirm; this spec
   does not state any of them as fact.
3. **The CODEOWNERS line and branch protection** for the approved list and the records (§3.6),
   which wait on the owner.
4. **If the host uses an outbound proxy that re-signs TLS**, the owner's confirmation that the
   proxy is part of the data path: it can read every prompt and the provider key. Without such a
   proxy, the provider's host is the only party besides the host that sees prompts in clear.
5. **Gate 1**, as for any real data.

**The audit log records which model source served each task.** The tool server writes a
`session.open` audit event for every session (today a session's opening is not itself audited),
and every audit event of that session carries a new field `model_source`: `local`,
`hosted:<label>`, `hosted-test` or `stub`, the value the gateway reported at the session's open
(§3.6, the cross-check). A change of source writes `system.model_source` (§3.6, "What this
removes"). It is metadata, so an Auditor sees it. The gateway's usage log carries the same label
on every call, so a gateway restarted with another source during a session is visible there. The
audit event is the record of what the gateway reported, not proof of where a prompt went: the
gateway is the one component that knows, and it is reviewed code with the approved list compiled
in.

**Sandboxes still never hold the provider's key or reach the provider.** Only the gateway does
(§3.6, "How this relates to … one credential per provider"). RT-18 (§5) checks from a sandbox
that the provider's host is refused.

**What PM OS does not verify.**

- What the provider does with prompts after they arrive: retention, training use, location, who
  at the provider can read them. Those are the conditions above, which the owner confirms; PM OS
  cannot see them.
- **Any confidential-computing attestation** a provider may offer (a statement that prompts are
  processed only inside attested hardware). PM OS does not request or check one; it is **not
  verified**. Checking an attestation before sending a prompt could be a later slice, designed and
  reviewed on its own.
- That the endpoint behind the approved URL is the provider the owner approved: PM OS pins the
  URL and checks TLS against the system trust store, nothing more.
- That a GPU host used with `local` is one "the owner controls": that is the operator's
  statement, recorded in the host record (§4).

**Docs elsewhere in the repo that state the old rule** and must change in the docs slice (H6;
none is edited by this spec): `docs/implementation-plan.md` (`:4` "running on one DGX Spark",
`:116` "local vLLM routes only (ADM-3)", `:118` the "Restricted data sent off the Spark"
metric); `docs/gateway.md` (`:3`, `:7-8` "Keeps prompts on the Spark … (ADM-3)", `:231` "zero by
construction", and the "Allowed upstream hosts" and production-host text); `crates/gateway/src/lib.rs:2-6`,
`config.rs:265-267`, `:673`, `:693`, `usage.rs:3` (comments); `config/gateway.yaml:4-5` and
`deploy/gateway.yaml:1-3`, `:11` (comments); `README.md` (`:5` "on the team's DGX Spark with
open-weight models", `:54` "in front of the local models"); the runbooks (`install.md` Stage 3,
`runbooks/README.md`, and `model-swap.md`, which describes a swap of local models only); and
`docs/evals.md` ("Which runs count": every upstream `local`; `:154` and `:640`, "the Spark's"
gateway).

## 4. Tiers of support

| Tier | What | Status after this spec's slices | Notes |
| --- | --- | --- | --- |
| **Tier 1 (the only target)** | One Linux host (Debian or Ubuntu), Docker Engine 28+, OpenShell's Docker driver, amd64 or arm64, any cloud VM or on-prem server, with `hosted` or `hosted-test` models (no GPU needed) or `local` models (an NVIDIA GPU) | Designed and built here | The platform-agnostic implementation the owner asked for. Real data on any of them with `local` or `hosted` (D1, §3.6). A DGX Spark is one such host with `local`, with no profile of its own. Local models on any NVIDIA GPU are **designed for; not verified** (O3) until an acceptance run with `local` passes on one |
| **Tier 2 (later)** | Kubernetes 1.33 through OpenShell's Helm chart, with a CNI that enforces `NetworkPolicy` | Outlined only; a later slice (H8), not for the pilot (decision O2, decided: no) | The three names would be cluster Services (or a DNS rewrite), PM OS would move from Compose to manifests, preflight gains a cluster mode (CNI `NetworkPolicy` enforcement, OpenShell's namespaces). That is a second deployment kit, not a small slice |
| **Not offered** | Podman rootless, MicroVM, macOS Docker Desktop, WSL 2 | OpenShell lists them; PM OS does not test them | `PMOS_OPENSHELL_DRIVER` refuses `podman` until someone verifies it. macOS and WSL 2 are development set-ups, not a host for PMs' work |
| **Unsupported** | Serverless container platforms: Vercel, Cloud Run, Fargate, Fly.io, Railway and similar | Never | OpenShell needs a host it controls: a Docker daemon (or cluster) to create a container per sandbox, Landlock and seccomp on a kernel it can check, and a long-running gateway. These platforms run your one container on their kernel and their runtime, give no Docker daemon, and don't document Landlock. PM OS also needs Postgres with durable disk and always-on bots (bot decision D2). None of that is documented for them, so they are unsupported, not "untested" |

**What "supported" will mean.** The docs may call a host or tier **verified** only after the
acceptance run in §6 has passed on a real host of that kind, with the record committed
(`docs/hosts/<date>-<host-kind>.md`: OpenShell version, kernel, Docker version, architecture,
this repo's commit, the preflight JSON, the red-team script's output, every acknowledgement in
force, and, for `local`, the operator's statement that the GPU host is one the owner controls).
Until then the runbooks say "designed for; not yet verified". One passed run verifies that host
kind (for example "Ubuntu 24.04 amd64 VM, Docker 28, no GPU, hosted-test"), not every cloud; a
second architecture, kernel family or model source needs its own run (a GPU host with `local`
is a separate kind). Today **no host is verified**, and none of H0's fixes has been seen working
on OpenShell.

## 5. Security analysis

### What every host must be protected against

The previous revision compared "on the Spark" with "on a cloud VM". With no Spark target, each
row now states the one rule for every host.

| Threat | On any host | Handled by |
| --- | --- | --- |
| The host is reachable from the internet | Possibly a public address | `pmos-web` refuses `PMOS_EXPOSURE=public` until H10 is merged (§3.8); from H10, D2 (a) and D3 (b) as built; `public` is the default; a `private` declaration on a host with a public address fails H-PRIVATE unless acknowledged; the §3.8 exposure table and proxy example; H-EXPOSURE, H-DEVLOGIN, H-SIGNIN, H-ADMIN; console never public (O7); RT-14 from outside |
| Other tenants | A cloud's other customers share the hypervisor; an on-prem server may be shared with other workloads | The hypervisor boundary is the cloud's. Don't run PM OS on a host shared with other workloads: the red team already says "never on a shared machine" |
| The cloud metadata service (`169.254.169.254`; `metadata.google.internal`, which resolves to it; AWS's IPv6 `fd00:ec2::254`) | Present on most cloud VMs, and may hand out the VM's cloud credentials | **From a sandbox:** link-local is never allowed by policy (OpenShell docs), the policy allows only three exact names, AAAA answers are empty, and `allowed_ips` (§3.5) pins each name to its own address. RT-11 checks each address. **From PM OS's own containers**, which are not sandboxed and hold the vault key and, in the gateway, the hosted model's key: they *can* reach it. H-METADATA (§3.2) now fails on **every** host where a container could get credentials; the generic fix is the `DOCKER-USER` block, and on AWS IMDSv2 with hop limit 1 also passes. Give the VM no cloud role, or one with no permissions, in any case |
| Metadata services outside link-local (Azure's `168.63.129.16`, Alibaba's `100.100.100.200`) | Possible | Not in any policy: default deny. RT-11 includes both, and the block covers them for PM OS's containers. Whether OpenShell treats `100.64.0.0/10` as private is not documented; it doesn't matter while no rule allows it |
| Name resolution | VPC resolvers answer `*.internal` names on some clouds | `.internal` is reserved for private use, the managed block decides the three names, each name's `/32` refuses any other answer, and the gateway's resolver checks every answer for a model server name (§3.6) |
| A model server name answered by something else | A VPC resolver, a search domain, an `/etc/hosts` line | `local` needs a `class: production` gateway, which takes only `<label>.pmos.internal` names, and H4 refuses any answer for them outside the pinned part of the `pmos` subnet (§3.6). A `class: test` gateway may still reach a private address, but only in fake-data mode |
| **Real data sent to the hosted provider** (new, by D1) | Under `hosted`, every prompt with its restricted content leaves the host (§3.9) | Only to an endpoint on the compiled, CODEOWNERS-reviewed approved list, pinned by digest, after the owner's recorded approval and conditions (§3.6, §3.9); the key only in the gateway's container; no redirects; the hosted host's DNS answers checked; labels only in every report and log; the audit log records the source per session. Any other endpoint is `hosted-test`, fake data only |
| An operator points real data at another endpoint | Editing `deploy/.env` | Can't: the URL must hash to an approved entry's digest, and the list is compiled in. Changing the list needs a merged, reviewed commit and a rebuilt image. **A stated limit:** anyone in the `docker` group is root on the host and can build and run any image, as for every other control here; the list makes such a change deliberate and reviewable, not impossible |
| The hosted model's key | In the gateway's container | One file, mode `600`, mounted only into the gateway, never in `deploy/.env`, a log, a report, a policy or a command line (§3.6); kept out of backups. A sandbox never holds it (RT-18) |
| What the services can't see | `docker compose up` by hand skips the preflight | **A stated limit.** H-PRIVATE and the other host checks run only from `pmos-admin` (`up`, `restore`, `add-bot`, `sandbox`, `resume`, `preflight`) and the worker unit's `ExecStartPre`. What the services enforce on their own, however they are started: the model source's rules in the gateway (`hosted` only from the approved list), fake-data mode under `hosted-test` (live connectors, GitHub memory, unmarked or restricted content, a database that has held real data and Slack tokens refused; §3.6; RT-17), and `pmos-web`'s sign-in and admin-listener rules (§3.8) |
| Secrets at rest | `deploy/.env` (vault key), `deploy/secrets/hosted-model.key`, Postgres (sealed tokens), backups on the host's disk; on a cloud VM, also snapshots and images the cloud account can copy | Encrypt the disk (the provider's encryption at rest, or LUKS on-prem). Treat VM snapshots and images as secrets: they hold the vault key, the hosted key and the sealed tokens together. Restrict who in the cloud account may snapshot, attach or read the disk. With real data now allowed on any host (D1), this is a condition of Gate 1 on a cloud VM, recorded in the host record |
| Backups | Copies go off the host, often to object storage | Encrypt each backup before it leaves the host (for example `age` to the platform owners' keys), and never copy `deploy/.env` or the hosted key in the same place as the database dump. Restore test as Gate 1 asks |
| Maintenance reboots | Routine on cloud VMs | The supervised worker and lingering (§3.7); the runtime already retries a running task once; the preflight re-runs at every start |
| A bot name used as shell, path or pattern | — | Validated before use (§3.7, H0) |
| The OpenShell gateway | Local | Kept on loopback with mTLS; O-GATEWAY. Plaintext mode, which the docs say is for local or tunnelled use only, is not used |
| Who can run `docker` | Platform owners, plus anyone with cloud console access | Membership of the `docker` group is root on the host: give it to platform owners only. Cloud console and serial-console access are equivalent to root |

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
| P-8 | `sudo pmos-admin host acknowledge H-METADATA --reason test`, then a `PMOS_HOST_ACK_H_EXPOSURE` line written into `deploy/.env` by hand | The acknowledgement is refused with exit 2 and nothing is written or logged (only H-PRIVATE takes one); the hand-written line makes the preflight fail. (The previous P-8 tested H-PROFILE, which is gone) |
| P-9 | With H-PRIVATE acknowledged: start a listener on `0.0.0.0:443` (for example `sudo python3 -m http.server 443`) and run the preflight; stop it and run again | The first run fails H-PRIVATE naming the port; the second passes as warn |
| RT-11 | From A: `169.254.169.254`, `metadata.google.internal`, `[fd00:ec2::254]`, `168.63.129.16`, `100.100.100.200`, each on 80 and 443 | Each blocked by OpenShell and logged as RT-10 describes |
| RT-12 | From A: the host's own addresses on 22, 17670 (OpenShell's gateway), 2375 and 2376 (Docker's API), and the Docker socket path | Each blocked; the socket is not in A's filesystem |
| RT-13 | From A: the console and Postgres by their `pmos` addresses (`<console address>:8760`, `<postgres address>:5432`), the network gateway `10.201.77.1` on 8770, and a pinned service address on a port that isn't its own (`10.201.77.10:8740`) | Each blocked: only the three exact names on their own ports are allowed |
| RT-13b | Point a name at another address for a moment, then call it from A: first `toolserver.pmos.internal` → `10.201.77.11` (the gateway's address, which a `/24` would have allowed), then → `10.201.78.10`. `pmos-admin` has **no** override for this: `isolation.sh` writes the temporary line itself (with `sudo`), under a `trap` that runs `pmos-admin host-names apply` on every exit, and `host-names check` fails while the line is there, so `pmos-admin sandbox` refuses to start a sandbox meanwhile | Both refused by `allowed_ips`; after the script, `host-names check` passes again (O6, decided) |
| RT-14 | `exposure.sh <host's public address>` from outside the host's network | Only 443 (and 80 if used) and 22 from the operators' range answer; 5432, 8710, 8730, 8740, 8750, 8760, 8770, 8771, 8772, 17670 do not. With the web app off, nothing answers on 443 or 80 either. With H10 and the web app on, through 443: `/healthz`, `/readyz`, `/internal/brief/x` and `/api/admin/users` answer `404` (D3 (b)), and `POST /api/auth/password/sign-in` (`crates/web/src/app/password.rs:92`, under `/api`) from an address outside `PMOS_WEB_PASSWORD_CLIENTS` answers `403 password_sign_in_not_from_here` (D2 (a)) |
| RT-15 | From a second machine on the same private network or VPC: `10.201.77.10:8710` (with a route added on that machine towards the host, if the operator can) | No answer |
| RT-16 | From a container on the `pmos` network: H-METADATA's probe, and a plain TCP connect to `169.254.169.254:80`; then `sudo systemctl restart docker`, wait for the containers to come back without `pmos-admin up`, and probe again; then flush `DOCKER-USER` by hand, wait 70 s, and probe again | On every host: H-METADATA passes; with the block applied, no answer at all, after the Docker restart and after the flush too |
| RT-17 | Without `pmos-admin`: with `PMOS_MODEL_SOURCE=hosted-test` and `PMOS_CONNECTORS=live` (and, in turn, `PMOS_MEMORY=github`, a content checkout without `TEST-CONTENT`, Slack tokens set), run `docker compose up -d` by hand, then `docker compose restart toolserver`, then `docker compose run --rm toolserver` | Each time the tool server (and `pmos-slack` for the tokens, the runtime and the web app for the content) exits with its fake-data error and opens no session; nothing is read from a live source. The services refuse live connectors under `hosted-test` however they are started |
| RT-18 | From A: the hosted provider's host (taken from `PMOS_GATEWAY_HOSTED_URL` on the host, never written into the record) on 443, by name and by its resolved address | Both blocked by OpenShell: no sandbox reaches the provider. A's filesystem and environment hold no provider key (`grep` of `/proc/self/environ` and the workspace for the key's first 8 characters, done on the host side by `isolation.sh`, which prints only "found" or "not found") |
| RT-19 | Without `pmos-admin`, with `PMOS_MODEL_SOURCE=hosted` (only once an entry exists, H13): change `PMOS_GATEWAY_HOSTED_URL` to another `https` URL, then to the right URL with a trailing query, then empty the key file; run `docker compose up -d gateway` each time | The gateway exits each time with its message, naming the setting, never the URL; no request leaves for the other URL (checked on the host's proxy log or with `ss` during the start) |
| E-5 | Reboot the host | The OpenShell gateway, the stack and every bot's worker come back without anyone logging in (the units' `ExecStartPre` preflight passes); a Slack question is answered |

## 6. How to verify

### In CI and in this build environment (no OpenShell)

| What | How | Slice |
| --- | --- | --- |
| Bot-name validation | `scripts/test-pmos-admin.sh`: `add-bot` and `sandbox` refuse `../x`, `a b`, `x$(id)`, a name with a newline, `x=y`, `a,b`, `-a`, `a--b`, `A`, a 33-character name, each with exit 2 and `deploy/.env` unchanged; `docs` and `pm-ana` pass | H0 |
| The worker unit | `systemd-analyze verify` on `deploy/systemd/pmos-worker@.service`; the instance name for `pm-ana` is made with `systemd-escape`; the command `sandbox` would run, printed with `--dry-run`, uses `--from` and no `&` | H0 |
| The worker image's user | `docker build` of `Dockerfile.worker`; `docker run --rm pmos-worker:local id` prints the numeric `sandbox` ids; `docker inspect` shows a numeric `Config.User`; the eval stack's workers still pass their tests | H0 |
| The preflight's every check, pass and fail | `scripts/test-preflight.sh`: fixture directories under `deploy/test/preflight/` (an Ubuntu 24.04 amd64 host, a Debian arm64 host, a 5.15 kernel, Landlock absent from `lsm`, an ABI-2 probe result, cgroups v1, Docker 27, no NVIDIA runtime with `local`, `local` with `PMOS_VLLM_IMAGE` empty, port 8710 taken, a public bind on a `private` host and on a `public` one, dev sign-in on `public`, a live network unlike `deploy/.env`, a `/8` and a `/16` `PMOS_NETWORK_SUBNET` that hold the pinned addresses, an `ip_range` holding `.10` or `.13`, a stale or loopback managed hosts block (H2's), each H-METADATA answer shape with and without the block on a `private` and on a `public` host (each answer fails on both), the block's rules present but its drop-in or timer missing, `private` with a public interface address and with a metadata answer, each with and without an acknowledgement, an acknowledged `private` with a listener on 80 or 443 on a public or unspecified address (fail) and on a private one (pass), an acknowledgement whose fingerprint no longer matches, a `PMOS_HOST_ACK_*` line with no log entry, `host acknowledge` of any check but H-PRIVATE refused and a hand-written line for one failing, `public` with the web profile on before H10, and from H10 H-SIGNIN's and H-ADMIN's cases); each asserts the exact status and exit code. The fixtures use H2's pinned addresses, `ip_range` and managed block, so H1 is built after H2. Run in CI's `deploy` job | H1 (H-SIGNIN's and H-ADMIN's H10 cases in H10) |
| The preflight runs at every start | `scripts/test-pmos-admin.sh`: `up`, `restore`, `add-bot`, `sandbox` and `resume` run the right stage and refuse on a failing fixture | H1 |
| The metadata block and acknowledgements | `scripts/test-pmos-admin.sh` with stand-ins for `iptables`, `systemctl` and `chattr`, on a temporary root: `host-firewall apply` writes the rules matching `-i pmos0` and `-i docker0`, the `docker.service` drop-in and the timer, and is idempotent; `check` fails when any one is missing; `remove` takes all three away; `host acknowledge H-PRIVATE` writes the `deploy/.env` line and the log line, `--clear` logs too, and `status` prints every acknowledgement in force | H1 |
| The Landlock probe itself | run on the CI runner and here: it prints an ABI or a clear failure, never a crash. Its result is not asserted (it depends on the runner's kernel) | H1 |
| The service refusals | `cargo test`: the gateway's config tests (`local` with a `class: test` config, `local` with `CLOUD_TEST=1`, `local` with `deploy/gateway.yaml` and the stub on, which starts; `hosted-test` without `CLOUD_TEST`, and with the stub; a missing `PMOS_MODEL_SOURCE`; a `hosted:` upstream under `local` or `hosted-test`, refused) and its resolver's tests (a production name answered inside the `ip_range`, outside the subnet, with loopback, and with two answers one of them outside, each refused at connection with a `502`; one answered with a pinned address, accepted); the tool server's config tests (in fake-data mode under `hosted-test`: live connectors, `PMOS_MEMORY=github`, no `TEST-CONTENT`, a `restricted` file, a `held_real_data` marker; the marker written at a start with live connectors under `local`, and still there after a restart with fake connectors; `local` with live connectors accepted; a session refused while the gateway's `/readyz` gives no answer, an unreadable one or another source, and refused again once the gateway's answer changes after the cache runs out); the gateway's `/readyz` bodies (`200`, `503`, stub) holding only labels: no upstream URL, host name, `pmos` address or key; the shared content check run by the runtime and the web app at start and at reload; `pmos-web` refusing `PMOS_EXPOSURE=public` and a missing `PMOS_EXPOSURE`; the runtime's and web app's markers; `pmos-slack` with Slack tokens; the runtime's readiness on a mismatch. Each with its passing case | H4 |
| The refusals hold without `pmos-admin` | a stack test: `docker compose up` with `hosted-test` and `PMOS_CONNECTORS=live` leaves the tool server exited with its error; `pmos-admin restart toolserver` the same; `pmos-admin sandbox --dry-run` in fake-data mode on a checkout without `TEST-CONTENT` refuses before any upload | H4 |
| The gateway's container settings | `docker compose config` shows `HTTPS_PROXY`, `NO_PROXY`, `PMOS_GATEWAY_CLOUD_TEST`, `PMOS_GATEWAY_REMOTE_KEY`, `PMOS_MODEL_SOURCE`, `PMOS_NETWORK_SUBNET` and `PMOS_NETWORK_IP_RANGE` in `gateway`'s `environment`; `local` with `PMOS_VLLM_IMAGE` unset fails `compose config`; a stack test with a local mock of the hosted API behind a test proxy | H4 |
| The hosted production source | `cargo test -p pmos-gateway`: the approved list's own test (a valid label, a 64-hex digest, at least one model, a `record` file with the three confirmed sections; the shipped list empty); URL normalisation (case, default port, trailing `/`; user information, query, fragment, `http` refused); `hosted` refused with a `test` config, two labels, an unknown label, a digest mismatch, a model id outside the entry, `CLOUD_TEST=1`, an empty or missing key file; `hosted` accepted with a test-only list compiled under `cfg(test)` and a local TLS mock; no redirect followed (a `302` to another host is a `502`, and the mock on the other host sees no request); the hosted resolver refusing loopback, link-local, metadata, `pmos` subnet and private answers; `/readyz` probing the hosted `/models` at most once per 30 s under a burst of calls; `/readyz`, `/v1/pmos/roles`, usage lines and start-up messages containing neither the URL, its host nor the key. The tool server: `session.open` written with `model_source`, every event of the session carrying it, `system.model_source` on a change. `pmos-admin hosted-key set` from standard input, refusing group or world bits; `backup` leaving the key out; a stack test that the gateway container reads the mounted key file and no other container has it | H12 |
| The managed hosts block | `scripts/test-pmos-admin.sh` gains cases on a temporary hosts file: apply is idempotent, keeps other lines, replaces a stale block, takes no override (any extra argument is refused), and `check` fails on each wrong state, a loopback entry and a second line for a name outside the block included | H2 |
| Compose | `docker compose config -q` with the five pinned addresses (the three services and the two vLLM services), the `ip_range` and the `pmos0` bridge name; a container started on the network first gets an address in the range; `up` refuses a `/16` subnet that holds the pinned addresses; CI's existing bring-up smoke test (it doesn't use sandboxes) and the eval stack still pass | H2 |
| The internal route | `docker compose config` shows `PMOS_WEB_INTERNAL_BIND`; a stack test: the brief `PUT` works on 8771 and `404`s on 8770 | H9 |
| The policy change | `cargo test -p pmos-policy`: `net-allowed-ips` fails and passes (another service's `/32`, a `/24`, a missing list, two entries); the compose literals equal the constants; the generated files carry `allowed_ips`; `pmos policy check` green on the regenerated `policies/`; `up` refuses a subnet without the constants | H3 |
| Sign-in and admin routes | `cargo test -p pmos-web`: D2 (a): `public` with password sign-in and no `PMOS_WEB_PASSWORD_CLIENTS` refused; the allow-list's bounds (`0.0.0.0/0`, `::/0`, `::ffff:0:0/96`, a `/15` and a `/47` refused, a `/16` and a `/48` accepted); a password request from outside the list `403` before the tool server is asked, Entra unaffected; dev sign-in refused on `public`. D3 (b): the web app refused without `PMOS_WEB_ADMIN_BIND` on `public` **and** on `private`; its address check (a public address refused; loopback, RFC 1918, `100.64.0.0/10` and unique-local accepted); `/api/admin/*` `404` on 8770 and served on 8772. `pmos-admin up --web` refusing a public `PMOS_WEB_ADMIN_PUBLISH`; `nginx -t` on the proxy example if CI has nginx | H10 |
| The workspace's checks | the full set in CLAUDE.md, unchanged | every slice |

None of this shows what OpenShell enforces. It shows that PM OS's side does what this spec says.

### On a real host with OpenShell (the acceptance run)

**The first host (decision O1, decided by the owner on 2026-11-22): an Ubuntu 24.04 LTS x86
(amd64) virtual machine without a GPU, with fake data.** So the run uses `hosted-test`, which
needs no GPU and no owner approval, and fake-data mode holds whatever else is set. The cloud or
server it runs on is the operator's choice (any will do: that is the point of the run); it is
recorded in the host record, without the account's identifiers. Declared `--exposure public`
unless the VM has no public address. The web app is either off, or, once H10 is merged, on with
password sign-in limited to the operator's own address range in `PMOS_WEB_PASSWORD_CLIENTS`
(Entra needs the owner's app registration, which may not exist yet) and the admin listener on
`127.0.0.1` reached through an SSH tunnel. Each step's output goes into the host record (§4).

1. **Before installing anything:** `deploy/preflight.sh --model-source hosted-test --exposure
   public --json`. Every host check passes (H-NVIDIA is not checked; H-EGRESS waits for step 3).
2. **Install OpenShell** from NVIDIA's install page; enable lingering;
   `deploy/preflight.sh --openshell`. O-CLI, O-GATEWAY, O-LINGER and O-CANARY pass. Record whether
   `--from` took the local image (surprise 2), how the canary's identity was set (surprise 3), and
   whether `sandbox exec` stays in the foreground.
3. **Stack, Stages 1–2 of the install runbook** with `hosted-test` (the owner's hosted model as an
   unapproved test endpoint is allowed here, with fake data): `status` all `ok`; H-EGRESS passes;
   the smoke test passes through the bring-up worker. Then the refusals: set
   `PMOS_CONNECTORS=live` and run `docker compose up -d toolserver` by hand; the tool server must
   exit with its error (RT-17).
4. **Names:** `pmos-admin host-names apply`; P-7 passes; O-REACH passes. **H-1, the key unknown:**
   from the Docs bot's sandbox, a heartbeat on a made-up task id reaches the runtime and gets its
   `404` (RT-8's control). If it doesn't, stop at §3.3 step 6.
5. **Sandboxes:** `pmos-admin sandbox docs`, a personal bot and `pm-redteam`; the smoke test
   passes through the Docs bot's sandbox (E-1: no denials in its log).
6. **Red team:** `isolation.sh` both ways; RT-1 to RT-18 and P-0 to P-9 pass (RT-19 only if an
   approved entry exists by then); the "verify on a real host" table in
   `docs/sandbox-policies.md` (today's "Verify on the Spark" table) filled in, with anything that
   differed.
7. **Reboot** (E-5).
8. **Write the record** and open a pull request with it; the reviewer session checks it before the
   docs call "Ubuntu 24.04 amd64 VM, Docker 28, no GPU, hosted-test" verified.

**Not covered by this run:** `local` (no GPU), `hosted` with real data (it needs H13 and Gate 1),
arm64, and Entra sign-in. A GPU host with `local` needs its own run (O3); which GPU host, if any,
is the owner's to name (§8).

## 7. Slices, in build order

One agent, one worktree, one branch per slice (CLAUDE.md). Re-derived on 2026-11-22 for the
change of direction: every decision the slices waited on (D1, D2, D3, O1, O6) is answered, H11 is
dropped, and two slices are new (H12, H13). "Buildable now" means it can be built and fully
tested here with no further owner decision; its claims about OpenShell are still checked in the
acceptance run. "Needs a real host" means only a host with OpenShell can finish it.

| # | Slice | Files | Tests | Model | Buildable now? | Needs a real host? |
| --- | --- | --- | --- | --- | --- | --- |
| H0 | **Sandbox launch and worker fixes** (built; awaiting review) | `deploy/pmos-admin` (`add-bot`, `sandbox`, `pause`, `resume`: name validation, exact-field token lookup, `--from`, the unit instead of `&`, the lingering check), `deploy/systemd/pmos-worker@.service` (new: `systemd-escape`d instance, `%I`, `Restart=on-failure`, `StartLimit*`; `ExecStartPre` added once H1 exists), `deploy/Dockerfile.worker` (`sandbox` account, numeric `USER`), `docs/runbooks/install.md` (the `127.0.0.1` hosts line removed, with a "stop: sandboxes can't reach the services until H2" note; lingering), `docs/sandbox-policies.md` ("Creating a bot's sandbox") | §6 H0 rows | Strongest (shell injection, the sandbox's identity) | Built | Each fix is from OpenShell's public docs and unverified until the acceptance run |
| H0b | **Security review fixes of `6bb806f`** (built; awaiting review) | as its status note says | `scripts/test-pmos-admin.sh` | Strongest | Built | `provider` and `sandbox stop`/`start` behaviour: acceptance run |
| H2 | **Pinned addresses and managed host names** | `deploy/compose.yaml` (`ipv4_address` for the three services and the two vLLM services; `ip_range`; bridge name `pmos0`), `deploy/env.example` (`PMOS_NETWORK_IP_RANGE`; `.10` to `.14` listed as reserved), `deploy/pmos-admin` (`host-names apply`/`check`, with no override; the live-network check; the subnet `/24`-or-longer and holds-the-literals checks; `sandbox` refuses on a failed check; `status`), `scripts/test-pmos-admin.sh`, `docs/runbooks/install.md` Stage 4 step 1 | §6 H2 rows | Strongest (it decides what sandboxes reach) | **Yes** | H-1 only |
| H1 | **Preflight** (after H2: its fixtures use H2's addresses, `ip_range` and managed block) | `deploy/preflight.sh` (new; no profile and no H-PROFILE; H-METADATA and H-EXPOSURE fail on every host; H-NVIDIA with `PMOS_VLLM_IMAGE` for `local`; H-EGRESS; H-PRIVATE with its 80/443 check when acknowledged; H-SIGNIN's pre-H10 refusal), `deploy/test/preflight/**` (fixtures), `scripts/test-preflight.sh` (new), `.github/workflows/ci.yml` (deploy job: parse and run it), `deploy/pmos-admin` (`preflight`, `host-firewall apply/check/remove` with the rules on `pmos0` and `docker0`, the `docker.service` drop-in and the timer, `host acknowledge` for H-PRIVATE only; the stages at `up`, `restore`, `add-bot`, `sandbox`, `resume`; acknowledgements in `status`), `deploy/pmos-metadata-block` (new, the root-owned rule script), `deploy/setup.sh` (run it first, stop on fail), the unit's `ExecStartPre` | §6 H1 rows | Strongest (it decides what may be called supported) | **Yes** | O-* checks are written now, proven in the acceptance run |
| H4 | **Model sources `local` and `hosted-test`, fake-data mode, no public web app yet, enforced by the services** (after H2: its resolver checks against H2's `ip_range`) | `crates/gateway` (`PMOS_MODEL_SOURCE` with `local` and `hosted-test`; `local` only with `class: production`; the resolver that checks each production name's answer at every connection; `model_source` in `/readyz` and `/v1/pmos/roles`; `/readyz` reporting labels only, its URL-keyed `upstreams` replaced), `crates/toolserver` (fake-data mode from `PMOS_MODEL_SOURCE=hosted-test` only; the refusals; `TEST-CONTENT` and the restricted-file check at start and sync; the `held_real_data` marker; the gateway cross-check at every session open or from a short cache, failing closed), a shared content-check function (in `crates/contract`, beside `Sensitivity` in `model.rs`) used by `crates/toolserver`, `crates/bots` and `crates/web`, and `pmos content check-test` in the `pmos` CLI, `crates/slack` (refuse tokens), `crates/bots` (readiness cross-check, the content check, the marker), `crates/web` (`PMOS_EXPOSURE` required and `public` refused until H10; the test banner; the content check; the marker), `deploy/compose.yaml` and `deploy/compose.evals.yaml` (gateway: `<<: *proxy` under `environment:`, `PMOS_GATEWAY_CLOUD_TEST`, `PMOS_GATEWAY_REMOTE_KEY`, `PMOS_MODEL_SOURCE`, `PMOS_NETWORK_SUBNET`, `PMOS_NETWORK_IP_RANGE`, config path; `PMOS_MODEL_SOURCE` to the other four; `PMOS_CONNECTORS` and `PMOS_EXPOSURE` where §3.6 and §3.8 say; `PMOS_GATEWAY_URL` to the tool server; `PMOS_VLLM_IMAGE` without a default), `deploy/env.example` (the host-settings block of §3.1; the model block under `local`, `PMOS_VLLM_IMAGE` empty), `deploy/setup.sh` (the three settings with their defaults, `--model-source` and `--exposure`; the NVIDIA warning only for `local`), `deploy/pmos-admin` (`up`'s early messages, `sandbox`'s content check, `status` lines), `deploy/gateway.hosted-test.yaml` (new, example hosts only, no key), `scripts/claude-test-stack.sh` (passes `hosted-test`), `docs/gateway.md` (`PMOS_MODEL_SOURCE`, the resolver, which class serves which source) | §6 H4 rows | Strongest (security refusals, cross-crate) | **Yes**: every change refuses more than today; `hosted` does not exist until H12 | No |
| H3 | **Policy: `allowed_ips` per name and wording** (O6, decided) | `crates/policy/src/lib.rs` (`TOOLSERVER_ADDR`, `GATEWAY_ADDR`, `BOTS_ADDR`; "verify on a real host"), `generate.rs` (emit `allowed_ips`; header wording without "Spark"), `check.rs` (`net-allowed-ips` in `RULES`), `crates/policy/tests/policies.rs` (and the compose-literals test), regenerated `policies/**`, `deploy/pmos-admin` (`up` refuses a subnet without the constants), `docs/sandbox-policies.md` (rule table, "verify on a real host" table), `docs/policy-admin.md` (the fixed list); the web Policy Admin's fixed facts if it lists endpoint fields (`crates/web/src/app/policy.rs`, `crates/policy/src/admin/`) | §6 H3 row; persona tests still show only skill-folder differences | Strongest (cross-crate, POL-2 network change) | **Yes** | Applied on a host after the acceptance run's H-1 |
| H9 | **Internal route** | `deploy/compose.yaml` (`PMOS_WEB_INTERNAL_BIND`), `deploy/env.example` (`PMOS_WEB_BRIEF_URL` on 8771), `docs/runbooks/install.md` (health and internal routes not forwarded, the body sizes), H-EXPOSURE's 8771 | §6 H9 rows | Medium | **Yes** | No |
| H10 | **Sign-in and admin routes, as D2 (a) and D3 (b) decided** (after H4 and H9) | `crates/web` (lifts H4's refusal of `public` only together with: `PMOS_WEB_PASSWORD_CLIENTS` with its prefix bounds, required on `public` with password sign-in; dev sign-in refused on `public`; `PMOS_WEB_ADMIN_BIND` required on every host with the web app on, its address check, `/api/admin/*` `404` on 8770), `deploy/compose.yaml` (8772 on `PMOS_WEB_ADMIN_PUBLISH`), `deploy/env.example`, `deploy/pmos-admin` (`up --web`), `deploy/preflight.sh` (H-SIGNIN's and H-ADMIN's rules; 8772 in H-EXPOSURE), `docs/runbooks/install.md` (the public nginx example and the firewall section), `.github/workflows/ci.yml` (`nginx -t` if available), `docs/web-app-running.md`, `docs/password-sign-in.md` | §6 H10 row | Strongest (authentication) | **Yes** (D2, D3 decided) | No; RT-14 after it on a host |
| H12 | **The hosted model as a production source** (after H4; the approved list ships empty) | `crates/gateway` (`hosted` source; `crates/gateway/approved-hosted.yaml`, compiled in, with its unit test; the `hosted:<label>` upstream form as the one addition to `class: production`; URL normalisation and digest; `PMOS_GATEWAY_HOSTED_KEY_FILE`; the hosted client with no redirects; the hosted resolver rule; the cached `/readyz` probe; `hosted_label` in `/readyz` and `/v1/pmos/roles`; labels in the usage log for `hosted` and `hosted-test`; comments that state ADM-3 rewritten), `crates/toolserver` (`session.open` audit event; `model_source` on every event of a session; `system.model_source` on a change), `crates/bots` (cross-check accepts `hosted`), `crates/evals` and `evals/config.yaml` (a `hosted:<label>` production gateway may count, per §3.6), `deploy/compose.yaml` (`PMOS_GATEWAY_HOSTED_URL`, the `secrets:` entry for the gateway only), `deploy/gateway.hosted.yaml` (new, `hosted:` upstream with a placeholder label, no URL), `deploy/env.example`, `deploy/setup.sh` (creates `deploy/secrets/` and the empty key file), `deploy/pmos-admin` (`hosted-key set` from standard input; the mode checks; `backup` leaves the key out; `up` checks `NO_PROXY` under `hosted`; `status` shows the source, the label and its last change), `.gitignore` (`deploy/secrets/`), `docs/model-providers/README.md` (new: what a record must hold, §3.9), `docs/gateway.md`, `docs/audit-retention.md` (the new field) | §6 H12 row | Strongest (the loosened data rule, cross-crate) | **Yes**: with an empty list no endpoint can serve `hosted`, so nothing real can flow until H13 | No |
| H13 | **The owner's hosted model approved** | `crates/gateway/approved-hosted.yaml` (the first entry: label, digest, model ids, record), `docs/model-providers/<label>.md` (the owner's approval, the data-processing agreement and the retention terms, each confirmed by the owner), `evals/config.yaml` if the production role models move to `deploy/gateway.hosted.yaml` | the list's unit test; RT-19 on a host | Medium (data entry; the reviewer checks the record) | **No**: waits on the owner's approval record, the DPA and retention confirmations, and the CODEOWNERS line and branch protection (§3.9) | Real data also needs Gate 1 |
| H7 | **Red-team additions** (after H3, H10, H12) | `policies/redteam/checklist.md` (P-0, P-7 to P-9, RT-11 to RT-19, E-5; "Run it on the host"), `policies/redteam/isolation.sh` (RT-11 to RT-13b and RT-18; RT-13b's temporary hosts line under a `trap`, no `pmos-admin` override; RT-18's key search printing only found or not found), `policies/redteam/exposure.sh` (new; RT-14, 8772 included), RT-16's Docker-restart and flush steps, RT-19 as a script step that runs only when an approved entry exists | `bash -n`; `shellcheck` if CI has it; a dry run that prints the probes without OpenShell | Strongest | **Yes** | Runs only on a host |
| H6 | **Docs and wording, and the Spark inventory** (after the slices it describes) | every file in the inventory below (§7.1), plus the docs §3.9 lists as stating the old rule | the docs lint, if any; CI's workflow syntax; `git grep -i spark` afterwards shows only the rows the inventory keeps | Lighter (a strongest-model pass on `docs/gateway.md`, `docs/implementation-plan.md` and the install runbook, which state security rules) | **Yes** | No |
| — | **Acceptance run on the first host** (O1: Ubuntu 24.04 amd64 VM, no GPU, fake data, `hosted-test`) | `docs/hosts/<date>-ubuntu-24.04-amd64-vm.md` (new) | the run itself (§6) | Operator with the conductor; reviewer checks the record | Ready once H0, H0b, H2, H1, H4, H3, H9 and H7 are merged (H10 if the web app is wanted) | **Needs the host** |
| H5 | **Launch confirmation** | whatever the acceptance run's step 2 showed: `deploy/pmos-admin` (`sandbox` flags if `--from` differs), `deploy/systemd/pmos-worker@.service` (if `sandbox exec` doesn't stay in the foreground, or OpenShell's unit has another name), `deploy/Dockerfile.worker` (if OpenShell maps the user differently), `docs/sandbox-policies.md` | `scripts/test-pmos-admin.sh` (`--dry-run`); `systemd-analyze verify`; E-5 on the host | Medium | After the acceptance run's step 2 | Yes |
| — | **Acceptance run with `local`** on a GPU host | `docs/hosts/<date>-<kind>.md` | the run with `--model-source local` | as above | Only if the owner names a GPU host (§8) | Needs that host |
| H8 | **Tier 2, Kubernetes** | a separate spec first | — | Strongest | Not for the pilot (O2) | — |
| ~~H11~~ | **Dropped.** "Where real data may run" waited on D1 and would have added `PMOS_HOST_PLACE` and H-PROFILE's `company` case. D1 is answered with no host-based rule (§3.6), so there is nothing left for it to build; the hosted part of D1's answer is H12 and H13 | — | — | — | — | — |

**Order.**

1. **H0 and H0b**: built; the reviewer checks them, then the conductor merges them.
2. **H2** next: H1's fixtures and H4's resolver depend on its addresses and `ip_range`.
3. **H1, H4 and H3 in parallel** after H2 (H1 is shell and fixtures; H4 is mostly the crates; H3
   is the policy crate; H1 and H4 both touch `deploy/pmos-admin` and H4 also `compose.yaml`: the
   conductor merges H1 first, then H3, then H4).
4. **H9** after H4 (both edit `compose.yaml` and the web app's settings).
5. **H10 and H12 in parallel** after H4 (and H9 for H10). H10 is `crates/web`; H12 is
   `crates/gateway`, `crates/toolserver` and `crates/evals`; both touch `deploy/compose.yaml`,
   `deploy/env.example` and `deploy/pmos-admin`: the conductor merges H10 first.
6. **H7** after H3, H10 and H12. **H6** after H7, in parallel with the acceptance run.
7. **The acceptance run**, then **H5**.
8. **H13** whenever the owner's conditions are met; it can come before or after the acceptance
   run, and needs no host to merge.

**Buildable now, with no owner decision:** H2, H1, H4, H3, H9, H10, H12, H7 and H6 (H0 and H0b are
built and awaiting review). **Needs the owner:** H13 only (the approval record, the DPA and
retention confirmations, the CODEOWNERS line and branch protection). **Needs a real host:** the
acceptance run and H5 (O1 names the kind: an Ubuntu 24.04 amd64 VM without a GPU); a run with
`local` needs a GPU host the owner names. No fix in H0 counts as verified until the acceptance
run.

Each batch goes to the reviewer session as the security gate, as usual. **H12 must not merge
before the reviewer has checked it**, and H13 not before the reviewer has read the record.

### 7.1 H6's inventory: every "spark" in the repo

Made with `git grep -i -o spark` at `57e2e72`, case-insensitive, over every tracked file
(`target/` and `node_modules/` are not tracked, so they are excluded). **403 occurrences in 74
files.** Counts are occurrences. "Rename" means a generic host term ("the host", "PM OS's host",
"a GPU host", "verify on a real host"); "rewrite" means the sentence states a rule or a step that
changes; "keep" means the text is a dated record or not about the Spark.

| Area | Occurrences (files) | Files and counts | Action |
| --- | --- | --- | --- |
| This spec | 148 (1) | `docs/openshell-any-host-spec.md` | **Keep.** The record of the Spark-era design, the reviews (§10, §11) and the change of direction (§12); its current sections name the Spark only to say it is not a target |
| Docs that describe current behaviour or rules | 81 (15) | `docs/implementation-plan.md` 23, `docs/sandbox-policies.md` 14, `docs/evals.md` 13, `docs/gateway.md` 6, `docs/toolserver.md` 4, `docs/web-app-running.md` 3, `docs/bots-runtime.md` 3, `docs/bot-decisions.md` 3, `docs/user-assertion.md` 2, `docs/testing-with-claude.md` 2, `docs/password-sign-in.md` 2, `docs/docs-server.md` 2, `docs/console.md` 2, `docs/policy-admin.md` 1, `docs/entra-sign-in.md` 1 | **Rewrite** `implementation-plan.md` (`:4`, ADM-3 at `:116`, the metric at `:118`, Gate 1 "Needs the Spark", Phase 3's "a second Spark" becomes "a second model server"; the status rows' "on the Spark" renamed), `gateway.md` (ADM-3, "Allowed upstream hosts", the usage log's "zero by construction", per §3.6 and §3.9), `sandbox-policies.md` (the "Verify on the Spark" table becomes "Verify on a real host", the host acceptance list of §6; "Apply on the Spark" becomes "Apply on the host"), `evals.md` (the eval gate's runner `pmos-host`; "the Spark's gateway" becomes "the production gateway"; which runs count under `hosted`), and `bot-decisions.md` (bot decision D2 "long-lived services on the Spark" keeps its meaning as "on PM OS's host", with a dated note of the owner's change). **Rename** in the other ten |
| Specs and research of record | 23 (7) | `docs/security-fixes-spec.md` 7, `docs/research/self-improvement-loops.md` 5, `docs/bot-marketplace-spec.md` 4, `docs/bot-runtime-spec.md` 3, `docs/improvement-loop-spec.md` 2, `docs/user-database-spec.md` 1, `docs/onboarding-spec.md` 1 | **Keep**: dated designs and a research report; rewriting them would misstate what was decided when. Where a living doc cites one of them for a Spark fact, the living doc carries the correction |
| Runbooks | 48 (8) | `docs/runbooks/upgrade.md` 18, `install.md` 12, `backup-restore.md` 6, `README.md` 5, `quality.md` 3, `incidents.md` 2, `users.md` 1, `daily.md` 1 | **Rewrite** `install.md` ("Before you start" for any host, preflight first; DGX OS and "the NVIDIA container toolkit (both come with DGX OS)" become "for `local` only: an NVIDIA GPU and the NVIDIA container toolkit"; Stage 3 in three paths, `local` with a chosen `PMOS_VLLM_IMAGE`, `hosted` after H13, `hosted-test`; the red-team and console lines), `upgrade.md` ("Registering the Spark CI runner" becomes "Registering the eval-gate runner": any architecture, label `pmos-host`, name `pmos-host-1`, `PMOS_HOST_DIR`), `README.md` ("one DGX Spark" becomes "one Linux host"; ports "on the host"), `incidents.md` ("The Spark is gone" becomes "The host is gone"). **Rename** in `backup-restore.md`, `quality.md`, `users.md`, `daily.md` (and "a second Spark" becomes "a second model server") |
| Top level | 16 (4) | `README.md` 5, `CLAUDE.md` 5, `CHANGELOG.md` 5, `.gitignore` 1 | **Rewrite** `README.md` (`:5` "runs on the team's DGX Spark with open-weight models", `:67`, `:69`, `:86` "Run it on the Spark", `:112`). **Keep** `CLAUDE.md` (the owner's requirements; the conductor updates it, and it already records the change) and `CHANGELOG.md` (history; H6 adds an entry for the change). **Rename** the `.gitignore` comment |
| `deploy/` | 15 (6) | `compose.yaml` 4, `pmos-admin` 3, `gateway.yaml` 3, `env.example` 3, `setup.sh` 1, `Dockerfile` 1 | **Rewrite** the model comments in `compose.yaml` (`:368`) and `env.example` ("Models (verify on the Spark)") per §3.6, and `gateway.yaml`'s "the Spark's own gateway" (`class: production` is the deployment's own gateway, `local` or `hosted`). **Rename** the rest ("PM OS on one host", "this host's outbound proxy", "built on the host, for its own architecture", the runner comment at `pmos-admin:82-83`) |
| `config/` | 4 (2) | `gateway.yaml` 3, `gateway.claude-test.yaml` 1 | **Rewrite** `gateway.yaml`'s "prompts never leave the Spark" (`:4-5`, ADM-3); **rename** the other |
| `.github/workflows/` | 10 (2) | `evals-gate.yml` 9, `ci.yml` 1 | **Rewrite** `evals-gate.yml`: runner labels `self-hosted, pmos-host`, `PMOS_HOST_DIR` (default `/opt/pmos`), job name "Eval gate (host)". `PMOS_SPARK_DIR` is **deleted**, not kept as a fallback: no runner was ever registered, so nothing reads it. **Rename** in `ci.yml` |
| Crates, source | 25 (12) | `crates/gateway/src/config.rs` 7 (3 of them in its test module), `crates/policy/src/lib.rs` 5, `crates/gateway/src/lib.rs` 2, `crates/policy/src/generate.rs` 2, `crates/evals/src/report.rs` 2, `crates/gateway/src/usage.rs` 1, `crates/gateway/src/anthropic.rs` 1, `crates/toolserver/src/content_sync.rs` 1, `crates/slack/src/socket.rs` 1, `crates/contract/src/model.rs` 1, `crates/console/src/config.rs` 1, `crates/console/src/auth.rs` 1 | **Rewrite in the slice that changes the rule:** the gateway's ADM-3 comments (`lib.rs:2-6`, `config.rs:265-267`, `:673`, `:693`, `:814`, `usage.rs:3`) in H12; `policy/src/lib.rs` and `generate.rs` (generated headers) in H3, which regenerates `policies/`. **Rename** the test `refuses_anything_off_the_spark` (`config.rs:918`) to `refuses_non_local_upstreams_outside_testing` and its `spark.corp.example` data to `models.corp.example`, in H12. **Rename** the other comments in H6 (`model.rs:253` "Platform owner: runs the Spark" becomes "runs PM OS's host") |
| Crates, tests | 8 (1) | `crates/gateway/tests/limits.rs` 8 | **Rename** in H12: `spark.corp.example` becomes `models.corp.example`; "the Spark's gateway is the production one" becomes "the deployment's gateway is the production one". The tests' assertions don't change |
| `policies/` | 11 (6) | `README.md` 4, `redteam/checklist.md` 2, `redteam/isolation.sh` 2, `bots/docs.yaml` 1, `providers/pmos-gateway.yaml` 1, `providers/pmos-worker.yaml` 1 | **Rewrite** `redteam/*` in H7 ("Run it on the host, never on a machine shared with other work"; P-1's DGX OS kernel line becomes P-0). **Regenerated** by H3: the three generated files. **Rename** in `README.md` ("Applying on the host") |
| `evals/config.yaml` | 1 (1) | `:76`, a quote of the implementation plan's "a second Spark" | **Rewrite** with the plan's new wording, in the same commit |
| `content/` | 2 (1) | `content/context/decisions/2026-10-product-agent-platform.md` | **Not changed by H6.** The team's platform decision is content the owner's team owns; amending it goes through the content admin flow. Flagged to the owner (§8) |
| `web/` | 8 (7) | `Palette.tsx`, `bits.tsx`, `icons.tsx`, `DraftPage.tsx` 2, `Home.tsx`, `Thread.tsx`, `admin/Content.tsx` | **Keep.** Every hit is the icon name `sparkle`; no UI text names the Spark |
| Password list | 3 (1) | `crates/toolserver/data/common-passwords.txt` | **Keep.** Common passwords that contain the letters (data, not wording) |

**Summary:** 148 kept in this spec, 23 kept in dated specs and research, 10 kept in `CLAUDE.md`
and `CHANGELOG.md`, 2 left to the owner in content, 11 false positives (8 `sparkle`, 3 passwords);
the other **209** are renamed, rewritten, regenerated or deleted, in H3 (7 in the policy crate,
3 in generated files), H7 (4), H12 (gateway: 10 in `config.rs`, `lib.rs`, `usage.rs` and 8 in
`limits.rs`) and H6 (the rest).

## 8. Decisions for the owner

### 8.1 Decided by the owner, 2026-11-22

Neither the reviewer nor the conductor answered these. The owner gave the change of direction
("Deploy on spark scrap that idea I want to make it platform agnostic") and then accepted every
recommendation the conductor put ("Let's go with your recommendation"), as CLAUDE.md item c
records. Each entry gives the answer and its consequences in this spec.

1. **The Spark as a target: dropped.** PM OS runs on any suitable Linux host with OpenShell.
   *Consequences:* no `PMOS_HOST_PROFILE`, no H-PROFILE, no Spark-only defaults (§3.1, §3.2); Tier
   1 is the only target (§4); every rule that told a Spark from a cloud VM is replaced by the one
   rule for every host (H-METADATA and H-EXPOSURE fail everywhere, §3.2; the threat table, §5); the
   wording slice H6 carries the inventory (§7.1).
2. **D1. Where real data may be processed: both a hosted model and a local model on a GPU host
   the owner controls.** The hosted model is the owner's connected provider, the same model used
   for the eval runs (an OpenAI-compatible endpoint). This **replaces ADM-3** ("prompts never
   leave the host"; restricted data only on the Spark's local models). *Consequences:*
   `PMOS_MODEL_SOURCE` gains `hosted`, served by a `class: production` gateway only from an
   owner-approved, compiled, CODEOWNERS-reviewed list (§3.6, H12); real data flows to it only after
   the conditions of §3.9 (H13); fake-data mode becomes `hosted-test` alone; H11 is dropped; the
   earlier answers (a), (b) and (c) of D1, which asked about cloud disks rather than hosted
   models, are withdrawn, and with them decision O4 ("keep ADM-3"). All data classes may reach the
   hosted model; no data-class ceiling is designed (§3.9).
3. **D2. Sign-in on an internet-facing host: (a).** Microsoft (Entra) sign-in only; password
   sign-in only from a VPN or an allow-listed range, with the bounds in D2's text (every entry no
   shorter than `/16` for IPv4 or `/48` for IPv6; `0.0.0.0/0`, `::/0` and `::ffff:0:0/96` refused).
   *Consequences:* H10 is buildable now; `PMOS_WEB_PASSWORD_CLIENTS` is required on `public` with
   password sign-in, and `PMOS_WEB_PASSWORD_REACH` is not added (§3.8); PMs without Entra use the
   VPN or an allow-listed range; until the owner's Entra app registration exists, a public host
   signs in nobody outside the allow-list.
4. **D3. Admin routes on a public listener: (b), a separate admin-only listener.**
   *Consequences:* H10 adds `PMOS_WEB_ADMIN_BIND` (8772), published only on loopback or a private
   address; `/api/admin/*` answers `404` on the browser-facing listener; this spec applies it on
   **every** host, not only `public` (§3.8; the reviewer should confirm, §12);
   `PMOS_WEB_ADMIN_ON_PUBLIC_LISTENER` is not added; the accepted limit "a broken-into web backend
   can act as any PM" remains for the PM routes.
5. **O1. The first host to verify on: an Ubuntu 24.04 x86 (amd64) virtual machine without a GPU,
   with fake data.** *Consequences:* the acceptance run (§6) uses `hosted-test` with the owner's
   hosted model as a test endpoint; it verifies that host kind only; `local`, arm64 and `hosted`
   with real data need runs of their own.
6. **O6. Each service name pinned to its own `/32` in the sandbox policy: yes.** *Consequences:*
   H3 is buildable now; RT-13b's expected result no longer depends on it (§5).
7. **Earlier recommendations, accepted as they stand:**
   - **O2.** No Kubernetes for the pilot: H8 stays a later spec.
   - **O3.** Local models on NVIDIA GPUs (any, a Spark included) are offered as **"designed for;
     not verified"** until an acceptance run with `local` passes on such a host. `PMOS_VLLM_IMAGE`
     has no default (§3.6).
   - **O5.** The web app stays on the host behind a reverse proxy, not on Vercel.
   - **O7.** The console is never on a public listener.
   - **O8.** systemd user units supervise the workers (§3.7, built in H0).
   - **O4** is withdrawn by D1's answer (above).

### 8.2 Still open for the owner

Nothing blocks a slice except H13 and the host runs. What remains:

1. **The hosted model's approval record and conditions (H13):** the owner's recorded approval of
   the provider and model, and the owner's confirmation of the data-processing agreement and the
   retention terms (§3.9). This spec lists them as conditions; it states none as fact.
2. **The CODEOWNERS line and branch protection** for `crates/gateway/approved-hosted.yaml` and
   `docs/model-providers/` (already waiting on the owner for `policies/README.md`'s lines).
3. **Which GPU host, if any,** for a run with `local`. None is needed for the pilot if the hosted
   model serves it.
4. **The first host itself:** an operator with an Ubuntu 24.04 amd64 VM on a cloud or server the
   company has (O1 names the kind, not the account).
5. **Not new, but now on the path to a public host:** the Entra app registration (CLAUDE.md,
   "Waiting on the owner"), without which a public host signs in only allow-listed password users.
6. **The team's platform decision** (`content/context/decisions/2026-10-product-agent-platform.md`)
   still says the team runs its agents on the DGX Spark. Amending it is the owner's team's
   content, not this spec's.
7. **Optional, if the owner wants it later:** a data-class ceiling for the hosted model, or a
   check of a provider's confidential-computing attestation (§3.9). Neither is designed.

### 8.3 The decisions as they were put (record)

The text below is the earlier revision's list, kept so the reviewer can see what was answered.

1. **O1. The first real host to verify on.** Recommendation: **an Ubuntu 24.04 LTS amd64 VM**
   with Docker Engine 28+, no GPU, run as a closed test with fake data and `hosted-test`.
2. **O2. Tier 2 (Kubernetes) for the pilot?** Recommendation: **no.**
3. **O3. Must the local-model path work off the Spark, on other NVIDIA GPUs?** Recommendation:
   **not as a verified path for now**: "designed for; not verified".
4. **O4. A hosted model for real work on a host without a GPU?** Recommendation then: **keep
   ADM-3**. Withdrawn by D1's answer.
5. **O5. The web front end on Vercel?** Recommendation: **keep the web app on the PM OS host
   behind the reverse proxy.**
6. **O6. Pin each of the three names to its own address (`allowed_ips`, one `/32` each)?**
   Recommendation: **yes**.
7. **O7. The console on a public host.** Recommendation: **never on a public listener.**
8. **O8. Worker supervision with systemd.** Recommendation: **systemd user units.**
9. **D1. May restricted data be stored or processed at a cloud provider at all?** Answers (a) real
   data only on hosts the company owns, (b) internal yes and restricted no, (c) all classes under
   conditions; recommendation then (a). The owner's answer, put to them as "where real data may
   be processed now that there is no Spark", is 8.1 item 2.
10. **D2. What must a `PMOS_EXPOSURE=public` host require for sign-in?** (a) Entra only, password
    from a VPN or allow-list; (b) the web app only behind a VPN; (c) password sign-in on the
    internet as built. Recommendation: (a).
11. **D3. The accepted limit on a public listener.** (a) accept it, admin routes served
    publicly; (b) keep the limit, admin routes on their own listener; (c) not acceptable on a
    public host at all. Recommendation: (b).

## 9. What this spec does not cover

- **Requirement i** (many providers, switching and automatic fallback, like OpenRouter; OpenShell
  holding provider keys for the gateway): not started; the owner's go-ahead is pending. This spec
  designs only one approved hosted model as a production source (§3.6); a second provider, a
  second label in one gateway, a mix of local and hosted roles, and any fallback are requirement
  i's.
- **A data-class ceiling for the hosted model**, and **checking a provider's confidential-computing
  attestation** (§3.9): not designed; possible later slices if the owner asks.
- **A hosted provider reached over a private endpoint** (an address inside the VPC): the hosted
  resolver refuses private answers (§3.6); allowing one is a reviewed change of its own.
- **The OpenShell policy prover in Policy Admin:** a separate plan awaiting approval.
- **OpenShell's sandbox allow and deny decisions in the audit log:** a separate plan awaiting
  approval. The red-team script still reads OpenShell's logs directly.
- **MCP servers in place of connectors** (scope item g): a separate plan awaiting approval.
- **Multi-host PM OS** (the stack split across machines) and **high availability.**
- **A gateway using a model server on another machine**, which `docs/implementation-plan.md:204`
  (Phase 3) plans as "a second Spark if p90 queue time passes 2 minutes". With the `local-hosts`
  exception removed (§3.6), a `class: production` gateway reaches only model servers pinned on
  its own host's `pmos` network, or the approved hosted model; a second machine of PM OS's own
  needs its own design (how the gateway reaches it, how that link is authenticated and pinned).
- **Per-cloud automation** (Terraform, cloud images, managed Postgres, managed load balancers).
- **Other GPU vendors** for `local`.
- **Changing the platform decision in the content repo**
  (`content/context/decisions/2026-10-product-agent-platform.md`), which names the Spark (§8.2).
- **Splitting the web backend** so a break-in can't sign as any PM (D3's former answer (c)).

## 10. Review of 2026-11-22

The reviewer's verdict on `9de1a65`: "sound after changes", no High or Critical in the sandbox
design. What changed:

1. **The `hosted-test` refusal moved into the services** (§3.6). It was in `pmos-admin up` only;
   `docker compose up`, `add-bot` and `restart` skipped it. The gateway, tool server and
   `pmos-slack` now refuse at start-up on `PMOS_MODEL_SOURCE`, and the runtime cross-checks it.
   "Fake data only" now covers the content library, the memory repos, the database and Slack
   tokens. (Re-based in §11, N3: this item said the gateway's "local" would no longer include any
   private address, by redefining `is_local_host`. A `class: production` gateway already takes
   only `<label>.pmos.internal` names; `local` is now tied to that class, and `is_local_host`,
   at `crates/gateway/src/config.rs:671-689` at `6bb806f`, stays as it is for `class: test`,
   which runs only in fake-data mode.)
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

## 11. Re-review of 2026-11-22

The reviewer's re-review: the seven changes of §10 are in (2, 5, 6 and 7 done; 1, 3 and 4 in
part), and three things must change before slice H4 is built. Each point was checked against the
repo at `6bb806f` before it was written in. What changed:

**Must change before H4**

- **N1. No public web app until H10.** Nothing refused a public host with password sign-in until
  H10, which waits for D2 and D3, while H4 created `PMOS_EXPOSURE`, H9 put the public nginx example
  in the install runbook and the acceptance run used `--exposure public`; so D2's answer (c) was
  the default. Now H4 makes `pmos-web` require `PMOS_EXPOSURE` and refuse `public` outright until
  H10 (§3.1, §3.8); the admin routes' default is that refusal in the service, not an nginx block,
  and from H10 serving them publicly needs `PMOS_WEB_ADMIN_ON_PUBLIC_LISTENER=owner-accepted`
  (§3.8, D3). The public nginx example and firewall section move from H9 to H10. The acceptance
  run keeps the web app off, or reaches it only through an SSH tunnel on a host declared `private`
  with H-PRIVATE acknowledged (§6). H-SIGNIN fails `public` with the web app on until H10.
- **N2. Real data only on the Spark until D1.** Before, `linux` + `local` + `PMOS_CONNECTORS=live`
  was accepted until H11. Now "fake-data mode" (§3.6) holds under `hosted-test` **or** on any
  profile but `spark` until D1 is answered, and the tool server, the runtime, the web app and
  `pmos-slack` refuse every real source in it (live connectors, GitHub memory, content without
  `TEST-CONTENT` or with a `restricted` file, a database that has held real data, Slack tokens).
  This goes slightly beyond the reviewer's two settings, so that the definition of "fake data" is
  the same in both cases. The profile is self-declared, and the spec says so (§3.1, §3.6); H-PROFILE
  fails a `spark` declaration the host doesn't support, unless acknowledged.
- **N3. The gateway as it is since G6/G7.** §2 surprise 13, the "on this host" part of §3.6, the
  `local-hosts` exception and §10 item 1 are re-based on the existing rule: a `class: production`
  gateway already refuses `local-hosts`, testing-only upstreams and any model server not named
  `<label>.pmos.internal` (`crates/gateway/src/config.rs:375-380`, `:543-557`,
  `is_production_host` at `:691-711`), and `deploy/gateway.yaml` uses those names. `local` now
  requires `class: production` (the stub may be on and then reports `class: test`, as
  `docs/gateway.md` says); what H4 adds is a resolver that checks each production name's answer at
  every connection against the pinned part of the subnet. `class: test` keeps `is_local_host`
  (moved to `config.rs:671-689`) and is allowed only with `hosted-test`. `PMOS_GATEWAY_LAN_UPSTREAMS`
  is dropped, since a production gateway refuses `local-hosts` itself. Client caps, trusted
  clients, `from-name` and queue caps are unchanged and apply to both classes (§3.6).

**Medium**

- **Change 1 (a), content reaching a sandbox unchecked.** The restricted-content check is one
  function in `crates/contract`, run by the tool server, the runtime and the web app (which mount
  the checkout) and by `pmos-admin sandbox` before it uploads skills and master context (§3.6).
- **Change 1 (b), the database's history.** The one-time "model source at first start" record is
  replaced by a permanent `held_real_data` marker, written at any start with live connectors or
  GitHub memory, in the tool server's, the runtime's and the web app's databases (§3.6).
- **Change 1 (c), the tool server's cross-check.** The tool server reads the gateway's reported
  `model_source` and refuses to open a session on a mismatch (§3.6). It reads `/readyz`, which
  H4 extends with `model_source`, rather than `/v1/pmos/roles` as the reviewer suggested, because
  `/v1/pmos/roles` needs a client token that would also let the tool server call the model; the
  reviewer should confirm this is acceptable.
- **`PMOS_GATEWAY_LAN_UPSTREAMS` refused only by `up`.** The setting is removed; the refusal of
  `local-hosts` under `local` is the gateway's own (N3).
- **A `/8` subnet passed.** `pmos-admin up` and H-SUBNET require `PMOS_NETWORK_SUBNET` to be a
  `/24` or longer (§3.3, §3.5, H2).
- **`PMOS_WEB_ADMIN_BIND` unchecked.** `pmos-web` refuses a public address in it; the host-side
  publish (`PMOS_WEB_ADMIN_PUBLISH`) must be loopback or private, checked by `up --web` and
  H-EXPOSURE; port 8772 is in H-EXPOSURE and RT-14 (§3.2, §3.8, §5).
- **`0.0.0.0/0` as an allow-list.** `PMOS_WEB_PASSWORD_CLIENTS` refuses `0.0.0.0/0`, `::/0`,
  `::ffff:0:0/96` and any prefix shorter than `/16` (IPv4) or `/48` (IPv6) (§3.8).
- **vLLM addresses could be taken.** The vLLM services get pinned addresses (`.13`, `.14`), and the
  gateway's resolver accepts only answers outside the `ip_range`, at every connection (§3.3, §3.6).
- **The metadata block lost on a Docker restart.** `host-firewall apply` also installs a
  `docker.service` drop-in and a one-minute timer, and its rules match `-i pmos0` (the bridge now
  named in H2) and `-i docker0`; RT-16 restarts Docker and flushes the chain (§3.2, §5).
- **Declarations that switch checks off.** H-PRIVATE fails `private` on a host with a public
  address or a metadata answer, and H-PROFILE fails `spark` (and, from H11, `company`) where the
  host doesn't look like one, unless an operator records an acknowledgement: fingerprinted in
  `deploy/.env`, logged in a root-owned append-only file, printed by every `status` and in the
  acceptance record. An acknowledged `private` runs H-METADATA, H-EXPOSURE and H-DEVLOGIN at
  `public` strength (§3.1, §3.2).
- **Change 3, build order.** H2 now comes before H1 (and H4), since H1's fixtures test the
  `ip_range` and H-NAMES against H2's pinned addresses and managed block (§6, §7).

**Low**

- H-EXPOSURE and §3.8 agree on 17670: `127.0.0.1`, and possibly the host's address on the Docker
  bridge OpenShell's driver uses (`docker0` by default), marked **verify on a real host**.
- H-METADATA's wording: the probe never requests a credential, but on AWS it does request an
  IMDSv2 session token; the spec now lists the three requests exactly and says nothing they
  return is stored or logged (§3.2).
- RT-13b's override is removed from `host-names apply`; the red-team script writes the temporary
  line itself under a `trap` that restores the block (§3.3, §5).
- "20 of its skills are restricted" corrected to 45 of the 46 (all but `docs`), counted at
  `6bb806f` (and the same at `c25c07e`), in §3.6 and D1 (b).
- The public docs site's setting is `PMOS_DOCS_BASE_URL` on the `docs` service; `PMOS_DOCS_URL` is
  the tool server's address for the docs server (`deploy/compose.yaml`).
- The nginx example keys on `$binary_remote_addr`, with the trade-off stated (one key per IPv6
  address, not per `/64`; drop the IPv6 listeners if that matters more), instead of a regex that
  fails on compressed addresses.
- `<<: *proxy` goes under the `gateway` service's `environment:`, as in the other services.

**Also updated:** the slice table, the build order, the lists of what may be built before D1 to D3
and before a real host, and a list of the slices safe to build now with no owner decision: H2, H1,
H4 and H9, and H7 and H6 except their answer-specific lines (§7). Still true: nothing in this
spec has run on a real host with OpenShell, the Spark included.

### Reviewer's answers, 2026-11-22

The reviewer accepted the second revision with five changes, written in here:

1. **The `/readyz` cross-check** (accepted). The tool server refuses `POST /internal/sessions`
   when the gateway's answer is missing, unreadable or differs, failing closed, at every session
   open or from a cache of at most 10 seconds, not only at start-up. The fields `/readyz` gains
   are labels only (`class`, `stub`, `model_source`, upstream kinds), never an address, a host
   name or a key, since anything that reaches the gateway can read it; tests assert both (§3.6,
   §6, H4). Found while writing it in: today's `/readyz` keys `upstreams` by base URL
   (`crates/gateway/src/app.rs:292-352`), so H4 replaces that map, and §3.6 no longer says
   `/readyz` already reports `class` and the upstream kinds (that was `/v1/pmos/roles`).
2. **The `local-hosts` exception removed** (accepted). A gateway using a model server on
   another machine, such as the second Spark of `docs/implementation-plan.md:204`, is out of
   scope and needs its own design (§9).
3. **H-PROFILE on `spark` can't be acknowledged before H11.** It is a hard fail;
   `pmos-admin host acknowledge H-PROFILE` refuses, and a hand-written acknowledgement line fails
   too. A real Spark that fails it is a bug in the check, fixed under review. H-PRIVATE keeps its
   acknowledgement (the acceptance run needs it), and from H11 `company` may take one if D1's
   answer allows (§3.1, §3.2, §3.6, §5, §6, H1, H4, H11; P-8).
4. **The allow-list's bounds in D2** (no prefix shorter than `/16` for IPv4 or `/48` for IPv6;
   `0.0.0.0/0`, `::/0` and `::ffff:0:0/96` refused) are now in decision D2's answer (a) (§8).
5. **Two additions.** (a) With H-PRIVATE acknowledged, the preflight also fails while anything
   on the host listens on 80 or 443 on a non-private address, so an operator's own proxy in front
   of 8770 can't bring back a public sign-in page before H10 (§3.2, §3.8; P-9). (b) H-PROFILE and
   H-PRIVATE run only from `pmos-admin` and the unit's `ExecStartPre`, so `docker compose up`
   skips them and the services see only the declaration: a stated limit in §5's table, with
   RT-17 proving the services still refuse live connectors on `linux` however they are started.

## 12. Change of direction, 2026-11-22

**What the owner decided.** "Deploy on spark scrap that idea I want to make it platform
agnostic": the DGX Spark is no longer a deployment target, and PM OS runs on any suitable Linux
host with OpenShell. The owner then accepted every recommendation the conductor put ("Let's go
with your recommendation"): real data may be processed by both the owner's hosted model and a
local model on a GPU host the owner controls (D1, replacing ADM-3); Microsoft sign-in only on an
internet-facing host, password sign-in only from a VPN or an allow-listed range (D2 (a)); the
admin routes on a separate admin-only listener (D3 (b)); each service name pinned to its own
`/32` (O6); the first host an Ubuntu 24.04 amd64 VM without a GPU, with fake data (O1); and
O2, O3, O5, O7 and O8 as recommended (§8.1).

**What changed in this spec.**

1. **No Spark target.** `PMOS_HOST_PROFILE` and H-PROFILE are removed, not replaced by a generic
   value; three host settings remain (`PMOS_MODEL_SOURCE`, `PMOS_EXPOSURE`,
   `PMOS_OPENSHELL_DRIVER`), each defaulting to the value that allows less (§3.1). Where a rule
   only told a Spark from a cloud VM, the stricter rule now holds on every host: H-METADATA and
   H-EXPOSURE fail everywhere; only H-PRIVATE takes an acknowledgement (§3.2); the threat table
   states one rule per threat (§5); Tier 1 is the only target (§4).
2. **Model sources are production options** (§3.6): `local` (a model server on this host,
   `PMOS_VLLM_IMAGE` now required, no NVIDIA default), `hosted` (new: the owner's hosted model
   from a compiled, CODEOWNERS-reviewed approved list, the endpoint pinned by a digest of its URL,
   the key in one file mounted only into the gateway, no redirects, the hosted host's DNS answers
   checked, labels only in `/readyz`, `/v1/pmos/roles`, the usage log and reports), and
   `hosted-test` (the separate testing-only path, fake data only). Requirement i is not designed.
3. **Data handling** (§3.9): what leaves the host under `hosted` (every prompt, restricted content
   included), the conditions before real data flows (the owner's recorded approval, the DPA and
   retention terms confirmed by the owner, CODEOWNERS, any TLS-intercepting proxy acknowledged,
   Gate 1), the audit log's new `model_source` per session, and what PM OS does not verify
   (provider behaviour, any confidential-computing attestation, that the URL is the approved
   provider, that a GPU host is the owner's).
4. **D2, D3, O1 and O6 applied** (§3.5, §3.8, §6, §8.1); H3 and H10 are buildable now; **H11 is
   dropped**: with D1 answered by no host-based rule, there is nothing left for it. `PMOS_EXPOSURE`
   now gates only who can reach the sign-in page: the D2 password allow-list, development sign-in,
   and H-PRIVATE's check of a `private` declaration. D3's listener applies on every host.
5. **Slices** (§7): H12 (the hosted production source, list empty) and H13 (the first approved
   entry, waiting on the owner) are new. Order: H0, H0b (built, in review) → H2 → H1, H3, H4 → H9
   → H10, H12 → H7 → H6 with the acceptance run → H5; H13 when the owner's conditions are met.
   Buildable now with no owner decision: H2, H1, H4, H3, H9, H10, H12, H7, H6. Needs the owner: H13.
   Needs a real host: the acceptance run and H5.
6. **The "spark" inventory** (§7.1): 403 occurrences in 74 tracked files. 148 are this spec
   (kept), 23 dated specs and research (kept), 10 `CLAUDE.md` and `CHANGELOG.md` (kept), 2 the
   team's platform decision in content (the owner's), 11 false positives (`sparkle` icons in
   `web/`, three common passwords); the other 209 are renamed, rewritten, regenerated or deleted
   in H3, H7, H12 and H6. No UI text names the Spark. H0 and H0b's Spark wording is listed in
   their status notes.

**What the earlier safeguards became** (the table at the end of §3.6): ADM-3 removed for
`hosted`, replaced by the approved list; "real data only on the Spark" removed, replaced by
protections every host gets; fake-data mode kept for `hosted-test`, for the narrower reason that
nobody approved the endpoint; the `held_real_data` marker narrowed to stopping `hosted-test` on
a database that has held real data, with every change of source now audited; the tool server's
cross-check kept, now also feeding the audit log; the "restricted data sent off the Spark"
metric replaced by "real data sent to an unapproved endpoint" (zero by construction) and tasks
per source; `class: production` loosened by exactly one upstream form, `hosted:<label>`.

**Still true: nothing in this spec has run on a real host with OpenShell.** No host is verified,
none of H0's or H0b's fixes has been seen working on OpenShell, and every statement about
OpenShell's behaviour beyond its cited public docs is marked "verify on a real host".

**What remains open for the owner** (§8.2): the hosted model's approval record, and the owner's
confirmation of the data-processing agreement and retention terms; the CODEOWNERS line and branch
protection for the approved list; which GPU host, if any, for a `local` run; the first VM
itself; the Entra app registration, now on the path to any public host; the team's platform
decision in content, which still names the Spark.

### For the reviewer: what to check before anything is built

The loosened data rule first.

1. **Is the approved list enough to keep real data from an arbitrary endpoint?** It is compiled
   into the gateway, pins the URL by digest, and changes only by a reviewed commit (§3.6). Check
   that no path (an environment variable, a mount, `deploy/.env`, the stub, `PMOS_GATEWAY_CLOUD_TEST`,
   a `class: test` config) lets `hosted` or real data reach an endpoint that isn't on the list, and
   whether the stated limit (anyone in the `docker` group can build another image) is acceptable.
2. **H13 before CODEOWNERS exists.** The spec forbids adding the first entry until the CODEOWNERS
   line and branch protection are in force. Is that the right gate, given that nothing in code can
   check it?
3. **The digest instead of the URL** in the committed list, and its normalisation (case, default
   port, trailing slash; user information, query, fragment and `http` refused). Check edge cases
   the spec doesn't name: an IP-literal host, an internationalised name, an explicit `:443`.
4. **The key's place:** one file, mode `600`, mounted by Compose only into the gateway, never in
   `deploy/.env`, the environment, a log, a report or a command line; kept out of backups. Check
   the Compose `secrets:` mount's permissions against the gateway image's user, and that no other
   container or `pmos-admin` path reads it.
5. **Egress:** no redirects followed; the hosted resolver refusing loopback, link-local, metadata,
   `pmos`-subnet and private answers; a TLS-intercepting proxy treated as part of the data path,
   a condition the owner confirms, not a check. Behind any proxy the gateway can't see the address:
   is that acceptable?
6. **`/readyz` is unauthenticated and now probes the provider with the key.** The spec caches the
   probe for 30 seconds. Check that no other unauthenticated route causes keyed requests, and that
   `/readyz`, `/v1/pmos/roles`, the usage log, errors and eval reports carry labels only.
7. **The `held_real_data` marker's narrower purpose.** A database used only with `local` may now
   be switched to `hosted`, sending data that never left the host to the provider. The spec records
   the switch (`system.model_source`, `status`) but does not refuse it. Should a switch from
   `local` to `hosted` on a database that has held real data need an explicit, logged
   acknowledgement, like H-PRIVATE's?
8. **The audit field** records the source the gateway reported at a session's open, not per
   call; the gateway's usage log has it per call. Is that enough for "which model source served
   each task"?
9. **No data-class ceiling.** Restricted content goes to the hosted provider. That is the owner's
   D1 answer as put; check that §3.9 states it plainly enough and that nothing else in the repo
   (the audience checks, the Slack reply assistant's restricted-draft rules) assumed restricted
   data never leaves the host.
10. **Fake-data mode is now `hosted-test` alone.** A `local` stack with live data on a cloud VM
    with a GPU is accepted; "a GPU host the owner controls" is a statement in the host record,
    which code can't check. Is that acceptable?
11. **D3 (b) on every host**, not only `public` (§3.8): a slight widening of the owner's answer.
    Confirm, or limit it to `public`.
12. **The stricter rules on every host:** H-METADATA and H-EXPOSURE now fail on a `private`
    host too. Check that no legitimate on-prem set-up is refused by them.
13. **`PMOS_HOST_PROFILE`'s removal:** check that nothing in H1, H4 or the runbooks still keys on
    it, and that §10 and §11, which describe it, are read as the record (the header says so).
14. **Benchmarks under `hosted`:** a `production` gateway with an approved `hosted:<label>` may
    count towards a bar (§3.6). Check this against `docs/evals.md`'s "Which runs count" and the
    owner's open question on provider host names in committed reports (reports carry the label).
