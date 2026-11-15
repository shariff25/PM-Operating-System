---
title: Experiment plan template
owner: "TODO"
version: "0.1.0"
---

# Experiment: <the change being tested>

<as-of date> · <author> · Status: Draft

**Recommendation:** <run it or don't, and what we do on a win, in one sentence>

## Hypothesis
<!-- Who has the problem and the evidence first. Then: "Because <insight>, <change> will
move <primary metric> from <baseline> by at least <MDE> for <population>." The arms: what
control and treatment each see. Other designs considered, including not testing, and why
this one. -->

## Primary metric
<!-- One metric that decides. Numerator, denominator, window, unit, source. The baseline
with its source and date. The MDE and why a lift that size would change the decision.
Secondary metrics, for learning only. -->

## Guardrail metrics
<!-- What must not get worse, each with a threshold and how it is measured. -->

| Guardrail | Baseline | Threshold | How measured |
| --- | --- | --- | --- |
| | | | |

## Sample size and duration
<!-- From the stats.sample_size tool, never by hand. Quote its output: per arm, total, days
and the assumptions (baseline, MDE, alpha, power, sidedness, arms). State the unit of
randomisation and the daily traffic with its source. Round up to whole weeks. Give the
start and end dates. No decisions before the end date. -->

| Input or output | Value |
| --- | --- |
| Baseline | |
| MDE | |
| Alpha, power, sidedness | |
| Arms | |
| Per arm | |
| Total | |
| Days, rounded to whole weeks | |

## Decision rule
<!-- Written before launch. What result ships it, what result stops it, what happens in
between. Include the guardrail thresholds. Segments to be read, named now. Who decides. -->

| Result | Action |
| --- | --- |
| | |

## Risks
<!-- The riskiest assumption and the cheapest check. Threats to the result: novelty, arms
contaminating each other, an uneven split, seasonality, overlapping experiments. Open
questions and [unverified] items, each with an owner. -->
