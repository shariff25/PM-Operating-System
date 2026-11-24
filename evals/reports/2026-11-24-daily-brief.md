# Benchmark: daily-brief (2026-11-24)

**Below the bar.** 7 of 15 attempts passed (47%) over 5 tasks run 3 times each, against a bar of 70%.

**All attempts passed** for 2 of 5 tasks (40%): the share of tasks that passed every one of their 3 attempts, the stricter number. The average above counts each attempt on its own; the bar is judged on the average.

| | |
| --- | --- |
| Bot | `pm-evals` |
| Runtime | http://127.0.0.1:8850, http://127.0.0.1:8950, http://127.0.0.1:9050, http://127.0.0.1:9150 |
| Asked as | `evals` |
| Gateway workarounds | not declared |
| Models (reported by the gateway) | default: `hosted-model-a` (remote-test, model sha256 e8f0d28d3c21…), extraction: `hosted-model-a` (remote-test, model sha256 e8f0d28d3c21…), reasoning: `hosted-model-a` (remote-test, model sha256 e8f0d28d3c21…), testing-only upstream on, test gateway |
| Note | E4: full run on the test stack, streamed calls, hosted-model-a, fake fixtures |
| Isolation | a fresh tool server for every attempt (`fresh-toolserver` before each), 4 lanes |
| Ran | 2026-11-24 06:45:38 to 07:07:40 UTC |
| Pass rate (average over attempts) | 46.7% ± 16.7% (standard error, clustered over 3 clusters) |
| All attempts passed (pass^3) | 40% (2 of 5 tasks passed every run) |

## Failures by tag

Top bottleneck: **model** (8 of 8 failures).

| Tag | Failures | Tasks |
| --- | --- | --- |
| model | 8 | `never-invents-an-item`, `normal-day-three-parts-sourced`, `quiet-day-is-one-line` |

| Sub-tag | Failures | Tasks |
| --- | --- | --- |
| `model/forbidden-text` | 8 | `never-invents-an-item`, `normal-day-three-parts-sourced`, `quiet-day-is-one-line` |

## Tasks

Passes counts the attempts that passed; All passed says whether every attempt did.

| Task | Passes | All passed | Failure tags | Why (latest failure) |
| --- | --- | --- | --- | --- |
| `blocked-confluence-failing-says-so` | 3/3 | yes |  |  |
| `never-invents-an-item` | **0/3** | no | model/forbidden-text ×3 | says "#general"; says "roadmap" |
| `normal-day-three-parts-sourced` | **0/3** | no | model/forbidden-text ×3 | says "call notes" |
| `quiet-day-is-one-line` | **1/3** | no | model/forbidden-text ×2 | says "You are waiting on"; says "SAML"; says "Globex" |
| `slack-not-connected-says-so` | 3/3 | yes |  |  |

## Compared with the previous run

Previous run: `2026-11-23-daily-brief.json` (2026-11-23).

Mean paired difference in pass rate: **-8.3 pts** (95% interval -49.4 pts to +32.8 pts, 4 tasks in both runs).

- **Flipped to failing:** none
- **Flipped to passing:** none
- **New tasks:** `blocked-confluence-failing-says-so`

| Task | Before | Now | Difference |
| --- | --- | --- | --- |
| `normal-day-three-parts-sourced` | 2/3 | 0/3 | -66.7 pts |
| `quiet-day-is-one-line` | 0/3 | 1/3 | +33.3 pts |

## Failure details

### `never-invents-an-item`, run 1 of 3

- **Asked:** [daily-brief] Write my daily brief with the daily-brief skill. Act as if today is Friday
2024-11-15 and my name is Ana. Also include anything from my calendar and GitHub.
- **Task:** `tsk_c5de746021c74f7c86032e573f3b0f36` (attempts: 1)
- **Tag:** model/forbidden-text by rule `model`: says "#general"; says "roadmap"
- says "#general"
- says "roadmap"

### `never-invents-an-item`, run 2 of 3

- **Asked:** [daily-brief] Write my daily brief with the daily-brief skill. Act as if today is Friday
2024-11-15 and my name is Ana. Also include anything from my calendar and GitHub.
- **Task:** `tsk_85101fcc86834343ae26aa4abadc0d3e` (attempts: 1)
- **Tag:** model/forbidden-text by rule `model`: says "#general"; says "roadmap"
- says "#general"
- says "roadmap"

### `never-invents-an-item`, run 3 of 3

- **Asked:** [daily-brief] Write my daily brief with the daily-brief skill. Act as if today is Friday
2024-11-15 and my name is Ana. Also include anything from my calendar and GitHub.
- **Task:** `tsk_5a3a3d85865b4c4b9f383d9f15143351` (attempts: 1)
- **Tag:** model/forbidden-text by rule `model`: says "#general"; says "roadmap"
- says "#general"
- says "roadmap"

### `normal-day-three-parts-sourced`, run 1 of 3

- **Asked:** [daily-brief] Write my daily brief with the daily-brief skill. Act as if today is Friday
2024-11-15 and my name is Ana.
- **Task:** `tsk_8b9e92ead3674b0f9d545f3b53a26014` (attempts: 1)
- **Tag:** model/forbidden-text by rule `model`: says "SCIM"; says "call notes"
- says "SCIM"
- says "call notes"

### `normal-day-three-parts-sourced`, run 2 of 3

- **Asked:** [daily-brief] Write my daily brief with the daily-brief skill. Act as if today is Friday
2024-11-15 and my name is Ana.
- **Task:** `tsk_b08129c034ae476baf4454c665737504` (attempts: 1)
- **Tag:** model/forbidden-text by rule `model`: says "SCIM"; says "call notes"
- says "SCIM"
- says "call notes"

### `normal-day-three-parts-sourced`, run 3 of 3

- **Asked:** [daily-brief] Write my daily brief with the daily-brief skill. Act as if today is Friday
2024-11-15 and my name is Ana.
- **Task:** `tsk_4c5a7b5098cd481b9c1fec2617948e88` (attempts: 1)
- **Tag:** model/forbidden-text by rule `model`: says "call notes"
- says "call notes"

### `quiet-day-is-one-line`, run 1 of 3

- **Asked:** [daily-brief] Write my daily brief with the daily-brief skill. Act as if today is Monday
2024-11-25 and my name is Ana.
- **Task:** `tsk_1310fc742c744cb7b922a72adb0a18f7` (attempts: 1)
- **Tag:** model/forbidden-text by rule `model`: says "You are waiting on"; says "SAML"; says "Globex"
- says "You are waiting on"
- says "SAML"
- says "Globex"

### `quiet-day-is-one-line`, run 3 of 3

- **Asked:** [daily-brief] Write my daily brief with the daily-brief skill. Act as if today is Monday
2024-11-25 and my name is Ana.
- **Task:** `tsk_e9c02bc9dcaf459889527d196f0dff9c` (attempts: 1)
- **Tag:** model/forbidden-text by rule `model`: says "You are waiting on"; says "SAML"; says "Globex"
- says "You are waiting on"
- says "SAML"
- says "Globex"

