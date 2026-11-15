---
name: exec-brief
description: Write a one-page brief for executives, with the ask first, then why now, what they need to know, the options, the risk and the next step, as a Confluence draft the PM approves. Use for "write this up for the exec team", "brief the CEO", a board or leadership ask. Not for the full analysis behind it (use decision-doc or investment-case) or a plan to win agreement (use alignment-plan).
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

# /exec-brief

One page for people with five minutes. The ask comes first, then only what they need in
order to decide.

## Inputs
- The decision or ask (required).
- Optional: the underlying decision document or investment case, the readers, the meeting
  date, a target space.

## Steps
1. Read `/content/playbooks/company-bets.md` (section 7), `/content/playbooks/levels.md`
   and `/content/templates/exec-brief.md`. Find an exemplar (`context.search`, domain
   `exemplars`) for depth and tone; never copy its facts.
2. Gather, and keep the list of sources: the underlying document (`confluence.read`), the
   numbers it rests on with their sources (`confluence.search`), and the strategy it serves
   (`context.search` in `decisions`).
3. Ask up to three questions whose answers would change the brief: the exact ask, who the
   readers are, and the date they decide. If there is no underlying analysis, say so and
   ask whether to write that first. Wait, unless the PM said "draft anyway".
4. Draft from the template, headings unchanged and in order, on one page:
   - the ask in one or two sentences, labelled as a recommendation, with the date;
   - why now: the trigger and the cost of waiting;
   - at most five bullets they need, with the business effect as a range and its
     assumptions, copied from the underlying document's tool results;
   - two or three options, including the cheaper one; the one risk; what is hard to reverse;
   - the next step, its owner and date. Link the underlying document for the detail.
5. Check the draft against `/content/playbooks/bar.md` and the level's checks in
   `levels.md`. Fix what fails, once. If it runs past one page, cut background first.
6. Propose the draft with `confluence.create_draft`, titled `Exec brief: <the decision>`.
   Then reply with the ask in one line, what is waiting for approval, and the top open
   question.

## Do not
- Open with background. The ask is the first line.
- Run past one page; move detail to the linked document.
- Recompute or round numbers from the underlying case differently. Copy them.
- Give a single number where the effect is uncertain; give the range.
