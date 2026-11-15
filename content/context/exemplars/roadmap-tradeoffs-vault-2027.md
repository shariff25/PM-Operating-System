---
title: "Exemplar: roadmap trade-offs, Acme Vault 2027"
owner: "TODO"
last-reviewed: 2026-11-14
sensitivity: internal
---

# Roadmap trade-offs: fund self-serve activation and identity, stop reporting v2, 2027

Example: replace with your team's best.

2026-11-10 · Staff PM, Acme Vault · Decider: VP Product · Decide by: 2026-11-27 · Status: Draft

## Recommendation
**Recommendation: put 2027 into self-serve activation and identity, and stop enterprise
reporting v2 until a review on 2027-07-15.** The two reporting squads move to onboarding and
pipeline integrations for H1; the identity squad builds single sign-on once the platform
session change lands. It should lift 90-day paid conversion from 2.1% to 3.0% by 2027-06-30
while Enterprise gross revenue retention stays at or above 92%.

## The choice
- **The decision:** where six engineers on enterprise reporting work in 2027. It has to be
  made before Q1 planning closes on 2026-11-27.
- **The goal:** the small-team strategy, which bets on activation and self-serve expansion
  (Confluence 4950). Baselines: first-week activation 31%, 90-day paid conversion 2.1%
  (Confluence 4870, September 2026).
- **What the rollup shows** (`jira.rollup`, `project in (VLT, IDN, RPT, INT)`, by project,
  2026-11-09: 162 issues counted, not truncated):

| Project | Total | To do | In progress | Done | Overdue | Unassigned |
| --- | --- | --- | --- | --- | --- | --- |
| RPT (reporting) | 51 | 15 | 30 | 6 | 3 | 2 |
| VLT (core and onboarding) | 44 | 20 | 12 | 12 | 1 | 3 |
| IDN (identity) | 34 | 22 | 5 | 7 | 4 | 6 |
| INT (integrations) | 33 | 18 | 8 | 7 | 2 | 4 |

- **Reading it:** reporting has 30 issues in progress across 6 engineers: too much started.
  Identity has 22 to do, 4 overdue and 6 unassigned with three engineers: starved.
- **Dependencies:** 6 identity issues are blocked by PLT-88, the platform team's session
  token change, which is outside the query (`to_group` is null, in progress). INT-17, the
  first pipeline integration, blocks VLT-2215, onboarding that ends at a real secret read.
- **Constraint:** no new hires before Q3 (Confluence 5512, FY27 headcount plan).

## Options

| Option | What it buys | People and quarters | Why it lost or won |
| --- | --- | --- | --- |
| A. Self-serve first (pick) | Onboarding and two pipeline integrations in H1; single sign-on by 2027-05-31 | Both reporting squads (6) to VLT and INT for H1; identity squad (3) unchanged | Won: it moves the strategy's two metrics and clears the starved identity backlog |
| B. Enterprise first | Reporting v2 by 2027-04-30 for the two renewals that asked | No moves; identity waits for H2 | Lost: protects about $380,000 `[unverified]` in two renewals (Jira VLT-1702, VLT-1744) but leaves activation where it is for a year |
| C. Cheaper: move one reporting squad | Onboarding in H1; integrations and reporting v2 both slip | 3 engineers to VLT for one quarter | Lost: tests the bet, but too slowly to act on in H1, and reporting v2 still has 30 issues open with half the people |
| D. Split everything evenly | Some progress everywhere | Each team keeps its people | Lost: funds four things partly and ships none in H1 |

## What we stop
- **Enterprise reporting v2,** until the review on 2027-07-15. The reporting lead keeps
  fixes and support. The 30 in-progress RPT issues are closed or moved to a parked epic by
  2026-12-15. Lost: the two renewals that asked for it (Jira VLT-1702, VLT-1744).
- **The hobby tier,** for 2027. It served solo developers, where Initech is free (Confluence
  4960). Lost: some word of mouth.
- **Self-hosted features for the Team plan.** 3% of Team asks (Jira VLT filter 811).
- **Told by:** the reporting lead to the two account owners by 2026-12-01, with the plan for
  the renewals below.

## Cross-team effects

| Team | Gives up | Gains | Must agree |
| --- | --- | --- | --- |
| Reporting | Both squads for H1; reporting v2 | A restart decision on 2027-07-15 with usage data | Reporting lead |
| Growth (VLT) | Nothing | Three engineers for onboarding | Growth PM |
| Integrations (INT) | Its own Q1 backlog order | Three engineers; INT-17 first | Integrations lead |
| Identity (IDN) | Nothing; waits on PLT-88 | Unblocked once PLT-88 lands | Identity PM |
| Platform (PLT) | Two weeks of its Q1 plan to finish PLT-88 by 2027-02-15 | Fewer one-off session fixes | Platform lead |
| Enterprise sales | Reporting v2 for two renewals | An export workaround and a date | Enterprise account director |

## Sequence
1. **Q1 2027.** Onboarding A/B result (already running; read 2027-01-04). Reporting squad A
   joins growth; squad B joins integrations on INT-17. Platform lands PLT-88 by 2027-02-15.
2. **Q2 2027.** Identity builds OpenID Connect sign-in after the pre-sale passes (8 or more
   of 50 commit, decision on 2026-12-18) and PLT-88 lands. Second pipeline integration.
3. **Q3 2027.** Review on 2027-07-15: restart reporting v2 or not, on Enterprise retention
   and add-on sales. SAML only if the add-on reaches 120 workspaces.
4. **Q4 2027.** Third integration, or reporting v2, per the Q3 review.

The pre-sale and the onboarding result come first because they test the riskiest
assumptions before any squad moves for long.

## Risks

| Risk or assumption | Early warning sign | Test or mitigation |
| --- | --- | --- |
| **Riskiest:** activation causes payment, not just good teams doing both | Checklist-activated teams pay at the old rate | 5% holdout in the onboarding test; read 2027-01-04. Within 0.3 points of holdout: reopen this choice |
| Losing reporting v2 costs a renewal | Either account asks for a discount or delays renewal | Export workaround from the audit log work; account director owns both by 2027-01-31 |
| PLT-88 slips past 2027-02-15 | Still in progress on 2027-02-01 | Identity starts on admin setup screens; single sign-on slips to Q3, not further |
| Moved squads take a quarter to be productive | Fewer than 5 VLT issues done by reporting engineers in January | Pair each with a growth engineer for the first sprint |

One-way: telling two Enterprise accounts reporting v2 is paused. Two-way: the squad moves,
which the Q3 review can undo at the cost of one quarter.

## How we'll know

| Metric | Baseline | Target | By when | How measured |
| --- | --- | --- | --- | --- |
| 90-day paid conversion | 2.1% | 3.0% | 2027-06-30 | Billing events by sign-up cohort |
| First-week activation | 31% | 40% | 2027-06-30 | Growth metrics, Confluence 4870 definition |
| Guardrail: Enterprise gross revenue retention | 94% | Not below 92% | Quarterly | FY revenue review |

**Review:** 2027-07-15. If activation is under 35% on 2027-03-31, the reporting squads go
back and this choice is reopened.

Open questions:
1. Does PLT-88 have a committed date? The rollup shows it in progress only. (owner: Platform lead)
2. What are the two renewals worth exactly? $380,000 is from the account plan
   `[unverified]`. (owner: Enterprise account director)
