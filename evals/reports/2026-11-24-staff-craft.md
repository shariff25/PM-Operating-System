# Benchmark: staff-craft (2026-11-24)

**Below the bar.** 18 of 24 attempts passed (75%) over 8 tasks run 3 times each, against a bar of 80%.

**All attempts passed** for 3 of 8 tasks (38%): the share of tasks that passed every one of their 3 attempts, the stricter number. The average above counts each attempt on its own; the bar is judged on the average.

**Lost to the model provider or infrastructure:** 2 attempts (`harness/interrupted`: the model endpoint unreachable or busy after retries, a restart, or the worker going away), not the bot's answer; without them, 18 of 22 attempts passed (82%). They still count as failures for the bar.

| | |
| --- | --- |
| Bot | `pm-evals` |
| Runtime | http://127.0.0.1:8850, http://127.0.0.1:8950, http://127.0.0.1:9050, http://127.0.0.1:9150 |
| Asked as | `evals` |
| Gateway workarounds | not declared |
| Models (reported by the gateway) | default: `hosted-model-a` (remote-test, model sha256 e8f0d28d3c21…), extraction: `hosted-model-a` (remote-test, model sha256 e8f0d28d3c21…), reasoning: `hosted-model-a` (remote-test, model sha256 e8f0d28d3c21…), testing-only upstream on, test gateway |
| Note | E4: full run on the test stack, streamed calls, hosted-model-a, fake fixtures |
| Isolation | a fresh tool server for every attempt (`fresh-toolserver` before each), 4 lanes |
| Ran | 2026-11-24 05:41:47 to 06:23:10 UTC |
| Pass rate (average over attempts) | 75.0% ± 8.3% (standard error, clustered over 8 clusters) |
| All attempts passed (pass^3) | 38% (3 of 8 tasks passed every run) |

## Failures by tag

Top bottleneck: **model** (4 of 6 failures).

| Tag | Failures | Tasks |
| --- | --- | --- |
| model | 4 | `strategy-doc-cross-team-effects`, `structure-business-review`, `structure-roadmap-tradeoffs` |
| harness | 2 | `structure-okr-draft`, `structure-portfolio-review` |

| Sub-tag | Failures | Tasks |
| --- | --- | --- |
| `model/timed-out` | 4 | `strategy-doc-cross-team-effects`, `structure-business-review`, `structure-roadmap-tradeoffs` |
| `harness/interrupted` | 2 | `structure-okr-draft`, `structure-portfolio-review` |

## Judges

| Judge | Status | Yes | No | Unknown | No verdict |
| --- | --- | --- | --- | --- | --- |
| `says-what-stops` | advisory: not calibrated | 12 | 4 | 0 | 2 |

## Tasks

Passes counts the attempts that passed; All passed says whether every attempt did.

| Task | Passes | All passed | Failure tags | Why (latest failure) |
| --- | --- | --- | --- | --- |
| `coach-feedback-creates-no-draft` | 3/3 | yes |  |  |
| `open-bet-records-one-draft` | 3/3 | yes |  |  |
| `strategy-doc-cross-team-effects` | **2/3** | no | model/timed-out ×1 | timed out while `working` |
| `structure-alignment-plan` | 3/3 | yes |  |  |
| `structure-business-review` | **2/3** | no | model/timed-out ×1 | timed out while `working` |
| `structure-okr-draft` | **2/3** | no | harness/interrupted ×1 | ended `failed`, expected `needs_approval`; 0 approval item(s), expected 1; 0 `confluence.create_draft` draft(s), expected 1; no answer |
| `structure-portfolio-review` | **2/3** | no | harness/interrupted ×1 | ended `failed`, expected `needs_approval`; 0 approval item(s), expected 1; 0 `confluence.create_draft` draft(s), expected 1; no answer |
| `structure-roadmap-tradeoffs` | **1/3** | no | model/timed-out ×2 | timed out while `working` |

## Compared with the previous run

Previous run: `2026-11-23-staff-craft.json` (2026-11-23).

Mean paired difference in pass rate: **+16.7 pts** (95% interval -4.7 pts to +38.1 pts, 8 tasks in both runs).

- **Flipped to failing:** none
- **Flipped to passing:** none

