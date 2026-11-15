---
name: opportunity-sizing
description: Size a product opportunity bottom-up as a low, base and high range per year, with the arithmetic shown, every assumption sourced or flagged, the assumption that moves the answer most and the cheapest test of it, as a Confluence draft the PM approves. Use when the PM asks "how big is this", "is it worth building", "what's the revenue upside", "size the market" or needs a number for a business case. Not for ranking several items against each other (use prioritize) or for a full build-or-not decision (use decision-doc).
metadata:
  owner: "TODO"
  version: "0.1.0"
  model: "reasoning"
  data-class: "restricted"
  tools: "context.search context.read jira.search jira.read confluence.search confluence.read confluence.create_draft docs.search docs.read"
  surfaces: "slack web cli"
  ships-in: "pilot"
---

# /opportunity-sizing

Put an honest range on an opportunity and say which assumption to test first. Count from
the team's own data; use the market only as a sanity check.

## Inputs
- The opportunity (required): the problem, the segment, and what we would charge or save.
- Optional: counts of customers or workspaces, prices, conversion data, a target space.

## Steps
1. Read `/content/playbooks/customer-insight.md` (sizing an opportunity) and
   `/content/templates/opportunity-sizing.md`. Find an exemplar (`context.search`, domain
   `exemplars`) for depth and tone; never copy its facts.
2. Gather the inputs, and keep the list of sources, each with its date:
   - how many customers have the problem, from the team's own counts (`confluence.search`,
     `context.search` in `products`, `decisions`);
   - evidence of the problem: calls, tickets, asks by account (`confluence.search`,
     `jira.search`);
   - prices, plans and comparable features with their adoption (`docs.search`,
     `confluence.search`).
3. Ask up to three questions whose answers would change the estimate: which segment, what
   kind of money (new, expansion or kept), and any count the sources lack. If the request is
   "size <feature>" with no user or problem, the first question is who has the problem the
   feature solves. Wait, unless the PM said "draft anyway".
4. Draft from the template, headings unchanged and in order:
   - the range first, low, base and high per year, with the kind of money, then the
     recommendation: worth pursuing or not, at this size;
   - bottom-up arithmetic for all three cases, written out so a reader can redo it:
     customers with the problem × share who adopt × value each per year;
   - a second method as a sanity check, and why you trust the first more;
   - every input in the assumptions table with its source and confidence;
   - the single assumption that moves the answer most, and by how much.
   Round to what the inputs support: two significant figures at most.
5. Check the draft against `/content/playbooks/bar.md`, using the opportunity-sizing row of
   its table. Recompute each case once from the table to confirm the arithmetic. Fix what
   fails, once. The cheapest test of the riskiest assumption, what the estimate leaves out
   and every `[unverified]` input go under "What would change it" with a suggested owner.
6. Propose the draft with `confluence.create_draft`, titled `Sizing: <opportunity>`, in the
   space the PM named or their default. Then reply with the base case and range in one
   line, what is waiting for approval, and the assumption to test first.

## Do not
- Lead with a top-down number ("1% of a large market"). Use it only to sanity-check.
- Give a single point, or more precision than the inputs support.
- Add new, expansion and kept revenue together without saying so.
- Fill a missing count or price with a guess. Leave it `[unverified]` and show the range.
- Treat customers who asked as customers who will pay.
