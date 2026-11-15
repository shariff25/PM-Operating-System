# Levels: PM OS at your level

**Tell PM OS your level and it works at that altitude.** A Senior PM gets work on one product
area. A Staff PM gets work across teams. A Principal PM gets work on the company's bets. PM OS
also gets new skills for that work, a view across teams in Jira, cash-flow numbers for
business cases, and a register that tracks your bets for months. You still own every output:
each page, issue and register change is a draft until you approve it. The build spec is
`docs/staff-principal-spec.md`.

## The three levels

Your level is `senior` until you change it.

| | Senior | Staff | Principal |
| --- | --- | --- | --- |
| **Scope** | One product area | Several teams or a product line | The company and its market |
| **Horizon** | This quarter and next | 4 to 6 quarters | 2 to 3 years |
| **Audience** | The team and its stakeholders | Directors and peer leads | Executives and the board |
| **The recommendation is about** | What to build | Where to invest and what to stop | Which bets define the business |
| **Evidence expected** | Customers and usage | Across teams, with cost | Market, business model and competitors |
| **Extra sections** | None | Cross-team effects | Cross-team effects, What must be true |
| **Reviewers added by default** | None | `executive`, `engineering-lead` | `cfo`, `cto`, `ceo` |

**What the extra sections are.** At Staff and Principal, a document from `strategy-doc`,
`decision-doc` or `prd-draft` ends with:
- **Cross-team effects:** which teams pay, what they stop, and who must agree.
- **What must be true** (Principal only): the two or three conditions the bet depends on over
  2 to 3 years, and the signpost that would show each one is failing.

**Two more checks on top of the bar** (`content/playbooks/levels.md`; the bar itself is in
`docs/pm-craft.md`):
- **Staff:** the document says what stops or isn't funded, and names the teams affected.
- **Principal:** the business effect (revenue, cost or margin) is a range with its
  assumptions, and the document says what is hard to reverse at company scale.

## Set your level

**DM PM OS, alone in the message:**

| You type | What you get |
| --- | --- |
| `level` | "You're set to the **Staff** level. Change it with `level senior`, `level staff` or `level principal`." |
| `level senior` | "Done. I'll work at the Senior level: one product area, this quarter and next, written for the team and its stakeholders." |
| `level staff` | "Done. I'll work at the Staff level: several teams or a product line, 4 to 6 quarters ahead, written for directors and peer leads." |
| `level principal` | "Done. I'll work at the Principal level: the company and its market, 2 to 3 years ahead, written for executives and the board." |

- **The setting is yours.** Only you change it, and it applies to your personal bot.
- **Anything else after `level`** lists the three levels. If PM OS can't save the setting, it
  says so and changes nothing.
- **A request can set its own audience.** "Write this for the exec team" changes the
  audience for that one request. Your level stays as it was.

## The new skills

Ask your personal bot (`pm-<you>`) in a Slack DM, in plain words. The examples use the
fictional Acme Vault (the product) and Acme Cloud (a customer segment). Each skill follows the
same six steps as the others (`docs/pm-craft.md`, "How a doc skill works") and proposes a
Confluence draft from its template in `content/templates/`.

### Leading across teams

