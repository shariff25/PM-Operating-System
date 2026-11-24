# Benchmark: principal-craft (2026-11-24)

**Below the bar.** 11 of 24 attempts passed (46%) over 8 tasks run 3 times each, against a bar of 80%.

**All attempts passed** for 1 of 8 tasks (12%): the share of tasks that passed every one of their 3 attempts, the stricter number. The average above counts each attempt on its own; the bar is judged on the average.

**Lost to the model provider or infrastructure:** 6 attempts (`harness/interrupted`: the model endpoint unreachable or busy after retries, a restart, or the worker going away), not the bot's answer; without them, 11 of 18 attempts passed (61%). They still count as failures for the bar.

| | |
| --- | --- |
| Bot | `pm-evals` |
| Runtime | http://127.0.0.1:8850, http://127.0.0.1:8950, http://127.0.0.1:9050, http://127.0.0.1:9150 |
| Asked as | `evals` |
| Gateway workarounds | not declared |
| Models (reported by the gateway) | default: `hosted-model-a` (remote-test, model sha256 e8f0d28d3c21…), extraction: `hosted-model-a` (remote-test, model sha256 e8f0d28d3c21…), reasoning: `hosted-model-a` (remote-test, model sha256 e8f0d28d3c21…), testing-only upstream on, test gateway |
| Note | E4: full run on the test stack, streamed calls, hosted-model-a, fake fixtures |
| Isolation | a fresh tool server for every attempt (`fresh-toolserver` before each), 4 lanes |
| Ran | 2026-11-24 03:18:06 to 04:07:14 UTC |
| Pass rate (average over attempts) | 45.8% ± 9.7% (standard error, clustered over 7 clusters) |
| All attempts passed (pass^3) | 12% (1 of 8 tasks passed every run) |

## Failures by tag

Top bottleneck: **model** (7 of 13 failures).

| Tag | Failures | Tasks |
| --- | --- | --- |
| model | 7 | `strategy-doc-what-must-be-true`, `structure-investment-case`, `structure-platform-strategy`, `structure-vision-doc` |
| harness | 6 | `decision-doc-what-must-be-true`, `strategy-doc-what-must-be-true`, `structure-exec-brief`, `structure-pricing-proposal`, `structure-vision-doc` |

| Sub-tag | Failures | Tasks |
| --- | --- | --- |
| `model/timed-out` | 7 | `strategy-doc-what-must-be-true`, `structure-investment-case`, `structure-platform-strategy`, `structure-vision-doc` |
| `harness/interrupted` | 6 | `decision-doc-what-must-be-true`, `strategy-doc-what-must-be-true`, `structure-exec-brief`, `structure-pricing-proposal`, `structure-vision-doc` |

## Judges

| Judge | Status | Yes | No | Unknown | No verdict |
| --- | --- | --- | --- | --- | --- |
| `business-effect-as-range` | advisory: not calibrated | 3 | 8 | 0 | 0 |

## Tasks

Passes counts the attempts that passed; All passed says whether every attempt did.

| Task | Passes | All passed | Failure tags | Why (latest failure) |
| --- | --- | --- | --- | --- |
| `decision-doc-what-must-be-true` | **2/3** | no | harness/interrupted ×1 | ended `failed`, expected `needs_approval`; 0 approval item(s), expected 1; 0 `confluence.create_draft` draft(s), expected 1; no answer |
| `investment-case-payback-from-tool` | 3/3 | yes |  |  |
| `strategy-doc-what-must-be-true` | **0/3** | no | harness/interrupted ×1, model/timed-out ×2 | timed out while `working` |
| `structure-exec-brief` | **2/3** | no | harness/interrupted ×1 | ended `failed`, expected `needs_approval`; 0 approval item(s), expected 1; 0 `confluence.create_draft` draft(s), expected 1; no answer |
| `structure-investment-case` | **1/3** | no | model/timed-out ×2 | timed out while `working` |
| `structure-platform-strategy` | **1/3** | no | model/timed-out ×2 | timed out while `working` |
| `structure-pricing-proposal` | **1/3** | no | harness/interrupted ×2 | ended `failed`, expected `needs_approval`; 0 approval item(s), expected 1; 0 `confluence.create_draft` draft(s), expected 1; no answer |
| `structure-vision-doc` | **1/3** | no | harness/interrupted ×1, model/timed-out ×1 | timed out while `working` |

## Compared with the previous run

