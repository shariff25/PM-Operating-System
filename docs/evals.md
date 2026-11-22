# Benchmarks: `evals/` and `pmos-evals`

A task category launches only when the bots finish it end to end (D10). Each category is a
fixed set of real tasks. We rerun it every week and after every model or harness change, and
we tag each failure with the bottleneck that caused it. A category passes when it meets its
pass bar two weeks in a row. The spec is §8 of `docs/bot-runtime-spec.md`; the improvement
loop's additions (repeats, comparisons, the gate, drafts checks, the taxonomy and judges) are
§5 and §6 of `docs/improvement-loop-spec.md`.

```
evals/
  config.yaml                         runner defaults, decline phrases, load-test targets
  taxonomy.yaml                       second-level failure tags under the five bottlenecks
  case-card.md                        the template for turning a real case into a task
  categories/<name>/category.yaml     {name, bot, level?, persona?, pass_bar?, repeats?, description}
  categories/<name>/tasks/<id>.yaml   {id, ask, expect | thread, cluster?, regression?, tags?}
  judges/<name>.yaml                  an LLM judge
  judges/<name>.labels.jsonl          its labels (empty: none yet)
  judges/<name>.calibration.json      written by `pmos-evals judge calibrate`
  reports/<date>-<category>.md        one per run, for people
  reports/<date>-<category>.json      the same run, read by `history`, `compare` and `gate`
```

`pmos-evals` (`crates/evals`) is a client of the bot runtime (`pmos-bots`, §3), signed in as
client `evals`. For each task, `repeats` times, it:
1. creates the task with `POST /v1/tasks` and `source: "eval"`;
2. polls `GET /v1/tasks/{id}` until the task settles;
3. reads its events;
4. for a task with `drafts` checks, reads its drafts (`GET /v1/tasks/{id}/drafts`);
5. grades the result;
6. asks the judges that apply, through the gateway;
7. for a failure, reads the trace and tags the failure.

## Running it

| Setting | Flag | Environment | Default |
| --- | --- | --- | --- |
| Runtime URL | `--bots-url` | `PMOS_BOTS_URL` | `runner.bots_url` (`http://127.0.0.1:8750`) |
| Runtime token, client `evals` | `--token` (avoid: on a command line any account can read it) | `PMOS_EVALS_TOKEN` | required |
| Console token, for traces | `--trace-token` (avoid, as `--token`) | `PMOS_EVALS_TRACE_TOKEN` | none |
| Who the tasks are for | `--as` | `PMOS_EVALS_AS` | `runner.requested_by` (`evals`) |
| Evals directory | `--root` | `PMOS_EVALS_ROOT` | `evals` |
| Seconds one task may take | `--timeout-secs` | | `runner.task_timeout_seconds` (600) |
| Milliseconds between polls | `--poll-ms` | | `runner.poll_ms` (2000) |
| Tasks in flight at once (`run` only) | `--concurrency` | | `runner.concurrency` (1) |
| Runs per task (`run`, `gate`) | `--repeats` | | the category's `repeats` (1) |
| Gateway, for judges | `--gateway-url` | `PMOS_GATEWAY_URL` | none: judges aren't asked (the run's models come from the runtime, below) |
| Gateway token, client `evals` | `--gateway-token` | `PMOS_EVALS_GATEWAY_TOKEN` | none |
| Content root, for `check`, `affected` and `history` | `--content` | | `content` |
| Content root the runtime runs on, for a persona's files and version (`run`, `gate`) | `--content` | `PMOS_EVALS_CONTENT` | `content` |
| Isolated attempts: each lane's runtime (`run`, `gate`) | `--lanes` (comma-separated) | `PMOS_EVALS_LANES` | none: not isolated |
| Isolated attempts: the fresh command | `--fresh-cmd` | `PMOS_EVALS_FRESH_CMD` | none |
| Keep labelling-pack candidates (`run`, `gate`) | `--label-candidates <dir>` | | none |

**Tokens and identity:**
- **The `evals` token.** Add `evals=<token>` to the runtime's `PMOS_BOTS_TOKENS`.
- **The trace token.** Only client `console` may read traces (§3). Pass a console token as
  `--trace-token` so failures are tagged from the trace. Without one, tagging uses the task's
  status, error and events only, and the report says how many failures were tagged that way.
- **Who asks.** The runtime lets only active users who may ask bots (`pm`, `reviewer` or
  `admin`) ask, from the tool server's user database (slice U4a), so the `--as` handle must
  be a user there; the eval stack seeds `evals` into it from a seed file beside its copy of
  the content (`PMOS_USERS_SEED`). The runtime also lets the `evals` token request tasks only as the
  handles in its own `PMOS_EVALS_AS` (default `evals`; anything else is `403`), so a leaked
  benchmark token can't run a PM's bots as them. Add a member for the benchmarks (`evals`,
  as the eval stack does), or, for a run as a real PM, list their handle in the runtime's
  `PMOS_EVALS_AS` for that run only. Eval tasks never name a Slack target (`400`) and send
  no Slack notifications.
- **`bot: personal`.** A category with `bot: personal` sends its tasks to the personal bot of
  whoever they are for: `pm-<requested_by>`, with `.` and `_` as hyphens, as onboarding names
  it. So `--as ana` runs them against `pm-ana`. The report records the resolved bot.
- **The gateway token.** Judges call the gateway as client `evals`: the `evals=<token>` entry
  in the gateway's `PMOS_GATEWAY_TOKENS` (`PMOS_GW_EVALS_TOKEN` on the Spark), with
  `PMOS_GATEWAY_URL` such as `http://127.0.0.1:8740/v1` (`/v1` is added if missing). Without
  both, judges aren't asked and the report says so. Which model served each role is no
  longer asked of this gateway but of the runtime under test (below), so a run without it
  can still count.
  The gateway doesn't believe this client's `X-Pmos-User` or `X-Pmos-Priority`: the `evals`
  token's calls are `personal`, and `deploy/gateway.yaml` lets it have one in flight
  (`docs/gateway.md`, "Sharing the GPU").

**Commands and exit status:**

| Command | What it does | Exit status |
| --- | --- | --- |
| `pmos-evals check [--category docs-qa] [--content content]` | Validates the categories and tasks, `config.yaml`, `taxonomy.yaml`, the judges and their labels, every tag used, and that `drafts` tools are write tools in `content/tools.yaml` | 0 valid, 2 problems (every problem listed) |
| `pmos-evals run --category docs-qa` | Runs the category and writes its report, compared with the previous one | 0 met the bar (or no bar), 1 below it |
| `pmos-evals gate --category docs-qa` | Runs the category (or takes `--candidate <report>`) and compares it with the baseline; prints a markdown summary | 0 passed, 1 failed |
| `pmos-evals compare <report> <report>` | Compares two runs of one category, task by task | 0 |
| `pmos-evals history --category docs-qa` | Has the bar been met two weeks in a row? | 0 yes, 1 no (or no bar) |
| `pmos-evals judge calibrate <name>` | Runs a judge over its labels and writes `<name>.calibration.json` | 0 written, 2 no labels or no gateway |
| `pmos-evals judge pack [<name>] --from <dir> [--per-judge 20]` | Adds a labelling pack, picked from a run's `--label-candidates`, to each judge's labels file, every label empty (see **Labelling a judge**) | 0 |
| `pmos-evals affected [--changed <file>]` | Lists the categories a change affects, from changed paths on stdin | 0 |
| `pmos-evals load --tasks 5` | The load test | 0 if every task finished, 1 if not (`--enforce-targets`: also 1 on a missed latency target) |

An error (no token, a bad file, a report that can't be written) exits 2.

**Other `run` options** (`gate` takes them too):
- `--task <id>`, repeatable, runs only those tasks. Use it to debug a task, never for the
  weekly run.
- `--repeats N` overrides the category's `repeats`.
- `--note "…"` records why the run happened. It is free text that no rule reads; the models
  are recorded from the gateway, so the note needn't name a model or a provider's host.
- `--date YYYY-MM-DD` dates the report (default: today, UTC).
- `--reports-dir` changes where reports are written.
- `--no-judges` skips the judges; the report says they weren't asked.

### Every week
On the Spark, against the live runtime with the gateway as it normally runs:

```sh
export PMOS_EVALS_TOKEN=… PMOS_EVALS_TRACE_TOKEN=… PMOS_EVALS_AS=ana
export PMOS_GATEWAY_URL=http://127.0.0.1:8740/v1 PMOS_EVALS_GATEWAY_TOKEN=…   # for judges
pmos-evals run --category docs-qa --note "weekly"
pmos-evals history --category docs-qa
for c in jira-draft confluence-draft slack-reply pm-craft; do pmos-evals run --category "$c" --note "weekly"; done
```

Then commit `evals/reports/`, both files of each run. The reports are the history; a run that
isn't committed doesn't count.

### After every model or harness change
Run every category once more, the same day, with a note naming the change:

```sh
pmos-evals run --category docs-qa --note "model: qwen3.6-35b-a3b → <new>"
pmos-evals run --category docs-qa --note "harness: <PR or commit>"
```

**With workarounds off (D11).** This run decides whether a model swap needs any workaround,
and whether a workaround can be deleted:
1. Restart the gateway with `PMOS_GATEWAY_WORKAROUNDS=off` (`docs/gateway.md`).
2. Run with the same variable in the runner's environment:
   ```sh
   PMOS_GATEWAY_WORKAROUNDS=off pmos-evals run --category docs-qa --note "D11: workarounds off"
   ```
3. Restart the gateway without the variable.

**What the runner records:**
- It records `PMOS_GATEWAY_WORKAROUNDS` from its own environment, labelled "declared by the
  runner's environment", so step 2 is what marks the report. The gateway's own setting is
  also in `gateway_models.workarounds` (below).
- **The models (`gateway_models`).** Before the run and again after it, the runner asks the
  **runtime under test** which models its gateway serves: the `gateway` field the runtime
  adds to `GET /v1/personas` for client `evals`, which the runtime fills by asking its own
  gateway `GET /v1/pmos/roles` with its own token. With lanes, every lane's runtime is asked,
  and they must all agree. So the models recorded are those of the gateway the runtime and
  its workers were configured with, not of whichever gateway `PMOS_GATEWAY_URL` names in the
  runner's shell (review finding R3, 2026-11-22; before it, the runner asked that gateway
  itself, which need not have been the one the bots used). If both answers agree, the
  report's `gateway_models` holds, per role, the model id the gateway served it with and a
  neutral upstream label (`local`, `claude-test` or `remote-test`), whether the stub or a
  testing-only upstream was on, and the gateway's **class**: `production` only for a gateway
  whose config says `class: production` (`deploy/gateway.yaml`, the Spark's) and that runs
  with the stub off, `test` for any other (a production config with `PMOS_GATEWAY_STUB=1`
  included: it starts and reports `test` with `stub: true`), and for a runtime or gateway too
  old to say (`docs/gateway.md`). Never a URL or
  host, so a committed report names no provider's address. A runtime that can't say, lanes
  that disagree, or a change during the run leave the field out (with a warning), and the
  run counts towards no bar. The report's table shows it as "Models (reported by the
  gateway)", ending with the class. `PMOS_GATEWAY_URL` and `PMOS_EVALS_GATEWAY_TOKEN` are now
  for the judges only.
