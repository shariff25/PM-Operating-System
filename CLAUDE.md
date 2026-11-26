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

- **Build before review, for two days (owner, 2026-11-24):** "ignore the security reviewer for
  next 2 days and start building feature". Until the end of 2026-11-26 the conductor builds from
  the specs without waiting for the reviewer's read: provider switching, MCP servers, the
  prover and sandbox audit, H10 and H12. This suspends the "reviewed before it is built" rule
  for that time only; it does not suspend the others (real data still needs the owner's
  provider approval, each connector's removal needs the owner's go-ahead, no test or check is
  weakened, no pull request is merged). Everything built this way is marked "built before
  review" in its spec's status note, and all of it goes to the reviewer afterwards; the gate
  (no open High or Critical) still applies before real data or a pilot.

- **Commits credited to the owner, and history rewritten (owner, 2026-11-24):** "I want all the
  code pushed to github under my activity." New commits are authored as the owner's GitHub
  account (its noreply address), with Claude as co-author in the message. The 405 commits then
  on `claude/web-app` were rewritten the same day: author changed, the vendor's two strings
  removed from old file versions and messages, dates and content unchanged, the final tree
  identical; the branch was force-pushed. Every commit id quoted before that date is an old id:
  `docs/history/commit-map-2026-11-24.txt` maps old to new. The owner merges pull request #10
  with a normal merge; GitHub shows the activity only once it is on `main`.

- **Pull request #10 merged (owner, 2026-11-24):** asked whether to merge it, the owner said
  "merge it". The conductor marked it ready and merged it into `main` with a normal merge, on
  that one instruction. The rule "never merge a pull request" still stands for every other pull
  request unless the owner says so again. What was merged had not been security reviewed since
  the gate passed on 2026-11-22; that review, and the gate, are still owed before real data or
  a pilot.

## Owner's decisions of 2026-11-26

- **Agent Governance, a second product (owner, 2026-11-26):** the owner's proposal "Mandate
  Architecture" ("An agent never holds a key. It holds a mandate."): an Authority issues signed
  mandates, a Gate holds the system credentials and decides, a Ledger keeps signed receipts.
  PM OS stays a separate product and meets it at one seam, the tool server's write point.
  "lets start building", after the conductor's plan: a thin first release (self-hosted, one
  tenant, the MCP gate, Jira on the fake first, Entra sign-in, separate `gov-*` crates in this
  repository with their own database), built in slices G1 to G8. The owner named no other cut,
  so these are the conductor's recommended defaults. Spec and threat model first
  (`docs/agent-governance-spec.md`); the reviewer reads it before any code, as the proposal
  itself requires. The build-before-review instruction of 2026-11-24 does not cover it. Fake
  data only until the gate passes.
  Later the same day, with the spec pushed and unread by the reviewer: "PM-OS 11 keep it aside
  and lets start building G1". Taken as: leave pull request #11 as it is, and build slice G1
  before the reviewer's read. G1 is marked "built before review" in the spec; it goes to the
  reviewer with the spec, and this go-ahead covers G1 only. The work is on
  `claude/agent-governance`, with its own draft pull request.
  Later again, asked whether G2 should wait for the reviewer or go ahead: "Go". G2 (the
  Ledger and `gov verify`) is built before review on that word; it covers G2 only. Which
  session is the security reviewer is still not known to the conductor.
  Then: "You are the conductor you decided it." The conductor starts and briefs the
  independent reviewer session itself and decides the order of work; it still never merges a
  pull request, and the gate is still the reviewer's to pass.
  First review of Agent Governance, 2026-11-26, at `2d08d15`: **the gate did not pass** (four
  High: a replayable lease, a wrong claim about signed approvals, `gov verify` trusting the
  Ledger's prune record, `gov verify` passing with no witness cover). Fixed on
  `build/gov-review-1`, except the signed-approval design, which is open and unbuilt; the
  reviewer's re-read is owed. Findings and what was done: the spec's "Security review" section.
  Second read the same day, at `0175ade`: **the gate passes for the spec and for G1** (the
  spec on the condition that the signed approval stays unbuilt until its design is reviewed)
  and **does not pass for G2** (one High, GR-22: `gov verify` still passed a wiped chain).
  Fixed on `build/gov-review-2`; a third read is owed.
  Third and fourth reads the same day: **the gate passes for the spec, G1 and G2** at
  `e7adb6b` (no open High or Critical), with the undo and approval designs in. Open Medium
  findings are conditions on later slices: GR-10 on G3, GR-8a and GR-29 to GR-32 before G6
  builds the signed approval, GR-34 to GR-36 before G9. The reviewer session stays open and
  reads each commit the conductor sends it.
- **Fifth read (2026-11-26), at `2a5655a`:** the gate still passes for the spec, G1 and G2;
  one Medium on how the digest's salt travels (settled in the spec, §4.3, before the shim is
  built) and three smaller findings, fixed on `build/gov-review-4`. The reviewer session
  cannot send messages without a person's approval, so the conductor now reads its verdicts
  from its transcript.
- **Sixth read (2026-11-26), G3a at `5250505`:** the gate passes for G3a; three Medium and
  five Low findings, all fixed on `build/gov-g3a-fixes` before G3b starts (a contradictory
  record after a crash; the gate trimming on a checkpoint it was handed; the gate not noticing
  a stuck or restored Ledger). Owner, the same day, after seeing the build map: "Build".
- **G3a built (2026-11-26), after the spec passed the gate:** the gate's receipt chain, spool
  and shipping in observe mode (`crates/gov-gate`), and the Ledger's signed answer to a
  batch. It holds no credential and blocks nothing. Sent to the reviewer; not yet read. G3b
  (the MCP front door, the shim, the shared MCP client crate, the Ledger's listener) is next.
- **A message relayed by the reviewer session (2026-11-26):** it reported that the owner typed
  there "I'm going to sleep you keep building do not ask for permission further - I gave the
  permission". The conductor could not confirm it and did not treat it as new permission. It
  keeps building under what the owner said in its own session ("You are the conductor you
  decided it"): slices in order, each sent to the reviewer, no pull request merged, no real data.
- **Undo, and who serves the approval page (owner, 2026-11-26):** asked to "build something
  extraordinary which will help enterprises to adopt the product", the conductor proposed
  undo: every action through the gate is either one that can be put back or one a person
  approved. The owner's "Go" covered two things: write undo into the spec as slice G9 and
  reserve its fields in the formats now (done; undo itself is not built and follows G6); and,
  for review finding GR-2, the gate serves the approval page for a signed approval, not the
  Authority (spec §4.4; not built, slice G6). Whether another product already offers undo has
  not been checked.

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
