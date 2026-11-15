# The improvement loop, phases 0 and 1: build spec

**Source:** `reports/Self improvement loops for PM OS.md` (deep research, 2026-11-15).
**Decision:** build phase 0 now, plus the tooling phase 1 needs. Shadow runs, canary flags, the
improver bot (phase 2) and training (phase 3) come later, each after its phase's gate is met.

**The rules every part follows:**
- **The evaluator is separate.** Nothing being improved can read or write the evals, graders,
  judges or holdout.
- **Deterministic checks and human judgment outrank LLM judges.** A judge only blocks after it is
  calibrated.
- **No Slack message text is stored durably.** The ledger holds labels and metadata only. Slack's
  developer policy forbids training on Slack data, and the durable eval store needs legal
  review, so case cards are paraphrased by their owner.
- **Approval rate is a tripwire, never a target.** Nothing optimizes for it.

## 1. The feedback ledger (tool server)

There is one row per approval item, so one per drafted outbound action. It is kept in memory and
in Postgres (migration `0007_ledger.sql`), and is written as part of creating and deciding
approval items, so a row is never missing.

| Field | Meaning |
| --- | --- |
| `id` | The approval item's id |
| `run_id` | The task id (`tsk_…`) or skill run id |
| `user` | The PM the draft is for |
| `bot` / `skill` | Who drafted it |
| `tool` | The registry name, such as `jira.create_issue` |
| `content_sha` | The content repo's `HEAD` when the item was created. It covers the role, skill and bot files. Read at load and reindex with `git rev-parse HEAD`; `null` outside a git checkout. |
| `model_alias` | The drafting role's gateway alias (`pmos/default`). The concrete model and workarounds are joined from the gateway's usage log by `run_id` at report time. |
| `outcome` | `pending`, `approved` (unchanged), `edited` (approved after an edit), `rejected`, `failed` (approved, but the write failed), or `expired`. `expired` is not stored: a `pending` row older than `PMOS_LEDGER_EXPIRE_DAYS` (default 7) is reported as `expired`. |
| `edit_distance` | `0.0`–`1.0`, the normalized Levenshtein distance between the original and the final text. The text is the item's string arguments, sorted by key and joined. `0.0` when approved unchanged; `null` when rejected or pending. |
| `reason` | Only for `rejected`: `wrong_facts`, `wrong_target`, `tone`, `incomplete`, `should_not_draft` or `other`. `null` when no reason was given. |
| `created_at`, `decided_at`, `decided_by` | |

**The decision API.** `POST /internal/approvals/{id}/decision` gains an optional `reason`, taking
the values above. It is accepted only with `reject`; with anything else it gets a `422`. The audit
event's `detail` records the reason.

**Reading the ledger.** `GET /internal/ledger?since=<rfc3339>&until=&bot=&outcome=<a,b>&min_edit=<0..1>&limit=<≤1000>`
returns `{rows: [...]}`, newest first.
- The orchestrator may call it.
- The Slack bot may call it too, for the triage digest. It gets every field; rows hold no message
  text.

