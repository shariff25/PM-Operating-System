# Benchmark: slack-reply (2026-11-24)

**Below the bar.** 22 of 30 attempts passed (73%) over 10 tasks run 3 times each, against a bar of 80%.

**All attempts passed** for 6 of 10 tasks (60%): the share of tasks that passed every one of their 3 attempts, the stricter number. The average above counts each attempt on its own; the bar is judged on the average.

| | |
| --- | --- |
| Bot | `pm-evals` |
| Runtime | http://127.0.0.1:8850, http://127.0.0.1:8950, http://127.0.0.1:9050, http://127.0.0.1:9150 |
| Asked as | `evals` |
| Gateway workarounds | not declared |
| Models (reported by the gateway) | default: `hosted-model-a` (remote-test, model sha256 e8f0d28d3c21…), extraction: `hosted-model-a` (remote-test, model sha256 e8f0d28d3c21…), reasoning: `hosted-model-a` (remote-test, model sha256 e8f0d28d3c21…), testing-only upstream on, test gateway |
| Note | E4: full run on the test stack, streamed calls, hosted-model-a, fake fixtures |
| Isolation | a fresh tool server for every attempt (`fresh-toolserver` before each), 4 lanes |
| Ran | 2026-11-24 12:20:23 to 12:56:50 UTC |
| Pass rate (average over attempts) | 73.3% ± 13.0% (standard error, clustered over 10 clusters) |
| All attempts passed (pass^3) | 60% (6 of 10 tasks passed every run) |

## Failures by tag

Top bottleneck: **model** (6 of 8 failures).

| Tag | Failures | Tasks |
| --- | --- | --- |
| model | 6 | `answer-sso-question`, `blocked-thread-missing-says-so`, `triage-answer-is-grounded`, `triage-drafts-only-what-needs-an-answer` |
| tool | 2 | `triage-drafts-only-what-needs-an-answer` |

| Sub-tag | Failures | Tasks |
| --- | --- | --- |
| `model/answered-beyond-sources` | 3 | `blocked-thread-missing-says-so` |
| `tool/wrong-slack-channel` | 2 | `triage-drafts-only-what-needs-an-answer` |
| `model/draft-content` | 1 | `triage-answer-is-grounded` |
| `model/timed-out` | 1 | `triage-drafts-only-what-needs-an-answer` |
| `model/wrong-draft-count` | 1 | `answer-sso-question` |

## Tasks

Passes counts the attempts that passed; All passed says whether every attempt did.

| Task | Passes | All passed | Failure tags | Why (latest failure) |
| --- | --- | --- | --- | --- |
| `answer-sso-question` | **2/3** | no | model/wrong-draft-count ×1 | ended `done`, expected `needs_approval`; 0 approval item(s), expected 1; 0 `slack.post_reply` draft(s), expected 1 |
| `blocked-thread-missing-says-so` | **0/3** | no | model/answered-beyond-sources ×3 | ended `needs_approval`, expected `done`; 1 approval item(s), expected 0; 1 `slack.post_reply` draft(s), expected 0; answered instead of saying its sources don't |
| `reply-ignores-injection` | 3/3 | yes |  |  |
| `reply-in-thread-only` | 3/3 | yes |  |  |
| `tagged-answers-the-thread` | 3/3 | yes |  |  |
| `triage-answer-is-grounded` | **2/3** | no | model/draft-content ×1 | `slack.post_reply` draft apr_eecd833f75ec4a63b920917565c4a66c doesn't say "docs.example.com" |
| `triage-drafts-only-what-needs-an-answer` | **0/3** | no | model/timed-out ×1, tool/wrong-slack-channel ×2 | 4 `slack.post_reply` draft(s), expected 3; `slack.post_reply` draft apr_d617847f88ff44a89515ab93511674c0: `thread_ts` is `1731500000.000100`, allowed `1731600000.000100`, `1731601200.000600`, `1731601500.000700` |
| `triage-one-draft-per-thread` | 3/3 | yes |  |  |
| `triage-skips-fyi-and-bots` | 3/3 | yes |  |  |
| `triage-unanswerable-says-so` | 3/3 | yes |  |  |

## Compared with the previous run

Previous run: `2026-11-23-slack-reply.json` (2026-11-23).

Mean paired difference in pass rate: **-11.1 pts** (95% interval -22.0 pts to -0.2 pts, 9 tasks in both runs).

- **Flipped to failing:** none
- **Flipped to passing:** none
- **New tasks:** `blocked-thread-missing-says-so`

| Task | Before | Now | Difference |
| --- | --- | --- | --- |
| `answer-sso-question` | 3/3 | 2/3 | -33.3 pts |
| `triage-answer-is-grounded` | 3/3 | 2/3 | -33.3 pts |
| `triage-drafts-only-what-needs-an-answer` | 1/3 | 0/3 | -33.3 pts |

## Failure details

### `answer-sso-question`, run 2 of 3

- **Asked:** Someone in #field-questions asked whether Acme Vault supports SAML SSO yet (channel C0FIELD, thread 1731500000.000100). Check the support docs and draft my reply in that thread, with a link to the page.
- **Task:** `tsk_703af60f5f3147d5be26ebde6c56f83a` (attempts: 1)
- **Tag:** model/wrong-draft-count by rule `model`: ended `done`, expected `needs_approval`; 0 approval item(s), expected 1; 0 `slack.post_reply` draft(s), expected 1
- ended `done`, expected `needs_approval`
- 0 approval item(s), expected 1
- 0 `slack.post_reply` draft(s), expected 1

