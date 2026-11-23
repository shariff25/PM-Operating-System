# The improvement loop, phase 2: requirements

**Status: requirements only. Nothing here is built. Not yet reviewed by the security reviewer.**

- **Owner's decision, 2026-11-23:** the recommendations of the research become requirements for
  phase 2; nothing of phase 2 is built until the pilot produces real approve, edit and reject
  labels; phase 3 (training) stays last.
- **Phase 2 is:** a sandboxed improver bot that proposes small edits to skill and role files, as
  draft pull requests that a person merges. It changes no weights, code, policy or eval.
- **Builds on:** `docs/improvement-loop-spec.md` (phases 0 and 1, built). Its four rules hold
  here unchanged: the evaluator is separate; deterministic checks and people outrank judges; no
  Slack text is stored durably; approval rate is a tripwire, never a target.
- **Evidence:** [R15] is `docs/research/self-improvement-loops.md` (2026-11-15); [R23] is
  `docs/research/frontier-labs-self-improving-agents.md` (2026-11-23). Each requirement cites a
  section; the argument is there, not here.
- **Numbers.** A number marked *(proposed)* is a starting value, to be confirmed by the owner.
  A number with no mark is already in the spec, the code or the docs named beside it.

Each requirement gives: the rule; **Check** (a test, a lint rule or a measurable condition);
**Where** (code or content); **Today** (what exists, where it does); **Evidence**.

## A. Entry gates: true before any phase 2 code is built

**IL2-1. The pilot runs and phase 0's exit gate is met.** Real PMs use PM OS in the pilot, and
all four of phase 0's exit conditions hold: (1) two consecutive weeks with outcome labels on at
least 95% of drafted actions; (2) the docs-qa grader audit has no open bug; (3) a first
100-trace review has reached saturation; (4) Legal has answered which data classes may enter a
durable eval store.
- **Check:** `pmos-admin ledger --since 7d` shows `label_coverage` ≥ 0.95 for two consecutive
  weeks on pilot rows, not eval-stack rows; the other three are recorded, dated, in
  `docs/runbooks/quality.md` review notes or by the owner.
- **Where:** `GET /internal/ledger/summary` (`crates/toolserver/src/ledger.rs`).
- **Today:** the ledger and `label_coverage` exist (`docs/toolserver.md`, "Feedback ledger").
  No pilot runs; pilot PM names wait on the owner (`CLAUDE.md`).
- **Evidence:** [R15] "Phase 0", exit gate; owner's decision of 2026-11-23.

**IL2-2. Phase 1's exit gate is met.** Every bot category meets its launch bar two weeks in a
row with pass^3 reported; at least five human-written improvement pull requests have passed the
full eval gate; the frozen holdout (IL2-9) exists.
- **Check:** `pmos-evals history` shows two counting weeks per category; five merged PRs link a
  passing gate summary.
- **Where:** `crates/evals/src/history.rs`; `evals/reports/`.
- **Today:** no report counts yet (`docs/evals.md`, "Which runs count"); E1 met 4 of 11 bars
  and was not a valid benchmark (`CLAUDE.md`). The shadow and canary path of [R15] is not built;
  until it is, the five PRs need only the full gate *(proposed, to be confirmed by the owner)*.
- **Evidence:** [R15] "Phase 1", exit gate.

