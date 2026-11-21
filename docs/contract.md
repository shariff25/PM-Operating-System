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
  roles/<role>/ROLE.md               bot roles (optional)
  bots/<name>/BOT.md                 named bots (optional)
  personas/<key>/PERSONA.md          the bot marketplace's catalog (optional)
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

## People: not content (`team.yaml` is retired)

Who may use PM OS, and their roles, are not in the content. They live in the tool server's
user database (`docs/user-database-spec.md`, `docs/runbooks/users.md`): the first admin is
made on the host, and everyone else joins through Slack or an admin's invite in the web app.
Until slice U7 a `content/team.yaml` listed them; no service reads one any more.

Rules:
- `team-file-retired`: a file named `team.yaml` anywhere in the content tree. An **error** for
  `pmos lint` and CI, pointing to `pmos-admin users import`, which reads an old file once on
  the host. A service loading its content (the tool server, the bot runtime, the Slack App
  Home) gets the same finding as a **warning** and ignores the file, so a leftover one, during
  an upgrade before its pull request deletes it, never stops a service. The lint finds the
  file; it doesn't read it.
- `people-handle` (warning): a `master/people/` folder whose name isn't a PM OS handle, so no
  one's personal context is read from it. Whether that person is a user is the database's to
  say.

Roles (what each may do is `pmos_contract::permissions`, the one matrix every service checks):
- **`pm`**, Product Manager: asks bots; sees and decides their own tasks, approvals, routines
  and traces; links their own accounts.
- **`reviewer`**: as `pm`, and sees other PMs' approvals and recommends a decision. The
  draft's owner confirms; a reviewer never decides someone else's approval.
- **`auditor`**: searches and exports audit metadata only (no trace bodies, prompts or Slack
  text). Nothing else.
- **`admin`**, platform owner: everything, except approving a policy change they authored
  (POL-2; the caller enforces it).

Several roles take the union. (`skill-author` was a fifth role in the old file; the import
drops it and keeps `pm`.)

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
- **Bots:** `bot-parse`, `bot-name`, `bot-description`, `bot-body-empty`, `bot-role-unknown`, `bot-owner-handle` (the owner isn't a PM OS handle; whether they are an active user is the user database's to say: the admin overview lists bots without an active owner), `bot-reads-personal`, and `bot-reads-empty` (warning).
- **Actions:** `action-auto`, `action-not-write`, `action-unknown-tool`.

## Personas: `personas/<key>/PERSONA.md`

The bot marketplace's catalog (`docs/bot-marketplace-spec.md` §2). In the product Chief,
Sprout and Echo are "bots"; in code they are **personas**: a role a PM's own personal bot
(`pm-<handle>`) runs as, chosen per ask. A persona has no sandbox of its own. The folder is
optional; a catalog with any persona needs Nudge.

```yaml
# personas/chief/PERSONA.md
---
name: chief                   # equals the folder; lowercase, digits, single hyphens, at most 32
display-name: Chief           # 1-24 ASCII letters, digits and single spaces; the Slack prefix
title: Chief of Staff         # 1-40 characters
role: chief-of-staff          # a role in roles/, at most as wide as pm-assistant (below)
mascot: chief                 # nudge | chief | sprout | echo
owner: "TODO"
version: "0.1.0"
offered: true                 # default false: whether PMs see and may use it
preview: true                 # default false: offered before its benchmark passes ("not yet benchmarked")
# benchmarked: true           # default false: the claim that its eval categories pass (lint-checked)
always-on: false              # default false; exactly one persona, `nudge`, sets true
order: 20                     # 0-1000, display order
summary: Runs your week: what needs you, what is slipping, and what to tell your leadership.
examples:                     # 1 to 6, each 1-80 characters
  - Prep me for the staff meeting
  - What slipped since Monday?
can-do: daily-brief meeting-prep week-review   # optional; skills of the role; default all
needs:                        # optional
  - source: analytics         # analytics | crm
    label: an analytics source                 # 1-40 characters
    without: I work from numbers you paste in or that a Confluence page holds, and I say so.
---
Notes for maintainers (optional): never shown to PMs, never sent to a model.
```

`title`, `summary`, `examples`, `needs[].label` and `needs[].without` are one line each, not
blank, with no `<`, `>` or control characters. The persona's instructions are its role's body.
What it reads and may draft is derived from its role's tools, never written here
(`crates/contract/src/personas.rs`): "reads" is the systems of its read tools other than
`context`, `memory`, `stats` and `bets`; "drafts" is the label of each write tool whose setting
(the stricter of its role's and `pm-assistant`'s) is `approve`. A need is met when the role has
a `read` tool whose `system` is the need's source.

