# Exemplars

Documents that represent the team's best work, for retrieval (CTX-4). Skills find them with
`context.search` in the `exemplars` domain and copy their depth and tone, never their facts.

The nine files here are examples about the fictional Acme Vault. Each follows its template's
headings and passes every check in `/content/playbooks/bar.md`; the last four also pass the
Staff or Principal checks in `/content/playbooks/levels.md`. Replace them with your
team's best, one per document type, and keep the template headings.

| File | Template | What it shows |
| --- | --- | --- |
| `prd-audit-log-export.md` | `prd.md` | A request reframed as a job; alternatives with costs; a bet and its pre-build test |
| `strategy-vault-smb.md` | `strategy-doc.md` | A diagnosis with a crux, one guiding choice, non-goals that cost something |
| `decision-sso-build-or-partner.md` | `decision-doc.md` | Weighted criteria, margin arithmetic, reversibility, a pre-sale test |
| `readout-onboarding-checklist.md` | `experiment-readout.md` | Results from `stats.compare`, intervals read against the MDE, guardrails |
| `ai-spec-policy-assistant.md` | `ai-feature-spec.md` | Behaviour by example, ranked failure modes, evals as gates, oversight |
| `roadmap-tradeoffs-vault-2027.md` | `roadmap-tradeoffs.md` | Staff: rollup counts and links read as signals, what stops, one line per team |
| `vision-vault-2029.md` | `vision.md` | Principal: a customer narrative, what must be true, signposts with dates, the business effect as a range |
| `investment-case-sso.md` | `investment-case.md` | Principal: three scenarios with `stats.cashflow` payback and NPV, the input that decides it |
| `exec-brief-sso.md` | `exec-brief.md` | Principal: the ask first, one page, options with what each gives up |

Past PRDs, POC plans and advisories can sit alongside them; copy them in with their original
date. Every file here needs front matter:

```yaml
---
title: "<document title>"
owner: "@handle"
last-reviewed: 2026-11-14
sensitivity: internal   # restricted | internal | public
---
```

Files not reviewed for 90 days are flagged stale by `pmos lint`.
