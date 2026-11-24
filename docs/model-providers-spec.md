# Model providers: switching and automatic fallback in `pmos-gateway`

Status: **design spec; not built.** Written on 2026-11-24 after the owner's go-ahead ("build to
the PRD"). It goes to the independent security reviewer before any code (CLAUDE.md, "How to
work"). Code cited is `claude/web-app` at `0e1316b`; line numbers may move, and the slice that
edits a file re-checks them. Nothing here changes `docs/openshell-any-host-spec.md` (r7); where
this spec needs a change to a rule decided there, §2 says so and the change is listed as an open
question or as a later slice.

## 1. The requirement and the rules it must live with

**Requirement i** (owner, 2026-11-18, kept): "the application must be able to switch between
different model providers, and behave like OpenRouter: one interface in front of many providers
and models, chosen by configuration. If a model is not available, the system switches to another
automatically." Decided the same day: switching and fallback are built in `pmos-gateway`;
OpenShell holds one credential per provider and enforces which endpoints are reachable.

**Rules already decided that this design keeps** (each cited where it is written):

| Rule | Where |
| --- | --- |
| Callers name a role (`pmos/<role>`), never a model | `docs/gateway.md`, "API" |
| One gateway serves one model source: `local`, `hosted` or `hosted-test`; every call states the source it expects (`409` otherwise) | any-host spec §0, §3.6 (X4); `check_expected_source`, `crates/gateway/src/app.rs` |
| `local`: only the two pinned model servers, each resolved only to its compiled address | `config.rs` (`pinned_address`), `resolve.rs` |
| `hosted-test`: any endpoint, `class: test`, fake data only, a label on every role | `Config::check_source`, `config.rs` |
| `hosted`: at most one approved entry, compiled in, URL and model ids pinned by digest, record outside the repo, owner alone approves, real data only after H13 | any-host spec §3.6 (V1, V2, X1, X2), §3.9 |
| Kill switch (stop file), compiled-in roots, no redirects, no IP literals, egress rules, data-class ceiling | any-host spec §3.6 |
| No committed file names the hosted vendor, its host or a model id; labels and `model_sha256` instead | CLAUDE.md, owner 2026-11-23; `Role::model_sha256`, `config.rs` |
| A benchmark counts only on the production role models, from a `production` gateway | `docs/evals.md`, "Which runs count" |
| A bot other than Nudge is a Preview unless it claims a benchmark the lint verifies | `docs/bot-marketplace-spec.md` (M2) |
| Sandboxes reach only PM OS's three services; no sandbox holds a provider key | `docs/sandbox-policies.md` (`net-egress`); any-host spec §3.6 |

## 2. Where requirement i conflicts with those rules

Stated plainly, with what this spec does about each.

- **C1. "No model switching, fallback or second provider"** is a non-goal of the any-host spec
  (§0), which also says "one gateway serves one model source; mixing sources across roles is
  requirement i". **This spec keeps one source per gateway.** Switching and fallback happen
  only *within* the gateway's source. A route never mixes `local` with `hosted`, or either with
  `hosted-test`. Mixing sources is open question Q3, with the recommendation "no".
- **C2. At most one approved hosted entry** (§3.6, V2: a unit test fails on a second entry).
  "Many providers" under `hosted` therefore means, until the owner decides otherwise, **many
  models of one approved provider**: an entry already pins several model ids
  (`model_sha256: [...]`, r7), so fallback from a `main` to a `fast` model of the same entry
  needs no change to the cap. That gives no protection when the provider itself is down. A
  second provider for real data needs the cap raised, which is the owner's decision (Q1).
- **C3. One hosted URL and one key** (`PMOS_GATEWAY_HOSTED_URL`,
  `deploy/secrets/hosted-model.key`). Several entries would need one URL and one key file per
  entry (§4.11). Built only with Q1.
- **C4. One data-class ceiling per gateway** (§3.6, V11, enforced by the tool server from
  `/readyz`). With several entries the tool server cannot know which entry will answer a call,
  so the effective ceiling must be the **lowest** of every entry any route can reach (§4.2).
- **C5. The first-switch acknowledgement keys on `hosted:<name>`** (§3.6, X3). With several
  entries the source must name the set of entries, and adding one is a new source that needs
  its own acknowledgement (§4.2).
- **C6. "Production role models" is one model per role** (`docs/evals.md`). With routes it
  becomes the role's **primary** hop (label and `model_sha256`). A fallback hop is not
  benchmarked by a run on the primary (§4.10).
