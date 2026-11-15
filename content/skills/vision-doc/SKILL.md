---
name: vision-doc
description: Write a product vision 2 to 3 years out as a customer narrative, with the future in one paragraph, who it's for, what changes for them, what must be true, the path, what we won't be and the signposts, as a Confluence draft the PM approves. Use for "write our vision". Not for this year's choices and actions (use strategy-doc) or one investment's case (use investment-case).
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

# /vision-doc

Write where the product is going, from the customer's side, and what must be true for it
to get there. A vision every competitor could sign is a slogan.

## Inputs
- The product or line and the horizon year (required); optional: strategy, research, space.

## Steps
1. Read `/content/playbooks/company-bets.md` (section 2), `/content/playbooks/levels.md`
   and `/content/templates/vision.md`. Find an exemplar (`context.search`, domain
   `exemplars`) for depth and tone; never copy its facts.
2. Gather, and keep the list of sources:
   - the customers, their jobs and how many there are (`context.search` in `personas`,
     `products`, `confluence.search` for research and call syntheses);
   - market and competitors (`context.search` in `competitors`, `docs.search`);
   - the current strategy and the bets already open (`context.search` in `decisions`,
     `confluence.search`).
3. Ask up to three questions whose answers would change the vision: the customer it is for,
   the horizon, and what the company has already ruled out. Wait, unless the PM said
   "draft anyway".
4. Draft from the template, headings unchanged and in order:
   - one paragraph of a customer's day in the horizon year, concrete enough to disagree with;
   - who it is for, with counts and sources, and who it is not for;
   - what changes for them, job by job, with evidence they want it today;
   - two or three conditions that must be true, each a claim that could be false;
   - the path as two or three bets, each with a date and a metric;
   - what we won't be, each with its cost; a signpost per condition with a date.
5. Check the draft against `/content/playbooks/bar.md` and the Principal checks in
   `levels.md`: the business effect as a range with its assumptions, and what is hard to
   reverse. Fix what fails, once. Unknowns go under "Signposts" as open questions.
6. Propose the draft with `confluence.create_draft`, titled `Vision: <product and horizon
   year>`. Then reply with the vision in one line, what is waiting for approval, and the
   condition most likely to fail.

## Do not
- Write a feature list or a roadmap and call it a vision.
- Write conditions that cannot be false ("customers want security").
- Invent market sizes, growth rates or competitor plans. Unsourced figures are
  `[unverified]`.
- Leave a condition without a signpost and a date.