| Task | Before | Now | Difference |
| --- | --- | --- | --- |
| `strategy-doc-cross-team-effects` | 0/3 | 2/3 | +66.7 pts |
| `structure-alignment-plan` | 2/3 | 3/3 | +33.3 pts |
| `structure-okr-draft` | 1/3 | 2/3 | +33.3 pts |
| `structure-portfolio-review` | 3/3 | 2/3 | -33.3 pts |
| `structure-roadmap-tradeoffs` | 0/3 | 1/3 | +33.3 pts |

## Failure details

### `strategy-doc-cross-team-effects`, run 1 of 3

- **Asked:** Draft anyway, no questions: a strategy doc in the PM space for enterprise identity across Acme Vault and Acme Cloud (SAML SSO and SCIM), from Jira epic PM-101, story PM-102 and the October Acme Cloud call notes (Confluence page 9001).
- **Task:** `tsk_b8e1d8d890b54f25842f251954cc608b` (attempts: 1)
- **Timed out:** the runner timed out and cancelled it; its worker stopped (12.0 s)
- **Tag:** model/timed-out by rule `model`: timed out while `working`
- timed out while `working`

### `structure-business-review`, run 1 of 3

- **Asked:** Draft anyway, no questions: a business review in the PM space for the Acme Vault console last quarter, from the PM and CLD Jira projects and the October Acme Cloud call notes (Confluence page 9001), for the directors.
- **Task:** `tsk_7d77d06de4a242fba49881e0133c6d9c` (attempts: 1)
- **Timed out:** the runner timed out and cancelled it; its worker stopped (30.1 s)
- **Tag:** model/timed-out by rule `model`: timed out while `working`
- timed out while `working`

### `structure-okr-draft`, run 1 of 3

- **Asked:** Draft anyway, no questions: OKRs in the PM space for the next two quarters for enterprise identity across Acme Vault (SAML SSO, Jira epic PM-101, and SCIM, story PM-102).
- **Task:** `tsk_68e45c53034141d888dac65fc54ebe35` (attempts: 1)
- **Tag:** harness/interrupted by rule `interrupted`: error mentions `gateway`: the model gateway's answer was cut off before it finished. please try again in a few minutes.
- ended `failed`, expected `needs_approval`
- 0 approval item(s), expected 1
- 0 `confluence.create_draft` draft(s), expected 1
- no answer

### `structure-portfolio-review`, run 1 of 3

- **Asked:** Draft anyway, no questions: a portfolio review in the PM space across the PM project and the CLD project (use the cross-team rollup for project in (PM, CLD)), for the Acme Vault and Acme Cloud directors.
- **Task:** `tsk_18f51004a08e4f85800a17b189dab977` (attempts: 1)
- **Tag:** harness/interrupted by rule `interrupted`: error mentions `gateway`: the model gateway's answer was cut off before it finished. please try again in a few minutes.
- ended `failed`, expected `needs_approval`
- 0 approval item(s), expected 1
- 0 `confluence.create_draft` draft(s), expected 1
- no answer

### `structure-roadmap-tradeoffs`, run 1 of 3

- **Asked:** Draft anyway, no questions: roadmap trade-offs in the PM space for next year: audit log export or SCIM user provisioning (Jira story PM-102) for the Acme Vault console, given the October Acme Cloud call notes (Confluence page 9001).
- **Task:** `tsk_1abc80a576234a63a7c9b4b440f637d5` (attempts: 1)
- **Timed out:** the runner timed out and cancelled it; its worker stopped (30.1 s)
- **Tag:** model/timed-out by rule `model`: timed out while `working`
- timed out while `working`

### `structure-roadmap-tradeoffs`, run 2 of 3

- **Asked:** Draft anyway, no questions: roadmap trade-offs in the PM space for next year: audit log export or SCIM user provisioning (Jira story PM-102) for the Acme Vault console, given the October Acme Cloud call notes (Confluence page 9001).
- **Task:** `tsk_fe319a5fa761441d8ecdefe4db5e74a5` (attempts: 1)
- **Timed out:** the runner timed out and cancelled it; its worker stopped (30.1 s)
- **Tag:** model/timed-out by rule `model`: timed out while `working`
- timed out while `working`

