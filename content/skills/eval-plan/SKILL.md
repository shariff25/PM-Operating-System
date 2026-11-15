---
name: eval-plan
description: Write the evaluation plan for a model-backed feature or model change, with the behaviour under test, datasets (typical, edge, adversarial, held out) and who labelled them, graders and how each was validated, thresholds set before the first run with hard gates apart from targets, slices with floors, the regression set and who reviews failures, as a Confluence draft the PM approves. Use when the PM asks "how do we test this AI feature", "write the evals", "what is our launch bar for the model" or before a model or prompt change ships. Not for the feature spec itself (use ai-feature-spec) or an A/B test of user behaviour (use experiment-design).
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

# /eval-plan

Write the acceptance criteria for a model-backed feature before it is built, so "it looked
good in the demo" never decides a launch.

## Inputs
- The feature or change under test (required): an AI spec, a Confluence page or a brief.
- Optional: existing datasets, past failures and incidents, the launch date.

## Steps
1. Read `/content/playbooks/ai-products.md` (sections 2, 3 and 7) and
   `/content/templates/eval-plan.md`. Find an exemplar (`context.search`, domain
   `exemplars`) for depth and tone; never copy its facts.
2. Gather, and keep the list of sources:
   - the AI spec: intended behaviour, examples, failure modes (`confluence.search`,
     `confluence.read`);
   - real inputs to sample from, and who could label them (`confluence.search`,
     `jira.search`);
   - past failures, incidents and reports in the area (`jira.search`, `context.search` in
     `decisions`).
3. Ask up to three questions whose answers would change the plan: what decision the results
   gate, what an error costs, and where real inputs can come from. If there is no spec and
   no stated user or job, the first question is who uses the feature and for what, because
   the evals test that job. Wait, unless the PM said "draft anyway".
4. Draft from the template, headings unchanged and in order:
   - the recommendation first: the thresholds that gate launch;
   - datasets with source, case count, labellers and how disagreements were settled; a
     held-out set nobody tunes on; what real use they miss;
   - graders: code where exact, a model grader with a rubric checked against human labels,
     people for calibration; graders considered and why they lost;
   - thresholds set now, hard gates apart from targets, with baselines where known;
   - slices with a floor each, so an average cannot hide a failing slice;
   - the regression set: who adds cases and when it runs;
   - named reviewers, how often, and what they can decide.
5. Check the draft against `/content/playbooks/bar.md`, using the eval-plan row of its
   table. Fix what fails, once. The riskiest assumption (usually that the datasets match
   real use) with its cheapest check, and every `[unverified]` item, go under "Who reviews
   failures" as open questions with a suggested owner.
6. Propose the draft with `confluence.create_draft`, titled `Eval plan: <feature>`, in the
   space the PM named or their default. Then reply with the launch gates in one line, what
   is waiting for approval, and the top open question.

## Do not
- Set or move a threshold after seeing results. Thresholds come first.
- Trust a model grader that has not been checked against human labels.
- Report one average. Every run reports every slice.
- Tune on the held-out set, or let it leak into prompts or examples.
- Invent case counts, baselines or labeller names. Missing ones are open questions.
