---
title: "Exemplar: decision, single sign-on build or partner"
owner: "TODO"
last-reviewed: 2026-11-14
sensitivity: internal
---

# Decision: build single sign-on for Team workspaces, or partner with a login broker

Example: replace with your team's best.

2026-11-06 · Identity PM, Acme Vault · Decider: VP Product · Decide by: 2026-11-20 · Status: Draft

## Recommendation
**Recommendation: build single sign-on ourselves, OpenID Connect first, and do not put a
login broker in the path.** Pre-sell it to 50 waiting-list workspaces before writing code. It
should reach 120 paying Team workspaces on the add-on by 2027-06-30 (baseline 0, billing
data), while sign-in failures stay under 0.5% of attempts.

## Context
- **Who has the problem:** admins of Team workspaces above 20 seats. 6 of 9 Team-plan
  accounts called in October asked for single sign-on; 5 of the 6 have more than 20 seats
  (Confluence 4931, call synthesis).
- **The job is removing leavers, not signing in faster.** "Every tool we add without single
  sign-on is a tool I have to remember to remove people from." (Head of IT, 60-seat
  account, 2026-10-14, Confluence 4925). Today admins remove leavers by hand from a
  checklist. 17 Team workspaces had a removed employee's account still active in the last
  audit sample (Jira VLT-2033).
- **Size:** 1,240 Team workspaces have more than 20 seats. The base case is about $200,000 a
  year in expansion revenue, range $120,000 to $310,000 (Confluence 4940, sizing).
- **Why now:** the small-team strategy names self-serve single sign-on as action 3
  (Confluence 4950). Globex, our largest Team account, renews on 2027-03-01 and has raised
  user provisioning four times (Jira VLT-1990).
- **Constraints:** one identity squad of three engineers; Enterprise single sign-on is
  sales-assisted and stays as it is.

## Options

### A. Build in-house, OpenID Connect first
Our own sign-in with OpenID Connect, which 78% of target admins' identity providers support
(survey of 214 admins, Confluence 4938). SAML follows only if the add-on sells. About 6 weeks
for three engineers, then 0.3 of an engineer to maintain (squad estimate, Jira VLT-2051).

### B. Partner with a login broker
A third-party broker handles SAML and OpenID Connect for every provider. About 3 weeks to
integrate. The fee is about $1.50 per user a month `[unverified]`: the quote is verbal.

### C. Cheapest: guided setup through the identity provider's app catalogue, no add-on
Publish setup guides so admins manage access from their identity provider with shared
credentials and a reminder to remove leavers. About 1 week of docs. No new revenue.

## Criteria
1. **Security of the login path.** We sell a secrets manager; a weak login path breaks the
   promise. This outranks speed.
2. **Margin on the add-on** at the base case.
3. **Coverage** of target workspaces' identity providers.
4. **Time to first paying workspace.**

## Tradeoffs

| Criterion | A. Build | B. Broker | C. Guides |
| --- | --- | --- | --- |
| Security of login path | Ours to review and patch | A third party sees every sign-in; needs a security review | Shared credentials; worse than today |
| Margin at base case | About 85% after maintenance | About 45%: ~170 workspaces × ~35 seats × $1.50 × 12 ≈ $107,000 a year in fees | No revenue |
| Coverage | 78% at first, about 95% with SAML | About 95% | Any provider, poorly |
| Time to first paying workspace | About 8 weeks | About 4 weeks | Not applicable |

- **Why B lost:** it is 4 weeks faster but gives up about half the margin, and puts a third
  party in the login path of a secrets product. Our security architect would block it
  without a full review (Confluence 4945, review notes).
- **Why C lost:** it does not solve the job. Admins still remove leavers by hand.
- **What A gives up:** 22% of target admins wait for SAML, and if the add-on does not sell
  we have spent 18 engineer-weeks on it. Workspaces under 20 seats are not served.

## Reversibility
- **A is two-way.** If the add-on fails, we stop at OpenID Connect and keep it for
  Enterprise. The cost is the six weeks already spent.
- **B is close to one-way.** Each customer's identity provider would point at the broker.
  Leaving means every customer reconfigures, and our sign-in breaks for any who do not.
- **Riskiest assumption:** that 15% to 30% of eligible workspaces will pay $1,200 a year.
  The attach rate moves the sizing by a factor of two; nothing else moves it that much.
- **Cheapest test:** offer the add-on at $1,200 to 50 waiting-list workspaces for 3 weeks,
  with an order form and payment on launch. If fewer than 8 commit, stop before build.
- **Review date:** 2026-12-18, with the pre-sale result.

## Open questions
1. The broker fee is a verbal quote. Get it in writing before the decision. (owner: Identity PM)
2. Does SAML need its own decision, or does it follow on sales? (owner: VP Product)
3. Does Globex need provisioning to renew, or sign-in alone? (owner: Globex account manager)
