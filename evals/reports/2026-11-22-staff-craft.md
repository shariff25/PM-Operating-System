# Benchmark: staff-craft (2026-11-22)

**Below the bar.** 17 of 24 attempts passed (71%) over 8 tasks run 3 times each, against a bar of 80%.

| | |
| --- | --- |
| Bot | `pm-evals` |
| Runtime | http://127.0.0.1:8750 |
| Asked as | `evals` |
| Gateway workarounds | not declared |
| Note | model: hosted-model-a (hosted-provider.invalid, OpenAI-compatible, every role); first full run (E1) on the test stack, fake fixtures |
| Ran | 2026-11-22 00:13:35 to 00:34:05 UTC |
| Pass rate | 70.8% ± 9.8% (standard error, clustered over 8 clusters) |
| pass^3 | 38% (3 of 8 tasks passed every run) |

## Failures by tag

Top bottleneck: **harness** (7 of 7 failures).

| Tag | Failures | Tasks |
| --- | --- | --- |
| harness | 7 | `coach-feedback-creates-no-draft`, `structure-alignment-plan`, `structure-okr-draft`, `structure-portfolio-review`, `structure-roadmap-tradeoffs` |

| Sub-tag | Failures | Tasks |
| --- | --- | --- |
| `harness/interrupted` | 7 | `coach-feedback-creates-no-draft`, `structure-alignment-plan`, `structure-okr-draft`, `structure-portfolio-review`, `structure-roadmap-tradeoffs` |

## Judges

| Judge | Status | Yes | No | No verdict |
| --- | --- | --- | --- | --- |
| `says-what-stops` | advisory: not calibrated | 13 | 3 | 1 |

## Tasks

| Task | Passes | Failure tags | Why (latest failure) |
| --- | --- | --- | --- |
| `coach-feedback-creates-no-draft` | **2/3** | harness/interrupted ×1 | ended `failed`, expected `done`; no answer |
| `open-bet-records-one-draft` | 3/3 |  |  |
| `strategy-doc-cross-team-effects` | 3/3 |  |  |
| `structure-alignment-plan` | **1/3** | harness/interrupted ×2 | ended `failed`, expected `needs_approval`; 0 approval item(s), expected 1; 0 `confluence.create_draft` draft(s), expected 1; no answer |
| `structure-business-review` | 3/3 |  |  |
| `structure-okr-draft` | **2/3** | harness/interrupted ×1 | ended `failed`, expected `needs_approval`; 0 approval item(s), expected 1; 0 `confluence.create_draft` draft(s), expected 1; no answer |
| `structure-portfolio-review` | **2/3** | harness/interrupted ×1 | ended `failed`, expected `needs_approval`; 0 approval item(s), expected 1; 0 `confluence.create_draft` draft(s), expected 1; no answer |
| `structure-roadmap-tradeoffs` | **1/3** | harness/interrupted ×2 | ended `failed`, expected `needs_approval`; 0 approval item(s), expected 1; 0 `confluence.create_draft` draft(s), expected 1; no answer |

## Failure details

### `coach-feedback-creates-no-draft`, run 2 of 3

- **Asked:** Coach me on how to give feedback to the PM who wrote the Key export v2 PRD (Confluence page 9002). What are the three changes that matter most?
- **Task:** `tsk_508119bd465f46c395cb78a9a729767f` (attempts: 1)
- **Tag:** harness/interrupted by rule `interrupted`: error mentions `gateway`: the model gateway returned an error (502). please try again in a few minutes.
- ended `failed`, expected `done`
- no answer

### `structure-alignment-plan`, run 2 of 3

- **Asked:** Draft anyway, no questions: an alignment plan in the PM space for moving SCIM user provisioning (Jira story PM-102) ahead of audit log export. The Acme Cloud team would have to wait; we need their lead and the engineering director to agree.
- **Task:** `tsk_8c8aa6af8ca04441b204befdde67b81e` (attempts: 1)
- **Tag:** harness/interrupted by rule `interrupted`: error mentions `gateway`: the model gateway returned an error (502). please try again in a few minutes.
- ended `failed`, expected `needs_approval`
- 0 approval item(s), expected 1
- 0 `confluence.create_draft` draft(s), expected 1
- no answer

### `structure-alignment-plan`, run 3 of 3

- **Asked:** Draft anyway, no questions: an alignment plan in the PM space for moving SCIM user provisioning (Jira story PM-102) ahead of audit log export. The Acme Cloud team would have to wait; we need their lead and the engineering director to agree.
- **Task:** `tsk_688b211ec4054260a096fa75b8497032` (attempts: 1)
- **Tag:** harness/interrupted by rule `interrupted`: error mentions `gateway`: the model gateway returned an error (502). please try again in a few minutes.
- ended `failed`, expected `needs_approval`
- 0 approval item(s), expected 1
- 0 `confluence.create_draft` draft(s), expected 1
- no answer

### `structure-okr-draft`, run 2 of 3

- **Asked:** Draft anyway, no questions: OKRs in the PM space for the next two quarters for enterprise identity across Acme Vault (SAML SSO, Jira epic PM-101, and SCIM, story PM-102).
- **Task:** `tsk_1920185d0086472a93513e74595a3104` (attempts: 1)
- **Tag:** harness/interrupted by rule `interrupted`: error mentions `gateway`: the model gateway returned an error (502). please try again in a few minutes.
- ended `failed`, expected `needs_approval`
- 0 approval item(s), expected 1
- 0 `confluence.create_draft` draft(s), expected 1
- no answer

### `structure-portfolio-review`, run 2 of 3

- **Asked:** Draft anyway, no questions: a portfolio review in the PM space across the PM project and the CLD project (use the cross-team rollup for project in (PM, CLD)), for the Acme Vault and Acme Cloud directors.
- **Task:** `tsk_540c8db984ba412fa814fd01b0c16adc` (attempts: 1)
- **Tag:** harness/interrupted by rule `interrupted`: error mentions `gateway`: the model gateway returned an error (502). please try again in a few minutes.
- ended `failed`, expected `needs_approval`
- 0 approval item(s), expected 1
- 0 `confluence.create_draft` draft(s), expected 1
- no answer

### `structure-roadmap-tradeoffs`, run 1 of 3

- **Asked:** Draft anyway, no questions: roadmap trade-offs in the PM space for next year: audit log export or SCIM user provisioning (Jira story PM-102) for the Acme Vault console, given the October Acme Cloud call notes (Confluence page 9001).
- **Task:** `tsk_65dd187f42454283844b70639c22c3d8` (attempts: 1)
- **Tag:** harness/interrupted by rule `interrupted`: error mentions `gateway`: the model gateway returned an error (502). please try again in a few minutes.
- ended `failed`, expected `needs_approval`
- 0 approval item(s), expected 1
- 0 `confluence.create_draft` draft(s), expected 1
- no answer

### `structure-roadmap-tradeoffs`, run 3 of 3

- **Asked:** Draft anyway, no questions: roadmap trade-offs in the PM space for next year: audit log export or SCIM user provisioning (Jira story PM-102) for the Acme Vault console, given the October Acme Cloud call notes (Confluence page 9001).
- **Task:** `tsk_c8462b7f4115450a909f673e96ce7360` (attempts: 1)
- **Tag:** harness/interrupted by rule `interrupted`: error mentions `gateway`: the model gateway returned an error (502). please try again in a few minutes.
- ended `failed`, expected `needs_approval`
- 0 approval item(s), expected 1
- 0 `confluence.create_draft` draft(s), expected 1
- no answer

