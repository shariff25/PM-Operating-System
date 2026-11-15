# The quality owner's runbook

Every bot has a quality owner: the bot's `owner` in its `BOT.md`. The quality owner reads what
PMs rejected or rewrote, turns the cases worth keeping into eval tasks, and keeps the judges
honest. This is how PM OS learns from edits and rejections without training on anyone's Slack.

**The rules you work under:**
- **No Slack message text is stored durably.** The feedback ledger holds labels and metadata
  only, and Slack text leaves a task's trace after 24 hours. What you keep, in case cards,
  labels and review notes, you paraphrase.
- **A case goes into a shared category only with the PM's OK.** The PM it came from says yes
  first.
- **Deterministic checks and people outrank LLM judges.** A judge blocks only once it is
  calibrated.
- **Approval rate is a tripwire, never a target.** A falling rate tells you to look. Don't
  tune anything to raise it: a bot that drafts less, or drafts blander, would.

| When | What | Time |
| --- | --- | --- |
| Every weekday morning | Read the triage digest | 5–15 minutes |
| Every 2–4 weeks | The 100-trace review | half a day |
| When a case is worth keeping | Write a case card | 20 minutes |
| When a judge has labels | Calibrate it | 30 minutes |
| When a PR touches your bot | Read the eval gate's result | 5 minutes |

## Daily: the triage digest

At 09:00 on weekdays (`PMOS_TRIAGE_AT`, in `PMOS_DEFAULT_TZ`), PM OS sends you a Slack DM with
yesterday's ledger rows for your bots that were **rejected**, or **edited** with an edit
distance of at least 0.3 (`PMOS_TRIAGE_MIN_EDIT`). No DM means nothing qualified.

Each row shows the tool, who the draft was for, the outcome and reason, the edit distance, and
a link to the console's task page.

**For each row:**
1. **Open the task page today.** The trace still holds the Slack text for 24 hours; after that
   only the metadata is left.
2. **Decide which it is:**
   - **A one-off:** the PM changed their mind, or the request was unusual. Nothing to do.
   - **A known failure mode:** note its taxonomy sub-tag (`evals/taxonomy.yaml`) in your
     review notes, as a count, not a quote.
   - **Worth keeping:** a clear, repeatable failure, or a good example of a should-not case.
     Write a case card (below), or note the task id to write one this week.
3. **Look at the reasons.** Many `wrong_target` rejections point at a `tool/wrong-*` problem;
   `should_not_draft` at a skill that drafts too eagerly (`policy/should-not-draft`); `tone` at
   instructions, not facts.

**Reply-assistant drafts** (`slack.post_reply`, `docs/slack-reply.md`) arrive in the same
digest. Two patterns are worth a case card:
- **Many expired drafts:** the bot drafts replies nobody wanted; look for threads it should
  have skipped.
- **Rejections with `should_not_draft`:** an FYI or an already-answered thread got a draft.

A reply blocked by the audience check stays pending until it's rejected or expires. That's the
check working, not a quality failure.

For the week as a whole, `deploy/pmos-admin ledger --since 7d` prints counts by outcome, the
label coverage (decided or expired rows; phase 0 wants 95%) and the median edit distance.

## Every 2–4 weeks: the 100-trace review

The digest shows the worst rows. The review shows what is normal, and finds failure modes no
check or tag knows yet. Block out half a day.

1. **Pick 100 traces.** Export the ledger for the period
   (`deploy/pmos-admin ledger --export > /tmp/ledger.jsonl`) and pick about 100 runs: all the
   rejected ones, then edited ones from the highest edit distance down, then a random handful
   of approved-unchanged ones, which are often wrong in ways nobody noticed. Spread them across
   PMs and tools.
