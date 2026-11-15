# Products

One file per product or product area: what it does, who buys it, how it is deployed, current release, known limits.
Start from `example-product.md`: copy it to `<product-id>.md` and fill in the TODOs.

Every file here needs front matter:

```yaml
---
owner: "@handle"
last-reviewed: 2026-11-14
sensitivity: internal   # restricted | internal | public
---
```

Files not reviewed for 90 days are flagged stale by `pmos lint`.
