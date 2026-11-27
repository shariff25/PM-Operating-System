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
- **Eighth read (2026-11-26), G3b's connector map and forwarder at `b733bee`:** the gate
  passes **for observe mode only**. Seven Medium findings (GR-61 to GR-67) let a hostile agent
  or server escape or falsify the record; four would be High once anything is enforced, and
  the reviewer said this code may not back shadow or enforce mode until they are closed.
  Fixed on `build/gov-review-8` with GR-68 to GR-73 (GR-72, the gate's clock, waits for G4);
  19 controls broken on purpose, each caught. The re-read, and the first read of the listener
  step, are owed.
- **Ninth read (2026-11-26), at `5c6c4e6`:** the gate passes for the spec, G1, G2, G3a and
  G3b steps 1 to 3 **in observe mode**; the four findings that barred shadow and enforce mode
  are closed, with a standing condition that each connector map has its own test and is read
  by the reviewer. Two new Medium (GR-74, refusals stored without limit; GR-75, nothing
  required an encrypted connection) and six Low; GR-77 and GR-81 stay open as conditions on
  the listener's hardening and on G4. The rest are fixed on `build/gov-review-9`; the
  reviewer's read of those fixes and of the programs is owed.
- **G3b step 4 built (2026-11-26):** the two programs, `gov-ledger` and `gov-gate`, in a new
  crate `gov-run` (`build/gov-g3b-programs`). Run as an operator would against a fake MCP
  server and Postgres, the auditor's check of the export passes, also across a gate restart.
  Keys are files, refused unless `GOV_DATA_CLASS=fake`; neither program speaks TLS, so both
  keep to their own machine unless told otherwise; the gate's connector map is an unsigned
  file until G4. Not yet read by the reviewer. Still to build in G3b: the shim.
- **The shim (2026-11-26):** its protocol is written in the spec (§5.3.1) and sent to the
  reviewer before any code; the conductor's choice, since the build-before-review go-aheads
  covered G1 and G2 only. The shim is the last part of G3b.
- **Tenth read (2026-11-26), at `09dea18`, and the shim's design at `4fba4cf`:** the gate
  passes for the spec, G1, G2, G3a and G3b steps 1 to 4 **in observe mode**. One new Medium to
  fix before G3b is called done (GR-82: the gate slows to a stop when the Ledger is down,
  because every receipt reads the whole spool under one lock) and five Low. The shim may be
  built once four Medium points in its design were answered in the text (GR-88 to GR-91: a
  copied mandate evicting the real session, a call run twice, a signing port a web page can
  reach, a failed proof with no mark of its own); they are, in §5.3.1. Standing condition
  from GR-75: any other embedder of the gate's libraries (G7) must make the programs' check
  on plain HTTP itself.
- **Eleventh read (2026-11-26), at `b97d8b3`:** the gate passes for the spec, G1, G2, G3a and
  G3b steps 1 to 4 in observe mode; G3b is done but for the shim, and **the shim may be built
  from §5.3.1 as written**. Four Low findings (GR-97 to GR-100). Before it, CI had found a
  real fault in an earlier fix (an agent leaving at the wrong instant left a receipt without
  its outcome); fixed at `e76ab0f` and read by the reviewer.
- **Owner, 2026-11-26: "Build the shim".** Given as the reviewer's verdict clearing it came
  in. The shim is built from §5.3.1, on `build/gov-shim`.
- **G3b step 5 built (2026-11-26): the shim,** `gov-shim`, and sessions at the gate
  (`build/gov-shim`). Run as three programs with fake data, an agent's call through the shim
  is recorded as proven by the agent its mandate names; after a gate restart the next call
  is forwarded once and recorded as a failed proof. The mandate comes from a test issuer:
  there is no Authority yet. With it G3b is built; the reviewer's read of the shim is owed.
- **Twelfth read (2026-11-26), the shim at `543407a`:** **the gate passes for G3b as a
  whole, in observe mode** (no High or Critical). Five Low findings, three fixed on
  `build/gov-review-12` (proven can only be built from the sessions check; an overflow in a
  time check; honest calls failing after a gate restart) and two stated as limits. To keep
  in mind: a proven receipt says who made a call, not that the call was authorised. Next
  slice: G4, the Authority.
- **G4, the Authority (2026-11-26):** how the slice is cut, and eight choices that were open,
  are written in the spec (§7.6) and sent to the reviewer before any code. Five steps, G4a to
  G4e. One departure from the spec's wording: admin actions are recorded in a signed document
  of their own, chained and witnessed like receipts, not as receipts.
- **Thirteenth read (2026-11-26), the G4 design at `b2a0c74`:** no High or Critical; G4a
  may be built once two Medium points were answered in §7.6 (how enrolment works; three
  conditions on admin records), which they are. A third (key roles: a root key that signs
  only the policy bundle) is settled in the text before G4b. One of the twelfth-read fixes
  opened a new Medium in the shim (GR-112), fixed on `build/gov-review-13`.
- **Owner, 2026-11-26: "Build don't stop and push the code as shariff25@gmail.com in
  github".** From this point commits are authored with that address instead of the account's
  noreply address (the instruction of 2026-11-24 is changed by this one); earlier commits are
  not rewritten. The address is then public in the repository's history. "Don't stop" is
  taken as: keep building the slices in order without waiting to be asked. It is not taken
  as leave to skip the reviewer's read of a design before code, to merge a pull request, or
  to use real data.
- **G4a, first part, built (2026-11-26):** the Authority's registry, enrolment by one-time
  code and its signed admin records (`crates/gov-authority`, on `build/gov-g4a`), on memory
  and Postgres stores. A library only: nothing is served, nobody is signed in, and its chain
  is not yet shipped to the Ledger. Not yet read by the reviewer.
- **G4a, second part, built (2026-11-26):** the Ledger holds chains of two kinds and reads
  each key as its own kind only; the Authority ships its admin records there, under the same
  witness as the gates' receipts; and the Ledger enrols a gate's key only on the Authority's
  signed word backed by a witnessed record (`build/gov-g4a2`). Still libraries: the three
  programs do not use the Authority yet. Not yet read by the reviewer.
- **Fourteenth read (2026-11-26), G4a's first part at `36ded25`:** no High or Critical; it
  passes as a library nothing serves yet. Two Medium findings in enrolment (one code could
  enrol two keys at the same moment; the value meant to authenticate the root key did not)
  and six Low, fixed on `build/gov-g4a2` with the second part, which was changed to follow
  the reviewer's advice: a gate's key reaches the Ledger through the witnessed record of its
  enrolment, and a disabled gate's key is retired there.
- **A test failed once in a full run (2026-11-26):** a PM OS password test, in code this work
  did not touch, while the disk was 100% full. With the disk cleared it passed five times
  running. Taken as caused by the disk; not proven.
- **CI failed once on `36ded25` (2026-11-26):** a gate test looked at the record a fixed 1.2
  seconds after eight slow calls and found one still open. The test now waits until every call
  is closed (20 seconds at most). Taken as a timing fault in the test, not in the gate; a
  deterministic test of the same control passed in that run.
- **The auditor's check of gate keys built (2026-11-26, GR-84):** `gov verify`, given an
  Authority's key, believes a gate's key only where a witnessed record of its enrolment bears
  it out (`build/gov-g4a3`). With it G4a is built as libraries; nothing is served yet. Not
  yet read by the reviewer, nor is G4a's second part (sent at `21645dd`).
- **Owner, 2026-11-26: "Once you are done with the current tasks, push the code and merge it
  into GitHub".** Taken as leave to merge pull request #12 (Agent Governance) into `main`
  once the work in hand was checked and pushed; it covers that one merge. What is merged has
  the reviewer's pass through G4a's first part; the second part and the auditor's check are
  unread. Fake data only still holds.
- **Pull request #12 merged (2026-11-26):** on the owner's instruction above, marked ready
  and merged into `main` with a normal merge at `4a8f6cd`, with CI green on its head. The
  rule "never merge a pull request" stands for every other pull request unless the owner
  says so again. Work goes on from `claude/agent-governance`, restarted from `main`.
- **Fifteenth read (2026-11-26), G4a's second part at `21645dd`:** no High or Critical; the
  gate passes for G4a, both parts. Three Medium findings to fix before G4b (an Authority's
  chain could be pruned; a disabled gate's keys could stay in use after a stop at the wrong
  instant; one route made the Ledger re-read whole chains for anyone) are fixed on
  `build/gov-review-15`, unread by the reviewer. Two Low are open (GR-126 with G4d, GR-127
  with G4b). The reviewer also listed what the bundle's format must settle before G4b.
- **Sixteenth read (2026-11-26), at `3ccfb5a`:** no High or Critical; the gate still passes
  and the three Medium fixes are closed. The design of G4b (the signed policy bundle, spec
  §7.6.1) stands, and may be built once one Medium point was answered in the text (GR-128:
  what the root key signs must come from the reviewed repository, and the signer is shown
  what changed), which it is. Six Low findings: four answered in the text, two open
  (GR-130 with G4d, GR-131 with the Ledger program's next change). Pull request #13 holds
  this work as a draft.
- **G4b built (2026-11-26): the signed policy bundle,** from the design the reviewer cleared
  (`build/gov-g4b`). A root key signs the bundle and nothing else; `gov policy check` and
  `gov policy sign` are the release step, and the signer is shown what changed; the
  Authority publishes only the next number, with a record; the gate and the shim load
  their connector map and the issuers' keys from the bundle and refuse to go back to an
  older one. The unsigned files remain only under a flag for tests. Run as three programs
  with fake data. Not yet read by the reviewer. The bundle is still a file an operator
  copies; its rules are not yet evaluated (G5).
- **PM OS as the reference application for Agent Governance (owner, 2026-11-26).** The owner
  sent feedback under that title, asked whether the conductor agreed, and then said:
  "Continue with the agreed items and the rest you keep building". Agreed and written into
  the spec (§1.2, §8.4, slice G7): PM OS is Governance's first customer and never a
  dependency of it; the first release is built toward one scene (a PM OS agent does its
  work, is manipulated into one action outside its mandate, is refused at the gate, and an
  outsider verifies the refusal); and no fallback, so that in enforce mode PM OS refuses a
  write Governance cannot decide. Not taken up, with reasons in §1.2: a decision interface
  the application calls, identity and mandate as text fields, and enforcement before shadow
  mode. The scene cannot be shown as working enforcement until G6; nothing is blocked today.
  "The rest you keep building": slices in order, as before.
- **Seventeenth and eighteenth reads (2026-11-26), G4b at `04e5117` and spec text at
  `1600134`:** no High or Critical; G4b is sound as built. The design of G4c (mandates from
  the Authority, spec §7.6.2) may be built once one Medium point was in the text (GR-138:
  the gate holds every mandate to the template in the root-signed bundle it holds, so a
  taken mandates key yields only what the templates allow), which it is. Two Medium points
  on the spec text written from the owner's feedback, both reworded: an outsider verifies
  that the gate recorded a refusal, not that nothing reached the system; and with the gate
  as a library inside PM OS, PM OS's operator can go round it, so the scene is run with a
  separate gate holding the credential. The reviewer agreed with the three items not taken
  up. Its session reported a usage limit once during this read and then answered.
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

## Owner's decisions of 2026-11-27

- **Agent access governance (owner's PRD of 2026-11-26, pasted on 2026-11-27 with "Check
  this prd"):** a context graph with labels that rise with what a session reads, a reviewer
  agent that may approve a narrow class of actions, access reviews, and three console
  screens. The conductor found four requirements that do not work as written (the label
  cannot see across gates; it resets with a new session or a restart; a sub-agent's reads
  do not reach its parent; suspension through the revocation feed contradicts GR-140). The
  owner: "You assess where it make sense". The assessment is `docs/access-governance-spec.md`:
  the PRD's requirements, eight departures, four added threats, and the order (the
  Authority first; the receipt fields right after G4c; everything that blocks or approves
  after G6). Not yet read by the reviewer; nothing built. Then: "Push the prd specs to
  github and design": the PRD (`docs/prd-access-governance.md`) and the graph explorer's
  design source (`docs/design/graph-explorer-design.dc.html`) are committed as the owner
  pasted them. The PRD's architecture image was never given and is not in the repository.
  Where the PRD and the spec differ, the spec is what is built from.

- **Nineteenth read (2026-11-27), at `96c7c1c`:** no High or Critical; the access governance
  spec passes the gate as a design, and G4c may be built from the main spec as it stands.
  Four Medium points on labels, answered in the spec's §11 (clearance applies to what any
  call sends, not only writes; label tokens merge without numbers; how gates learn each
  other's token keys; one label per agent key across its live mandates). One decision left
  to the owner: whether a gate may ask the Authority at the moment of an outward call, to
  close the window in which an agent reads at one gate and writes at another.
- **Research for a patent (owner, 2026-11-27):** "Governance graphs is the most of this
  product ... do deep research and come back with unique idea. I want to patent it". The
  report is in `reports/` on the conductor's machine and was sent to the owner; it and its
  notes are kept out of git on purpose, and the ideas it shortlists are **not to be written
  into any committed file** until the owner has decided about filing. The owner then:
  "Continue building G4c".

- **What makes the product hard to replace (owner, 2026-11-27):** asked how to build a moat,
  the conductor answered: a library of reviewed connector maps, being the record outsiders
  accept (with proof that it is complete), the gate holding the credential, and undo and
  access reviews as recurring work; and recommended narrowing. The owner chose otherwise:
  "Keep the exiting feature and build and add the new things you suggested". So nothing is
  dropped, and three slices are added to the main spec's §10: CM1 (connector maps for Jira,
  GitHub and Slack), CM2 (drift) and CP (proof about the record). CP's design stays out of
  every committed file until the owner decides about filing; the repository answered a
  request with no login on 2026-11-27, so it is taken to be public until the owner says
  otherwise. Not buildable by the conductor: design partners and a real deployment.
- **G4c built (2026-11-27), on `build/gov-g4c`:** mandates from the Authority. Templates
  are in the signed bundle and a gate holds every mandate to its template; the Authority
  issues to an agent's owner, delegates narrower at the holder's signed request, lists,
  revokes, and signs a numbered revocation feed; `gov verify` reports a mandate nobody
  issued. Run as three programs with fake data on a mandate the Authority issued; a mandate
  signed with a stolen mandates key but wider than its template is not recorded as proven.
  Not yet read by the reviewer. The gate does not read the feed yet (G4d), and nothing is
  served (G4e).

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
