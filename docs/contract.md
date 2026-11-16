# PM OS content contract

Skills, context, playbooks, reviewer rubrics and templates are plain markdown in `content/`. Any harness
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
  playbooks/*.md                     how a kind of PM work is done, and the bar (optional)
  team.yaml                          who may use PM OS (optional)
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

Rules: `tools-missing`, `tools-parse`, `tool-duplicate`, `tool-name`,
`tool-harness-collision` (two names that become the same harness name).

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
  model: "default"              # required: default | extraction | reasoning (gateway model role)
  data-class: "restricted"      # required: public | internal | restricted
  tools: "context.search jira.read confluence.create_draft"   # required, space-separated
  surfaces: "slack web cli"     # optional, defaults to all three
  ships-in: "phase-0"           # optional: phase-0 | pilot | team-launch | later
  track: "Spec and review"      # optional, ≤ 40 characters: the catalog heading it is listed under
---

# /prd-draft
Instructions ...
```

- `tools` is an allowlist: the skill may call only these tools (PRD: Authorization). Each must
  be in `tools.yaml`.
- `model` names a role, not a model. The gateway maps `default`, `extraction` and `reasoning`
  to open-weight models, so a bake-off result changes no skill files.

| Role | Addressed as | Used for |
| --- | --- | --- |
| `default` | `pmos/default` | Answers and everyday drafts |
| `extraction` | `pmos/extraction` | Extraction and triage, and the benchmark judges |
| `reasoning` | `pmos/reasoning` | The PM-craft skills (`docs/pm-craft.md`): work that weighs alternatives and checks itself against the bar. It ships pointing at the same model as `default`; a platform owner can point it at a larger one in the gateway config (`docs/gateway.md`) |
- `data-class` is the most sensitive data the skill may handle. The gateway routes everything
  to local models.
- `track` groups skills into a catalog, such as the Slack App Home's **Skills** section
  (`docs/slack-home.md`). It is free text, but the shipped skills use ten: Everyday,
  Product sense, Strategy and decisions, Metrics and experiments, Spec and review, Customer
  and market, AI products, Delivery and launch, Leading across teams, Company bets. A skill without one is listed under
  "Other".

Rules: `front-matter`, `skill-parse`, `skill-name`, `skill-description`, `skill-body-empty`,
`skill-metadata-missing`, `skill-metadata-type`, `skill-version`, `skill-model`,
`skill-data-class`, `skill-unknown-tool`, `skill-tool-duplicate`, `skill-surface`,
`skill-phase`, `skill-track` (empty, or longer than 40 characters), `skill-missing-file`.

## Master context: `master/AGENTS.md`

Plain markdown in the [AGENTS.md](https://agents.md) format, loaded into every session. The
team file comes first; a PM's `master/people/<handle>/AGENTS.md` adds to it (CTX-3).

Rules: `master-missing`, `master-empty`.

## Context library: `context/<domain>/*.md`

Domains: `products`, `personas`, `competitors`, `partners`, `stakeholders`, `style`,
`glossary`, `decisions`, `exemplars`.

```yaml
---
title: Acme Vault           # optional
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
lens: Threat model; correctness of security claims and defaults
flags:
  - Wrong or unscoped security claims
owner: "@handle"            # optional
---
```

The body is the rubric. `/review` runs each chosen persona as a parallel sub-agent and merges
the findings by severity: blocker, major or minor (REV-1, REV-2).
- **One sub-agent per file.** When a role's skills include `review`, the harness registers a
  sub-agent for every file here: its name is the file stem, its description the `lens`, its
  prompt the rubric.
- **Read tools only.** A reviewer gets the session's `read` tools (`tools.yaml`) and nothing
  else, so it can't write, draft or approve.
- **At most 4 a run,** always including `senior-pm`.

Rules: `reviewer-parse`, `reviewer-flags`, `reviewer-body-empty`.

## Team: `team.yaml`

Who may use PM OS until SSO replaces this file (Phase 1). It's optional; without it nobody can
use the Slack bot.

```yaml
members:
  - handle: ana            # PM OS handle; matches master/people/<handle>/ if present
    slack-id: U012AB3CD    # Slack member id (U… or W…)
    email: ana@example.com # optional; Atlassian account email, needed to link Jira/Confluence
    roles: [pm, skill-author]   # pm | skill-author | admin | reviewer | auditor
```

Roles (what each may do is `pmos_contract::permissions`, the one matrix every service checks):
- **`pm`**, Product Manager: asks bots; sees and decides their own tasks, approvals, routines
  and traces; links their own accounts.
- **`skill-author`**: as `pm`, and proposes skill and bot changes by pull request.
- **`reviewer`**: as `pm`, and sees other PMs' approvals and recommends a decision. The
  draft's owner confirms; a reviewer never decides someone else's approval.
- **`auditor`**: searches and exports audit metadata only (no trace bodies, prompts or Slack
  text). Nothing else.
- **`admin`**, platform owner: everything, except approving a policy change they authored
  (POL-2; the caller enforces it).

Several roles take the union. A well-formed role name this version doesn't know (one added by
a newer version) still parses but grants nothing, so a new role doesn't break services that
aren't upgraded yet; versions before `reviewer` and `auditor` existed reject them, so upgrade
every service before using them.

With live connectors the tool server enforces this file: only members get sessions, and a
linked account must match the member (the Slack member id for Slack, the email for
Atlassian).

Rules: `team-parse`, `team-handle`, `team-slack-id`, `team-email`, `team-duplicate` (a
handle or Slack id listed twice), `team-roles` (no roles), `team-role-unknown` (warning: a
role this version doesn't know), `team-role-separation` (warning: admin and auditor together),
`people-unknown` (warning: a `master/people/` folder
for someone not on the team).

## Roles and bots: `roles/<role>/ROLE.md`, `bots/<name>/BOT.md`

PM OS is a team of named bots (`docs/bot-decisions.md`). Each bot is made from one **role**.

```yaml
# roles/pm-assistant/ROLE.md
---
name: pm-assistant            # equals the folder name
description: A PM's personal assistant …
owner: "@handle"
version: "0.1.0"
skills: "ask meeting-notes prd-draft"   # skills its bots may run
data-class: "restricted"      # the bots' ceiling; no skill may be above it
model: "default"              # gateway model role
actions:                      # per outbound tool: approve | never (auto is refused in v1)
  slack.send_message: approve
---
Standing instructions for the role.
```

```yaml
# bots/docs/BOT.md
---
name: docs
role: docs-helper
scope: team                   # team | personal
owner: "@handle"              # personal: the PM it works for
description: One line for the roster.
reads:                        # team bots only: what they may read
  jira-projects: [PM]
  confluence-spaces: [PM]
  slack-channels: [C0123ABC]
actions: {}                   # overrides of the role's settings
---
The bot's job description.
```

Rules:
- **Roles:** `role-parse`, `role-name`, `role-version`, `role-description`, `role-body-empty`, `role-skills`, `role-skill-unknown`, `role-skill-data-class`.
- **Bots:** `bot-parse`, `bot-name`, `bot-description`, `bot-body-empty`, `bot-role-unknown`, `bot-owner-unknown` (an error once `team.yaml` lists people), `bot-reads-personal`, and `bot-reads-empty` (warning).
- **Actions:** `action-auto`, `action-not-write`, `action-unknown-tool`.

## Templates: `templates/*.md`

```yaml
---
title: PRD template         # optional
owner: "@handle"
version: "0.1.0"
---
```

Rules: `template-parse`, `template-version`, `template-body-empty`.

**Headings are part of the contract.** A skill drafts from its template with the `##` headings
unchanged and in order, and the `pm-craft` benchmarks check them by name
(`docs/pm-craft-spec.md` §1 lists them). Change a heading and its benchmark task together.

## Playbooks: `playbooks/*.md`

A playbook says how a kind of PM work is done: what to ask, what good looks like, and the
shape of the answer. `bar.md` is the one every doc skill checks its draft against
(`docs/pm-craft.md`). The directory is optional.

```yaml
---
title: The bar              # optional
owner: "@handle"            # required
version: "0.1.0"            # required
---
```

Shipped: `bar`, `product-sense`, `strategy`, `metrics-and-experiments`, `customer-insight`
and `ai-products`.

Rules: `playbook-parse` (the front matter is missing or wrong), `playbook-body-empty`.

## What a sandbox sees

The harness mounts these read-only in a bot's sandbox, each only if the directory exists:

| Path in the sandbox | From | Which files |
| --- | --- | --- |
| `/content/skills/` | `content/skills/` | Only the skills of the bot's role |
| `/content/templates/` | `content/templates/` | All |
| `/content/reviewers/` | `content/reviewers/` | All |
| `/content/playbooks/` | `content/playbooks/` | All |

A skill reads its playbook and template by path, such as `/content/playbooks/bar.md` and
`/content/templates/decision-doc.md`. The context library is not mounted: it is read through
`context.search` and `context.read`, which enforce each file's `sensitivity`. Templates,
reviewers and playbooks carry no sensitivity label, so keep customer names and other
restricted material out of them and in the context library.

## Running the lint

```sh
cargo run -p pmos -- lint --root content                 # errors fail; warnings don't
cargo run -p pmos -- lint --root content --deny-warnings
cargo run -p pmos -- lint --root content --format json   # for tooling
cargo run -p pmos -- lint --today 2026-11-14             # pin the date for staleness
```
