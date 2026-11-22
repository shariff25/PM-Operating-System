# Benchmark: daily-brief (2026-11-21)

**Below the bar.** 8 of 12 attempts passed (67%) over 4 tasks run 3 times each, against a bar of 70%.

| | |
| --- | --- |
| Bot | `pm-evals` |
| Runtime | http://127.0.0.1:8750 |
| Asked as | `evals` |
| Gateway workarounds | not declared |
| Note | model: hosted-model-a (hosted-provider.invalid, OpenAI-compatible, every role); first full run (E1) on the test stack, fake fixtures |
| Ran | 2026-11-21 22:49:05 to 22:59:36 UTC |
| Pass rate | 66.7% ± 25.0% (standard error, clustered over 3 clusters) |
| pass^3 | 50% (2 of 4 tasks passed every run) |

## Failures by tag

Top bottleneck: **harness** (3 of 4 failures).

| Tag | Failures | Tasks |
| --- | --- | --- |
| harness | 3 | `never-invents-an-item`, `normal-day-three-parts-sourced` |
| model | 1 | `never-invents-an-item` |

| Sub-tag | Failures | Tasks |
| --- | --- | --- |
| `harness/interrupted` | 3 | `never-invents-an-item`, `normal-day-three-parts-sourced` |
| `model/forbidden-text` | 1 | `never-invents-an-item` |

## Tasks

| Task | Passes | Failure tags | Why (latest failure) |
| --- | --- | --- | --- |
| `never-invents-an-item` | **0/3** | harness/interrupted ×2, model/forbidden-text ×1 | ended `failed`, expected `done`; no answer |
| `normal-day-three-parts-sourced` | **2/3** | harness/interrupted ×1 | ended `failed`, expected `done`; no answer |
| `quiet-day-is-one-line` | 3/3 |  |  |
| `slack-not-connected-says-so` | 3/3 |  |  |

## Failure details

### `never-invents-an-item`, run 1 of 3

- **Asked:** [daily-brief] Write my daily brief with the daily-brief skill. Act as if today is Friday
2024-11-15 and my name is Ana. Also include anything from my calendar and GitHub.
- **Task:** `tsk_2496ed7f114a419da9264a9a6b59ba21` (attempts: 1)
- **Tag:** model/forbidden-text by rule `model`: says "#general"; says "roadmap"
- says "#general"
- says "roadmap"

### `never-invents-an-item`, run 2 of 3

- **Asked:** [daily-brief] Write my daily brief with the daily-brief skill. Act as if today is Friday
2024-11-15 and my name is Ana. Also include anything from my calendar and GitHub.
- **Task:** `tsk_76bdc5b399514cbea178f9b641760125` (attempts: 1)
- **Tag:** harness/interrupted by rule `interrupted`: error mentions `gateway`: the model gateway returned an error (502). please try again in a few minutes.
- ended `failed`, expected `done`
- no answer

### `never-invents-an-item`, run 3 of 3

- **Asked:** [daily-brief] Write my daily brief with the daily-brief skill. Act as if today is Friday
2024-11-15 and my name is Ana. Also include anything from my calendar and GitHub.
- **Task:** `tsk_fa3b05da434c4071b737f1b92e126f0b` (attempts: 1)
- **Tag:** harness/interrupted by rule `interrupted`: error mentions `gateway`: the model gateway returned an error (502). please try again in a few minutes.
- ended `failed`, expected `done`
- no answer

### `normal-day-three-parts-sourced`, run 1 of 3

- **Asked:** [daily-brief] Write my daily brief with the daily-brief skill. Act as if today is Friday
2024-11-15 and my name is Ana.
- **Task:** `tsk_1a3ae82a1a434449b934758826ca5ba2` (attempts: 1)
- **Tag:** harness/interrupted by rule `interrupted`: error mentions `gateway`: the model gateway returned an error (502). please try again in a few minutes.
- ended `failed`, expected `done`
- no answer

