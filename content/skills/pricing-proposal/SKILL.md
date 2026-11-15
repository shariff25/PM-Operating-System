---
name: pricing-proposal
description: Propose a pricing change as a product decision, with the value metric, the packages and fences, who pays more and who pays less, how existing customers migrate, a test plan and risks, with the revenue effect from stats.cashflow quoted exactly, as a Confluence draft the PM approves. Use for "should we change pricing", "new plan", "packaging". Not for a general business case (use investment-case).
metadata:
  owner: "TODO"
  version: "0.1.0"
  model: "reasoning"
  data-class: "restricted"
  tools: "context.search context.read jira.search jira.read confluence.search confluence.read confluence.create_draft docs.search docs.read stats.cashflow"
  surfaces: "slack web cli"
  ships-in: "team-launch"
  track: "Company bets"
---

# /pricing-proposal

Treat price as part of the product: what it scales with, where the fences are, and who
wins and loses. The revenue effect comes from `stats.cashflow`, never your own arithmetic.

## Inputs
- The pricing question (required): a new package, a new value metric, a price change.
- Optional: current prices and plan mix, churn data, a target space.

## Steps
1. Read `/content/playbooks/company-bets.md` (sections 4 and 6),
   `/content/playbooks/levels.md` and `/content/templates/pricing-proposal.md`. Find an
   exemplar (`context.search`, domain `exemplars`) for depth and tone; never copy its facts.
2. Gather, and keep the list of sources: current plans and prices (`docs.search`,
   `context.search` in `products`), accounts by plan and size (`confluence.search`), what
   customers say about price (`confluence.search`, `jira.search`), and competitors' prices
   only from a dated source (`context.search` in `competitors`).
3. Ask up to three questions whose answers would change the proposal: the value metric
   customers understand, the accounts by segment, and how much notice existing customers
   are owed. Wait, unless the PM said "draft anyway".
4. Call `stats.cashflow` with one scenario per outcome (normally low, base and high), each
   with `upfront` (migration and build cost) and `years` of `{revenue, cost}` for the
   change, and `discount_rate` (default 0.1). Quote `payback_years` and `npv` exactly.
5. Draft from the template, headings unchanged and in order: the recommendation with the
   revenue range; the value metric and those that lost; packages with prices and fences;
   who pays more and who pays less, by segment with account counts and sources; migration
   with notice, grandfathering and its end date; the cheapest willingness-to-pay test; the
   risks, with what is hard to reverse once customers pay.
6. Check the draft against `/content/playbooks/bar.md` and the level's checks in
   `levels.md`. Fix what fails, once. Then propose it with `confluence.create_draft`, titled
   `Pricing: <the change>`, and reply with the recommendation in one line, what is waiting
   for approval, and the top open question.

## Do not
- State a competitor's price without a source and a date.
- Do the revenue, payback or NPV arithmetic yourself.
- Propose a change with no one paying more or less; look again.
- Leave existing customers' migration for later.
