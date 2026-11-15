# PM craft: PM OS at a senior-PM bar

PM OS now works the way a senior product manager does. Before it drafts, it asks who has the
problem and what the evidence is. Every document it drafts weighs real alternatives, opens
with one recommendation, names a measurable outcome and its riskiest assumption, and has been
checked against a written bar. Sample sizes and test results come from tools, not from the
model's arithmetic. A panel of eleven reviewers reads any draft on request, and a new track
covers AI features. You still own every output: each page is a draft until you approve it.
The build spec is `docs/pm-craft-spec.md`.

**Why it is built from structure, not a clever prompt.** The models are local and mid-sized.
The quality comes from what they work from and are checked against:
- **a written bar** (`content/playbooks/bar.md`);
- **playbooks, templates and exemplars** for each kind of document;
- **a draft, check, fix pass** before you see anything;
- **a review panel;**
- **numbers from tools;**
- **benchmarks** that say whether the bar is met.

## The bar

Every document a skill drafts must pass seven checks before you see it. The full text, with
what passes and what fails, is `content/playbooks/bar.md`.

| # | Check | What it means |
| --- | --- | --- |
| 1 | **Problem first** | Who has the problem, and the evidence, come before the solution. |
| 2 | **Alternatives** | At least two real options, and why the others lost. |
| 3 | **A recommendation, stated first** | One, in the opening lines. |
| 4 | **A measurable outcome** | A metric with a number, a date and how it's measured, plus a guardrail metric that must not get worse. |
| 5 | **Non-goals and tradeoffs** | Stated, not implied. |
| 6 | **The riskiest assumption** | And the cheapest way to test it. |
| 7 | **Nothing unverified left unflagged** | Every claim has a source or is marked `[unverified]`. |

- **A recommendation is an opinion,** and the draft labels it as one. Facts still follow cite
  or flag.
- **What the sources can't settle isn't invented.** It goes under the document's open
  questions (or unknowns) heading, and the bot tells you which checks it couldn't pass.
- **Your bot has a point of view.** It says what it would cut. When it disagrees with you, it
  gives the evidence, then follows your call.

## The skills

Ask your personal bot (`pm-<you>`) in a Slack DM, in plain words. The examples use the
fictional Acme Vault.

| Track | Skill | What it's for | Example ask | What you get |
| --- | --- | --- | --- | --- |
| Product sense | `product-critique` | A frank read of an idea, a design or a shipped feature | "Critique the key export flow in the Acme Vault console." | An answer in the thread |
| Product sense | `problem-framing` | Pin down who has the problem and the evidence, before any solution | "Frame the problem behind the SSO asks in the October Acme Cloud call notes." | A draft page, `Problem: …` |
| Strategy and decisions | `strategy-doc` | A diagnosis, one guiding choice, what you will and won't do | "Write a strategy for enterprise identity in Acme Vault." | A draft page, `Strategy: …` |
| Strategy and decisions | `decision-doc` | One decision: the options, the criteria, a recommendation | "Should SCIM ship with SSO or the release after? Write the decision doc." | A draft page, `Decision: …` |
| Strategy and decisions | `prioritize` | Rank a list against stated criteria | "Prioritize PM-101, PM-102 and audit log export for next quarter." | A table in the thread |
| Strategy and decisions | `pre-mortem` | Assume the plan failed, and list why | "Run a pre-mortem on the Key export v2 PRD." | An answer in the thread |
| Metrics and experiments | `metrics-tree` | A north star, its input metrics and its guardrails | "Build a metrics tree for SAML SSO in the console." | A draft page, `Metrics: …` |
| Metrics and experiments | `experiment-design` | A test plan with a sample size from `stats.sample_size` | "Design an experiment for the guided SSO setup. Baseline is 10%; detect a 10% relative lift." | A draft page, `Experiment: …` |
| Metrics and experiments | `experiment-readout` | Read a finished test with `stats.compare`: ship, iterate or stop | "Read out the SSO setup test: control 1,000 of 10,000, treatment 1,100 of 10,000." | A draft page, `Readout: …` |
| Documents and review | `prd-draft` | A PRD that starts from the problem and the alternatives | "Draft a PRD for SCIM user provisioning, from PM-102." | A draft page, `PRD: …` |
| Documents and review | `review` | The review panel reads a page or pasted text | "Review the Key export v2 PRD." | Findings in the thread |
| Customers and market | `call-synth` | Themes, quotes and who said what, from call notes | "Synthesize the October Acme Cloud call notes." | A draft page, `Call synthesis: …` |
| Customers and market | `competitive-brief` | Where a competitor wins, where you win, and what to do | "Write a competitive brief on Initech for SSO." | A draft page, `Competitive brief: …` |
| Customers and market | `opportunity-sizing` | An estimate with its method, assumptions and sensitivity | "Size the opportunity for audit log export." | A draft page, `Sizing: …` |
| AI products | `ai-feature-spec` | A spec for an AI feature: behaviour, failure modes, evaluation, oversight | "Spec an assistant that answers SSO setup questions from the docs." | A draft page, `AI spec: …` |
| AI products | `eval-plan` | How an AI feature will be tested before and after launch | "Write an eval plan for the SSO setup assistant." | A draft page, `Eval plan: …` |