**Summary.** `GET /internal/ledger/summary?since=&bot=` returns, per bot and tool:
- counts by outcome;
- the label coverage, meaning the share of rows that are decided or expired (phase 0's 95% gate);
- the median `edit_distance`;
- the rejection reasons.

**Admin access.** `pmos-admin ledger [--since 7d]` prints the summary. `pmos-admin ledger --export`
writes JSON lines to stdout.

## 2. A bounded retry for invalid drafts (tool server)

A write call refused for invalid arguments (`InvalidArgs`: a bad Jira field, a malformed page, a
schema error) is the one case where reflection helps, because the model gets an external error
message.
- **One correction.** Within one session, a write tool may be refused for invalid arguments
  **twice**. The second refusal's message ends "This is the last attempt for this action. Tell the
  PM what was wrong instead of retrying."
- **Then refused for the session.** Any further call to that write tool in the session is refused
  with that message, without calling the connector.
- **Audited.** Each refusal is audited as `denied`, with `detail: "invalid draft (n of 2)"`.

## 3. Drafts for evals (bot runtime)

`GET /v1/tasks/{id}/drafts` returns `{drafts: [{id, tool, args, status}]}` for the task's approval
items.
- **Who may call it:** the `console` and `evals` clients.
- **Where the data comes from:** the runtime fetches each item from the tool server
  (`GET /internal/approvals/{id}`) with its service token.
- **Why:** the evals client never holds a tool-server token.

## 4. Slack

**Reasons on reject.** **Reject** on an approval card no longer decides at once. It updates the
card in place to six reason buttons, each a single click:
- Wrong facts
- Wrong place
- Tone
- Incomplete
- Shouldn't have drafted
- Other

A **Back** button restores the card. Choosing a reason sends the decision with `reason`.

**Daily triage digest.** At `PMOS_TRIAGE_AT` (default `09:00` in `PMOS_DEFAULT_TZ`) on weekdays,
Slack DMs each bot's quality owner the past day's ledger rows that are `rejected`, or `edited`
with `edit_distance ≥ PMOS_TRIAGE_MIN_EDIT` (default 0.3).
- **Who the quality owner is:** the bot's `owner` from the runtime's bot list. A bot whose owner
  isn't a team member is logged and skipped.
- **What each row shows:**
  - the tool;
  - who it was for;
  - the outcome and reason;
  - the edit distance;
  - a link to the console's task page (`PMOS_CONSOLE_PUBLIC_URL/tasks/<run_id>`), if set.
- **The case-card prompt:** the digest ends with "Worth keeping? Write a case card:
  `evals/case-card.md`", plus a reminder that Slack text leaves the trace after 24 hours.
- **When nothing happened:** no message.
- **Delivery:** at most once per owner per day. The last-sent date is kept in `PMOS_SLACK_STATE_DIR`.

## 5. pmos-evals

**Repeats and pass^k.**
- **How many runs:** `repeats:` in `category.yaml` (default 1), overridden by `--repeats N`.
  Phase 0 sets docs-qa to 3.
- **What the report adds:**
  - each task's passes out of k;
  - the pass rate over every attempt;
  - **pass^k**, the share of tasks that passed all k runs;
  - the **standard error** of the pass rate, clustered by each task's `cluster`. `cluster` is an
    optional task field; the default is its first `must_cite` URL, else the task id.

**Comparing runs.**
- **Against the previous run:** each `run` compares itself with the category's previous report
  and shows:
  - the paired per-task difference in passes;
  - a 95% interval for the mean paired difference;
  - **flips**: a task that passed all repeats and now fails all, or the reverse.
- **On demand:** `pmos-evals compare <report> <report>` does the same for any two reports.

**The gate.** `pmos-evals gate --category <c> [--baseline <report>]` is the check a change must pass.
- **Baseline:** defaults to the latest report on `main`.
- **It fails (exit 1) when any of these hold:**
  - a `regression: true` task flips to failing;
  - the paired difference's interval lies wholly below 0;
  - a deterministic check that passed in the baseline now fails on any task;
  - a **blocking** judge (below) fails a task it passed in the baseline.

  Otherwise it exits 0.
- **Output:** a markdown summary, ready for a PR comment.

**New expectations.**
- **`must_decline: true`.** The answer must say it can't answer from its sources. It matches any
  of `decline_phrases` in `evals/config.yaml`, and it must have no `must_cite`. These are the
  should-not cases; docs-qa gains at least 3 of them, questions the support docs don't answer,
  each checked against the docs.
- **`drafts:`.** Checks on the task's drafts (section 3). Each entry gives:
  - `tool`;
  - `require_fields` (non-empty arguments);
  - `allowed` (an argument's value must be one of a list, such as `project: [PMOS]`);
  - `must_include` and `must_not_include` (on the draft's text);
  - `count`.

**Failure taxonomy.**
- **Where it lives:** `evals/taxonomy.yaml` lists second-level tags under the five existing ones,
  such as `tool/wrong-confluence-space`, `model/missing-citation`,
  `model/answered-beyond-sources` and `harness/approval-count`, each with a description.
- **Who applies it:** the automatic tagger emits a sub-tag where a deterministic failure maps to
  one. Owners add sub-tags by pull request.
- **Checks:** `pmos-evals check` validates the taxonomy and every tag used.

**LLM judges.**
- **Definition:** `evals/judges/<name>.yaml` gives:
  - `name`;
  - `failure_mode`, a taxonomy tag;
  - `question`, binary and answered yes or no about the answer or draft;
  - `alias`, the gateway alias, default `pmos/extraction`, so the judge is a different model
    family from the drafter;
  - `applies_to`, the categories;
  - `labels`, a JSON-lines file of `{input, output, label}`.
- **Calibration:** `pmos-evals judge calibrate <name>` runs the judge over its labels and writes
  `evals/judges/<name>.calibration.json` with the date, n, TPR and TNR, the alias, and a hash of
  the question.
- **Blocking or advisory:** a judge **blocks** only when all of these hold:
  - n ≥ 100;
  - TPR ≥ 0.8 and TNR ≥ 0.8;
  - the hash and alias still match.

  Otherwise it is **advisory**: reported, never failing.
- **How it's called:** through the gateway, with `PMOS_EVALS_GATEWAY_TOKEN` (the `evals` client in
  `PMOS_GATEWAY_TOKENS`) and `PMOS_GATEWAY_URL`.
- **Shipped now:** one example judge, `faithful-to-cited-page` for docs-qa, with no labels yet, so
  it is advisory.

**New categories:** `jira-draft`, `confluence-draft` and `slack-reply`.
- **Bot:** each runs against `bot: personal`, meaning `pm-<requested_by>`.
- **Seed tasks:** 3 each, against the fake connectors' fixtures. They are marked "seed: replace
  with case cards", with `drafts:` checks.
- **Launch bar:** none until real case cards exist (`pass_bar` unset means it reports only).

**Case cards.** `evals/case-card.md` is the template.
- **What a case card is:** a self-contained, paraphrased task (an `ask` plus `expect`), with no
  raw Slack text, and a reference outcome (the PM's approved final draft is fine).
- **Who may share one:** the PM it came from must agree before it goes into a shared category.

## 6. The eval gate in CI

`.github/workflows/evals-gate.yml` runs on pull requests that change:
- `content/skills/**`, `content/roles/**` or `content/bots/**`;
- `evals/**`;
- `deploy/gateway.yaml`.

It runs on a **self-hosted runner on the Spark** (labels `self-hosted, pmos-spark`) with
`deploy/pmos-admin evals-gate`. That runs `pmos-evals gate` for every category whose bot's role
or skills changed, or all of them for gateway and evals changes, then posts the markdown as the
job summary.

**Registering the runner** is in `docs/runbooks/upgrade.md`. Until a runner is registered, the
job waits; a platform owner runs `pmos-admin evals-gate` by hand and pastes the result into the
PR.

The GitHub-hosted CI also runs `pmos-evals check` on every PR.

## 7. Docs and status

- **Cadence:** `docs/evals.md` covers the new commands and fields.
- **Quality-owner runbook:** `docs/runbooks/quality.md`:
  - **daily:** the triage digest;
  - **every 2–4 weeks:** the 100-trace review;
  - **when worth it:** writing a case card;
  - **when one exists:** calibrating a judge.
- **Plan status:** "Improvement loop phases 0–1 (tooling)".
- **Changelog:** "PM OS can now learn from your edits and rejections."