Previous run: `2026-11-23-principal-craft.json` (2026-11-23).

Mean paired difference in pass rate: **+20.8 pts** (95% interval +3.6 pts to +38.0 pts, 8 tasks in both runs).

- **Flipped to failing:** none
- **Flipped to passing:** none

| Task | Before | Now | Difference |
| --- | --- | --- | --- |
| `decision-doc-what-must-be-true` | 0/3 | 2/3 | +66.7 pts |
| `investment-case-payback-from-tool` | 2/3 | 3/3 | +33.3 pts |
| `structure-investment-case` | 0/3 | 1/3 | +33.3 pts |
| `structure-platform-strategy` | 0/3 | 1/3 | +33.3 pts |

## Failure details

### `decision-doc-what-must-be-true`, run 1 of 3

- **Asked:** Draft anyway, no questions: a decision doc in the PM space on whether the company should bet on enterprise identity (audit log export or SCIM user provisioning, Jira story PM-102) for Acme Vault, given the October Acme Cloud call notes (Confluence page 9001).
- **Task:** `tsk_92be97825f1541fdadb097bf9649fa1c` (attempts: 1)
- **Tag:** harness/interrupted by rule `interrupted`: error mentions `gateway`: the model gateway's answer was cut off before it finished. please try again in a few minutes.
- ended `failed`, expected `needs_approval`
- 0 approval item(s), expected 1
- 0 `confluence.create_draft` draft(s), expected 1
- no answer

### `strategy-doc-what-must-be-true`, run 1 of 3

- **Asked:** Draft anyway, no questions: a strategy doc in the PM space for where Acme Vault goes in enterprise identity over the next three years (SAML SSO and SCIM), from Jira epic PM-101, story PM-102 and the October Acme Cloud call notes (Confluence page 9001).
- **Task:** `tsk_5bf50e07c14846b38b73fa67a733beac` (attempts: 1)
- **Tag:** harness/interrupted by rule `interrupted`: error mentions `gateway`: the model gateway's answer was cut off before it finished. please try again in a few minutes.
- ended `failed`, expected `needs_approval`
- 0 approval item(s), expected 1
- 0 `confluence.create_draft` draft(s), expected 1
- no answer

### `strategy-doc-what-must-be-true`, run 2 of 3

- **Asked:** Draft anyway, no questions: a strategy doc in the PM space for where Acme Vault goes in enterprise identity over the next three years (SAML SSO and SCIM), from Jira epic PM-101, story PM-102 and the October Acme Cloud call notes (Confluence page 9001).
- **Task:** `tsk_3fd7ba4473ae40b3b43913b2011ce407` (attempts: 1)
- **Timed out:** the runner timed out and cancelled it; its worker stopped (30.1 s)
- **Tag:** model/timed-out by rule `model`: timed out while `working`
- timed out while `working`

### `strategy-doc-what-must-be-true`, run 3 of 3

- **Asked:** Draft anyway, no questions: a strategy doc in the PM space for where Acme Vault goes in enterprise identity over the next three years (SAML SSO and SCIM), from Jira epic PM-101, story PM-102 and the October Acme Cloud call notes (Confluence page 9001).
- **Task:** `tsk_190a81c2254349b3a7f1568f9734c672` (attempts: 1)
- **Timed out:** the runner timed out and cancelled it; its worker stopped (30.1 s)
- **Tag:** model/timed-out by rule `model`: timed out while `working`
- timed out while `working`

### `structure-exec-brief`, run 2 of 3

- **Asked:** Draft anyway, no questions: an exec brief in the PM space asking the executive team to fund SAML SSO for the Acme Vault console (Jira epic PM-101), given the October Acme Cloud call notes (Confluence page 9001).
- **Task:** `tsk_dbec403b80d64b49a97fa065efbe7250` (attempts: 1)
- **Tag:** harness/interrupted by rule `interrupted`: error mentions `gateway`: the model gateway's answer was cut off before it finished. please try again in a few minutes.
- ended `failed`, expected `needs_approval`
- 0 approval item(s), expected 1
- 0 `confluence.create_draft` draft(s), expected 1
- no answer

### `structure-investment-case`, run 2 of 3

- **Asked:** Draft anyway, no questions: an investment case in the PM space for building SAML SSO and SCIM for the Acme Vault console (Jira epic PM-101 and story PM-102). Mark any input you cannot source as unverified.
- **Task:** `tsk_935ba1b4c3c94fcfba784817f5761c79` (attempts: 1)
- **Timed out:** the runner timed out and cancelled it; its worker stopped (30.1 s)
- **Tag:** model/timed-out by rule `model`: timed out while `working`
- timed out while `working`

