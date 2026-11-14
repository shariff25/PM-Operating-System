# Benchmarks: `evals/` and `pmos-evals`

A task category launches only when the bots finish it end to end (D10). Each category is a
fixed set of real tasks. We rerun it every week and after every model or harness change, and
we tag each failure with the bottleneck that caused it. A category passes when it meets its
pass bar two weeks in a row. The spec is §8 of `docs/bot-runtime-spec.md`.

```
evals/
  config.yaml                       runner defaults and the load-test latency targets
  categories/<name>/category.yaml   {name, bot, pass_bar, description}
  categories/<name>/tasks/<id>.yaml {id, ask, expect}
  reports/<date>-<category>.md      one per run, for people
  reports/<date>-<category>.json    the same run, read by `history`
```

`pmos-evals` (`crates/evals`) is a client of the bot runtime (`pmos-bots`, §3), signed in as
client `evals`. For each task it:
1. creates the task with `POST /v1/tasks` and `source: "eval"`;
2. polls `GET /v1/tasks/{id}` until the task settles;
3. reads its events;
4. grades the result;
5. for a failure, reads the trace and tags the failure.

## Running it

| Setting | Flag | Environment | Default |
| --- | --- | --- | --- |
| Runtime URL | `--bots-url` | `PMOS_BOTS_URL` | `runner.bots_url` (`http://127.0.0.1:8750`) |
| Runtime token, client `evals` | `--token` | `PMOS_EVALS_TOKEN` | required |
| Console token, for traces | `--trace-token` | `PMOS_EVALS_TRACE_TOKEN` | none |
| Who the tasks are for | `--as` | `PMOS_EVALS_AS` | `runner.requested_by` (`evals`) |
| Evals directory | `--root` | `PMOS_EVALS_ROOT` | `evals` |
| Seconds one task may take | `--timeout-secs` | | `runner.task_timeout_seconds` (600) |
| Milliseconds between polls | `--poll-ms` | | `runner.poll_ms` (2000) |
| Tasks in flight at once (`run` only) | `--concurrency` | | `runner.concurrency` (1) |

**Tokens and identity:**
- **The `evals` token.** Add `evals=<token>` to the runtime's `PMOS_BOTS_TOKENS`.
- **The trace token.** Only client `console` may read traces (§3). Pass a console token as
  `--trace-token` so failures are tagged from the trace. Without one, tagging uses the task's
  status, error and events only, and the report says how many failures were tagged that way.
- **Who asks.** A team bot serves team members only, so the `--as` handle must be in
  `content/team.yaml`. Use a real PM, or add a member for the benchmarks.

**Commands and exit status:**

| Command | What it does | Exit status |
| --- | --- | --- |
| `pmos-evals check [--category docs-qa]` | Validates the category and task files, and `config.yaml` | 0 valid, 2 problems (every problem listed) |
| `pmos-evals run --category docs-qa` | Runs the category and writes its report | 0 met the bar, 1 below it |
| `pmos-evals history --category docs-qa` | Has the bar been met two weeks in a row? | 0 yes, 1 no |
| `pmos-evals load --tasks 5` | The load test | 0 if every task finished, 1 if not (`--enforce-targets`: also 1 on a missed latency target) |

