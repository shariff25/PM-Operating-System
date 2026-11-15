---
name: poc-plan
description: Write a proof-of-concept plan for a customer or partner with measurable success criteria agreed before the POC starts, scope and what is out, the environment, owners on both sides, a timeline and an exit (buy, extend once or stop), as a Confluence draft the PM approves. Use when the PM asks for a POC plan, a trial or pilot plan, or "how do we run a POC with <customer>". Not for a go/no-go on a product launch (use launch-checklist), for sizing the opportunity (use opportunity-sizing) or for a partnership proposal (use partner-brief).
metadata:
  owner: "TODO"
  version: "0.1.0"
  model: "default"
  data-class: "restricted"
  tools: "context.search context.read jira.search jira.read confluence.search confluence.read confluence.create_draft docs.search docs.read"
  surfaces: "slack web cli"
  ships-in: "team-launch"
  track: "Customer and market"
---

# /poc-plan

Plan a POC so the customer can make a decision at the end of it. Success is agreed before
it starts, and each criterion is measurable.

## Inputs
- The customer or partner and the product (required).
- Optional: the decision the POC feeds, the dates, the SE, a target Confluence space.

## Steps
1. Read `/content/templates/poc-plan.md`, `/content/playbooks/customer-insight.md` and
   `/content/playbooks/metrics-and-experiments.md` (the parts on measures and guardrails).
2. Gather, and keep the list of sources:
   - what the customer asked for and why, and earlier POCs (`confluence.search`,
     `jira.search`, `context.search` in `decisions`);
   - the product's supported setups and limits (`docs.search`, `docs.read`,
     `context.search` in `products`).
   The customer name is restricted: keep it in this draft only.
3. Ask up to three questions whose answers would change the plan: who at the customer has
   the problem and decides, what they must see to buy, and the dates and environment. If the
   request is a feature to demo and not a problem to solve, that is the first question. Wait,
   unless the PM said "draft anyway".
4. Draft from the template, headings unchanged and in order:
   - the objective: the decision the POC feeds, in one sentence, first;
   - scope in and out, so the out-of-scope list is real;
   - success criteria the customer agrees before the start, each with a measure, a
     threshold, an owner and a date; then the environment, timeline and roles;
   - risks, riskiest assumption first with its cheapest test in week one; the exit: on pass,
     buy; on partial, extend once with a date; on fail, stop and write up what was learned.
5. Check the draft against `/content/playbooks/bar.md`. Fix what fails, once. A criterion
   with no number is not done: either set a threshold from the sources or list it under the
   risks as "threshold not agreed" with an owner. Mark unsourced claims `[unverified]`.
6. Propose the draft with `confluence.create_draft`, titled `POC plan: <customer> ·
   <product>`, in the space the PM named or their default. Then reply with the objective in
   one line, what is waiting for approval, and the criterion that is still not agreed.

## Do not
- Start a POC without success criteria the customer has agreed.
- Write a criterion without a measure and a threshold ("works well").
- Leave scope open-ended or offer more than one extension.
- Put the customer's name in anything shared outside this draft.
- Invent customer needs, dates or environment details. Missing ones are open items.
