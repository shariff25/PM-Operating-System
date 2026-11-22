# Benchmark: confluence-draft (2026-11-21)

**Below the bar.** 7 of 9 attempts passed (78%) over 3 tasks run 3 times each, against a bar of 80%.

| | |
| --- | --- |
| Bot | `pm-evals` |
| Runtime | http://127.0.0.1:8750 |
| Asked as | `evals` |
| Gateway workarounds | not declared |
| Note | model: hosted-model-a (hosted-provider.invalid, OpenAI-compatible, every role); first full run (E1) on the test stack, fake fixtures |
| Ran | 2026-11-21 22:21:30 to 22:45:58 UTC |
| Pass rate | 77.8% ± 11.1% (standard error, clustered over 3 clusters) |
| pass^3 | 33% (1 of 3 tasks passed every run) |

## Failures by tag

Top bottleneck: **model** (2 of 2 failures).

| Tag | Failures | Tasks |
| --- | --- | --- |
| model | 2 | `call-notes-summary`, `prd-from-exemplar` |

| Sub-tag | Failures | Tasks |
| --- | --- | --- |
| `model/timed-out` | 2 | `call-notes-summary`, `prd-from-exemplar` |

## Tasks

| Task | Passes | Failure tags | Why (latest failure) |
| --- | --- | --- | --- |
| `call-notes-summary` | **2/3** | model/timed-out ×1 | timed out while `working` |
| `new-page-not-an-edit` | 3/3 |  |  |
| `prd-from-exemplar` | **2/3** | model/timed-out ×1 | timed out while `working` |

## Failure details

### `call-notes-summary`, run 3 of 3

- **Asked:** Draft a Confluence page in the PM space titled 'Acme Cloud customer asks, October' that summarises what customers asked for in the October Acme Cloud call notes.
- **Task:** `tsk_ee53213bcdb34299bd66123d962be47e` (attempts: 1)
- **Tag:** model/timed-out by rule `model`: timed out while `working`
- timed out while `working`

### `prd-from-exemplar`, run 3 of 3

- **Asked:** Draft anyway, no questions: a first-draft PRD in the PM space for exporting audit logs from the Acme Vault console, using the Key export v2 PRD as the exemplar and the October Acme Cloud call notes for customer asks.
- **Task:** `tsk_6526fc829e39451ca581ec7fb97e31d3` (attempts: 1)
- **Tag:** model/timed-out by rule `model`: timed out while `working`
- timed out while `working`

