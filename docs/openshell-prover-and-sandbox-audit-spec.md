# OpenShell's policy prover in Policy Admin, and sandbox decisions in the audit log

> **Build status (2026-11-24), on `build/prover-and-sandbox-audit`: slices A1, A2, A3, A4, B1,
> B2, B3 and B5 are built, built before review (owner's instruction of 2026-11-24).** None has
> been read by the security reviewer; all of it goes to the reviewer, and the gate (no open High
> or Critical) applies before real data or a pilot. Not built: A5 and B6 (need a real host), A6
> and B4 (need their own review and the owner's answer on GitHub permissions and the canary).
> Nothing here has run against a real OpenShell or a real prover: every statement about either
> is still from the documentation, marked "verify on a real host" (or "with the real pinned
> prover") in the code, and §5 stands as written.
>
> - **A1** `policies/boundary/pmos-bot.yaml` (hand-written), `crates/policy/src/prove.rs`
>   (classifier, network view, pin, comparison with the check, boundary-alone rule),
>   `pmos policy prove` and `pmos policy boundary-alone`, the stand-in
>   `deploy/test/stand-ins/openshell-prover`. The prover runs with an empty environment (only a
>   fixed `PATH`) and is killed at 90 seconds.
> - **A2** CI job `policy-prover` (`scripts/policy-prover-ci.sh`, checked by
>   `scripts/test-policy-prover-ci.sh`). **`deploy/openshell-prover.lock` pins nothing yet, so the
>   job fails with "prover not available" on every run until the owner pins a release** (version,
>   release tag, the SHA-256 of each archive and of each extracted binary). It is never a pass.
>   When the base branch has no boundary (the pull request that adds it) the job proves against
>   the checked-out one and says "BOOTSTRAP"; the boundary-alone rule applies from the next change.
> - **A3** migration `0030_policy_proofs.sql` (number may move on merge), `pmos-toolserver
>   policy-proof record`, `pmos-admin policy prove <bot>|--all`. Deviation: `sandbox` and
>   `resume` print the next step (`policy prove`) instead of running it, because the kit's
>   existing tests check their exact OpenShell calls; running it there is left for review.
> - **A4** Policy Admin's Prover row (`GET /api/admin/policy` reads `GET /internal/admin/policy/
>   proofs`), "not run here" on the propose and proposals parts. A proof is current only when its
>   `candidate_sha256` (the SHA-256 of the policy's canonical form, so formatting and key order
>   don't matter) is the checked-in file's and it is not older than the sandbox's last
>   `sandbox.policy_loaded`.
> - **B1** migration `0031_sandbox_audit.sql` (number may move on merge; adds `policy_hash` and
>   `os_correlation_uid` beyond §B.3's list), client `sandbox_audit`, `POST /internal/sandbox-
>   audit/{batch,heartbeat}`, attribution, gap, down and silence detection, `denials_since` and
>   `sandbox_gaps_since`.
> - **B2** crate `pmos-sandbox-audit` (OTLP/HTTP protobuf decoded by hand, uncompressed only).
> - **B3** compose service `sandbox-audit` (profile of that name, off unless asked for),
>   `pmos-admin sandbox-audit configure|check|up`, 8780 in H-PORTS and H-EXPOSURE. The tokens are
>   made by `configure`, not by `setup.sh`, so nothing turns on by itself (env.example lists
>   them blank).
> - **B5** the overview's "Sandbox denials, last 24 hours" and collector state, the audit list's
>   sandbox columns and action suggestions, each bot's latest denials on Policy Admin, and the
>   Auditor's destination class (`GET /internal/admin/sandbox`).
>
> Defaults taken for §6's open questions (the owner's to change): 1 and 2, nothing set (GitHub
> settings and CODEOWNERS are the owner's); 3, an Auditor sees the destination class only; 4,
> 5-minute allow counts; 5, no canary (B4 not built); 6, no Checks permission (A6 not built);
> 7, nobody is notified, the overview only; 8, `policy_proofs` is append-only and not purged.

Status: **design spec; not built.** Scope item c (`CLAUDE.md`): "add OpenShell's policy prover to
Policy Admin, and bring OpenShell's sandbox allow and deny decisions into the audit log" (owner,
2026-11-18, plan only). Owner, 2026-11-24: go ahead and build to the PRD; this spec is the first
step and goes to the independent security reviewer before any code. Written against
`claude/web-app` at `73f12ff`. Nothing in this repository has run on a real OpenShell host yet:
every statement about OpenShell's behaviour below is from its documentation, and §5 lists each
one that must be verified.

Part A puts the prover beside `pmos policy check`. Part B brings OpenShell's own record of each
sandbox's network decisions into the tool server's audit log. Both add trust in something PM OS
does not control, so each part says exactly what it proves or records, and what it does not.

## 1. What OpenShell's documentation says

All pages read on **2026-11-24** from `docs.nvidia.com`, version "Latest (v0.1.9)". Each page
was fetched as Markdown (append `.md` to the URL). Nothing else was needed; no page failed.

| Page (title) | URL |
| --- | --- |
| Policy Prover | https://docs.nvidia.com/openshell/how-it-works/policies/prover |
| Support Matrix (Standalone Policy Prover, Releases) | https://docs.nvidia.com/openshell/about/support-matrix |
| Manage Sandbox Policies | https://docs.nvidia.com/openshell/how-it-works/policies/manage-policies |
| Sandbox Logging | https://docs.nvidia.com/openshell/observability/logging |
| Accessing Logs | https://docs.nvidia.com/openshell/observability/accessing-logs |
| OCSF JSON Export | https://docs.nvidia.com/openshell/observability/ocsf-json-export |
| Observability Overview | https://docs.nvidia.com/openshell/observability/overview |
| Gateway Configuration File (OTLP Export, OCSF Log Export) | https://docs.nvidia.com/openshell/how-it-works/gateways/configuration |

The versioned prover pages at `/openshell/v0.1.0/…` to `/v0.1.8/…` also exist; the
`v0.0.116` docs have no prover page.

### 1.1 The policy prover

- **What it is.** "OpenShell's policy verification engine. It uses an SMT solver to check whether
  policies satisfy specified properties." Two checks: a **boundary check** ("a policy allows no
  access beyond a boundary policy") run with the `openshell-prover` CLI, and a **proposal risk
  check** that runs automatically inside the policy advisor. PM OS keeps the advisor off (RT-2),
  so only the boundary check is used here. "Passing one does not imply passing the other."
- **How it is invoked.** `openshell-prover check candidate.yaml --boundary boundary.yaml`, with
  `--output json` for scripts ("Read the `result` and `reason_code` fields instead of parsing the
  text output") and `--timeout` (default 10 seconds). "The CLI reads policy files on your
  machine, does not need a gateway, and does not apply or approve policies." For a running
  sandbox: `openshell sandbox get my-sandbox --policy-only > candidate.yaml`; "the effective
  policy includes rules from attached providers … The prover does not add provider rules itself."
- **Outputs and exit codes.** `within_boundary` 0; `exceeds_boundary` 1 (with one
  `counterexample`, such as `filesystem write /tmp`); `error` 2; `unsupported` 3; `inconclusive`
  3. "Only `within_boundary` means that the check passed. Treat every other result as a
  failure." Text output has a `coverage: domains=filesystem,network_l4,network_rest,process,landlock`
  line; scripts should "check that `coverage.domains` includes every part of the policy that you
  rely on". `reason_code` examples: `unsupported_policy_shape`, `solver_timeout`,
  `resource_limit`.
- **What it proves, by field.** Filesystem read and write "path by path, so both policies must
  list the same paths"; "a boundary entry for `/` covers every path"; different paths, even one
  inside the other, give `unsupported`. Process: `run_as_user`/`run_as_group` equal passes, to
  root exceeds, any other change `unsupported`. Landlock: the `compatibility` setting
  (`hard_requirement` to `best_effort` exceeds). Network: "which binaries can reach which hosts,
  ports, and destination addresses, including `allowed_ips` ranges", with and without binary
  enforcement; "It does not resolve hostnames." REST: method and path rules on `protocol: rest`
  endpoints with `enforcement: enforce`.
- **Limits.** "If a policy uses anything else, such as GraphQL or MCP rules, the prover reports
  that it cannot check the policy instead of ignoring those rules." A pass "does not mean that
  the policy is as narrow as it could be, that it is safe for a particular task, or that a
  running sandbox enforces it." More than 1,024 network rules or 4,096 endpoints give
  `inconclusive`. Credential bindings, MCP methods and tools, and uploads are not in its model.
- **Distribution and version.** Packages (Homebrew, Debian, RPM, snap) include it; standalone
  archives `openshell-prover-x86_64-unknown-linux-musl.tar.gz` and `…-aarch64-…` with
  `openshell-prover-checksums-sha256.txt` on the GitHub releases. **The version it first
  appeared in is not documented**; its page is in the v0.1.0 docs and not in v0.0.116. Its
  stability tier (Stable or Experimental, per RFC 0014) is **not documented**.

### 1.2 How OpenShell records a sandbox's decisions

- **Events.** Network (`NET:`, OCSF class 4001) and HTTP (`HTTP:`, 4002) decisions are OCSF
  v1.8.0 events with an action `ALLOWED`, `DENIED` or `BLOCKED`. Shorthand:
  `OCSF NET:OPEN [MED] DENIED /usr/bin/curl(64) -> httpbin.org:443 [policy:- engine:opa] [reason:no matching policy]`.
  `policy:-` is no matching rule; an L7 denial names the rule. Reasons include `no matching
  policy`, `resolves to always-blocked address`, `resolves to <ip> which is not in allowed_ips,
  connection rejected`, `l7 deny`. Findings (`FINDING:`, 2004) and gateway authorization denials
  of a sandbox principal (`API:`, 6003, reason `cross_sandbox`) are also recorded.
- **JSON fields** (OCSF JSON Export): `time`, `action` (`Denied` on every refusal),
  `disposition`, `status_detail`, `dst_endpoint {domain, ip, port}`, `actor.process {name, pid,
  cmd_line}`, `firewall_rule {name, type}`, `metadata.uid` ("uniquely identifies an event"),
  `metadata.product.version`, `metadata.correlation_uid` (per inspected HTTP request), and the
  sandbox as `device.container.uid` and `device.container.name`. `message` is free text.
- **How it is read.** (a) `openshell logs <sandbox> --source sandbox`: the gateway keeps "up to
  2,000 recent log lines per sandbox in memory", lost on restart, and "the push channel is
  bounded and can drop events"; a resume cursor exists, but **its CLI flag is not documented**.
  (b) Supervisor files `/var/log/openshell-ocsf.YYYY-MM-DD.jsonl` (with `ocsf_json_enabled`) on a
  64 MiB tmpfs that "`docker cp` cannot copy", three files kept. (c) `docker logs --follow
  <supervisor-container>` for `OCSF-JSON` stderr lines, a 1,024-line queue that drops. (d) **OTLP
  log export**: `[openshell.gateway.otlp.ocsf] enabled = true` with its own `endpoint`; "sandbox
  supervisors … export their own OCSF events there directly"; headers (`headers_file`) are
  delivered "with each sandbox's launch credentials, never through the sandbox environment";
  attributes include `ocsf.metadata.uid` and `openshell.sandbox.id`; delivery "is at least once.
  Deduplicate on `ocsf.metadata.uid`"; dropped records are reported by a WARN record
  `openshell.ocsf.gap` with `openshell.ocsf.gap.dropped` and `openshell.ocsf.gap.since_unix_ms`;
  "Records lost during shutdown produce no gap record."
- **Retention.** None durable: "Neither the local files nor the gateway stream guarantees
  complete audit history." Collection is "best-effort".
- **Filesystem decisions.** Landlock emits `CONFIG:` events at start-up only, into the workload
  container's log. **A per-access record of a refused file write is not documented.**

## 2. Part A: the prover in Policy Admin

### A.1 Where it runs

The prover needs no gateway and no sandbox: it reads two YAML files. So it runs in two places,
never inside a sandbox, never with a bot's credentials, and never in `pmos-web` or the tool
server (neither shells out; `pmos-web` holds no GitHub token, `docs/policy-admin.md`).

1. **CI, on every pull request** (the gate). A new job `policy-prover` in `.github/workflows/ci.yml`
   downloads the pinned archive, checks its SHA-256 against the value committed in
   `deploy/openshell-prover.lock` (not only against the release's own checksum file), and runs
   `pmos policy prove` (A.3) over every `policies/bots/*.yaml`. The job has no secrets and no
   write token.
2. **The host, after a policy is applied** (what is enforced). `pmos-admin policy prove [<bot> |
   --all]`, run as the OpenShell gateway's user (the same user `pmos-admin sandbox` requires),
   refuses root. It saves each sandbox's effective policy with `openshell sandbox get pmos-<bot>
   --policy-only` into a mode-600 temporary file, runs the pinned prover on it, and records the
   result (A.5). `pmos-admin sandbox`, `resume` and the documented step after `openshell policy
   set` run it too. It holds no provider token: the CLI's operator credentials are the user's own.

### A.2 What it is asked to prove, for every bot

One boundary for every bot, **written by hand**, never generated: `policies/boundary/pmos-bot.yaml`.
It restates the claim of `crates/policy/src/lib.rs` independently, so a change to the generator's
constants that the boundary does not also carry fails the prover. Its properties, in PM OS's terms:

| Id | Property | Boundary content | Prover domain |
| --- | --- | --- | --- |
| P-NET | Only `/usr/local/bin/node` connects, and only to `toolserver.pmos.internal:8710`, `gateway.pmos.internal:8740`, `bots.pmos.internal:8750`, each with `allowed_ips` exactly its own `/32` (`.10`, `.11`, `.12`); nothing else | three endpoints, one binary | `network_l4` |
| P-REST | The gateway allows only `POST /v1/chat/completions`; the runtime only `POST /v1/worker/claim` and `POST /v1/worker/tasks/*/{heartbeat,events,trace,complete,fail}`; both `enforce` | the same rules | `network_rest` |
| P-WRITE | No write outside `/sandbox/workspace`, `/tmp`, `/dev/null`; `include_workdir: false` | `read_write` those three; `read_only: [/]` | `filesystem` |
| P-PROC | The workload runs as `sandbox:sandbox`, never root | `run_as_user`/`group: sandbox` | `process` |
| P-LL | Landlock is `hard_requirement` | the same | `landlock` |

**Not proved, and said so on screen:** which paths are readable (the boundary allows reading `/`,
because the prover compares paths exactly and each bot reads different skill folders; `fs-readable`
covers it), the MCP methods and tool allowlist (`net-tools`), credential bindings
(`cred-binding`, `cred-providers`), uploads and the launch spec (`fs-uploads`, `spec-*`),
and anything about name resolution or what a running sandbox enforces.

**The MCP endpoint.** The tool-server endpoint is `protocol: mcp`, which the prover reports as
`unsupported`. So `pmos policy prove` builds a **network view** of each candidate: a copy in which
that one endpoint loses `protocol`, `mcp`, `rules`, `path` and `enforcement`, keeping `host`,
`port`, `allowed_ips` and the binaries. Removing request rules only widens what an endpoint
allows, so if the view is within the boundary the real policy is too. The view is code, reviewed
under POL-2 (`crates/policy/`): it may only delete those five keys from an endpoint whose
`protocol` is `mcp`, and a unit test fails if it changes or adds anything else. The screen says
"proved on the network view (MCP rules removed, which only widens)". When a pinned prover lists an
`mcp` domain in `coverage.domains`, the view is dropped in its own slice.

### A.3 `pmos policy prove`

`pmos policy prove [--policies policies] [--boundary policies/boundary/pmos-bot.yaml]
[--prover <path>] [--candidate <file>] [--format text|json]` (crate `pmos-policy`, new module
`prove.rs`). For each candidate: build the view, run `<prover> check <view> --boundary <b>
--output json --timeout 60`, kill it at 90 seconds, and classify:

| PM OS status | When |
| --- | --- |
| `proved` | exit 0 **and** `result: within_boundary` **and** `coverage.domains` holds all five domains |
| `exceeds` | exit 1 and `result: exceeds_boundary`; the counterexample is shown (it names a path, host, port or route from the policy, never data) |
| `unsupported`, `inconclusive` | exit 3 with that result; the `reason_code` and reason are shown |
| `error` | exit 2, any other exit, a timeout, output that isn't the documented JSON, an exit code and result that disagree, or a missing domain |
| `not_available` | no prover binary, or its SHA-256 isn't the pinned one |
| `version_mismatch` | the binary's version is not the pinned version (older or newer) |

Only `proved` passes. Exit status: 0 when every bot is `proved`, 1 otherwise, 2 when it could not
run. Output is deterministic (bots sorted) and carries the prover version and SHA-256, and the
SHA-256 of the boundary, of each candidate file and of each view.

### A.4 Relationship to `pmos policy check`

The prover does not replace the check: both must pass in CI, and Policy Admin shows both. Each
domain maps to the rules that claim the same thing:

| Domain | `pmos policy check` rules |
| --- | --- |
| `network_l4` | `net-egress`, `net-allowed-ips`, `net-binaries` |
| `network_rest` | `net-gateway-routes`, `net-runtime-routes`, `net-inspection` (gateway, runtime) |
| `filesystem` (writes) | `fs-writable`, `fs-workdir` |
| `process` | `proc-identity` |
| `landlock` | `fs-landlock` |

**A disagreement is a finding.** If the check passes every mapped rule while the prover returns
`exceeds` in that domain, PM OS's rules have a hole: the CI job fails with `disagreement`, names
the rule and the counterexample, and the conductor treats it as a High finding until the
reviewer has triaged it. If a mapped rule fails while the prover returns `proved`, the check is
stricter than the boundary (for example `net-allowed-ips` refusing a bare address): the job
still fails on the check, and the report lists it as `check-stricter` for review, not as an
error.

### A.5 Recording and showing the result

**CI** results live in the pull request's checks. **Host** results are recorded by
`pmos-admin policy prove` through a one-off tool server container, as `pmos-admin users` does
(`docs/user-database-spec.md` §5): `pmos-toolserver policy-proof record` with
`PMOS_HOST_USER`. It writes one row per bot to a new append-only table `policy_proofs` (`at`,
`bot`, `status`, `reason_code`, `coverage`, `counterexample` (at most 256 characters),
`prover_version`, `prover_sha256`, `boundary_sha256`, `candidate_sha256`, `host_user`) and an
audit event `policy.prove` (client `host`, outcome `ok` for `proved`, `denied` for `exceeds`,
`error` otherwise). The table's triggers refuse UPDATE, DELETE and TRUNCATE. The audit event is
purged at 90 days with the rest; the table keeps the newest row per bot and is not purged in v1
(an open question, §6).

**Policy Admin** (`GET /api/admin/policy`, `web/src/pages/admin/Policy.tsx`) gains, per bot, a
**Prover** row beside **Rules**:

- `Proved (5 of 5 domains)` with the prover version and the time, plus the fixed "not proved" list
  of A.2, always shown;
- `Exceeds boundary: <counterexample>` in the danger style;
- `Not checked: unsupported (<reason>)`, `Not checked: inconclusive`, `Prover error`;
- `Prover not available on the host`, `Prover version too old (or different): <found>, pinned
  <pinned>`;
- `Stale`: the last proof's `candidate_sha256` is not the checked-in policy's hash, or it is older
  than the sandbox's last policy load. A stale or missing proof is never shown as proved.

The tool server serves the newest row per bot on `GET /internal/admin/policy/proofs?user=`
(`view-policies`: admins and auditors; it holds no data beyond policy terms). `pmos-web` runs no
prover. On **validate** and **proposals**, the screen says "The prover runs on the pull request
(CI check `policy-prover`). Not run here." and links the pull request; reading the check's
result into the proposals list needs the GitHub App's **Checks: read** permission (slice A6,
an owner question).

### A.6 How it gates a proposal

**Recommendation: it gates the merge, not the opening.** A proposal cannot be proved before the
pull request exists, and opening one writes nothing to the default branch. So:

- `policy-prover` is a required status check on `PMOS_CONTENT_BRANCH` (the owner sets it, beside
  the content lint and `pmos policy check`, `docs/policy-admin.md`). A failed, missing,
  unsupported or timed-out proof blocks the merge.
- **Boundary changes.** CI proves the head's policies against the **base branch's** boundary,
  so a pull request cannot widen a policy and its boundary together. A pull request that changes
  `policies/boundary/` may change no other file (the job fails otherwise), needs POL-2 review,
  and has its own CODEOWNERS line (§6).
- **Stays a human decision:** whether a widening inside the boundary is wanted (POL-2), any
  boundary change, what a counterexample or a disagreement means, and applying a merged policy on
  the host (`openshell policy set`). The prover never approves anything.

### A.7 Version pinning

`deploy/openshell-prover.lock` holds the version and the SHA-256 of each Linux archive. CI and
`pmos-admin` use only that version. A bump is its own pull request under CODEOWNERS; on it CI runs
the old and the new prover and fails if any bot's status differs, so a change in OpenShell's model
is reviewed, not absorbed. A newer prover on the host gives `version_mismatch` until the pin
moves; an older one gives `version_mismatch` with "too old". If a release changes the JSON fields,
the parser returns `error`, never `proved`.

### A.8 Tests

- **Stand-in prover** (`deploy/test/stand-ins/openshell-prover`, as the kit's other stand-ins):
  canned answers chosen by the candidate's hash: within; exceeds with a counterexample; unsupported;
  inconclusive; exit 2; exit 0 with `exceeds_boundary`; exit 1 with `within_boundary`; a missing
  domain; non-JSON output; a hang (killed); a wrong version; a wrong binary hash. Only the first
  gives `proved`.
- **The view:** property tests over generated policies: the view differs from the candidate only
  by the five removed keys on the MCP endpoint; a policy whose `pmos_toolserver` endpoint gains a
  second host, port or address keeps it in the view.
- **Disagreement:** a hand-edited policy that passes the check but which the stand-in says exceeds
  yields `disagreement`.
- **Boundary rule:** a fixture pull request that edits the boundary and a policy fails.
- **Host command:** `scripts/test-pmos-admin.sh` with stand-ins for `openshell` and the prover:
  refuses root; records one row per bot; a missing prover records `not_available`; a stale row
  shows `Stale` in the web tests.
- **With the real pinned prover, in CI (slice A2):** every checked-in policy's view is `proved`;
  the unmodified policy is `unsupported` (MCP); planted widenings (a fourth host, a `/24`, a
  writable `/sandbox`, `best_effort`, root) are each `exceeds`.

## 3. Part B: sandbox decisions in the audit log

### B.1 Source: OpenShell's OTLP log export

Of the four ways OpenShell offers (§1.2), only OTLP export is pushed by OpenShell itself from
outside the sandbox, carries the full record, is at least once with a dedup key, and reports its
own loss (`openshell.ocsf.gap`). `openshell logs` is an in-memory buffer of 2,000 lines that drops;
the supervisor's files sit on a tmpfs PM OS can't read; `docker logs` needs Docker access, which is
root on the host. **Recommendation: OTLP export to a PM OS collector on loopback.** On the host,
in OpenShell's `gateway.toml` (`pmos-admin sandbox-audit configure` prints it and `check`
verifies it):

```toml
[openshell.gateway.otlp.ocsf]
enabled      = true
endpoint     = "http://127.0.0.1:8780"
protocol     = "http/protobuf"
headers_file = "<state dir>/secrets/otlp-ocsf.headers"   # authorization=Bearer <token>
```

No `[openshell.gateway.otlp]` endpoint is set, so no traces or agent traces are exported. The
workload cannot reach `127.0.0.1` (OpenShell never allows loopback) and never holds the header.

### B.2 The collector

- **Process.** `pmos-sandbox-audit`, a new crate and a compose service `sandbox-audit`, running as
  its own non-root uid, published only on `127.0.0.1:8780`, on no sandbox policy. It serves only
  `POST /v1/logs` (OTLP/HTTP protobuf). It mounts no Docker socket and no OpenShell configuration,
  and runs no `openshell` command: it is a receiver, not a reader with rights over OpenShell.
- **Inputs it trusts:** a request with the header token (compared in constant time), from
  OpenShell's gateway or supervisors. Everything inside a record (hosts, paths, process names)
  was chosen by the workload's behaviour and is treated as untrusted text.
- **Fields it keeps: an allowlist, never a denylist.** `time`, `metadata.uid`,
  `metadata.product.version`, `metadata.correlation_uid`, `class_uid`, `action`, `disposition`,
  `firewall_rule.name`, `firewall_rule.type`, `status_detail` mapped to a fixed reason set (else
  `other`), `dst_endpoint.domain`, `.ip`, `.port`, the HTTP method, the URL **path** only (no
  scheme credentials, query or fragment; at most 256 characters), `actor.process.name` (at most 128),
  `device.container.name`, `device.container.uid` and `openshell.sandbox.id`. Dropped always:
  `cmd_line`, `message`, headers, bodies, `pid`, `src_endpoint`, and everything not listed. Kept
  strings are printable ASCII (others become `?`).
- **What it forwards:** every `Denied` NET or HTTP record; every `FINDING` and every sandbox
  `cross_sandbox` API denial; every `CONFIG:LOADED` (policy reload, with `policy_hash`); each gap
  record; and allow counts (B.4).
- **To the tool server:** `POST /internal/sandbox-audit/batch` on `127.0.0.1:8710` with its own
  token `PMOS_SANDBOX_AUDIT_TOKEN`, a new client `sandbox_audit` that may call **only** that route
  and `POST /internal/sandbox-audit/heartbeat`. Body: `{collector_run, events: [{seq, …}]}`, at most
  500 events and 256 KiB. `collector_run` is a random id per start; `seq` counts from 1 per run.
- **Spool.** Before acknowledging OpenShell it appends the batch to a bounded spool (64 MiB,
  fsync), and replays it until the tool server answers `200`. When the spool is full it answers
  OpenShell `503`; OpenShell then retries, drops and reports the gap.

### B.3 The event in PM OS's audit log

Rows of `audit_events` (so the same 90-day purge covers them, `docs/audit-retention.md`), with new
nullable columns in one migration: `sandbox` (the name), `sandbox_uid`, `decision`
(`allow`/`deny`/`finding`), `rule`, `reason`, `dest_host`, `dest_ip`, `dest_port`, `http_method`,
`http_path`, `process`, `allow_count`, `os_event_uid`, `os_version`, `collector_run`,
`collector_seq`; and a widened `client` check for `sandbox_audit`.

| Field | Value |
| --- | --- |
| `tool` (action) | `sandbox.deny`, `sandbox.deny_summary`, `sandbox.allow_summary`, `sandbox.finding`, `sandbox.policy_loaded`, `sandbox.source_gap`, `sandbox.collector_gap`, `sandbox.collector_down`, `sandbox.unparsed` |
| `outcome` | `denied` for a deny or finding, `ok` for an allow summary or policy load, `error` for a gap, a down period or unparsed records |
| `bot` | from the sandbox name `pmos-<bot>` (checked with `valid_name`, and against the sandbox id `pmos-admin sandbox` recorded at create); otherwise `null` and `sandbox` kept, flagged `unknown sandbox` |
| `user` | the PM the task was for, when derivable (below); else the fixed actor `pmos-sandbox-audit` |
| `run_id` | the task id when derivable; else `sandbox` |
| `detail` | a fixed note only (`attributed`, `ambiguous`, `no task`, `unknown sandbox`) |

**Which PM, and how.** The tool server (not the collector, which knows no users) derives it at
ingest from its own bot sessions (`docs/toolserver.md`, **Bot sessions**): a personal bot's PM is
its owner, always. For a team bot, if exactly one session of that bot was open at the event's
time, its `run_id` (`tsk_…`) and `user` are used; with none or several, `user` stays the fixed
actor and the note says `no task` or `ambiguous`. A deny with `no task` is itself notable: the
sandbox acted with no task running.

**Links.** `run_id` joins the feedback ledger (`run_id`), the gateway's usage log (`run`), the
tool server's own audit events and the runtime's trace for the task. `os_event_uid` and
`metadata.correlation_uid` join OpenShell's own records while they exist.

### B.4 Volume

Allows can be one per heartbeat, model call and tool call. They are all already recorded by the
service that received them (the tool server's audit, the gateway's usage log, the runtime), so a
row per allow adds volume, not information. **Recommendation:**

- **every deny is recorded**, one row each, up to 100 per sandbox per minute; past that the
  collector holds them and writes one `sandbox.deny_summary` with the count and the distinct rules,
  so a looping sandbox can't flood the log and no deny goes uncounted (the gateway's refusal
  summary works the same way, `docs/gateway.md`);
- **allows are counted**: one `sandbox.allow_summary` per sandbox, rule, destination host and port,
  and method, per 5-minute window, with `allow_count`, and no path. An allow summary with no open
  session is flagged `no task`.

The tool server deduplicates denies on `os_event_uid` (a partial unique index); allow counts may
count a redelivered record twice, which the screen states.

### B.5 How a deny is surfaced

- **Admin's view:** the overview gains "Sandbox denials, last 24 hours" per bot and the collector's
  state (`receiving`, `silent since <t>`, `gap at <t>`); the audit list filters on the new
  actions; Policy Admin's bot page lists its latest denials (rule, destination, time).
- **Tripwire:** the tool server exposes `denials_since(bot, t)` and `sandbox_gaps_since(t)` to its
  own code. The phase 2 halt (IL2-16, `docs/improvement-loop-phase2-requirements.md`) reads them:
  any `sandbox.deny` or `sandbox.finding` for the improver's sandbox halts it, and so does any
  gap or down period covering its run, because no record of a denial is not proof of none.
  Notifying a person (Slack or email) is not designed here (§6).

### B.6 Integrity

- The collector's token can only append; the database's append-only triggers already refuse
  UPDATE, DELETE and TRUNCATE for every role but the purge function (`docs/audit-retention.md`).
- The source is OpenShell's record made outside the sandbox. The workload can't reach the
  collector or hold its header; records that reach the tool server any other way are refused.
- **Gaps are detectable.** The tool server checks each `collector_run`'s `seq` runs on with no hole;
  a hole records `sandbox.collector_gap` (from, to, missing count). A new `collector_run` records
  `sandbox.collector_down` from the last heartbeat (every 60 seconds) of the previous run to the
  new run's first. No heartbeat for 5 minutes records `collector_down` with an open end, closed when
  it returns. OpenShell's own gap record becomes `sandbox.source_gap` (dropped count, since).
- **Loss OpenShell doesn't report** (shutdown): a liveness canary (slice B4), a minimal sandbox
  `pmos-audit-canary`, attempts one denied connection to `audit-canary.invalid` every 15 minutes
  from a systemd timer; if its deny is not received within 2 minutes the tool server records
  `sandbox.source_gap` with note `canary missing`.

### B.7 Retention, Auditor view and privacy

- **Retention:** these are `audit_events` rows; the one purge job deletes them at 90 days; nothing
  else deletes them. The JSON-lines copy (`PMOS_AUDIT_LOG`) gets them as it gets every event.
- **Privacy:** a destination or path can say what a bot tried to reach (a customer's host, an
  internal id). **Recommendation:** an Admin sees host, address, port, method and path; an
  Auditor sees the action, bot, time, outcome, rule, reason, port and a destination class
  (`pmos service`, `private`, `public`, `always blocked`), not the host, address or path; a PM
  sees none of it. Served through `GET /internal/admin/audit` with the existing role checks.

### B.8 Failure modes: visible, never silent

| Failure | What happens |
| --- | --- |
| Collector down | OpenShell queues up to 10,000 records, retries, then drops and reports a gap; PM OS records `collector_down` and `source_gap`; the overview shows it |
| Tool server down | the collector spools and replays; full spool: `503` to OpenShell, gap |
| Export switched off in `gateway.toml` | no records, no heartbeat from the canary: `canary missing` within 17 minutes; `pmos-admin sandbox-audit check` and `status` fail |
| OpenShell upgrade changes the format | a record without the expected fields is counted, and `sandbox.unparsed` records the count and `os_version`; a new `metadata.product.version` records it once and shows "format unverified" until the OpenShell version recorded in `deploy/openshell-prover.lock` (the version the format was last verified on) moves |
| OpenShell gateway restart | in-flight records may be lost with no gap record (documented); the canary bounds the unseen gap |
| Log rotation | not used: OTLP is pushed, nothing reads OpenShell's files |

### B.9 Tests

- **Stand-in stream:** OTLP/HTTP payloads built in tests from OCSF fixtures (the documented
  examples, plus PM OS's own denials from `policies/redteam/isolation.sh`'s comments): each kept
  field mapped; `cmd_line`, query strings, `message` and a fake secret in a path's query never
  reach the stored row or the JSON-lines copy; `<script>`, newlines, ANSI codes, 10 KB hosts and
  non-ASCII are cleaned; a wrong or missing header is `401`; duplicates by `metadata.uid` stored
  once; 1,000 denies in a minute give 100 rows and one summary with the rest counted; allows
  summed per window.
- **Tool server (Postgres):** the new client may call only its two routes (the per-token route
  matrix, as `tests/approvals_assertion.rs`); `seq` holes, a new run and a silent collector each
  recorded; attribution for a personal bot, one open session, none and two; an UPDATE or DELETE
  of a sandbox row refused; the purge deletes them at 90 days and not before.
- **Kit:** `scripts/test-pmos-admin.sh`: `sandbox-audit configure` prints the table, `check`
  fails on each missing line; compose shows `127.0.0.1:8780` only and no socket mounted; the
  preflight's H-EXPOSURE counts 8780.

## 4. Threats this adds

| Threat | Control | Test |
| --- | --- | --- |
| A new privileged reader on the host | The collector is a push receiver: no Docker socket, no OpenShell CLI or config, own uid, loopback only, two narrow tokens | compose config check; route matrix |
| The sandbox forges decision records | Loopback is never reachable from a sandbox; the header token is delivered to supervisors, never the workload; only OTLP from OpenShell is accepted | RT-22 below: from a sandbox, POST to `127.0.0.1:8780` and to the host's address is blocked; the token is not in the sandbox's environment or files |
| Another local process forges records | Header token in a mode-640 file; constant-time compare; `collector_run` on every row | `401` tests |
| The collector's token is stolen | It can only append through the tool server's validation; it can't read, rewrite or delete; rotation in `docs/runbooks/incidents.md` | route matrix; trigger tests |
| A flood of denies fills the log | 100 rows a sandbox a minute, then a counted summary; batch caps | the 1,000-deny test |
| Injection through workload-chosen strings | Allowlisted fields, cleaned, length-capped, rendered as text | the cleaning tests |
| A secret or payload enters the audit log | `cmd_line`, `message`, query, headers and bodies dropped by allowlist | the fixture test |
| Silent loss | seq, heartbeat, OpenShell's gap record, the canary | B.9 |
| People over-trust a prover pass | The screen lists what is not proved; only `proved` passes; never "passed" when it did not run; stale proofs marked | web tests |
| A widening slips past via the boundary | Base-branch boundary; a boundary change comes alone, under CODEOWNERS and POL-2 | the boundary fixture |
| The network view hides a widening | It may only delete five keys of the MCP endpoint; `net-tools` still required | property tests |
| A tampered or drifted prover binary | SHA-256 pinned in the repository; exact version required; a bump compares results | stand-in hash and version tests |
| The prover runs where it shouldn't | Only in CI and as the host's gateway user; refuses root; never `sandbox exec`; no provider tokens | `test-pmos-admin.sh` |

## 5. What must be verified

**With the real pinned prover, in CI (no OpenShell host needed):** the JSON field names
(`result`, `reason_code`, `coverage.domains`, the counterexample's key); a version flag; that
PM OS's policy files parse with `credential_binding` and `mcp` present; that the unmodified
policy is `unsupported`; that the view is `within_boundary`; that `read_only: [/]` covers the
skill paths; that `include_workdir: false` does not make the result depend on the working
directory; that the runtime's `*` route globs compare; that 60 seconds is enough; that
`credential_binding` is ignored rather than refused; the prover's stability tier.

**On a real host with OpenShell:** that `openshell sandbox get <name> --policy-only` prints a
policy the prover reads, and that it equals the base policy for PM OS's endpointless providers;
that `[openshell.gateway.otlp.ocsf]` with its own endpoint exports without trace export, over
`http/protobuf`, with `headers_file`; that supervisors under the Docker driver reach
`127.0.0.1:8780` (host network mode is the documented default); that records carry
`openshell.sandbox.id` and `device.container.name` = `pmos-<bot>`; how PM OS's MCP and REST denials
look as OCSF (class, rule name, path); how many allow records a task produces; whether OTLP
export needs `ocsf_json_enabled`; the gap record's exact form; how to tell that OpenShell's gateway
restarted; that the canary's deny to `audit-canary.invalid` is recorded; that a refused file write
leaves no per-access record (§1.2); that `openshell sandbox get` shows the sandbox's id; and
the `openshell logs` resume flag (not used by this design). New red-team rows in
`policies/redteam/checklist.md`: **RT-22** (above), **RT-23** (a denied request from bot A appears
in the audit log within 60 seconds with A's bot, rule and task), **RT-24** (stop the collector for
5 minutes: `collector_down` with both ends), **RT-25** (`pmos-admin policy prove --all` is `proved`
for every sandbox, and a planted widening on a test sandbox is `exceeds`).

## 6. Slices, requirements, non-goals, open questions

**Slices, in build order.** Every slice waits for the reviewer's verdict on this spec. Those
marked *stand-ins* can then be built and tested here; those marked *real* need the CI prover or a
real host; those marked *review* need the reviewer to read their own text first.

| Slice | What | Kind |
| --- | --- | --- |
| A1 | Boundary file, network view, `pmos policy prove`, the classifier, stand-in prover | stand-ins |
| A2 | CI job with the pinned prover; the disagreement report; the boundary-alone rule | real (CI) |
| A3 | `policy_proofs` table, `pmos-toolserver policy-proof record`, `pmos-admin policy prove` | stand-ins |
| A4 | Policy Admin's Prover row, the proofs read route, the "not run here" text | stand-ins |
| A5 | Host acceptance of A3 (RT-25) | real (host) |
| A6 | Proposals list reads the `policy-prover` check (needs Checks: read) | review, owner |
| B1 | Migration, `sandbox_audit` client, batch and heartbeat routes, attribution, gap detection | stand-ins |
| B2 | `pmos-sandbox-audit` collector: OTLP, allowlist, cleaning, limits, summaries, spool | stand-ins |
| B3 | Kit: compose service, tokens in `setup.sh`, `sandbox-audit configure|check`, H-EXPOSURE | stand-ins |
| B4 | Liveness canary sandbox and timer | review (a new standing sandbox) |
| B5 | Admin overview, audit filters, Policy Admin denials, the Auditor's destination class | stand-ins |
| B6 | Host acceptance: RT-22 to RT-24, §5's list filled in | real (host) |

**Requirements.**

| Id | Requirement | Acceptance check |
| --- | --- | --- |
| PRV-1 | The prover runs only in CI and on the host as the gateway user; never in a sandbox, `pmos-web` or the tool server | code search: no prover call in `crates/web`, `crates/toolserver`; `test-pmos-admin.sh` root refusal |
| PRV-2 | A hand-written boundary states P-NET, P-REST, P-WRITE, P-PROC, P-LL | A2: every bot `proved`; planted widenings `exceeds` |
| PRV-3 | Only exit 0 with `within_boundary` and all five domains is `proved` | stand-in matrix |
| PRV-4 | The network view only deletes five keys of the MCP endpoint | property tests |
| PRV-5 | CI proves against the base branch's boundary; a boundary change comes alone | boundary fixture |
| PRV-6 | A disagreement on a mapped rule fails CI as `disagreement` | disagreement fixture |
| PRV-7 | The prover is pinned by version and SHA-256; any other binary is `not_available` or `version_mismatch` | stand-in hash and version tests |
| PRV-8 | A pin bump fails if any bot's status changes | CI on a bump fixture |
| PRV-9 | Host proofs are recorded append-only with `host_user` and an audit event | Postgres tests |
| PRV-10 | Policy Admin never shows "proved" for a stale, missing, failed or unrun proof, and always shows what is not proved | web tests |
| PRV-11 | `policy-prover` is a required check on the content branch | the protection read shows it (owner sets it) |
| SBA-1 | The source is OpenShell's OTLP export, received on loopback with a header token | RT-22; `401` tests |
| SBA-2 | The collector has no Docker, OpenShell CLI or config access | compose config check |
| SBA-3 | Its token appends only, through two routes | route matrix |
| SBA-4 | Stored fields are the allowlist; no `cmd_line`, `message`, query, header or body | fixture test |
| SBA-5 | Every deny is recorded or counted; allows are counted per 5 minutes | volume tests |
| SBA-6 | Denies are deduplicated on `metadata.uid` | duplicate test |
| SBA-7 | The PM and task are attributed only from one open session or a personal bot's owner | attribution tests |
| SBA-8 | Sequence holes, collector restarts, silence and OpenShell's gap records are each an event | gap tests; RT-24 |
| SBA-9 | The liveness canary bounds unreported loss to 17 minutes | B4 test; host run |
| SBA-10 | The events are `audit_events` rows, purged only by the 90-day job | retention tests |
| SBA-11 | An Auditor sees a destination class, not host, address or path | route tests |
| SBA-12 | An unknown format is counted and shown, never dropped silently | unparsed test |
| SBA-13 | A deny reaches the audit log within 60 seconds with bot, rule and task | RT-23 |
| SBA-14 | The halt (IL2-16) can read denials and gaps for a bot | `denials_since` tests |

**Non-goals.** Running the prover in the web app or on every validate; the policy advisor and its
proposal risk check (it stays off); proving MCP tool allowlists, read paths, credentials or
uploads; recording Landlock file decisions (OpenShell documents none per access); storing allowed
requests one by one; keeping OpenShell's raw records; shipping events off the host; a SIEM;
notifying people of denies; the improver bot's halt itself (phase 2); changing any sandbox policy.

**Open questions for the owner.**

1. Make `policy-prover` a required status check on the content branch (it is GitHub's setting)?
2. Who owns `policies/boundary/` in CODEOWNERS: the platform owners, or the owner alone?
3. May an Auditor see destination hosts and paths, or only the destination class (recommended)?
4. Are 5-minute allow counts enough, or should every allow be stored (about one row per model,
   tool and heartbeat call)?
5. May a standing canary sandbox run on every host for the liveness check?
6. Grant the GitHub App **Checks: read**, so the proposals list shows the prover's result (A6)?
7. Who is told of a deny, and how (Slack DM to admins, email, the overview only)?
8. Should `policy_proofs` rows also be purged at 90 days, or keep only the newest per bot?
