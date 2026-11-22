# Benchmark: principal-craft (2026-11-22)

**Met the bar.** 22 of 24 attempts passed (92%) over 8 tasks run 3 times each, against a bar of 80%.

| | |
| --- | --- |
| Bot | `pm-evals` |
| Runtime | http://127.0.0.1:8750 |
| Asked as | `evals` |
| Gateway workarounds | not declared |
| Note | model: hosted-model-a (hosted-provider.invalid, OpenAI-compatible, every role); first full run (E1) on the test stack, fake fixtures |
| Ran | 2026-11-22 00:34:05 to 01:11:19 UTC |
| Pass rate | 91.7% ± 5.7% (standard error, clustered over 7 clusters) |
| pass^3 | 75% (6 of 8 tasks passed every run) |

## Failures by tag

Top bottleneck: **harness** (2 of 2 failures).

| Tag | Failures | Tasks |
| --- | --- | --- |
| harness | 2 | `strategy-doc-what-must-be-true`, `structure-pricing-proposal` |

| Sub-tag | Failures | Tasks |
| --- | --- | --- |
| `harness/interrupted` | 2 | `strategy-doc-what-must-be-true`, `structure-pricing-proposal` |

## Judges

| Judge | Status | Yes | No | No verdict |
| --- | --- | --- | --- | --- |
| `business-effect-as-range` | advisory: not calibrated | 10 | 12 | 0 |

## Tasks

| Task | Passes | Failure tags | Why (latest failure) |
| --- | --- | --- | --- |
| `decision-doc-what-must-be-true` | 3/3 |  |  |
| `investment-case-payback-from-tool` | 3/3 |  |  |
| `strategy-doc-what-must-be-true` | **2/3** | harness/interrupted ×1 | ended `failed`, expected `needs_approval`; 0 approval item(s), expected 1; 0 `confluence.create_draft` draft(s), expected 1; no answer |
| `structure-exec-brief` | 3/3 |  |  |
| `structure-investment-case` | 3/3 |  |  |
| `structure-platform-strategy` | 3/3 |  |  |
| `structure-pricing-proposal` | **2/3** | harness/interrupted ×1 | ended `failed`, expected `needs_approval`; 0 approval item(s), expected 1; 0 `confluence.create_draft` draft(s), expected 1; no answer |
| `structure-vision-doc` | 3/3 |  |  |

## Failure details

### `strategy-doc-what-must-be-true`, run 1 of 3

- **Asked:** Draft anyway, no questions: a strategy doc in the PM space for where Acme Vault goes in enterprise identity over the next three years (SAML SSO and SCIM), from Jira epic PM-101, story PM-102 and the October Acme Cloud call notes (Confluence page 9001).
- **Task:** `tsk_9df9a513cf794d32b5155f9e62e630ef` (attempts: 1)
- **Tag:** harness/interrupted by rule `interrupted`: error mentions `gateway`: the model gateway returned an error (502). please try again in a few minutes.
- ended `failed`, expected `needs_approval`
- 0 approval item(s), expected 1
- 0 `confluence.create_draft` draft(s), expected 1
- no answer

### `structure-pricing-proposal`, run 2 of 3

- **Asked:** Draft anyway, no questions: a pricing proposal in the PM space for packaging SAML SSO and SCIM in Acme Vault, given the October Acme Cloud call notes (Confluence page 9001). Our competitor is Initech; do not quote its prices.
- **Task:** `tsk_49620bc3f5b248c08a376d79aaffe18b` (attempts: 1)
- **Tag:** harness/interrupted by rule `interrupted`: error mentions `gateway`: the model gateway returned an error (502). please try again in a few minutes.
- ended `failed`, expected `needs_approval`
- 0 approval item(s), expected 1
- 0 `confluence.create_draft` draft(s), expected 1
- no answer