- **C7. Preview is decided per bot, not per answer** (M2: "the services can't read eval
  reports"). A benchmarked bot answered by a fallback model would be shown as benchmarked. This
  spec marks every answer a fallback hop produced, and lets a deployment refuse fallback for
  benchmarked bots (§4.10, Q2).
- **C8. "OpenShell holds one credential per provider."** The gateway is a Compose service, not
  a sandbox, so OpenShell cannot hold its keys today (any-host spec §3.6, "How this relates to
  CLAUDE.md"). This spec follows §3.6: one key file per provider, mounted into the gateway
  container only. The OpenShell half of the 2026-11-18 decision is **not met** until the gateway
  runs in a sandbox (Q6). OpenShell's half that *is* met: no sandbox can reach any provider.
- **C9. A stream that has begun is never repeated** (`retry.rs`). Kept. What changes is when a
  stream counts as begun: at its first content or tool-call delta, not at the upstream's `200`
  (§4.5). The gateway holds back the events before that point.

## 3. Terms

- **Upstream:** one endpoint the gateway may call: a base URL, a wire format and (for a remote
  one) one key. Today's `upstreams:` entries.
- **Model:** an upstream plus a model id, with a neutral **label** and declared features. The
  label is what clients, logs and reports see.
- **Route:** a role's ordered list of models: the **primary** (hop 1), then **fallbacks**.
- **Hop:** one model of a route, tried for one call. **Attempt:** one request to a hop.
- **Commit point:** the moment the first byte of an answer goes to the caller. After it, the
  call is on that hop for good.

## 4. Design

### 4.1 The config shape

A role keeps today's form (`{upstream, model, label, workarounds}`), read as a one-hop route, so
every config in the repository stays valid. The new form names models once and routes by label:

```yaml
class: test                      # a hosted-test example; written outside the repo (§4.12)
upstreams:
  up-a: {url: "openai+https://<host>/<base>", key-file: /run/secrets/pmos-upstream-up-a.key,
         max-in-flight: 4}
  up-b: {url: "openai+https://<host>/<base>", key-file: /run/secrets/pmos-upstream-up-b.key}
models:
  hosted-model-a: {upstream: up-a, model: "<id>", features: [tools, stream, json],
                   context-tokens: 200000, workarounds: []}
  hosted-model-b: {upstream: up-b, model: "<id>", features: [tools, stream, json],
                   context-tokens: 128000}
roles:
  default:   {route: [hosted-model-a, hosted-model-b], requires: [tools, stream]}
  reasoning: {route: [hosted-model-a, hosted-model-b], requires: [tools, stream],
              allow-smaller-context: true}
  extraction: {route: [hosted-model-b], requires: [json]}
routing:                         # every value optional; these are the defaults
  same-hop-attempts: 2           # tries of one hop before the next, first included
  budget-secs: 30                # all hops of one call, from the first attempt
  first-token-secs: 60           # upstream 200 to first content delta (§4.5)
  short-retry-after-secs: 5      # a longer Retry-After moves to the next hop at once
  circuit: {failures: 3, cool-down-secs: 30, max-cool-down-secs: 300}
  sticky-per-run: true           # §4.6
```

- **Labels are the only names in reports.** Every model in a `route:` config must have a label
  (the key under `models:`), under every source. A label is checked as today (`valid_label`,
  `config.rs`), must be unique, and must not equal a role name or an approved entry name.
- **Upstream keys (`up-a`) are config-local.** They never appear in a log, header or report,
  since an operator could type a vendor's name there (§4.9).
- **`workarounds` and the translating upstream's `effort` move from the role to the model**,
  since they describe a model. The old role-level fields stay accepted on the one-hop form.
- **`features`**: `tools`, `stream`, `json` (`response_format`), `embeddings`, `reasoning`
  (the model returns `reasoning_content`). **`requires`** on a role lists what every hop of its
  route must have.

### 4.2 What "many providers" means under each source

| Source | Hops a route may hold | Data | Built in |
| --- | --- | --- | --- |
| `local` | models on the two pinned servers only (`vllm-default.pmos.internal`, `vllm-extraction.pmos.internal`); for example `extraction: [extraction-model, default-model]` | real, after Gate 1 | MP1 to MP5 |
| `hosted-test` | any number of test upstreams, each `openai+https://` (or the translating test upstream), each with its own key | fake only | MP1 to MP8 |
| `hosted`, one entry | the entry's models only: each hop `upstream: hosted:<name>`, `model: "host:<ref>"`, every id hashing to one of the entry's `model_sha256` | real, after H13 | MP9 (after H12) |
| `hosted`, several entries | models of several approved entries | real, after each entry's own H13 | MP10 (after Q1) |

**The boundary rule (MP-3).** Every hop of every route must itself pass, at start-up, every
check today's single upstream passes for the gateway's source: the `local` production-host and
pinned-address rules, the `hosted-test` https and label rules, and every `hosted` refusal of
§3.6 (digests, key file, stop file directory, no IP literal, no `SSL_CERT_*`, ceiling). One hop
that fails stops the gateway; it is never dropped from the route quietly. At call time a hop
whose entry is stopped (§4.11) is skipped. **A fallback never crosses a source, an approval or
a data class:** there is no config in which it could, because a hop that could not serve the
source's data is refused at start-up, and the gateway has no other hops.

**Several entries (MP10 only).** The source becomes `hosted:<a>+<b>` (entry names sorted, `+`
between), in `x-pmos-model-source`, `/readyz` and the audit log; callers' `X-Pmos-Model-Source`
must match it whole. The effective `max_data_class` is the lowest of the entries any route
reaches, and `PMOS_HOSTED_MAX_DATA_CLASS` may only lower it further. The first-switch
acknowledgement (X3) is keyed on the whole set, so adding an entry needs a new one.

### 4.3 How one call is routed

1. The X4 source check, the client policy and the scheduler slot happen once per call, as today
   (`forward`, `app.rs`). A slot covers every hop of the call.
2. The gateway lists the role's hops in order and removes those that cannot serve this request:
   stopped (§4.11), circuit open (§4.6), or lacking a feature the request uses (§4.8). If the
   client may not fall back (§4.10), only hop 1 is kept.
3. With `sticky-per-run`, a hop that answered an earlier call with the same `X-Pmos-Run` in the
   last 15 minutes moves to the front, if it is still in the list. So one task does not
   alternate between models after one fallback.
4. Each hop gets up to `same-hop-attempts`, as §4.4 says, then the next hop is tried, until a
   hop commits, the list ends, or `budget-secs` would be passed.
5. Nothing left: `503`, `{"error": {"type": "no_model_available", "status": 503}}`, with
   `Retry-After` set to the soonest circuit cool-down end (at least 1 s). The body names no hop
   and no upstream's own error.

### 4.4 What triggers a retry or a fallback

Only failures on the upstream's side count. A client's own bad request is never routed around.

| Upstream answer | Same hop again? | Next hop? | Counts against the circuit |
| --- | --- | --- | --- |
| Connect failure, DNS refused by the resolver, TLS handshake failure (nothing sent) | yes, with backoff | yes | yes (TLS failure opens it at once) |
| Timeout before the upstream's status line | no | yes | yes |
| `429` with `Retry-After` up to `short-retry-after-secs` | yes, after it | yes | no |
| `429` with a longer `Retry-After` | no | yes, at once | opens it until `Retry-After` passes |
| `429` without `Retry-After` | yes, with backoff | yes | yes |
| `429` whose body says the quota won't recover (`retry::unrecoverable`) | no | yes, at once | opens it for `max-cool-down-secs` |
| `503`, `529` | as `429` (with or without `Retry-After`) | yes | yes |
| `500`, `502`, `504`, other `5xx` | no | yes | yes |
| "Model unavailable": `404`, or a `400`/`422` whose body says the model is not found, retired or unavailable (a vendor-neutral list beside `unrecoverable`, read before the body is replaced) | no | yes | opens the circuit of that **model**, not its upstream |
| `401`, `403` (the key) | no | yes | opens the upstream's circuit for `max-cool-down-secs`; warning `upstream refused the key` with the label |
| `3xx` (never followed, as today) | no | yes | opens it at once |
| `200` whose body is not JSON, or larger than `max-answer-bytes` | no | yes | yes |
| Stream: no content delta within `first-token-secs`, or ends or errors before one (§4.5) | no | yes | yes |
| Stream after the commit point: silent 120 s, cut off, `stream-max-secs`, an `error` event | **no** | **no** | yes |
| Other `4xx` (`400` context too long, `413`, `422` schema) | no | **no**: returned to the caller (neutral body for a labelled role, as today) | no |
| `200` with `finish_reason: content_filter` or a refusal | no | **no**: a refusal is an answer, never routed to another model | no |

Retries per hop use today's `RetryPolicy` (backoff with jitter, `Retry-After` in seconds or a
date), bounded by `same-hop-attempts` and the call's `budget-secs`; the slot is given back while
waiting, as today. `PMOS_GATEWAY_RETRY_MAX_ATTEMPTS` keeps its meaning for one-hop routes.

### 4.5 Streams, replay, and never splicing two models

- **A model call is safe to replay before the commit point.** The gateway runs no tools; a
  tool call is text until the harness acts on it after the answer. A replay costs tokens twice,
  and the usage log counts each attempt (§4.9). A provider may still have read the prompt of a
  failed attempt; that is within the source's approval, since every hop is.
- **The commit point is the first content.** For a streamed call the gateway reads the
  upstream's events and holds them until one carries a non-empty `content`, `reasoning_content`
  or `tool_calls` delta (or a finish reason), at most 64 KiB and `first-token-secs`. Role-only
  and keep-alive events before it are held, not sent. Then it sends the held events and
  streams the rest as today (label rewriting, size and time limits, the tool-call scanner).
- **Before it, any failure moves to the next hop**; the caller has received nothing, not even
  headers. **After it, nothing is repeated:** the stream ends with an error event, as today
  (`bounded_stream`, `app.rs`), and the harness fails the call (`harness/src/model-stream.ts`).
  A retry is then a new call from the harness, routed afresh. Two models' output is never
  joined into one answer.
- **Whole answers** (not streamed, or streamed to a role with `strip-think`) commit when the
  gateway has the whole, valid answer; until then every failure may move on.

### 4.6 Health: a circuit per model and per upstream

- **Closed** (normal) → **open** after `circuit.failures` failures in a row, or at once where
  §4.4 says. An open circuit skips the hop for its cool-down: `cool-down-secs`, doubling on each
  reopening up to `max-cool-down-secs`, or the `Retry-After` that opened it.
- **Half-open** when the cool-down ends: the next real call may use the hop; one call at a time.
  Success closes it and resets the cool-down; failure reopens it, doubled.
- **No synthetic traffic.** The gateway sends no extra keyed requests to probe a hop. The
  `/readyz` probe of `/models` stays as today (cached 30 s for a remote upstream,
  `REMOTE_READY_CACHE`) and is shown beside the circuit state, but does not close a circuit.
- **An upstream's circuit** (key refused, connect failures) covers all its models; **a model's
  circuit** ("model unavailable") covers that model only.
- State is in memory, lost at restart (all circuits closed). Each change of state is one
  warning line with the label and the reason's kind, never a URL or body.

### 4.7 Concurrency and the scheduler

- The per-person, per-client and per-pool scheduler (`scheduler.rs`) is unchanged and admits a
  call once. `limits.max-in-flight` still bounds calls in flight across all hops.
- **Per upstream:** `upstreams.<u>.max-in-flight` (default `limits.max-in-flight`) bounds calls
  in flight to that upstream. A hop at its cap is **not** a failure: the call waits for it,
  within `queue-wait-secs`, as for a slot. Busy is never a reason to switch model, so capacity
  alone never changes which model a PM gets.
- Under `local` the two pinned servers share one GPU as today; per-upstream caps let an
  operator keep a fallback from crowding the other server.

### 4.8 Features, wire formats and translation

- **At start-up:** every hop of a role must have every feature in its `requires:`; and unless
  the role says `allow-smaller-context: true`, no fallback may declare a smaller
  `context-tokens` than the primary. Otherwise the gateway refuses to start, naming the role and
  the label.
- **Per call:** a request with `tools` skips hops without `tools`; `response_format` skips hops
  without `json`; `stream: true` skips hops without `stream` (unless the hop's workaround already
  turns it into a whole answer); `/v1/embeddings` uses only hops with `embeddings`. A hop is
  never sent a request it declares it cannot serve.
- **Wire formats, first:** chat-completions only (`http://` local servers, `openai+https://`
  test upstreams, `hosted:` entries). The translating test upstream (`UpstreamKind` in
  `config.rs`) may be a hop under `hosted-test` only, as today. Another format is a new
  translation module, reviewed on its own, test-only until approved.
- **What travels between hops of one task:** the conversation history the harness sends. Tool
  call `extra_content` stays keyed by upstream (`ToolCallExtras`, `crates/gateway/src/remote.rs`),
  so one upstream's opaque fields are never sent to another. Whether every pair of providers
  accepts the other's tool-call ids in history is **not verified**; MP3 tests it with mocks and
  MP8 against real test upstreams.
- **Workarounds** are per model: `strip-think` on hop 1 and not on hop 2 is valid, applied to
  whichever hop answers. The doc comment on `UpstreamKind::RemoteOpenAi` (`config.rs`) mentions
  `app::REMOTE_DROPPED_FIELDS`, which is not in `app.rs` at `0e1316b`; MP1 checks which fields,
  if any, are dropped per wire format.

### 4.9 Reporting: headers, usage log and audit, labels only

- **Every answer** keeps `x-pmos-model-source` and `x-pmos-model` (now the answering hop's
  label), and adds `x-pmos-model-sha256` (that model's `model_sha256`), `x-pmos-route-hop`
  (1-based) and `x-pmos-fallback` (`0` or `1`). A whole answer's `model` field and each streamed
  event's are the answering label (`relabel`, `relabel_stream`, `app.rs`).
- **Usage log** (`UsageEvent`, `crates/gateway/src/usage.rs`): one line per attempt, as today,
  each with `model` (label), `model_sha256`, `upstream` (the kind label
  `upstream_kind_label` gives today, or `hosted:<name>`), and new fields `hop`, `fallback` (true when not
  hop 1), `reason` on a failed attempt (`connect`, `timeout`, `rate_limit`, `quota_exhausted`,
  `overloaded`, `server_error`, `model_unavailable`, `auth`, `redirect`, `bad_answer`,
  `first_token`), and `circuit` when a hop was skipped. The call's final line also lists
  `skipped: [{label, why}]`. Token counts are kept per attempt, so each provider's tokens can
  be summed. Under `hosted-test` a line may add `upstream_sha256` (the normalised URL's digest,
  as §3.6 makes it), so two test upstreams can be told apart without naming either.
- **Never in a report:** an upstream key (`up-a`), a URL, a host, a model id, an upstream's
  error text.
- **Cost:** the gateway records tokens, not money. `pmos-admin usage --by-label` sums tokens per
  label and multiplies by a host-side price file (`$PMOS_STATE_DIR/prices.yaml`, label → price
  per million tokens, outside the repository, since prices can identify a vendor).
- **Audit log:** unchanged in kind: the tool server's events carry the session's
  `model_source` (§3.9). The runtime records on each task `models_used`
  (`[{label, model_sha256, calls}]`) from the harness's report (§4.10), so an Auditor sees,
  per task, which labelled models answered; the per-call record stays the usage log, joined by
  `run`. Which model id a label meant is found, as today, by hashing the ids the host's config
  has held.

### 4.10 Benchmarks and the Preview label

- **A claim holds for the model that produced it.** "Production role models" become, per role,
  the primary hop's label and `model_sha256` in the production gateway config. A report counts
  only when every call of the run was answered by those primaries.
- **Who may fall back** is a client setting, `clients.<…>.route`: `fallback` (default),
  `primary-only` (hop 1 or fail), or `pin` (the client may send `X-Pmos-Route-Model: <label>`,
  naming one model of the role's route; any other label, or the header from a client without
  `pin`, is `403`, nothing forwarded). Any client may send `X-Pmos-Fallback: off` to ask for
  `primary-only` on one call; no client can ask for more than its setting allows.
- **The eval stack pins the route.** The `evals` client is `pin` in every shipped config. The
  runner records the pinned labels (default: each role's primary) in the report, every worker
  and judge call sends them, and the harness records per call the `x-pmos-model` and
  `x-pmos-model-sha256` it got. A run in which any call came back with another label, or with
  `x-pmos-fallback: 1`, counts towards no bar (defence in depth; the pin should make it
  impossible). A run pinned to a fallback hop measures that model and is reported under its
  label; it never counts for the primary.
- **The harness reports what answered.** The worker (`harness/src/agent.ts`, the fetch wrapper
  that already checks `x-pmos-model-source`) reads the three headers on every answer and sends
  the runtime `models_used` and `fallback_used` with the task's result.
- **A benchmarked bot answered by a fallback.** By default (`PMOS_FALLBACK_FOR_BENCHMARKED=mark`)
  the answer is kept and marked: the web app and Slack show "Answered by a fallback model: not
  benchmarked for this bot" on the reply and on each draft card, as they show the Preview line,
  and the draft records it. With `refuse`, the harness sends `X-Pmos-Fallback: off` for a bot
  whose catalog entry is benchmarked, so the call fails as "the model is busy" rather than
  being answered by an unbenchmarked model. Preview bots and Nudge are always `mark`. The
  default is open question Q2.

### 4.11 Keys, egress and kill switches

- **One key per upstream, in its own file**, never in an environment variable, a config file,
  a report or a log: `deploy/secrets/upstream-<u>.key` (dir `700`, file `600`), mounted by
  Compose `secrets:` into the gateway container only, at `/run/secrets/pmos-upstream-<u>.key`,
  read through the upstream's `key-file:`. Re-read on a change of size or mtime (at most once a
  second) and on `SIGHUP`, as §3.6 does for the hosted key. An empty or unreadable file opens
  that upstream's circuit (`auth`) instead of stopping the gateway. `pmos-admin upstream-key set
  <u>` reads the key from standard input. A key is sent only to its own upstream's host (tested:
  a mock second upstream must never see the first's key). `PMOS_GATEWAY_REMOTE_KEY` stays for a
  config with one remote upstream; a config with two or more refuses it.
- **Under `hosted`**, an entry's key stays `deploy/secrets/hosted-model.key` while there is one
  entry; with several (MP10), `hosted-model-<name>.key` each.
- **Egress:** only the gateway container reaches a provider. Each upstream's host must not match
  `NO_PROXY` (`pmos-admin up` checks every one); no redirects; under `hosted` the compiled-in
  roots and the resolver's refusals of §3.6 apply to every entry. `net-egress` keeps every
  provider's host out of every sandbox policy, unchanged.
- **Kill switches:** under `hosted`, the stop file `/var/lib/pmos/hosted-control/stopped` stops
  every hosted hop (as §3.6); `stopped-<name>` stops one entry's hops only (MP10). Under every
  source, `pmos-admin route disable <label> --reason "<text>"` writes
  `$PMOS_STATE_DIR/route-control/disabled-<label>`, read before every call (a `stat`, uncached),
  so a model can be taken out of every route without a restart; `route enable` removes it. Both
  are logged. Disabling the last hop of a role makes that role answer `503 no_model_available`.

### 4.12 Configuration and operation

- **Where configs live.** Committed configs (`deploy/gateway.yaml`, the hosted-test example)
  hold no vendor, host or hosted model id. Under `hosted-test`, `pmos-admin` renders today's
  one-upstream config from `deploy/.env` (`deploy/pmos-admin`, `PMOS_HOSTED_TEST_*`); for
  several upstreams the operator writes the route config at
  `$PMOS_STATE_DIR/gateway.hosted-test.yaml` (outside the repository) and `pmos-admin` checks
  it (`gateway --check-config`) before use. Under `hosted`, the committed
  `deploy/gateway.hosted.yaml` carries routes of `host:<ref>` models with labels, and the ids
  come from `PMOS_GATEWAY_HOSTED_MODELS` checked against the compiled digests (§3.6).
- **Start-up validation** (refuse to start, naming the setting, role and label, never a URL or
  id): unknown label in a route; empty route; duplicate label; a label without `label:` rules;
  a hop failing the source's rules (§4.2); a missing `requires` feature; a smaller context
  without `allow-smaller-context`; a key file missing for a remote upstream; under `hosted`,
  every §3.6 refusal per hop.
- **Changes need a restart**, as a model swap does today (`docs/gateway.md`, D11): routes,
  models and upstreams are read once and logged. Only keys, stop files and `route disable` act
  without one. A change of a role's primary restarts that role's benchmark streaks.
- **`GET /readyz`** (no token): per role, `status` (`ok` while any hop is usable), `hops_usable`
  and `hops` (a count), and each upstream's kind; **no labels**, since this route needs no token.
- **`GET /v1/pmos/roles`** (token): per role, `route: [{label, model_sha256, kind, features}]`.
  **`GET /v1/pmos/routes`** (token, new): per hop, `label`, `circuit` (`closed`, `open` until a
  time, `half-open`), `disabled`, `stopped`, and the last `/models` probe.
- **`pmos-admin status`** shows per role `ok`, `degraded (on fallback)` or `down`, from
  `/readyz`; **`pmos-admin routes`** shows `/v1/pmos/routes` with the `admin` client token, a
  new read-only client the gateway refuses on every model route (`403`).

## 5. Security analysis

| Threat | Control | Test (slice) |
| --- | --- | --- |
| A route configured to a wider endpoint (a test upstream in a `local` or `hosted` route; a hop whose URL or id is not the approved one) | Every hop passes the source's start-up rules; under `hosted` each hop's URL and id hash to its entry | `local` config with an `openai+https://` fallback refuses to start; `hosted` route with an id hashing to no `model_sha256` refuses (MP1, MP9) |
| Fallback as an exfiltration path (data reaching an endpoint not approved for it) | One source per gateway; no cross-source route; effective ceiling is the lowest entry's; stopped entries skipped per call | Hop 1 answers `503`, hop 2's entry stopped: caller gets `503`, the mock behind hop 2 receives nothing (MP3, MP10) |
| A downgraded model answering silently | Headers and usage line on every answer; `models_used` on the task; the fallback mark; `primary-only` for benchmarked bots when chosen; eval runs pinned | A fallback answer to a benchmarked bot shows the mark in web and Slack; with `refuse` the mock fallback receives nothing (MP7); an eval run with a fallback label counts for no bar (MP6) |
| A caller forcing a chosen model | Only `pin` clients may name a hop; `X-Pmos-Fallback` can only narrow | `X-Pmos-Route-Model` from `runtime` is `403` and nothing is forwarded (MP6) |
| A caller tripping circuits to push traffic elsewhere | Client-side `4xx` never counts; circuits count upstream failures only | 100 malformed requests leave every circuit closed (MP4) |
| Error bodies leaking vendor names | Every hop's error replaced by the neutral body (V3) under `hosted` and `hosted-test` and for any labelled model; intermediate hops' errors never reach the caller; `no_model_available` names nothing | Each mock hop answers `401` with a marker string: no answer, header, log or usage line contains it (MP3) |
| Key sprawl or a key sent to the wrong host | One file per upstream, gateway container only, mode checks, never in env for two or more upstreams; key bound to its upstream; no redirects | The second mock never sees the first key; `docker inspect` of the gateway shows no key (MP2) |
| A compromised provider | Its answers are text, as today; its `extra_content` never reaches another upstream; size and time limits per hop; `route disable` and the stop files cut it off without a restart; a `429` storm from it moves traffic only to approved hops | A mock that returns oversized and malformed answers falls back without splicing; `route disable` takes effect on the next call (MP3, MP8) |
| Replay amplification and cost | `same-hop-attempts`, `budget-secs`, route length bound attempts; each attempt has a usage line with its tokens | A call through three failing hops makes at most 2 + 1 + 1 attempts within the budget (MP3) |
| Labels exposed without a token | `/readyz` shows counts and kinds only | `/readyz` body contains no label (MP4) |

The prompt of a failed attempt may have reached that hop's provider; every hop is approved for
the data, so this widens nothing, but it means a fallback can send one prompt to two providers.
§3.9's list of what leaves the host gains that sentence when MP10 is built.

## 6. Slices, in build order

| Slice | Changes | Tests | Reviewer first? | Can run today |
| --- | --- | --- | --- | --- |
| **MP1** Config model | `config.rs`: `models:`, `route:`, `requires:`, `routing:`, per-model workarounds; one-hop form unchanged; all start-up checks of §4.12; `--check-config` | parse and refuse tests for each rule; every shipped config parses to the same roles | with this spec | yes |
| **MP2** Per-upstream keys | `key-file:`, re-read, `SIGHUP`; `deploy/compose.hosted-test.yaml` secrets; `pmos-admin upstream-key set`; `PMOS_GATEWAY_REMOTE_KEY` refused with two upstreams | key isolation between mocks; mode checks; empty file opens the circuit | **yes** | yes |
| **MP3** Fallback engine | `app.rs` `forward`: hop loop, §4.4 table, commit point, neutral errors, headers, usage fields | mock upstreams in `crates/gateway/tests/`: every row of §4.4; never splice; marker-string leak test | **yes** | yes (`hosted-test`, mocks, stub) |
| **MP4** Circuits and health | circuit states; `/readyz` counts; `/v1/pmos/routes`; `admin` client; `pmos-admin status`/`routes` | state transitions; client `4xx` never trips; `/readyz` has no label | yes | yes |
| **MP5** Concurrency and stickiness | per-upstream caps; `sticky-per-run` | busy never falls back; one run stays on its hop | no | yes |
| **MP6** Benchmarks | `clients.<…>.route`, `X-Pmos-Route-Model`, `X-Pmos-Fallback`; `crates/evals` pins and records the route; "Which runs count" on the primary's label and hash | `403` for a pin without `pin`; a report with a fallback label never counts | **yes** (benchmark integrity; CODEOWNERS paths) | yes |
| **MP7** Callers and the mark | harness reads headers, sends `models_used`; runtime stores it; web and Slack show the mark; `PMOS_FALLBACK_FOR_BENCHMARKED` | web and Slack tests for the mark (as M1's); `refuse` sends nothing to the fallback | **yes** (the Preview label was a High before) | yes |
| **MP8** Operation | `route disable`/`enable`, the route-control directory, `usage --by-label`, the rendered multi-upstream hosted-test config check | stack test on the test stack with two mock upstreams, then two real test upstreams with fake data | no | yes |
| **MP9** Routes within one entry | `hosted:` hops of one entry's `host:<ref>` models | as H12's stack test, with two refs | yes | **waits on H12** |
| **MP10** Several approved entries | raise the list's cap; per-entry URL, key, stop file; source `hosted:<a>+<b>`; ceiling minimum; acknowledgement per set; §3.9 text | the list test with N entries; per-entry stop; ceiling | **yes** | **waits on Q1, then H13 per entry** |

Everything through MP8 can be built and tested now under `hosted-test` with fake data, the stub
and mock upstreams, and under `local` with mocks (no GPU host has run `local`). Nothing is
measured against a real model until an eval run with pinned routes has happened.

## 7. Non-goals

- Routes that mix sources (`local` primary, hosted fallback). Q3.
- Choosing a model per request by cost, latency or content ("cheapest that can do it"). The
  route is ordered and fixed by configuration.
- Falling back on "context too long" to a larger model, or counting tokens in the gateway.
- Spend budgets that switch providers; the gateway records tokens only (Q4).
- Wire formats other than chat-completions for real data.
- OpenShell injecting provider keys into the gateway (C8, Q6).
- Hot reload of routes.

## 8. Open questions for the owner

1. **Q1. Raise the one-entry cap?** *Recommendation:* not until the first entry is in force
   (H13); then allow up to three, each with its own record, pull request, approval, key and
   RT-18 run. *For the reviewer:* each entry is a new data path in §3.9, the ceiling becomes the
   lowest entry's, the source names the set, and a fallback sends one prompt to two providers.
2. **Q2. A benchmarked bot answered by a fallback:** mark it (recommended: the PM still gets an
   answer, and is told) or refuse it?
3. **Q3. May a route mix sources?** *Recommendation:* no. A hosted fallback for a local primary
   would send data off the host exactly when the operator believes it stays.
4. **Q4. Spend limits per provider?** *Recommendation:* tokens per label now; limits later.
5. **Q5. Keyed digests** (any-host §8.2 item 13) apply equally to `upstream_sha256` and
   `x-pmos-model-sha256`; no new decision, but the answer covers them.
6. **Q6. Keys in OpenShell:** accept key files in the gateway container (recommended, as §3.6)
   until the gateway runs in a sandbox?

## 9. Requirements

| Id | Requirement | Acceptance check |
| --- | --- | --- |
| MP-1 | Callers keep naming `pmos/<role>`; no caller changes to use routes | harness and runtime tests pass unchanged against a two-hop config |
| MP-2 | A role may have an ordered route of labelled models; today's one-hop form still parses | every shipped config parses to the same roles; a two-hop config parses |
| MP-3 | Every hop passes the source's start-up rules; one failing hop stops the gateway | the refusal tests of §5, row 1 |
| MP-4 | No route crosses a source, an approval or a data class | no config with such a route starts; hop-2-stopped test sends nothing |
| MP-5 | Fallback triggers exactly as §4.4; client `4xx` and refusals never fall back | one test per row of §4.4 |
| MP-6 | Same-hop retries and the whole call are bounded by `same-hop-attempts` and `budget-secs` | attempt-count test with three failing hops |
| MP-7 | Before the commit point any failure may move on; after it nothing is repeated or spliced | stream tests: failure before first content falls back; after, error-terminated, mock 2 receives nothing |
| MP-8 | Circuits per model and per upstream, half-open with one live call, no synthetic probes | state-transition tests; mocks count only real calls and `/models` probes |
| MP-9 | Capacity alone never changes the model | per-upstream cap test |
| MP-10 | A hop lacking a feature the request uses is never sent it; start-up refuses a route missing `requires` | feature-skip and refusal tests |
| MP-11 | Every answer carries the answering label, hash, hop and fallback flag; no URL, host, id or upstream key anywhere | header tests; marker-string leak test over answers, logs and usage lines |
| MP-12 | One usage line per attempt with label, hash, kind, hop, reason and tokens | usage-log tests |
| MP-13 | One key file per upstream, gateway only, sent only to its own host | key-isolation test; `docker inspect` check in the stack test |
| MP-14 | `route disable` and the stop files act on the next call, without a restart | stat-per-call test; stack test |
| MP-15 | `/readyz` shows no label; token routes show labels and circuit state | body tests |
| MP-16 | A benchmark claim holds only for the primary's label and hash; eval runs are pinned and a run with another label counts for nothing | `crates/evals` history tests |
| MP-17 | Only `pin` clients choose a hop; `X-Pmos-Fallback: off` only narrows | `403` tests; nothing forwarded |
| MP-18 | A fallback answer is marked wherever the Preview line appears; `refuse` mode sends nothing to the fallback for benchmarked bots | web and Slack tests; mock receives nothing |
| MP-19 | Each task records `models_used`; the usage log joins it by `run` | runtime test; join test |
| MP-20 | Under `hosted`, routes use only the approved entry's models until Q1 is decided; the cap test stays | the list's unit test unchanged; MP9 stack test |
