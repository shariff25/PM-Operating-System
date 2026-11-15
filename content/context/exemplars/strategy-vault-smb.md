---
title: "Exemplar: strategy, Acme Vault for small teams"
owner: "TODO"
last-reviewed: 2026-11-14
sensitivity: internal
---

# Strategy: Acme Vault for small engineering teams, 2027 H1

Example: replace with your team's best.

2026-11-02 · PM, Acme Vault self-serve · Status: Draft

## Recommendation
**Recommendation: win engineering teams of 5 to 50 on the fastest path to a first secret read
in production, self-serve, and do not add sales capacity for them.** It should lift
first-week activation from 31% to 40% and 90-day paid conversion from 2.1% to 3.0% by
2027-06-30. It costs the two squads now on enterprise reporting for two quarters, and the
hobby tier we planned.

## Diagnosis
- **Enterprise growth is capped by sales capacity, not demand.** Six account executives
  closed 41 new Enterprise accounts in the last four quarters; the qualified pipeline they
  could not work was 3.2 times that (Confluence 4410, FY26 sales review).
- **Small teams arrive but stall.** About 7,800 workspaces are created each month. 31% read a
  secret from an application in their first week, and 2.1% pay within 90 days (Confluence
  4870, growth metrics, September 2026).
- **Teams that activate pay; teams that don't, don't.** Activated workspaces pay within 90
  days at 5.8%, others at 0.4% (same source). This is a correlation, not yet a cause.
- **The crux is the gap between storing a secret and reading it from code.** In 14 recorded
  onboarding sessions, 11 teams stored a secret within ten minutes and 9 of them then
  stopped at the pipeline step (Confluence 4902, onboarding research). Missing features
  came up in 2 of 14.
- **What is true about us.** Our pipeline guides are good but buried. We have no self-serve
  single sign-on, and teams above 20 seats ask for it: 6 of 9 Team-plan accounts called in
  October (Confluence 4931, call synthesis). Initech's free tier wins solo developers
  (Confluence 4960, competitive brief); we do not need them.

## Guiding choice
Win teams of 5 to 50 engineers on time to first production secret, self-serve, instead of
hiring sales people to chase them or a free tier to chase solo developers.
- **Where to play:** teams that deploy through a pipeline, sign up without talking to us,
  and pay by card. Not solo developers; not accounts that need a security review to buy.
- **How to win:** the product teaches itself. Every step from sign-up to the first read
  happens without a person from Acme.

## What we will do

| # | Action | Owner | By when | Measure it moves |
| --- | --- | --- | --- | --- |
| 1 | Onboarding that ends at a secret read from a real service, tested as an A/B experiment | Growth PM | 2026-12-15 | First-week activation |
| 2 | Native integrations for the three most used deploy pipelines in our sign-ups | Integrations PM | 2027-03-31 | Activation; time to first read |
| 3 | Self-serve single sign-on on the Team plan above 20 seats | Identity PM | 2027-05-31 | Expansion revenue; seats per team |

Action 1 goes first because it tests the riskiest assumption below. Actions 2 and 3 start
only if it holds. Each one raises the share of sign-ups that reach value without talking
to us.

## What we won't do
- **A hobby tier.** It would lift sign-ups, not activation of teams, and Initech already
  wins that segment free. Cost: we give up solo developers who might grow into teams.
- **Sales outreach below $15,000 a year.** Self-serve must work on its own. Cost: some
  mid-sized deals close slower.
- **Self-hosted features for the Team plan.** They serve 3% of Team workspaces' asks
  (Jira VLT filter 811) and would take one squad a quarter.
- **Enterprise reporting v2 this half.** The squads move to actions 1 and 2. Cost: two
  Enterprise renewals asked for it (Jira VLT-1702, VLT-1744).

## Alternatives considered

| Alternative | What it buys | Why it lost |
| --- | --- | --- |
| Hire four account executives for small teams | About 25 more paid accounts a quarter `[unverified]`, fast | Each costs more than a small team pays in three years; it does not fix the stall |
| A free hobby tier | More sign-ups and word of mouth | Wins solo developers, where Initech is free and we gain little; does not touch activation |
| Do the onboarding fix only, and keep enterprise reporting | Cheaper: one squad, not two | Tests the bet but too slowly to act on in H1; integrations would wait a year |

## Risks

| Risk or assumption | Early warning sign | Test or mitigation |
| --- | --- | --- |
| **Riskiest:** activation causes payment, rather than good teams doing both | Checklist-activated teams pay at the old rate | The onboarding A/B test keeps a 5% holdout; read 90-day paid conversion on 2027-01-04. If within 0.3 points of the holdout, revisit the diagnosis |
| Pipeline integrations take longer than a quarter | Second integration slips past 2027-02-15 | Ship the most used one first; guides cover the rest |
| Single sign-on pulls us into enterprise identity work | Asks for provisioning or custom roles from Team accounts | Scope to sign-in only; decision doc before any provisioning |

One-way decisions: Team-plan pricing once announced, and any third party in the login path.
Both get a decision document. The onboarding changes are two-way and do not.

## How we'll know

| Metric | Baseline | Target | By when | How measured |
| --- | --- | --- | --- | --- |
| First-week activation | 31% | 40% | 2027-06-30 | Growth metrics, Confluence 4870 definition |
| 90-day paid conversion | 2.1% | 3.0% | 2027-06-30 | Billing events by sign-up cohort |
| Guardrail: Enterprise gross revenue retention | 94% | Not below 92% | Quarterly | FY revenue review |

**Kill criterion:** if first-week activation is under 35% on 2027-03-31 after the onboarding
work ships, stop actions 2 and 3 and revisit the diagnosis.

Open questions:
1. What do four extra account executives actually close? The estimate above is a guess.
   (owner: Sales operations)
2. Which three pipelines lead in sign-ups this quarter? (owner: Growth analyst)
