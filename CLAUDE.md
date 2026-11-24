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
  user database and password sign-in; Medium findings G1 to G5 were fixed afterwards).
  Gate passed again on 2026-11-22 at `11a5768`, `2efb73f`, `2544d01` and `6bb806f`, each time
  from reading the code (no open High or Critical). The Medium findings of each round (N1 to N5,
  R1 to R3, F1 to F4, G1 to G3, S1 and S2) were fixed and pushed; the reviewer has not yet read
  the last two batches (the admin-script fixes at `0e1f4b8` and the gateway fixes at `c78b5fb`).
  Still open by the reviewer's own account: eval reports are unsigned, and the CODEOWNERS file
  and branch protection that protect them wait on the owner.
- **c. Policy Admin** for NVIDIA OpenShell. The app opens a pull request; the merge happens in
  GitHub.
  Owner, 2026-11-18, plan only (build nothing until the plan is approved): add OpenShell's
  policy prover to Policy Admin, and bring OpenShell's sandbox allow and deny decisions into
  the audit log.
  Owner, 2026-11-22: the bots must also be able to run in OpenShell sandboxes on hosts other
  than the Spark ("I want to deploy it in another platform apart from spark"), as a generic,
  host-independent implementation, not one tied to a single cloud. The design is written as a
  spec and reviewed before it is built (`docs/openshell-any-host-spec.md`).
  Owner, 2026-11-22, later the same day: "Deploy on spark scrap that idea I want to make it
  platform agnostic." The Spark is no longer a deployment target; PM OS runs on any suitable
  Linux host. The owner accepted the conductor's recommendations: real data may be processed by
  a hosted model (the owner's connected model, the same one used for the eval runs) and by a
  local model on a GPU host the owner controls; an internet-facing host takes Microsoft sign-in
  only, with password sign-in only from a VPN or an allowed range; admin routes go on a separate
  admin-only listener; each service name is pinned to its own address in the sandbox policy;
  the first host to verify on is an Ubuntu 24.04 x86 virtual machine without a GPU, with fake
  data. This replaces the earlier rule that restricted data is processed only on the Spark's
  local models; the spec is revised and reviewed before anything is built from it.
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
  the fixes are at `c00e682` and the reviewer confirmed the High closed on 2026-11-22. Real run on the test stack
  at `c00e682` with fake data: pick a team in onboarding, ask Chief and Echo, attributed drafts,
  Echo's draft line, approving after a bot left the team, the Admin's view. Not verified: a real
  Slack workspace, a held draft and the brief switch on the stack.
  First eval run on 2026-11-21 and 22 (E1, the owner's hosted model, fake data, 93 tasks
  three times each; reports in `evals/reports/`): 4 of 11 categories met their bar. It is not a
  valid benchmark: attempts shared one inbox, 28 attempts were lost to the provider's connection,
  and the judges have no human labels. Attempts are isolated since E2 (test stack only). Every
  new bot stays a Preview.
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

## Owner's decisions of 2026-11-23

- **Hide the vendor:** "Hide the vendor info in the code." No committed file names the hosted
  model's vendor, its model id or its host. The gateway reports a neutral role `label:` and a
  hash of the model id. Older commits on `claude/web-app` still carry the names; `main` never
  did. Whether a plain hash is enough, or the hash needs a key the owner holds, is open.
- **Eval:** run it the same day, then "stop the eval, we can pick it another time". Ten of
  eleven categories ran (E3, isolated attempts, fake data, test class): slack-reply, jira-draft
  and docs-qa met their bar. It is not a valid benchmark: 45 attempts were lost to a 300-second
  cutoff on non-streamed model calls from a hop beyond the gateway, pm-craft did not finish and
  the judges have no human labels. Before the next run: stream model calls or get that limit
  raised, and have the runner cancel a task it times out.
- **Deploy for end-to-end testing:** the deploy kit ran for the first time, in the conductor's
  cloud container, with fake data and password sign-in. The owner cannot reach that container
  and it does not survive a restart; how the owner tests hands-on (a host they can reach, Slack,
  or directing the conductor) is not yet decided.
- **Self-improving agents:** after the research in
  `docs/research/frontier-labs-self-improving-agents.md`, the owner approved building three
  measurements now (decision time in the feedback ledger, eval cases where the bot must say it
  is blocked, every eval category on a role or skill change) and writing the rest as
  requirements for phase 2 (`docs/improvement-loop-phase2-requirements.md`). The improver bot
  itself waits for the pilot's real labels; training stays last.

## Owner's decisions of 2026-11-24

- **"Build everything end-to-end based on PRD - run eval."** Taken as the go-ahead for
  requirement i and for the MCP, prover and sandbox-audit work. Each is still a spec first,
  read by the reviewer before code: `docs/model-providers-spec.md`,
  `docs/mcp-servers-spec.md`, `docs/openshell-prover-and-sandbox-audit-spec.md`.
- **MCP servers:** all three kinds are supported (the vendor's hosted server, a self-hosted
  open-source one, one PM OS builds), chosen per deployment; an Admin approves a server and a
  tool; removing each built-in connector needs the owner's explicit go-ahead. The side-by-side
  comparison of old and new runs on fake data only.

## Where the build stands (2026-11-23)

- Pushed and checked: `claude/web-app` (draft pull request #10).
- Security gate passed at `52dbae2` (no open High or Critical). Sent to the reviewer and not
  yet answered: the fixes for its findings at `52dbae2`, spec revision r7, and slices H1, H2,
  H3, H4 and H9.
- Any-host slices built and passing CI's deploy job on a real Docker host: H0, H0b, H1, H2, H3,
  H4, H9 (status notes at the top of `docs/openshell-any-host-spec.md`). Nothing has run on
  OpenShell. Blocked until the reviewer reads r7: H10 (admin listener) and H12 (hosted source).
  Not started: H7, H6. H13 is the owner's.
- Since H4, every service needs `PMOS_MODEL_SOURCE`, Slack is refused in fake-data mode (the
  test stack runs without Slack), and `pmos-web` refuses `PMOS_EXPOSURE=public` until H10.
- The checks need Postgres running (`pg_ctlcluster 16 main start`); a restarted container has
  it stopped, and the database tests then fail with "connection refused". A restart also stops
  the Docker daemon. Local kit tests use stand-ins for Docker, so only CI's deploy job shows
  whether a service starts on a real stack.

## Waiting on the owner

For real data on a hosted model (`docs/openshell-any-host-spec.md` §3.9 and slice H13): the
owner's recorded approval of the provider and confirmation of its data-processing and retention
terms; the first host (an Ubuntu 24.04 x86 virtual machine); an Entra app registration, with
`acct` as an optional claim; human labels for the eval judges
(`evals/judges/LABELLING.md`); pilot PM names; edits to the nudge wording; a GitHub App
with pull-request permission and branch protection on the content branch; approval of the three
plans (MCP servers and which ones for the pilot, the OpenShell prover, sandbox decisions in the
audit log) and the go-ahead for requirement i; where the provider's approval record is kept
(outside the repository) and who besides the owner may approve a provider; whether to
squash-merge pull request #10 so the vendor's name stays out of `main`'s history; the CODEOWNERS lines and branch-protection settings in
`policies/README.md`, so a change to a bot's instructions needs a second reviewer.
