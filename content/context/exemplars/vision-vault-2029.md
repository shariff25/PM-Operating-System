---
title: "Exemplar: vision, Acme Vault 2029"
owner: "TODO"
last-reviewed: 2026-11-14
sensitivity: internal
---

# Vision: Acme Vault for growing engineering teams, 2029

Example: replace with your team's best.

2026-11-13 · Principal PM, Acme Vault · Horizon: 2027 to 2029 · Status: Draft

**Recommendation:** adopt "access to production, self-serve, from 5 engineers to 500" as
the 2029 vision, and commit 2027 to its first bet: identity as a paid add-on for Team
workspaces above 20 seats.

## The future in one paragraph
It is 2029. A platform lead at a 120-engineer company joins on a Monday. By lunch she has
added a new service, and its secrets reached production through the pipeline without anyone
pasting a value into a chat. On Wednesday an engineer leaves; his access to every secret and
every environment ends the hour the identity provider removes him, and nobody keeps a
checklist. On Friday the auditor asks who read the payments database password last quarter;
she sends an export in five minutes. The company signed up with a card when it had eight
engineers, moved to the Business plan at 30, and first spoke to someone at Acme when it
wanted a custom contract at 200.

## Who it's for
- **Engineering teams of 5 to 500 that deploy through a pipeline** and buy software without
  a procurement process until they are large. About 7,800 workspaces sign up each month
  (Confluence 4870); 1,240 Team workspaces already have more than 20 seats (Confluence 4940).
- **The buyer changes as they grow:** a lead engineer at 5, an IT or platform lead at 30, a
  security lead at 200. The product must sell itself to each in turn.
- **Not for:** solo developers, where Initech is free and we gain little (Confluence 4960);
  and enterprises that need a self-hosted vault and a security review to buy, who stay with
  the sales-led Enterprise plan as today.

## What changes for them

| Job | Today | In the vision | Evidence |
| --- | --- | --- | --- |
| Get a secret into a deployment | Stored in minutes, then stuck at the pipeline step | Read from code in the first session | 9 of 14 onboarding sessions stopped at the pipeline (Confluence 4902) |
| Remove a leaver | By hand, from a checklist | Ends with the identity provider, within the hour | 17 Team workspaces with leavers still active (Jira VLT-2033) |
| Answer an auditor | Screenshots and a ticket to us | A self-serve export of who read what | Audit export is the top Team ask after single sign-on (Confluence 5302) |
| Give a service access | A shared, long-lived token | A machine identity with short-lived access | `[unverified]`: no research yet |

## What must be true
1. **Growing teams keep choosing a dedicated secrets tool** over the one built into their
   cloud provider, because it works across clouds and pipelines.
2. **Self-serve buyers pay for identity and audit, not only storage.** At least 15% of Team
   workspaces above 20 seats attach a paid identity or audit feature.
3. **A small team can run identity at scale.** Sign-in with every major identity provider
   costs under 0.5 of an engineer a year per 1,000 paying workspaces to keep working.

## The path

| Step | When | The bet | Metric that says it worked |
| --- | --- | --- | --- |
| 1. Activation and identity add-on | 2027 | Teams that reach a first production read pay; teams above 20 seats pay $1,200 a year for single sign-on | 90-day paid conversion 2.1% to 3.0% by 2027-06-30; 120 add-on workspaces by 2027-06-30 |
| 2. A Business plan | 2028 | Identity plus audit export make a plan between Team and Enterprise | 20% of eligible workspaces on it by 2028-12-31 |
| 3. Machine identities | 2029 | Services get short-lived access the same way people do | 25% of Business workspaces use it by 2029-12-31 |

- **Business effect, as a range:** the Business plan adds about $650,000 to $1.3 million a
  year by 2029. Assumptions: about 1,800 eligible workspaces by 2029 (today's 1,240 growing
  20% a year from 2027 `[unverified]`), 15% to 30% on the plan, and $2,400 a year more than Team.
- **Guardrail:** Enterprise gross revenue retention stays at or above 92% (baseline 94%, FY
  revenue review). A Business plan that pulls Enterprise accounts down fails.
- **Riskiest assumption:** condition 2. The 2027 pre-sale (50 workspaces, read on
  2026-12-18) is its first test; the add-on's attach rate a year after launch is the second.

## What we won't be
- **A free tool for solo developers.** Cost: fewer sign-ups and less word of mouth.
- **A general identity provider.** We connect to the customer's; we don't replace it. Cost:
  no revenue from small teams without one.
- **Sales-led below $15,000 a year.** Cost: some mid-sized deals close slower.
- **Hard to reverse at company scale:** the Business plan's price once customers pay it, a
  public machine-identity API once services depend on it, and the brand promise that
  nobody needs to talk to us. Each step ships first as something we can change: an add-on
  before a plan, a beta API with a version before a stable one.

## Signposts

| Condition | Signpost it is failing | Where we'd see it | By when |
| --- | --- | --- | --- |
| 1. Dedicated tool wins | Loss reviews citing the cloud provider's built-in tool double from today's share | Loss reviews (Confluence 5290) | Each half, from 2027-06-30 |
| 2. Self-serve pays for identity | Add-on attach under 10% a year after launch | Billing | 2028-06-30 |
| 3. Identity runs lean | Maintenance above 0.5 of an engineer per 1,000 workspaces | Identity squad time tracking | 2028-03-31 |

Open questions:
1. What share of loss reviews cite a cloud provider's built-in tool today? (owner: Product
   marketing)
2. Is 20% a year a fair growth rate for eligible workspaces? (owner: Growth analyst)
3. Do services need machine identities, or is it our idea? Needs research before 2028.
   (owner: Principal PM)