### `structure-investment-case`, run 3 of 3

- **Asked:** Draft anyway, no questions: an investment case in the PM space for building SAML SSO and SCIM for the Acme Vault console (Jira epic PM-101 and story PM-102). Mark any input you cannot source as unverified.
- **Task:** `tsk_e4c70e1435be47fe858647f0cb56f85d` (attempts: 1)
- **Timed out:** the runner timed out and cancelled it; its worker stopped (30.0 s)
- **Tag:** model/timed-out by rule `model`: timed out while `working`
- timed out while `working`

### `structure-platform-strategy`, run 1 of 3

- **Asked:** Draft anyway, no questions: a platform strategy in the PM space for identity in Acme Vault and Acme Cloud: should we build, buy or partner for SSO and SCIM (Jira epic PM-101, story PM-102)?
- **Task:** `tsk_18f34604ee974badb8321005f7afe029` (attempts: 1)
- **Timed out:** the runner timed out and cancelled it; its worker stopped (30.1 s)
- **Tag:** model/timed-out by rule `model`: timed out while `working`
- timed out while `working`

### `structure-platform-strategy`, run 3 of 3

- **Asked:** Draft anyway, no questions: a platform strategy in the PM space for identity in Acme Vault and Acme Cloud: should we build, buy or partner for SSO and SCIM (Jira epic PM-101, story PM-102)?
- **Task:** `tsk_d1ac568dfd8448caab39f9c04088e4ec` (attempts: 1)
- **Timed out:** the runner timed out and cancelled it; its worker stopped (30.1 s)
- **Tag:** model/timed-out by rule `model`: timed out while `working`
- timed out while `working`

### `structure-pricing-proposal`, run 1 of 3

- **Asked:** Draft anyway, no questions: a pricing proposal in the PM space for packaging SAML SSO and SCIM in Acme Vault, given the October Acme Cloud call notes (Confluence page 9001). Our competitor is Initech; do not quote its prices.
- **Task:** `tsk_4827e76375874c4caef7efbd646a5bbe` (attempts: 1)
- **Tag:** harness/interrupted by rule `interrupted`: error mentions `gateway`: the model gateway's answer was cut off before it finished. please try again in a few minutes.
- ended `failed`, expected `needs_approval`
- 0 approval item(s), expected 1
- 0 `confluence.create_draft` draft(s), expected 1
- no answer

### `structure-pricing-proposal`, run 2 of 3

- **Asked:** Draft anyway, no questions: a pricing proposal in the PM space for packaging SAML SSO and SCIM in Acme Vault, given the October Acme Cloud call notes (Confluence page 9001). Our competitor is Initech; do not quote its prices.
- **Task:** `tsk_23578c4755b940cbb80eae6945dbbba5` (attempts: 1)
- **Tag:** harness/interrupted by rule `interrupted`: error mentions `gateway`: the model gateway's answer was cut off before it finished. please try again in a few minutes.
- ended `failed`, expected `needs_approval`
- 0 approval item(s), expected 1
- 0 `confluence.create_draft` draft(s), expected 1
- no answer

### `structure-vision-doc`, run 1 of 3

- **Asked:** Draft anyway, no questions: a vision doc in the PM space for Acme Vault in 2029, from the October Acme Cloud call notes (Confluence page 9001) and Jira epic PM-101.
- **Task:** `tsk_9444f6c3c1714fe89157beb587369bb9` (attempts: 1)
- **Tag:** harness/interrupted by rule `interrupted`: error mentions `gateway`: the model gateway's answer was cut off before it finished. please try again in a few minutes.
- ended `failed`, expected `needs_approval`
- 0 approval item(s), expected 1
- 0 `confluence.create_draft` draft(s), expected 1
- no answer

### `structure-vision-doc`, run 3 of 3

- **Asked:** Draft anyway, no questions: a vision doc in the PM space for Acme Vault in 2029, from the October Acme Cloud call notes (Confluence page 9001) and Jira epic PM-101.
- **Task:** `tsk_ad79c8db61d24e9db703f7c47fbec26d` (attempts: 1)
- **Timed out:** the runner timed out and cancelled it; its worker stopped (30.1 s)
- **Tag:** model/timed-out by rule `model`: timed out while `working`
- timed out while `working`

