# PM craft: build spec

PM OS should work like a senior product manager at a top technology company: it questions the
problem, weighs alternatives, recommends, gets its numbers right and checks its own work. The
plan is in `docs/implementation-plan.md`; how PMs use it is in `docs/pm-craft.md`.

**Why structure, not prompts.** The models are local and mid-sized. Quality comes from a
written bar, playbooks and exemplars to work from, a draft-critique-revise pass, a review
panel, numbers from tools, and benchmarks that say whether the bar is met.

**The rules every part follows:**
- **A PM owns every output.** Every write stays an approval. Nothing here changes that.
- **Cite or flag** still holds; a recommendation is an opinion and is labelled as one.
- **No company is named** in the content. Examples use the fictional Acme Vault and Acme
  Cloud, `docs.example.com`, Globex (a customer) and Initech (a competitor).

## 1. Names (the contract between the three builds)

**Playbooks** (`content/playbooks/<name>.md`, front matter `title`, `owner`, `version`):
`bar`, `product-sense`, `strategy`, `metrics-and-experiments`, `customer-insight`,
`ai-products`.

**Skills** (`content/skills/<name>/SKILL.md`), their template and the title their draft gets:

| Skill | Template (`content/templates/`) | Draft title starts | Extra tools beyond the base set |
| --- | --- | --- | --- |
| `product-critique` | none (answers in the thread) | no draft | |
| `problem-framing` | `problem-statement.md` | `Problem:` | |
| `strategy-doc` | `strategy-doc.md` | `Strategy:` | |
| `decision-doc` | `decision-doc.md` | `Decision:` | |
| `prioritize` | none (answers in the thread, a table) | no draft | |
| `pre-mortem` | none (answers in the thread) | no draft | |
| `metrics-tree` | `metrics-tree.md` | `Metrics:` | |
| `experiment-design` | `experiment-plan.md` | `Experiment:` | `stats.sample_size` |
| `experiment-readout` | `experiment-readout.md` | `Readout:` | `stats.compare` |
| `prd-draft` (upgraded) | `prd.md` (upgraded) | `PRD:` | |
| `review` | none (answers in the thread) | no draft | sub-agents |
| `call-synth` | `call-synthesis.md` | `Call synthesis:` | |
| `competitive-brief` | `competitive-brief.md` | `Competitive brief:` | |
| `opportunity-sizing` | `opportunity-sizing.md` | `Sizing:` | |
| `ai-feature-spec` | `ai-feature-spec.md` | `AI spec:` | |
| `eval-plan` | `eval-plan.md` | `Eval plan:` | |

- **The base tool set** of a doc skill: `context.search context.read jira.search jira.read
  confluence.search confluence.read confluence.create_draft docs.search docs.read`. Skills
  that answer in the thread drop `confluence.create_draft`.
- **Front matter** as today: `data-class: "restricted"`, `surfaces: "slack web cli"`,
  `ships-in: "pilot"`, `model: "reasoning"`, `owner: "TODO"`, `version: "0.1.0"`.
- **Drafts** go to `confluence.create_draft`, in the space the PM named or their default.

**Required `##` headings of each template** (the benchmarks check these exact strings):

| Template | Headings, in order |
| --- | --- |
| `problem-statement.md` | Who has the problem, The problem, Evidence, What they do today, Why now, What we don't know |
| `strategy-doc.md` | Recommendation, Diagnosis, Guiding choice, What we will do, What we won't do, Alternatives considered, Risks, How we'll know |
| `decision-doc.md` | Recommendation, Context, Options, Criteria, Tradeoffs, Reversibility, Open questions |
| `metrics-tree.md` | North star, Input metrics, Guardrail metrics, How each is measured, What we won't optimise |
| `experiment-plan.md` | Hypothesis, Primary metric, Guardrail metrics, Sample size and duration, Decision rule, Risks |
| `experiment-readout.md` | Decision, Result, Guardrails, What we learned, Next steps |
| `prd.md` | Summary, Problem and evidence, Goals and non-goals, Users, Alternatives considered, Solution overview, Requirements, Key user journeys, Non-functional requirements, Success metrics, Launch criteria, Rollout plan, Risks and mitigations, Open questions, Sources |
| `call-synthesis.md` | Headline, Themes, Quotes, Who said what, What to do, Gaps |
| `competitive-brief.md` | Bottom line, Where they win, Where we win, Evidence, What to do, Unknowns |
| `opportunity-sizing.md` | Estimate, Method, Assumptions, Sensitivity, What would change it |
| `ai-feature-spec.md` | Summary, Users and jobs, Intended behaviour, Examples, Failure modes, Misuse and safety, Evaluation, Cost and latency, Human oversight, Rollout and monitoring, Open questions |
| `eval-plan.md` | What we are testing, Datasets, Graders, Thresholds, Slices, Regression set, Who reviews failures |

