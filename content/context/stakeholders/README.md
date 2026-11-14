# Stakeholders

Internal stakeholders by team: who reviews what, and how they like to be engaged.

Every file here needs front matter:

```yaml
---
owner: "@handle"
last-reviewed: 2026-11-14
sensitivity: internal   # restricted | internal | public
---
```

Files not reviewed for 90 days are flagged stale by `pmos lint`.
