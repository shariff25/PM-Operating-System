---
name: experiment-readout
description: Write up the results of a finished A/B test or experiment, leading with a ship, iterate or stop decision, then the result with rates, lift, interval and p-value worked out by the stats.compare tool, the guardrails, what was learned and next steps, as a Confluence draft the PM approves. Use when the PM shares experiment results or counts per arm and asks "did it work", "is this significant", "read out the test" or "should we ship it". Not for planning a test (use experiment-design).
metadata:
  owner: "TODO"
  version: "0.1.0"
  model: "reasoning"
  data-class: "restricted"
  tools: "context.search context.read jira.search jira.read confluence.search confluence.read confluence.create_draft docs.search docs.read stats.compare"
  surfaces: "slack web cli"
  ships-in: "pilot"
---

# /experiment-readout

Turn experiment results into a decision. Every rate, lift, interval and p-value comes from
the `stats.compare` tool, never from your own arithmetic.

## Inputs
- Users and conversions for each arm, for the primary metric (required). Percentages alone
  are not enough; ask for the counts.
- Optional: the experiment plan, guardrail counts, segment counts, the dates it ran.

## Steps
1. Read `/content/playbooks/metrics-and-experiments.md` (reading results) and
   `/content/templates/experiment-readout.md`. Find an exemplar (`context.search`, domain
   `exemplars`) for depth and tone; never copy its facts.
2. Gather, and keep the list of sources:
   - the plan: hypothesis, primary metric, MDE, planned sample size, decision rule,
     guardrail thresholds, named segments (`confluence.search` for the experiment page,
     `confluence.read`);
   - the results page or issue the PM points to (`confluence.read`, `jira.read`).
3. Ask up to three questions whose answers would change the readout: the counts per arm,
   the plan or decision rule if none is found, and the dates. If there is no hypothesis
   anywhere, the first question is what was tested, for whom and what it was meant to
   change. Wait, unless the PM said "draft anyway".
4. Call `stats.compare` with `control: {users, conversions}` and `treatment: {users,
   conversions}` for the primary metric. Call it again for each guardrail that is a rate,
   and for each segment named in the plan. Copy the output into the draft exactly:
   `control_rate`, `treatment_rate`, `absolute_lift`, `relative_lift`, `ci_low`, `ci_high`,
   `p_value`, `significant`. Writing a rate as a percentage is fine; changing it is not.
5. Draft from the template, headings unchanged and in order:
   - the decision first, labelled as a recommendation, by applying the plan's decision rule
     to the tool's output. If no rule was set in advance, say so;
   - if either arm is below the planned sample size, the decision is "not yet readable";
   - read both ends of the interval against zero and the MDE;
   - guardrails: the bad end of each interval against its threshold;
   - what the result does not show: late slices, lagging outcomes, novelty.
6. Check the draft against `/content/playbooks/bar.md`, using the experiment-readout row of
   its table. Fix what fails, once. What can't be settled goes under "Next steps" as open
   questions with owners.
7. Propose the draft with `confluence.create_draft`, titled `Readout: <the experiment>`, in
   the space the PM named or their default. Then reply with the decision and the lift with
   its interval in one line, what is waiting for approval, and the top open question.

## Do not
- Compute or estimate a rate, lift, interval or p-value yourself, even as a check.
- Call a win on the p-value alone, or "no effect" when the interval still includes the MDE.
- Slice the data until something is significant. Unplanned segments are hypotheses.
- Test a mean, a median or revenue per user with this tool; it compares rates. Report those
  figures as given, mark them "not tested", and name who should analyse them.
- Invent counts, dates or a decision rule the plan did not contain.
