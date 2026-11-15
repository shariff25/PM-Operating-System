---
name: competitive-brief
description: Write a competitive brief on one competitor for one segment, from evidence (won and lost deals, customer quotes, dated public docs and pricing), covering the bottom line, where they win, where we win, the evidence, what to do and the unknowns, as a Confluence draft the PM approves. Use when the PM asks "how do we compare to <competitor>", "why are we losing to them", "write a battlecard" or "what should we do about <competitor>". Not a feature grid. Not for choosing between our own options (use decision-doc) or setting direction for an area (use strategy-doc).
metadata:
  owner: "TODO"
  version: "0.1.0"
  model: "reasoning"
  data-class: "restricted"
  tools: "context.search context.read jira.search jira.read confluence.search confluence.read confluence.create_draft docs.search docs.read"
  surfaces: "slack web cli"
  ships-in: "pilot"
  track: "Customer and market"
---

# /competitive-brief

Say where a competitor beats us, with which customers and why, and what we should do about
it. Evidence, not a feature grid.

## Inputs
- The competitor (required).
- Optional: the segment or deals in question, the decision the brief feeds, a target
  Confluence space.

## Steps
1. Read `/content/playbooks/customer-insight.md` (competitive briefs) and
   `/content/templates/competitive-brief.md`. Find an exemplar (`context.search`, domain
   `exemplars`) for depth and tone; never copy its facts.
2. Gather, and keep the list of sources, each with the date it was read or written:
   - what the team already knows (`context.search` in `competitors`, `decisions`);
   - won and lost deals with the stated reason, and call notes that name the competitor
     (`confluence.search`, `jira.search`);
   - what our product does today, so "where we win" is accurate (`docs.search`,
     `context.search` in `products`).
3. Ask up to three questions whose answers would change the brief: which segment, which
   deals, and what decision it feeds. If the PM asks to match a competitor's feature, the
   first question is which of our customers have the problem that feature solves. Wait,
   unless the PM said "draft anyway".
4. Draft from the template, headings unchanged and in order:
   - the bottom line first: where they win, where we win, what we should do, for which
     segment, labelled as a recommendation;
   - wins and losses by customer and job, each tied to evidence with a count and a date;
   - what would make one of our customers switch, and one of theirs;
   - actions ranked, with the options considered (doing nothing among them) and why the
     others lost; what we will not copy, and why;
   - the signal that tells us it worked: a metric, a number, a date, and a guardrail.
5. Check the draft against `/content/playbooks/bar.md`, using the competitive-brief row of
   its table. Fix what fails, once. Field hearsay, stale facts, the riskiest assumption and
   its cheapest check go under "Unknowns" with a suggested owner.
6. Propose the draft with `confluence.create_draft`, titled `Competitive brief:
   <competitor> · <segment>`, in the space the PM named or their default. Then reply with
   the bottom line in one line, what is waiting for approval, and the top unknown.

## Do not
- Build a feature grid. Grids reward the longest list, not the customer outcome.
- State a competitor fact without a source and a date. Hearsay is `[unverified]`.
- Recommend matching a feature by default. Copying is a choice; say why.
- Describe the competitor in sneering or flattering terms. Plain facts only.
- Put restricted deal data in a brief meant for a wider audience unless the PM says so.
- Invent deals, prices, quotes or win rates.