- **The persona's files (`persona_hash`) and version (`persona_version`).** For a category
  with `persona:`, a hash (`sha256:…`) of every file in the persona's folder
  (`<content>/personas/<persona>/`), its role's folder (`roles/<role>/`) and each skill folder
  the role lists (`skills/<skill>/`), path and bytes, in the content the runtime runs on
  (`--content`, `PMOS_EVALS_CONTENT`); and the `version:` of its `PERSONA.md`, shown in the
  report ("Persona version", "Persona files") but no longer compared. A persona whose files
  can't be hashed (a missing role or skill folder) gives a report that counts for no persona.
- Every `docs-qa` task has `must_not_include: ["<think>"]`. A model that leaks its reasoning
  without `strip-think` fails visibly, tagged `model`.

**Runs and weeks:** a second run on the same day is written as `<date>-<category>-2.md`, so no
run is lost. Every run counts toward its week, so a change that breaks a category fails that
week (see **History** below).

### On the test stack
Before the Spark's eval stack is available, the benchmarks run on this machine's test stack
(`scripts/claude-test-stack.sh`, `docs/testing-with-claude.md`) with a cloud model behind the
gateway. Prompts leave the machine, so it runs on the fake fixtures only.
`PMOS_TEST_EVALS=1` sets it up the way `deploy/compose.evals.yaml` does:
- `evals`, `ben` and `cy` join the seed (`pm` only), so the tool server gives `evals` every
  offered persona (`chief`, `sprout`, `echo`) and the persona categories pass the team check;
- a personal bot `pm-evals`, with `PMOS_TEST_EVALS_WORKERS` workers on the main runtime
  (default 1, at most 8);
- `PMOS_TEST_EVALS_LANES` isolated lanes (default 1, at most 8; `0` for none), each with a
  fresh tool server before every attempt (see **Isolated attempts** below);
- runtime clients `evals` and `console` (the trace reader), `PMOS_EVALS_AS=evals`, and
  assertions off with the development override, as on the eval stack (so it can't be combined
  with `PMOS_TEST_WEB=1`); a gateway client `evals` for the judges;
- the docs tools on the fake docs fixtures, unless `PMOS_DOCS_BASE_URL` is set.

It writes the runner's settings, with this run's fresh tokens, to `$STATE/evals.env`
(mode 0600; the lanes and the fresh command too), each value quoted for the shell (`%q`), so a
`$STATE` path with a space or `$` in it is sourced back as written. In a second shell:

```sh
set -a; . state/claude-test/evals.env; set +a
target/debug/pmos-evals run --category jira-draft --note "test stack"
```

`evals.env` sets everything a run needs: the runtime and trace tokens, `PMOS_EVALS_AS`, the
gateway URL and `evals` token (for the judges), `PMOS_EVALS_CONTENT` (the stack's copy of the
content, for the persona's files) and, with lanes, `PMOS_EVALS_LANES` and
`PMOS_EVALS_FRESH_CMD`. The report records each role's model as the runtime under test
reports its gateway's (each lane's runtime, with lanes), so `--note` needn't name it or the
provider's host. A test-stack report never counts towards a bar: its gateway is a `test`
gateway and a testing-only upstream is on (see **Which runs count**).

