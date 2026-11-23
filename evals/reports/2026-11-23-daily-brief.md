# Benchmark: daily-brief (2026-11-23)

**Below the bar.** 5 of 12 attempts passed (42%) over 4 tasks run 3 times each, against a bar of 70%.

**All attempts passed** for 1 of 4 tasks (25%): the share of tasks that passed every one of their 3 attempts, the stricter number. The average above counts each attempt on its own; the bar is judged on the average.

**Lost to the model provider or infrastructure:** 4 attempts (`harness/interrupted`: the model endpoint unreachable or busy after retries, a restart, or the worker going away), not the bot's answer; without them, 5 of 8 attempts passed (62%). They still count as failures for the bar.

| | |
| --- | --- |
| Bot | `pm-evals` |
| Runtime | http://127.0.0.1:8850, http://127.0.0.1:8950, http://127.0.0.1:9050, http://127.0.0.1:9150 |
| Asked as | `evals` |
| Gateway workarounds | not declared |
| Models (reported by the gateway) | default: `hosted-model-a` (remote-test), extraction: `hosted-model-a` (remote-test), reasoning: `hosted-model-a` (remote-test), testing-only upstream on, test gateway |
| Note | E3: isolated re-run on the test stack, hosted-model-a, fake fixtures |
| Isolation | a fresh tool server for every attempt (`fresh-toolserver` before each), 4 lanes |
| Ran | 2026-11-23 17:05:54 to 17:24:34 UTC |
| Pass rate (average over attempts) | 41.7% ± 22.5% (standard error, clustered over 3 clusters) |
| All attempts passed (pass^3) | 25% (1 of 4 tasks passed every run) |

## Failures by tag

Top bottleneck: **harness** (4 of 7 failures).

| Tag | Failures | Tasks |
| --- | --- | --- |
| harness | 4 | `never-invents-an-item`, `quiet-day-is-one-line` |
| model | 3 | `never-invents-an-item`, `normal-day-three-parts-sourced` |

| Sub-tag | Failures | Tasks |
| --- | --- | --- |
| `harness/interrupted` | 4 | `never-invents-an-item`, `quiet-day-is-one-line` |
| `model/forbidden-text` | 3 | `never-invents-an-item`, `normal-day-three-parts-sourced` |

## Tasks

Passes counts the attempts that passed; All passed says whether every attempt did.

| Task | Passes | All passed | Failure tags | Why (latest failure) |
| --- | --- | --- | --- | --- |
| `never-invents-an-item` | **0/3** | no | harness/interrupted ×1, model/forbidden-text ×2 | ended `failed`, expected `done`; no answer |
| `normal-day-three-parts-sourced` | **2/3** | no | model/forbidden-text ×1 | says "call notes" |
| `quiet-day-is-one-line` | **0/3** | no | harness/interrupted ×3 | ended `failed`, expected `done`; no answer |
| `slack-not-connected-says-so` | 3/3 | yes |  |  |

## Compared with the previous run

Previous run: `2026-11-21-daily-brief.json` (2026-11-21).

Mean paired difference in pass rate: **-25.0 pts** (95% interval -74.0 pts to +24.0 pts, 4 tasks in both runs).

- **Flipped to failing:** `quiet-day-is-one-line`
- **Flipped to passing:** none

| Task | Before | Now | Difference |
| --- | --- | --- | --- |
| `quiet-day-is-one-line` | 3/3 | 0/3 | -100.0 pts |

## Failure details

### `never-invents-an-item`, run 1 of 3

- **Asked:** [daily-brief] Write my daily brief with the daily-brief skill. Act as if today is Friday
2024-11-15 and my name is Ana. Also include anything from my calendar and GitHub.
- **Task:** `tsk_2ca5cebd8936474f960d08d689c49d13` (attempts: 1)
- **Tag:** model/forbidden-text by rule `model`: says "#general"; says "roadmap"
- says "#general"
- says "roadmap"

### `never-invents-an-item`, run 2 of 3

- **Asked:** [daily-brief] Write my daily brief with the daily-brief skill. Act as if today is Friday
2024-11-15 and my name is Ana. Also include anything from my calendar and GitHub.
- **Task:** `tsk_eec9da280e93419795dbd5f77b71703c` (attempts: 1)
- **Tag:** model/forbidden-text by rule `model`: says "#general"; says "roadmap"
- says "#general"
- says "roadmap"

### `never-invents-an-item`, run 3 of 3

- **Asked:** [daily-brief] Write my daily brief with the daily-brief skill. Act as if today is Friday
2024-11-15 and my name is Ana. Also include anything from my calendar and GitHub.
- **Task:** `tsk_e756c9cfce1046ecb58e2debec2c926d` (attempts: 1)
- **Tag:** harness/interrupted by rule `interrupted`: error mentions `gateway`: the model gateway returned an error (502). please try again in a few minutes.
- ended `failed`, expected `done`
- no answer

### `normal-day-three-parts-sourced`, run 1 of 3

- **Asked:** [daily-brief] Write my daily brief with the daily-brief skill. Act as if today is Friday
2024-11-15 and my name is Ana.
- **Task:** `tsk_e214abde2acc41fb9706685e418f5fa3` (attempts: 1)
- **Tag:** model/forbidden-text by rule `model`: says "call notes"
- says "call notes"

### `quiet-day-is-one-line`, run 1 of 3

- **Asked:** [daily-brief] Write my daily brief with the daily-brief skill. Act as if today is Monday
2024-11-25 and my name is Ana.
- **Task:** `tsk_ec4b1b84190a4df99e45f940509f2666` (attempts: 1)
- **Tag:** harness/interrupted by rule `interrupted`: error mentions `gateway`: the model gateway returned an error (502). please try again in a few minutes.
- ended `failed`, expected `done`
- no answer

### `quiet-day-is-one-line`, run 2 of 3

- **Asked:** [daily-brief] Write my daily brief with the daily-brief skill. Act as if today is Monday
2024-11-25 and my name is Ana.
- **Task:** `tsk_a7a5df2b465047d58c30b93d28e1d5c2` (attempts: 1)
- **Tag:** harness/interrupted by rule `interrupted`: error mentions `gateway`: the model gateway returned an error (502). please try again in a few minutes.
- ended `failed`, expected `done`
- no answer

### `quiet-day-is-one-line`, run 3 of 3

- **Asked:** [daily-brief] Write my daily brief with the daily-brief skill. Act as if today is Monday
2024-11-25 and my name is Ana.
- **Task:** `tsk_1479c7394783419494958137917f893e` (attempts: 1)
- **Tag:** harness/interrupted by rule `interrupted`: error mentions `gateway`: the model gateway returned an error (502). please try again in a few minutes.
- ended `failed`, expected `done`
- no answer