**Reviewers** (`content/reviewers/<name>.md`), four new: `senior-pm`, `data-scientist`,
`design-ux`, `ai-safety`. Same front matter and output format as the existing seven.

**Exemplars** (`content/context/exemplars/<name>.md`, context front matter, `sensitivity:
internal`, `last-reviewed: 2026-11-14`): `prd-audit-log-export`, `strategy-vault-smb`,
`decision-sso-build-or-partner`, `readout-onboarding-checklist`, `ai-spec-policy-assistant`.
Each follows its template's headings exactly, is about fictional Acme Vault, and opens with
"Example: replace with your team's best."

**The model role:** `reasoning`, addressed as `pmos/reasoning`.

**The stats tools:** `stats.sample_size` and `stats.compare` (system `stats`, kind `read`).

## 2. The bar (`content/playbooks/bar.md`)

Every document a skill drafts must pass these before the PM sees it:
1. **Problem first.** Who has the problem, and the evidence, come before the solution.
2. **Alternatives.** At least two real options, and why the others lost.
3. **A recommendation, stated first.** One, in the opening lines.
4. **A measurable outcome.** A metric with a number, a date and how it's measured, plus a
   guardrail metric that must not get worse.
5. **Non-goals and tradeoffs,** stated.
6. **The riskiest assumption,** and the cheapest way to test it.
7. **Nothing unverified left unflagged.**

`content/master/AGENTS.md` gains a short "The bar" section pointing at
`/content/playbooks/bar.md` with the seven checks in one line each. The `pm-assistant` role
body adds: have a point of view; when a request gives a solution and no problem, ask about
the user and the problem before drafting; say what you would cut; disagree with evidence, then
follow the PM's call.

## 3. The skill pattern

Each doc skill's steps:
1. **Read** its playbook (`/content/playbooks/<x>.md`) and template
   (`/content/templates/<x>.md`), and find an exemplar (`context.search`, domain `exemplars`).
2. **Gather** evidence with the read tools, and keep the list of sources.
3. **Ask** up to three questions whose answers would change the output. If the request has a
   solution but no user or problem, that is the first question. Wait, unless the PM said
   "draft anyway".
4. **Draft** from the template, headings unchanged and in order.
5. **Check** the draft against `/content/playbooks/bar.md`; fix what fails, once. What can't
   be fixed from the sources goes under the open questions or unknowns heading.
6. **Propose** the Confluence draft, then say in the reply: the recommendation in one line,
   what is waiting for approval, and the top open question.

Skills that answer in the thread do steps 1 to 3, then answer in the playbook's shape.

## 4. The review panel

- **Sub-agents.** When a role's skills include `review`, the harness registers one sub-agent
  per file in `content/reviewers/`: `name` is the file stem, `description` is the front
  matter `lens`, the prompt is the rubric body, and the tools are the session's **read** tools
  only (kind `read` in `tools.yaml`). No sub-agent can write, draft or approve.
- **The `review` skill:**
  1. reads the document (a Confluence page id, or text in the thread);
  2. picks up to 4 reviewers that fit it, always including `senior-pm`;
  3. runs each with the `task` tool, giving it the document text;
  4. merges the findings: deduplicated, grouped blocker, major, minor, each with where and the
     fix, naming the reviewer;
  5. ends with "The three changes that matter most".
- **Limit:** 4 reviewers a run.