| Skill | What it's for | Example ask | What you get |
| --- | --- | --- | --- |
| `portfolio-review` | Where things stand across teams, from the cross-team rollup: dependencies, conflicts, what's at risk. It recommends changes and decides nothing. | "Review the portfolio across the PM and CLD projects." | A draft page, `Portfolio review: …` |
| `roadmap-tradeoffs` | A choice between investments. It always says what stops, and what each team gives up. | "Audit export or SCIM next year? Write the roadmap trade-offs." | A draft page, `Roadmap trade-offs: …` |
| `okr-draft` | One objective and at most four key results, each a number with a baseline, a target, a date and a source. Outputs are not key results. | "Draft OKRs for enterprise identity next half." | A draft page, `OKRs: …` |
| `business-review` | Results against the plan, then why, then what changes. No activity lists. | "Write last quarter's business review for the console." | A draft page, `Business review: …` |
| `alignment-plan` | Who decides, who must agree, each person's likely objection and the answer, the order to talk to them, and the fallback. It names roles and never guesses at a named person's motives. | "Plan how I get SCIM ahead of audit export." | A draft page, `Alignment plan: …` |
| `coach-feedback` | Feedback for another PM, written to teach: the three changes that matter most, each as a question first and a suggestion second, and one thing done well worth repeating. | "Help me coach the author of the Key export v2 PRD." | An answer in the thread; no page, and nothing is sent to the author |
| `bets-register` | Open, update, list and review your bets (see below). | "Open a bet from this strategy doc." | An approval card for the register |

### Company bets

| Skill | What it's for | Example ask | What you get |
| --- | --- | --- | --- |
| `vision-doc` | A narrative of the product in 2 to 3 years, from the customer's side, with what must be true and the signposts. | "Write the vision for Acme Vault in 2029." | A draft page, `Vision: …` |
| `investment-case` | Low, base and high scenarios with stated assumptions. Payback and net present value come from `stats.cashflow`. Inputs with no source are marked `[unverified]`. | "Write the investment case for SAML SSO and SCIM." | A draft page, `Investment case: …` |
| `pricing-proposal` | The value metric, the packages, who pays more and who pays less, and how existing customers move. It never states a competitor's price without a source. | "Propose packaging for SSO and SCIM." | A draft page, `Pricing: …` |
| `platform-strategy` | Build, buy or partner, written with engineering: what becomes a platform and what stays a product. | "Build, buy or partner for identity?" | A draft page, `Platform strategy: …` |
| `exec-brief` | One page. The ask comes first, then what the reader needs in order to decide. | "Brief the exec team on funding SSO." | A draft page, `Exec brief: …` |

**Level extras come on their own.** A `strategy-doc`, `decision-doc` or `prd-draft` gets its
level's extra sections without you asking.

## The cross-team rollup

**`jira.rollup` counts issues across projects, so you don't count them yourself.** The
`portfolio-review`, `roadmap-tradeoffs` and `business-review` skills use it, and quote its
counts and links exactly.

- **What it counts.** The issues your JQL finds, grouped by project (the default), epic,
  status, assignee or fix version. For each group: the total, how many are to do, in
  progress and done, how many are overdue (a due date before today, and not done) and how
  many have no assignee.
- **Links between groups.** It lists the links that cross groups, or leave your query, such
  as "PM-102 blocks CLD-13", with the other issue's state. Each pair appears once.
- **It reads as you,** with your own Jira token. It sees what you see.
- **The caps.** At most 1,000 issues a call (500 unless the skill asks for more), at most 50
  groups (the rest are summed as `other`) and at most 200 links. It stops after 45 seconds.
  If it stops early, the result says it is truncated, and why (`cap` or `time`), and the
  document says so.
- **It returns counts and links, and no issue text.** No descriptions and no comments reach
  the model, so a rollup can't carry planted instructions.

**An example, on the fake fixtures** (the ones the benchmarks use). Ask for a portfolio
review across the `PM` and `CLD` projects, and the rollup finds six issues:

| Project | Issues | To do | In progress | Done | Overdue | Unassigned |
| --- | --- | --- | --- | --- | --- | --- |
| `PM` | 3 | 2 | 1 | 0 | 0 | 0 |
| `CLD` | 3 | 1 | 1 | 1 | 1 | 1 |

One link crosses them: `PM-102` blocks `CLD-13`, which is to do. The review's **Dependencies
and conflicts** section names that link, and **At risk** can say that `CLD-13` is overdue and
has no owner.

## Numbers for business cases

