# Decisions

Dated decision records: what was decided, by whom, why, and what it replaced. Name files YYYY-MM-<slug>.md.

Every file here needs front matter:

```yaml
---
owner: "@handle"
last-reviewed: 2026-11-14
sensitivity: internal   # restricted | internal | public
---
```

Files not reviewed for 90 days are flagged stale by `pmos lint`.
