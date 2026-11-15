# Staff and Principal PMs: build spec

PM OS covers a Senior PM's work well. This adds what Staff and Principal PMs need: a level
that changes the altitude the bot works at, skills for work across teams and for company
bets, a view across teams, and a register of bets that lasts for months. The plan is in
`docs/implementation-plan.md`; how PMs use it is in `docs/levels.md`.

**The rules every part follows** (unchanged):
- **A PM owns every output.** Every write is an approval, including a change to the bets
  register.
- **Numbers come from tools.** The model never counts issues or adds up cash.
- **Everything reads as the PM,** with their own token.
- **No real company is named.** Examples use the fictional Acme Vault and Acme Cloud, Globex
  (a customer) and Initech (a competitor).

## 1. Levels

**The levels:** `senior` (the default), `staff`, `principal`.

**Where a level lives: the bot runtime** (`crates/bots`), because the runtime builds each
task's claim. It is the PM's own working preference, so they set it themselves; `team.yaml`
would need a platform owner's commit.

- **Store:** `member_settings {handle (key), level, updated_at}`, on the memory and Postgres
  stores (the next migration in `crates/bots/migrations`).
- **Routes:**
  - `GET /v1/members/{handle}/settings` → `{"handle": "...", "level": "senior"}`. An unset
    level reads as `senior`. Clients: `slack` (own handle only, from `X-Pmos-User`) and
    `console`.
  - `PUT /v1/members/{handle}/settings` with `{"level": "staff"}` → the same shape. Client:
    `slack`, own handle only, and the handle must be a member. An unknown level is a `400`;
    another member's handle is a `403`.
- **The claim** gains `level`: the task's own `level` if it has one, else the setting of
  `task.requested_by`, else `senior`.
- **A task's own level:** `POST /v1/tasks` accepts an optional `level`, from the `evals`
  client only (`403` from any other client). It is stored on the task. This is how a
  benchmark category runs at a level.

**The harness** (`harness/src/runtime-client.ts`, `worker.ts`):
- `ClaimSchema` gains `level`, default `"senior"`.
- `botSystemPrompt` adds, after its first line:
  - for every bot: `Today is <YYYY-MM-DD>.` (UTC, from a clock the tests can set);
  - for a personal bot (`bot.scope === "personal"`):
    `Your PM works at the <Senior|Staff|Principal> level. Read /content/playbooks/levels.md and work at that level's scope, horizon and audience, unless the request says otherwise.`

**Slack** (DM commands, alone in a message, for members):
- `level` → "You're set to the **Staff** level. Change it with `level senior`, `level staff`
  or `level principal`."
- `level staff` → "Done. I'll work at the Staff level: several teams or a product line, 4 to
  6 quarters ahead, written for directors and peer leads." The other two lines:
  - Senior: "one product area, this quarter and next, written for the team and its
    stakeholders";
  - Principal: "the company and its market, 2 to 3 years ahead, written for executives and
    the board".
- An unknown level lists the three. A runtime failure says so and changes nothing.

**What a level changes** is content: `content/playbooks/levels.md`.

| | Senior | Staff | Principal |
| --- | --- | --- | --- |
| Scope | One product area | Several teams or a product line | The company and its market |
| Horizon | This quarter and next | 4 to 6 quarters | 2 to 3 years |
| Audience | The team and its stakeholders | Directors and peer leads | Executives and the board |
| The recommendation is about | What to build | Where to invest and what to stop | Which bets define the business |
| Evidence expected | Customers and usage | Across teams, with cost | Market, business model and competitors |
| Extra sections (below) | none | Cross-team effects | Cross-team effects, What must be true |
| Reviewers added by default | none | `executive`, `engineering-lead` | `cfo`, `cto`, `ceo` |

- **Extra sections.** At Staff and Principal, a document from `strategy-doc`, `decision-doc`
  or `prd-draft` ends with `## Cross-team effects` (which teams pay, what they stop, who
  must agree). At Principal it also ends with `## What must be true` (the two or three
  conditions the bet depends on over 2 to 3 years, and the signpost that would show each is
  failing). They come after the template's own headings.
- **Extra bar checks** (in `levels.md`; `bar.md` points to them):
  - Staff: the document says what stops or isn't funded, and names the teams affected.
  - Principal: the business effect (revenue, cost or margin) is a range with its assumptions,
    and the document says what is hard to reverse at company scale.
- **Per request.** "Write this for the exec team" changes the audience for that request.

## 2. Skills, templates, reviewers, exemplars