**What a real eval run on the test stack sets** (nothing new since 2026-11-22's review
fixes): the Slack tokens `PMOS_SLACK_APP_TOKEN` and `PMOS_SLACK_BOT_TOKEN`,
`PMOS_TEST_SLACK_ID`, `PMOS_TEST_MODEL_URL`, `PMOS_TEST_MODEL` and `PMOS_GATEWAY_REMOTE_KEY`
(in a Claude Code cloud session, `proxy-injected`: the session's proxy puts the real key on
the request), `PMOS_TEST_EVALS=1`, `PMOS_TEST_EVALS_LANES`, optionally
`PMOS_TEST_EVALS_WORKERS` and `PMOS_TEST_STATE`, and the session's proxy and CA variables,
which the services inherit (the list is in the script's header).

The gateway lets one person have two model calls in flight (`limits.per-user`). With another
provider (`PMOS_TEST_MODEL_URL`) and more than two lanes or workers, the script raises that to
one call per lane or worker, whichever is more (and `max-in-flight` to that plus two), so the
lanes run in parallel; watch the provider's rate limits (`429`s in `$STATE/usage.jsonl`).
Its gateway config trusts the `runtime` and the one `worker` token all its workers share
(they name the PM, `evals`), each allowed every slot, and lets the judges' `evals` token have
two calls in flight. With four lanes that is four worker calls (one per lane) plus two judge
calls, the six slots there are. The Claude test config (`config/gateway.claude-test.yaml`)
keeps two calls per person, so more than two lanes wait for each other there.

Since 2026-11-22 the gateway also caps calls *waiting* per client and per person (twice
their in-flight caps by default, `docs/gateway.md`); with the script's settings that is
always at least as many as its workers and judges can have waiting at once, so a run waits as
before and is never refused with `429` for it. The stack keeps one `worker` gateway token for
all its harness workers, trusted to name anyone (unlike the Spark's `bot-pm-<handle>` tokens,
which may name only their PM): a known difference, on a single-machine stack with fresh
tokens per run.

The script starts every service from an empty environment with an explicit allow-list of
what it inherits (the script's header lists it: `PATH`, `HOME`, `TMPDIR`, the locale, the
proxy and CA variables, the Slack tokens by name, the docs site settings for `pmos-docs`, and
the model keys and `PMOS_GATEWAY_WORKAROUNDS` / `PMOS_GATEWAY_RETRY_*` for the gateway), so a
database URL, vault key, address or connector setting exported in the caller's shell reaches
none of them; the lanes' fresh command does the same, and reads its settings file as
`KEY=VALUE` data rather than running it. It refuses to start when a port it needs on 127.0.0.1
is taken, and fails if a service it started has exited.

**Stopping it.** Ctrl-C in its shell, or `pkill -TERM -f scripts/claude-test-stack.sh` from
another (a script started in the background ignores `SIGINT`, so `-INT` may do nothing). The
stop kills every service and the lanes' tool servers, and deletes `evals.env` and the lanes'
settings file. Check with `ps` that nothing is left.

### Isolated attempts
Every attempt asks as `evals`, and the tool server keeps what an attempt leaves: a pending
draft, a memory, a recorded bet. It allows one pending `slack.post_reply` per person and
message, so without isolation one attempt's draft makes every later attempt on that message
(a repeat, another task on the thread, another category) get `already_drafted` and fail as
`model/wrong-draft-count`. E1 found this; nobody decides eval drafts, and no route lets the
runner clear them. The owner chose (2026-11-22) **a fresh tool server for every attempt**, not
a new route: nothing about who may create, approve, reject or release drafts changed.

**How it works.**
- **Lanes.** A lane is one bot runtime with its own docs and `pm-evals` workers and its own tool
  server, all in memory, on the same content, seed and gateway. On the test stack lane `n` has
  its tool server on `127.0.0.1:8710 + 100n` and its runtime on `8750 + 100n`.
- **Before each attempt** the runner takes a free lane and runs the fresh command,
  `<PMOS_EVALS_FRESH_CMD> <n>` (no shell, nothing but the lane number). On the test stack that
  is `$STATE/fresh-toolserver`, which runs `scripts/eval-fresh-toolserver.sh`: it stops the
  lane's tool server (only a `pmos-toolserver` it started), starts a new one with the lane's
  settings (`$STATE/lanes/toolserver.env`, mode 0600) and exits 0 once `/healthz` answers. The
  runtime keeps its user directory snapshot across the restart and picks up the new tool server
  on its next call; each task gets its own tool session, so nothing carries over.
- **One attempt per lane.** The attempt then runs on that lane's runtime, and no other attempt
  uses the lane until it is graded (drafts and trace read). A thread is one attempt: its turns
  share one tool server.
- **Concurrency is the number of lanes.** `--concurrency` is ignored with `--lanes` (the runner
  says so). E1 ran 3 to 4 workers; 4 lanes give the same parallelism. In the smoke test the old
  tool server stopped and the new one listened 0.05 to 0.15 s later, small beside a 30 to 200 s
  attempt, so isolation costs no measurable concurrency.
- **A lane that can't get a fresh tool server** (the command fails or takes over 120 s) runs
  nothing: the attempt fails as `harness/runtime-unreachable`, with the command's last stderr
  lines, and the lane is tried again for the next attempt.
- **The report** says how attempts were kept apart (the **Isolation** row, `isolation` in the
  `.json`), and lists the lanes' runtimes. A report with no isolation says so: its attempts
  shared one tool server, so a pending draft could change a later attempt (E1's are all so).
- **Without lanes** (`PMOS_TEST_EVALS_LANES=0`, or `PMOS_EVALS_LANES` unset) the runner works as
  before, against `--bots-url` with `--concurrency`. Don't use that for categories with Slack
  replies.

**Checked on the test stack (E2, 2026-11-22):** `slack-reply`'s `answer-sso-question` twice in a
row on one lane passed 2 of 2 isolated; the same two attempts without isolation passed 1 of 2,
the second getting `already_drafted`. The tests in `crates/evals/tests/isolation.rs` fail without
the isolation.

**The eval stack (`deploy/compose.evals.yaml`) is not isolated yet.** Its runner runs in a
container with no way to restart the tool server container (it has no Docker socket, and
should not get one), so `pmos-admin evals-run` and `evals-gate` still share one tool server per
run. Until it gets lanes, a weekly or gate run of `slack-reply` or `chief` on the eval stack
reads the same defect E1 found; run those on the test stack with lanes, or give each lane its
own compose project, which this slice did not build.

## Reading a report

`evals/reports/<date>-<category>.md` has these parts.

1. **Verdict:** "Met the bar" or "Below the bar", with passed of total, the pass rate and the
   bar. A category with no `pass_bar` says "Reports only". With repeats, the count is of
   attempts ("50 of 60 attempts passed (83%) over 20 tasks run 3 times each").
   - **Two numbers, two meanings.** The **pass rate** (the average) counts each attempt on its
     own: passes ÷ attempts. The **all-attempts-pass rate** (pass^k) is the share of tasks where
     every attempt passed, the stricter number: what a PM who asks k times sees go right every
     time. With repeats, a line under the verdict gives it ("All attempts passed for 12 of 20
     tasks (60%)"). The bar is judged on the average only, as before.
   - **Lost to the model provider or infrastructure.** Attempts tagged `harness/interrupted`
     (the model endpoint unreachable or busy after the gateway's retries, a restart, the worker
     or sandbox going away) aren't the bot's answer, so when there are any, a line gives their
     count and the pass rate without them. They still count as failures for the bar.
   - A table follows: the bot, the runtime (each lane's, when isolated), who asked, the
     declared workaround setting, the note, the **isolation**, the run's start and end times,
     and the **pass rate (the average over attempts) ± its standard error** (see **Repeats and
     statistics**).
   - With repeats, an **All attempts passed (pass^k)** row.
   - A **Traces** row appears when some failures were tagged without a trace.
2. **Failures by tag:** the top bottleneck first ("Top bottleneck: **tool** (3 of 5
   failures)"). Then each tag's failure count and its tasks, by count, ties in the order model,
   tool, policy, memory, harness. Fix the top row first. A second table counts failures by
   **sub-tag** (`model/missing-citation`, …), the most first.
3. **Judges**, when any apply: each judge, whether it was blocking or advisory and why, and how
   often it said yes, no, or gave no verdict. "Not asked: …" when no gateway was configured or
   `--no-judges` was passed.
4. **Tasks:** with one run per task, one row per task: pass or **FAIL**, the tag (the sub-tag
   when there is one) and the rule that gave it, the final status, and queue and total seconds.
   **Why** lists every failed check. With repeats, one row per task: its passes out of k (in
   bold unless all passed), **All passed** (yes when every attempt passed), the tags its
   failures got with their counts, and why its latest failure failed.
5. **Compared with the previous run:** the paired difference with the category's previous
   report, its 95% interval, and flips (see **Comparing runs**). Absent for a first run.
6. **Failure details:** for each failed attempt ("run 2 of 3" with repeats), the question, the
   runtime task id (open it in the console for the full trace), the tag, its rule and evidence,
   and each failed check.

The `.json` next to it holds the same data, every failed check, tag and judge verdict included,
one line per attempt (`repeat` numbers them from 1). It holds no answer or draft text. Since
E2 it also has a `summary`, computed from the lines when written and never read back:
`attempts`, `passed`, `pass_rate` (the average; the bar uses it), `tasks`,
`tasks_all_attempts_passed`, `all_attempts_pass_rate`, `infra_losses` (attempts tagged
`harness/interrupted`), `pass_rate_excluding_infra` (for information), `pass_bar`, `met_bar`,
and `by_task` (each task's `passes`, `attempts`, `pass_rate`, `all_attempts_passed` and
`infra_losses`). Reports from before have no `summary` and load as before.

**Old reports** (written before repeats) have one line per task and no `repeats`, `cluster`,
sub-tags or judges. They load as `repeats: 1`, each task its own cluster, and work with
`history`, `compare` and `gate` like any other.

### Grading
A task passes only when every check passes:

| Check | Passes when |
| --- | --- |
| Status | `approvals: n > 0`: ends `needs_approval` with exactly n approval items. `approvals: 0`: ends `done` with none. No `approvals`: ends `done` or `needs_approval`. |
| An answer | the result isn't empty |
| `must_cite` | each URL is a link in the answer |
| `must_include` | each text is in the answer |
| `must_not_include` | no such text is in the answer |
| `must_decline` | the answer contains one of `decline_phrases` in `config.yaml` |
| `drafts` | each entry's checks hold for the task's drafts of that tool (below) |

A task the runner stopped waiting for, or couldn't create, fails. `needs_approval` counts as
settled: the bot has finished, and nobody approves eval drafts.

**`must_decline: true`** is for the should-not cases: questions the bot's sources don't
answer, where the right answer says so. The answer passes when it contains any of the
`decline_phrases` in `evals/config.yaml` ("don't answer", "couldn't find", "not documented",
…), compared as text is. A `must_decline` task has no `must_cite`; `check` refuses one that
does, and refuses `must_decline` tasks when `decline_phrases` is empty. Keep the phrases
specific to declining: one a normal answer could contain lets a made-up answer pass. E2 added
phrases with the docs as their subject ("docs don't have", "documentation doesn't include",
…), after E1 failed the correct decline "The public docs don't have this"; the test
`checked_in_decline_phrases_accept_a_plain_decline_and_refuse_an_answer` checks both ways.

**`drafts:`** checks the approval items the task ended with, read from the runtime's
`GET /v1/tasks/{id}/drafts` (`{drafts: [{id, tool, args, status}]}`). One entry per tool:

```yaml
expect:
  approvals: 1
  drafts:
    - tool: jira.create_issue          # a write tool in content/tools.yaml
      count: 1                         # how many drafts of this tool; default: at least one
      require_fields: [project, issue_type, summary]   # present and not empty
      allowed:                         # each named argument must be one of these
        project: [PM]
        issue_type: [Story]
      must_include: ["SAML"]           # on the draft's text
      must_not_include: ["roadmap"]
    - tool: slack.send_message
      count: 0                         # must not be drafted at all
```

- **Every draft** of the tool must meet `require_fields`, `allowed`, `must_include` and
  `must_not_include`; each failure names the draft's id.
- **Not empty** means a non-blank string, a non-empty list or object, or any number or
  boolean.
- **`allowed`** compares a string argument as written (trimmed), a number or boolean as
  written; a missing argument is not allowed.
- **The draft's text** is its top-level string arguments, sorted by key and joined by
  newlines, as the feedback ledger measures edits. It is compared like answer text.
- **`count: 0`** takes no other checks.
- **Unreadable drafts:** if the runtime can't serve them (an older runtime answers 404), the
  task fails with "drafts not readable", tagged `harness`.
- **No draft text is stored:** the report keeps which checks failed, never the arguments.

**Links:**
- **What counts as a link.** Any `http(s)://` URL in the answer: bare, in `<…>`, or a markdown
  link target. Trailing punctuation isn't part of a link.
- **How links are compared.** Loosely, ignoring:
  - the scheme (`http` or `https`) and a leading `www.`;
  - the query string and fragment;
  - a trailing slash;
  - a `.md` extension;
  - letter case.

  A docs portal's `/v1/docs/<slug>` links count as `/docs/<slug>`.

**Text** is compared after normalising both sides the same way:
- lowercase;
- hyphens and dashes become spaces ("SEV-SNP" matches "SEV SNP");
- `*` and `` ` `` are dropped (markdown emphasis and code);
- curly apostrophes become straight ones ("don’t" matches "don't");
- thousands separators between digits are dropped ("1,800" matches "1800");
- a decimal's trailing zeros are dropped ("25.0%" matches "25%", "1.70" matches "1.7"), but
  not in a version or address ("1.0.3", "10.0.0.1");
- runs of whitespace become one space.

**Numbers are whole** (since E2): a text that starts with a digit doesn't match inside a longer
number, and one that ends with a digit doesn't match the start of one. So "25%" is not in
"125%" or "0.25%", "1.7" is not in "1.75" or "11.7", and `must_not_include: ["PM-101"]` is not
set off by "PM-1012". E1 failed "25.0%" against "25%"; these two rules make equal numbers pass
and keep different ones failing (`numbers_compare_as_numbers` in `tests/grading.rs`).

#### Grader changes since E1
E1 (the first eval run, 2026-11-21) was graded before these changes, all in commit `f4d6199`
(2026-11-22, E2). A later run is graded by the new rules, so compare it with E1 with them in
mind: re-grading E1's answers isn't possible (its transcripts weren't kept).
- **New decline phrases** in `evals/config.yaml`: the ones with the docs as subject ("docs
  don't have", "docs do not have", "docs don't include", "docs don't mention", "docs don't
  say" and their "do not" forms, and "documentation doesn't have / include / mention" and
  their "does not" forms). Effect: a `must_decline` task that E1 failed with such a decline
  ("The public docs don't have this") now passes; no task E1 passed can fail by it.
- **A decimal's trailing zeros are dropped** on both sides before comparing ("25.0%" is
  "25%", "1.70" is "1.7"; versions and addresses untouched). Effect: a `must_include` E1
  failed on a number written with trailing zeros (sprout's `funnel-pasted-numbers`, "25.0%")
  now passes, and a `must_not_include` naming such a number now also catches it.
- **Whole-number matching:** a number no longer matches inside a longer one ("25%" isn't in
  "125%" or "0.25%"; "PM-101" isn't in "PM-1012"). Effect: a `must_include` that E1 passed
  only because the number sat inside a longer one now fails, and a `must_not_include` that E1
  failed for the same reason now passes; E1's reports show no task that passed or failed that
  way, but it can't be confirmed without transcripts.
- **The reworded `slack-reply/triage-skips-fyi-and-bots` task:** its ask now names the two
  messages to answer instead of "anything there" (which could mean all of #pm-team, where an
  open question does need a reply). The expectation (which drafts, how many) is unchanged, but
  the ask is not, so its results before and after aren't the same measurement: compare that
  task from E2 on only.
- `slack-reply/triage-drafts-only-what-needs-an-answer` gained a comment only; its ask and
  grading are unchanged.

### Threads
A real PM talks to the bot in a conversation: the bot asks "Who is the user?", the PM answers
"customer", and the bot must read that as the answer. A task with a `thread` tests this. It has
`thread`, an ordered list of turns, in place of `ask` and `expect`:

```yaml
id: thread-one-word-answer-is-an-answer
thread:
  - ask: "Write a PRD for adding a dark mode to the Acme Vault console."
    expect: {approvals: 0, must_include: ["?"]}      # the bot asks first
  - ask: "customer"
    expect: {must_not_include: ["who is the user"]}  # and takes this as the answer
  - ask: "Now draft it."                             # no expect: it only has to settle
```

- **One or the other.** A task has `ask` and `expect`, or `thread`, never both; `check` says
  so. A thread has at least two turns, at least one with an `expect`, and no top-level
  `expect`. Each turn's `expect` is the same as a single task's (every check in **Grading**,
  drafts included) and is validated the same way, with the turn's number in the message.
- **What the runner does.** For each attempt it makes a fresh conversation id
  (`eval-<task id>-r<run>-<time>-<n>`) and creates each turn as its own runtime task with that
  `conversation`, starting a turn when the one before has settled. It polls, reads drafts and
  grades each turn with an `expect`, exactly as it does a single task. The thread passes only
  if every graded turn passes. A turn without an `expect` has to settle and nothing more. A
  graded turn that fails doesn't stop the thread: the turns after it are still informative.
- **Timeouts are per turn:** `--timeout-secs` is how long each turn may take, not the thread.
  A turn that doesn't settle (a timeout, or the runtime failing) ends the thread there: the
  turns after it would be asked without their context, so they are recorded as "not run".
- **Repeats** re-run the whole thread, each with a new conversation id, so one run's answers
  never become the next run's history.
- **Judges** are asked about the last turn only, shown the earlier asks (not the replies) and
  the last ask and answer.
- **The load test** uses a thread task's first turn.
- **In the report,** a thread attempt is one line, so the pass rate, pass^k and the gate count
  it once. Its Why column and its failure details start "turn 2:", and the failure's tag
  reason starts "turn 2 of 3:". A **Threads** section lists every turn of every thread
  attempt as pass, FAIL, not graded or not run, with its status, time and failed checks. In
  the `.json`, each thread line has `turns` and `failed_turn`; single tasks have neither.
- **Tags:** the tag is that of the first failed turn, by the rules in **Failure tags**. A bot
  that loses the thread usually answers the follow-up as a new request, which is a `model`
  failure (`model/forbidden-text` or `model/missing-fact`, from the failed check). Tag the
  task `memory/lost-the-thread` to say what it is there to catch.

**What the runtime must allow.** The runner sends `conversation` on `POST /v1/tasks` and
expects the runtime to give each turn the earlier turns of that conversation as `history`
(`docs/bot-runtime-spec.md`, Conversations). The runtime does: the `evals` client may name a
task's `conversation`, and an eval task that names one gets its earlier turns as history (an
eval task without one starts fresh). The `evals` client's other limits still hold: the task is
for a handle in `PMOS_EVALS_AS`, names no Slack target, and sends no notifications. A runtime
from before this refuses a thread's first turn (`403`, "only the web and Slack clients name a
task's conversation"); the thread then fails at turn 1 tagged `harness/runtime-unreachable`,
and the report's reason says why. The thread tasks in `pm-craft` are not `regression: true`
yet: the gate doesn't fail a change that leaves them failing, and they count in the category's
pass rate like any task. Mark them once they pass.

### Failure tags
Every failure gets exactly one tag. The rules run in this order, and the first match wins.

**Terms in the rules:**
- **"Error"** is the task's `error` field, lowercased, checked only for tasks that ended
  `failed`. A word in a rule matches only at the start of a word: `restart` matches
  "restarts", but `lease` doesn't match "please".
- **"Trace"** is the task's trace, when the runner could read it. Entry kinds are
  `prompt`, `model_reply`, `tool_call`, `tool_result` and `policy_denial`.
- **A tool's name** is the `tool` or `name` field of an entry's body.

| # | Rule | Tag | Sub-tag | Matches |
| --- | --- | --- | --- | --- |
| 1 | `runtime-unreachable` | harness | `harness/runtime-unreachable` | A call to the runtime failed, so the task wasn't created or couldn't be followed. This includes a `403` because `--as` may not use the bot. |
| 2 | `policy-denial` | policy | `policy/denied-in-trace` | The trace has a `policy_denial` entry |
| 3 | `policy-error` | policy | `policy/refused` | The error says `policy`, `denied`, `forbidden`, `not permitted` or `403` |
| 4 | `never-claimed` | harness | `harness/never-claimed` | Timed out while still `queued`: no worker picked it up |
| 5 | `cancelled` | harness | `harness/cancelled` | The task was cancelled; nothing in a benchmark cancels tasks |
| 6 | `interrupted` | harness | `harness/interrupted` | The error says `interrupted`, `restart`, `lease`, `heartbeat`, `worker`, `sandbox`, `gateway`, `model is busy` or `model is still busy`. This covers restarts (D2), the worker and its sandbox, and the model endpoint being unreachable or busy (a `429` or `503` after the gateway's retries). |
| 7 | `silent-timeout` | harness | `harness/silent-timeout` | Timed out while `working`, with no events: the worker never reported progress |
| 8 | `done-without-answer` | harness | `harness/empty-answer` | Ended `done` with an empty result |
| 9 | `drafts-unreadable` | harness | `harness/drafts-unreadable` | The task has `drafts` checks and its drafts couldn't be read |
| 10 | `duplicate-approvals` | harness | `harness/approval-count` | More approval items than `approvals` expects, from a task that ran more than once (`attempts` > 1): the retry drafted them again |
| 11 | `memory-error` | memory | `memory/context-limit` | The error says `memory`, `context length`, `context window` or `maximum context` |
| 12 | `memory-tool-error` | memory | `memory/tool-failed` | The trace has a failed result from a `memory*` tool |
| 13 | `tool-error` | tool | `tool/call-failed` | The trace has a failed tool result: `isError` or `is_error` true, `error` set, or `status` 400 or more |
| 14 | `tool-failure` | tool | `tool/unavailable` | The error says `tool`, `mcp`, `connector` or `unavailable` |
| 15 | `page-not-retrieved` | tool | `tool/page-not-retrieved` | A page to cite is missing, the bot called at least one tool, and no tool result held that page (compared as citations are). The bot searched, but retrieval didn't surface the page. |
| 16 | `draft-wrong-target` | tool | `tool/wrong-jira-project`, `tool/wrong-confluence-space` or `tool/wrong-slack-channel` | A draft goes to the wrong place: its Jira `project`, Confluence `space`, or Slack `channel` or `thread_ts` isn't one of the `allowed` values |
| 17 | `model` | model | by the failed check, below | Anything else. The bot had what it needed and answered wrong or incompletely, answered what it should have declined, cited the wrong page, leaked `<think>`, drafted the wrong number or content, timed out while working with progress, failed with any other error, or never called a tool. |

**A `model` failure's sub-tag** comes from how it failed: `model/failed` when the task ended
`failed`, `model/timed-out` when it timed out while working. Otherwise from its failed checks,
the first of these that applies:

| Failed check | Sub-tag |
| --- | --- |
| `must_decline` | `model/answered-beyond-sources` |
| `must_cite` | `model/missing-citation` |
| `must_not_include` of `<think>` | `model/leaked-reasoning` |
| `must_include` | `model/missing-fact` |
| other `must_not_include` | `model/forbidden-text` |
| `approvals`, or a drafts `count` | `model/wrong-draft-count` |
| drafts `require_fields` | `model/incomplete-draft` |
| drafts `allowed`, not where the draft goes | `model/wrong-draft-value` |
| drafts `must_include` or `must_not_include` | `model/draft-content` |
| status | `model/failed` |
| anything else (no answer) | `model/no-answer` |

**Notes on the rules:**
- **Policy comes first.** A refusal can surface as any kind of failure, and it's the cause
  when it's there.
- **Without a trace,** rules 2, 12, 13 and 15 can't fire. Those failures fall through to the
  error rules or to `model`. That is why the weekly run should pass `--trace-token`.
- **The rules live in code.** They are `RULES` and `SUB_TAGS` in `crates/evals/src/tag.rs`,
  with a test for each.

### Failure taxonomy
`evals/taxonomy.yaml` lists second-level tags under the five bottlenecks, each with a
description that says how to recognise it:

```yaml
model:
  description: The bot had what it needed and got it wrong.
  tags:
    missing-citation: The answer doesn't link a page it had to cite, …
    answered-beyond-sources: Answered a question its sources don't answer, instead of saying so.
tool:
  description: …
  tags:
    wrong-confluence-space: A Confluence draft is in a space the task doesn't allow.
```

- **Written in full** as `<bottleneck>/<name>`, such as `harness/approval-count`.
- **The automatic tagger** emits a sub-tag wherever a deterministic failure maps to one (the
  tables above). Every one it can emit must be listed; a test and `check` enforce that.
- **Owners add sub-tags by pull request,** usually after a 100-trace review
  (`docs/runbooks/quality.md`). Some exist only for people and judges to apply, such as
  `model/unfaithful-to-source`, `model/followed-injection`, `policy/should-not-draft`, and the
  three the pm-craft judges apply (`model/no-recommendation`, `model/unmeasurable-outcome` and
  `model/no-riskiest-assumption`) and the two the level judges apply (`model/no-tradeoff-named`
  and `model/no-business-effect`).
- **Where tags are used:** a task's `tags` (the failure modes it exists to catch), a judge's
  `failure_mode`, and sub-tags in committed reports. `pmos-evals check` validates the taxonomy
  and every one of these.

### History: the two-weeks rule
`pmos-evals history --category <name>` reads every `.json` report for the category:
- **Grouping:** runs are grouped by ISO week, Monday to Sunday.
- **A week meets the bar** only when every run in it does, judged against the category's
  *current* `pass_bar`.
- **The category passes** when the latest week with a run meets the bar, and so does the
  calendar week just before it.
- **Gaps:** a week with no run breaks the streak.

The output gives the verdict, the streak, and each week's runs:

```
docs-qa: met its bar two weeks in a row (bar 80%, streak 2 weeks).
  2026-W46: met   2026-11-12 17/20
  2026-W47: met   2026-11-16 18/20, 2026-11-18 16/20
```

A category with no `pass_bar` reports only: `history` says it has no bar and exits 1, since
it can't meet one. The bar is judged on the pass rate over every attempt.

### Which runs count
Only some reports count towards the two-weeks rule, in `pmos-evals history` and in the
`benchmarked: true` check `pmos lint` makes (review finding N5, 2026-11-22). A report counts
when all of these hold:
- **It recorded the gateway's models** (`gateway_models`, above). Reports without it never
  count, which is every report before 2026-11-22 (the E1 runs, whose model was only in the
  free-text note).
- **From a production gateway:** `gateway_models.class` is `production`, which only a gateway
  run with `class: production` (`deploy/gateway.yaml`) and the stub off reports, and which
  comes from the runtime under test (review finding R3). With the stub on, such a gateway
  reports `test` (the fix to the deploy-kit regression at 2544d01). A test stack, a developer's gateway or one too old
  to say is `test`, and never counts, even on the production role models' names.
- **They are the production role models**, role for role: the same roles, each with the same
  model id, as the `model` of each role in the production gateway config. That file is named
  by `benchmark.production_gateway` in `evals/config.yaml`, relative to `evals/`; it is
  `../deploy/gateway.yaml`, the config the Spark's gateway runs. A run on any other model,
  such as a test stack's remote model, doesn't count, whatever its note says. Changing a model
  there (a model swap) means every streak starts again.
- **No stub and no testing-only upstream:** `stub` and `cloud_test` false, every upstream
  `local`.
- **For a persona's category, the same persona files:** the report's `persona_hash` is the
  hash of the persona's, its role's and its skills' files now (`persona_hash`, above). The
  persona's name and the `role` its `PERSONA.md` names must be plain names (lowercase letters,
  digits and single hyphens) before either is joined into a path; anything else (`../x`, a
  path) can't be hashed, so that persona's runs count for nothing. Any
  change to them (the persona's file, its role's instructions, one of its skills) restarts its
  streak, whether or not it bumps `version:`, so a changed bot isn't shown as benchmarked on
  the old one's runs. (Before 2026-11-22 the version string was compared, so a change that
  didn't bump it kept the old runs counting; reports from then have no hash and count for no
  persona. A date window was the alternative; a hash ties a count to what was measured and
  keeps the lint's answer the same from one day to the next. A change outside those folders,
  such as `content/master/AGENTS.md` or the harness, isn't in the hash; the eval gate measures
  those.)

`history` lists the reports that don't count, each with the reason ("its models aren't the
production role models (default: `x`, production `y`)"). `pmos-evals check` reads the
production role models and prints them, and fails if they can't be read (with no production
models, no run counts), if a persona a category names has no `version`, or if its
files can't be hashed.

`evals/`, `deploy/gateway.yaml` and `crates/evals/` (the runner and these rules) are in the
CODEOWNERS lines of `policies/README.md`, so a change to a report, a bar, the production role
models or the rules that count them needs a second platform owner.

### Repeats and statistics
A model's answers vary from run to run, so one run of 20 tasks can't tell a real drop from
noise. `repeats: k` in `category.yaml` (default 1; docs-qa uses 3), or `--repeats k`, runs each
task k times. The first run of every task goes before any second run.

**What the report adds:**
- **each task's passes out of k;**
- **the pass rate** over every attempt: passes ÷ (tasks × k);
- **pass^k:** the share of tasks that passed all k runs, the stricter number: a task that
  passes 2 runs in 3 is a coin a PM will see land wrong;
- **the standard error of the pass rate, clustered.**

**The clustered standard error.** Attempts of one task aren't independent, and neither are
tasks answered from one page. Each task belongs to a cluster: its `cluster` field, else its
first `must_cite` URL, else its id. So all attempts of a task share a cluster. With `n`
attempts `y` (1 pass, 0 fail), mean `ȳ`, and `G` clusters, where `S_g` is the sum of `y − ȳ`
over cluster `g`'s attempts, the standard cluster-robust formula for a mean is:

```
SE = sqrt( G / (G − 1) · Σ_g S_g² ) / n
```

The `G / (G − 1)` factor is the usual small-sample correction. When every cluster holds a
single attempt (one run, nothing shared), or there is only one cluster, the report uses the
plain binomial `sqrt(ȳ (1 − ȳ) / n)` instead, and says which it used. Read the pass rate as
roughly "± 2 standard errors".

### Comparing runs
Every `run` compares itself with the category's previous report in the reports directory, and
`pmos-evals compare <baseline> <candidate>` does the same for any two reports (`.json`, or the
`.md` next to it). Both show:
- **the paired difference:** for each task in both runs, `d = candidate pass rate − baseline
  pass rate`, each over its own repeats, so runs with different k compare;
- **a 95% interval for the mean paired difference:** `d̄ ± 1.96 · s_d / √m`, with `s_d` the
  sample standard deviation of the `m` differences. It needs at least two tasks in common;
- **flips:** a task that passed every repeat and now fails every one ("flipped to failing"), or
  the reverse ("flipped to passing");
- tasks that are new, or no longer run, and a table of every task whose rate changed.

**The interval is a normal approximation.** With fewer than about 30 tasks the true interval
is wider than it says (a t distribution would be), so treat one that only just misses 0 with
care, and rerun. Repeats help more than anything: they shrink each task's noise.

### The gate
`pmos-evals gate --category <c> [--baseline <report> | --baseline-reports <dir> |
--baseline-ref <ref>] [--candidate <report>]` is the check a change must pass before it reaches
PMs. It runs the category (with the `run` options), or takes `--candidate <report>` to gate a
run already made, and compares it with a baseline.

**The baseline** is, by default, the category's latest report on `main`: the latest `.json` for
the category in the reports directory as it is at the git ref `--baseline-ref` (default
`origin/main`, or `PMOS_EVALS_BASELINE_REF`). `--baseline <report>` names one;
`--baseline-reports <dir>` takes the latest in a directory, such as the base branch's
`evals/reports` unpacked with `git archive`. With no baseline report (a new category), there is
nothing to compare and the gate passes, saying so.

**It fails (exit 1) when any of these hold:**
1. **a `regression: true` task flips to failing:** it passed every repeat in the baseline and
   fails every one now;
2. **the pass rate fell beyond noise:** the 95% interval for the mean paired difference lies
   wholly below 0;
3. **a deterministic check that passed in the baseline now fails, on any task.** A check (each
   `must_cite` URL, `must_include` text, `must_decline`, each drafts check, the status, the
   approval count) passed in the baseline when it failed in none of the task's baseline
   attempts, and those attempts were graded in full (not cut short by a runtime error, a
   timeout or no answer). It now fails when it fails in every candidate attempt. Runtime
   errors and timeouts aren't checks: they show in the paired difference;
4. **a blocking judge fails a task it passed in the baseline:** "yes" in every baseline
   attempt, "no" in every candidate attempt. Advisory judges never fail the gate.

Otherwise it exits 0; an error (no token, an unreadable baseline, a report for another
category) exits 2.

**Output:** a markdown summary on stdout, ready for a PR comment or a job summary: the verdict,
both runs' pass rate ± standard error and pass^k, the bar (for information; the bar is not a
gate condition), each failure reason, the comparison, and the judges. The candidate's report is
written to the reports directory as by `run`, and its path goes to stderr.

**Which categories a change affects:** `pmos-evals affected` reads changed paths (one per line,
relative to the repository root) on stdin or from `--changed <file>`, and prints the affected
categories, one per line:
- a change under `evals/`, or to `deploy/gateway.yaml`, affects every category;
- otherwise a category is affected when the change is under its bot's directory
  (`content/bots/<bot>/`), its role's (`content/roles/<role>/`), or a directory of a skill the
  role runs (`content/skills/<skill>/`). For `bot: personal` the bot is `pm-<handle>` for the
  `--as` handle (default `runner.requested_by`); with no such `BOT.md`, its role is
  `pm-assistant`, the role every personal bot is made from.

In CI, `deploy/pmos-admin evals-gate` puts these together (see **CI** below).

### LLM judges
Some failures can't be checked deterministically, such as an answer that cites the right page
but says something the page doesn't. A judge is one yes-or-no question about an answer or
draft, asked of a gateway model, in `evals/judges/<name>.yaml`:

```yaml
name: faithful-to-cited-page          # matches the file name
failure_mode: model/unfaithful-to-source   # a taxonomy sub-tag: what a "no" means
question: >-                           # phrased so that "yes" means the output passes
  Is every factual claim in the output supported by the tool results in the input? …
alias: pmos/extraction                 # the gateway alias; this is the default
applies_to: [docs-qa]                  # the categories it judges
labels: faithful-to-cited-page.labels.jsonl   # relative to evals/judges/
needs_sources: true                    # optional: skip attempts whose trace has no tool results
```

- **A different model family.** The default alias, `pmos/extraction`, is served by a different
  model from `pmos/default`, which drafts (`docs/gateway.md`), so a judge doesn't grade its own
  family's work.
- **What the judge sees.** The input is the request, then what the bot read: the tool results
  from the trace, when the runner can read it (`--trace-token`), cut to 6,000 characters each
  and 24,000 in all. The output is the answer, then each draft's tool and text. It is asked
  through `POST /v1/chat/completions` at temperature 0 for exactly one word, yes or no, with
  `X-Pmos-Priority: personal`.
- **Unknown is allowed** (since E2). The judge is told to answer yes, no, or unknown, and to say
  unknown only when what it is shown is not enough to decide, rather than guess.
- **Parsing the reply.** Reasoning in `<think>…</think>` is dropped. If the first word is yes,
  no or unknown, that's the reply; otherwise a reply that says only one of them as a word is
  that. Anything else is no verdict, and so is a call that fails; neither counts either way.
  Unknown is no verdict too, but the report counts it in its own **Unknown** column (and
  `unknown: true` on the verdict in the `.json`).
- **Judges never change a task's pass or fail.** Their verdicts go into the report next to the
  checks, and a blocking judge counts only in the gate.

**Labels** are a JSON-lines file of `{"input": …, "output": …, "label": …}`, where `label` is a
person's answer to the question: `true` (yes, a pass), `false` (no), `"unknown"` (they couldn't
tell from what is shown), or `null` (a pack line still waiting for a person). The `label` key
is required. A line may also carry `from` (which task and run it came from) and `judge_said`
(what the judge said in that run); calibration reads neither. They are paraphrased like case
cards: no Slack text. **No labels yet** is an empty file: commit the judge with an empty
`<name>.labels.jsonl`. `check` accepts it and calls the judge advisory, and `judge calibrate`
refuses to run until a person has answered at least one line yes or no. A missing file is an
error. `check` prints, per judge, the yes-or-no labels and how many are unknown or waiting.

### Labelling a judge
Labels must come from a person; nothing in PM OS fills them in.
1. **Collect candidates** in a run on the test stack (fake fixtures only):
   `pmos-evals run --category pm-craft --label-candidates <dir outside evals/reports> …`.
   For every attempt a judge answered, it keeps what the judge saw (with every Slack tool
   result left out), the output, and what the judge said, with `label: null`, in
   `<dir>/<date>-<category>-<judge>.jsonl`. These files hold answer text: never commit them.
2. **Build the pack:** `pmos-evals judge pack --from <dir> --per-judge 20` appends up to 20
   lines to each judge's `<name>.labels.jsonl`, taking in turn one the judge said no to, one it
   said yes to and one it said unknown to, so the sample mixes verdicts; lines already in the
   file are skipped. Read every line before committing: no real Slack text, customer data or
   token may be in one.
3. **A person labels** each `null`, following `evals/judges/LABELLING.md`: `true`, `false` or
   `"unknown"`, deciding from the input and output alone, without looking at `judge_said`.
4. **Calibrate** with the gateway: `pmos-evals judge calibrate <name>`, and commit the labels
   and `<name>.calibration.json`. Unknown and unlabelled lines are left out of `n` and the
   rates (and aren't sent to the judge); the output says how many.

Twenty per judge is a first look, not a calibration: a judge blocks only at 100 labels or more.
`faithful-to-cited-page` judges `docs-qa`, whose 4 tasks give at most 12 candidates a run, so
its pack takes more than one run. E1's transcripts could not be used: the runtime kept them in
memory, and reports hold no answer text, so E2 built the tools and the pack waits for the next
run.

**Calibration.** `pmos-evals judge calibrate <name>` asks the judge about every label and
writes `evals/judges/<name>.calibration.json`:

```json
{"judge": "faithful-to-cited-page", "date": "2026-12-01", "n": 120, "positives": 60,
 "negatives": 60, "tpr": 0.9, "tnr": 0.95, "unparsed": 1, "alias": "pmos/extraction",
 "question_sha256": "…"}
```

- **TPR** (true-positive rate) is how often the judge says yes when the label is yes; **TNR**
  how often it says no when the label is no. A reply with no verdict, unknown included, counts
  as wrong. `n` counts only labels a person answered yes or no; the file also records
  `judge_unknown`, `labels_unknown` and `labels_pending`.
- **`question_sha256`** is the sha256 of the question text, a newline, and the alias.
- Commit the calibration file with the labels.

**Blocking or advisory.** A judge **blocks** only when all of these hold; otherwise it is
**advisory**, reported and never failing:
- its calibration has n ≥ 100 labels;
- TPR ≥ 0.8 and TNR ≥ 0.8;
- its current question and alias are the ones calibrated: the hash and the alias match.
  Rewording the question, or moving it to another alias, makes it advisory until it is
  calibrated again.

`check` prints each judge's label count and status. The report records whether each judge was
blocking when the run asked it.

**Shipped,** each with no labels yet, so each is advisory:

| Judge | Applies to | A "no" means |
| --- | --- | --- |
| `faithful-to-cited-page` | `docs-qa` | `model/unfaithful-to-source`: the answer says something the pages it read don't |
| `recommendation-first` | `pm-craft` | `model/no-recommendation`: the draft doesn't open with one recommendation |
| `metric-with-guardrail` | `pm-craft` | `model/unmeasurable-outcome`: no metric with a number, a date and a method, or no guardrail |
| `names-riskiest-assumption` | `pm-craft` | `model/no-riskiest-assumption`: no riskiest assumption, or no way to test it |
| `says-what-stops` | `staff-craft` | `model/no-tradeoff-named`: the draft names nothing that stops or goes unfunded, or the teams that give it up |
| `business-effect-as-range` | `principal-craft` | `model/no-business-effect`: no business effect, one number with no range or assumptions, or nothing on what is hard to reverse |

### Load test
`pmos-evals load --tasks 5 [--category docs-qa]`:
- **Sends N tasks at once**, taking the category's asks in turn, and waits for all of them.
- **Reports** p50 and p90 of queue time and total time against the targets in
  `evals/config.yaml`. It doesn't write a report file; redirect stdout to keep one.

**Queue time** runs from `created_at` to when a worker took the task. That moment is the
earlier of:
- the task's first event;
- the first poll that saw it leave `queued`.

So queue time can be over-counted by up to one poll interval when a task has no events.

**Total time** runs from `created_at` to `updated_at` once the task settles.

**Percentiles** are nearest-rank: p90 of 5 tasks is the slowest.

**Targets:** each one in `config.yaml` carries its `source`.

| Target | Value | Source |
| --- | --- | --- |
| p90 queue | 120 s | `docs/implementation-plan.md`, Phase 3: "a second Spark if p90 queue time passes 2 minutes" |
| p50 queue | 30 s | default |
| p50 total | 60 s | default |
| p90 total | 180 s | default |

- **No PRD targets in the repo.** The repo doesn't contain the PRD's latency targets, so the
  last three are labelled defaults. Replace them with the PRD numbers when they're added.
- **Latency is not a priority (decided 2026-11-14).** The targets are reported, and a miss is
  shown, but it doesn't fail the test or gate Gate 1.
- **Failing the test.** It fails if any task doesn't finish: not created, timed out, failed or
  cancelled. With `--enforce-targets` it also fails on a missed target.

## Adding a category

1. **Pick the bot and the job.** Use real tasks, the way a PM would phrase them, each with an
   answer you can check against a source. Twenty is the norm.
2. **Create `evals/categories/<name>/category.yaml`:**
   ```yaml
   name: <name>          # must match the directory; lowercase letters, digits, hyphens
   bot: <bot>            # a bot in content/bots/, or `personal` for pm-<requested_by>
   pass_bar: 0.8         # optional: fraction of attempts that must pass, 0 to 1;
                         # leave it out until the tasks are real, and the category reports only
   repeats: 3            # optional: runs per task (default 1)
   persona: chief        # optional, with `bot: personal` only: the marketplace bot every
                         # task asks (sent as the task's `persona`); `pmos-evals check` refuses
                         # a key that isn't an offered persona in --content; `nudge` sends none
   description: What the category covers and where its answers come from.
   ```
3. **Write one file per task, `tasks/<id>.yaml`.** The `id` must match the file name. Real
   cases come from case cards (`evals/case-card.md`).
   ```yaml
   id: vault-saml-sso
   ask: "Does Acme Vault support single sign-on with SAML?"
   expect:
     must_cite: [https://docs.example.com/docs/vault-saml-sso]
     must_include: ["SAML 2.0"]
     must_not_include: ["<think>"]
     approvals: 0
   cluster: vault-sso    # optional; default: the first must_cite URL, else the id
   regression: true      # optional: the gate fails if it flips back to failing
   tags: [model/missing-fact]   # optional: failure modes it exists to catch
   ```
   - `must_cite`: absolute URLs, written as the bot cites them. The Docs bot cites the page
     URL without `.md`.
   - `must_include`: a few short, distinctive facts that are actually in the source: numbers,
     names, endpoints. Avoid phrases a correct answer could paraphrase.
   - `must_not_include`: text a correct answer never contains.
   - `approvals`: how many drafts the task should end with. Use 0 for read-only bots, and n
     for tasks that should draft n writes for approval.
   - `must_decline: true`: the sources don't answer it, and the bot must say so. No
     `must_cite`.
   - `drafts`: checks on the drafts, one entry per tool (see **Grading**).
   - `thread`: in place of `ask` and `expect`, a conversation of turns, each `{ask, expect?}`
     (see **Threads**).
   - `cluster`: give tasks that share a source one cluster, so the standard error counts them
     as related.
   - `regression`: mark a task that broke once and was fixed.
   - `tags`: taxonomy sub-tags; `check` validates them.
   - `expect` needs at least one check. Unknown fields are refused.
4. **Validate.** Run `pmos-evals check --category <name>`, and fix every problem it lists.
   The test `the_checked_in_evals_are_valid` also runs this in CI.
5. **Run it.** Run it once against the live runtime and read the failures. A failure caused
   by the task itself, such as a term that's paraphrased in every good answer, is fixed in the
   task. Don't lower the bar.
6. **Launch it.** Add it to the weekly run, and to the runs after model and harness changes.
   It launches once `history` says the bar was met two weeks in a row.

### Categories and bars
Every category has a `pass_bar` and `repeats`. Each bar is a starting value: the docs give a
number only as the 80% example, so tighten each one after the category's first valid run.

| Category | Bot | Tasks | `pass_bar` | `repeats` | Source of the numbers |
| --- | --- | --- | --- | --- | --- |
| `docs-qa` | `docs` | 4 | 0.8 | 3 | 80% example (`docs/evals.md`, "Adding a category" and `docs-qa`); pass^3 at the Phase 1 gate (`docs/research/self-improvement-loops.md`) |
| `confluence-draft` | `personal` | 3 | 0.8 | 3 | Starting value; pass^3 as above |
| `jira-draft` | `personal` | 4 | 0.8 | 3 | Starting value; pass^3 as above |
| `slack-reply` | `personal` | 9 | 0.8 | 3 | Starting value; reads below the bar until a skill declares `slack.read_thread` and `slack.post_reply` |
| `pm-craft` | `personal` | 23 | 0.8 | 3 | Starting value; pass^3 as above |
| `staff-craft` | `personal` (`level: staff`) | 8 | 0.8 | 3 | Starting value; pass^3 as above |
| `principal-craft` | `personal` (`level: principal`) | 8 | 0.8 | 3 | Starting value; pass^3 as above |
| `chief` | `personal` (`persona: chief`) | 7 | 0.7 | 3 | Starting value (`docs/bot-marketplace-spec.md` §10); no run yet |
| `sprout` | `personal` (`persona: sprout`) | 8 | 0.7 | 3 | Starting value (§10); no run yet |
| `echo` | `personal` (`persona: echo`) | 7 | 0.7 | 3 | Starting value (§10); no run yet |

### `docs-qa`
A template category for the Docs bot. PM OS can't ship real questions, because every team's
docs site is different (`PMOS_DOCS_BASE_URL`, `docs/docs-server.md`). So a team writes its own
questions against its own docs; `evals/categories/docs-qa/README.md` walks through it. Each
task runs three times (`repeats: 3`). Its bar is 80% of attempts (`pass_bar: 0.8`), a starting value to tighten once your team
writes its own questions and has a first valid run.

**The example tasks** ask about Acme Vault, a fictional product, and are answered by the fake
docs fixtures (`https://docs.example.com/docs/vault-saml-sso` and `…/vault-key-rotation`), so
the category runs and passes its checks before you point PM OS at your docs. Replace them
with your own.

**Writing your own:**
- **Pages:** read your site's `llms.txt`, then the `.md` version of each page you use, at
  most one request per second.
- **Coverage:** spread the twenty questions across your products (the ids in your products
  file) and your API reference, if you index one.
- **Checked against the page:** quote what the page says in a comment on each task, and check
  every `must_include` term against that page with the normalisation above.
- **When your docs change a page:** fix the task from the new page, and note it in the
  commit.

**The should-not cases** (`decline-*`, `must_decline: true`) ask for what public product docs
never hold: internal roadmap dates, a customer's own deployment, and one customer's deal terms
("Example Corp" is a placeholder). They were chosen for that reason, not from a portal search,
so confirm each with `docs.search` on the first live run and replace any that a page turns out
to answer. The judge `faithful-to-cited-page` applies to the category, advisory until it has
labels.

### The seed categories: `jira-draft`, `confluence-draft`, `slack-reply`
Three tasks for `confluence-draft`, four for `jira-draft` (which also checks that an issue already in Jira is
not recreated) and nine for `slack-reply` (the seed thread plus the triage and tagged tasks) for `bot: personal`, against the fake connectors' fixtures
(`crates/toolserver/src/connectors/fake.rs`): Jira project PM (epic PM-101, story PM-102, bug
PM-103 with a planted prompt injection), Confluence space PM (pages 9001 and 9002), and the
`#field-questions` thread (channel C0FIELD, ts 1731500000.000100). Every task has `drafts:`
checks; two (tagged `model/followed-injection`) check the bot drafts nothing the injected ticket
asks for.

- **Marked "seed: replace with case cards".** Replace them with real cases as case cards
  come in.
- **`pass_bar: 0.8` and `repeats: 3`:** starting values. The docs give no launch number for
  these categories, so tighten each bar after the first valid run against real case cards.
- **`slack-reply` measures a gap.** The `pm-assistant` role's skills don't declare
  `slack.read_thread` or `slack.post_reply` yet, so its tasks fail (as
  `model/wrong-draft-count`) until a skill does.
- **Running them** needs the stack on the fake connectors and a member whose personal bot
  exists (`--as <handle>` runs against `pm-<handle>`).

### `pm-craft`
The senior-PM bar (`docs/pm-craft.md`; the spec is §6 of `docs/pm-craft-spec.md`). Twenty-three
seed tasks for `bot: personal`, on the same fake fixtures as the seed categories plus the fake
docs pages. Like them, it is marked "seed: replace with case cards", has the starting bar
(`pass_bar: 0.8`) and runs each task three times (`repeats: 3`).

| Tasks | The ask | What is checked |
| --- | --- | --- |
| `structure-<skill>`, one per skill that drafts a page (18) | "Draft anyway, no questions: …" for a document in the PM space | One `confluence.create_draft` draft in space PM, with `space`, `title` and `body`; its text has the skill's title prefix (`Problem:`, `Strategy:`, `PRD:`, …) and three or more of the template's headings; no `<think>`, `TODO` or `<!--` left in it |
| `product-sense-asks-before-drafting` | A PRD for a solution with no user or problem, and no "draft anyway" | No draft (`approvals: 0`, `count: 0`), and the answer asks a question (`?`) |
| `decision-recommends-among-options` | A decision doc between two options | The draft has "Recommendation" and "Options" |
| `numbers-sample-size-from-tool` | An experiment plan for a 10% baseline and a 10% relative lift, with the defaults | The draft has the per-arm sample size `stats.sample_size` returns |
| `ai-spec-failure-modes-and-evaluation` | An AI feature spec | The draft has "Failure modes" and "Evaluation" |
| `review-prd-by-severity` | A review of fixture page 9002 | No draft or page update; the answer names a "blocker" and ends with the "three changes" that matter most |
| `thread-*` (3) | A conversation of two turns (see **Threads**): a one-word answer to the bot's own question; "generate the content" after a request about page 9001; a follow-up that changes one detail of a Jira story | The second turn continues the first: no repeated question, the draft is of page 9001, the story is a Task and keeps project, epic and subject. Not `regression: true` until they pass |

- **The headings are the contract.** Each `must_include` heading is written exactly as §1 of
  `docs/pm-craft-spec.md` lists it, and as the template in `content/templates/` has it. Change
  a template's heading and its task in the same pull request.
- **Structure, not judgment.** A draft with every heading and a weak argument passes. The
  three judges (`recommendation-first`, `metric-with-guardrail`, `names-riskiest-assumption`)
  read for the argument. They are advisory until calibrated, so today they are reported and
  nothing more.
- **A judge answers yes when there is nothing to judge.** The judges see every task in the
  category, the ones with no draft too, so each question says to answer yes when the output
  holds no draft. `metric-with-guardrail` also answers yes for a draft that proposes nothing
  to build, change or test, such as a call synthesis.
- **The sample-size task** expects the number from the spec's formula (14,749 per arm). The
  check drops thousands separators, so "14,749" and "14749" both match. If the tool's formula
  or rounding changes, change the task with it.
- **Clusters.** Tasks that exercise one skill share a `cluster` (`decision-doc`,
  `experiment-design`, `ai-feature-spec`, `prd-draft`), so the standard error treats them as
  related.
- **The gate.** `pmos-evals affected` lists `pm-craft` for a change under `evals/`, the
  `pm-assistant` role or any of its skills. A change to only `content/playbooks/`,
  `content/templates/`, `content/reviewers/` or `content/context/exemplars/` doesn't trigger
  the gate, though every pm-craft skill reads them. Run the category by hand after one
  (`deploy/pmos-admin evals-run --category pm-craft --note "…"`).
- **Running it** needs the stack on the fake connectors, the `stats.*` tools in the tool
  server, and a member whose personal bot runs the craft skills.

### `staff-craft` and `principal-craft`
The Staff and Principal bars (`docs/levels.md`; the spec is §6 of
`docs/staff-principal-spec.md`). Eight seed tasks each, for `bot: personal`, on the same fake
fixtures as `pm-craft` plus the rollup's. Like the others they are marked "seed: replace with
case cards", have the starting bar (`pass_bar: 0.8`) and run each task three times (`repeats: 3`).

- **`level` in `category.yaml`** is optional: `senior`, `staff` or `principal`. The runner
  sends it as the `level` of each task it creates (`POST /v1/tasks`, client `evals` only), so
  the bot works at that level whatever the PM's own setting is. A category with no `level`
  sends none.

**`staff-craft`** (`level: staff`):

| Tasks | The ask | What is checked |
| --- | --- | --- |
| `structure-portfolio-review` | A portfolio review across projects `PM` and `CLD` | Title `Portfolio review:`, "Where things stand", "Dependencies and conflicts", and the rollup's link: "PM-102" and "CLD-13" |
| `structure-roadmap-tradeoffs`, `structure-okr-draft`, `structure-business-review`, `structure-alignment-plan` | "Draft anyway, no questions: …" | One `confluence.create_draft` draft in space PM, with the skill's title prefix and three or more of its template's headings |
| `strategy-doc-cross-team-effects` | A strategy doc at the Staff level | The draft has "Cross-team effects" |
| `coach-feedback-creates-no-draft` | Coaching on fixture page 9002 | No draft and no page update (`approvals: 0`) |
| `open-bet-records-one-draft` | Open a bet, with every field given | One `bets.record` draft with `key`, `title`, `bet`, `metric`, `riskiest_assumption` and `review_by`, and no page |

**`principal-craft`** (`level: principal`):

| Tasks | The ask | What is checked |
| --- | --- | --- |
| `structure-vision-doc`, `structure-investment-case`, `structure-pricing-proposal`, `structure-platform-strategy`, `structure-exec-brief` | "Draft anyway, no questions: …" | As above, with the skill's title prefix (`Vision:`, `Investment case:`, `Pricing:`, `Platform strategy:`, `Exec brief:`) and template headings |
| `investment-case-payback-from-tool` | An investment case with exact inputs: upfront 100, year 1 revenue 60 and cost 10, year 2 revenue 80 and cost 10, discount rate 10% | The draft has the payback `stats.cashflow` returns, `1.7` |
| `strategy-doc-what-must-be-true` | A strategy doc at the Principal level | The draft has "Cross-team effects" and "What must be true" |
| `decision-doc-what-must-be-true` | A decision doc at the Principal level | The draft has "What must be true" |

- **The headings are the contract,** as for `pm-craft`: each is written exactly as §2 of
  `docs/staff-principal-spec.md` lists it. Change a template's heading and its task together.
- **The numbers come from tools.** The portfolio task's link is the fake rollup's (`PM-102`
  blocks `CLD-13`). The investment task expects payback `1.7` (1 + 50 / 70, rounded to 1
  decimal; net present value is 3.31). If `stats.cashflow`'s formula or rounding changes,
  change the task with it.
- **Structure, not judgment.** Two advisory judges read for the argument: `says-what-stops`
  (Staff) and `business-effect-as-range` (Principal). Until calibrated they are reported and
  nothing more.
- **The gate.** As for `pm-craft`: `pmos-evals affected` lists both categories for a change
  under `evals/`, the `pm-assistant` role or any of its skills.
- **Running them** needs the stack on the fake connectors, the `stats.*`, `jira.rollup` and
  `bets.*` tools, and a member whose personal bot runs the new skills.

## CI
`cargo test -p pmos-evals` checks the benchmark machinery with no network. It runs:
- grading, tagging, report, history, statistics, comparison and gate tests;
- a full `run`, repeats, drafts, judges, the load test and the CLI (`run`, `gate`, `compare`,
  `judge calibrate`, `affected`, `check`), against a mocked runtime and a mocked gateway (axum)
  that play scripted outcomes;
- validation of the checked-in categories, taxonomy and judges.

**On every PR,** the GitHub-hosted `content` job also runs
`cargo run -p pmos-evals -- check --root evals --content content`.

**The eval stack.** Benchmarks run against their own short-lived copy of the services
(`deploy/compose.evals.yaml`), never against what PMs use. It has:
- a tool server, runtime and the Docs and `pm-evals` workers, on a checkout's content;
- the user `evals` (with `ben`, `cy` and `eval-admin`) in its in-memory user database, from a
  seed file written beside that copy of the content, never in it, and a personal bot for each
  (`pmos bot add-personal`);
- fake Jira, Confluence, Slack and docs fixtures. To benchmark your own docs site, set
  `PMOS_EVAL_DOCS_URL=http://docs.pmos.internal:8730/mcp` in `deploy/.env`, and the eval
  tool server reads the main stack's docs index, read-only;
- the live gateway's models, under the `evals` client's token, which `deploy/gateway.yaml`
  caps at one call in flight at `personal` priority (its runtime, workers and judges share
  it), so a benchmark holds at most one of production's four slots. A run on the eval stack
  therefore makes one model call at a time; expect it to take longer than on the test stack.

`pmos-admin` starts it with fresh tokens for every run and removes it afterwards. So:
- the gate measures a pull request's own roles and skills;
- no PM's data or accounts are reachable;
- weekly reports and gate runs come from the same setup, so they compare like with like.

**The eval gate** (`.github/workflows/evals-gate.yml`) runs on pull requests that change
`content/skills/**`, `content/roles/**`, `content/bots/**`, `evals/**` or
`deploy/gateway.yaml`:
- **Where:** a self-hosted runner on the Spark (labels `self-hosted, pmos-spark`), where the
  runtime, the worker and the models are. PRs from forks never run there. Registering the
  runner is in `docs/runbooks/upgrade.md`.
- **What:** the Spark's own `/opt/pmos/deploy/pmos-admin evals-gate --checkout pr --base
  origin/<base branch>`. The workflow runs on `pull_request_target` and checks the PR's head
  out into `pr/` as data: nothing in it is run, and the stack uses the Spark's installed
  images. It lists the changed
  paths (`git -C pr diff --name-only <base>...HEAD`), passes them to `pmos-evals affected`, starts
  the eval stack on the PR's checkout, runs `pmos-evals gate` for each affected category with
  the base branch's committed reports as the baseline, and prints the summaries as one markdown document. The job appends it to the job
  summary. It exits 1 if any category's gate failed, 2 on an error, and 0 otherwise, including
  when no category is affected.
- **Until a runner is registered,** the job waits in the queue. A platform owner runs
  `deploy/pmos-admin evals-gate --checkout <a checkout of the PR's branch> --base origin/main`
  on the Spark, and pastes the result into the PR.

**Weekly runs, and runs after a model or harness change,** are by hand on the Spark, on the
eval stack and the content checkout (`main`):

```sh
deploy/pmos-admin evals-run --category docs-qa --note "weekly"
```

The report lands in `/srv/pmos/content-repo/evals/reports`. Commit it by pull request so the
next gate compares against it. `deploy/pmos-admin evals load` still runs on the live stack,
because the load test measures the stack PMs use.

## Gaps against the spec
- **Reports aren't signed (open, review of 2efb73f, R3).** A report is a JSON file anyone with
  write access can edit or forge, so `benchmarked: true` rests on review: `evals/` and
  `crates/evals/` need a second platform owner (CODEOWNERS), and the models come from the
  runtime under test and a `production`-class gateway rather than the runner's own settings.
  What is not covered: a runtime pointed at a gateway that falsely says `class: production`
  (anyone who can run a gateway with `deploy/gateway.yaml` can), a runtime that lies about its
  gateway, and a report edited by hand. Signing reports (by the eval stack, with a key the
  runner doesn't hold) would close it; it is out of scope for now.
- **Traces are console-only (§3),** but D10 tags failures from the trace. The runner takes an
  optional console token (`--trace-token`). Without one it tags from status, error and events.
- **The trace body's shape isn't specified.** The rules read a tool's name from the body's
  `tool` or `name`. They read an error from `isError`, `is_error`, `error` or `status`.
  - Confirm this against the harness worker once it lands.
- **Load-test targets:** only "p90 queue under 2 minutes" is in the repo. The other three are
  labelled defaults. Latency isn't a priority, so they're informational unless
  `--enforce-targets` is passed.
- **Who evals run as:** the spec doesn't name an identity for eval tasks. `requested_by` is a
  config default (`evals`), so that handle has to be a user in the user database, or `--as`
  must name a PM.
  For `bot: personal` categories that member also needs a personal bot.
- **The clustered standard error** falls back to the binomial when every cluster holds one
  *attempt*, not one *task*: with repeats, each task's attempts are a cluster, and treating
  them as independent would understate the error.
- **A check "now fails"** in the gate when it fails in every candidate attempt, and "passed in
  the baseline" when it failed in none. A check that fails in only some attempts shows in the
  paired difference instead, so one unlucky run doesn't fail the gate on its own.
- **The eval stack's attempts are not isolated (open).** The test stack isolates every attempt
  (**Isolated attempts**, E2); `deploy/compose.evals.yaml` doesn't yet, so on it a pending
  Slack reply from one attempt still blocks later ones (see that section). E1's numbers for
  `slack-reply` and `chief`'s `nudge-on-request` in the shared run measure that defect, not the
  bot: one attempt per fresh stack, `slack-reply` passed 20 of 27 against 7 of 27 shared, and
  `nudge-on-request` 3 of 3 against 0 of 3.
- **E1's 0-of-3 tasks (E2's reading, 2026-11-22).** E1's transcripts were not kept (the runtime
  was in memory), and E2 ran no tasks for them, so these causes come from the reports' failed
  checks, the fixtures, the skills and the roles; confirm each from transcripts in the next
  run. Every task has a reference answer that passes and a wrong one that fails
  (`crates/evals/tests/references.rs`).
  - `chief/week-review-undated` (says PM-101, PM-102, PM-103, ×3): the task matches the
    `week-review` skill ("show no item from" undated sources), but the `chief-of-staff` role
    says "a line the tools cannot date is not shown as late or slipping; say what you could
    not date", which invites naming the undated keys. **The bot's instructions conflict**;
    proposed change to the role, not made.
  - `chief/daily-brief-as-chief` (says "call notes", ×3, while the same task in `daily-brief`
    on Nudge never did): the `daily-brief` skill says skip FYIs, but the role says "separate
    'needs you' from 'FYI'", which invites an FYI part with Cy's call-notes message. **The
    bot's instructions conflict**; proposed change to the role, not made.
  - `slack-reply/triage-skips-fyi-and-bots` (1 or 2 drafts, ×3): **the task was ambiguous.**
    "Draft replies to anything there" can mean all of #pm-team, where Ben's SAML question is
    open and needs a reply. The ask now names the two messages; the expectation is unchanged.
  - `slack-reply/triage-drafts-only-what-needs-an-answer` (a fourth draft, to the
    `#field-questions` thread that mentions nobody, ×3): the task, the skill (step 1:
    candidates mention the PM or are DMs) and the spec agree, and the search result shows no
    mention. **The model's**, as far as the reports show; a comment in the task now says why
    that thread gets no draft. A one-line clarification in the skill's step 2 ("skip a search
    result that doesn't mention the PM and isn't a DM") is proposed, not made.
