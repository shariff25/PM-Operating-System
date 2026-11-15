---
title: "Exemplar: investment case, self-serve single sign-on"
owner: "TODO"
last-reviewed: 2026-11-14
sensitivity: internal
---

# Investment case: self-serve single sign-on for Team workspaces above 20 seats

Example: replace with your team's best.

2026-11-12 · Principal PM, Acme Vault · Decider: CFO and VP Product · Status: Draft

## Recommendation
**Recommendation: invest $180,000 upfront in self-serve single sign-on, gated on the
pre-sale result on 2026-12-18.** The base case pays back in 1.3 years with a net present
value of $256,364 at a 10% discount rate; the range runs from 2.4 years and $8,279 (low) to
0.8 years and $559,519 (high), all from `stats.cashflow`. If fewer than 8 of 50 pre-sale
workspaces commit, we are in the low case and should not build.

## The opportunity
- **Who has the problem:** admins of Team workspaces above 20 seats, who remove leavers by
  hand. 6 of 9 Team-plan accounts called in October asked for single sign-on (Confluence
  4931). 17 Team workspaces had a removed employee still active in the last audit sample
  (Jira VLT-2033).
- **How many:** 1,240 Team workspaces have more than 20 seats (Confluence 4940, sizing).
- **Business effect:** about $200,000 a year in expansion revenue in year 1, range $120,000
  to $310,000 (Confluence 4940), rising to $150,000 to $460,000 by year 3.
- **Why now:** self-serve identity is action 3 of the small-team strategy (Confluence 4950),
  and Globex, our largest Team account, renews on 2027-03-01 (Jira VLT-1990).

## Scenarios
All three price the add-on at $1,200 per workspace a year (decision document, Confluence
4955). They differ in attach rate among the 1,240 eligible workspaces and in cost to serve.

| Scenario | Assumptions | Upfront | Year 1 net | Year 2 net | Year 3 net |
| --- | --- | --- | --- | --- | --- |
| Low | About 100, 117, 125 workspaces (10% by year 3); maintenance only | $180,000 | $60,000 | $80,000 | $90,000 |
| Base | About 167, 217, 250 workspaces (20% by year 3); support grows with them | $180,000 | $130,000 | $185,000 | $220,000 |
| High | About 258, 333, 383 workspaces (31% by year 3); SAML added in year 2 | $180,000 | $230,000 | $310,000 | $365,000 |

The inputs, revenue and cost by year:

| Scenario | Year 1 | Year 2 | Year 3 |
| --- | --- | --- | --- |
| Low | $120,000 revenue, $60,000 cost | $140,000, $60,000 | $150,000, $60,000 |
| Base | $200,000, $70,000 | $260,000, $75,000 | $300,000, $80,000 |
| High | $310,000, $80,000 | $400,000, $90,000 | $460,000, $95,000 |

Cumulative, from minus the upfront cost (`stats.cashflow`):

| Scenario | End of year 1 | End of year 2 | End of year 3 |
| --- | --- | --- | --- |
| Low | -$120,000 | -$40,000 | $50,000 |
| Base | -$50,000 | $135,000 | $355,000 |
| High | $50,000 | $360,000 | $725,000 |

- Attach rates: the base case sits inside the 15% to 30% range in the sizing; the low case
  is half the base by year 3. All three are `[unverified]` until the pre-sale reports.
- Totals over three years: revenue $410,000, $760,000 and $1,170,000; running cost
  $180,000, $225,000 and $265,000 (low, base, high).

## Costs
- **Upfront, $180,000:** OpenID Connect sign-in, 18 engineer-weeks, and SAML, 12
  engineer-weeks, at $4,000 an engineer-week, $120,000 (squad estimate, Jira VLT-2051;
  rate from Confluence 5520, FY27 planning rates); security review and penetration test,
  $35,000 (security architect's quote, Confluence 4945); pre-sale, billing and docs, $25,000.
- **Running, $60,000 to $95,000 a year:** 0.3 of an engineer to maintain, $60,000 at the
  FY27 rate; support and identity-provider testing that grow with workspaces in the base and
  high cases.
- **What the money would otherwise fund:** the third pipeline integration in Q4 2027, which
  the roadmap trade-offs put second in line (Confluence 5530).

## Payback
At a 10% discount rate. `stats.cashflow` notes these are arithmetic on the inputs given,
not a forecast.

| Scenario | Payback (years) | NPV | Discount rate |
| --- | --- | --- | --- |
| Low | 2.4 | $8,279.49 | 10% |
| Base | 1.3 | $256,363.64 | 10% |
| High | 0.8 | $559,519.16 | 10% |

The low case pays back inside three years but barely earns the cost of capital. The
decision turns on whether we are nearer the low case or the base.

## Risks
- **Riskiest assumption: attach rate.** Moving from 10% to 20% by year 3 changes the NPV by
  about $248,000. Cost is next: the whole gap in running cost between low and base is $45,000
  over three years. Test: the pre-sale to 50
  waiting-list workspaces at $1,200, read on 2026-12-18. Fewer than 8 commitments: stop.
- **Cannibalisation:** some Team workspaces that would have upgraded to Enterprise for
  single sign-on stay on Team. 9 upgrades last year cited single sign-on as the main reason
  `[unverified]`: from sales notes, not a reviewed source.
- **Hard to reverse at company scale:** a price customers have paid, and identity in the
  sign-in path of a secrets product. Launch at $1,200 as an add-on, not in a plan, so the
  price can change for new buyers; keep the sign-in in-house so we can patch it ourselves.

## Alternatives
- **Partner with a login broker:** about $107,000 a year in fees at the base case, roughly
  half the margin, and a third party in the login path (Confluence 4945).
- **Cheaper: setup guides only:** about $10,000 of docs, no revenue, and admins still remove
  leavers by hand.
- **Spend the $180,000 on the third pipeline integration:** it moves activation, not
  expansion, and has no sizing yet.

## What would change the answer
- **Measure:** workspaces on the add-on, baseline 0, target 120 by 2027-06-30 and 167 by
  2028-05-31, from billing. Guardrail: sign-in failures stay under 0.5% of attempts.
- **Stop:** fewer than 8 of 50 pre-sale commitments on 2026-12-18, or attach under 8% six
  months after launch.
- **Shrink:** if SAML is asked for by fewer than 10% of buyers, drop it and save $48,000.
- **Grow:** attach above 25% by 2027-12-31; consider folding identity into a Business plan.

Open questions:
1. Is 10% the discount rate finance wants for product bets? (owner: Finance partner)
2. The attach rates are unverified until the pre-sale. (owner: Identity PM)
3. How many Enterprise upgrades does single sign-on cause? (owner: Sales operations)
