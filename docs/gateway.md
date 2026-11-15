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
```

- **`model`** is the vLLM server's `--served-model-name`.
- **No caller changes:** the harness and the bots keep asking for `pmos/default`.
- **Re-run the benchmark** (0g) after any swap, as D11 requires.
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
- **Per person:** at most `per-user` of them belong to one person (`X-Pmos-User`).
- **Priority** (`X-Pmos-Priority`): `interactive` (someone waiting in Slack), then `scheduled` (routines), then `personal` (background work). Within a priority it's first come, first served. With no header, a call is interactive.
- **Queue full:** a call that would have to wait gets `503` with `Retry-After: 5` when `max-queue` calls are already waiting. A call that can start at once always starts.
- **Hang-ups:** a caller that hangs up while waiting gives its place back.

## API
| Endpoint | Auth | Purpose |
| --- | --- | --- |
| `POST /v1/chat/completions` | client token | Chat, streaming or not. `model` must be `pmos/<role>`; a concrete model name gets `400`. |
| `POST /v1/embeddings` | client token | Embeddings for a role that serves them |
| `GET /v1/models` | client token | The role aliases |
| `GET /v1/pmos/workarounds` | client token | The workaround registry |
| `GET /healthz` | none | Liveness |
| `GET /readyz` | none | `200` when every model server answers `/models` within 2 seconds; `503` names the one that doesn't |

**Optional headers:**
- `X-Pmos-User`: a handle;
- `X-Pmos-Priority`;
- `X-Pmos-Run`: logged with the call;
- each response carries `X-Pmos-Model`, the model that answered.

**Errors:** in OpenAI's shape. A model server's own refusal, such as "context length exceeded", is passed through; an unreachable server is `502`.

## Usage log
One event per call:
- client, user, run, role, model, upstream host;
- priority, stream or not, status;
- queue and total time, token counts, workarounds applied.

It never holds prompts or answers. Events go to the service log, and with `PMOS_GATEWAY_USAGE_LOG` also to a JSON-lines file for shipping off-box. Because every upstream must be local, "restricted data sent off the Spark" is zero by construction; the upstream host on each event lets anyone check.

## Configuration
| Variable | Default | Notes |
| --- | --- | --- |
| `PMOS_GATEWAY_TOKENS` | required | `name=token,…`, one per caller (the bot runtime, the benchmark runner). Each token is at least 32 characters, and all are distinct. |
| `PMOS_GATEWAY_CONFIG` | `config/gateway.yaml` | Roles, upstreams, limits |
| `PMOS_GATEWAY_BIND` | `127.0.0.1:8740` | |
| `PMOS_GATEWAY_USAGE_LOG` | none | JSON-lines usage file |
| `PMOS_GATEWAY_STUB` | off | `1` answers with a deterministic stub model and no model server, for CI and development |
| `PMOS_GATEWAY_WORKAROUNDS` | `on` | `off` for the benchmark run |
| `PMOS_GATEWAY_CLOUD_TEST` | off | **Testing only.** `1` accepts the Claude test upstream (below) |
| `PMOS_GATEWAY_ANTHROPIC_KEY` | none | **Testing only.** The Claude API key for the Claude test upstream |

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

**Shutdown:** SIGTERM finishes in-flight calls before exiting.
