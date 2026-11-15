---
name: investment-case
description: Write an investment case with low, base and high scenarios and their assumptions, payback and net present value from stats.cashflow quoted exactly, costs, risks, alternatives and what would change the answer, as a Confluence draft the PM approves. Use for "build the business case" or "what's the payback". Not for sizing a market (use opportunity-sizing) or pricing (use pricing-proposal).
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

# /investment-case

Make the case for money and people as a range, not a point. Payback and net present value
come from the `stats.cashflow` tool, never from your own arithmetic.

## Inputs
- The investment (required).
- Upfront cost, and revenue and cost by year for each scenario, with sources. Ask for any
  that are missing and not in the sources.

## Steps
1. Read `/content/playbooks/company-bets.md` (sections 3 and 6),
   `/content/playbooks/levels.md` and `/content/templates/investment-case.md`. Find an
   exemplar (`context.search`, domain `exemplars`) for depth and tone; never copy its facts.
2. Gather, and keep the list of sources: the sizing and customer evidence
   (`confluence.search`, `context.search`), cost estimates (`jira.search`,
   `confluence.search`), and the alternatives competing for the same people.
3. Ask up to three questions whose answers would change the case: the input that moves it
   most (often price or attach rate), the costs by year, and the discount rate if finance
   sets one. Wait, unless the PM said "draft anyway".
4. Call `stats.cashflow` with `scenarios` (1 to 5 of `{name, upfront, years: [{revenue,
   cost}]}`, normally low, base and high) and `discount_rate` (default 0.1 unless the PM or
   finance set one). Copy `net_by_year`, `cumulative_by_year`, `payback_years` and `npv`
   into the draft exactly as returned. If `payback_years` is `null`, say it never pays back
   within the years given.
5. Draft from the template, headings unchanged and in order: the recommendation with the
   base-case payback and NPV and the range; the opportunity; each scenario's assumptions with
   a source or `[unverified]`; costs and what the money would otherwise fund; the payback
   table and the tool's note that this is arithmetic, not a forecast; the riskiest
   assumption; at least two alternatives; what would change the answer.
6. Check the draft against `/content/playbooks/bar.md` and the level's checks in
   `levels.md`. Fix what fails, once. Then propose it with `confluence.create_draft`, titled
   `Investment case: <the investment>`, and reply with the recommendation and the base-case
   payback in one line, what is waiting for approval, and the top open question.

## Do not
- Compute, round differently or adjust payback, NPV, totals or cumulative figures. Quote
  the tool.
- Present one scenario as the answer, or drop the low case because it looks bad.
- Use an input with no source without marking it `[unverified]`.
- Leave out what the same money and people would otherwise fund.
