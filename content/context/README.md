# Context library

The team's shared, owned source of truth for product context (CTX-1). Every agent and every PM
uses it. PM OS reads these files through the tool server (`context.search`, `context.read`),
which enforces each file's sensitivity label.

| Folder | Holds |
| --- | --- |
| `products/` | Products and product areas |
| `personas/` | Buyer and user personas |
| `competitors/` | Competitor files |
| `partners/` | Partner files |
| `stakeholders/` | Internal stakeholders |
| `style/` | Writing style |
| `glossary/` | Names and terms |
| `decisions/` | Dated decision records |
| `exemplars/` | Past documents worth copying |

Rules: one owner per file, a `last-reviewed` date, and a sensitivity label (`restricted`,
`internal` or `public`). Change a file by pull request; its owner reviews.
