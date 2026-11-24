# Benchmark: staff-craft (2026-11-23)

**Below the bar.** 14 of 24 attempts passed (58%) over 8 tasks run 3 times each, against a bar of 80%.

**All attempts passed** for 3 of 8 tasks (38%): the share of tasks that passed every one of their 3 attempts, the stricter number. The average above counts each attempt on its own; the bar is judged on the average.

**Lost to the model provider or infrastructure:** 6 attempts (`harness/interrupted`: the model endpoint unreachable or busy after retries, a restart, or the worker going away), not the bot's answer; without them, 14 of 18 attempts passed (78%). They still count as failures for the bar.

| | |
| --- | --- |
| Bot | `pm-evals` |
| Runtime | http://127.0.0.1:8850, http://127.0.0.1:8950, http://127.0.0.1:9050, http://127.0.0.1:9150 |
| Asked as | `evals` |
| Gateway workarounds | not declared |
| Models (reported by the gateway) | default: `hosted-model-a` (remote-test), extraction: `hosted-model-a` (remote-test), reasoning: `hosted-model-a` (remote-test), testing-only upstream on, test gateway |
| Note | E3: isolated re-run on the test stack, hosted-model-a, fake fixtures |
| Isolation | a fresh tool server for every attempt (`fresh-toolserver` before each), 4 lanes |
| Ran | 2026-11-23 17:54:55 to 18:38:33 UTC |
| Pass rate (average over attempts) | 58.3% ± 15.1% (standard error, clustered over 8 clusters) |
| All attempts passed (pass^3) | 38% (3 of 8 tasks passed every run) |

## Failures by tag

Top bottleneck: **harness** (6 of 10 failures).

| Tag | Failures | Tasks |
| --- | --- | --- |
| harness | 6 | `strategy-doc-cross-team-effects`, `structure-alignment-plan`, `structure-business-review`, `structure-roadmap-tradeoffs` |
| model | 4 | `strategy-doc-cross-team-effects`, `structure-okr-draft`, `structure-roadmap-tradeoffs` |

| Sub-tag | Failures | Tasks |
| --- | --- | --- |
| `harness/interrupted` | 6 | `strategy-doc-cross-team-effects`, `structure-alignment-plan`, `structure-business-review`, `structure-roadmap-tradeoffs` |
| `model/timed-out` | 4 | `strategy-doc-cross-team-effects`, `structure-okr-draft`, `structure-roadmap-tradeoffs` |

## Judges

| Judge | Status | Yes | No | Unknown | No verdict |
| --- | --- | --- | --- | --- | --- |
| `says-what-stops` | advisory: not calibrated | 9 | 2 | 0 | 3 |

## Tasks

Passes counts the attempts that passed; All passed says whether every attempt did.

| Task | Passes | All passed | Failure tags | Why (latest failure) |
| --- | --- | --- | --- | --- |
| `coach-feedback-creates-no-draft` | 3/3 | yes |  |  |
| `open-bet-records-one-draft` | 3/3 | yes |  |  |
| `strategy-doc-cross-team-effects` | **0/3** | no | harness/interrupted ×2, model/timed-out ×1 | timed out while `working` |
| `structure-alignment-plan` | **2/3** | no | harness/interrupted ×1 | ended `failed`, expected `needs_approval`; 0 approval item(s), expected 1; 0 `confluence.create_draft` draft(s), expected 1; no answer |
| `structure-business-review` | **2/3** | no | harness/interrupted ×1 | ended `failed`, expected `needs_approval`; 0 approval item(s), expected 1; 0 `confluence.create_draft` draft(s), expected 1; no answer |
| `structure-okr-draft` | **1/3** | no | model/timed-out ×2 | timed out while `working` |
| `structure-portfolio-review` | 3/3 | yes |  |  |
| `structure-roadmap-tradeoffs` | **0/3** | no | harness/interrupted ×2, model/timed-out ×1 | ended `failed`, expected `needs_approval`; 0 approval item(s), expected 1; 0 `confluence.create_draft` draft(s), expected 1; no answer |

## Compared with the previous run

Previous run: `2026-11-22-staff-craft.json` (2026-11-22).

Mean paired difference in pass rate: **-12.5 pts** (95% interval -45.0 pts to +20.0 pts, 8 tasks in both runs).

- **Flipped to failing:** `strategy-doc-cross-team-effects`
- **Flipped to passing:** none

| Task | Before | Now | Difference |
| --- | --- | --- | --- |
| `coach-feedback-creates-no-draft` | 2/3 | 3/3 | +33.3 pts |
| `strategy-doc-cross-team-effects` | 3/3 | 0/3 | -100.0 pts |
| `structure-alignment-plan` | 1/3 | 2/3 | +33.3 pts |
| `structure-business-review` | 3/3 | 2/3 | -33.3 pts |
| `structure-okr-draft` | 2/3 | 1/3 | -33.3 pts |
| `structure-portfolio-review` | 2/3 | 3/3 | +33.3 pts |
| `structure-roadmap-tradeoffs` | 1/3 | 0/3 | -33.3 pts |

## Failure details

### `strategy-doc-cross-team-effects`, run 1 of 3

- **Asked:** Draft anyway, no questions: a strategy doc in the PM space for enterprise identity across Acme Vault and Acme Cloud (SAML SSO and SCIM), from Jira epic PM-101, story PM-102 and the October Acme Cloud call notes (Confluence page 9001).
- **Task:** `tsk_fd778094aa1845059235b13e889350f3` (attempts: 1)
- **Tag:** harness/interrupted by rule `interrupted`: error mentions `gateway`: the model gateway returned an error (502). please try again in a few minutes.
- ended `failed`, expected `needs_approval`
- 0 approval item(s), expected 1
- 0 `confluence.create_draft` draft(s), expected 1
- no answer

