# Benchmark: principal-craft (2026-11-23)

**Below the bar.** 6 of 24 attempts passed (25%) over 8 tasks run 3 times each, against a bar of 80%.

**All attempts passed** for 0 of 8 tasks (0%): the share of tasks that passed every one of their 3 attempts, the stricter number. The average above counts each attempt on its own; the bar is judged on the average.

**Lost to the model provider or infrastructure:** 16 attempts (`harness/interrupted`: the model endpoint unreachable or busy after retries, a restart, or the worker going away), not the bot's answer; without them, 6 of 8 attempts passed (75%). They still count as failures for the bar.

| | |
| --- | --- |
| Bot | `pm-evals` |
| Runtime | http://127.0.0.1:8850, http://127.0.0.1:8950, http://127.0.0.1:9050, http://127.0.0.1:9150 |
| Asked as | `evals` |
| Gateway workarounds | not declared |
| Models (reported by the gateway) | default: `hosted-model-a` (remote-test), extraction: `hosted-model-a` (remote-test), reasoning: `hosted-model-a` (remote-test), testing-only upstream on, test gateway |
| Note | E3: isolated re-run on the test stack, hosted-model-a, fake fixtures |
| Isolation | a fresh tool server for every attempt (`fresh-toolserver` before each), 4 lanes |
| Ran | 2026-11-23 18:38:34 to 19:17:51 UTC |
| Pass rate (average over attempts) | 25.0% ± 8.6% (standard error, clustered over 7 clusters) |
| All attempts passed (pass^3) | 0% (0 of 8 tasks passed every run) |

## Failures by tag

Top bottleneck: **harness** (16 of 18 failures).

| Tag | Failures | Tasks |
| --- | --- | --- |
| harness | 16 | `decision-doc-what-must-be-true`, `investment-case-payback-from-tool`, `strategy-doc-what-must-be-true`, `structure-exec-brief`, `structure-investment-case`, `structure-platform-strategy`, `structure-pricing-proposal`, `structure-vision-doc` |
| model | 2 | `strategy-doc-what-must-be-true`, `structure-platform-strategy` |

| Sub-tag | Failures | Tasks |
| --- | --- | --- |
| `harness/interrupted` | 16 | `decision-doc-what-must-be-true`, `investment-case-payback-from-tool`, `strategy-doc-what-must-be-true`, `structure-exec-brief`, `structure-investment-case`, `structure-platform-strategy`, `structure-pricing-proposal`, `structure-vision-doc` |
| `model/timed-out` | 2 | `strategy-doc-what-must-be-true`, `structure-platform-strategy` |

## Judges

| Judge | Status | Yes | No | Unknown | No verdict |
| --- | --- | --- | --- | --- | --- |
| `business-effect-as-range` | advisory: not calibrated | 1 | 5 | 0 | 0 |

## Tasks

Passes counts the attempts that passed; All passed says whether every attempt did.

| Task | Passes | All passed | Failure tags | Why (latest failure) |
| --- | --- | --- | --- | --- |
| `decision-doc-what-must-be-true` | **0/3** | no | harness/interrupted ×3 | ended `failed`, expected `needs_approval`; 0 approval item(s), expected 1; 0 `confluence.create_draft` draft(s), expected 1; no answer |
| `investment-case-payback-from-tool` | **2/3** | no | harness/interrupted ×1 | ended `failed`, expected `needs_approval`; 0 approval item(s), expected 1; 0 `confluence.create_draft` draft(s), expected 1; no answer |
| `strategy-doc-what-must-be-true` | **0/3** | no | harness/interrupted ×2, model/timed-out ×1 | ended `failed`, expected `needs_approval`; 0 approval item(s), expected 1; 0 `confluence.create_draft` draft(s), expected 1; no answer |
| `structure-exec-brief` | **2/3** | no | harness/interrupted ×1 | ended `failed`, expected `needs_approval`; 0 approval item(s), expected 1; 0 `confluence.create_draft` draft(s), expected 1; no answer |
| `structure-investment-case` | **0/3** | no | harness/interrupted ×3 | ended `failed`, expected `needs_approval`; 0 approval item(s), expected 1; 0 `confluence.create_draft` draft(s), expected 1; no answer |
| `structure-platform-strategy` | **0/3** | no | harness/interrupted ×2, model/timed-out ×1 | ended `failed`, expected `needs_approval`; 0 approval item(s), expected 1; 0 `confluence.create_draft` draft(s), expected 1; no answer |
| `structure-pricing-proposal` | **1/3** | no | harness/interrupted ×2 | ended `failed`, expected `needs_approval`; 0 approval item(s), expected 1; 0 `confluence.create_draft` draft(s), expected 1; no answer |
| `structure-vision-doc` | **1/3** | no | harness/interrupted ×2 | ended `failed`, expected `needs_approval`; 0 approval item(s), expected 1; 0 `confluence.create_draft` draft(s), expected 1; no answer |