## 5. Code

**Harness** (`harness/src/agent.ts`, `worker.ts`):
- Mount, read-only like the skills: `/content/templates/`, `/content/reviewers/`,
  `/content/playbooks/` (each only if the directory exists).
- `MODEL_ALIASES` gains `reasoning: "pmos/reasoning"`.
- `reviewerSubagents(contentRoot, readTools)` builds the sub-agents; `createPmosAgent` passes
  them to `createDeepAgent` when `options.skills` includes `review`.

**Contract** (`crates/contract`):
- `ModelRole::Reasoning` (`"reasoning"`).
- Lint: `content/playbooks/*.md` need `owner` and `version` (rules `playbook-parse`,
  `playbook-body-empty`); the directory is optional.

**Gateway:** a `reasoning` role in `config/gateway.yaml`, `deploy/gateway.yaml` and
`config/gateway.claude-test.yaml`, pointing at the same upstream and model as `default`, with
a comment that the bake-off may point it at a larger model. The stub-model mode answers for it.

**Tool server, the stats tools** (deterministic, no connector, audited like any read):

`stats.sample_size` arguments:
- `metric`: `"proportion"` or `"mean"`;
- `baseline`: the baseline rate (0–1, exclusive) for a proportion, or the mean;
- `std_dev`: required for `mean`, > 0;
- `mde`: the minimum detectable effect, > 0;
- `mde_kind`: `"relative"` (default; 0.05 means 5% of the baseline) or `"absolute"`;
- `alpha`: default 0.05; `power`: default 0.8; `two_sided`: default true;
- `variants`: total arms including control, default 2, at most 10;
- `daily_users`: optional; the users entering the experiment per day, all arms together.

It returns `{per_arm, total, days (when daily_users is given), assumptions: {...}, note}`.
Proportion uses the two-proportion z-test formula with unpooled variance,
`n = (z_{α/2} + z_β)² · (p1(1−p1) + p2(1−p2)) / (p2 − p1)²`, rounded up; mean uses
`n = 2 · (z_{α/2} + z_β)² · σ² / δ²`. `days` is `ceil(total / daily_users)`, and the note says
to run whole weeks.

`stats.compare` arguments: `control: {users, conversions}`, `treatment: {users, conversions}`,
`alpha` (default 0.05). It returns `{control_rate, treatment_rate, absolute_lift,
relative_lift, ci_low, ci_high (of the absolute lift), p_value, significant, note}`: a
two-proportion z-test, pooled for the p-value, unpooled for the interval. The note warns that
a result read before the planned sample size is not reliable.

Bad input (a rate outside 0–1, zero users, conversions above users, `mde` ≤ 0) is `InvalidArgs`.

## 6. Benchmarks (`evals/categories/pm-craft/`)

`bot: personal`, no `pass_bar`, "seed: replace with case cards", on the fake fixtures. About
16 tasks:
- **Structure**, one per doc skill: `drafts:` with `tool: confluence.create_draft`,
  `count: 1`, `must_include` the title prefix and three or more of the template's headings.
- **Product sense:** a request with a solution and no problem gets no draft and a question.
- **Decisions:** the decision doc includes "Recommendation" and "Options".
- **Numbers:** `experiment-design` for a 10% baseline, 10% relative lift, defaults; the draft
  includes the tool's per-arm number.
- **AI:** the spec includes "Failure modes" and "Evaluation".
- **Review:** a review of fixture page 9002 names findings by severity and creates no draft.

**Judges** (advisory, no labels yet): `recommendation-first`, `metric-with-guardrail`,
`names-riskiest-assumption`, each `applies_to: [pm-craft]`, with new taxonomy tags
`model/no-recommendation`, `model/unmeasurable-outcome`, `model/no-riskiest-assumption`.

## 7. Docs

- `docs/pm-craft.md`: what each skill is for with an example ask; the bar; the review panel;
  the stats tools; making the bar your own (edit `bar.md`, swap the exemplars).
- `docs/evals.md`, `docs/toolserver.md`, `docs/contract.md` (playbooks, the model role), the
  plan status, the changelog and the README's skill list.