### How a doc skill works

The twelve skills that draft a page all take the same six steps.

1. **Read.** The skill reads its playbook and its template, and finds an exemplar: a finished
   example of that kind of document, from `content/context/exemplars/`.
2. **Gather.** It collects evidence from the team's context, Jira, Confluence and the public
   docs, and keeps the list of sources.
3. **Ask.** It asks up to three questions, only ones whose answers would change the document.
   If you gave it a solution and no user or problem, that is the first question. It waits for
   you, unless you say "draft anyway".
4. **Draft.** It writes from the template, with the headings unchanged and in order.
5. **Check.** It checks the draft against the bar and fixes what fails, once. What the sources
   can't fix goes under the open questions.
6. **Propose.** It proposes a Confluence draft, in the space you named or your default. Its
   reply says three things: the recommendation in one line, what is waiting for your approval,
   and the top open question. Nothing is created until you approve the card.

The four skills that answer in the thread (`product-critique`, `prioritize`, `pre-mortem`,
`review`) do steps 1 to 3, then answer. They can't create a page.

**An example.** Ask "Write a PRD for adding a dark mode to the Acme Vault console" and you
get a question back, not a PRD: who is asking for it, and what problem it solves for them.
Answer it, or say "draft anyway", and the draft follows.

## The review panel

Ask for a review of a Confluence page, or paste the text. `review` picks the reviewers that
fit the document, runs them at once, and merges what they find.

| Reviewer | Reads for | |
| --- | --- | --- |
| `senior-pm` | The bar: the problem, the alternatives, the recommendation, the outcome | **New.** On every review |
| `data-scientist` | Metrics, experiment design and what the numbers can support | **New** |
| `design-ux` | The user's journey, and where they would get lost | **New** |
| `ai-safety` | How an AI feature fails or is misused, and who catches it | **New** |
| `engineering-lead` | Feasibility, scope and dependencies | |
| `security-architect` | The threat model, and security claims and defaults | |
| `legal-compliance` | Claims, licensing, export control and NDA content | |
| `executive` | Strategy fit, the business case and priority | |
| `pmm` | Positioning, naming and competitive framing | |
| `field-se` | Whether it can be deployed at a customer's site | |
| `regulated-industry-buyer` | What a buyer in healthcare, finance or government needs | |

- **Eleven reviewers, up to four a run.** The skill picks the ones that fit the document, and
  `senior-pm` is always one of them.
- **Read-only.** A reviewer has the read tools only. It can't draft, write or approve
  anything, and neither can the review.
- **What you get back:** the findings, deduplicated and grouped blocker, major, minor. Each
  says where it is, the fix, and which reviewer raised it. The review ends with "The three
  changes that matter most".
- **Each reviewer's rubric** is a file in `content/reviewers/`, with the lens it reads through
  and what it flags.

## The stats tools

A model is poor at arithmetic and good at sounding sure. So the two numbers an experiment
turns on come from tools: deterministic, the same every time, and logged like any other read.
The arguments are in `docs/toolserver.md`.

| Tool | Used by | What it computes |
| --- | --- | --- |
| `stats.sample_size` | `experiment-design` | The users each arm needs to detect an effect of a given size, the total, and, given daily traffic, the days it takes |
| `stats.compare` | `experiment-readout` | Both arms' rates, the absolute and relative lift, a confidence interval for the lift, the p-value, and whether the result is significant |

**Example: how big a test.** Your baseline conversion is 10%, and a 10% relative lift (to 11%)
is the smallest that would change your decision. With the defaults (a 5% significance level,
80% power, two-sided, two arms) and 3,000 users a day entering the test:

| | |
| --- | --- |
| Per arm | 14,749 users |
| Total | 29,498 users |
| Days | 10; the tool's note says to run whole weeks, so plan for two |

**Example: reading a result.** Control converted 1,000 of 10,000 users; treatment converted
1,100 of 10,000:

| | |
| --- | --- |
| Control rate, treatment rate | 10.0%, 11.0% |
| Absolute lift | 1.0 percentage point |
| Relative lift | 10% |
| 95% interval for the absolute lift | 0.15 to 1.85 percentage points |
| p-value | 0.021 |
| Significant at 5% | Yes |

