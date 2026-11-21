# PM OS: standing requirements for Claude sessions

The owner's requirements for the PM OS build. Read this before planning or building. Stated by
the owner on 2026-11-16 and 2026-11-17 (three decisions of 2026-11-17 reached this session through
the reviewer session: the user database, the required assertion, 90-day purge); update it when they change something.

## Scope (the owner's list, a to f, i and j; g and h were dropped)

- **a. Authentication:** primarily Azure AD (Entra ID) single sign-on.
  Owner, 2026-11-20: add a second way to sign in without Entra: email and password kept by
  PM OS, with an authenticator-app code required as a second step. It sits beside Entra and is
  switched on by configuration (`docs/password-sign-in.md`).
- **b. Authorization:** role-based access with four roles: Admin, Product Manager, Reviewer,
  Auditor. A Reviewer recommends and the draft's owner confirms. An Auditor sees metadata only.
  The PM OS user database is the source of users and roles, not `team.yaml`; the design is
  written as a spec and reviewed before it is built (`docs/user-database-spec.md`).
  Built 2026-11-18 (slices U1 to U7): every service decides from the database and `team.yaml`
  is retired. Security gate passed on 2026-11-21 at `04c175f` (no open High or Critical for the
  user database and password sign-in; Medium findings G1 to G5 are being fixed).
- **c. Policy Admin** for NVIDIA OpenShell. The app opens a pull request; the merge happens in
  GitHub.
  Owner, 2026-11-18, plan only (build nothing until the plan is approved): add OpenShell's
  policy prover to Policy Admin, and bring OpenShell's sandbox allow and deny decisions into
  the audit log.
- **d. Front end:** a web app PMs love. The owner holds the design to the standard of the
  Baseten and WorkOS dashboards and will keep challenging it. Design approved for the MVP on
  2026-11-16: `web/` as rebuilt in `claude/web-redesign-v3`.
- **e. Harness management and context-layer management.**
- **f. Audit logging.** Retention: purge at 90 days, no archive. One controlled job deletes
  older events and records its own run; nothing else can delete, and it cannot delete newer
  events.
- **g. Dropped by the owner on 2026-11-18:** connectors. The owner wants MCP servers used
  instead of the connectors. The connector code is to be removed and an MCP feature added in
  its place (owner, 2026-11-18). Plan only for now: do not remove or build anything until the
  owner approves the plan.
- **i. Model providers:** the application must be able to switch between different model
  providers, and behave like OpenRouter: one interface in front of many providers and
  models, chosen by configuration. If a model is not available, the system switches
  to another automatically. Added by the owner on 2026-11-18 as a requirement only: not
  planned or built yet; do not start it until the owner says so.
  Decided by the owner on 2026-11-18: keep this requirement. Switching and automatic fallback
  are built in PM OS's own gateway service, which already sits between the bots and the
  model; OpenShell holds one credential per provider and enforces which endpoints are
  reachable. (OpenShell's docs, v0.1.9 "Inference", say its managed inference route was
  removed and the workload chooses the model, so it gives no switching or fallback.)
- **j. Bot marketplace** (owner, 2026-11-20): widen the product beyond one PM assistant. Three new bots: a Chief of Staff for a
  product manager, a Growth product manager and an Outbound product manager. As part of
  onboarding the user sees a bot marketplace and chooses which bots to include. Each bot has
  its own mascot.
  Go-ahead given by the owner on 2026-11-20 ("integrate the bot marketplace as onboarding"),
  with the conductor's recommended defaults since the owner named none: each PM gets their own
  use of each bot they pick; today's assistant (Nudge) stays as the always-on generalist; PMs
  choose freely from the catalog an Admin offers; the mascots are Chief (Chief of Staff),
  Sprout (Growth PM) and Echo (Outbound PM), in Nudge's style, which the owner chose to keep.
  Spec: `docs/bot-marketplace-spec.md`.
  Owner's decisions of 2026-11-21, after the reviewer's review of the spec: (1) the three new
  bots are offered as **Preview** ("not yet benchmarked") until their eval run passes: an
  accepted exception to decision D10; (2) Echo's customer-facing drafts carry the line "Draft
  for internal review. Not approved for external use." and need no extra approval step, until
  Legal's outbound policy exists; (3) decision D12 is changed: the Chief of Staff, Growth and
  Outbound bots are built now (none of them directs other bots).
  Built 2026-11-21 (slices C1, B1, R1, W1, M1, F2, F3, S1, A1, R2; status notes in the spec's
  §13): the catalog and lint, the tool server's teams, the runtime and harness, the web API and
  screens, Slack, the Admin's view, and Chief writing the daily brief. Every bot but Nudge is a
  Preview until its content file claims a benchmark that lint verifies against an eval report.
  The security gate did not pass at `8e8f8fe` (one High: the web app dropped the Preview label);
  the fixes are at `c00e682` and the reviewer's re-check is pending. Real run on the test stack
  at `c00e682` with fake data: pick a team in onboarding, ask Chief and Echo, attributed drafts,
  Echo's draft line, approving after a bot left the team, the Admin's view. Not verified: a real
  Slack workspace, a held draft and the brief switch on the stack, and any eval run.
- **h. Dropped by the owner on 2026-11-17:** the review of how the Grok bot was built. Do not
  build it.

Earlier priorities that still hold: fix what the live test found (retry on quota errors, search
before asking, keep thread context, a valid eval run); simple onboarding with gentle, funny
nudges; a Slack pilot with real connectors; a daily brief from Jira, Confluence and Slack with
follow-up tracking.

## How to work

- **One conductor session owns the build.** It plans, then builds each phase in sequence, and
  within a phase runs sub-agents in parallel, choosing the model by complexity (the strongest
  model for security, design and cross-crate work; a lighter one for contained front-end or
  documentation work).
- **One slice, one agent, one git worktree, one branch.** The agent commits and never pushes.
  The conductor merges with `--no-ff`, re-runs the whole workspace's checks, then pushes.
- **Never merge a pull request.** That stays with the owner.
- **Never skip or disable a test, a lint rule or an accessibility rule to get green.**
- **Checks:** `cargo fmt --check --all`; `cargo clippy --workspace --all-targets --locked -- -D
  warnings -D clippy::result_large_err` (CI's clippy is newer than a local one may be);
  `cargo test --workspace` with `PMOS_TEST_DATABASE_URL` set; in `web/`: typecheck, lint, test,
  build and the browser tests; in `harness/`: typecheck, test (it has no lint script).
- **Secrets:** never print, log or send a token. The test stack uses fake data only, and Slack
  message text never goes into a report.
- **The signed user assertion is required whenever the web app or console is deployed.** The
  runtime and tool server refuse to start otherwise, and the mode is saved in `deploy/.env` so
  no later command can leave it off.
- **An independent reviewer session is the security gate.** Send it the commit for each batch;
  the gate passes only with no open finding of High or Critical severity.
- **Report honestly:** say what was verified and what was not. Nothing counts as measured
  against a model until an eval run has happened.

## Waiting on the owner

GitHub Actions starting jobs again on the account (every CI job on pull request #10 ends within
seconds without running); an Entra app registration, with `acct` as an optional claim; a model
key with enough quota for an eval run; pilot PM names; edits to the nudge wording; a GitHub App
with pull-request permission and branch protection on the content branch; approval of the three
plans (MCP servers and which ones for the pilot, the OpenShell prover, sandbox decisions in the
audit log) and the go-ahead for requirement i; the CODEOWNERS lines and branch-protection settings in
`policies/README.md`, so a change to a bot's instructions needs a second reviewer.