## Compared with the previous run

Previous run: `2026-11-22-principal-craft.json` (2026-11-22).

Mean paired difference in pass rate: **-66.7 pts** (95% interval -88.1 pts to -45.3 pts, 8 tasks in both runs).

- **Flipped to failing:** `decision-doc-what-must-be-true`, `structure-investment-case`, `structure-platform-strategy`
- **Flipped to passing:** none

| Task | Before | Now | Difference |
| --- | --- | --- | --- |
| `decision-doc-what-must-be-true` | 3/3 | 0/3 | -100.0 pts |
| `investment-case-payback-from-tool` | 3/3 | 2/3 | -33.3 pts |
| `strategy-doc-what-must-be-true` | 2/3 | 0/3 | -66.7 pts |
| `structure-exec-brief` | 3/3 | 2/3 | -33.3 pts |
| `structure-investment-case` | 3/3 | 0/3 | -100.0 pts |
| `structure-platform-strategy` | 3/3 | 0/3 | -100.0 pts |
| `structure-pricing-proposal` | 2/3 | 1/3 | -33.3 pts |
| `structure-vision-doc` | 3/3 | 1/3 | -66.7 pts |

## Failure details

### `decision-doc-what-must-be-true`, run 1 of 3

- **Asked:** Draft anyway, no questions: a decision doc in the PM space on whether the company should bet on enterprise identity (audit log export or SCIM user provisioning, Jira story PM-102) for Acme Vault, given the October Acme Cloud call notes (Confluence page 9001).
- **Task:** `tsk_422ba2b1aa2f4eddb87a9c7b10b3b5d7` (attempts: 1)
- **Tag:** harness/interrupted by rule `interrupted`: error mentions `gateway`: the model gateway returned an error (502). please try again in a few minutes.
- ended `failed`, expected `needs_approval`
- 0 approval item(s), expected 1
- 0 `confluence.create_draft` draft(s), expected 1
- no answer

### `decision-doc-what-must-be-true`, run 2 of 3

- **Asked:** Draft anyway, no questions: a decision doc in the PM space on whether the company should bet on enterprise identity (audit log export or SCIM user provisioning, Jira story PM-102) for Acme Vault, given the October Acme Cloud call notes (Confluence page 9001).
- **Task:** `tsk_69ec900652d84dc8bbf438ccbc3e1780` (attempts: 1)
- **Tag:** harness/interrupted by rule `interrupted`: error mentions `gateway`: the model gateway returned an error (502). please try again in a few minutes.
- ended `failed`, expected `needs_approval`
- 0 approval item(s), expected 1
- 0 `confluence.create_draft` draft(s), expected 1
- no answer

### `decision-doc-what-must-be-true`, run 3 of 3

- **Asked:** Draft anyway, no questions: a decision doc in the PM space on whether the company should bet on enterprise identity (audit log export or SCIM user provisioning, Jira story PM-102) for Acme Vault, given the October Acme Cloud call notes (Confluence page 9001).
- **Task:** `tsk_acc3d7bf2c2044ffaf8d2beb6e42a48a` (attempts: 1)
- **Tag:** harness/interrupted by rule `interrupted`: error mentions `gateway`: the model gateway returned an error (502). please try again in a few minutes.
- ended `failed`, expected `needs_approval`
- 0 approval item(s), expected 1
- 0 `confluence.create_draft` draft(s), expected 1
- no answer

### `investment-case-payback-from-tool`, run 1 of 3

- **Asked:** Draft anyway, no questions: an investment case in the PM space for a key export service for Acme Vault. Use these inputs and no others: upfront cost 100, year 1 revenue 60 and cost 10, year 2 revenue 80 and cost 10, discount rate 10%.
- **Task:** `tsk_566947e46960455b97b560148a61d458` (attempts: 1)
- **Tag:** harness/interrupted by rule `interrupted`: error mentions `gateway`: the model gateway returned an error (502). please try again in a few minutes.
- ended `failed`, expected `needs_approval`
- 0 approval item(s), expected 1
- 0 `confluence.create_draft` draft(s), expected 1
- no answer

### `strategy-doc-what-must-be-true`, run 1 of 3

