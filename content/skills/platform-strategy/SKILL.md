---
name: platform-strategy
description: Write a platform strategy with engineering, covering build, buy or partner for a capability, what becomes a shared platform and what stays in each product, what must be true, the sequence and the risks, as a Confluence draft the PM approves. Use for "should this be a platform" across products. Not for one build-or-partner choice (use decision-doc) or a product's direction (use strategy-doc).
metadata:
  owner: "TODO"
  version: "0.1.0"
  model: "reasoning"
  data-class: "restricted"
  tools: "context.search context.read jira.search jira.read confluence.search confluence.read confluence.create_draft docs.search docs.read"
  surfaces: "slack web cli"
  ships-in: "team-launch"
  track: "Company bets"
---

# /platform-strategy

Decide what the company builds once and shares, and what it buys or partners for. Written
with engineering: a platform strategy without the engineering view is a wish.

## Inputs
- The capability (required): identity, billing, audit, an AI layer.
- Optional: the products that need it, the engineering lead to write with, a target space.

## Steps
1. Read `/content/playbooks/company-bets.md` (sections 5 and 6),
   `/content/playbooks/levels.md` and `/content/templates/platform-strategy.md`. Find an
   exemplar (`context.search`, domain `exemplars`) for depth and tone; never copy its facts.
2. Gather, and keep the list of sources:
   - which products need the capability and in what form (`jira.search`,
     `confluence.search`, `context.search` in `products`);
   - architecture notes and what scale breaks (`confluence.search`);
   - vendors and partners (`context.search` in `partners`, `docs.search`) and earlier
     decisions (`context.search` in `decisions`).
3. Ask up to three questions whose answers would change the strategy: which engineering
   lead it is written with, which products must use it and when, and what must stay in our
   control. Wait, unless the PM said "draft anyway".
4. Draft from the template, headings unchanged and in order:
   - the recommendation in the first three lines: build, buy or partner, and what it costs;
   - the diagnosis: who needs it, what breaks at scale, what is true about us;
   - build, buy and partner, each with cost to start and run, control and why it lost or won;
   - what becomes a platform (needed by two or more products in the same form) and what
     stays in each product;
   - two or three conditions that must be true, with signposts; the sequence by quarter.
5. Check the draft against `/content/playbooks/bar.md` and the level's checks in
   `levels.md`. Fix what fails, once. Mark each engineering claim not confirmed by the
   engineering lead `[unverified]` and list it under "Risks".
6. Propose the draft with `confluence.create_draft`, titled `Platform strategy: <the
   capability>`. Then reply with the recommendation in one line, what is waiting for
   approval, and which engineering claims still need the lead's review.

## Do not
- Make something a platform because one product needs it.
- Put a third party in a critical path without saying what leaving would cost.
- Present engineering estimates nobody in engineering gave.
- Invent vendor prices or capabilities. Unsourced ones are `[unverified]`.
