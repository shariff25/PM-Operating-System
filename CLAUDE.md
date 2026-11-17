# PM OS: standing requirements for Claude sessions

The owner's requirements for the PM OS build. Read this before planning or building. Stated by
the owner on 2026-11-16 and 2026-11-17; update it when they change something.

## Scope (the owner's list, a to h)

- **a. Authentication:** primarily Azure AD (Entra ID) single sign-on.
- **b. Authorization:** role-based access with four roles: Admin, Product Manager, Reviewer,
  Auditor. A Reviewer recommends and the draft's owner confirms. An Auditor sees metadata only.
  The PM OS user database is the source of users and roles.
- **c. Policy Admin** for NVIDIA OpenShell. The app opens a pull request; the merge happens in
  GitHub.
- **d. Front end:** a web app PMs love. The owner holds the design to the standard of the
  Baseten and WorkOS dashboards and will keep challenging it. Design approved for the MVP on
  2026-11-16: `web/` as rebuilt in `claude/web-redesign-v3`.
- **e. Harness management and context-layer management.**
- **f. Audit logging.** Retention target: 90 days (not yet confirmed as final).
- **g. Connectors.**
- **h. A review of how the Grok bot was built,** with pointers for PM OS.

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
  build and the browser tests; in `harness/`: typecheck, lint, test.
- **Secrets:** never print, log or send a token. The test stack uses fake data only, and Slack
  message text never goes into a report.
- **Report honestly:** say what was verified and what was not. Nothing counts as measured
  against a model until an eval run has happened.

## Waiting on the owner

An Entra app registration; a model key with enough quota for an eval run; pilot PM names and
live connector accounts; confirmation of 90-day audit retention; edits to the nudge wording.
