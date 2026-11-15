---
title: Metrics and experiments
owner: "TODO"
version: "0.1.0"
---

# Metrics and experiments

Pick a few metrics that show customers getting value, define them exactly, and test changes
in a way you cannot fool yourself with. The stats tools do the arithmetic; you never do.

## Metrics

### North star and inputs
- **North star:** one metric that counts customers getting the product's core value and that
  leads revenue. "Workspaces reading a secret from production each week", not sign-ups, page
  views or revenue itself.
- **Input metrics:** three to five that a team can move within a quarter and that drive the
  north star: breadth (how many), depth (how much each), frequency, and conversion between
  steps. Say how each one feeds it.
- If no team can move a metric, it is a result to report, not an input to manage.

### Guardrails and counter-metrics
- For every target ask: how could we hit this and make the product worse? The answer is the
  counter-metric. A sign-up target needs activation; a speed target needs error rate.
- Give each guardrail a threshold: "support contacts per new workspace must not rise by more
  than 2 points".

### Leading and lagging
- Revenue and retention are lagging: true, and too slow to steer by. Pick a leading metric
  you have shown predicts them in your own data, and say how strong the link is.
- A correlation is a hypothesis until an experiment moves the leading metric and the lagging
  one follows.

### Definitions
Write each metric so two analysts would compute the same number:
- **Numerator** and **denominator**. Without a denominator it is a count, and counts grow
  with traffic.
- **Window:** "within 7 days of sign-up", "per calendar week".
- **Unit:** user, workspace or account. In a team product, usually the workspace.
- **Exclusions:** internal and test accounts, automated traffic.
- **Source:** the table or dashboard, and who owns it.

### What not to optimise
Name the metrics you will watch and not chase, and why. A measure that becomes a target gets
gamed: time in product, tickets closed, features shipped.

## Experiments

### Design
- **Hypothesis:** "Because <insight>, <change> will move <primary metric> from <baseline> by
  at least <MDE> for <population>."
- **One primary metric.** It decides. Secondary metrics explain; each extra primary metric
  raises the chance of a false win.
- **MDE (minimum detectable effect):** the smallest lift that would change the decision. Set
  it from what the lift is worth, not from the traffic you have.
- **Unit of randomisation:** the unit you analyse. Randomise workspaces, not users, when
  team-mates would otherwise see different versions.
- **Sample size and duration: always from `stats.sample_size`, never by hand.** Quote its
  output: per arm, total, days and the assumptions. Round the days up to whole weeks, two at
  the least, so every weekday is covered equally.
- If the tool says more than about eight weeks, do not run this test. Test a bolder change,
  a more sensitive metric or a larger population, or use another method.
- **Decision rule, written before launch:** what result ships it, what result stops it, and
  what happens in between. Include each guardrail's threshold.

### Running
- **No peeking.** Checking early and stopping when it looks good produces false wins. Fix the
  end date; look early only for breakage and guardrail breaches.
- Check the split. If arms differ in size by more than chance allows, the assignment is
  broken and the result is void.
- **Novelty and primacy.** Existing users react to change itself. Read the lift by week; a
  lift that fades is novelty.

### When not to test
- Too little traffic to detect a lift that matters.
- The decision is already made. Do not run a test for show.
- The change is one-way, or unfair to one arm: most pricing, security and legal changes.
- It is cheap to undo and clearly better. Ship behind a flag and monitor.
- Otherwise: a staged rollout with a holdout, a before-and-after with a comparison group, or
  five user sessions.

### Reading results
- Compare the arms with `stats.compare` and quote its output: both rates, the absolute and
  relative lift, the interval and the p-value. Never compute these yourself.
- **Intervals over p-values.** The interval is the range of lifts the data supports. Read
  both ends against the MDE and against zero.
- **Statistical is not practical.** A significant lift whose interval sits below the MDE is
  real and too small. A non-significant result whose interval still includes the MDE has not
  shown "no effect"; it was inconclusive.
- **Guardrails:** compare the bad end of the interval with the threshold, not the midpoint.
- **Segments:** only those named in the plan count as evidence. A slice found afterwards is
  a hypothesis for the next test.
- A result read before the planned sample size is not a result.

## Worked example: onboarding checklist (Acme Vault, fictional)
- **Metric.** First-week activation: workspaces that read a secret from an application
  within 7 days of sign-up, divided by workspaces created, excluding internal and test
  workspaces. Baseline 31%.
- **Hypothesis.** Because new teams stall between storing a secret and reading it from code,
  a four-step checklist will lift first-week activation by at least 10% relative.
- **Sample size.** `stats.sample_size` (proportion, baseline 0.31, MDE 0.10 relative, alpha
  0.05, power 0.8, two arms, 260 workspaces a day) returned 3,583 per arm, 7,166 in total,
  28 days. Four whole weeks.
- **Guardrail.** Support contacts per new workspace in 14 days must not rise by more than
  2 points.
- **Decision rule.** Ship if the interval for the lift excludes zero and the guardrail holds.
  Stop if the interval excludes the MDE. Otherwise iterate once.
- **Result.** `stats.compare` returned 31.0% against 34.9%: an absolute lift of 3.9 points,
  interval 1.7 to 6.1, p = 0.0004. The guardrail's worst case was a rise of 1.9 points,
  inside the limit. Ship, and keep a 5% holdout to watch whether paid conversion follows.
