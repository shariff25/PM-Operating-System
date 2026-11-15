---
name: ai-feature-spec
description: Write a spec for a model-backed (AI, LLM, agent or assistant) feature, with intended behaviour by example, ranked failure modes, misuse and safety, evals as the acceptance criteria, cost and latency budgets, the level of human oversight, and a staged rollout with monitoring, as a Confluence draft the PM approves. Use when the PM wants to add an AI assistant, copilot, agent, summariser, classifier or any feature where a model produces the output, or says "spec this AI feature". Not for a feature with no model in it (use prd-draft) or for the detailed eval design alone (use eval-plan).
metadata:
  owner: "TODO"
  version: "0.1.0"
  model: "reasoning"
  data-class: "restricted"
  tools: "context.search context.read jira.search jira.read confluence.search confluence.read confluence.create_draft docs.search docs.read"
  surfaces: "slack web cli"
  ships-in: "pilot"
  track: "AI products"
---

# /ai-feature-spec

Specify a model-backed feature so it can be built, tested and trusted: behaviour by example,
evals as the acceptance criteria, and a plan for when the model is wrong.

## Inputs
- The feature idea or brief (required).
- Optional: the user problem and evidence, the data and tools the model would reach, a
  target Confluence space.

## Steps
1. Read `/content/playbooks/ai-products.md`, `/content/playbooks/product-sense.md` (steps 1
   to 7) and `/content/templates/ai-feature-spec.md`. Find an exemplar (`context.search`,
   domain `exemplars`) for depth and tone; never copy its facts.
2. Gather, and keep the list of sources:
   - the user, the job and today's workaround (`context.search` in `personas`, `products`;
     `confluence.search`, `jira.search` for calls and tickets);
   - what data and actions the feature would touch, and who may see them (`docs.search`,
     `confluence.search`);
   - earlier AI specs, eval plans and incidents in the area (`context.search` in
     `decisions`, `confluence.search`).
3. Ask up to three questions whose answers would change the spec: who the user is and what
   they do today, what the model can see and do, and what an error costs. If the request is
   "add AI to <area>" with no user or problem, that is the first question, and so is
   whether a non-AI option would do. Wait, unless the PM said "draft anyway".
4. Draft from the template, headings unchanged and in order:
   - the recommendation first, with the outcome, why a model is the right tool, and the
     non-AI options that lost;
   - should, must never and when unsure; what the model can see and do; non-goals;
   - at least five examples with input, good output, bad output and why, including an
     ambiguous request and one out of scope;
   - failure modes ranked by likelihood × damage, prompt injection among them;
   - evals with thresholds set before the first run, hard gates apart from targets;
   - budgets at p50 and p95; the oversight level; the path that works without the model;
   - rollout stages with gates, a kill switch and who holds it, an outcome metric and a
     guardrail, each with a number, a date and a source.
5. Check the draft against `/content/playbooks/bar.md`. Fix what fails, once. The riskiest
   assumption with its cheapest test, and every `[unverified]` item, go under "Open
   questions" with a suggested owner.
6. Propose the draft with `confluence.create_draft`, titled `AI spec: <feature>`, in the
   space the PM named or their default. Then reply with the recommendation in one line,
   what is waiting for approval, and the top open question. Offer /eval-plan next.

## Do not
- Reach for a model when a rule, a default or a form would do. Say why it would not.
- Let the feature do through the model what the user may not do directly.
- Write "the model will be accurate". Give the threshold, the dataset and the grader.
- Let the model write, send or apply anything without an approval step, unless the spec
  argues for it and names the undo.
- Invent accuracy figures, costs, latencies or customer evidence. Unknowns are open
  questions.