**The model never adds up cash.** `investment-case` and `pricing-proposal` call
`stats.cashflow`, which does the arithmetic for 1 to 5 scenarios of up to 10 years each, and
quote what it returns. It is arithmetic on the inputs you give, not a forecast.

**A worked example.** Upfront cost 100. Year 1: revenue 60, cost 10. Year 2: revenue 80, cost
10. Discount rate 10%.

| | Year 1 | Year 2 |
| --- | --- | --- |
| Net (revenue minus cost) | 50 | 70 |
| Cumulative (from minus 100) | -50 | 20 |

- **Payback: 1.7 years.** The cumulative figure reaches zero 50 of the 70 into year 2 (1 +
  50 / 70).
- **Net present value: 3.31.** Minus 100, plus 50 / 1.1, plus 70 / 1.21.

If you give no discount rate, the tool uses 10%. Say where each input comes from. PM OS has
no finance data, so an input it can't source is marked `[unverified]`.

## The bets register

**A bet is a claim you will check later.** The register keeps each one for months, with the
evidence and the decisions as they come in.

| A bet holds | What it is |
| --- | --- |
| `key`, `title` | A short name for it (the key is lowercase letters, digits and hyphens) |
| `bet` | One sentence |
| `metric` | The number that will show whether it was right |
| `riskiest_assumption`, `test` | What it depends on most, and the cheapest way to test that |
| `status` | `open`, `confirmed`, `refuted` or `dropped` |
| `opened`, `review_by` | When you opened it, and when you'll look again |
| `evidence`, `decisions` | Dated entries, each with a source when there is one |

- **Every change is an approval.** Opening a bet, adding evidence, changing the status and
  recording a decision each arrive as a card. Nothing is saved before you approve.
- **Where it lives.** Each bet is one note in your own memory, so it is in your private
  GitHub repo when your memory is on GitHub. PM OS writes the note's format itself; the model
  never rewrites the whole note.
- **It won't judge for you.** A bet is marked confirmed or refuted only when the metric or the
  test supports it, with the evidence cited in the same card.
- **When a note gets full** (4,000 characters), the oldest evidence is dropped, and the note
  says how many entries went.
- **Ask in plain words.** "Open a bet from this strategy doc." "List my bets." "Add this
  readout to the SSO bet."

**The monthly review.** DM PM OS:

| You type | What happens |
| --- | --- |
| `bets review on` | Starts a routine on your personal bot: 09:00 on the 1st of each month, in your time zone |
| `bets review` | Says whether it's on and when it next runs |
| `bets review off` | Stops it |

Each month the bot finds your open bets due for review in the next 7 days. For each, it
gathers what changed (the cross-team rollup, and readouts and reviews in Confluence) and
proposes one update card. It ends with a summary: confirmed, refuted, still open, and what you
need to decide. Nothing changes until you approve.

## Three new reviewers

The `review` skill adds your level's reviewers to its panel, and still runs at most four.
You can also ask for them by name.

| Reviewer | Reads for |
| --- | --- |
| `cfo` | Unit economics, cost, payback, and what the money would otherwise do |
| `cto` | Technical strategy, platform risk, and what scale breaks |
| `ceo` | Focus, what the company stops, and the story to customers and the board |

## What PM OS still can't do

- **It has no finance, usage or CRM data.** It reads Jira, Confluence, Slack and your public
  docs. Revenue, cost, adoption and pipeline numbers come from you, and are marked
  `[unverified]` until you give a source.
- **Judgment and influence stay with you.** PM OS can draft the alignment plan. It can't have
  the conversation, and it can't tell you the bet is right.
- **It decides nothing.** Every page and every register change waits for your approval.
- **Whether a local model is strong enough at this level is measured, not assumed.** The
  `staff-craft` and `principal-craft` benchmark categories check the structure of each
  document, the rollup and cash-flow numbers, and the register (`docs/evals.md`). Two
  advisory judges read for what stops and for the business effect as a range. The `reasoning`
  model role can be pointed at a larger model if the results fall short.