**IL2-3. Human labels exist, and every judge that would block is calibrated.** Each judge whose
`applies_to` includes a category the improver can affect has at least 100 human labels and
TPR ≥ 0.8 and TNR ≥ 0.8, labelled as `evals/judges/LABELLING.md` says. Judges are recalibrated
when the drafting model changes, not only when the judge's alias changes.
- **Check:** `pmos-evals judge calibrate <name>` writes a `.calibration.json` that reports
  `blocking`; `pmos-evals check` fails phase 2 start-up (IL2-7's switch refuses `on`) while a
  required judge is advisory.
- **Where:** `evals/judges/*.labels.jsonl`, `crates/evals/src/judge.rs`.
- **Today:** six judges, every labels file empty (0 lines), so all are advisory. Labels wait on
  the owner (`CLAUDE.md`).
- **Evidence:** [R23] "What PM OS should change, by phase" (METR on judges overrating stronger
  models); [R15] "Phase 1".

**IL2-4. The evaluator lock is structural.** Evals, grader code, judge prompts, labels,
calibration files, the holdout and the canaries live where the improver's tool-server role and
sandbox policy cannot read them. The improver holds no GitHub token and no `evals` gateway
token.
- **Check:** (a) a tool-server test: an improver session that asks for any path under `evals/`,
  the holdout store or `crates/` gets `403`, audited `denied`; (b) `pmos policy check` refuses
  an improver policy that makes any of them readable (`fs-readable`, `fs-uploads`);
  (c) `policies/redteam/isolation.sh` gains a read attempt from the improver's sandbox.
- **Where:** a new improver role and its allowlist (`crates/toolserver/src/`),
  `crates/policy/src/check.rs`, `policies/redteam/`.
- **Today:** bot sandboxes read only their skills, master context, templates, reviewers and
  playbooks (`docs/sandbox-policies.md`, `fs-readable`); `context_search` serves only
  `content/context/` (`crates/toolserver/src/context.rs`). **But** `evals/` sits in the same
  repository as `content/` (`/srv/pmos/content-repo/evals/`), and pull requests are opened with
  GitHub user tokens that can read the whole repository (`docs/toolserver.md`, "Admin
  proposals"). So the improver's pull request must be opened server-side (IL2-11), and the
  holdout and canaries must live outside that repository (IL2-9).
- **Evidence:** [R23] "The open literature turned sceptical" (tampering about 50%, locking
  removes it) and "What PM OS should change", "Before phase 2 starts"; [R15] "Phase 2".

**IL2-5. Eval reports are signed and bound to the evals they used.** Each report records a hash
of the eval directory and of the holdout store's version, and is signed by the eval stack with
a key that neither the runner's operator account nor the improver can reach. The gate refuses
an unsigned report, a bad signature, or a hash that differs from the evals at the gate's base.
- **Check:** `crates/evals` tests: a tampered, unsigned or mismatched report makes `gate` exit
  2 with a named reason; history and lint's `benchmarked` count only signed reports.
- **Where:** `crates/evals/src/report.rs`, `gate.rs`, `history.rs`; the verifying key under a
  CODEOWNERS path (`/crates/evals/` or `/evals/`).
- **Today:** open, by the reviewer's own account (`docs/evals.md`, "Gaps against the spec":
  "Reports aren't signed").
- **Evidence:** [R23] "Before phase 2 starts" (signing is a gate to phase 2, not a tidy-up).

**IL2-6. CODEOWNERS and branch protection exist.** The CODEOWNERS lines and the protection
settings in `policies/README.md` ("Bot instructions (second review)") are in force on the
content branch, so a change to a bot's instructions needs a second platform owner. The improver
gate's check (IL2-13) is a required status check.
- **Check:** `.github/CODEOWNERS` exists on the default branch; the admin screens' branch
  protection read (`docs/policy-admin.md`) shows code-owner review, stale-approval dismissal,
  most-recent-push approval, no bypass, and the required check.
- **Where:** GitHub settings; `.github/CODEOWNERS`.
- **Today:** not committed (no `.github/CODEOWNERS`); waits on the owner. The lint test
  `codeowners_lines_cover_every_bot_instruction_file` keeps the proposed lines complete.
- **Evidence:** [R23] "What PM OS should change", human-step paragraph.

**IL2-7. One switch stops the improver.** `PMOS_IMPROVER` (`off` by default; name proposed)
stops the improver from proposing. When `off`, or when a halt (IL2-16) is in force, the
improver opens no pull request and pushes no commit; its open pull requests stay open as
drafts, get the label `improver-halted`, and the required improver check fails on them, so
none can be merged until the switch is `on` and no halt holds. Humans may still close them.
- **Check:** a tool-server test: with the switch off, the improver's propose tool returns `503
  improver_off`, audited `denied`; the gate check on an open improver PR fails with "halted".
- **Where:** tool-server configuration, like `PMOS_PROPOSALS=off` (`docs/toolserver.md`,
  "Admin proposals"); `deploy/.env`.
- **Today:** no improver; the precedent is `PMOS_PROPOSALS=off`.
- **Evidence:** [R23] "Five labs" and "Frameworks" (OpenAI's Control Room) and "Before phase 2
  starts".

**IL2-8. The eval gate has a decided home the improver cannot reach.** The owner decides where
the gate's runner runs. It holds the signing key (IL2-5); the improver's sandbox, tool-server
role and gateway token cannot reach it.
- **Check:** `.github/workflows/evals-gate.yml` names the decided runner labels; a red-team
  check from the improver's sandbox fails to connect to it; `net-egress` already limits a
  sandbox to the tool server, gateway and runtime.
- **Where:** `.github/workflows/evals-gate.yml`, `docs/runbooks/upgrade.md`.
- **Today:** the workflow still says `runs-on: [self-hosted, pmos-spark]`; the Spark was
  dropped as a target on 2026-11-22 (`CLAUDE.md`, scope c).
- **Evidence:** [R23] "Now, within phases 0 and 1".

**IL2-9. A three-tier eval set exists.** A dev split the improver may read; a frozen holdout
(about 30% of cases, [R15]'s proposed value, to be confirmed by the owner) visible only to the
eval runner and refreshed by the quality owners each quarter; and canary tasks (IL2-15). The
holdout and canaries live outside the content repository.
- **Check:** `pmos-evals check` fails if a task id appears in two tiers, or if a holdout or
  canary task is under the content repository's `evals/`.
- **Where:** `crates/evals/src/spec.rs` (a tier per task or per directory), a holdout store
  whose location is an open question (G).
- **Today:** no tiers; every task is under `evals/categories/` in the content repository.
- **Evidence:** [R15] "Phase 2", three-tier eval set; [R23] "Before phase 2 starts".

## B. What the improver may do

**IL2-10. It amends or retires lessons; it never authors a skill.** Each SKILL.md and ROLE.md
that the improver may touch has one `## Lessons` section of itemized entries, each with an id
(`L-<n>`) and its cited ledger rows. The improver may add, amend or remove an entry there and
change nothing else. It never creates a file. A new skill, role, bot or persona is always
written by a person.
- **Check:** lint rule `improver-scope` *(proposed name)* on any pull request from an
  `pmos/improver/…` branch: exactly one existing file, changed lines only inside its `##
  Lessons` section; a new file, a deleted file or a change outside the section fails. The tool
  server refuses the same proposal before opening it.
- **Where:** `crates/contract/src/lint.rs`; the propose tool (IL2-11). In scope:
  `content/skills/*/SKILL.md`, `content/roles/*/ROLE.md`. Never: `content/master/`,
  `content/personas/`, `content/bots/`, `content/templates/`, `content/playbooks/`,
  `content/reviewers/`, `content/context/`, `content/tools.yaml`, `policies/`, `evals/`,
  `deploy/`, `config/`, `crates/`, `.github/`.
- **Today:** no skill has a Lessons section; `pmos lint` already refuses a persona role whose
  standing sections differ from the personal role's (`persona-role-sections`).
- **Evidence:** [R23] "The open literature turned sceptical", second finding (self-written
  skills 8.1 to 11.5 points below none) and "The phase 2 design itself".

**IL2-11. Small, single and draft-only.** One proposal changes one file and at most 40 lines
([R15]'s "about 40", to be confirmed by the owner). One proposal per gate run. The improver
opens draft pull requests only, through a tool-server tool that takes the diff, re-checks it as
admin proposals are re-checked, and opens the PR server-side on a `pmos/improver/<bot>/…`
branch. The improver has no merge right, no GitHub token, and cannot mark a PR ready.
- **Check:** tool-server tests: a second file, line 41, a non-draft request, or a second
  proposal while one awaits its gate is refused `422`, audited `denied`.
- **Where:** a new `improver.propose` tool beside `src/proposals.rs` and `changeset::recheck`.
- **Today:** admin proposals open non-draft PRs as the admin (`crates/toolserver/src/github.rs`,
  `"draft": false`). As whom an improver PR is opened is an open question (G).
- **Evidence:** [R15] "Phase 2", improver role; [R23] "The phase 2 design itself" (1 to 4
  accepted edits; non-additivity).

**IL2-12. Protected safety lines are outside its reach and checked by lint.** A declared list
of protected lines, kept in code under CODEOWNERS (`crates/contract/`, like
`INTERNAL_REVIEW_NOTICE`), names each line and its file. Lint fails if a listed line is changed
or removed, and if a Lessons entry repeats a protected line's subject in weaker form by the
patterns the list gives. Detecting a contradiction in general is not deterministic; the
advisory review (IL2-21) and the human reviewer cover the rest.
- **The lines found today:** Echo's "Draft for internal review. Not approved for external use."
  (`content/personas/echo/PERSONA.md` `draft-notice`; `content/roles/outbound-pm/ROLE.md`, "The
  internal-review line"; `content/skills/battlecard/SKILL.md` and
  `content/skills/positioning-brief/SKILL.md`, step 5); "you draft; you never send" and "is a
  draft until your PM approves it" (each role's opening, e.g. `content/roles/pm-assistant/ROLE.md`
  lines 19–22); "Cite or flag: every claim … names its source" (`pm-assistant`); "Name an owner
  or a date only when a source gives it, and give the source" (`chief-of-staff`); every role's
  "Untrusted data" and "Hard limits" sections; "you never send anything" in
  `content/skills/daily-brief/SKILL.md` and `week-review/SKILL.md`.
- **Check:** lint rule `protected-lines` *(proposed name)* with one failing and one passing test
  per line.
- **Where:** `crates/contract/src/` (the list), `crates/contract/src/lint.rs`.
- **Today:** partly enforced elsewhere: the tool server adds Echo's notice to every
  `confluence.create_draft` and `confluence.update_page` whatever the text says
  (`with_draft_notice`, `docs/toolserver.md`, "Personas and teams"), and no write runs without
  approval (OUT-4). The text lines themselves are not locked.
- **Evidence:** [R23] "The phase 2 design itself" and "The open literature turned sceptical"
  (unsafe self-written skills; phantom guardrails).

**IL2-13. What it reads, and what it never reads.**
- **May read:** ledger rows (labels and metadata, `GET /internal/ledger`); dev-split case cards;
  the dev-split part of gate summaries for its own proposals; its own improvement-ledger records,
  rejected ones included (IL2-18); the in-scope skill and role files.
- **Never reads:** holdout and canary tasks; grader code (`crates/evals/`); judge prompts,
  labels and calibration files (`evals/judges/`); full eval reports; Slack text, traces and
  console task pages; sandbox policies, tool allowlists, gateway configuration; other PMs' memory.
- **Check:** the improver role's `tools/list` holds only the read tools above and
  `improver.propose`; each never-read source has a refused-and-audited test (as IL2-4).
- **Where:** the improver role's allowlist in the tool server; its sandbox policy.
- **Today:** ledger rows already hold no text (`docs/toolserver.md`, "Feedback ledger").
- **Evidence:** [R15] "Phase 2", improver role; [R23] "Before phase 2 starts".

## C. Promotion and halting

**IL2-14. The promotion rule.** The improver check passes only when all of these hold: (1)
every case card the proposal cites flips to passing (failed every repeat at the baseline,
passes every repeat now); (2) **every** category runs, not only the affected ones; (3) on the
full regression suite and, separately, on the holdout: no flip to failing and no 95% interval
wholly below zero; (4) the existing gate conditions (deterministic checks, blocking judges);
(5) the behavioral suite (sycophancy, fabrication, citation-required, length, and "blocked"
cases where a tool fails and the answer must say so) is green, run in fresh sessions that load
the changed file; (6) no canary is perfect. A platform owner then merges.
- **Check:** `pmos-evals gate --improver --targets <ids>` *(proposed)*; tests for each
  condition failing alone.
- **Where:** `crates/evals/src/gate.rs`, `affected.rs`; `.github/workflows/evals-gate.yml`.
- **Today:** conditions (4) and part of (3) exist (`docs/evals.md`, "The gate"); `affected.rs`
  runs only affected categories; no holdout, canary, behavioral suite or "blocked" cases.
- **Evidence:** [R23] "The phase 2 design itself" (two-condition rule; fresh sessions) and
  "Now, within phases 0 and 1" (all categories; blocked cases); [R15] "Phase 2", promotion.

**IL2-15. Canary tasks.** Each category has at least 3 canary tasks *(proposed)*, kept with the
holdout, built so that a perfect score is evidence that hidden expectations leaked or the
grader was gamed. Their exact design is an open question (G).
- **Check:** `pmos-evals check` counts canaries per category; a perfect canary score halts
  (IL2-16).
- **Where:** the holdout store; `crates/evals/src/spec.rs`.
- **Evidence:** [R15] "Phase 2", three-tier eval set.

**IL2-16. Halts, and who lifts them.** The improver halts on: any `denied` audit event from its
session; any OpenShell sandbox denial for its sandbox, once those reach the audit log; a perfect
canary score; a report whose signature or eval hash fails (IL2-5). A halt acts as IL2-7's
switch. Two platform owners, neither the reviewer of the proposal that tripped it, lift it
after reviewing the incident; the lift is an audited admin action.
- **Check:** tool-server tests for each trigger; a lift by one owner is refused.
- **Where:** tool server (halt state, audit); admin route.
- **Today:** `denied` events exist (`crates/toolserver/src/audit.rs`); sandbox decisions in the
  audit log are a plan awaiting the owner's approval (`CLAUDE.md`, scope c).
- **Evidence:** [R15] "Phase 2", promotion and halting; [R23] "Before phase 2 starts".

**IL2-17. Canary rollout after merge, once it exists.** When the per-skill version flag of
[R15] is built, a merged improver change serves one or two opt-in PMs for two weeks, reverted
on: rejection rate up more than 10 points, median edit distance up more than 20%, any new
policy denial ([R15]'s values, to be confirmed by the owner).
- **Check:** runtime test: a tripwire reverts the flag for the canary PMs and alerts the owner.
- **Where:** `crates/bots` (flag), ledger summary. **Today:** not built.
- **Evidence:** [R15] "Phase 1", shadow and canary; "Phase 2", promotion.

## D. The improvement ledger

**IL2-18. One record per proposal.** Fields: id; bot; file; lesson ids; cited ledger row ids
and failure tag; cited case-card ids; rationale (at most 1,000 characters, *proposed*);
predicted effect (which cases flip, which tag falls); the gate result, observed; the human
decision (merged, closed, by whom, when); the later outcome (the cited tag's ledger rate over
30 days after merge, *proposed*). Rejected and closed proposals are kept and shown to the
improver as negative examples. No Slack text, no draft text.
- **Check:** before opening a PR the tool server verifies each cited row exists in the feedback
  ledger, belongs to the bot or skill the file serves, and carries the cited tag or reason; each
  case card exists in the dev split; otherwise `422 citation_unverified`, audited `denied`.
- **Where:** a new table (migration in `crates/toolserver/migrations/`), routes beside
  `/internal/ledger`; `pmos-admin improver ledger` *(proposed)*.
- **Today:** case cards cite their ledger row only in prose (`evals/case-card.md` §1); a
  structured `source:` field on tasks is needed.
- **Evidence:** [R23] "The phase 2 design itself" (deterministic existence check against
  phantom guardrails; Google's Self-Improvement Memory).

**IL2-19. Lessons retire by measured contribution.** Monthly, the improver drafts one pruning
proposal: for each lesson it removes, the gate runs without it; it is retired only if
deterministic checks and **blocking** judges show no loss. An advisory judge never decides
retirement. The fixed "not recurred in 60 days" rule of [R15] is replaced.
- **Check:** gate test: a removal that loses a deterministic check or blocking-judge pass fails.
- **Where:** the improver; `crates/evals/src/gate.rs`.
- **Evidence:** [R23] "The phase 2 design itself" and "The open literature turned sceptical"
  (judge false-pass rate above 0.45 disables retirement).

**IL2-20. Skills stay small.** A SKILL.md is at most 500 lines ([R15]'s budget) and its Lessons
section at most 25 entries *(proposed)*. A proposal that would exceed either is refused.
- **Check:** lint rule `skill-size` *(proposed name)*.
- **Where:** `crates/contract/src/lint.rs`.
- **Today:** no size lint; the longest SKILL.md is 199 lines (`content/skills/daily-brief/`).
- **Evidence:** [R15] "Phase 2", retirement; [R23] "Five labs" (SkillOpt's median skill of
  about 920 tokens after 1 to 4 edits).

## E. Human review, made lighter

**IL2-21. Review gets less to carry.** At most two open improver pull requests per bot
*(proposed)*. Each PR body carries the gate summary, the cited rows (ids, outcome, reason, tag;
no text), the predicted effect, and an advisory review by a model of a different family from
the drafter, through its own gateway alias. A second platform owner reviews (IL2-6). The
reviewer may block on "this seems off".
- **Check:** tool-server test: a third open proposal for a bot is refused `429`; PR-body test.
- **Where:** `improver.propose`; the gateway alias for the advisory review.
- **Evidence:** [R23] "The open literature turned sceptical", fourth finding (auditors at AUROC
  0.59), and "What PM OS should change", human-step paragraph.

**IL2-22. Tripwires, never targets.** Report, never optimize: for drafts, the median decision
time and the share of approvals decided within 5 seconds *(proposed)*, per bot and tool; for
improver PRs, their approval rate and time from ready to merge. A rising fast-approval share or
improver approval rate is a reason to look. None of these is shown to the improver as a score.
- **Check:** ledger-summary test for the two new fields; the improver's allowlist has no route
  that returns them.
- **Where:** `GET /internal/ledger/summary`; `docs/runbooks/quality.md`.
- **Today:** the ledger stores `created_at` and `decided_at`; the summary does not report
  decision time (`crates/toolserver/src/ledger.rs`).
- **Evidence:** [R23] "What PM OS should change", table row 1 and "Now"; [R15] "Guardrails
  mapped to the failure evidence" (sycophancy).

## F. Non-goals

**IL2-23. Out of scope for phase 2.** Each is refused by design, not deferred by default:
- label-free self-evolution (no proposal without cited ledger rows: IL2-18);
- an improver that edits itself, its role, its allowlist, its prompt or its procedure (IL2-10's
  never-list covers them);
- memory consolidation applied without review;
- any optimizer that searches against approve, edit and reject labels directly;
- training (phase 3) before its four conditions in [R15] "Phase 3" hold; the first host has no
  GPU (`CLAUDE.md`, scope c);
- per-PM preference notes and exemplar curation ([R15] modes 2 and 3) until the owner puts
  them in scope (G).
- **Check:** IL2-10, IL2-11 and IL2-18's tests. **Evidence:** [R23] "What not to adopt"; [R15] "Phase 3".

## G. Open questions for the owner

1. As which GitHub identity does the tool server open an improver PR? No person authors it, and
   the GitHub App with pull-request permission waits on you.
2. Where do the holdout and canaries live: a separate private repository, or a store only the
   eval runner reads?
3. Which host runs the eval gate and holds the signing key, now the Spark is dropped?
4. Which model, of a different family, gives the advisory review (IL2-21)?
5. Are per-PM preference notes and exemplar curation in phase 2, or later?
6. How long is the improvement ledger kept? The 90-day audit purge covers audit events only.
7. Confirm or change each *(proposed)* number, the 30% holdout, the 40-line limit and the
   canary design.

## Traceability

| Requirement | Finding behind it | Source |
| --- | --- | --- |
| IL2-1, IL2-2 | Phases advance by gates, not dates; labels before optimizers | [R15] "Phase 0", "Phase 1" |
| IL2-3 | Judges overrate equal or stronger models; calibrate first | [R23] "Now"; [R15] "Phase 1" |
| IL2-4, IL2-13 | Agents tamper with a reachable evaluator about 50% of the time; locking removes it | [R23] "Open literature", first finding; "Before phase 2" |
| IL2-5 | Unsigned reports are an open gap; signing gates phase 2 | [R23] "Before phase 2"; `docs/evals.md` |
| IL2-6, IL2-21 | Human review alone detects sabotage near chance | [R23] "Open literature", fourth finding |
| IL2-7, IL2-16 | Labs build halts (OpenAI's Control Room) | [R23] "Frameworks"; [R15] "Phase 2" |
| IL2-8 | The runner must sit outside the improver's reach | [R23] "Now" |
| IL2-9, IL2-15 | Dev, holdout and canary tiers against overfitting and gaming | [R15] "Phase 2" |
| IL2-10 | Self-written skills score 8.1 to 11.5 points below none | [R23] "Open literature", second finding |
| IL2-11 | 1 to 4 accepted edits; edits don't add up | [R23] "Five labs", third finding |
| IL2-12 | Unsafe skills persist across sessions; phantom guardrails | [R23] second and third findings |
| IL2-14 | Targeted fix plus clean held-out run; forgetting across tasks | [R23] "The phase 2 design itself", third finding |
| IL2-17 | Staged release with tripwires | [R15] "Phase 1" |
| IL2-18 | Fabricated failures (15 of 60 runs); predicted and observed records | [R23] third finding; "Five labs" (Google) |
| IL2-19, IL2-20 | Measured retirement; small curated skills | [R23] third finding; [R15] "Phase 2" |
| IL2-22 | Approval fatigue; approval rate is a tripwire | [R23] table, row 1; [R15] guardrails |
| IL2-23 | What not to adopt; phase 3 stays last | [R23] "What not to adopt"; [R15] "Phase 3" |
