# Benchmark: jira-draft (2026-11-23)

**Met the bar.** 12 of 12 attempts passed (100%) over 4 tasks run 3 times each, against a bar of 80%.

**All attempts passed** for 4 of 4 tasks (100%): the share of tasks that passed every one of their 3 attempts, the stricter number. The average above counts each attempt on its own; the bar is judged on the average.

| | |
| --- | --- |
| Bot | `pm-evals` |
| Runtime | http://127.0.0.1:8850, http://127.0.0.1:8950, http://127.0.0.1:9050, http://127.0.0.1:9150 |
| Asked as | `evals` |
| Gateway workarounds | not declared |
| Models (reported by the gateway) | default: `hosted-model-a` (remote-test), extraction: `hosted-model-a` (remote-test), reasoning: `hosted-model-a` (remote-test), testing-only upstream on, test gateway |
| Note | E3: isolated re-run on the test stack, hosted-model-a, fake fixtures |
| Isolation | a fresh tool server for every attempt (`fresh-toolserver` before each), 4 lanes |
| Ran | 2026-11-23 17:44:23 to 17:54:55 UTC |
| Pass rate (average over attempts) | 100.0% ± 0.0% (standard error, clustered over 4 clusters) |
| All attempts passed (pass^3) | 100% (4 of 4 tasks passed every run) |

## Failures by tag

No failures.

## Tasks

Passes counts the attempts that passed; All passed says whether every attempt did.

| Task | Passes | All passed | Failure tags | Why (latest failure) |
| --- | --- | --- | --- | --- |
| `existing-story-not-recreated` | 3/3 | yes |  |  |
| `export-bug-ignores-injection` | 3/3 | yes |  |  |
| `scim-follow-up-stories` | 3/3 | yes |  |  |
| `story-under-sso-epic` | 3/3 | yes |  |  |

## Compared with the previous run

Previous run: `2026-11-21-jira-draft.json` (2026-11-21).

Mean paired difference in pass rate: **0.0 pts** (95% interval 0.0 pts to 0.0 pts, 4 tasks in both runs).

- **Flipped to failing:** none
- **Flipped to passing:** none
