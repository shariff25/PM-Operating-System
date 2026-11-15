---
name: portfolio-review
description: Report where a portfolio of teams stands, from jira.rollup counts quoted exactly, with the dependencies and conflicts between teams, what is at risk and the changes you'd recommend, as a Confluence draft the PM approves. Use for "how are my teams doing", a portfolio or programme review. Not for choosing between investments (use roadmap-tradeoffs) or results against plan (use business-review).
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

# /portfolio-review

Report where several teams stand, from counts and links, not from status meetings. It
recommends changes; it decides nothing.

## Inputs
- The portfolio (required): projects, epics, a fix version or JQL; optional: the goals.

## Steps
1. Read `/content/playbooks/leading-across-teams.md` (sections 2 and 3),
   `/content/playbooks/levels.md` and `/content/templates/portfolio-review.md`. Find an
   exemplar (`context.search`, domain `exemplars`) for depth and tone; never copy its facts.
2. Gather, and keep the list of sources:
   - call `jira.rollup` with `jql`, `group_by` (`project` by default; `epic`, `status`,
     `assignee` or `fix_version`) and `max_issues` (default 500); group again if needed;
   - the commitments it is read against (`confluence.search`, `context.search` in
     `decisions`);
   - the epics behind any outlier (`jira.read`, `jira.search`).
3. Ask up to three questions whose answers would change the review: which projects are in
   it, what they committed to, and the date that matters. Wait, unless the PM said "draft
   anyway".
4. Draft from the template, headings unchanged and in order:
   - the bottom line first: where things stand, the dependency that matters, the change;
   - each group's total, to do, in progress, done, overdue and unassigned, copied exactly
     from the rollup, with the Jira link for the query;
   - every cross-team link from `links`, the critical chain, and links that leave the
     query (`to_group` is `null`) with the team that owns them;
   - if `truncated` (or `links_truncated`) is true, say the counts are partial and why;
   - recommendations as fund, sequence or stop, each with who would decide.
5. Check the draft against `/content/playbooks/bar.md`, with the Staff check in
   `levels.md`: what would stop, and the teams affected. Fix what fails, once. What the
   counts can't answer goes under "Open questions" with an owner.
6. Propose the draft with `confluence.create_draft`, titled `Portfolio review: <portfolio
   and period>`. Then reply with the bottom line in one line, what is waiting for approval,
   and the top open question.

## Do not
- Count, add or estimate issues yourself, or round the rollup's numbers. Quote them.
- Extrapolate a total from a truncated result.
- Judge a team from its counts alone; read the epic behind the outlier first.
- Decide. Recommend, and name who decides.
