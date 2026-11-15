# Changelog

One "PM OS can now ___" line per capability a PM would care about (the launch-post test,
`docs/bot-decisions.md` D9). Plumbing that a PM never sees doesn't get a line.

## Unreleased (before Gate 1; nothing is live yet)
- PM OS now works at your level: Senior, Staff or Principal ([how to use it](docs/levels.md)).
  - PM OS can now work at your level: DM `level staff` (or `senior`, or `principal`) and it changes the scope, horizon and audience of what it writes, adds a Cross-team effects section at Staff and a What must be true section at Principal, and brings in the executive, CFO, CTO or CEO reviewers.
  - PM OS can now do 12 new kinds of work, in two tracks. **Leading across teams:** `portfolio-review`, `roadmap-tradeoffs`, `okr-draft`, `business-review`, `alignment-plan`, `coach-feedback`, `bets-register`. **Company bets:** `vision-doc`, `investment-case`, `pricing-proposal`, `platform-strategy`, `exec-brief`.
  - PM OS can now count work across teams: a rollup of Jira issues by project, epic, status, assignee or fix version, with overdue and unassigned counts and the links that block one team on another. It reads with your own access and returns counts and links, not issue text.
  - PM OS can now get business-case numbers right: payback and net present value come from a cash-flow tool, not from the model's arithmetic.
  - PM OS can now keep a register of your bets for months: what each bet is, the metric, the riskiest assumption, the evidence and the decisions. Every change is an approval, and `bets review on` starts a monthly review that proposes updates for the bets that are due.
  - PM OS can now be measured at these levels: the `staff-craft` and `principal-craft` benchmark categories have 8 seed tasks each and two advisory judges (says what stops, business effect as a range).
- Tag PM OS in a thread, and a Home for your drafts, skills and accounts.
  - PM OS can now be tagged in any channel thread: write `@PM OS` and what you want, and a draft reply arrives in your DM to post as you, one per tag ([how](docs/slack-reply.md)).
  - PM OS can now show what's waiting for you in its Slack Home: tick drafts and approve or reject them together, browse the skills by track, and connect or disconnect your accounts ([how](docs/slack-home.md)).
- PM OS now works like a senior PM: it questions the problem, recommends, and checks its own work ([how to use it](docs/pm-craft.md)).
  - PM OS can now be held to a written bar: it asks who has the problem before it drafts a solution, and every document weighs real alternatives, opens with one recommendation, names a measurable outcome with a guardrail and its riskiest assumption, and is checked against `content/playbooks/bar.md` before you see it. Six playbooks say how each kind of work is done, and you can edit them and the example documents to match your team.
  - PM OS can now do 15 new kinds of work, by track. **Product sense:** `product-critique`, `problem-framing`. **Strategy and decisions:** `strategy-doc`, `decision-doc`, `prioritize`, `pre-mortem`. **Metrics and experiments:** `metrics-tree`, `experiment-design`, `experiment-readout`. **Customers and market:** `call-synth`, `competitive-brief`, `opportunity-sizing`. **Review:** `review`. **AI products:** `ai-feature-spec`, `eval-plan`. The PRD skill is upgraded to the same bar. Say "draft anyway" to skip the questions.
  - PM OS can now do the remaining launch work, in seven more skills. **Delivery and launch:** `release-notes` (customer-facing, checked against the docs), `status-update` (bottom line first), `launch-checklist` (a go/no-go with owners, dates and a rollback), `jira-draft` (up to eight issues a run, skipping what Jira already has, one approval each). **Customers and market:** `poc-plan` (success criteria agreed up front), `partner-brief`. **Trust:** `advisory-draft`, which only drafts: security and legal approve before anything is sent, and it never invents a CVE id, severity, version or fix date.
  - PM OS can now review a page with a panel of up to four of eleven reviewers, four of them new (senior PM, data scientist, design and UX, AI safety). They only read, and you get the findings by severity and the three changes that matter most.
  - PM OS can now get its experiment numbers right: sample sizes and test results come from two statistics tools (`stats.sample_size`, `stats.compare`), not from the model's arithmetic.
  - PM OS can now spec an AI feature and plan its evaluation: intended behaviour, failure modes, misuse, human oversight, and the tests that gate launch.
  - PM OS can now be measured against the bar: the `pm-craft` benchmark category has 23 seed tasks and three advisory judges (recommendation first, metric with guardrail, riskiest assumption).
- PM OS can now be set up by any company's product-management team: the team charter, product files and glossary are templates to fill in, and you point the Docs bot at your own docs site with `PMOS_DOCS_BASE_URL` (plus an optional products file for `/docs product:<id>`).
- PM OS can now draft replies to questions people ask you in Slack: type `reply assistant on`, and approve, edit, snooze or reject each draft before it posts as you.
- PM OS can now learn from your edits and rejections: a rejected draft asks why in one tap, each bot's owner gets a daily list of what went wrong, and changes to a bot's skills must pass its benchmarks before they merge.
- PM OS can now be installed on the Spark with one setup script and run day to day with `pmos-admin`: status, backups and restore, adding bots, and runbooks written for PM platform owners.
- PM OS can now be tried end to end in a test Slack workspace before the Spark is ready, with a Claude model standing in for the local models (testing only).
- PM OS can now onboard you itself: DM it, tap **Request access**, and once a platform owner approves you get your own `pm-<you>` bot and a `setup` checklist to connect your accounts.
- PM OS can now keep your bot's notes in your own private repo in your GitHub org, so you can read and edit what it remembers.
- PM OS can now be talked to as named bots: DM the Docs bot (`docs: …`) or your own `pm-<you>` bot in Slack, and watch its progress in one message.
- PM OS can now draft a Jira issue, Confluence page or Slack message and show you a card to Approve, Edit or Reject before anything is sent.
- PM OS can now run a routine you describe in plain words ("every weekday at 8am, summarize #launch"), and you can pause or delete it from Slack.
- PM OS can now show every bot, routine, pending approval and task trace on one small console.
- PM OS can now search your company's public docs from Slack: `/docs rotate a key`, visible only to you.
- PM OS can now draft a Confluence page or Jira issue and wait for your approval before anything is created.
- PM OS can now read Jira, Confluence and Slack as you, with your own permissions, once you link your accounts.
