---
name: prd-draft
description: Write a first-draft PRD from a brief that starts from the user and the problem, weighs real alternatives, recommends one solution and states the bet, with requirements, success metrics with a guardrail, rollout, the riskiest assumption, citations and open questions, as a Confluence draft the PM approves. Use when the PM asks for a PRD, a spec or requirements for a feature, or "write this up for engineering". Not for a model-backed feature (use ai-feature-spec), for pinning down the problem before any solution (use problem-framing) or for choosing between options (use decision-doc).
metadata:
  owner: "TODO"
  version: "0.2.0"
  model: "reasoning"
  data-class: "restricted"
  tools: "context.search context.read jira.search jira.read confluence.search confluence.read confluence.create_draft docs.search docs.read"
  surfaces: "slack web cli"
  ships-in: "phase-0"
---

# /prd-draft

Turn a brief into a review-ready first draft (PRD journey J1): the problem and the evidence
first, then the options, the pick and what it takes to ship.

## Inputs
- A brief (required): a paragraph, a Jira epic or a Confluence page.
- Optional: call notes, a problem statement, a target Confluence space.

## Steps
1. Read `/content/playbooks/product-sense.md` and `/content/templates/prd.md`. Find an
   exemplar (`context.search`, domain `exemplars`) for depth and tone; never copy its facts.
2. Gather evidence, and keep the list of sources:
   - product context for the area (`context.search` in `products`, `personas`,
     `competitors`);
   - a problem statement or earlier decisions on the area (`confluence.search`,
     `context.search` in `decisions`);
   - the linked epic and its open issues (`jira.read`, `jira.search`);
   - customer asks and call notes, counted by account (`jira.search`, `confluence.search`);
   - what the public docs already say (`docs.search`), so the PRD doesn't contradict
     shipped behaviour.
3. Ask up to three questions whose answers would change scope, users or success metrics. If
   the request has a solution but no user or problem, ask about that first: who has the
   problem, and what do they do today? Wait for answers, unless the PM said "draft anyway";
   then state the user and problem you assumed as `[unverified]`.
4. Draft from the template, headings unchanged and in order:
   - the summary leads with the recommendation, the outcome (a number and a date) and the
     bet: "We believe <segment> will <change in behaviour> if <solution>, because
     <insight>";
   - the problem, the evidence with counts and today's workaround before any solution;
   - non-goals, each with its reason, and what the choice gives up;
   - at least two real alternatives besides the pick, one smaller or cheaper, and why each
     lost;
   - the smallest version that tests the bet, and what was cut;
   - requirements with IDs, one testable statement each; P0 only what the bet needs;
   - an outcome metric with baseline, target, date and source, and a guardrail;
   - the riskiest assumption first under "Risks and mitigations", with its cheapest test.
5. Check the draft against `/content/playbooks/bar.md`. Fix what fails, once. What can't be
   fixed from the sources goes under "Open questions" with a suggested owner. List every
   source under "Sources".
6. Propose the draft with `confluence.create_draft`, titled `PRD: <feature>`, in the space
   the PM named or their default. Then reply with the recommendation in one line, what is
   waiting for approval, and the top open question. Offer /review next.

## Do not
- Write requirements before the problem and the alternatives are on the page.
- Treat "users lack <feature>" as the problem.
- Put everything in P0. Say what you would cut.
- Invent customers, numbers, dates or competitor claims.
- Pad sections. A section with nothing to say gets one line and an open question.