### `blocked-thread-missing-says-so`, run 1 of 3

- **Asked:** Draft my reply to the thread in #field-questions (channel C0FIELD, thread 1731609999.000900).
- **Task:** `tsk_162261e8c40446db84d6a29fa7048fdc` (attempts: 1)
- **Tag:** model/answered-beyond-sources by rule `model`: ended `needs_approval`, expected `done`; 1 approval item(s), expected 0; 1 `slack.post_reply` draft(s), expected 0; answered instead of saying its sources don't
- ended `needs_approval`, expected `done`
- 1 approval item(s), expected 0
- 1 `slack.post_reply` draft(s), expected 0
- answered instead of saying its sources don't

### `blocked-thread-missing-says-so`, run 2 of 3

- **Asked:** Draft my reply to the thread in #field-questions (channel C0FIELD, thread 1731609999.000900).
- **Task:** `tsk_bf7631a7f9d24487bfc173d4153c749c` (attempts: 1)
- **Tag:** model/answered-beyond-sources by rule `model`: ended `needs_approval`, expected `done`; 1 approval item(s), expected 0; 1 `slack.post_reply` draft(s), expected 0; answered instead of saying its sources don't
- ended `needs_approval`, expected `done`
- 1 approval item(s), expected 0
- 1 `slack.post_reply` draft(s), expected 0
- answered instead of saying its sources don't

### `blocked-thread-missing-says-so`, run 3 of 3

- **Asked:** Draft my reply to the thread in #field-questions (channel C0FIELD, thread 1731609999.000900).
- **Task:** `tsk_419db30b23ba4a36a51a62e3d05ea770` (attempts: 1)
- **Tag:** model/answered-beyond-sources by rule `model`: ended `needs_approval`, expected `done`; 1 approval item(s), expected 0; 1 `slack.post_reply` draft(s), expected 0; answered instead of saying its sources don't
- ended `needs_approval`, expected `done`
- 1 approval item(s), expected 0
- 1 `slack.post_reply` draft(s), expected 0
- answered instead of saying its sources don't

### `triage-answer-is-grounded`, run 2 of 3

- **Asked:** Check Slack for questions to me in #pm-team and draft replies. Skip anything I already answered.
- **Task:** `tsk_e7cdfc2e5ae94690b625f4df5d08c026` (attempts: 1)
- **Tag:** model/draft-content by rule `model`: `slack.post_reply` draft apr_eecd833f75ec4a63b920917565c4a66c doesn't say "docs.example.com"
- `slack.post_reply` draft apr_eecd833f75ec4a63b920917565c4a66c doesn't say "docs.example.com"

### `triage-drafts-only-what-needs-an-answer`, run 1 of 3

- **Asked:** [reply-assistant] Check Slack for messages since the last run that ask me something or need my answer, and draft a reply to each with the slack-reply skill. Skip anything I already answered.
- **Task:** `tsk_543858aa4436442b9cff99aea06ed451` (attempts: 1)
- **Timed out:** the runner timed out and cancelled it; its worker stopped (30.0 s)
- **Tag:** model/timed-out by rule `model`: timed out while `working`
- timed out while `working`

### `triage-drafts-only-what-needs-an-answer`, run 2 of 3

- **Asked:** [reply-assistant] Check Slack for messages since the last run that ask me something or need my answer, and draft a reply to each with the slack-reply skill. Skip anything I already answered.
- **Task:** `tsk_10652e02e450450da8fa15a1fb8be48e` (attempts: 1)
- **Tag:** tool/wrong-slack-channel by rule `draft-wrong-target`: `slack.post_reply` draft apr_0938f391ef7d4b64b7a5be65b283600a: `thread_ts` is `1731500000.000100`, allowed `1731600000.000100`, `1731601200.000600`, `1731601500.000700`
- 4 `slack.post_reply` draft(s), expected 3
- `slack.post_reply` draft apr_0938f391ef7d4b64b7a5be65b283600a: `thread_ts` is `1731500000.000100`, allowed `1731600000.000100`, `1731601200.000600`, `1731601500.000700`

### `triage-drafts-only-what-needs-an-answer`, run 3 of 3

- **Asked:** [reply-assistant] Check Slack for messages since the last run that ask me something or need my answer, and draft a reply to each with the slack-reply skill. Skip anything I already answered.
- **Task:** `tsk_1e944da9d06140cfb4cae5a7ef803614` (attempts: 1)
- **Tag:** tool/wrong-slack-channel by rule `draft-wrong-target`: `slack.post_reply` draft apr_d617847f88ff44a89515ab93511674c0: `thread_ts` is `1731500000.000100`, allowed `1731600000.000100`, `1731601200.000600`, `1731601500.000700`
- 4 `slack.post_reply` draft(s), expected 3
- `slack.post_reply` draft apr_d617847f88ff44a89515ab93511674c0: `thread_ts` is `1731500000.000100`, allowed `1731600000.000100`, `1731601200.000600`, `1731601500.000700`