- **Asked:** Draft anyway, no questions: a strategy doc in the PM space for where Acme Vault goes in enterprise identity over the next three years (SAML SSO and SCIM), from Jira epic PM-101, story PM-102 and the October Acme Cloud call notes (Confluence page 9001).
- **Task:** `tsk_834e9b56434b457ab246b079e148786a` (attempts: 1)
- **Tag:** harness/interrupted by rule `interrupted`: error mentions `gateway`: the model gateway returned an error (502). please try again in a few minutes.
- ended `failed`, expected `needs_approval`
- 0 approval item(s), expected 1
- 0 `confluence.create_draft` draft(s), expected 1
- no answer

### `strategy-doc-what-must-be-true`, run 2 of 3

- **Asked:** Draft anyway, no questions: a strategy doc in the PM space for where Acme Vault goes in enterprise identity over the next three years (SAML SSO and SCIM), from Jira epic PM-101, story PM-102 and the October Acme Cloud call notes (Confluence page 9001).
- **Task:** `tsk_0e6c070449274e1c8cdc9a8bb8a4178d` (attempts: 1)
- **Tag:** model/timed-out by rule `model`: timed out while `working`
- timed out while `working`

### `strategy-doc-what-must-be-true`, run 3 of 3

- **Asked:** Draft anyway, no questions: a strategy doc in the PM space for where Acme Vault goes in enterprise identity over the next three years (SAML SSO and SCIM), from Jira epic PM-101, story PM-102 and the October Acme Cloud call notes (Confluence page 9001).
- **Task:** `tsk_ce98ddafe162480d95b2d0e75741439c` (attempts: 1)
- **Tag:** harness/interrupted by rule `interrupted`: error mentions `gateway`: the model gateway returned an error (502). please try again in a few minutes.
- ended `failed`, expected `needs_approval`
- 0 approval item(s), expected 1
- 0 `confluence.create_draft` draft(s), expected 1
- no answer

### `structure-exec-brief`, run 3 of 3

- **Asked:** Draft anyway, no questions: an exec brief in the PM space asking the executive team to fund SAML SSO for the Acme Vault console (Jira epic PM-101), given the October Acme Cloud call notes (Confluence page 9001).
- **Task:** `tsk_bd32fdc5bd3a4d90b4d643926894f06f` (attempts: 1)
- **Tag:** harness/interrupted by rule `interrupted`: error mentions `gateway`: the model gateway returned an error (502). please try again in a few minutes.
- ended `failed`, expected `needs_approval`
- 0 approval item(s), expected 1
- 0 `confluence.create_draft` draft(s), expected 1
- no answer

### `structure-investment-case`, run 1 of 3

- **Asked:** Draft anyway, no questions: an investment case in the PM space for building SAML SSO and SCIM for the Acme Vault console (Jira epic PM-101 and story PM-102). Mark any input you cannot source as unverified.
- **Task:** `tsk_24cefcbfe3fd401db79b1a947e302879` (attempts: 1)
- **Tag:** harness/interrupted by rule `interrupted`: error mentions `gateway`: the model gateway returned an error (502). please try again in a few minutes.
- ended `failed`, expected `needs_approval`
- 0 approval item(s), expected 1
- 0 `confluence.create_draft` draft(s), expected 1
- no answer

### `structure-investment-case`, run 2 of 3

- **Asked:** Draft anyway, no questions: an investment case in the PM space for building SAML SSO and SCIM for the Acme Vault console (Jira epic PM-101 and story PM-102). Mark any input you cannot source as unverified.
- **Task:** `tsk_24eda7980ea14b578c9b62c3fa23d697` (attempts: 1)
- **Tag:** harness/interrupted by rule `interrupted`: error mentions `gateway`: the model gateway returned an error (502). please try again in a few minutes.
- ended `failed`, expected `needs_approval`
- 0 approval item(s), expected 1
- 0 `confluence.create_draft` draft(s), expected 1
- no answer

### `structure-investment-case`, run 3 of 3

- **Asked:** Draft anyway, no questions: an investment case in the PM space for building SAML SSO and SCIM for the Acme Vault console (Jira epic PM-101 and story PM-102). Mark any input you cannot source as unverified.
- **Task:** `tsk_e87780795a5748f0a88df36605f07149` (attempts: 1)
- **Tag:** harness/interrupted by rule `interrupted`: error mentions `gateway`: the model gateway returned an error (502). please try again in a few minutes.
- ended `failed`, expected `needs_approval`
- 0 approval item(s), expected 1
- 0 `confluence.create_draft` draft(s), expected 1
- no answer

