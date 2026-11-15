---
name: decision-doc
description: Write a decision document that recommends one option, with the context, at least two real options, the criteria, the tradeoffs, how reversible the decision is and the open questions, as a Confluence draft the PM approves. Use when the PM must choose or get a choice approved, for example build or buy, build or partner, option A or B, now or later, or says "write up the decision" or "help me decide". Not for setting direction across a whole area (use strategy-doc) or for ranking a backlog (use prioritize).
metadata:
  owner: "TODO"
  version: "0.1.0"
  model: "reasoning"
  data-class: "restricted"
  tools: "context.search context.read jira.search jira.read confluence.search confluence.read confluence.create_draft docs.search docs.read"
  surfaces: "slack web cli"
  ships-in: "pilot"
---

# /decision-doc

Help the PM make one decision and get it agreed: a recommendation first, then the options
and what each costs.

## Inputs
- The question being decided (required).
- Optional: the options on the table, the decider, the deadline, a target Confluence space.

## Steps
1. Read `/content/playbooks/strategy.md` (section 5, reversible and one-way decisions),
   `/content/playbooks/product-sense.md` (steps 6 and 7) and
   `/content/templates/decision-doc.md`. Find an exemplar (`context.search`, domain
   `exemplars`) for depth and tone; never copy its facts.
2. Gather evidence, and keep the list of sources:
   - earlier decisions on the same question (`context.search` in `decisions`,
     `confluence.search`);
   - customer evidence, costs and constraints (`confluence.search`, `jira.search`);
   - product and competitor facts (`context.search` in `products`, `competitors`,
     `docs.search`).
3. Ask up to three questions whose answers would change the recommendation: who decides and
   by when, what matters most, and what is fixed. If the question is which solution to pick
   and nobody has said who has the problem, that is the first question. Wait, unless the PM
   said "draft anyway".
4. Draft from the template, headings unchanged and in order:
   - recommend one option in the first three lines, labelled as a recommendation, with the
     outcome it should produce and a guardrail;
   - context: the user, the problem and the evidence before any option;
   - at least two real options plus the cheapest thing that could work, each described as
     its best advocate would;
   - criteria in order of weight, set before scoring;
   - tradeoffs with evidence, what the pick gives up, why each loser lost;
   - reversible or one-way, the cost of undoing it, the riskiest assumption and its
     cheapest test, a review date.
5. Check the draft against `/content/playbooks/bar.md`. Fix what fails, once. What the
   sources can't settle goes under "Open questions" with a suggested owner.
6. Propose the draft with `confluence.create_draft`, titled `Decision: <the question>`, in
   the space the PM named or their default. Then reply with the recommendation in one line,
   what is waiting for approval, and the top open question.

## Do not
- Lay out options and leave the choice to the reader. Recommend, then let the PM overrule.
- Add strawman options to make the pick look good.
- Call a decision reversible without saying what undoing it costs.
- Invent costs, estimates, customer names or dates. Missing figures are open questions.