An error (no token, a bad file, a report that can't be written) exits 2.

**Other `run` options:**
- `--task <id>`, repeatable, runs only those tasks. Use it to debug a task, never for the
  weekly run.
- `--note "…"` records why the run happened.
- `--date YYYY-MM-DD` dates the report (default: today, UTC).
- `--reports-dir` changes where reports are written.

### Every week
On the Spark, against the live runtime with the gateway as it normally runs:

```sh
export PMOS_EVALS_TOKEN=… PMOS_EVALS_TRACE_TOKEN=… PMOS_EVALS_AS=ana
pmos-evals run --category docs-qa --note "weekly"
pmos-evals history --category docs-qa
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
- The runner can't see the gateway's own setting. It records `PMOS_GATEWAY_WORKAROUNDS` from
  its own environment, labelled "declared by the runner's environment", so step 2 is what
  marks the report.
- Every `docs-qa` task has `must_not_include: ["<think>"]`. A model that leaks its reasoning
  without `strip-think` fails visibly, tagged `model`.

**Runs and weeks:** a second run on the same day is written as `<date>-<category>-2.md`, so no
run is lost. Every run counts toward its week, so a change that breaks a category fails that
week (see **History** below).

## Reading a report

`evals/reports/<date>-<category>.md` has four parts.

1. **Verdict:** "Met the bar" or "Below the bar", with passed of total, the pass rate and the
   bar.
   - A table follows: the bot, the runtime, who asked, the declared workaround setting, the
     note and the run's start and end times.
   - A **Traces** row appears when some failures were tagged without a trace.
2. **Failures by tag:** the top bottleneck first ("Top bottleneck: **tool** (3 of 5
   failures)"). Then each tag's failure count and its tasks, by count, ties in the order model,
   tool, policy, memory, harness. Fix the top row first.
3. **Tasks:** one row per task. It shows pass or **FAIL**, the tag and the rule that gave it,
   the final status, and queue and total seconds. **Why** lists every failed check.
4. **Failure details:** for each failure, the question, the runtime task id (open it in the
   console for the full trace), the tag, its rule and evidence, and each failed check.

The `.json` next to it holds the same data, every failed check and tag included.

### Grading
A task passes only when every check passes:

| Check | Passes when |
| --- | --- |
| Status | `approvals: n > 0`: ends `needs_approval` with exactly n approval items. `approvals: 0`: ends `done` with none. No `approvals`: ends `done` or `needs_approval`. |
| An answer | the result isn't empty |
| `must_cite` | each URL is a link in the answer |
| `must_include` | each text is in the answer |
| `must_not_include` | no such text is in the answer |

A task the runner stopped waiting for, or couldn't create, fails. `needs_approval` counts as
settled: the bot has finished, and nobody approves eval drafts.

**Links:**
- **What counts as a link.** Any `http(s)://` URL in the answer: bare, in `<…>`, or a markdown
  link target. Trailing punctuation isn't part of a link.
- **How links are compared.** Loosely, ignoring:
  - the scheme (`http` or `https`) and a leading `www.`;
  - the query string and fragment;
  - a trailing slash;
  - a `.md` extension;
  - letter case.

  The portal's `/v1/docs/<slug>` links count as `/docs/<slug>`.

**Text** is compared after normalising both sides the same way:
- lowercase;
- hyphens and dashes become spaces ("SEV-SNP" matches "SEV SNP");
- `*` and `` ` `` are dropped (markdown emphasis and code);
- thousands separators between digits are dropped ("1,800" matches "1800");
- runs of whitespace become one space.

### Failure tags
Every failure gets exactly one tag. The rules run in this order, and the first match wins.

**Terms in the rules:**
- **"Error"** is the task's `error` field, lowercased, checked only for tasks that ended
  `failed`.
- **"Trace"** is the task's trace, when the runner could read it. Entry kinds are
  `prompt`, `model_reply`, `tool_call`, `tool_result` and `policy_denial`.
- **A tool's name** is the `tool` or `name` field of an entry's body.

| # | Rule | Tag | Matches |
| --- | --- | --- | --- |
| 1 | `runtime-unreachable` | harness | A call to the runtime failed, so the task wasn't created or couldn't be followed. This includes a `403` because `--as` may not use the bot. |
| 2 | `policy-denial` | policy | The trace has a `policy_denial` entry |
| 3 | `policy-error` | policy | The error says `policy`, `denied`, `forbidden`, `not permitted` or `403` |
| 4 | `never-claimed` | harness | Timed out while still `queued`: no worker picked it up |
| 5 | `cancelled` | harness | The task was cancelled; nothing in a benchmark cancels tasks |
| 6 | `interrupted` | harness | The error says `interrupted`, `restart`, `lease`, `heartbeat`, `worker`, `sandbox` or `gateway`. This covers restarts (D2), the worker and its sandbox, and the model endpoint being unreachable. |
| 7 | `silent-timeout` | harness | Timed out while `working`, with no events: the worker never reported progress |
| 8 | `done-without-answer` | harness | Ended `done` with an empty result |
| 9 | `memory-error` | memory | The error says `memory`, `context length`, `context window` or `maximum context` |
| 10 | `memory-tool-error` | memory | The trace has a failed result from a `memory*` tool |
| 11 | `tool-error` | tool | The trace has a failed tool result: `isError` or `is_error` true, `error` set, or `status` 400 or more |
| 12 | `tool-failure` | tool | The error says `tool`, `mcp`, `connector` or `unavailable` |
| 13 | `page-not-retrieved` | tool | A page to cite is missing, the bot called at least one tool, and no tool result held that page (compared as citations are). The bot searched, but retrieval didn't surface the page. |
| 14 | `model` | model | Anything else. The bot had what it needed and answered wrong or incompletely, cited the wrong page, leaked `<think>`, drafted the wrong number of approvals, timed out while working with progress, failed with any other error, or never called a tool. |

**Notes on the rules:**
- **Policy comes first.** A refusal can surface as any kind of failure, and it's the cause
  when it's there.
- **Without a trace,** rules 2, 10, 11 and 13 can't fire. Those failures fall through to the
  error rules or to `model`. That is why the weekly run should pass `--trace-token`.
- **The rules live in code.** They are `RULES` in `crates/evals/src/tag.rs`, with a test for
  each.

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
   bot: <bot>            # a bot in content/bots/
   pass_bar: 0.8         # fraction of tasks that must pass, 0 to 1
   description: What the category covers and where its answers come from.
   ```
3. **Write one file per task, `tasks/<id>.yaml`.** The `id` must match the file name.
   ```yaml
   id: dsm-port-requirements
   ask: "Which ports do applications need open to reach an on-premises Fortanix DSM cluster?"
   expect:
     must_cite: [https://support.fortanix.com/docs/fortanix-data-security-manager-port-requirements]
     must_include: ["443", "5696"]
     must_not_include: ["<think>"]
     approvals: 0
   ```
   - `must_cite`: absolute URLs, written as the bot cites them. The Docs bot cites the page
     URL without `.md`.
   - `must_include`: a few short, distinctive facts that are actually in the source: numbers,
     names, endpoints. Avoid phrases a correct answer could paraphrase.
   - `must_not_include`: text a correct answer never contains.
   - `approvals`: how many drafts the task should end with. Use 0 for read-only bots, and n
     for tasks that should draft n writes for approval.
   - `expect` needs at least one check. Unknown fields are refused.
4. **Validate.** Run `pmos-evals check --category <name>`, and fix every problem it lists.
   The test `the_checked_in_evals_are_valid` also runs this in CI.
5. **Run it.** Run it once against the live runtime and read the failures. A failure caused
   by the task itself, such as a term that's paraphrased in every good answer, is fixed in the
   task. Don't lower the bar.
6. **Launch it.** Add it to the weekly run, and to the runs after model and harness changes.
   It launches once `history` says the bar was met two weeks in a row.

### `docs-qa`
Twenty questions for the Docs bot, built from the public portal on 2026-11-14.

**How they were built:**
- **Pages:** `llms.txt` was read, then the `.md` version of each page used, at one request per
  second.
- **Coverage:** DSM, CCM, Confidential AI, Key Insight, Armor and the DSM REST API.
- **Checked against the page:** each task's comment quotes what its page says, and every
  `must_include` term was checked against that page with the normalisation above.
- **When the portal changes a page:** fix the task from the new page, and note it in the
  commit.

## CI
`cargo test -p pmos-evals` checks the benchmark machinery with no network. It runs:
- grading, tagging, report and history tests;
- a full `run`, the load test and the CLI, against a mocked runtime (axum) that plays scripted
  outcomes;
- validation of the checked-in categories.

The live benchmark needs the runtime, the worker and the gateway. It runs on the Spark, not in
CI.

## Gaps against the spec
- **Traces are console-only (§3),** but D10 tags failures from the trace. The runner takes an
  optional console token (`--trace-token`). Without one it tags from status, error and events.
- **The trace body's shape isn't specified.** The rules read a tool's name from the body's
  `tool` or `name`. They read an error from `isError`, `is_error`, `error` or `status`.
  - Confirm this against the harness worker once it lands.
- **Load-test targets:** only "p90 queue under 2 minutes" is in the repo. The other three are
  labelled defaults. Latency isn't a priority, so they're informational unless
  `--enforce-targets` is passed.
- **Who evals run as:** the spec doesn't name an identity for eval tasks. `requested_by` is a
  config default (`evals`), so that handle has to be in `team.yaml`, or `--as` must name a PM.
