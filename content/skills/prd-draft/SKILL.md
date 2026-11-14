---
name: prd-draft
description: Write a first-draft PRD from a one-paragraph brief, grounded in the team PRD template, product context, similar past PRDs, open Jira issues and recent customer asks, with citations and an open-questions list. The draft lands as a Confluence draft page after the PM approves it.
metadata:
  owner: "TODO"
  version: "0.1.0"
  model: "default"
  data-class: "restricted"
  tools: "context.search context.read jira.search jira.read confluence.search confluence.read confluence.create_draft"
  surfaces: "slack web cli"
  ships-in: "phase-0"
---

# /prd-draft

Turn a brief into a review-ready first draft (PRD journey J1).

## Inputs
- A one-paragraph brief (required).
- Optional: a Jira epic, Confluence pages, call notes, a target Confluence space.

## Steps
1. Read `templates/prd.md`. The draft follows its sections in order.
2. Gather, and keep a list of what you read:
   - product context for the area (`context.search` in `products`, `personas`, `competitors`);
   - two or three similar past PRDs from `exemplars`;
   - the linked epic and its open issues (`jira.read`, `jira.search`);
   - recent customer asks in Jira and Confluence call notes.
3. Ask up to three clarifying questions where the answer would change scope, users or success
   metrics. Wait for answers, or for the PM to say "draft anyway".
4. Write the draft:
   - Lead each section with its point. Requirements get IDs and P0/P1/P2 priorities.
   - Cite every factual claim. Mark the rest `[unverified]`.
   - Success metrics need a number and a measurement method, or go to open questions.
   - End with open questions, each with a suggested owner.
5. Propose the Confluence draft with `confluence.create_draft`, titled `PRD: <feature>`,
   in the space the PM named or their default space. Include a "Sources" list at the end.
6. Offer the next step: "Run /review with engineering, security, field and executive?"

## Do not
- Invent customers, numbers, dates or competitor claims.
- Pad sections. A section with nothing to say gets one line and an open question.
