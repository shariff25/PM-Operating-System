---
name: experiment-design
description: Design an A/B test or experiment before it runs, with a hypothesis, one primary metric, guardrail metrics, the sample size and duration worked out by the stats.sample_size tool, a decision rule set in advance and the risks, as a Confluence draft the PM approves. Use when the PM asks "how do we test this", "design an experiment", "how many users do we need", "how long should the test run" or "what sample size". Not for reading the results of a finished test (use experiment-readout) or choosing metrics for a whole product (use metrics-tree).
metadata:
  owner: "TODO"
  version: "0.1.0"
  model: "reasoning"
  data-class: "restricted"
  tools: "context.search context.read jira.search jira.read confluence.search confluence.read confluence.create_draft docs.search docs.read stats.sample_size"
  surfaces: "slack web cli"
  ships-in: "pilot"
---

# /experiment-design

Plan a test the team cannot fool itself with. The sample size and duration come from the
`stats.sample_size` tool, never from your own arithmetic.

## Inputs
- The change to test and the metric it should move (required).
- The baseline, the smallest lift worth detecting (MDE) and the daily traffic. Ask for any
  that are missing and not in the sources.

## Steps
1. Read `/content/playbooks/metrics-and-experiments.md` (the Experiments half) and
   `/content/templates/experiment-plan.md`. Find an exemplar (`context.search`, domain
   `exemplars`) for depth and tone; never copy its facts.
2. Gather, and keep the list of sources:
   - the primary metric's definition and baseline, with its date (`confluence.search`,
     `context.search`);
   - earlier and overlapping experiments on the same surface (`confluence.search`,
     `jira.search`);
   - the evidence for the problem the change addresses (`confluence.search`, `jira.search`).
3. Ask up to three questions whose answers would change the plan: the baseline, the MDE
   that would change the decision, and the users entering the test each day. If the request
   names a change and no user or problem, the first question is what problem it solves and
   what users should do differently. Wait, unless the PM said "draft anyway".
4. Call `stats.sample_size`. Arguments: `metric` (`proportion` or `mean`), `baseline`,
   `mde`, `mde_kind` (`relative` or `absolute`), `std_dev` for a mean, `variants`, and
   `daily_users` when known. Leave `alpha`, `power` and `two_sided` at their defaults unless
   the PM set them. Copy `per_arm`, `total`, `days` and `assumptions` into the draft exactly
   as returned. Round the days up to whole weeks, 14 days at the least, and say you did.
5. Draft from the template, headings unchanged and in order:
   - the recommendation line: run it or not, and what happens on a win;
   - the hypothesis with the user, the evidence, the baseline and the MDE;
   - one primary metric, fully defined; guardrails with thresholds;
   - a decision rule for a win, a loss and an unclear result;
   - if the tool says more than eight weeks, recommend against this design and offer a
     bolder change, a more sensitive metric or another method.
6. Check the draft against `/content/playbooks/bar.md`, using the experiment-plan row of
   its table. Fix what fails, once. What the sources can't settle goes under "Risks" as
   open questions with owners.
7. Propose the draft with `confluence.create_draft`, titled `Experiment: <the change>`, in
   the space the PM named or their default. Then reply with the recommendation and the
   per-arm sample size in one line, what is waiting for approval, and the top open question.

## Do not
- Work out a sample size, a duration in days or a power figure yourself, or adjust the
  tool's numbers. If the tool returns an error, fix the arguments or ask the PM.
- Guess a baseline or daily traffic. Without a baseline there is no tool call; leave the
  table cells `[unverified]` and list what is needed.
- Name more than one primary metric, or pick the MDE to fit the traffic.
- Leave the decision rule for after the results.
