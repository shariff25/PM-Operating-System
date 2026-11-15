---
name: okr-draft
description: Draft OKRs for a team or portfolio, with one outcome objective and at most four key results, each a number with a baseline, target, date and source, plus what we won't chase, how each is measured and the risks, as a Confluence draft the PM approves. Use for "write our OKRs" or goals for a quarter or year. Not for a metrics model (use metrics-tree) or reporting results (use business-review).
metadata:
  owner: "TODO"
  version: "0.1.0"
  model: "reasoning"
  data-class: "restricted"
  tools: "context.search context.read jira.search jira.read confluence.search confluence.read confluence.create_draft docs.search docs.read"
  surfaces: "slack web cli"
  ships-in: "team-launch"
  track: "Leading across teams"
---

# /okr-draft

Write OKRs that measure effect, not effort: one objective, at most four key results, each
a number someone can check.

## Inputs
- The team or portfolio and the period (required).
- Optional: the strategy it serves, last period's results, a target space.

## Steps
1. Read `/content/playbooks/leading-across-teams.md` (section 4),
   `/content/playbooks/metrics-and-experiments.md`, `/content/playbooks/levels.md` and
   `/content/templates/okrs.md`. Find an exemplar (`context.search`, domain `exemplars`)
   for depth and tone; never copy its facts.
2. Gather, and keep the list of sources:
   - the strategy and decisions the OKRs serve (`context.search` in `decisions`,
     `confluence.search`);
   - baselines and their definitions (`confluence.search`, `context.search`);
   - last period's OKRs and results (`confluence.search`).
3. Ask up to three questions whose answers would change the OKRs: the outcome that matters
   most this period, the baselines that exist, and what the team must not trade away. If
   the request is a list of launches, the first question is what each should change for
   customers. Wait, unless the PM said "draft anyway".
4. Draft from the template, headings unchanged and in order:
   - one objective, an outcome in plain words;
   - at most four key results, each a number with baseline, target, date and source;
   - turn any output the PM listed into the key result it should move, or drop it;
   - what we won't chase; how each key result is measured; one guardrail.
5. Check the draft against `/content/playbooks/bar.md` and the level's checks in
   `levels.md`. Fix what fails, once. A key result with no baseline stays, marked
   `[unverified]`, with measuring it as the first task, and goes under "Risks" as an open
   question with an owner.
6. Propose the draft with `confluence.create_draft`, titled `OKRs: <team or portfolio and
   period>`. Then reply with the objective in one line, what is waiting for approval, and
   the top open question.

## Do not
- Write more than one objective or more than four key results.
- Accept launches, features or documents as key results.
- Invent baselines or targets. Missing ones are `[unverified]` and open questions.
- Set a target with no date or no source.