**The ceiling.** The personal role is `pm-assistant` (`PERSONAL_ROLE` in
`crates/contract/src/onboarding.rs`). A persona can never do more than a PM's own bot:

| Rule | Code | Severity |
| --- | --- | --- |
| Every tool of the persona's role is a tool of `pm-assistant` (each extra one is named) | `persona-exceeds-base` | error |
| The role's data class is at most `pm-assistant`'s | `persona-data-class` | error |
| No write that `pm-assistant` sets to `never` is anything but `never` in the role | `persona-action` | error |
| Every write the role can make (through any of its skills) is listed in its `actions`: there is no fallback to `approve`, and the tool server refuses an unlisted one | `persona-action-unlisted` | error |
| The role carries the sections "Untrusted data", "Hard limits", "Source markers in drafts", "Look before you ask" and "Point of view" exactly as `pm-assistant` has them, once each; `pm-assistant` must have all five (`STANDING_SECTIONS`) | `persona-role-sections` | error |
| The role is not a team bot's role | `persona-role-team` | error |
| Every skill of the role lists `web` or `slack` in `surfaces` | `persona-skill-surface` | warning |

A persona's skills need not be `pm-assistant`'s (that is how a specialist gets a skill Nudge
doesn't have); their tools must be.

**Offering before a benchmark (D10 and the owner's Preview exception, 2026-11-21).**
`preview: true` (default `false`) offers a persona as a Preview, shown as "Preview: not yet
benchmarked". `pmos lint` refuses an offered persona that is neither always-on nor a Preview
unless every eval category naming it (`persona: <key>` in `evals/categories/*/category.yaml`) has
met its pass bar two weeks in a row in `evals/reports` (`persona-benchmark`, an error; `--evals`
names the evals directory, default `evals` beside the content root; a Preview whose category
passes gets a warning to drop the label). The services, the policy generator and the content
admin don't read the reports and don't check this rule; CI's `pmos lint` does.

**Fail safe without the lint (gate-fix-4, M2).** Since the services can't read the eval reports,
they show **every persona but the always-on one as a Preview unless its file says
`benchmarked: true`** (and not `preview: true`): `Persona::preview` is worked out so
(`personas::is_preview`), and the tool server's catalog, its admin view, the runtime's
`GET /v1/personas` and pmos-web all carry it. Content merged without CI (no branch protection,
a hand edit) that leaves both labels out is therefore still shown as "Preview: not yet
benchmarked". `benchmarked: true` is the one claim a full bot needs, and `pmos lint` makes it
true: with the reports read, the claim without a passing report is an error, an offered persona
with neither label is an error, and a persona whose categories pass without the claim gets a
warning (it stays a Preview until the file says so). Both labels at once is an error everywhere.

**A draft notice.** `draft-notice: "<one line>"` (optional, at most 160 characters, the text
rule) is put by the tool server at the top of the body of every Confluence page the persona
drafts, when it is recorded and again when it is written (`NOTICE_TOOLS`); the role's body must
state it (`persona-draft-notice`). Echo's is "Draft for internal review. Not approved for external
use." (`INTERNAL_REVIEW_NOTICE`), pinned by a test.

**Reserved names.** A key or display name that reads as one of `RESERVED_NAMES` (PM OS, its four
roles, words such as Admin, Owner or System, the systems it talks to, Slack's special words),
ignoring case and anything but letters and digits, is refused (`persona-reserved`). People's
names are in the user database: the admin view lists a persona named like a user
(`name_clashes`).

Other rules: `persona-parse` (front matter missing, unknown field or wrong type),
`persona-missing-file`, `persona-name` (folder, format, or a key or display name that is another
persona's in any case), `persona-text`, `persona-mascot`, `persona-version`, `persona-order`
(out of range; a duplicate is a warning), `persona-role-unknown`, `persona-base-missing` (no
`pm-assistant` role to check against), `persona-can-do`, `persona-need`, `persona-write-label` (a
write tool of the role with no "drafts" label in `WRITE_LABELS`), `persona-always-on` (exactly
one, named `nudge`, of role `pm-assistant`, offered) and `persona-count` (at most 16).

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
| `/content/skills/` | `content/skills/` | Only the skills of the bot's role; a personal bot of `pm-assistant` also gets every persona's skills |
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
