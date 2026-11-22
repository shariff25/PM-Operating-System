# The model gateway: `pmos-gateway`

Every bot reaches the language model through one OpenAI-compatible endpoint on the Spark.
The gateway does four jobs:
- **Picks the model:** callers name a *role* (`pmos/default`, `pmos/extraction`,
  `pmos/reasoning`), never a model, and the gateway knows which local model serves each role.
- **Keeps prompts on the Spark:** it refuses to start with a model server that isn't local
  (ADM-3).
- **Shares the GPU fairly** (MOD-4).
- **Logs every call:** metadata only, never the text.

```
bot runtime / harness ──► pmos-gateway ──► vLLM (default role: Qwen)
   (ChatOpenAI, model "pmos/default")  └──► vLLM (extraction role: Nemotron)
```

## Swapping a model (D11)
Edit `config/gateway.yaml` and restart the gateway.

```yaml
upstreams:
  qwen: http://127.0.0.1:8001/v1
roles:
  default: {upstream: qwen, model: qwen3.6-35b-a3b, workarounds: [strip-think]}
limits: {max-in-flight: 4, per-user: 2, max-queue: 64}
clients:
  runtime: {trusted: true, max-in-flight: 2}
  bot-*: {trusted: true, max-in-flight: 3}
  evals: {max-in-flight: 1, priority: personal}
```

- **`model`** is the vLLM server's `--served-model-name`.
- **No caller changes:** the harness and the bots keep asking for `pmos/default`.
- **Re-run the benchmark** (0g) after any swap, as D11 requires. The roles' models in
  `deploy/gateway.yaml` are the *production role models*: an eval report counts towards a bar
  only if the models the gateway reported for its run are these (`docs/evals.md`, "Which runs
  count"), so a swap there starts every streak again.
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
  only when the gateway believes the header (below).
- **Per client:** each client token has its own cap, `clients.<name>.max-in-flight` (default
  `limits.per-client`, itself defaulting to `per-user`), whoever its calls name. So one token
  can't hold every slot (review finding N3); in `deploy/gateway.yaml` the `evals` token, which
  the whole eval stack shares, holds at most one (N4).
- **Priority** (`X-Pmos-Priority`): `interactive` (someone waiting in Slack), then `scheduled` (routines), then `personal` (background work). Within a priority it's first come, first served.
- **Which clients are believed:** only a client listed with `trusted: true` has its
  `X-Pmos-User` and `X-Pmos-Priority` used; with no priority header its calls get its
  `priority` (default `interactive`). Every other client, listed or not, is keyed by its own
  name (`client:<name>`, under its own cap only) and always gets its `priority` (default
  `personal`), whatever it sends. Malformed headers are refused (`400`) from every client.
  The usage log's `user` is empty for an untrusted client.
- **Client names:** an exact client name from `PMOS_GATEWAY_TOKENS`, or a pattern ending in
  `*` (`bot-*` covers every bot's worker); an exact name wins, then the longest pattern.
  `deploy/gateway.yaml` trusts `runtime` (it names the PM its caller verified; cap 2) and
  `bot-*` (each worker names the PM whose task it runs; cap 3 of 4), and caps `evals` at 1,
  `personal`. A bot worker's token can therefore still name any PM, but no longer holds every
  slot (S8 is narrowed, not closed).
- **Queue full:** a call that would have to wait gets `503` with `Retry-After: 5` when `max-queue` calls are already waiting. A call that can start at once always starts.
- **Queue deadline:** a call that has waited `limits.queue-wait-secs` (default 300) for a slot
  gives up with the same `503` and leaves the queue. A retry waiting for a slot again gives up
  the same way and returns the upstream's last answer.
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
  as it would have without retries.
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
| `GET /v1/pmos/roles` | client token | Each role's model id and upstream kind (`local`, `claude-test` or `remote-test`; never an address), and whether the stub, a testing-only upstream and the workarounds are on. The benchmark runner records it in each report. |
| `GET /healthz` | none | Liveness |
| `GET /readyz` | none | `200` when every model server answers `/models` within 2 seconds; `503` names the one that doesn't |

**Optional headers:**
- `X-Pmos-User`: a handle (used only from a trusted client);
- `X-Pmos-Priority` (used only from a trusted client);
- `X-Pmos-Run`: logged with the call;
- each response carries `X-Pmos-Model`, the model that answered, and (for model-server
  calls) `X-Pmos-Upstream-Attempts`.

**Errors:** in OpenAI's shape. A model server's own refusal, such as "context length exceeded", is passed through; an unreachable server is `502`.

## Usage log
One event per call:
- client, user, run, role, model, upstream host;
- priority, stream or not, status;
- queue and total time, token counts, workarounds applied;
- `attempt` and `attempts`: every try at the model server gets a line, with `retrying: true`
  on those that failed and will be retried. The last line of a call is its final outcome.

It never holds prompts or answers. Events go to the service log, and with `PMOS_GATEWAY_USAGE_LOG` also to a JSON-lines file for shipping off-box. Because every upstream must be local outside testing, "restricted data sent off the Spark" is zero by construction; the upstream host on each event lets anyone check (a testing-only upstream shows its remote host).

## Configuration
| Variable | Default | Notes |
| --- | --- | --- |
| `PMOS_GATEWAY_TOKENS` | required | `name=token,…`, one per caller (the bot runtime, the benchmark runner). Each token is at least 32 characters, and all are distinct. |
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
| `clients.<name or prefix*>.trusted` | `false` | Believe `X-Pmos-User` and `X-Pmos-Priority` |
| `clients.<…>.max-in-flight` | `limits.per-client` | This client's cap |
| `clients.<…>.priority` | `interactive` if trusted, else `personal` | An untrusted client's every call; a trusted one's calls without the header |

At start-up the gateway logs each client's name, trust, cap and priority (never a token).

**Upstream forms:**
- `http://<local host>/<base>`: an OpenAI-compatible local model server (vLLM). The only form
  outside testing.
- `anthropic+https://api.anthropic.com`: the Claude test upstream (testing only, below).
- `openai+https://<host>/<base path>`: a remote test upstream, any OpenAI-compatible API on the
  internet (testing only, below).

**Allowed upstream hosts:**
- loopback and private addresses;
- names without a dot, such as compose services like `vllm`;
- `localhost` and `*.pmos.internal`;
- anything listed under `local-hosts:` in the config.

Anything else stops the gateway at start-up.

**The Claude test upstream (testing only, decided 2026-11-14):**
- **What it is:** an upstream written `anthropic+https://api.anthropic.com` sends a role's
  calls to the Claude Messages API (`crates/gateway/src/anthropic.rs`), so PM OS can be tried
  before the Spark's local models are up.
- **When the gateway accepts it:** only with `PMOS_GATEWAY_CLOUD_TEST=1`. It then logs a
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
- **When the gateway accepts it:** only with `PMOS_GATEWAY_CLOUD_TEST=1`, only over `https`,
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
- **Per-role setting:** none beyond `model` and `workarounds` (`effort` is Claude only).
- **How to run a full test stack with it:** "Another provider" in
  [testing-with-claude.md](testing-with-claude.md).

**Shutdown:** SIGTERM finishes in-flight calls before exiting.
