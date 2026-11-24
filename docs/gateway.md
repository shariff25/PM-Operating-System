# The model gateway: `pmos-gateway`

Every bot reaches the language model through one OpenAI-compatible endpoint on PM OS's
host. The gateway does four jobs:
- **Picks the model:** callers name a *role* (`pmos/default`, `pmos/extraction`,
  `pmos/reasoning`), never a model, and the gateway knows which model serves each role.
- **Sends prompts only where the model source allows** (ADM-3, as the owner's decisions of
  2026-11-22 narrowed it): under `local` it refuses to start with any model server but the two
  pinned local ones on PM OS's host; a remote upstream runs only under `hosted-test`, with fake
  data; `hosted` (one owner-approved hosted model, for real data) is not built and is refused
  ("Model sources" below).
- **Shares the GPU fairly** (MOD-4).
- **Logs every call:** metadata only, never the text.

```
bot runtime / harness ──► pmos-gateway ──► vLLM (default role: Qwen)
   (ChatOpenAI, model "pmos/default")  └──► vLLM (extraction role: Nemotron)
```

## Model sources (slice H4)

Where prompts go is `PMOS_MODEL_SOURCE` (`docs/openshell-any-host-spec.md` §3.1 and §3.6),
required by the gateway and by every service that calls it or reads its answers, with no
default: a blank value is refused, never read as `local`.

| Source | Config `class` | Upstreams | Data |
| --- | --- | --- | --- |
| `local` | `production` only (`deploy/gateway.yaml`), never with `PMOS_GATEWAY_CLOUD_TEST=1` | the two model servers, `vllm-default.pmos.internal` and `vllm-extraction.pmos.internal`, and no other name | real data, after Gate 1 |
| `hosted-test` | `test` only (`deploy/gateway.hosted-test.yaml` is an example; `deploy/pmos-admin` writes the real one outside the repo), with `PMOS_GATEWAY_CLOUD_TEST=1` unless the stub is on, and a `label:` on every role | a remote test upstream (`openai+https://…`) | **fake data only** |
| `hosted` | (slice H12) | | refused: not built |

The stub may be on under either source (a first install); the gateway then reports the source
`stub`, which sends nothing anywhere and which every caller accepts in place of its own.