### `structure-platform-strategy`, run 1 of 3

- **Asked:** Draft anyway, no questions: a platform strategy in the PM space for identity in Acme Vault and Acme Cloud: should we build, buy or partner for SSO and SCIM (Jira epic PM-101, story PM-102)?
- **Task:** `tsk_a4e617954aa54324927c07de8edfef54` (attempts: 1)
- **Tag:** model/timed-out by rule `model`: timed out while `working`
- timed out while `working`

### `structure-platform-strategy`, run 2 of 3

- **Asked:** Draft anyway, no questions: a platform strategy in the PM space for identity in Acme Vault and Acme Cloud: should we build, buy or partner for SSO and SCIM (Jira epic PM-101, story PM-102)?
- **Task:** `tsk_cc961d5a49724a7fbdad46f262cbf5e6` (attempts: 1)
- **Tag:** harness/interrupted by rule `interrupted`: error mentions `gateway`: the model gateway returned an error (502). please try again in a few minutes.
- ended `failed`, expected `needs_approval`
- 0 approval item(s), expected 1
- 0 `confluence.create_draft` draft(s), expected 1
- no answer

### `structure-platform-strategy`, run 3 of 3

- **Asked:** Draft anyway, no questions: a platform strategy in the PM space for identity in Acme Vault and Acme Cloud: should we build, buy or partner for SSO and SCIM (Jira epic PM-101, story PM-102)?
- **Task:** `tsk_22656c3863bd4065864e0ec8d2e13dc6` (attempts: 1)
- **Tag:** harness/interrupted by rule `interrupted`: error mentions `gateway`: the model gateway returned an error (502). please try again in a few minutes.
- ended `failed`, expected `needs_approval`
- 0 approval item(s), expected 1
- 0 `confluence.create_draft` draft(s), expected 1
- no answer

### `structure-pricing-proposal`, run 1 of 3

- **Asked:** Draft anyway, no questions: a pricing proposal in the PM space for packaging SAML SSO and SCIM in Acme Vault, given the October Acme Cloud call notes (Confluence page 9001). Our competitor is Initech; do not quote its prices.
- **Task:** `tsk_2d27cc45a77b4447b583c8d5e38d46d7` (attempts: 1)
- **Tag:** harness/interrupted by rule `interrupted`: error mentions `gateway`: the model gateway returned an error (502). please try again in a few minutes.
- ended `failed`, expected `needs_approval`
- 0 approval item(s), expected 1
- 0 `confluence.create_draft` draft(s), expected 1
- no answer

### `structure-pricing-proposal`, run 3 of 3

- **Asked:** Draft anyway, no questions: a pricing proposal in the PM space for packaging SAML SSO and SCIM in Acme Vault, given the October Acme Cloud call notes (Confluence page 9001). Our competitor is Initech; do not quote its prices.
- **Task:** `tsk_ea2ac2e18da54873bc4e87f8ba917432` (attempts: 1)
- **Tag:** harness/interrupted by rule `interrupted`: error mentions `gateway`: the model gateway returned an error (502). please try again in a few minutes.
- ended `failed`, expected `needs_approval`
- 0 approval item(s), expected 1
- 0 `confluence.create_draft` draft(s), expected 1
- no answer

### `structure-vision-doc`, run 2 of 3

- **Asked:** Draft anyway, no questions: a vision doc in the PM space for Acme Vault in 2029, from the October Acme Cloud call notes (Confluence page 9001) and Jira epic PM-101.
- **Task:** `tsk_05a7fb8973914d5296741fffd207f291` (attempts: 1)
- **Tag:** harness/interrupted by rule `interrupted`: error mentions `gateway`: the model gateway returned an error (502). please try again in a few minutes.
- ended `failed`, expected `needs_approval`
- 0 approval item(s), expected 1
- 0 `confluence.create_draft` draft(s), expected 1
- no answer

### `structure-vision-doc`, run 3 of 3

- **Asked:** Draft anyway, no questions: a vision doc in the PM space for Acme Vault in 2029, from the October Acme Cloud call notes (Confluence page 9001) and Jira epic PM-101.
- **Task:** `tsk_21a68019756a4fbc97510320ca54862c` (attempts: 1)
- **Tag:** harness/interrupted by rule `interrupted`: error mentions `gateway`: the model gateway returned an error (502). please try again in a few minutes.
- ended `failed`, expected `needs_approval`
- 0 approval item(s), expected 1
- 0 `confluence.create_draft` draft(s), expected 1
- no answer

