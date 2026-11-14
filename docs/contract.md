# PM OS content contract

Skills, context, reviewer rubrics and templates are plain markdown in `content/`. Any harness
that reads them must follow this contract (MOD-1). Today that means Deep Agents for the team
service and Hermes on NemoClaw for personal agents. `pmos lint` checks it on every pull
request; the rule IDs below are what the lint reports.

```
content/
  tools.yaml                         tool registry
  master/AGENTS.md                   team master context (required)
  master/people/<handle>/AGENTS.md   personal master context (optional)
  skills/<name>/SKILL.md             one folder per skill
  context/<domain>/*.md              context library
  reviewers/*.md                     reviewer personas
  templates/*.md                     document templates
```

`README.md` files are documentation and are never linted as content.

## Tool registry: `tools.yaml`

```yaml
tools:
  - name: jira.create_issue   # <system>.<action>, lowercase; starts with its system
    system: jira
    kind: write               # read | internal | write
    description: Create an epic, story or task
```

| Kind | Meaning | Approval |
| --- | --- | --- |
| `read` | Reads under the signed-in user's own permissions | None |
| `internal` | Changes PM OS's own state, such as the user's memory | None |
| `write` | Writes outside PM OS: Jira, Confluence, Slack | Per action (OUT-4) |

Every connector acts as the signed-in user with that user's own token: Atlassian OAuth 2.0 for
Jira and Confluence, and the PM's Slack user token for Slack. Inside the harness, dots become
underscores (`jira.create_issue` is `jira_create_issue`), because OpenAI-style function names
allow no dots.

Rules: `tools-missing`, `tools-parse`, `tool-duplicate`, `tool-name`.

## Skills: `skills/<name>/SKILL.md`

The header follows the [Agent Skills](https://agentskills.io) spec, so any compliant harness
loads the same file. PM OS fields go under `metadata`, and every value is a quoted string.

```yaml
---
name: prd-draft                 # lowercase, digits, single hyphens; equals the folder name; ≤ 64
description: Write a first-draft PRD from a brief ...   # ≤ 1,024 characters; says when to use it
metadata:
  owner: "@handle"              # required
  version: "0.1.0"              # required, MAJOR.MINOR.PATCH
  model: "default"              # required: default | extraction (gateway model role)
  data-class: "restricted"      # required: public | internal | restricted
  tools: "context.search jira.read confluence.create_draft"   # required, space-separated
  surfaces: "slack web cli"     # optional, defaults to all three
  ships-in: "phase-0"           # optional: phase-0 | pilot | team-launch | later
---

# /prd-draft
Instructions ...
```

- `tools` is an allowlist: the skill may call only these tools (PRD: Authorization). Each must
  be in `tools.yaml`.
- `model` names a role, not a model. The gateway maps `default` and `extraction` to
  open-weight models, so a bake-off result changes no skill files.
- `data-class` is the most sensitive data the skill may handle. The gateway routes everything
  to local models.

Rules: `front-matter`, `skill-parse`, `skill-name`, `skill-description`, `skill-body-empty`,
`skill-metadata-missing`, `skill-metadata-type`, `skill-version`, `skill-model`,
`skill-data-class`, `skill-unknown-tool`, `skill-tool-duplicate`, `skill-surface`,
`skill-phase`, `skill-missing-file`.

## Master context: `master/AGENTS.md`

Plain markdown in the [AGENTS.md](https://agents.md) format, loaded into every session. The
team file comes first; a PM's `master/people/<handle>/AGENTS.md` adds to it (CTX-3).

Rules: `master-missing`, `master-empty`.

## Context library: `context/<domain>/*.md`

Domains: `products`, `personas`, `competitors`, `partners`, `stakeholders`, `style`,
`glossary`, `decisions`, `exemplars`.

```yaml
---
title: CCM                  # optional
owner: "@handle"            # required; "TODO" warns until someone takes it
last-reviewed: 2026-11-14   # required, YYYY-MM-DD
sensitivity: internal       # required: public | internal | restricted
---
```

Unknown fields are errors, which catches typos such as `last_reviewed`. A file not reviewed for
more than 90 days is a warning (CTX-2); a review date in the future is an error. The tool server
enforces `sensitivity` on every read.

Rules: `context-domain`, `context-parse`, `context-stale` (warning), `context-future-date`,
`context-body-empty` (warning), `owner-missing`, `owner-unassigned` (warning).

## Reviewers: `reviewers/*.md`

```yaml
---
name: Security architect
lens: Threat model; attestation and key-handling correctness
flags:
  - Wrong claims about TEEs, attestation or key release
owner: "@handle"            # optional
---
```

The body is the rubric. `/review` runs each chosen persona as a parallel sub-agent and merges
the findings by severity: blocker, major or minor (REV-1, REV-2).

Rules: `reviewer-parse`, `reviewer-flags`, `reviewer-body-empty`.

## Templates: `templates/*.md`

```yaml
---
title: PRD template         # optional
owner: "@handle"
version: "0.1.0"
---
```

Rules: `template-parse`, `template-version`, `template-body-empty`.

## Running the lint

```sh
cargo run -p pmos -- lint --root content                 # errors fail; warnings don't
cargo run -p pmos -- lint --root content --deny-warnings
cargo run -p pmos -- lint --root content --format json   # for tooling
cargo run -p pmos -- lint --today 2026-11-14             # pin the date for staleness
```
