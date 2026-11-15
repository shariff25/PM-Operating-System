---
name: alignment-plan
description: Plan how to get a cross-team decision made without authority, with who decides, who must agree, each role's likely objection and the answer, the order to talk to them and the fallback, as a Confluence draft the PM approves. Use for "how do I get buy-in" or "who do I need to convince". Not for the decision itself (use decision-doc) or a one-page exec ask (use exec-brief).
metadata:
  owner: "TODO"
  version: "0.1.0"
  model: "reasoning"
  data-class: "restricted"
  tools: "context.search context.read jira.search jira.read confluence.search confluence.read confluence.create_draft docs.search docs.read"
  surfaces: "slack web cli"
  ships-in: "team-launch"
  track: "Leading across teams"
---

# /alignment-plan

Plan the path to a decision the PM cannot make alone. It names roles and what their teams
lose; it never guesses at a named person's motives.

## Inputs
- The decision and the recommendation (required); optional: the decision document, the date.

## Steps
1. Read `/content/playbooks/leading-across-teams.md` (section 6),
   `/content/playbooks/levels.md` and `/content/templates/alignment-plan.md`. Find an
   exemplar (`context.search`, domain `exemplars`) for depth and tone; never copy its facts.
2. Gather, and keep the list of sources:
   - the decision document and earlier decisions on it (`confluence.search`,
     `context.search` in `decisions`);
   - stakeholders and their teams' goals (`context.search` in `stakeholders`);
   - what each team would lose: its committed work (`jira.search`, `confluence.search`).
3. Ask up to three questions whose answers would change the plan: who decides, who can veto,
   and the date. If nobody has written down the recommendation, ask for it first. Wait,
   unless the PM said "draft anyway".
4. Draft from the template, headings unchanged and in order:
   - the decision and the recommendation in the first three lines;
   - one role that decides; the roles that must agree, with what each team loses and gains;
   - each role's likely objection as its best case, and the answer with evidence; if an
     objection is right, what changes in the plan;
   - the order: veto holders and the most affected teams first, one to one, before any
     group meeting, each with a date and the ask;
   - the fallback if the answer is no or the date passes.
5. Check the draft against `/content/playbooks/bar.md` and the level's checks in
   `levels.md`: the alternatives are the fallback and the paths not taken. Fix what fails,
   once. Unknowns go under "Fallback" as open questions with owners.
6. Propose the draft with `confluence.create_draft`, titled `Alignment plan: <the
   decision>`. Then reply with the first conversation to have in one line, what is waiting
   for approval, and the top open question.

## Do not
- Guess at a named person's motives, mood or politics. Describe the role and its team's
  stake.
- Plan to surprise anyone in a group meeting.
- Leave out the fallback.
- Treat agreement as the goal; the goal is the decision, made by the person who owns it.
