---
title: "Exemplar: PRD, audit log export"
owner: "TODO"
last-reviewed: 2026-11-14
sensitivity: internal
---

# PRD: continuous audit log export to customer storage

Example: replace with your team's best.

2026-11-10 · Audit PM, Acme Vault · Status: Draft

## Summary
**Recommendation: deliver every audit event, hourly, to a storage bucket the customer owns,
for Enterprise workspaces.** The outcome is audit-related support tickets falling from 49 a
month to under 10 by 2027-04-30. Why now: three Enterprise deals lost in Q3 cited audit
export, and in-product retention is 90 days while most auditors ask for a year. We believe
security leads will stop filing tickets for audit evidence if events land in their own
storage, because the job is keeping a year of evidence where their tools already look, not
downloading a file.

## Problem and evidence
- **Who:** security leads at Enterprise workspaces who must prove to an auditor who read or
  changed which secret, over 12 months.
- **The cost today:** 146 support tickets in Q3 from 61 Enterprise accounts asked for audit
  data the console cannot give: older than 90 days, or in bulk (Confluence 5302, ticket
  review). Each takes support about 40 minutes to pull by hand (same source).
- **What they do today:** screenshot the console audit view, or file a ticket for a CSV.
  4 of 7 security leads interviewed keep a script that scrapes the audit API nightly; 2 of
  them said it breaks after our API changes (Confluence 5310, interviews).
- **Deals:** 3 Enterprise deals lost in Q3 named audit export as a reason; 2 of them chose
  Initech (Confluence 5290, loss reviews). Initech streams audit events to customer tools
  `[unverified]`: field report, not checked against their docs.
- **Evidence against:** 3 of 7 security leads only need evidence once a year, and a CSV
  would do for them.

## Goals and non-goals
Goals, each with a measure in Success metrics:
1. Security leads get a full year of audit events without filing a ticket.
2. Events reach the customer complete and in order, within two hours.

Non-goals for v1, each with the reason:
- Native connectors to named security tools: every customer uses a different one, and all
  of them can read from a bucket.
- Longer retention inside Acme Vault: it costs us storage and still does not put events
  where customer tools look.
- Team-plan workspaces: 2 of 146 tickets came from them.

The tradeoff: customers without their own bucket get nothing new in v1, and customers who
want events in a tool within seconds must wait for a streaming option.

## Users

| Persona | Who | Top jobs | Primary surface |
| --- | --- | --- | --- |
| Security lead (primary) | Owns audit evidence for an Enterprise workspace | Prove access history to auditors; investigate incidents | Console settings |
| Auditor (secondary) | External; never logs in | Receive a complete, tamper-evident record | Files from the security lead |

## Alternatives considered
Costs are the squad's estimates (Jira VLT-2120).

| Option | What it solves | Cost | Why it lost, or why it won |
| --- | --- | --- | --- |
| A. CSV download of the console view (the ask) | One-off evidence for 90 days | 2 weeks | Lost: no history past 90 days, and the scripts would stay |
| B. Hourly export to a customer-owned bucket | A year or more of evidence, in the customer's tools | 7 weeks | **Won:** removes the tickets and the scripts for every tool at once |
| C. Native connectors to the three most used security tools | Events in seconds, in those tools | 16 weeks, plus upkeep | Lost: covers about half the accounts that asked `[unverified]`; three integrations to maintain |
| Do nothing | Nothing; tickets stay at ~49 a month | Support time | Lost: costs deals |

## Solution overview
An admin adds a bucket and a write-only credential in settings. Every hour, Acme Vault
writes the hour's audit events there as compressed, line-delimited JSON, with a signed
manifest so an auditor can check nothing was removed. A status panel shows the last
delivery and any failure. The smallest version that tests the bet is hourly delivery and
the manifest; backfill of the last 90 days, a CSV view and streaming were cut to get there.

## Requirements

| ID | Requirement | Priority |
| --- | --- | --- |
| EXP-1 | An admin can configure one export destination per workspace, with a write-only credential | P0 |
| EXP-2 | Every audit event is written to the destination within 2 hours of occurring | P0 |
| EXP-3 | Each hourly file has a signed manifest listing event count and hash | P0 |
| EXP-4 | A failed delivery retries for 72 hours and alerts the admin after 3 failures | P0 |
| EXP-5 | On setup, events from the last 90 days are backfilled | P1 |
| EXP-6 | The settings page shows the last successful delivery time and error | P1 |

## Key user journeys
### J1. Security lead sets up export
1. Opens Settings, Audit export, and enters a bucket and a write-only credential.
2. Acme Vault writes a test file and shows "Test file delivered".
3. Within two hours the first hourly file lands, and the status panel shows its time.

## Non-functional requirements
| Area | Target |
| --- | --- |
| Performance | No more than 5 ms added at p95 to the audit write path |
| Scale | 50 million events a day across all workspaces |
| Availability | 99.9% of hours delivered on the first attempt |
| Security | Write-only credential, encrypted at rest; manifests signed with a workspace key |
| Platforms | Hosted Acme Vault; self-hosted in a later release |

## Success metrics

| Metric | Baseline | Target | By when | How measured |
| --- | --- | --- | --- | --- |
| Audit-related tickets from Enterprise | 49 a month (Q3) | Under 10 a month | 2027-04-30 | Support ticket tag `audit-data` |
| Enterprise workspaces with an active export | 0 | 40% | 2027-06-30 | Export configuration table |

| Guardrail | Threshold | How measured |
| --- | --- | --- |
| Audit write latency | p95 rises by no more than 5 ms | Service metrics |
| Delivery completeness | 99.99% of events delivered; zero gaps in manifests | Nightly reconciliation job |

## Launch criteria
- Reconciliation shows zero missing events for 14 days with design partners.
- Security review of the credential handling and signing passed.
- Docs and a support runbook for failed deliveries published.

## Rollout plan

| Phase | Audience | Dates | Gate to the next phase |
| --- | --- | --- | --- |
| 1 | 8 design partners, including Globex | 2027-01-11 to 2027-02-05 | Zero gaps; 5 of 8 ingest the files into their tools |
| 2 | All Enterprise workspaces, opt-in | From 2027-02-15 | Guardrails hold for 4 weeks |

Turned off per workspace by a flag; delivery pauses and resumes from the last manifest.

## Risks and mitigations

| Risk | Mitigation |
| --- | --- |
| **Riskiest assumption:** security leads will wire a bucket into their tools rather than want a file once a year | Before build, send a scripted daily file to 8 design partners for 2 weeks. If fewer than 4 ingest it, build option A instead |
| Customers misconfigure buckets and blame us for gaps | Test file on setup; alerts after 3 failures |
| Initech's streaming wins deals that need events in seconds | Track loss reasons; streaming is a later decision |

## Open questions
1. Does Initech stream audit events, and to which tools? (owner: PMM, from their public docs)
2. Is 90 days of backfill enough for the first audit after setup? (owner: Audit PM, ask design partners)
3. Can self-hosted customers use the same format? (owner: Engineering lead)
4. Would three native connectors cover half the 61 accounts? (owner: Audit PM, from tickets)

## Sources
- Confluence 5302, Q3 audit ticket review; 5310, security lead interviews; 5290, Q3 loss reviews.
- Jira VLT-2120, squad estimates for options A to C.
- Support ticket tag `audit-data`, July to September 2026.
