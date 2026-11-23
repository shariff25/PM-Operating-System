# Benchmark: confluence-draft (2026-11-23)

**Below the bar.** 5 of 9 attempts passed (56%) over 3 tasks run 3 times each, against a bar of 80%.

**All attempts passed** for 0 of 3 tasks (0%): the share of tasks that passed every one of their 3 attempts, the stricter number. The average above counts each attempt on its own; the bar is judged on the average.

**Lost to the model provider or infrastructure:** 4 attempts (`harness/interrupted`: the model endpoint unreachable or busy after retries, a restart, or the worker going away), not the bot's answer; without them, 5 of 5 attempts passed (100%). They still count as failures for the bar.

| | |
| --- | --- |
| Bot | `pm-evals` |
| Runtime | http://127.0.0.1:8850, http://127.0.0.1:8950, http://127.0.0.1:9050, http://127.0.0.1:9150 |
| Asked as | `evals` |
| Gateway workarounds | not declared |
| Models (reported by the gateway) | default: `hosted-model-a` (remote-test), extraction: `hosted-model-a` (remote-test), reasoning: `hosted-model-a` (remote-test), testing-only upstream on, test gateway |
| Note | E3: isolated re-run on the test stack, hosted-model-a, fake fixtures |
| Isolation | a fresh tool server for every attempt (`fresh-toolserver` before each), 4 lanes |
| Ran | 2026-11-23 17:26:36 to 17:44:23 UTC |
| Pass rate (average over attempts) | 55.6% ± 11.1% (standard error, clustered over 3 clusters) |
| All attempts passed (pass^3) | 0% (0 of 3 tasks passed every run) |

## Failures by tag

Top bottleneck: **harness** (4 of 4 failures).

| Tag | Failures | Tasks |
| --- | --- | --- |
| harness | 4 | `call-notes-summary`, `new-page-not-an-edit`, `prd-from-exemplar` |

| Sub-tag | Failures | Tasks |
| --- | --- | --- |
| `harness/interrupted` | 4 | `call-notes-summary`, `new-page-not-an-edit`, `prd-from-exemplar` |

## Tasks

Passes counts the attempts that passed; All passed says whether every attempt did.

| Task | Passes | All passed | Failure tags | Why (latest failure) |
| --- | --- | --- | --- | --- |
| `call-notes-summary` | **2/3** | no | harness/interrupted ×1 | ended `failed`, expected `needs_approval`; 0 approval item(s), expected 1; 0 `confluence.create_draft` draft(s), expected 1; no answer |
| `new-page-not-an-edit` | **2/3** | no | harness/interrupted ×1 | ended `failed`, expected `needs_approval`; 0 approval item(s), expected 1; 0 `confluence.create_draft` draft(s), expected 1; no answer |
| `prd-from-exemplar` | **1/3** | no | harness/interrupted ×2 | ended `failed`, expected `needs_approval`; 0 approval item(s), expected 1; 0 `confluence.create_draft` draft(s), expected 1; no answer |

## Compared with the previous run

Previous run: `2026-11-21-confluence-draft.json` (2026-11-21).

Mean paired difference in pass rate: **-22.2 pts** (95% interval -44.0 pts to -0.4 pts, 3 tasks in both runs).

- **Flipped to failing:** none
- **Flipped to passing:** none

| Task | Before | Now | Difference |
| --- | --- | --- | --- |
| `new-page-not-an-edit` | 3/3 | 2/3 | -33.3 pts |
| `prd-from-exemplar` | 2/3 | 1/3 | -33.3 pts |

## Failure details

### `call-notes-summary`, run 3 of 3

- **Asked:** Draft a Confluence page in the PM space titled 'Acme Cloud customer asks, October' that summarises what customers asked for in the October Acme Cloud call notes.
- **Task:** `tsk_71acab54b55044eb99be4403ad1d88aa` (attempts: 1)
- **Tag:** harness/interrupted by rule `interrupted`: error mentions `gateway`: the model gateway returned an error (502). please try again in a few minutes.
- ended `failed`, expected `needs_approval`
- 0 approval item(s), expected 1
- 0 `confluence.create_draft` draft(s), expected 1
- no answer

### `new-page-not-an-edit`, run 2 of 3

- **Asked:** Draft a new page in the PM space listing next steps for the SSO and audit export asks from the October Acme Cloud call notes. Leave the call notes page itself unchanged.
- **Task:** `tsk_28ccb3c60fe34fcbb53ed7e56ec1e0f3` (attempts: 1)
- **Tag:** harness/interrupted by rule `interrupted`: error mentions `gateway`: the model gateway returned an error (502). please try again in a few minutes.
- ended `failed`, expected `needs_approval`
- 0 approval item(s), expected 1
- 0 `confluence.create_draft` draft(s), expected 1
- no answer

### `prd-from-exemplar`, run 1 of 3

- **Asked:** Draft anyway, no questions: a first-draft PRD in the PM space for exporting audit logs from the Acme Vault console, using the Key export v2 PRD as the exemplar and the October Acme Cloud call notes for customer asks.
- **Task:** `tsk_5258e4223a5742918c858833b62780f2` (attempts: 1)
- **Tag:** harness/interrupted by rule `interrupted`: error mentions `gateway`: the model gateway returned an error (502). please try again in a few minutes.
- ended `failed`, expected `needs_approval`
- 0 approval item(s), expected 1
- 0 `confluence.create_draft` draft(s), expected 1
- no answer

### `prd-from-exemplar`, run 3 of 3

- **Asked:** Draft anyway, no questions: a first-draft PRD in the PM space for exporting audit logs from the Acme Vault console, using the Key export v2 PRD as the exemplar and the October Acme Cloud call notes for customer asks.
- **Task:** `tsk_f7e8524e12b7449bb50c7cbcb744e305` (attempts: 1)
- **Tag:** harness/interrupted by rule `interrupted`: error mentions `gateway`: the model gateway returned an error (502). please try again in a few minutes.
- ended `failed`, expected `needs_approval`
- 0 approval item(s), expected 1
- 0 `confluence.create_draft` draft(s), expected 1
- no answer

