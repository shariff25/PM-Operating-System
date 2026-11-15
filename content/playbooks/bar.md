---
title: The bar
owner: "TODO"
version: "0.1.0"
---

# The bar

Every document a skill drafts passes these seven checks before the PM sees it. They are what a
senior PM asks of any proposal. A draft that reads well and fails one is not ready.

## The seven checks

### 1. Problem first
Who has the problem, and the evidence, come before the solution.
- **Passes when:** a named user segment, their problem and at least one cited piece of
  evidence appear before any solution is described.
- **Fails when:** the draft opens with the feature, or the problem is only the absence of the
  feature ("users lack a dashboard").

### 2. Alternatives
At least two real options, and why the others lost.
- **Passes when:** two or more options a sensible team could have picked are named, one of
  them smaller or cheaper than the pick, each with the reason it lost.
- **Fails when:** the only alternatives are "do nothing" and a strawman, or options are
  listed with no reason given.

### 3. A recommendation, stated first
One, in the opening lines.
- **Passes when:** the first three lines say what the author recommends, labelled as a
  recommendation, and a reader who stops there knows what is being asked of them.
- **Fails when:** the document surveys options and leaves the choice to the reader, or the
  recommendation first appears after the background.

### 4. A measurable outcome
A metric with a number, a date and how it's measured, plus a guardrail metric that must not
get worse.
- **Passes when:** one outcome metric has a baseline, a target, a date and a data source, and
  one guardrail names what must not get worse and by how much.
- **Fails when:** success is an output ("ship by June"), an adjective ("better adoption") or
  a target with no baseline, date or source; or there is no guardrail.

### 5. Non-goals and tradeoffs
Stated, not implied.
- **Passes when:** the draft names what is out of scope and what the pick costs: who is not
  served, what gets slower or harder, what is given up.
- **Fails when:** non-goals are empty or trivial, or the pick is described as having no
  downside.

### 6. The riskiest assumption
And the cheapest way to test it.
- **Passes when:** one assumption is named as the one that sinks the plan if false, with a
  test that takes days or weeks, and the result that would change the plan.
- **Fails when:** risks are an unranked generic list (delays, low adoption), or the only test
  offered is building the whole thing.

### 7. Nothing unverified left unflagged
Cite or flag, without exception.
- **Passes when:** every customer, number, date, quote and competitor claim links to a source
  or carries `[unverified]`, and each flagged item is listed under the open questions or
  unknowns heading.
- **Fails when:** any figure or claim has neither a source nor a flag, or a sample size,
  interval or p-value was worked out by hand instead of with the stats tools.

## How to run the check
1. Run it once, after the draft is complete and before proposing it.
2. For each check, find the sentence in the draft that passes it. If you can't point to one,
   it fails.
3. Fix what fails, once, from the sources you already gathered.
4. Never fix a failure by inventing. A missing number, customer or date goes under the open
   questions or unknowns heading, with a suggested owner and how to find it.
5. In the reply to the PM, say which checks could not be passed from the sources, in one line.

## How the checks apply to narrower documents
Some documents are narrower than a PRD. Apply each check in the form that fits.

| Document | Recommendation is | Alternatives are | Outcome and guardrail are |
| --- | --- | --- | --- |
| Problem statement | Pursue now, park or drop | What users do today; other framings of the problem | The size of the problem and how we'd see it shrink |
| Strategy, decision, PRD, AI spec | The choice or the build | Options a team could have picked | As written in check 4 |
| Metrics tree | The north star to adopt | Candidate north stars that lost | The north star's target and the guardrails |
| Experiment plan | Run it, and the rule for acting | Other designs or not testing | The primary metric, the MDE and the guardrails |
| Experiment readout | Ship, iterate or stop | Other readings of the result | The measured lift with its interval |
| Call synthesis, competitive brief | What to do about it | Other readings of the evidence | The signal that would confirm or refute it |
| Opportunity sizing | Worth pursuing or not, at this size | Other methods and their answers | The estimate as a range, and what would change it |
| Eval plan | The thresholds that gate launch | Other graders or datasets considered | The thresholds, by slice |

The riskiest assumption (check 6) and cite or flag (check 7) apply to every document as
written.
