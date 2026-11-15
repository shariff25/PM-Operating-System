---
title: "Exemplar: exec brief, self-serve single sign-on"
owner: "TODO"
last-reviewed: 2026-11-14
sensitivity: internal
---

# Exec brief: fund self-serve single sign-on, gated on a pre-sale

Example: replace with your team's best.

2026-11-14 · Principal PM, Acme Vault · For: CEO, CFO, CTO · Decide by: 2026-11-27 · Status: Draft

## The ask
**Recommendation: approve $180,000 to build self-serve single sign-on for Team workspaces
above 20 seats, released only if 8 or more of 50 pre-sale workspaces commit by
2026-12-18.** We need your yes by 2026-11-27 to start the pre-sale this month.

## Why now
- **Globex, our largest Team account, renews on 2027-03-01** and has raised user
  provisioning four times (Jira VLT-1990). A yes now lets us promise single sign-on by
  2027-05-31 at that renewal; without one we have nothing to promise.
- **Q1 planning closes on 2026-11-27.** The identity squad's quarter is set then, and the
  platform change it needs (PLT-88) is planned to land by 2027-02-15.
- **Waiting costs the pre-sale's learning:** without it, the 2027 roadmap's identity step
  rests on an attach rate nobody has measured.

## What you need to know
- **The problem:** admins of Team workspaces above 20 seats remove leavers by hand. 6 of 9
  Team accounts called in October asked for single sign-on; 17 workspaces had a removed
  employee still active in the last audit sample (Confluence 4931, Jira VLT-2033).
- **The size:** 1,240 Team workspaces above 20 seats. About $200,000 a year in expansion
  revenue in year 1, range $120,000 to $310,000, at $1,200 a workspace a year (Confluence
  4940).
- **The return** (`stats.cashflow`, 10% discount rate, three years): the base case pays
  back in 1.3 years with a net present value of $256,364; low 2.4 years and $8,279; high 0.8
  years and $559,519 (investment case, Confluence 5540).
- **The assumption that decides it:** attach rate. Between the low and base cases it moves
  the net present value by about $248,000; the pre-sale tests it before we spend.
- **What it costs elsewhere:** the third pipeline integration moves from Q4 2027 to 2028.

## Options

| Option | Cost | Business effect | Gives up |
| --- | --- | --- | --- |
| A. Build, gated on the pre-sale (recommended) | $180,000 upfront; $60,000 to $95,000 a year to run | $120,000 to $310,000 in year 1; payback 0.8 to 2.4 years | The third integration until 2028 |
| B. Partner with a login broker | About 3 weeks to integrate; about $107,000 a year in fees at the base case | Same revenue at about half the margin | A third party in the login path of a secrets product |
| C. Setup guides only | About $10,000 of docs | None | The job: admins still remove leavers by hand |

- **Why not B:** it is faster by about 4 weeks, but halves the margin and puts a third party
  where our security architect would block it without a full review (Confluence 4945).
- **Why not C:** it costs almost nothing and solves nothing. Admins keep the checklist.

## Risk
- **The one that would make you say no:** the add-on cannibalises Enterprise upgrades.
  9 upgrades last year cited single sign-on `[unverified]`. We will track upgrades citing it
  each month; the guardrail is Enterprise gross revenue retention at or above 92% (today
  94%).
- **Also watch:** PLT-88 slipping past 2027-02-15 moves single sign-on to Q3, past the
  date we would promise Globex. The platform lead reports on it on 2027-02-01.
- **Hard to reverse at company scale:** a price customers have paid, and whoever is in the
  sign-in path. We launch as an add-on, not a plan, so the price can change for new buyers,
  and we keep the sign-in in-house.

## Next step
- **On a yes:** the Identity PM opens the pre-sale to 50 waiting-list workspaces on
  2026-11-30. The result comes to you on 2026-12-18 with a build or stop recommendation.
- **On a no:** we publish setup guides (option C) in December and tell Globex at renewal
  that single sign-on is not planned for 2027.
- **The bet goes in the bets register** with its metric (add-on workspaces, 120 by
  2027-06-30), the riskiest assumption and the review date.
- **Detail:** investment case (Confluence 5540), decision document (Confluence 4955),
  roadmap trade-offs for 2027 (Confluence 5530).

Open questions:
1. Is 10% the discount rate finance wants for product bets? (owner: Finance partner)
2. The broker fee is a verbal quote. (owner: Identity PM)
3. Does Globex need provisioning to renew, or sign-in alone? (owner: Globex account
   manager)
4. Which identity providers do the 50 pre-sale workspaces use? (owner: Identity PM)
