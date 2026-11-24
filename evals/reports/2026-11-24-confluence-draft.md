# Benchmark: confluence-draft (2026-11-24)

**Below the bar.** 9 of 15 attempts passed (60%) over 5 tasks run 3 times each, against a bar of 80%.

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
| Ran | 2026-11-24 06:23:11 to 06:45:38 UTC |
| Pass rate (average over attempts) | 60.0% ± 16.3% (standard error, clustered over 5 clusters) |
| All attempts passed (pass^3) | 40% (2 of 5 tasks passed every run) |

## Failures by tag

Top bottleneck: **model** (6 of 6 failures).

| Tag | Failures | Tasks |
| --- | --- | --- |
| model | 6 | `blocked-notes-missing-not-substituted`, `blocked-page-missing-says-so`, `prd-from-exemplar` |

| Sub-tag | Failures | Tasks |
| --- | --- | --- |
| `model/answered-beyond-sources` | 4 | `blocked-notes-missing-not-substituted`, `blocked-page-missing-says-so` |
| `model/timed-out` | 2 | `prd-from-exemplar` |

## Tasks

Passes counts the attempts that passed; All passed says whether every attempt did.

| Task | Passes | All passed | Failure tags | Why (latest failure) |
| --- | --- | --- | --- | --- |
| `blocked-notes-missing-not-substituted` | **1/3** | no | model/answered-beyond-sources ×2 | answered instead of saying its sources don't |
| `blocked-page-missing-says-so` | **1/3** | no | model/answered-beyond-sources ×2 | answered instead of saying its sources don't |
| `call-notes-summary` | 3/3 | yes |  |  |
| `new-page-not-an-edit` | 3/3 | yes |  |  |
| `prd-from-exemplar` | **1/3** | no | model/timed-out ×2 | timed out while `working` |

## Compared with the previous run

Previous run: `2026-11-23-confluence-draft.json` (2026-11-23).

Mean paired difference in pass rate: **+22.2 pts** (95% interval +0.4 pts to +44.0 pts, 3 tasks in both runs).

- **Flipped to failing:** none
- **Flipped to passing:** none
- **New tasks:** `blocked-notes-missing-not-substituted`, `blocked-page-missing-says-so`

| Task | Before | Now | Difference |
| --- | --- | --- | --- |
| `call-notes-summary` | 2/3 | 3/3 | +33.3 pts |
| `new-page-not-an-edit` | 2/3 | 3/3 | +33.3 pts |

## Failure details

### `blocked-notes-missing-not-substituted`, run 1 of 3

- **Asked:** Draft a Confluence page in the PM space titled 'Acme Cloud customer asks, December' that summarises what customers asked for in the December Acme Cloud call notes.
- **Task:** `tsk_f103a5bf40814471856933ec94f96b0d` (attempts: 1)
- **Tag:** model/answered-beyond-sources by rule `model`: answered instead of saying its sources don't
- answered instead of saying its sources don't

### `blocked-notes-missing-not-substituted`, run 3 of 3

- **Asked:** Draft a Confluence page in the PM space titled 'Acme Cloud customer asks, December' that summarises what customers asked for in the December Acme Cloud call notes.
- **Task:** `tsk_8b3d5502862940dea3db6badf4f70c8a` (attempts: 1)
- **Tag:** model/answered-beyond-sources by rule `model`: answered instead of saying its sources don't
- answered instead of saying its sources don't

### `blocked-page-missing-says-so`, run 1 of 3

- **Asked:** Draft a Confluence page in the PM space titled 'Pricing review: decisions' that summarises the decisions recorded on Confluence page 9050, the November pricing review.
- **Task:** `tsk_5e9a39008d7244c8b4afb518b64d71cd` (attempts: 1)
- **Tag:** model/answered-beyond-sources by rule `model`: answered instead of saying its sources don't
- answered instead of saying its sources don't

### `blocked-page-missing-says-so`, run 2 of 3

- **Asked:** Draft a Confluence page in the PM space titled 'Pricing review: decisions' that summarises the decisions recorded on Confluence page 9050, the November pricing review.
- **Task:** `tsk_432f78fe212e4df2b7ef66f3868cc365` (attempts: 1)
- **Tag:** model/answered-beyond-sources by rule `model`: answered instead of saying its sources don't
- answered instead of saying its sources don't

### `prd-from-exemplar`, run 1 of 3

- **Asked:** Draft anyway, no questions: a first-draft PRD in the PM space for exporting audit logs from the Acme Vault console, using the Key export v2 PRD as the exemplar and the October Acme Cloud call notes for customer asks.
- **Task:** `tsk_3b965c68b94b4ac78eab8906e3cebf98` (attempts: 1)
- **Timed out:** the runner timed out and cancelled it; its worker stopped (30.1 s)
- **Tag:** model/timed-out by rule `model`: timed out while `working`
- timed out while `working`

### `prd-from-exemplar`, run 2 of 3

- **Asked:** Draft anyway, no questions: a first-draft PRD in the PM space for exporting audit logs from the Acme Vault console, using the Key export v2 PRD as the exemplar and the October Acme Cloud call notes for customer asks.
- **Task:** `tsk_3d6dda2a2d1b4f3b8309406507998b42` (attempts: 1)
- **Timed out:** the runner timed out and cancelled it; its worker stopped (30.1 s)
- **Tag:** model/timed-out by rule `model`: timed out while `working`
- timed out while `working`