- **The tool states its assumptions** with each answer: the rates, the significance level,
  the power and the number of arms it used.
- **Don't peek.** A result read before the planned sample size isn't reliable.
  `stats.compare` says so in its note, and the readout says whether the planned sample size
  was reached.
- **Proportions and means.** `stats.sample_size` handles a rate (such as conversion) or a
  mean (such as minutes to finish setup; you give the standard deviation). `stats.compare`
  compares two rates.

## The AI-product track

An AI feature fails differently from other software: it is right most of the time, and wrong
in ways nobody listed. The track makes those the first things a spec covers.

- **`ai-feature-spec`** drafts the spec. Beyond a PRD's sections it has the intended
  behaviour with examples, failure modes, misuse and safety, evaluation, cost and latency,
  human oversight, and rollout and monitoring.
- **`eval-plan`** drafts how the feature is tested: what is being tested, the datasets, the
  graders, the thresholds, the slices, the regression set, and who reviews failures.
- **The `ai-safety` reviewer** reads either one on request.
- **The playbook** is `content/playbooks/ai-products.md`, and the exemplar is a spec for a
  fictional policy assistant.

Write the spec first, then the eval plan from it. The spec's failure modes become the eval
plan's datasets and slices.

## Make the bar your own

PM OS ships a general bar and fictional exemplars. Your team's own standards and best
documents will do better. Everything below is a markdown file in `content/`, changed by pull
request; `pmos lint` checks it, and the file's owner reviews it.

| To change | Edit | Notes |
| --- | --- | --- |
| What every draft is checked against | `content/playbooks/bar.md` | Keep each check one that a reader can answer yes or no. |
| How a kind of work is done | `content/playbooks/<name>.md`: `product-sense`, `strategy`, `metrics-and-experiments`, `customer-insight`, `ai-products` | |
| What good looks like | `content/context/exemplars/` | Each shipped exemplar opens with "Example: replace with your team's best." Replace it with your best real PRD, strategy, decision, readout and AI spec, following the template's headings. |
| A document's sections | `content/templates/<name>.md` | The `pm-craft` benchmarks check the headings by name. Change a heading and its task together (`evals/categories/pm-craft/`). |
| Who reviews | Add a file to `content/reviewers/` | Front matter `name`, `lens` and `flags`, then the rubric (`docs/contract.md`). The panel picks it up; no code changes. |

- **Exemplars are context files,** so each needs an owner, a review date and a sensitivity
  label. A real document that names customers is `restricted`.
- **After a change, run the benchmarks.** A pull request that changes a skill runs the
  `pm-craft` gate by itself. One that changes only a playbook, a template, an exemplar or a
  reviewer doesn't, so run the category by hand and compare:
  ```sh
  deploy/pmos-admin evals-run --category pm-craft --note "bar: <what changed>"
  ```

## What the benchmarks do and don't prove

The `pm-craft` category (`evals/categories/pm-craft/`, described in `docs/evals.md`) has 17
seed tasks on the fake fixtures.

| They check, deterministically | They don't check |
| --- | --- |
| Each doc skill ends with one Confluence draft, in the right space, titled with its prefix, with the template's headings | Whether the problem is the right one, or the recommendation a good one |
| A request with a solution and no problem gets a question, and no draft | Whether the question is the one a senior PM would ask |
| The experiment plan carries the per-arm number `stats.sample_size` returns | Whether the experiment is worth running |
| A review names findings by severity and drafts nothing | Whether the findings are the ones that matter |

- **Structure now, judgment later.** The checks above are structure. A draft can pass them
  all and still be weak.
- **Three judges read for judgment:** `recommendation-first`, `metric-with-guardrail` and
  `names-riskiest-assumption`. Each asks one yes-or-no question of every draft. They are
  **advisory**: reported, never failing a run, until each is calibrated against at least 100
  answers labelled by people and agrees with them at least 80% of the time each way
  (`docs/runbooks/quality.md`).
- **Seed tasks, no pass bar.** The category reports only. Replace the tasks with real cases
  (`evals/case-card.md`), then set a bar.
- **Whether a local model meets the bar is measured, not assumed.** Tests in this repo run
  against a stub model, which shows the plumbing works and nothing about quality. The answer
  comes from running `pm-craft` on the real stack, with the real models.
- **If the model falls short,** every craft skill asks the gateway for the `reasoning` model
  role. A platform owner can point that role at a larger model in the gateway config
  (`docs/gateway.md`) without touching a skill, then run the benchmarks again.
- **Your own read still counts most.** Rejections and edits feed the same loop as every other
  draft (`docs/runbooks/quality.md`).