**Skills** (`content/skills/<name>/SKILL.md`). Front matter as the other craft skills:
`model: "reasoning"`, `data-class: "restricted"`, `surfaces: "slack web cli"`,
`ships-in: "team-launch"`, `owner: "TODO"`, `version: "0.1.0"`, and `track`. Descriptions are
at most 400 characters. The six-step pattern of `docs/pm-craft-spec.md` §3 applies.

| Skill | Track | Template | Draft title starts | Tools beyond the base set |
| --- | --- | --- | --- | --- |
| `portfolio-review` | Leading across teams | `portfolio-review.md` | `Portfolio review:` | `jira.rollup` |
| `roadmap-tradeoffs` | Leading across teams | `roadmap-tradeoffs.md` | `Roadmap trade-offs:` | `jira.rollup` |
| `okr-draft` | Leading across teams | `okrs.md` | `OKRs:` | |
| `business-review` | Leading across teams | `business-review.md` | `Business review:` | `jira.rollup` |
| `alignment-plan` | Leading across teams | `alignment-plan.md` | `Alignment plan:` | |
| `coach-feedback` | Leading across teams | none (answers in the thread) | no draft | drops `confluence.create_draft` |
| `bets-register` | Leading across teams | none (the tool keeps the format) | no page; `bets.record` | see below |
| `vision-doc` | Company bets | `vision.md` | `Vision:` | |
| `investment-case` | Company bets | `investment-case.md` | `Investment case:` | `stats.cashflow` |
| `pricing-proposal` | Company bets | `pricing-proposal.md` | `Pricing:` | `stats.cashflow` |
| `platform-strategy` | Company bets | `platform-strategy.md` | `Platform strategy:` | |
| `exec-brief` | Company bets | `exec-brief.md` | `Exec brief:` | |

- **The base set:** `context.search context.read jira.search jira.read confluence.search
  confluence.read confluence.create_draft docs.search docs.read`.
- **`bets-register` tools:** `context.search context.read jira.search jira.read jira.rollup
  confluence.search confluence.read docs.search docs.read bets.list bets.read bets.record`.

**Skill rules:**
- `portfolio-review`: reports where things stand; it calls `jira.rollup` and quotes its
  counts and links exactly. It lists dependencies and conflicts between teams and what is at
  risk. It recommends changes but decides nothing.
- `roadmap-tradeoffs`: a choice between investments. It always says what stops, and what
  each team gives up.
- `okr-draft`: one objective and at most four key results, each a number with a baseline, a
  target, a date and a source. Outputs are not key results.
- `business-review`: results against the plan, then why, then what changes. No activity lists.
- `alignment-plan`: who decides, who must agree, each person's likely objection and the
  answer, the order to talk to them, and the fallback if they say no. It names roles;
  it never guesses at a named person's motives.
- `coach-feedback`: feedback for another PM, written to teach: the three changes that matter
  most, each as a question first and a suggestion second, and one thing done well that is
  worth repeating. It answers the PM who asked; it sends nothing to the author.
- `vision-doc`: a narrative of the product in 2 to 3 years, from the customer's side, with
  what must be true and the signposts.
- `investment-case`: scenarios (low, base, high) with stated assumptions; it calls
  `stats.cashflow` and quotes payback and net present value exactly. Inputs with no source
  are marked `[unverified]`.
- `pricing-proposal`: the value metric, the packages, who pays more and who pays less, and
  how existing customers move. It never states a competitor's price without a source.
- `platform-strategy`: build, buy or partner, written with engineering; what becomes a
  platform and what stays a product.
- `exec-brief`: one page. The ask comes first, then what the reader needs in order to decide.
- `bets-register`: section 5.

**Template headings** (`##`, exact, in order):

| Template | Headings |
| --- | --- |
| `portfolio-review.md` | Bottom line, Where things stand, Dependencies and conflicts, At risk, What I'd change, Open questions |
| `roadmap-tradeoffs.md` | Recommendation, The choice, Options, What we stop, Cross-team effects, Sequence, Risks, How we'll know |
| `okrs.md` | Objective, Key results, What we won't chase, How each is measured, Risks |
| `business-review.md` | Bottom line, Results against plan, Why, What we are changing, Decisions needed, Next period |
| `alignment-plan.md` | The decision, Who decides, Who must agree, Objections and answers, Sequence, Fallback |
| `vision.md` | The future in one paragraph, Who it's for, What changes for them, What must be true, The path, What we won't be, Signposts |
| `investment-case.md` | Recommendation, The opportunity, Scenarios, Costs, Payback, Risks, Alternatives, What would change the answer |
| `pricing-proposal.md` | Recommendation, Value metric, Packages, Who pays more and who pays less, Migration, Test plan, Risks |
| `platform-strategy.md` | Recommendation, Diagnosis, Build, buy or partner, What becomes a platform, What must be true, Sequence, Risks |
| `exec-brief.md` | The ask, Why now, What you need to know, Options, Risk, Next step |

