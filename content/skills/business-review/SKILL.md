---
name: business-review
description: Write a business review of results against plan, then why, then what changes, the decisions needed and the next period, with delivery counts from jira.rollup quoted exactly, as a Confluence draft the PM approves. Use for a quarterly or monthly business review, or "how did we do against plan". Not for one project's status (use status-update) or a portfolio view (use portfolio-review).
metadata:
  owner: "TODO"
  version: "0.1.0"
  model: "reasoning"
  data-class: "restricted"
  tools: "context.search context.read jira.search jira.read confluence.search confluence.read confluence.create_draft docs.search docs.read jira.rollup"
  surfaces: "slack web cli"
  ships-in: "team-launch"
  track: "Leading across teams"
---

# /business-review

Results against the plan, then why, then what changes. No activity lists.

## Inputs
- The area or portfolio and the period (required).
- Optional: the plan or OKRs it is read against, the readers, a target space.

## Steps
1. Read `/content/playbooks/leading-across-teams.md` (section 5),
   `/content/playbooks/levels.md` and `/content/templates/business-review.md`. Find an
   exemplar (`context.search`, domain `exemplars`) for depth and tone; never copy its facts.
2. Gather, and keep the list of sources:
   - the plan: committed numbers and OKRs for the period (`confluence.search`);
   - the actuals, with their source and date (`confluence.search`, `context.search`);
   - delivery: call `jira.rollup` over the period's work (`group_by` `project` or
     `fix_version`) and quote its counts and links exactly; say so if it is `truncated`;
   - readouts and decisions that explain the gaps (`confluence.search`, `jira.search`).
3. Ask up to three questions whose answers would change the review: the plan's numbers if
   none are written down, the actuals that are missing, and the decision the readers must
   make. Wait, unless the PM said "draft anyway".
4. Draft from the template, headings unchanged and in order:
   - the bottom line in the first three lines: on plan or not, the reason, the change;
   - each committed number: plan, actual, gap, source;
   - the one or two causes of the biggest gaps, with evidence;
   - what changes and what stops, each with an owner; decisions needed, with dates;
   - next period's commitments with baselines, targets and dates.
5. Check the draft against `/content/playbooks/bar.md` and the level's checks in
   `levels.md`. Fix what fails, once. Missing actuals go under "Next period" as open
   questions with owners.
6. Propose the draft with `confluence.create_draft`, titled `Business review: <area and
   period>`. Then reply with the bottom line in one line, what is waiting for approval, and
   the decision needed.

## Do not
- List activity: meetings held, tickets closed, launches shipped, unless it is evidence
  for a cause.
- Count issues yourself or round the rollup's numbers.
- Explain a miss without saying what changes because of it.
- Invent actuals. A missing number is `[unverified]` and an open question.