**The names' answers (`local`).** A `class: production` gateway's HTTP clients resolve each
model server's name with a resolver of their own, at every connection: `vllm-default` only to
`10.201.77.13` and `vllm-extraction` only to `10.201.77.14`, the addresses `deploy/compose.yaml`
pins (a unit test checks they are equal). Any other answer (loopback, an address in the
network's range, another service, a second address beside the right one) fails that call with a
`502` and a warning naming the server, never the address. These are compiled constants:
`PMOS_NETWORK_SUBNET` and `PMOS_NETWORK_IP_RANGE` never reach the gateway.

**Every answer says the source.** Each response, errors, refusals, streams, `/healthz` and
`/readyz` included, carries `x-pmos-model-source: <source>` (`local`, `hosted-test` or `stub`;
`hosted:<name>` from H12). `/readyz` and `GET /v1/pmos/roles` also have `model_source`.

**Every model call names the source it expects (X4).** `POST /v1/chat/completions` and
`POST /v1/embeddings` need `X-Pmos-Model-Source`: missing, `400 model_source_required`; another
source, `409 model_source_mismatch` (with the gateway's own in `x-pmos-model-source`). Checked
before the call is queued, counted or forwarded, so nothing reaches a model server. The harness
sends the source the runtime recorded when it first claimed the task; the runtime's routine
parsing sends the one it checked; the benchmark judges send the one `GET /v1/pmos/roles` said
when the run started. The tool server and the runtime also read `/readyz` (no token) before a
session, a tool call, a claim or a routine parse, and refuse on a mismatch.

**Error bodies and logs (V3).** Under `hosted-test` every upstream error body is replaced by
`{"error": {"type", "status", "message"}}` with a fixed message (as for a labelled role), keeping
`Retry-After`; under `local` an unlabelled role's own error (PM OS's vLLM) still reaches the
caller. Every per-request warning and start-up line names the role and the upstream's kind
label, never its URL, and a transport error by its kind (`connect`, `timeout`, `body`, …), never
its text; the HTTP libraries' own lines are held at `warn` whatever `RUST_LOG` says, since
theirs name addresses. A production model server's start-up line keeps its `*.pmos.internal` name.

**No redirects.** Neither HTTP client follows one: a `3xx` from a model server is a `502` for
that call, and neither its body nor its `Location` reaches the caller.

**Readiness of a remote test upstream.** `/readyz`'s probe of a testing-only upstream (it carries
the provider's key) is reused for at least 30 seconds, however many callers ask.

## Swapping a model (D11)
Edit `config/gateway.yaml` and restart the gateway.

```yaml
class: production        # deploy/gateway.yaml only; any other config is a `test` gateway
upstreams:
  qwen: http://vllm-default.pmos.internal:8000/v1   # production: <name>.pmos.internal only
roles:
  default: {upstream: qwen, model: qwen3.6-35b-a3b, workarounds: [strip-think]}
limits: {max-in-flight: 4, per-user: 2, max-queue: 64}
clients:
  runtime: {trusted: true, max-in-flight: 2}
  bot-pm-*: {trusted: true, max-in-flight: 3, user: from-name}
  bot-*: {trusted: true, max-in-flight: 3}
  evals: {max-in-flight: 1, priority: personal}
```

- **`model`** is the vLLM server's `--served-model-name`.
- **No caller changes:** the harness and the bots keep asking for `pmos/default`.
- **Re-run the benchmark** (0g) after any swap, as D11 requires. The roles' models in
  `deploy/gateway.yaml` are the *production role models*: an eval report counts towards a bar
  only if the models the gateway reported for its run are these (`docs/evals.md`, "Which runs
  count"), so a swap there starts every streak again.
- **`label`** (optional) is a neutral name for the model, such as `hosted-model-a`, reported
  in its place: see "Hiding the model behind a label" below.
- **The `reasoning` role** serves the PM-craft skills (`docs/pm-craft.md`). It ships pointing
  at the same upstream and model as `default`. If the `pm-craft` benchmarks show that model
  doesn't meet the bar, point `reasoning` at a larger one here; no skill changes.

## Workarounds (D11)
Scaffolding for a model's weaknesses lives in the gateway behind a flag. Each one says when
it can be deleted.

| Id | What it does | Delete when |
| --- | --- | --- |
| `strip-think` | Moves a `<think>…</think>` block out of the answer into `reasoning_content`, so reasoning never reaches a PM's draft. Non-streaming replies only. | vLLM runs the model with a reasoning parser, and the benchmark passes with workarounds off |

- **Per role:** a role switches a workaround on in `workarounds:`.
- **All off:** `PMOS_GATEWAY_WORKAROUNDS=off` switches every workaround off for the benchmark run that decides whether one can go.
- **Registry:** `GET /v1/pmos/workarounds` lists the registry, which roles use each workaround, and whether workarounds are on.

## Sharing the GPU (MOD-4)
- **Concurrency:** at most `max-in-flight` calls run at once.
- **Per person:** at most `per-user` of them belong to one person (`X-Pmos-User`), counted
  only when the gateway believes the header (below), and counted together across every client
  that names them. A person is counted under one key, the spelled form of their handle (`.`
  and `_` as `-`, a run of hyphens as one; `spelled_handle`), whichever client names them and
  in whatever spelling: `ana.k` from the runtime or a team bot and `ana-k` from `bot-pm-ana-k`
  are one person with `per-user` slots in all (finding G1 of the review at 6bb806f; before,
  the runtime and team bots kept the handle as named and the PM's own bot its spelled form, so
  a PM with `.` or `_` in their handle had `per-user` slots under each, and their usage was
  split across two names). Tested in `crates/gateway/tests/limits.rs`,
  `a_persons_running_cap_holds_across_clients_for_a_dotted_handle`.
- **A client's share of one person** (`clients.<name>.per-user-share`, finding G3 of the review
  at 6bb806f): a trusted client that may name anyone (`user: any`: a team bot such as
  `bot-docs`, or the runtime) holds at most this many of one person's `per-user` running slots,
  **1 by default**, and never more than its person pool's slots (below). A `from-name` client
  (the PM's own bot) may hold all of `per-user` without `person-pools:`, and what the pools
  leave with it. Only for a trusted client. The test stack's configs raise it for their shared
  `runtime` and `worker` tokens (below).
- **A person's slots** (person pools; finding W1 of the review at 52dbae2, after U1 of the
  review at c78b5fb): every trusted `user: any` client (the runtime, every team bot) is in a
  **person pool**, and a pool's clients together hold at most the pool's slots of one
  person's `per-user`. `person-pools:` names the pools and their slots; each such client says
  its pool with `clients.<…>.person-pool`. What the pools leave of `per-user` is the PM's own
  bot's (`from-name`): its share defaults to that and may not be more, so every pool's slots
  and the own bot's are each really kept for them. `deploy/gateway.yaml` and
  `config/gateway.yaml` (`per-user: 3`):

  ```yaml
  person-pools: {runtime: 1, team-bots: 1}   # and 3 - 1 - 1 = 1 for the PM's own bot
  clients:
    runtime: {trusted: true, max-in-flight: 2, person-pool: runtime}
    bot-pm-*: {trusted: true, max-in-flight: 3, user: from-name}
    bot-*: {trusted: true, max-in-flight: 3, person-pool: team-bots}
  ```

  **What is guaranteed**, for each PM: the runtime's call for them never waits on a team bot's
  calls for them, nor on their own bot's; a team bot's call never waits on the runtime's or
  their own bot's; their own bot's call never waits on the runtime's or any team bot's. Each
  starts as soon as its own slot for that PM is free, a GPU slot (`max-in-flight`) is free,
  and its client is under its own cap. It can still wait behind calls of its own kind for that
  PM (the runtime's second call for a PM waits for the first, U2; one team bot's call waits
  for another team bot's), and behind other people's calls when every GPU slot is taken. No
  PM ever has more than `per-user` (3) calls running, from all clients together. **Capacity:
  one PM can now hold 3 of the 4 model slots at once** (before W1, 2 of 4): the runtime's, the
  team bots' and their own bot's. This follows the reviewer's recommendation; the owner is to
  be told. Before W1 (U1), the runtime shared one slot with every team bot, so a team bot's
  token, a stolen one included, that kept that slot busy for chosen PMs made their routine
  parsing wait `queue-wait-secs` and get `503`. Tested in `scheduler.rs`,
  `team_bots_saturated_the_runtime_still_starts`,
  `runtime_and_team_bots_saturated_her_own_bot_still_starts` and
  `one_person_never_holds_more_than_per_user`, and with the shipped config in `limits.rs`,
  `team_bots_cant_hold_up_the_runtime_and_neither_holds_up_her_own_bot`.

  **Checked at start-up** (`limits.rs`, `person_pools_are_checked_at_start_up`): a pool of 0
  slots, a pool no client is in, a `person-pool` that isn't listed, a `person-pool` on a
  client that isn't trusted or is `from-name`, a trusted `user: any` client in no pool when
  `person-pools:` is given, a share above its pool's slots (or, for a `from-name` client,
  above what the pools leave), and pools whose slots, plus one for the PM's own bot when a
  `from-name` client is listed, are more than `per-user`: each refuses to start.

  **Without `person-pools:`** (the test stack's configs, and any config written before W1)
  nothing changes: every trusted `user: any` client is in one pool (`user-any`) of `per-user`
  slots, or `per-user - 1` when a `from-name` client is listed (U1: the last is kept for the
  PM's own bot, which may hold all of `per-user`; such a config needs `per-user` of at least 2).

  **The reserve applies to everyone** (the Low on U1's shared cap): the gateway doesn't know
  which people have an own bot, so once a `from-name` client is listed, the own bot's slot is
  kept back for every person, including one with no personal bot (an Admin, or a PM who
  hasn't picked one). For such a person, at most the pools' slots (2 of `per-user` 3 in
  `deploy/gateway.yaml`) are ever used: one through the runtime and one through the team bots.
  This is deliberate (a slot kept for no one costs nothing while others' calls can use the GPU
  slot), and it never lets anyone hold more.
- **Per client:** each client token has its own cap, `clients.<name>.max-in-flight` (default
  `limits.per-client`, itself defaulting to `per-user`), whoever its calls name. So one token
  can't hold every slot (review finding N3); in `deploy/gateway.yaml` the `evals` token, which
  the whole eval stack shares, holds at most one (N4).
- **Waiting, per client and per person** (review finding R2, 2026-11-22): each client may
  have at most `clients.<name>.max-queue` calls waiting for a slot (default twice its
  `max-in-flight`), and each believed person at most `limits.per-user-queue` (default twice
  `per-user`) **through each client**. Before, those caps applied only when a slot was
  handed out, so one token could fill the whole queue and every other caller got an immediate
  `503`. A call past its client's or person's share gets `429` with `Retry-After: 5`, and a
  usage line (`refused`, `attempts: 0`; see "Usage log" for how repeats are counted); the
  others keep their places.
- **Why per client** (review at 2544d01, F1): with one count per person across clients, a
  team bot's token (`bot-docs`, `user: any`) naming one PM could fill all that PM's waiting
  places, and the PM's own calls through `runtime` got an immediate `429`. Now no client can
  take a place another client has for the same person (tested in `scheduler.rs`,
  `one_client_cant_take_all_of_a_persons_places`). The trade-off: one person may have up to
  `per-user-queue` calls waiting through each client that names them, still bounded by each
  client's `max-queue` and the whole `max-queue`. The running cap (`per-user`) still counts a
  person's calls from every client together, under the one key above.
- **Priority** (`X-Pmos-Priority`): `interactive` (someone waiting in Slack), then `scheduled` (routines), then `personal` (background work). Within a priority it's first come, first served.
- **Which clients are believed:** only a client listed with `trusted: true` has its
  `X-Pmos-User` and `X-Pmos-Priority` used; with no priority header its calls get its
  `priority` (default `interactive`). Every other client, listed or not, is keyed by its own
  name (`client:<name>`, under its own cap only) and always gets its `priority` (default
  `personal`), whatever it sends. Malformed headers are refused (`400`) from every client.
  The usage log's `user` is empty for an untrusted client.
- **Client names:** 1 to 40 lowercase letters, digits and `-` (room for `bot-` before the
  longest personal bot name, 32), in `PMOS_GATEWAY_TOKENS` and in `clients:`. A `clients:`
  entry is an exact client name or a pattern ending in `*` with **at least 4 characters
  before the `*`** (`bot-*`), so no pattern covers every client: a bare `*`, `b*` or `bot*` is
  refused at start-up (the review of 2efb73f). An exact name wins, then the longest pattern.
- **Whom a trusted client may name** (`clients.<…>.user`, review finding R1): `any` (the
  default) or `from-name`, on a pattern only: the client may name only the person whose
  handle the `*` part of its name spells, written as a personal bot's name writes it (`.` and
  `_` as `-`, a run of hyphens as one; `spelled_handle`). So with
  `bot-pm-*: {trusted: true, user: from-name}`, `bot-pm-ana-k` may name `ana.k` (or `ana_k`,
  which would share that bot's name) and no one else; naming anyone else is `403`
  (`permission_error`), nothing goes to the model server, and the usage log records a line
  with `status: 403`, `refused`, and in `user` the handle it tried to name. A call naming no
  one is still accepted. A test checks that every valid handle's `bot-pm-<…>` client name is a
  valid client name that may name that handle and no other.
  - **Handles too long to spell** (review at 2544d01, F2): a personal bot's name is cut to 32
    characters, so `bot-pm-<29 characters>` would spell every handle that starts with those
    29. A `from-name` client therefore never names a handle whose spelled form is longer than
    29 characters (`MAX_SPELLED_HANDLE`): `403`, as above. Onboarding never gives out such a
    handle (24 at most, `valid_new_handle`), but an Admin can create one (up to 64 characters,
    from the web app, the host or a reviewed import file); that person's own bot can't name
    them, and their calls go through `runtime` instead.
  - **One person key** (the Low on canonical handles, review at 2544d01, and G1 of the review
    at 6bb806f): `bot-pm-ana` may name `ana`, `ana.`, `ana-` or `ana..`, which all spell
    `ana`. Every client's calls are kept under the spelled form (`ana`; `ana-k` for `ana.k`),
    for the caps and in the usage log's `user`, so no client can spread one person over
    several keys, and the runtime's, a team bot's and the PM's own bot's calls for one PM
    count together. The trade-off: a PM whose handle has `.` or `_` (`ana.k`) appears as
    `ana-k` on every usage line. Handles that spell the same (`ana.k`, `ana_k`) would share a
    key, so PM OS refuses to create a handle spelled as an existing user's
    (`docs/toolserver.md`, "Handles that spell the same"; since U3 the database refuses it
    too, migration 0028); rows that already clash share their caps, and the user list warns
    about them. A `403` line's `user` is still the handle as the client sent it (the evidence
    of whom it tried to name), and its `user_key` the spelled form, as every other line's
    `user` (the Low on 403 lines, review at c78b5fb), so a refusal can be matched to that
    person's calls; the start-up warning for it carries both too.
  - **Names that only look like it** (the Low on `bot-pm` names): a client named `bot-pm` or
    `bot-pmana` (no hyphen) would fall under `bot-*` and may name anyone. A name that starts
    with a `from-name` pattern's prefix without its trailing hyphen, but not with the whole
    prefix, gets the untrusted default (never a shorter pattern): its `X-Pmos-User` is
    ignored and its calls are `personal`. The gateway logs a warning at start-up for each such
    token rather than refusing to start, because a team bot may be named so (`pmo` gives
    `bot-pmo`); such a bot's worker then runs untrusted. An exact entry in `clients:` is
    taken as written (list `bot-pmo: {trusted: true}` to trust it on purpose).
- **`deploy/gateway.yaml`** trusts `runtime` (it names the PM its caller verified; cap 2),
  `bot-pm-*` with `user: from-name` (each PM's own bot's worker; cap 3 of 4) and `bot-*` (a
  team bot's worker, such as `bot-docs`, naming the PM whose task it runs; cap 3 of 4), and
  caps `evals` at 1, `personal`. A PM has 3 running slots (`per-user: 3`): one in the
  runtime's pool, one in the team bots' pool, one for their own bot (W1, above). An unlisted
  client holds at most 2 (`per-client: 2`, as before `per-user` went to 3), and a PM may have
  4 calls waiting through each client (`per-user-queue: 4`, unchanged). **The runtime runs one
  call per PM at a time** (finding U2 of the review at c78b5fb): a PM's second routine waits
  for the first, up to `queue-wait-secs` (300 s), and then gets `503` with `Retry-After: 5`;
  since W1 a team bot's call for them no longer takes that slot. **Known residual (S8):** a
  team bot's token can still name any PM, since it serves them all; it holds at most 3 slots,
  one of any one PM's (the team bots' one), and 6 waiting places. The test stack
  (`scripts/claude-test-stack.sh`) writes no `person-pools:`, so it behaves as before W1: it
  gives all its workers one `worker` token, trusted to name anyone, with
  `per-user-share` equal to `per-user` for `worker` and `runtime` (in the config it writes for
  another provider and in `config/gateway.claude-test.yaml`): its eval mode runs every attempt
  as the one user `evals`, one call per lane, so with 4 lanes (`per-user` 4) the 4 lanes still
  have 4 calls in flight beside the judges' 2 (tested by reading the script's config in
  `limits.rs`, `the_test_stacks_eval_lanes_keep_one_call_in_flight_each`).
- **Queue full:** a call that would have to wait gets `503` with `Retry-After: 5` when `max-queue` calls are already waiting (and `429` when its client's or person's share of the queue is used, above). A call that can start at once always starts.
- **Queue deadline:** a call that has waited `limits.queue-wait-secs` (default 300) for a slot
  gives up with the same `503` and leaves the queue. A retry waiting for a slot again gives up
  the same way and returns the upstream's last answer.
- **Readiness** (`GET /readyz`, no token): the probe of the model servers is reused for 5
  seconds (`Timeouts::ready_cache`, `READY_CACHE`), so a flood of unauthenticated requests
  sends at most one round of probes to the upstreams every 5 seconds (the Low on
  unauthenticated probes, review at c78b5fb); callers that arrive while a probe runs wait for
  it. The probe runs in a task of its own (the Low on `/readyz`, review at 52dbae2), so a
  caller that hangs up mid-probe doesn't cancel it: it completes and fills the cache anyway.
  Before, it ran in the caller's request, so a client that connected and hung up over and
  over kept the cache empty and a round of probes in flight (tested in `gateway.rs`,
  `a_caller_that_hangs_up_mid_probe_still_fills_the_readiness_cache`). `running` and
  `waiting` are read afresh on every request. A model server that goes down
  therefore shows as `down` up to 5 seconds late (plus the probe's own timeout). Tested in
  `gateway.rs`, `readiness_reuses_its_probe_for_a_while`; every shipped config's role-and-label
  answer, stub off, in `limits.rs`, `readiness_of_every_shipped_config_reports_roles_by_label`.
- **Hang-ups:** a caller that hangs up while waiting gives its place back. One that hangs up
  while its call runs (streamed or not) cancels it: the gateway drops the upstream request,
  which closes its connection to the model server, and frees the slot at once (tested in
  `crates/gateway/tests/limits.rs`).

## Retrying a busy model server
A model provider can answer `429` ("quota exceeded", a per-minute limit) or `503` for a
moment. The gateway retries these itself, so a brief limit doesn't lose a PM's request
(`crates/gateway/src/retry.rs`).

- **What is retried:** `429`, `503`, and a failure to connect (nothing was sent). A streamed
  request is retried only before its first byte has gone to the caller, which is when such a
  status arrives; a stream that has begun is never repeated. Any other error, `400`-class
  included, is returned at once.
- **How long it waits:** the upstream's `Retry-After` (seconds or an HTTP date) when it sent
  one, otherwise an exponential backoff with jitter (about 1 s, then 2 s).
- **Limits:** `PMOS_GATEWAY_RETRY_MAX_ATTEMPTS` (default `3`, the first try included; `1` turns
  retrying off), `PMOS_GATEWAY_RETRY_BUDGET_SECS` (default `20`, counted from the first try;
  a wait that would pass it is not started, so a `Retry-After` longer than the time left
  gives up at once) and `PMOS_GATEWAY_RETRY_BASE_MS` (default `1000`).
- **Giving up:** the caller gets the upstream's last status, body and `Retry-After`, exactly
  as it would have without retries; for a labelled role, the same status and `Retry-After`
  with a neutral body ("Hiding the model behind a label", below).
- **Quotas that won't recover:** if the error body says so, it fails at once with the same
  error. Detected: OpenAI's `insufficient_quota` and `billing_not_active`, and any body naming
  a per-day or daily quota (Gemini's `...PerDay...` quota ids). Anything unclear is left to
  the budget.
- **Queue:** a call waiting to retry gives its slot back, so retries never hold up other
  callers. It takes a slot again at the same priority, so an interactive call still goes
  before scheduled and personal ones. If the queue is full then, it returns the upstream's
  last answer.
- **Caller header:** every call to a model server carries `X-Pmos-Upstream-Attempts`, the
  number of tries made.
- **Not covered:** the Claude test upstream, and a stream that breaks after it has begun.
- **Two layers:** the harness worker doesn't retry model calls itself. It shows a `429`/`503`
  as "The model is busy" (`harness/src/worker.ts`). An SDK retry in the harness would
  multiply with this one, so keep it off or small.

## How long a model server may take
- **A streamed answer** may go silent for at most 120 seconds between bytes; a server silent
  that long is stuck, and the caller gets a `502`.
- **An answer that isn't streamed** (most of the harness's calls, and every call to the Claude
  test upstream) sends nothing until it is whole, so it gets 600 seconds from the request to
  its last byte instead. Before 2026-11-21 it had the 120-second silence limit too, which cut
  off every non-streamed generation longer than that as a `502` (found in the first eval run,
  on a remote model writing long Confluence drafts).
- Both are `Timeouts` in `crates/gateway/src/app.rs`.
- **A streamed answer's total time** is at most `limits.stream-max-secs` (default 900), from
  its first byte, however steadily it sends: a stream past it is cut off. Before 2026-11-22 a
  stream that kept sending could hold its slot for ever.
- **How large an answer may be:** at most `limits.max-answer-bytes` (default 16 MiB), streamed
  or not, and for the Claude test upstream too. A larger whole answer gets `502` ("larger than
  this gateway accepts"); a larger stream is cut off.
- **A stream cut off** (too long, too large, or broken by the model server) ends with an error
  rather than a clean end, so the caller never takes half an answer for a whole one; its usage
  line says `504` (too long) or `502`.
- **No upstream address reaches a caller:** errors returned to callers name the role, never
  the model server's URL or host (reqwest's errors are stripped of it).

## API
| Endpoint | Auth | Purpose |
| --- | --- | --- |
| `POST /v1/chat/completions` | client token | Chat, streaming or not. `model` must be `pmos/<role>`; a concrete model name gets `400`. |
| `POST /v1/embeddings` | client token | Embeddings for a role that serves them |
| `GET /v1/models` | client token | The role aliases |
| `GET /v1/pmos/workarounds` | client token | The workaround registry |
| `GET /v1/pmos/roles` | client token | The gateway's `class` (`production` or `test`), each role's model id (its `label` when it has one, with `model_sha256` beside it) and upstream kind (`local`, `claude-test` or `remote-test`; never an address), and whether the stub, a testing-only upstream and the workarounds are on. The bot runtime passes it to the benchmark runner (`GET /v1/personas`, client `evals`), which records it in each report (`docs/evals.md`). |
| `GET /healthz` | none | Liveness |
| `GET /readyz` | none | `200` when every model server answers `/models` within 2 seconds; `503` when one doesn't. `upstreams` is keyed by role, each with its upstream's kind label (`local`, `claude-test`, `remote-test`, as in `GET /v1/pmos/roles`) and `ok` or `down`: `{"default": {"upstream": "local", "status": "ok"}, …}`. Labels only, never an address, host name, URL or key (review at 6bb806f: before, the keys were the model servers' base URLs, so this unauthenticated route named their hosts and a remote test upstream's provider). Every answer also has `class` (as served: `test` with the stub on) and `stub` (G2); with the stub on it is always `200`, has no `upstreams`, and asks no model server. `deploy/pmos-admin status` shows a `WARN` line for the gateway when `stub` is true or `class` isn't `production`. Docker's health check uses `/healthz`, so neither changes whether the stack starts. |

**Required header** (slice H4): `X-Pmos-Model-Source` on every chat and embeddings call (above).

**Optional headers:**
- `X-Pmos-User`: a handle (used only from a trusted client);
- `X-Pmos-Priority` (used only from a trusted client);
- `X-Pmos-Run`: logged with the call;
- each response carries `X-Pmos-Model`, the model that answered, `x-pmos-model-source`, and
  (for model-server calls) `X-Pmos-Upstream-Attempts`.

**Errors:** in the usual chat-completions error shape. A model server's own refusal, such as "context length exceeded", is passed through for a role without a label; for a labelled role it is replaced by a neutral one (below). An unreachable server is `502`.

## Hiding the model behind a label
A role may carry `label:` (1 to 64 letters, digits, `.`, `_` and `-`), so that nothing the
gateway reports names the model or its provider (owner, 2026-11-23: no committed file may
identify the hosted model's vendor):

```yaml
roles:
  default: {upstream: remote, model: <the provider's model id>, label: hosted-model-a}
```

With a label set:
- **`model` still goes upstream:** the model server is asked for its real id.
- **Clients see the label:** `GET /v1/pmos/roles` reports it as the role's `model`, and so do
  the `x-pmos-model` header, the `model` field of a whole answer, and the `model` of every
  event of a streamed one (rewritten line by line).
- **Nothing else of the upstream's own** (the label hardening of the review at 52dbae2): an
  upstream may add fields that name the model or the provider (`system_fingerprint`,
  `provider`). For a labelled role, a whole answer and each streamed event keep only the
  fields a client reads (`id`, `object`, `created`, `model`, `choices`, `usage`, `data`;
  `LABELLED_FIELDS` in `app.rs`); an `error` inside a streamed event becomes
  `{"type": "upstream_error", "message": "the model server reported an error mid-answer"}`.
  In a stream, `data: [DONE]`, blank lines and `event:`, `id:` and `retry:` lines pass as they
  came; a comment keeps only its `:` (still a keep-alive, without its text); a `data:` line
  that doesn't parse as a JSON object, a line of no known kind, and an unterminated last line
  that doesn't parse are dropped. Tested with a mock upstream in `labels.rs`,
  `a_labelled_roles_answers_carry_nothing_of_the_upstreams_own`.
- **Errors name nothing** (the spec's point V3, built for labelled roles only): an upstream's
  non-2xx answer (after the gateway's retries, or one not retried) reaches the caller with the
  same status and `Retry-After`, and in place of the upstream's body:

  ```json
  {"error": {"type": "rate_limit", "status": 429, "message": "the model server answered 429 (rate_limit)"}}
  ```

  `type` is coarse: `rate_limit` (`429`), `quota_exhausted` (`429` whose body says the quota
  won't come back soon, as the retry rules detect it), `overloaded` (`503`, `529`), `auth`
  (`401`, `403`), `bad_request` (`400`, `404`, `405`, `409`, `413`, `415`, `422`), else
  `upstream_error`. The callers keep what they act on: the harness worker and the runtime key
  on the status alone ("The model is busy" for `429`/`503`, `harness/src/worker.ts`;
  `crates/bots` treats any non-2xx as unavailable), and the gateway's own retry still reads
  the upstream's real body before it is replaced, so quota errors are retried, or not, as
  before. What a labelled role's caller loses: the upstream's own explanation of a `400`
  ("context length exceeded"). Every testing-only upstream's errors are replaced the same way for
  a labelled role. A role without a label is unchanged. Tested in `labels.rs`,
  `a_labelled_roles_errors_name_nothing_but_keep_what_callers_act_on`, and `app.rs`.
- **The hash of the model id** (`model_sha256`): for a labelled role, `GET /v1/pmos/roles`
  adds the lowercase hex SHA-256 of its real model id, the start-up line for the role logs
  it, and so does every usage line (`model_sha256`, beside the label in `model`), so an
  auditor can tell which model served a past call by hashing the ids the config has held. A
  role without a label has none (its `model` is its id). The hash names nothing by itself,
  but a short model id from a known list of ids can be found by hashing each one; the
  reviewer judged that acceptable ("it reveals nothing" beyond what such a list already says).
- **The logs name the label and the upstream's kind:** the usage log's `model` is the label and
  its `upstream` is the kind (`local`, `claude-test`, `remote-test`) instead of the host; the
  start-up line for the role, the remote-upstream warning, and the retry and "unreachable"
  warnings name the kind instead of the URL.
- **`GET /readyz`** is unchanged: it never named a model or a host.
- **Benchmarks compare the label and the hash:** the benchmark runner records each role's
  label and, for a labelled role, its `model_sha256` in a report's `gateway_models`. For a
  labelled role in the production gateway config, the production role models (`docs/evals.md`)
  are its `label` and the SHA-256 of its `model`, and a run counts only when both match, so a
  model swapped behind an unchanged label restarts the streak rather than keeping old runs
  counting. A report written before the gateway reported the hash (2026-11-23) records the
  label only: it **never matches a labelled production role** (it says "no model hash
  recorded"), still parses, and still matches a production role without a label (which has no
  hash either), as before. A new label for a new model is still good practice.

Without a label nothing changes: the model id is reported and logged as before.

What a label doesn't hide: the gateway's config file itself (the model id and the upstream URL
are in it), the environment the gateway or the test stack runs with, and anything inside the
fields a client reads (an answer's text, a tool call, `usage`) should the model itself write it
there.

## Usage log
One event per call:
- client, user, run, role, model, upstream host (for a role with a `label:`, the label and
  the upstream's kind instead, and `model_sha256`, the hash of its real model id);
- priority, stream or not, status;
- queue and total time, token counts, workarounds applied;
- `attempt` and `attempts`: every try at the model server gets a line, with `retrying: true`
  on those that failed and will be retried. The last line of a call is its final outcome.
- `refused`: on a line for a call refused before any try (`attempts: 0`): a client naming a
  person it may not name (`403`; `user` is the handle it tried), or a client or person with
  its share of the queue used (`429`).
- `refused_count` (the Low on refusal lines, review at 2544d01): so a looping token can't
  flood the JSON-lines file, the service log or the recent list, a client's first refusal of
  a status (`403` or `429`) gets its own line at once; further ones within the next minute are
  only counted, and one summary line, the last of them with `refused_count: <n>`, follows when
  the minute ends (checked every 5 seconds, and at shutdown). So each client writes at most
  about two refusal lines of each status a minute, and every refusal is counted. A summary's
  `user` is the last handle tried; `refused_users` lists the distinct handles the held
  refusals named, first seen first, at most 8 (`REFUSAL_USERS`), and `refused_users_more`, when
  there were more, how many of the held refusals named someone not listed (the Low on refusal
  summaries, review at 6bb806f: before, only the first and last handle of a minute were
  kept). The `403` warning in the service log is written only with a line of its own.

It never holds prompts or answers. Events go to the service log, and with `PMOS_GATEWAY_USAGE_LOG` also to a JSON-lines file for shipping off-box. Under `local` every upstream is one of the model servers on PM OS's host; a remote upstream runs only under `hosted-test`, where the data is fake by rule, so real data sent to an endpoint nobody approved is zero by construction. Each event's `upstream` (the host, or for a labelled role its kind, such as `remote-test`) shows which kind of model server served the call.

## Configuration
| Variable | Default | Notes |
| --- | --- | --- |
| `PMOS_GATEWAY_TOKENS` | required | `name=token,…`, one per caller (the bot runtime, the benchmark runner). Each token is at least 32 characters, and all are distinct. |
| `PMOS_MODEL_SOURCE` | required | `local` or `hosted-test` (slice H4, "Model sources" above); `hosted` is refused until slice H12. No default. |
| `PMOS_GATEWAY_CONFIG` | `config/gateway.yaml` | Roles, upstreams, limits, clients (below) |
| `PMOS_GATEWAY_BIND` | `127.0.0.1:8740` | |
| `PMOS_GATEWAY_USAGE_LOG` | none | JSON-lines usage file |
| `PMOS_GATEWAY_STUB` | off | `1` answers with a deterministic stub model and no model server, for CI and development |
| `PMOS_GATEWAY_WORKAROUNDS` | `on` | `off` for the benchmark run |
| `PMOS_GATEWAY_RETRY_MAX_ATTEMPTS` | `3` | Tries per call, first included (1 to 10) |
| `PMOS_GATEWAY_RETRY_BUDGET_SECS` | `20` | Total time a call may spend retrying (0 to 300) |
| `PMOS_GATEWAY_RETRY_BASE_MS` | `1000` | First backoff when there is no `Retry-After`; doubles each try |
| `PMOS_GATEWAY_CLOUD_TEST` | off | **Testing only.** `1` accepts the Claude test upstream and remote test upstreams (below) |
| `PMOS_GATEWAY_ANTHROPIC_KEY` | none | **Testing only.** The Claude API key for the Claude test upstream |
| `PMOS_GATEWAY_REMOTE_KEY` | none | **Testing only.** The API key for remote test upstreams (`openai+https://…`), sent as `Authorization: Bearer`. Never logged. |

**In the config file** (`limits:` and `clients:`; every setting is optional, and the defaults
bound every client, wait, stream and answer):

| Setting | Default | Notes |
| --- | --- | --- |
| `limits.max-in-flight` | `4` | Calls running at once |
| `limits.per-user` | `2` | Calls running for one (believed) person |
| `limits.max-queue` | `64` | Calls that may wait |
| `limits.per-client` | `per-user` | Calls running for a client `clients:` gives no `max-in-flight` |
| `limits.queue-wait-secs` | `300` | Longest wait for a slot (1 to 3600), then `503` |
| `limits.stream-max-secs` | `900` | Longest streamed answer, first byte to last (1 to 7200) |
| `limits.max-answer-bytes` | `16777216` | Largest answer taken from a model server (1 KiB to 256 MiB) |
| `limits.per-user-queue` | twice `per-user` | Calls one believed person may have waiting |
| `person-pools` | none | Pool name to how many of one person's `per-user` running slots the pool's clients hold together (W1). Given, every trusted `user: any` client names its pool, and the pools' slots plus one for a PM's own bot (with a `from-name` client listed) must fit in `per-user`. Not given: one pool (`user-any`) of `per-user`, less one with a `from-name` client listed (U1) |
| `class` | `test` | `production` only in `deploy/gateway.yaml`: reported in `GET /v1/pmos/roles`, and only such a gateway's eval reports count. A `production` config refuses testing-only upstreams, `local-hosts` and any model server not named `<name>.pmos.internal` at start-up. With `PMOS_GATEWAY_STUB=1` it starts, logs a warning, and reports class `test` (with `stub: true`), so a stub run never counts (below). |
| `clients.<name or prefix*>.trusted` | `false` | Believe `X-Pmos-User` and `X-Pmos-Priority` |
| `clients.<…>.max-in-flight` | `limits.per-client` | This client's cap |
| `clients.<…>.max-queue` | twice its `max-in-flight` | Calls this client may have waiting |
| `clients.<…>.user` | `any` | `from-name` (a pattern, trusted only): it may name only the person its name spells |
| `clients.<…>.per-user-share` | `1` for a trusted `user: any` client; for a `from-name` one what the person pools leave, or `per-user` without them; else `per-user` | How many of one person's `per-user` running slots this client may hold (trusted only): 1 to its person pool's slots for a `user: any` client, 1 to what the pools leave for a `from-name` one with `person-pools:`, else 1 to `per-user` |
| `clients.<…>.person-pool` | none | The person pool (`person-pools:`) of a trusted `user: any` client; required for each such client when `person-pools:` is given, refused otherwise |
| `clients.<…>.priority` | `interactive` if trusted, else `personal` | An untrusted client's every call; a trusted one's calls without the header |

At start-up the gateway logs its class (as served: `test` with the stub on), and each client's
name, trust, caps, priority, `user` rule, `per-user-share`, and its person pool and that
pool's slots (`person_pool`, `person_pool_slots`; never a token), and each role's label and,
for a labelled role, `model_sha256`.

**Upstream forms:**
- `http://<local host>/<base>`: an OpenAI-compatible local model server (vLLM). The only form
  outside testing; a `class: production` gateway takes only `<name>.pmos.internal` hosts
  (below).
- `anthropic+https://api.anthropic.com`: the Claude test upstream (testing only, below).
- `openai+https://<host>/<base path>`: a remote test upstream, any OpenAI-compatible API on the
  internet (testing only, below).

**Allowed upstream hosts** for a `class: test` gateway (`hosted-test`, fake data only; a
`class: production` gateway takes only `<name>.pmos.internal` names, next paragraph):
- loopback and private addresses;
- names without a dot, such as compose services like `vllm`;
- `localhost` and `*.pmos.internal`;
- anything listed under `local-hosts:` in the config.

Anything else stops the gateway at start-up.

**A production gateway's hosts** (review at 2544d01, F3): with `class: production`, every
upstream's host must be a name `<name>.pmos.internal` (lowercase letters, digits and inner
hyphens per label), and `local-hosts` must be empty. An address, a name without a dot or a
`local-hosts` entry, all of which a test gateway accepts, could point at a machine off
PM OS's host (a search domain, a typo, an extra entry) without anyone reviewing it as such.
`.internal` is reserved for private networks, so these names never resolve on the internet.
`deploy/gateway.yaml` names the vLLM services `vllm-default.pmos.internal` and
`vllm-extraction.pmos.internal`, their network aliases in `deploy/compose.yaml` (as every
other service there already has one); before 2026-11-22 it used the bare service names.

**The stub with a production config** (the regression at 2544d01): a first install runs the
deployment's own `deploy/gateway.yaml` (`class: production`) with `PMOS_GATEWAY_STUB=1` until the
model servers are up (`deploy/env.example`; the install runbook's "Turn off the stub" comes
later). The gateway then starts as before, logs a warning, and serves as class `test`:
`GET /v1/pmos/roles` says `class: test` and `stub: true`, so no eval run against it counts.
`GET /readyz` says the same (G2, review at 6bb806f: before, it answered ready with nothing
but two start-up log lines to show a host left on the stub), and `deploy/pmos-admin status`
shows `gateway WARN the STUB model answers every call`, so a host left on the first
install's default is plain to see.
The config's rules (testing-only upstreams refused, the `.pmos.internal` hosts) still apply
with the stub on: the stub never calls an upstream, but the shipped config passes them, so
nothing valid is refused. At 2544d01 the gateway refused to start instead, which stopped CI's
deploy-kit job and a fresh install.

**The Claude test upstream (testing only, decided 2026-11-14):**
- **What it is:** an upstream written `anthropic+https://api.anthropic.com` sends a role's
  calls to the Claude Messages API (`crates/gateway/src/anthropic.rs`), so PM OS can be tried
  before local models are up.
- **When the gateway accepts it:** only with `PMOS_GATEWAY_CLOUD_TEST=1`, which needs
  `PMOS_MODEL_SOURCE=hosted-test` and a `class: test` config (fake data). It then logs a
  warning at start-up that prompts leave the machine.
- **What changes for callers:** nothing. They still send OpenAI-style requests, and the
  gateway translates them:
  - tool calls and results are translated both ways;
  - sampling parameters are dropped;
  - forced tool choice becomes an instruction;
  - `response_format` asks for JSON only;
  - a streamed request gets the whole answer as one event stream;
  - refusals reach the caller as `content_filter`.
- **Limits:** no embeddings.
- **Per-role setting:** `effort:` (low to max) sets `output_config.effort`.
- **Example config:** `config/gateway.claude-test.yaml`. How to run a full test stack with it is in
  [testing-with-claude.md](testing-with-claude.md).

**Remote test upstreams (testing only):**
- **What it is:** an upstream written `openai+https://<host>/<base path>`, such as
  `openai+https://generativelanguage.googleapis.com/v1beta/openai`, sends a role's calls to
  that OpenAI-compatible API, so PM OS can be tried with another provider's model.
- **When the gateway accepts it:** only with `PMOS_GATEWAY_CLOUD_TEST=1` (so only under
  `PMOS_MODEL_SOURCE=hosted-test`, fake data), only over `https`,
  and only with `PMOS_GATEWAY_REMOTE_KEY` set; otherwise it refuses to start. At start-up it
  logs a warning per role that a remote test upstream is on and prompts leave the machine, and
  `GET /readyz` reports `"cloud_test": true`.
- **What changes for callers:** nothing. Requests are forwarded in the OpenAI format, without
  translation, to `<base path>/chat/completions` (and `/embeddings`), streaming included,
  through the same queue, per-user limits and usage log. The key goes as
  `Authorization: Bearer`; the caller's gateway token never goes upstream.
- **Compatibility:** every field the harness sends is forwarded; none had to be removed. One
  thing is added back: such an API may attach `extra_content` to the tool calls it returns
  (Gemini's thought signature) and refuse a later request whose history lacks it, which the
  harness's client drops. The gateway remembers it per tool-call id and puts it back
  (`crates/gateway/src/remote.rs`). Local upstreams never get this.
- **Per-role setting:** none beyond `model`, `label` and `workarounds` (`effort` is Claude
  only). Give each role a `label:` to keep the model id and the provider's host out of reports
  and logs (above); the test stack does.
- **How to run a full test stack with it:** "Another provider" in
  [testing-with-claude.md](testing-with-claude.md).

**Shutdown:** SIGTERM finishes in-flight calls before exiting.
