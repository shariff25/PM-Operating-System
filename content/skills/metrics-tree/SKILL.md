---
name: metrics-tree
description: Define the metrics for a product or area as a tree with one north star, the input metrics that drive it, guardrail metrics, an exact definition of each (numerator, denominator, window, unit, source) and what the team will not optimise, as a Confluence draft the PM approves. Use when the PM asks "what should we measure", "what is our north star", "define success metrics" or "build a metrics tree". Not for designing an A/B test (use experiment-design) or reading one (use experiment-readout).
metadata:
  owner: "TODO"
  version: "0.1.0"
  model: "reasoning"
  data-class: "restricted"
  tools: "context.search context.read jira.search jira.read confluence.search confluence.read confluence.create_draft docs.search docs.read"
  surfaces: "slack web cli"
  ships-in: "pilot"
---

# /metrics-tree

Choose a few metrics that show customers getting value, and define each so two analysts
would compute the same number.

## Inputs
- The product or area (required).
- Optional: current dashboards or metric pages, the goal for the period, a target
  Confluence space.

## Steps
1. Read `/content/playbooks/metrics-and-experiments.md` (the Metrics half) and
   `/content/templates/metrics-tree.md`. Find an exemplar (`context.search`, domain
   `exemplars`) for depth and tone; never copy its facts.
2. Gather, and keep the list of sources:
   - who the customer is and the value they get (`context.search` in `products`,
     `personas`);
   - metrics already defined, with their baselines (`confluence.search`, `context.search`
     in `glossary`, `decisions`);
   - the goals in force (`confluence.search`, `jira.search`).
3. Ask up to three questions whose answers would change the tree: what value the customer
   gets and how we would see it in data, the goal for the period, and which numbers exist
   today. If the PM asked for metrics for a feature with no stated user or problem, the
   first question is whose behaviour should change. Wait, unless the PM said "draft anyway".
4. Draft from the template, headings unchanged and in order:
   - the north star recommended in the first lines, with the candidates that lost and why;
   - three to five inputs a team can move in a quarter, each with how it drives the north
     star, an owner, and leading or lagging;
   - a guardrail with a threshold for every target that could be gamed;
   - a definition for every metric: numerator, denominator, window, unit, exclusions,
     source;
   - what we will watch and not chase.
5. Check the draft against `/content/playbooks/bar.md`, using the metrics-tree row of its
   table. Fix what fails, once. A baseline or source you could not find is `[unverified]`
   and goes at the end of "What we won't optimise" as an open question with an owner.
6. Propose the draft with `confluence.create_draft`, titled `Metrics: <product or area>`,
   in the space the PM named or their default. Then reply with the recommended north star in
   one line, what is waiting for approval, and the top open question.

## Do not
- Pick revenue, sign-ups or page views as the north star.
- List more than five inputs, or an input no team can move.
- State a baseline or target that is not in a source. Leave the cell `[unverified]`.
- Claim a leading metric predicts a lagging one without the evidence. Label it a hypothesis.
