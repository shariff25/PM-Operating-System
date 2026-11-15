---
name: roadmap-tradeoffs
description: Recommend where to invest across teams over 4 to 6 quarters and what stops to pay for it, with options, what each team gives up, the sequence and how we'll know, from jira.rollup counts, as a Confluence draft the PM approves. Use for "what should we fund", "what do we drop", re-planning a roadmap across teams. Not for status (use portfolio-review) or one choice inside one team (use decision-doc).
metadata:
  owner: "TODO"
  version: "0.1.0"
  model: "reasoning"
  data-class: "restricted"
  tools: "context.search context.read jira.search jira.read confluence.search confluence.read confluence.create_draft docs.search docs.read jira.rollup"
  surfaces: "slack web cli"
  ships-in: "team-launch"
  track: "Leading across teams"
---

# /roadmap-tradeoffs

Make a choice between investments, and say what stops to pay for it. A trade-off that
stops nothing has not been made.

## Inputs
- The investments in question and the period (required).
- Optional: the goal, the people available, the decider, a target space.

## Steps
1. Read `/content/playbooks/leading-across-teams.md` (sections 1 to 3),
   `/content/playbooks/levels.md` and `/content/templates/roadmap-tradeoffs.md`. Find an
   exemplar (`context.search`, domain `exemplars`) for depth and tone; never copy its facts.
2. Gather, and keep the list of sources:
   - call `jira.rollup` over the teams involved (`group_by` `project` or `epic`), and quote
     its counts and links exactly; say so if the result is `truncated`;
   - the goal and current strategy (`context.search` in `decisions`, `confluence.search`);
   - customer evidence and the cost in people for each option (`confluence.search`,
     `jira.search`).
3. Ask up to three questions whose answers would change the choice: the goal and its date,
   the people that can move, and what is already promised to customers. Wait, unless the PM
   said "draft anyway".
4. Draft from the template, headings unchanged and in order:
   - the recommendation in the first three lines: where to invest, what stops, the outcome;
   - at least two real options, one cheaper, with people and quarters and why each lost;
   - under "What we stop", every project or request that stops or slips, and its team;
   - one line per team under "Cross-team effects": gives up, gains, must agree;
   - the sequence by quarter, with the dependency that sets each date from the rollup;
   - the riskiest assumption with its test; a metric with baseline, target and date.
5. Check the draft against `/content/playbooks/bar.md` and the Staff check in `levels.md`.
   Fix what fails, once. What the sources can't settle goes at the end of "How we'll know"
   as open questions with owners.
6. Propose the draft with `confluence.create_draft`, titled `Roadmap trade-offs: <the choice
   and period>`. Then reply with the recommendation and what stops in one line, what is
   waiting for approval, and the top open question.

## Do not
- Fund every option, or describe the cost as "some engineering time".
- Count issues yourself or adjust the rollup's numbers.
- Leave a team out of "Cross-team effects" because the news is bad for it.
- Invent estimates, customer commitments or dates. Unsourced figures are `[unverified]`.