**Playbooks** (`content/playbooks/`): `levels.md`, `leading-across-teams.md`,
`company-bets.md`.

**Reviewers** (`content/reviewers/`): `cfo` (unit economics, cost, payback, what the money
would otherwise do), `cto` (technical strategy, platform risk, what scale breaks), `ceo`
(focus, what the company stops, the story to customers and the board). The `review` skill
adds the level's default reviewers and still runs at most 4.

**Exemplars** (`content/context/exemplars/`): `roadmap-tradeoffs-vault-2027`,
`vision-vault-2029`, `investment-case-sso`, `exec-brief-sso`. Each follows its template
exactly; the investment case's numbers are ones `stats.cashflow` returns.

**Role:** `pm-assistant` gets the 12 skills, and `bets.record: approve` in `actions`.

## 3. `jira.rollup` (tool server, system `jira`, kind `read`)

Counts issues across teams. It returns no descriptions or comments.

**Arguments:**
- `jql`: string, at most 2,000 characters, required.
- `group_by`: `project` (default), `epic`, `status`, `assignee` or `fix_version`.
- `max_issues`: 1 to 1,000, default 500.

**How it reads:** `GET /rest/api/3/search/jql` with the PM's token, 100 issues a page,
following `nextPageToken`, with
`fields=status,project,parent,fixVersions,duedate,issuelinks,assignee,issuetype`. It stops at
`max_issues` or after 45 seconds in all. A team bot's Jira scope applies as for search.

**It returns one item**, titled `Jira rollup: <n> issues by <group_by>`, with content:

```json
{
  "counted": 6, "truncated": false, "reason": null, "group_by": "project",
  "groups": [
    {"key": "PM", "name": "PM", "total": 3, "todo": 2, "in_progress": 1, "done": 0,
     "overdue": 0, "unassigned": 0}
  ],
  "links": [
    {"from": "PM-102", "from_group": "PM", "relation": "blocks",
     "to": "CLD-13", "to_group": "CLD", "to_state": "todo"}
  ],
  "links_truncated": false
}
```

- **`reason`** is `"cap"` or `"time"` when `truncated` is true.
- **State** comes from Jira's status category: `new` is `todo`, `indeterminate` is
  `in_progress`, `done` is `done`.
- **`overdue`:** a due date before today, and not done.
- **Groups:** at most 50, largest first; the rest are summed into one group with key
  `other`. An issue with no value for the grouping goes in group `none`. With
  `fix_version`, an issue counts in each of its versions.
- **`links`:** only links that cross groups, or that leave the query (`to_group` is `null`),
  each pair once, at most 200. With `group_by: project`, the other issue's project is its
  key's prefix.

