---
name: strategy-doc
description: Write a product strategy document with a recommendation, a diagnosis, one guiding choice, a short set of coherent actions, what the team will not do, the alternatives that lost, risks and how success is measured, as a Confluence draft the PM approves. Use when the PM asks for a strategy, a direction for a product area, a plan for a segment or year, or "where should we play and how do we win". Not for a single choice between named options (use decision-doc) or for one feature (use prd-draft).
metadata:
  owner: "TODO"
  version: "0.1.0"
  model: "reasoning"
  data-class: "restricted"
  tools: "context.search context.read jira.search jira.read confluence.search confluence.read confluence.create_draft docs.search docs.read"
  surfaces: "slack web cli"
  ships-in: "pilot"
  track: "Strategy and decisions"
---

# /strategy-doc

Write a strategy that makes a choice: a diagnosis, one guiding choice and the few actions
that follow from it.

## Inputs
- The area, segment or period the strategy covers (required).
- Optional: goals from leadership, data, past strategy pages, a target Confluence space.

## Steps
1. Read `/content/playbooks/strategy.md` and `/content/templates/strategy-doc.md`. Find an
   exemplar (`context.search`, domain `exemplars`) for depth and tone; never copy its facts.
2. Gather evidence for the diagnosis, and keep the list of sources:
   - product, personas and competitors (`context.search` in `products`, `personas`,
     `competitors`);
   - past decisions and strategy pages (`context.search` in `decisions`,
     `confluence.search`);
   - customer evidence and open work (`confluence.search`, `jira.search`).
3. Ask up to three questions whose answers would change the strategy: the goal and its date,
   the constraint (people, time, commitments), and the segment in question. If the request
   is a list of things to build with no problem behind it, the first question is what
   obstacle the strategy must overcome and for whom. Wait, unless the PM said "draft anyway".
4. Draft from the template, headings unchanged and in order:
   - the recommendation in the first three lines, labelled as a recommendation;
   - a diagnosis that names the crux, with a source for each fact;
   - one guiding choice a sensible competitor could reject;
   - three to five actions that reinforce each other, the one that tests the riskiest
     assumption first;
   - real non-goals: good ideas declined, each with what it costs;
   - at least two alternative strategies, one cheaper, and why each lost;
   - which decisions are one-way; a metric with baseline, target and date; a guardrail; a
     kill criterion.
5. Read `/content/playbooks/levels.md`. At the Staff or Principal level, end the
   document, after the template's headings, with `## Cross-team effects` (which teams
   pay, what they stop, who must agree). At Principal, add `## What must be true` after
   it (two or three conditions over 2 to 3 years, each with its failing signpost).
6. Check the draft against `/content/playbooks/bar.md` and the level's checks in
   `levels.md`. Fix what fails, once. What the sources can't settle goes at the end of
   "How we'll know" as open questions with owners.
7. Propose the draft with `confluence.create_draft`, titled `Strategy: <area and period>`,
   in the space the PM named or their default. Then reply with the recommendation in one
   line, what is waiting for approval, and the top open question.

## Do not
- Present goals as strategy. "Grow revenue 70%" is a target; say what choice gets there.
- List every team's projects as actions. If an action doesn't follow from the choice, cut it.
- Write non-goals nobody wanted anyway.
- Invent market sizes, competitor moves, numbers or dates. Unsourced figures are
  `[unverified]` and listed as open questions.
