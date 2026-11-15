---
title: Eval plan template
owner: "TODO"
version: "0.1.0"
---

# Eval plan: <feature or model behaviour>

<as-of date> · <author> · Status: Draft

## What we are testing
<!-- Lead with the recommendation: the thresholds that gate launch, in one or two
sentences. Then the behaviour under test, for which users and jobs, and the decision the
results feed. What is out of scope. Link the feature spec. -->

## Datasets
<!-- Typical, edge and adversarial cases. For each set: where it came from, how many
cases, who labelled it and how disagreements were settled. A held-out set nobody tunes on.
Say what real usage the sets do not cover. -->

| Dataset | Source | Cases | Labelled by | Used for |
| --- | --- | --- | --- | --- |
| | | | | |

## Graders
<!-- Code where the answer can be checked exactly. A model grader with a written rubric
where judgement is needed, checked against human labels before it is trusted. People for
calibration and disputes. Other graders considered and why they lost. -->

| What is graded | Grader | How the grader was validated |
| --- | --- | --- |
| | | |

## Thresholds
<!-- Set before the first run. Hard gates block launch on one failure. Targets are judged
with their sample size in mind. Give the current baseline where one exists. -->

| Measure | Hard gate or target | Threshold | Baseline |
| --- | --- | --- | --- |
| | | | |

## Slices
<!-- The cuts every run reports: segment, language, input length, risk category. The floor
for each slice. An average can hide a failing slice. -->

| Slice | Why it matters | Floor |
| --- | --- | --- |
| | | |

## Regression set
<!-- Every failure found in testing or production becomes a case. Who adds cases, when the
set runs (every change of prompt, model or retrieval source) and what a failure blocks. -->

## Who reviews failures
<!-- Named owners, how often, and what they can decide (block, ship with a known issue, add
to the regression set). The riskiest assumption in this plan (usually that the datasets
match real use) and the cheapest check. Open questions and [unverified] items, each with an
owner. -->

| Review | Who | How often | Can decide |
| --- | --- | --- | --- |
| | | | |
