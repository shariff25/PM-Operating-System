---
title: "Exemplar: experiment readout, onboarding checklist"
owner: "TODO"
last-reviewed: 2026-11-14
sensitivity: internal
---

# Readout: four-step onboarding checklist for new Team workspaces

Example: replace with your team's best.

2026-10-09 · Growth PM, Acme Vault · Ran 2026-09-07 to 2026-10-04 · Status: Draft

## Decision
**Recommendation: ship the checklist to all new Team workspaces, and keep a 5% holdout until
2027-01-04.** First-week activation rose from 31.0% to 34.9%, an absolute lift of 3.9 points
(95% interval 1.7 to 6.1), and both guardrails held. We tested whether a four-step checklist
ending at "read a secret from your app" helps new teams get past the stall between storing a
secret and reading it from code (Confluence 5120, the plan). The plan's rule was: ship if the
interval excludes zero and every guardrail holds; stop if the interval excludes the MDE;
otherwise iterate once. The planned sample of 3,583 workspaces per arm was reached in both arms.

## Result
From `stats.compare`, primary metric. First-week activation is workspaces that read a secret
from an application within 7 days of sign-up, over workspaces created, excluding internal and
test workspaces (Confluence 4870, metric definitions).

| Arm | Users | Conversions | Rate |
| --- | --- | --- | --- |
| Control | 3,612 | 1,120 | 31.0% |
| Treatment | 3,598 | 1,256 | 34.9% |

- Absolute lift 3.9 points; relative lift 12.6%; interval 1.7 to 6.1 points; p = 0.0004.
- The low end, 1.7 points, is above zero, so the lift is real. The MDE was 10% relative, or
  3.1 points. The interval contains it, so the lift may be smaller than we hoped. It is not
  shown to be at least the MDE.
- Segments named in the plan, each from `stats.compare`:

| Segment | Control | Treatment | Lift | Interval | p |
| --- | --- | --- | --- | --- | --- |
| 5 or more invited members | 520 of 1,410 (36.9%) | 575 of 1,402 (41.0%) | 4.1 pts | 0.5 to 7.7 | 0.025 |
| Fewer than 5 | 600 of 2,202 (27.2%) | 681 of 2,196 (31.0%) | 3.8 pts | 1.1 to 6.5 | 0.006 |

- Lift by weekly sign-up cohort, from the experiment dashboard (Confluence 5188), point
  estimates only and not tested: 4.4, 3.6, 4.1 and 3.5 points. No sign of a novelty effect
  fading within four weeks.

## Guardrails
Both from `stats.compare`, against the thresholds in the plan.

| Guardrail | Threshold | Change | Interval | Held? |
| --- | --- | --- | --- | --- |
| Workspaces contacting support within 14 days | Rise of no more than 2.0 pts | 9.0% to 9.6%, +0.6 pts | −0.8 to +1.9 | Yes, worst case 1.9 |
| Workspaces deleted within 14 days | Rise of no more than 1.5 pts | 2.6% to 2.8%, +0.2 pts | −0.5 to +1.0 | Yes |

The support guardrail held, but its bad end sits 0.1 points under the limit. Read it again on
the full rollout.

## What we learned
- **The hypothesis held for both team sizes.** New teams stall at the step from storing a
  secret to reading it, and a checklist that ends at the read moves them past it. The lift is
  similar for small and larger teams, so we will not build a separate flow for each.
- **Another reading:** the checklist works because it links to the pipeline guides, not
  because of the checklist itself. 41% of treatment workspaces that activated opened a
  pipeline guide, against 23% in control (dashboard, Confluence 5188) `[unverified]`: this
  was not a planned metric. We prefer the checklist reading because the lift appears in
  workspaces that never opened a guide, but that split was not tested either.
- **What it does not show.** Whether activation turns into payment. Paid conversion is read
  at 90 days, and the strategy rests on it (Confluence 4950, small-team strategy).
- **Riskiest remaining assumption:** that teams activated by the checklist pay at the rate
  of teams that activated on their own. If the holdout shows paid conversion within 0.3
  points of control on 2027-01-04, the activation-drives-payment bet is in doubt.

## Next steps

| Step | Owner | By when |
| --- | --- | --- |
| Ship the checklist to 95% of new Team workspaces; 5% holdout | Growth PM | 2026-10-19 |
| Read support contacts on the full rollout against the 2.0-point limit | Support lead | 2026-11-16 |
| Read 90-day paid conversion, holdout against checklist | Growth analyst | 2027-01-04 |
| Test the pipeline-guide reading: guides inline in the checklist step | Growth PM | Plan by 2026-11-30 |

Not doing: a separate checklist for solo developers, and more checklist steps. Neither is
supported by this result.

Open questions:
1. Do checklist-activated teams pay at the same rate? (owner: Growth analyst, 2027-01-04)
2. The pipeline-guide split above is `[unverified]`; confirm from raw events. (owner: Growth
   analyst)
