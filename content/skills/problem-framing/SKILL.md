---
name: problem-framing
description: Write a problem statement before any solution is chosen, covering who has the problem, the evidence, what they do today, why now and what is unknown, as a Confluence draft the PM approves. Use when the PM has a feature request, a customer ask or a hunch and needs to pin down the user and the problem first, or says "frame this", "what problem are we solving" or "write the problem statement". The draft contains no solution. Not for requirements (use prd-draft) or for choosing between options (use decision-doc).
metadata:
  owner: "TODO"
  version: "0.1.0"
  model: "reasoning"
  data-class: "restricted"
  tools: "context.search context.read jira.search jira.read confluence.search confluence.read confluence.create_draft docs.search docs.read"
  surfaces: "slack web cli"
  ships-in: "pilot"
  track: "Product sense"
---

# /problem-framing

Pin down the user and the problem before anyone picks a solution. The draft has no solution
in it.

## Inputs
- A request, customer ask, idea or observation (required).
- Optional: call notes, Jira issues, a product area, a target Confluence space.

## Steps
1. Read `/content/playbooks/product-sense.md` (steps 1 to 5) and
   `/content/templates/problem-statement.md`. Find an exemplar (`context.search`, domain
   `exemplars`) for depth and tone; never copy its facts.
2. Gather evidence, and keep the list of sources:
   - who is affected: personas and product context (`context.search` in `personas`,
     `products`);
   - what customers said and did: call notes, tickets, asks (`confluence.search`,
     `jira.search`);
   - what the product does today, so the workaround is described correctly (`docs.search`).
3. Ask up to three questions whose answers would change the statement. If the request names
   a solution and no user or problem, the first question is who has the problem and what they
   do today. Wait for answers, unless the PM said "draft anyway".
4. Draft from the template, headings unchanged and in order:
   - one segment, named by situation; say who is left out;
   - the job and the pain in the user's words, pains ranked by frequency × severity;
   - evidence as counts by account and segment, with verbatim quotes; include evidence
     against;
   - the workaround and what it costs them;
   - the recommendation line at the top: pursue now, park or drop.
5. Check the draft against `/content/playbooks/bar.md`, using the problem-statement row of
   its table. Fix what fails, once. What the sources can't settle goes under "What we don't
   know", with a suggested owner.
6. Propose the draft with `confluence.create_draft`, titled `Problem: <the problem>`, in the
   space the PM named or their default. Then reply with the recommendation in one line, what
   is waiting for approval, and the top open question.

## Do not
- Describe, name or hint at a solution. If the request came as one, state the problem
  behind it.
- Write the problem as a missing feature ("users lack export").
- Turn one account's ask into a segment's problem. Count accounts, not mentions.
- Invent customers, quotes, counts or dates. Thin evidence is a finding; say so.