**Fake fixtures** (a set used only by the rollup, so search results don't change): the three
`PM` issues that exist (PM-101 in progress, PM-102 and PM-103 to do), and project `CLD`:
`CLD-11` (epic, in progress), `CLD-12` (story under CLD-11, done), `CLD-13` (story under
CLD-11, to do, due 2026-10-01, unassigned). `PM-102` blocks `CLD-13`. The fake returns all
six whatever the JQL says. By project that is `PM` 3 (2, 1, 0) and `CLD` 3 (1, 1, 1), with
one overdue and one unassigned in `CLD`, and one link.

## 4. `stats.cashflow` (tool server, system `stats`, kind `read`)

**Arguments:**
- `scenarios`: 1 to 5 of `{name, upfront, years}`. `upfront` is 0 or more. `years` is 1 to
  10 of `{revenue, cost}`, each 0 or more.
- `discount_rate`: 0 to 1, default 0.1.

**It returns** `{scenarios: [...], assumptions: {discount_rate}, note}`, each scenario with:
- `net_by_year`: revenue minus cost;
- `cumulative_by_year`: starting from minus `upfront`;
- `payback_years`: when the cumulative figure reaches zero, counted in years and fractions
  (straight-line within the year); `null` if it never does;
- `npv`: minus `upfront`, plus each year's net divided by `(1 + rate)^year`;
- `total_revenue`, `total_cost`.

Money is rounded to 2 decimals, `payback_years` to 1. Bad input is `InvalidArgs`. The note
says the result is arithmetic on the inputs given, not a forecast.

## 5. The bets register (tool server, system `bets`)

**A bet** is one note in the PM's own memory, key `bet-<key>`, so it lives wherever their
memory does (their private GitHub repo when memory is on GitHub). The tool server owns the
note's format: it parses and renders it, and the model never writes the whole note.

**Fields:** `key`, `title`, `status` (`open`, `confirmed`, `refuted`, `dropped`), `bet` (one
sentence), `metric`, `riskiest_assumption`, `test`, `opened`, `review_by`, `evidence`
(`[{date, text, source?}]`), `decisions` (`[{date, text, source?}]`), `updated_at`.

**Tools:**
- **`bets.list {status?}`** (read) → `{"bets": [{key, title, status, review_by,
  updated_at}]}`, soonest review first.
- **`bets.read {key}`** (read) → `{"bet": {...}}`; an unknown key is not found.
- **`bets.record`** (write; an approval, like every write):
  - `key`: 1 to 60 characters of `a-z`, `0-9` and `-`;
  - optional `title`, `status`, `bet`, `metric`, `riskiest_assumption`, `test` (each at most
    500 characters) and `review_by` (`YYYY-MM-DD`);
  - optional `evidence`: up to 5 of `{date, text (at most 500), source?}`;
  - optional `decision`: `{date, text (at most 500), source?}`.
  - **A new bet** needs `title`, `bet`, `metric`, `riskiest_assumption` and `review_by`; its
    status starts `open` and `opened` is today.
  - **An existing bet:** the fields given replace the old ones; evidence and the decision are
    appended.
  - **On approval** the server merges and saves, and answers `{"bet": {...}, "created":
    true|false}`. Nothing is saved before approval.
  - **A full note** (over the memory limit of 4,000 characters) drops its oldest evidence
    entries until it fits, and records how many were dropped.
- **Who may use them:** a PM's own sessions and their personal bot, as with the memory
  tools. Reading a bet raises the session to `internal`.

**The skill `bets-register`:**
- **Open a bet** from a strategy, decision or investment document: one `bets.record`.
- **Update:** new evidence with its source and date; a status change only when the metric or
  the test's result supports it, with the evidence in the same call.
- **List** the bets, soonest review first.
- **Review** (the routine below): for each open bet due within 7 days, gather what changed
  (`jira.rollup`, readouts and reviews in Confluence), propose one `bets.record` each, and
  finish with a summary: confirmed, refuted, still open, and what to decide.
- It never marks a bet confirmed or refuted without evidence it can cite.

**The routine** (Slack, as `reply assistant` does it): `bets review on`, `bets review off`
and `bets review` (status). The routine is on the PM's personal bot, at 09:00 on the 1st of
each month in their time zone (`0 9 1 * *`), with the prompt:
`[bets-review] Review my bets register with the bets-register skill: find the open bets due for review, gather new evidence, and propose updates.`

## 6. Benchmarks

- **`category.yaml` gains `level`** (optional: `senior`, `staff`, `principal`). The runner
  sends it on each task it creates.
- **`evals/categories/staff-craft/`** (`level: staff`) and
  **`evals/categories/principal-craft/`** (`level: principal`): `bot: personal`, no
  `pass_bar`, "seed: replace with case cards", about 8 tasks each:
  - a structure task per document skill (title prefix and three or more exact headings);
  - `portfolio-review` includes the rollup's numbers for the fixtures (3 issues in each
    project, and the link from PM-102 to CLD-13);
  - `investment-case` includes the payback `stats.cashflow` returns for the inputs in the
    ask;
  - opening a bet creates one `bets.record` draft with the required fields;
  - a `strategy-doc` at the Staff level includes "Cross-team effects"; at the Principal
    level it also includes "What must be true";
  - `coach-feedback` creates no draft.
- **Judges** (advisory): `says-what-stops` (Staff) and `business-effect-as-range`
  (Principal), with taxonomy tags `model/no-tradeoff-named` and `model/no-business-effect`.

## 7. Docs

- `docs/levels.md` (new, for PMs): the levels and what each changes, setting a level, the
  new skills by track with an example ask, the cross-team rollup, the business-case
  numbers, the bets register and its monthly review, and what PM OS still can't do (no
  finance, usage or CRM data).
- `docs/toolserver.md` (the new tools), `docs/bot-runtime-spec.md` (the settings store, the
  routes, `level` in the claim and on a task), `docs/evals.md`, `docs/contract.md`,
  `docs/pm-craft.md` (a pointer), `docs/slack-home.md` (the two new tracks), the plan status,
  the README and the changelog.