### `strategy-doc-cross-team-effects`, run 2 of 3

- **Asked:** Draft anyway, no questions: a strategy doc in the PM space for enterprise identity across Acme Vault and Acme Cloud (SAML SSO and SCIM), from Jira epic PM-101, story PM-102 and the October Acme Cloud call notes (Confluence page 9001).
- **Task:** `tsk_832febe6b747432eabed621558566ad1` (attempts: 1)
- **Tag:** harness/interrupted by rule `interrupted`: error mentions `gateway`: the model gateway returned an error (502). please try again in a few minutes.
- ended `failed`, expected `needs_approval`
- 0 approval item(s), expected 1
- 0 `confluence.create_draft` draft(s), expected 1
- no answer

### `strategy-doc-cross-team-effects`, run 3 of 3

- **Asked:** Draft anyway, no questions: a strategy doc in the PM space for enterprise identity across Acme Vault and Acme Cloud (SAML SSO and SCIM), from Jira epic PM-101, story PM-102 and the October Acme Cloud call notes (Confluence page 9001).
- **Task:** `tsk_87d3b6772b364957b96b14133e42f59d` (attempts: 1)
- **Tag:** model/timed-out by rule `model`: timed out while `working`
- timed out while `working`

### `structure-alignment-plan`, run 2 of 3

- **Asked:** Draft anyway, no questions: an alignment plan in the PM space for moving SCIM user provisioning (Jira story PM-102) ahead of audit log export. The Acme Cloud team would have to wait; we need their lead and the engineering director to agree.
- **Task:** `tsk_d190930d4a9140219a7ffd53080b25e3` (attempts: 1)
- **Tag:** harness/interrupted by rule `interrupted`: error mentions `gateway`: the model gateway returned an error (502). please try again in a few minutes.
- ended `failed`, expected `needs_approval`
- 0 approval item(s), expected 1
- 0 `confluence.create_draft` draft(s), expected 1
- no answer

### `structure-business-review`, run 2 of 3

- **Asked:** Draft anyway, no questions: a business review in the PM space for the Acme Vault console last quarter, from the PM and CLD Jira projects and the October Acme Cloud call notes (Confluence page 9001), for the directors.
- **Task:** `tsk_f2305e183dc645d5a6b359ab33b82cc8` (attempts: 1)
- **Tag:** harness/interrupted by rule `interrupted`: error mentions `gateway`: the model gateway returned an error (502). please try again in a few minutes.
- ended `failed`, expected `needs_approval`
- 0 approval item(s), expected 1
- 0 `confluence.create_draft` draft(s), expected 1
- no answer

### `structure-okr-draft`, run 1 of 3

- **Asked:** Draft anyway, no questions: OKRs in the PM space for the next two quarters for enterprise identity across Acme Vault (SAML SSO, Jira epic PM-101, and SCIM, story PM-102).
- **Task:** `tsk_1c8205df29754c5d8080ebcabe9f3f5d` (attempts: 1)
- **Tag:** model/timed-out by rule `model`: timed out while `working`
- timed out while `working`

### `structure-okr-draft`, run 3 of 3

- **Asked:** Draft anyway, no questions: OKRs in the PM space for the next two quarters for enterprise identity across Acme Vault (SAML SSO, Jira epic PM-101, and SCIM, story PM-102).
- **Task:** `tsk_14971ba6e76a4dbdac6dc980cd610d89` (attempts: 1)
- **Tag:** model/timed-out by rule `model`: timed out while `working`
- timed out while `working`

### `structure-roadmap-tradeoffs`, run 1 of 3

- **Asked:** Draft anyway, no questions: roadmap trade-offs in the PM space for next year: audit log export or SCIM user provisioning (Jira story PM-102) for the Acme Vault console, given the October Acme Cloud call notes (Confluence page 9001).
- **Task:** `tsk_0fcb82f8bf454c51bb1264354ac40789` (attempts: 1)
- **Tag:** model/timed-out by rule `model`: timed out while `working`
- timed out while `working`

### `structure-roadmap-tradeoffs`, run 2 of 3

- **Asked:** Draft anyway, no questions: roadmap trade-offs in the PM space for next year: audit log export or SCIM user provisioning (Jira story PM-102) for the Acme Vault console, given the October Acme Cloud call notes (Confluence page 9001).
- **Task:** `tsk_6743cd2317254a3cae66617308316882` (attempts: 1)
- **Tag:** harness/interrupted by rule `interrupted`: error mentions `gateway`: the model gateway returned an error (502). please try again in a few minutes.
- ended `failed`, expected `needs_approval`
- 0 approval item(s), expected 1
- 0 `confluence.create_draft` draft(s), expected 1
- no answer

### `structure-roadmap-tradeoffs`, run 3 of 3

- **Asked:** Draft anyway, no questions: roadmap trade-offs in the PM space for next year: audit log export or SCIM user provisioning (Jira story PM-102) for the Acme Vault console, given the October Acme Cloud call notes (Confluence page 9001).
- **Task:** `tsk_34b60fc1584f4ea981b8107d5f297442` (attempts: 1)
- **Tag:** harness/interrupted by rule `interrupted`: error mentions `gateway`: the model gateway returned an error (502). please try again in a few minutes.
- ended `failed`, expected `needs_approval`
- 0 approval item(s), expected 1
- 0 `confluence.create_draft` draft(s), expected 1
- no answer

