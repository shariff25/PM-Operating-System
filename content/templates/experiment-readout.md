---
title: Experiment readout template
owner: "TODO"
version: "0.1.0"
---

# Readout: <the experiment>

<as-of date> · <author> · Ran <start date> to <end date> · Status: Draft

## Decision
<!-- Ship, iterate or stop, in the first sentence, labelled as a recommendation, with the
one number that supports it. Then what was tested, for whom and why, in two sentences, and
the decision rule from the plan. Say whether the planned sample size was reached. -->

## Result
<!-- From the stats.compare tool, never by hand. Quote its output for the primary metric:
both rates, absolute and relative lift, the interval and the p-value. Read both ends of the
interval against the MDE and zero. Then the segments named in the plan, and the lift by
week. -->

| Arm | Users | Conversions | Rate |
| --- | --- | --- | --- |
| Control | | | |
| Treatment | | | |

## Guardrails
<!-- Each guardrail: the threshold from the plan, the measured change and its interval.
Compare the bad end of the interval with the threshold. -->

| Guardrail | Threshold | Change | Interval | Held? |
| --- | --- | --- | --- | --- |
| | | | | |

## What we learned
<!-- What the result says about the hypothesis and the user. Other readings of the data and
why you prefer yours. What it does not show: segments found afterwards are hypotheses, and
lagging outcomes are not yet known. The riskiest remaining assumption. -->

## Next steps
<!-- What happens now, with owners and dates. The follow-up metric and when it will be
read. What we are not doing as a result. Open questions and [unverified] items. -->

| Step | Owner | By when |
| --- | --- | --- |
| | | |