2. **Open coding.** For each trace, open its task page and write one or two plain sentences on
   the first thing that went wrong, or "fine". Don't use the taxonomy yet; describe what you
   see ("drafted the story in ENG though the PM said PM", "quoted a port the page doesn't
   list"). Paraphrase; never paste message text.
3. **Axial coding.** Group the notes into failure modes. Name each group, count it, and map it
   to a taxonomy sub-tag. When none fits, add one by pull request to `evals/taxonomy.yaml`, with
   a description that tells the next reviewer how to recognise it.
4. **Pick one or two modes to act on:** the most common, or the most costly to a PM. For each:
   - write case cards for two or three representative traces, so the benchmark catches it;
   - if it can't be checked deterministically, it may need a judge: start collecting labels;
   - open an issue for the fix (a skill's instructions, a role, a tool, the harness).
5. **Keep the counts** (mode, count, period) in your review notes, so the next review can see
   whether a fix worked. Delete the open-coding notes once the counts are kept.

## When worth it: writing a case card

A case card turns one real case into an eval task. The template is `evals/case-card.md`.

1. **Copy the template** and fill it in: where it came from (the ledger row or task id), what
   went wrong, the task, and the reference outcome. The PM's approved final draft is a fine
   reference, paraphrased if it quotes Slack.
2. **Paraphrase everything.** The `ask` is the request in your own words, self-contained, with
   no Slack text. Ticket keys and page titles are fine.
3. **Make it checkable.** Use `must_include` for facts, `drafts:` for where a draft goes and
   what it must say, `must_decline` for a question the sources don't answer. Prefer a few sharp
   checks to many loose ones.
4. **Get the PM's OK.** Ask the PM the case came from whether it may go into a shared category,
   and record who and when in the card. No OK, no shared task: keep it out of the repo.
5. **Open a pull request** adding `evals/categories/<category>/tasks/<id>.yaml`. Run
   `pmos-evals check --category <category>` first. The eval gate runs on the PR.
6. **Once the failure is fixed,** mark the task `regression: true`, so the gate fails if it
   ever breaks again.

The seed tasks in `jira-draft`, `confluence-draft`, `slack-reply` and `pm-craft` are
placeholders on fake fixtures. Replace them with case cards as these come in; give a category
a `pass_bar` once it holds real cases.

## When one exists: calibrating a judge

A judge (`evals/judges/<name>.yaml`) is advisory until it is calibrated against people. Only a
calibrated judge can fail the eval gate.

1. **Collect labels.** Each label is one line of `evals/judges/<name>.labels.jsonl`:
   `{"input": "...", "output": "...", "label": true}`. The input is what the judge would see
   (the request, and what the bot read); the output is the answer or draft; `label` is your
   answer to the judge's question (`true` is yes, the output passes). Paraphrase as for case
   cards.
   - **At least 100,** with both answers well represented; aim for at least 30 of each.
   - **From real outputs,** including hard cases near the line, not only obvious ones.
   - **Labelled before you see the judge's verdict,** so it doesn't sway you.
2. **Calibrate** on the Spark, where the gateway is:
   ```sh
   cd /opt/pmos
   deploy/pmos-admin evals-stack judge calibrate <name>
   ```
   This runs on the eval stack, using the content checkout's `evals/` and the live gateway's
   judge model. Commit the `<name>.calibration.json` it writes in
   `/srv/pmos/content-repo/evals/judges/`.
   It prints n, the true-positive rate (TPR: says yes when you did) and the true-negative rate
   (TNR: says no when you did), and whether the judge now blocks, and writes
   `<name>.calibration.json`.
3. **Read the numbers.** It blocks only with n ≥ 100 and TPR and TNR both ≥ 0.8.
   - **Too low:** read the labels it got wrong. Sharpen the question (say exactly what counts),
     or fix labels you now disagree with. Changing the question changes its hash, so
     calibrate again.
   - **Still too low:** leave it advisory. An advisory judge is still useful in reports.
4. **Commit** the labels and the calibration file together, by pull request.
5. **Calibrate again** after you change the question or the alias (it turns advisory until you
   do), and after a model swap behind its alias (`docs/runbooks/model-swap.md`): the alias
   stays the same, so the judge stays blocking, but the model answering is new.

## Reading the eval gate's result

A pull request that changes a role, a skill, a bot, the evals or the gateway's model config
runs the eval gate on the Spark. Its summary is the job summary of the "Eval gate" check (or,
until the runner is registered, pasted into the PR by a platform owner). One section per
affected category:

- **"passed" or "FAILED"**, then the baseline (the category's latest report on `main`) and the
  candidate (this PR's run).
- **Pass rate ± standard error, and pass^k,** for both. Two runs within about two standard
  errors of each other are the same, as far as the benchmark can tell.
- **Fails because:** each reason the gate failed:
  - **a regression task flipped to failing:** a case that broke before and was fixed is broken
    again. Treat it as a real regression;
  - **the pass rate fell beyond noise:** the 95% interval for the change is wholly below 0.
    Look at the "Compared with the baseline" table for the tasks that moved;
  - **a check passed in the baseline and now fails:** a specific expectation (a citation, a
    fact, a draft's target) that held on every baseline run now fails on every run. The
    check's name says which;
  - **a blocking judge passed it in the baseline and now fails it.**
- **Compared with the baseline:** the mean paired difference and its interval, the flips, new
  and removed tasks, and every task whose rate changed.
- **Judges:** each judge, blocking or advisory, and how often it said no. Advisory judges
  never fail the gate; read their "no"s as leads.

**What to do with a failure:** read the failing tasks' details in the candidate report
(written on the Spark, linked from the job log), and open their task pages. If a failure is
the task's fault (a phrase every good answer paraphrases), fix the task in the same PR. If the
change really made things worse, fix the change. Don't lower a bar or delete a task to pass the
gate. With few tasks the interval is only approximate: if it barely misses 0, rerun once
before acting.

**"No baseline"** means the category has no report on `main` yet: the gate passes and says so.
Merge a report from a normal run to give it one.
