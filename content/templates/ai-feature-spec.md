---
title: AI feature spec template
owner: "TODO"
version: "0.1.0"
---

# AI spec: <feature>

<as-of date> · <author> · Status: Draft

## Summary
<!-- Lead with the recommendation: what to build, for whom, and the outcome (a number and a
date). Why a model is the right tool here, and the non-AI options that lost. The bet in one
sentence. Five sentences at most. -->

## Users and jobs
<!-- Who has the problem, the job they are doing and what they do today, with evidence.
One primary user. Cite each fact. -->

## Intended behaviour
<!-- What the feature does, what it must never do, and what it does when unsure: ask,
decline or hand over. What the model can see and what it can do. Non-goals. -->

| The feature should | The feature must never | When unsure, it |
| --- | --- | --- |
| | | |

## Examples
<!-- The examples are the spec. For each main case and edge case: the input, a good output,
a bad output, and why the bad one is bad. Include an ambiguous request and one out of
scope. -->

| # | Input | Good output | Bad output | Why it is bad |
| --- | --- | --- | --- | --- |
| 1 | | | | |

## Failure modes
<!-- Hallucination, wrong refusal, over-reach, prompt injection, drift, and any specific to
this feature. For each: how likely, how bad, how it is detected, what the user sees. Rank
by likelihood × damage. -->

| Failure mode | Likelihood | Damage | How detected | What the user sees |
| --- | --- | --- | --- | --- |
| | | | | |

## Misuse and safety
<!-- Who would abuse it and the worst output. The feature acts with the user's permissions,
never more. What data goes to the model, where it is processed, what is kept and for how
long. -->

## Evaluation
<!-- The acceptance criteria. Datasets (source, size, who labelled), graders, thresholds
set before the first run, slices, and the regression set. Separate hard gates from targets.
Link the eval plan if there is one. -->

| What is measured | Dataset | Grader | Threshold | Hard gate or target |
| --- | --- | --- | --- | --- |
| | | | | |

## Cost and latency
<!-- Budget per request and per active user per month. Latency at p50 and p95, and what the
user sees while waiting. What happens over budget. -->

| Budget | Target | Over budget, we |
| --- | --- | --- |
| | | |

## Human oversight
<!-- The level: suggest, draft for approval, act with undo, or act alone, and why. What the
reviewer sees. The path that works without the model. What the user does when the model is
wrong: see why, correct, undo, report. -->

## Rollout and monitoring
<!-- Stages with gates: eval thresholds plus production signals (acceptance, edits,
reverts, reports). The outcome metric and a guardrail, each with a number, a date and a
source. The kill switch and who holds it. Evals re-run on every model or prompt change. -->

| Stage | Audience | Gate to the next stage |
| --- | --- | --- |
| | | |

## Open questions
<!-- The riskiest assumption and the cheapest test of it. Every [unverified] item, each
with a suggested owner. -->
1. <question> (owner: <name>)
