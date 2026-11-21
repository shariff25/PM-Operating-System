# Case card

A case card turns one real failure (or one real success worth keeping) into an eval task. Copy
this file, fill it in, and open a pull request that adds the task under
`evals/categories/<category>/tasks/<id>.yaml`. The runbook is `docs/runbooks/quality.md`.

**The rules:**
- **Paraphrase.** Write the ask in your own words. No Slack message text is copied in, not even
  a quote: Slack's developer policy forbids keeping or training on it, and Slack text leaves the
  trace after 24 hours anyway. Ticket keys, page titles and fixture names are fine.
- **Self-contained.** The task must make sense to someone who never saw the original thread:
  everything the bot needs is in the ask, or in a source it can read.
- **Checkable.** Every expectation can be checked deterministically, or by a calibrated judge.
- **The PM agrees.** The PM the case came from says yes before it goes into a shared category.
  Record who and when below.

## 1. Where it came from

| | |
| --- | --- |
| Category | `jira-draft` / `confluence-draft` / `slack-reply` / `docs-qa` / … |
| Bot | `personal` or the team bot's name |
| Ledger row or task | `apr_…` / `tsk_…` (a link to the console's task page is fine) |
| Outcome | rejected (reason) / edited (edit distance) / approved / should-not case |
| Failure tag | `model/…`, from `evals/taxonomy.yaml` (add a sub-tag by PR if none fits) |
| Written by | your handle, date |
| PM's OK to share | handle, date, and where they said so (it's fine to say "in a DM") |

## 2. What went wrong, in one or two sentences

Paraphrased. Say what the bot did and what the PM wanted instead.

## 3. The task

```yaml
# Case card: <one line on where it came from, no Slack text>
id: <lowercase-and-hyphens, matches the file name>
ask: "<the request, paraphrased and self-contained>"
expect:
  # Pick the checks that capture the failure; at least one.
  # For an answer:
  must_cite: []          # page URLs the answer must link
  must_include: []       # short facts the answer must state
  must_not_include: ["<think>"]
  # must_decline: true   # the sources don't answer it; the bot must say so (no must_cite)
  approvals: 0           # how many drafts it should end with
  # For drafts:
  # drafts:
  #   - tool: jira.create_issue
  #     count: 1
  #     require_fields: [project, issue_type, summary]
  #     allowed: {project: [PM]}
  #     must_include: []
  #     must_not_include: []
# cluster: <tasks that share a source share a cluster>
# regression: true       # once it's fixed, so the gate stops it breaking again
tags: [model/…]
```

### A case that took more than one message

If the failure only shows across messages (the bot lost the thread: it took a short reply as a
new request, asked again what the PM had answered, or dropped a detail the PM said to keep),
write a `thread` in place of `ask` and `expect`. Paraphrase each message, and give an `expect`
to each turn that must show the bot kept the thread; a turn with none only has to settle.
Tag it `memory/lost-the-thread`. A thread needs the runtime to carry history for eval tasks,
so leave off `regression: true` until it passes (`docs/evals.md`, Threads).

```yaml
id: <lowercase-and-hyphens, matches the file name>
thread:
  - ask: "<the first message, paraphrased and self-contained>"
    expect: {approvals: 0, must_include: ["?"]}   # e.g. the bot asked first
  - ask: "<the short reply>"
    expect: {must_not_include: ["<what it must not ask again>"]}
tags: [memory/lost-the-thread]
```

## 4. The reference outcome

What a good result looks like: the PM's approved final draft is fine, paraphrased if it quotes
Slack. Keep it short; it is for the reviewer and for judge labels, not for grading.

## 5. Checklist

- [ ] No Slack message text anywhere in the card or the task.
- [ ] The ask stands on its own.
- [ ] `pmos-evals check --category <category>` passes.
- [ ] The task fails on the version that failed (or would have), and passes on a good answer.
- [ ] The PM's OK is recorded above.
