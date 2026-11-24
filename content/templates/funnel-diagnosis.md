---
title: Funnel diagnosis template
owner: "TODO"
version: "0.1.0"
---

# Funnel diagnosis: <funnel> · <period>

<as-of date> · <PM> · Unit: <users, accounts or sessions> · Status: Draft

## Finding
<!-- First: where the funnel leaks most, with its step-to-step rate and the users lost,
labelled as the PM's read to confirm. If the lowest rate and the largest loss are at different
steps, say so and say which to chase first. With no counts, say that PM OS has no analytics
source and list what is needed instead; state no rate. -->

## The funnel
<!-- Counts as given, and the rates by plain division with the counts beside them. Source and
date for each count: the PM's message or the page. -->

| Step | Count | From previous step | From the top | Users lost | Source |
| --- | --- | --- | --- | --- | --- |
| | | | | | |

## Where it leaks
<!-- The biggest drop and what the sources say about the step: its definition, known issues,
changes shipped near it. Comparisons across periods or segments only from stats.compare, quoted
exactly with the interval; otherwise "not tested". -->

## Hypotheses and tests
<!-- Two at least. Each: "Because <evidence or mechanism>, <cause>". The evidence, or
[unverified]. The test that would separate it from the other: a cut of the existing counts, a
check with a named team, or an experiment to size with experiment-design. -->

| Hypothesis | Evidence | Test | What would change the finding |
| --- | --- | --- | --- |
| | | | |

## What the numbers do not show
<!-- The unit and cohort, the period, missing segments, survivorship and seasonality, and any
step the sources could not define. The riskiest assumption (usually that the steps share a
cohort) and the cheapest way to check it. -->

## Next
<!-- The one next step, with an owner only if a source names one. Every [unverified] item from
above is listed here with a suggested owner. -->
1. <step> (owner: <name>)
