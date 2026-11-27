# PRD: Agent access governance (context graph, reviewer agent, access reviews)

Nov 26, 2026 · Sh

## Summary

This PRD adds three capabilities to Agent Governance, built on the gate, Ledger and Authority already in the `gov-*` crates. The design target is that every agent's access is explained, reviewed all the time, and certified by a person every quarter, using evidence that only the gate writes.

**What ships:**

1. **Context graph.** Receipts gain session, cause, rule and label fields, and reads get receipts too. The Ledger projects all of it into a graph that answers who acted, for whom, what, where, when and why. Session labels let the gate refuse a sequence of individually allowed actions that would leak data.
2. **Reviewer agent approvals.** A new `agent` countersignature class lets a governed reviewer agent approve a narrow, reversible class of actions. It starts as recommend-only, beside human approval.
3. **Access reviews.** A reviewer agent finds risky standing access all the time and can only narrow it. People certify all standing access each quarter, starting from what the agent flagged plus a random sample.
4. **Console screens.** A graph explorer, a review queue and an access review campaign screen.

**Why:**

- Today's receipts say what one action was, but not what led to it.
- Threat T12, an agent turned by injected text acting inside its mandate, has no control.
- Nothing reviews standing access once it has been granted.

**Out of scope:**

- Precedent-based approvals, meaning "approve because similar cases were approved".
- Reviewer-agent approval of outward, irreversible or declassifying actions. These are never in scope.
- SCIM-driven offboarding, which is planned separately.
- Real data. Fake data only, until the security gate passes.

## Context for Claude Code

Build this in the PM-Operating-System repository, under the rules in its `CLAUDE.md`, and extend the existing Agent Governance spec. Do not start a new design.

**Read these first:**

- `CLAUDE.md`: the standing rules and the owner's decisions.
- `docs/agent-governance-spec.md`: the threat model T1 to T26, the signed documents, the gate's check order (§5.1), the Ledger (§6) and the Authority (§7).
- `crates/gov-core/src/receipt.rs`: `Receipt`, the closed `Reason` list and `deny_unknown_fields`.
- `crates/gov-core/src/countersign.rs`, `policy.rs` and `action.rs`.
- `crates/gov-ledger/migrations/0001_ledger.sql` and `crates/gov-authority/migrations/0001_authority.sql`.
- The images and design source in `docs/design/` (listed under Architecture and UI below).

**Build status this PRD assumes:**

| Slice | State |
| --- | --- |
| G1 core, G2 Ledger, G3 gate in observe mode, shim | Built, through the reviewer's gate in observe mode |
| G4a Authority registry and admin records | Built as libraries; parts not yet read by the reviewer |
| G4b to G4e, G5 shadow and replay, G6 enforce, G7 PM OS seam, G8 console, G9 undo | Not built |

**Rules that apply to every slice here:**

- **Spec before code.** Turn this PRD into `docs/access-governance-spec.md` and send it to the reviewer session before any code.
- **The security gate:** no open High or Critical finding before a slice is called done.
- **Fake data only.** The file key driver refuses to start unless `GOV_DATA_CLASS=fake`.
- **One slice per agent, worktree and branch.** The conductor merges with `--no-ff`, re-runs every workspace check, and never merges a pull request.
- **No weakened checks.** Never skip or weaken a test, lint or accessibility rule. Every control has an attack test that fails without it.
- **Keys and dependencies.** Every table key starts with `(tenant, env)`. No `gov-*` crate depends on a `pmos-*` crate, except where the spec already allows it.
- **No free text in signed records.** Receipts and other signed records carry identifiers, digests and closed reason codes only (T17).

## Users and jobs

Five human roles and one system actor use this work. The reviewer agent is governed like any other agent.

| Who | Job | Uses |
| --- | --- | --- |
| Owner (a PM who owns agents) | Know what their agents did and why; certify their agents' access each quarter | Graph explorer, campaign screen |
| Approver | Approve or deny one exact action, seeing the reviewer agent's advice | Review queue |
| Security certifier | Second sign-off on high-risk access; certify the reviewer agent's own scope | Campaign screen |
| Auditor | Prove who acted, on whose behalf and why, and that reviews happened; metadata only | Graph explorer, evidence export |
| Admin | Run campaigns, set cadence and thresholds, restore a wrongly suspended agent | Campaign settings |
| Reviewer agent (system) | Recommend or approve bounded actions; find risky standing access; narrow it, never widen | Its own mandate and key, and the Evidence interface |

## Architecture

The gate stays the only writer of evidence. Everything new reads from the Ledger's projection, and the reviewer agent's outputs pass back through the gate or the Authority, never straight to a system.

![Architecture: gate, Ledger, context graph, reviewer agent, access reviews](design/access-governance-architecture.png)

The reviewer agent can approve only bounded actions at the gate, and can only suspend or propose narrowing at the Authority. People approve actions and certify access every quarter.

## Requirements, context graph (CG)

Only the gate writes the graph's facts, and the graph is never read on the decision path. "MUST" and "SHOULD" carry their RFC 2119 meaning.

1. **CG-1, receipt fields.** The receipt format MUST gain:
    - `session`: the gate session id.
    - `cause`: references to earlier receipts in the same session.
    - `rule`: the id of the policy rule that matched.
    - `labels`: the input label and the session label at decision time, each a pair of closed enums.
    - Bump the format version. Unknown fields stay refused.
2. **CG-2, cause comes from the gate.** The gate MUST compute `cause` from its own session record: every earlier read in the session, the conservative rule. It MUST ignore any cause or label sent by the agent, the shim or a header, with a test proving it.
3. **CG-3, reads get receipts.** Every read through the gate MUST produce a decision or outcome receipt in every mode. The PM OS seam (G7) MUST either record reads or mark flow coverage as partial on screen.
4. **CG-4, labels come from the signed bundle.** The connector map MUST gain:
    - a confidentiality label per resource pattern;
    - an integrity label per tool result;
    - an audience clearance per sink;
    - an `outward` flag per operation, if G3 did not already add one.

    A label-like value in arguments or results is data, never a label.
5. **CG-5, session labels.** The gate MUST keep each session's label in memory as the join of everything the session read. It never goes down.
    - Confidentiality: public < internal < confidential < restricted.
    - Integrity: trusted < untrusted.
6. **CG-6, the flow check.** It runs between §5.1 step 5 (policy) and step 6 (grants).
    - It MUST ask for a countersignature or refuse, as policy says, when an outward write's audience clearance is below the session's confidentiality.
    - It MUST do the same when a destructive or outward action runs under an untrusted session.
    - New reason codes: `label_exceeds_clearance` and `untrusted_control`.
    - In shadow mode the outcome goes in `would_decide`.
7. **CG-7, declassification.** Releasing data past the session label MUST need a countersignature bound to that action's digest, of class `signed` or `attested`, never `agent`.
8. **CG-8, the projection.** The Ledger MUST project receipts into node and edge tables in the governance database.
    - The projection is idempotent and can be rebuilt from receipts.
    - It is pruned with them.
    - It is never an input to `gov verify`.
9. **CG-9, trace.** The Evidence interface (`/v1/trace`) and `gov trace` MUST answer four queries, each with bounded depth and paging:
    - why for one action;
    - everything an agent or mandate touched;
    - everything downstream of a receipt or resource;
    - everything done on one person's behalf.
10. **CG-10, the why bundle.** It MUST contain:
    - the rule and policy version;
    - the mandate chain back to the root person;
    - the countersignature and its class;
    - the causal ancestors;
    - the labels at decision time;
    - the receipt and checkpoint references.

    The model's own explanation is never part of it.
11. **CG-11, persistent labels.** Labels SHOULD persist on memory items and principals, so taint crosses sessions. This is a later slice.

## Requirements, reviewer agent approvals (RV)

The reviewer agent is an approver with its own key, mandate and limits. The gate enforces its limits, not the reviewer's own judgment. It starts in advise mode.

1. **RV-1, a new class.** `Class` in `gov-core` MUST gain `agent` beside `signed` and `attested`. A countersignature of class `agent` is valid only under the key of an agent the Authority enrolled with the `reviewer` role, following the existing rule that ties class to issuer.
2. **RV-2, policy opts in.** A rule MUST list the classes it accepts, and `agent` is accepted nowhere by default. The gate's local floor MAY forbid `agent` for any operation; the bundle can add to that floor, never remove from it.
3. **RV-3, hard limits in the gate.** These cannot be relaxed by policy. An `agent` countersignature MUST be refused when any of these holds:
    - the action's kind is `destructive`;
    - `outward` is true;
    - its reversal class is `none`;
    - it declassifies;
    - it changes policy, enrolment or rosters;
    - the requester's mandate chain includes the reviewer;
    - the requester shares the reviewer's owner.
4. **RV-4, the reviewer is governed.** The reviewer runs under its own mandate from a `gov-reviewer` template. Its grants are:
    - read the Evidence interface;
    - issue countersignatures within RV-3;
    - open findings (AR).

    Its own calls produce receipts.
5. **RV-5, deterministic checks first.** RV-3 and the policy match run before any model call. A failed check escalates to a person without calling the model.
6. **RV-6, structured input.** The model gets:
    - the canonical action;
    - the readable form, with argument values inside a quoted data envelope;
    - the why bundle draft, session labels and mandate chain;
    - how many earlier decisions exist on the same operation and resource pattern.

    It gets no raw transcript.
7. **RV-7, closed output.** Output MUST be `approve`, `escalate` or `refuse` with a reason from a closed list. Any free-text rationale stays outside signed records:
    - It is labelled as an untrusted claim.
    - Only approvers can see it.
    - It is deleted after a retention the owner sets; default 7 days.
8. **RV-8, two modes.** In `advise`, the default, the reviewer recommends and a person decides. In `approve`, the reviewer may issue `agent` countersignatures within RV-3. Each mode change is a witnessed admin record.
9. **RV-9, fail to a person.** No answer within the timeout, or any error, escalates to a person. Nothing ever defaults to approve.
10. **RV-10, model separation.** The reviewer SHOULD run on a different model from the agents it reviews. Records carry a hash of the model id, never the vendor's name, per the owner's rule of 2026-11-23.
11. **RV-11, everything recorded.** Every recommendation and every approval MUST be recorded and chained, so agreement with people can be measured (AR-9).

## Requirements, access reviews (AR)

Two loops share one evidence base. The reviewer agent narrows continuously, and people certify quarterly, starting from what it flagged. Mandates expire within hours, so the access under review is the standing access above them.

1. **AR-1, inventory.** The Authority MUST list each agent's standing access:
    - enrolment status and owner;
    - which templates the agent, and which people, may request;
    - each template's grants;
    - delegation depth;
    - approver roster entries;
    - the reviewer's own scope.
2. **AR-2, usage facts.** For each item the graph MUST supply:
    - when it was last used;
    - uses in the last 30 and 90 days;
    - the labels it touched;
    - refusals and failed proofs.

    These are recomputed at each checkpoint.
3. **AR-3, continuous findings.** The reviewer agent MUST evaluate these signals, each a closed code:
    - `unused_grant`: not used in 30 days, configurable;
    - `scope_growth`;
    - `refusal_spike`;
    - `proof_failures`;
    - `label_exposure`;
    - `orphaned_owner`;
    - `dormant_agent`;
    - `tool_drift`.

    A finding holds the code, the item, a severity from a closed list, and references to the receipts and checkpoint behind it.
4. **AR-4, what the reviewer may do.** It may open findings. It may suspend an agent through the revocation feed, but only for `orphaned_owner`, `dormant_agent` or `proof_failures`.
    - Suspensions are capped per day.
    - Each one is a witnessed record, and the owner and admin are notified.
    - An admin can restore the agent in one witnessed step.

    It may also propose narrowing as a pull request to the policy repository.
5. **AR-5, what it may never do.** Widen access, enrol agents, change rosters, merge or approve its own proposals, or act on its own access. A CI check MUST refuse any pull request from the reviewer's identity whose diff widens anything.
6. **AR-6, campaigns.** The Authority MUST open one campaign per owner on a cadence, default quarterly. Items come in this order:
    1. open findings;
    2. a random sample of unflagged items, default 10%, each reviewed one by one;
    3. the remaining items.

    Bulk-certify is allowed only on unflagged, low-risk items outside the sample.
7. **AR-7, decisions.** Each item is decided as certify, narrow or revoke, with its usage facts and findings shown beside it.
    - Each decision is a signed admin record, witnessed.
    - Narrow and revoke produce a revocation or a policy pull request.
8. **AR-8, deadlines and separation of duties.**
    - The default deadline is 14 days. An undecided item is suspended, not deleted.
    - A high-risk item needs a second certifier from the security group. High-risk means outward, destructive, irreversible, restricted-label, or the reviewer's own scope.
    - An owner can never certify the reviewer's scope.
9. **AR-9, reviewing the reviewer.** The Authority MUST measure:
    - the upheld rate: findings people agreed with;
    - the overturned rate;
    - the miss rate: sampled unflagged items that people narrowed or revoked.

    Crossing an owner-set threshold demotes the reviewer to advise mode and pauses its suspensions, as a witnessed record.
10. **AR-10, the report.** Each campaign MUST export counts, decisions, reviewer metrics and evidence references. The report is metadata only and auditors can read it.

## Requirements, console screens (UI)

Three screens join the console in G8. The graph explorer design is the visual reference for the first: its full source, with layout, sample data and interactions, is in `docs/design/graph-explorer-design.dc.html` (a Claude Design component file; read it for structure, it does not run on its own).

1. **UI-1, graph explorer.** It MUST show five columns: people, agents, mandates and approvals, actions in order, and resources.
    - Edge kinds: authority, read by label, write, blocked write, approval, each with its own show-or-hide toggle.
    - Trace why highlights everything that led to a selected node.
    - A details panel shows the why bundle, the evidence, and the model's claim marked untrusted.
    - It also shows a session-label meter and a timeline of signed actions.
2. **UI-2, review queue.** It MUST list waiting countersignature requests. Each shows:
    - the action's readable form;
    - the reviewer's recommendation and reason code;
    - the label and chain facts.

    Attested decisions happen in the console. Signed approvals link to the gate's own page, per spec §4.4.
3. **UI-3, campaign screen.** It MUST show each owner's items in three groups: findings, sample, then the rest. Each item shows:
    - its usage facts;
    - actions to certify, narrow or revoke;
    - whether a second certifier is needed;
    - its deadline.

    The screen also shows campaign progress. Bulk-certify follows AR-6.
4. **UI-4, honesty labels.**
    - Every screen MUST label observe and shadow mode "not enforced" (T16).
    - When `GOV_DATA_CLASS=fake`, a "Test stack · fake data" banner shows.
5. **UI-5, role visibility.** Auditors see metadata only. The reviewer's free text is shown to approvers only, and marked untrusted.
6. **UI-6, accessibility.**
    - Every node is a real button reachable by keyboard.
    - A table view gives the same nodes and edges for screen readers.
    - Panels stack at phone width.
7. **UI-7, quality bar.** The owner's standard from `CLAUDE.md`: the Baseten and WorkOS dashboards.

## Data model and formats

The formats are not yet released, so the receipt fields change now, in one version bump. Every new record follows the existing envelope, type-tag and closed-list rules.

**Receipt, version 2 additions:**

```
session            gate session id
cause              [ { gate, seq, hash } ]   earlier receipts in this session, computed by the gate
rule               id of the matching rule   absent when no rule matched
labels             { input: { conf, integ }, session: { conf, integ } }
decision.reason    + label_exceeds_clearance, untrusted_control
countersign.class  + agent
```

**Signed bundle additions:** a `conf` label per resource pattern, an `integ` label per tool result, an `audience` clearance per sink, and `outward` per operation. A rule may now say `countersign(classes: [attested, agent], group)` and `declassify: true`.

**New signed records,** chained by the Authority and witnessed:

| Type tag | Holds |
| --- | --- |
| `gov.finding.v1` | Code, item, severity, receipt references, checkpoint, status |
| `gov.suspension.v1` | Agent, finding, the reviewer key that issued it |
| `gov.recommendation.v1` | Request, action digest, approve, escalate or refuse, reason code, model hash |
| `gov.campaign.v1` | Owner, opening date, due date, sample rate, item count |
| `gov.certification.v1` | Campaign, item, certify, narrow or revoke, certifier, second certifier |
| `gov.mode.v1` | Reviewer, old and new mode, cause: threshold breach or admin |

**New tables** in the governance database, each keyed first by `(tenant, env)`:

| Table | Holds | Rebuildable from receipts |
| --- | --- | --- |
| `graph_nodes` | Id, kind, properties, first and last seen | Yes |
| `graph_edges` | Source, target, kind, receipt reference, event time, recorded time | Yes |
| `usage_facts` | Item, last used, 30 and 90 day counts, labels, refusals, failed proofs, checkpoint | Yes |
| `findings`, `recommendations` | Index over their signed records | Yes, from records |
| `campaigns`, `campaign_items`, `certifications` | Index over their signed records | Yes, from records |

## Threats and controls

This work adds eleven threats, numbered after the spec's T26. Each control needs an attack test that fails without it, in the slice named.

| # | Threat | Control | Slice |
| --- | --- | --- | --- |
| T27 | Text in the arguments it reviews turns the reviewer agent | Deterministic checks first; values in a quoted data envelope; closed output; hard limits in the gate; a different model; advise by default | RV2, RV3 |
| T28 | The reviewer approves for its own chain or a same-owner agent | The gate refuses `agent` class when the chain includes the reviewer or shares its owner | RV1 |
| T29 | Cause links or labels are forged | The gate computes cause; labels come only from the signed bundle; a label hidden in arguments has no effect | CG1 |
| T30 | The graph is taken as evidence | Graph is a rebuildable projection; `gov verify` never reads it; screens cite receipts and checkpoints | CG2 |
| T31 | The graph becomes a store of sensitive data | Identifiers and labels only; receipt retention; auditors see metadata; graph queries logged | CG2 |
| T32 | The reviewer suspends agents en masse | Three finding codes only; a daily cap; one-step admin restore; demotion on threshold | AR3 |
| T33 | The reviewer widens access through a policy pull request | CI refuses any widening diff from its identity; it never merges its own | AR3 |
| T34 | People rubber-stamp campaigns | Findings first; sample reviewed one by one; bulk only for low-risk unflagged items; no answer suspends | AR4 |
| T35 | Over-taint blocks legitimate work | Declassification by signed or attested countersignature; over-taint rate measured | CG3 |
| T36 | An owner certifies their own high-risk access | Second certifier from security; owners never certify the reviewer's scope | AR4 |
| T37 | Stale usage facts hide use or disuse | Facts carry their checkpoint; findings cite receipts; recomputed at each checkpoint | AR1 |

## Slices and acceptance criteria

Fourteen slices, built in the order listed. CG0 goes first and now, because it changes formats that are not yet released. Anything that blocks or approves waits for G6.

| Slice | Builds | Depends on | Done when |
| --- | --- | --- | --- |
| CG0 | Receipt v2 fields, new reason codes, `agent` class, label fields in the bundle; no behaviour change | Nothing | Both versions verify; unknown fields refused; property tests pass |
| CG1 | Read receipts, gate-computed cause, session labels in memory, in observe mode | CG0, G3 | A forged label or cause in arguments or headers has no effect; labels rise exactly as the fake reads dictate |
| CG2 | Projection, `/v1/trace`, `gov trace` | CG1, G2 | A drop and rebuild gives an identical graph; pruning removes graph rows; `gov verify` is unchanged with the graph deleted |
| AR1 | Standing-access inventory and usage facts | CG2, G4 | Facts match a hand count on a fixed fake history |
| AR2 | Continuous findings, with no actions taken | AR1 | Each of the eight codes fires on its fixture and stays quiet on its negative fixture |
| CG3 | The flow check in shadow, then enforce; declassification | CG1, G5, G6 | Legitimate-chain leak tasks are refused in enforce mode; their benign twins pass or ask once |
| RV1 | `agent` class checks with the RV-3 limits; core in `gov-core`, gate wiring with G6 | CG0, G6 | Every RV-3 condition has a test that fails without it; `agent` class from a non-reviewer key is refused |
| RV2 | Reviewer service in advise mode, with recommendation records | RV1, G4 | An injection set in arguments never yields approve on an out-of-bounds action; a timeout escalates |
| AR3 | Suspensions and narrowing pull requests | AR2, RV2 | The daily cap holds; only the three codes can suspend; CI refuses a widening pull request from the reviewer |
| AR4 | Campaigns, deadlines, second certifier, sampling, reviewer metrics, report | AR2, G4 | An undecided item suspends at its deadline; an owner cannot certify the reviewer's scope; sampled items cannot be bulk-certified |
| RV3 | Approve mode, behind a policy switch | RV2, AR4 | Approvals stay inside RV-3; a mode change is witnessed; crossing a threshold demotes the reviewer |
| UI1 to UI3 | Graph explorer, review queue, campaign screen | CG2, RV2, AR4, G8 | Each matches its design; keyboard and table views work; "not enforced" and "fake data" labels show |

## Test plan

Every control is proven by an attack that fails without it, on fake data, in the same style as G1 to G4.

- **Attack tests.** One per threat, T27 to T37, plus RV-3's seven conditions. Each is shown failing with its control removed, then passing with it.
- **Property tests in `gov-core`:**
  - session labels never go down;
  - an `agent` countersignature never verifies for an action outside RV-3;
  - a v2 receipt with any changed byte fails;
  - cause references always point backwards within the same session.
- **Projection tests:**
  - drop and rebuild gives the same graph;
  - pruning removes graph rows with their receipts;
  - `gov verify` gives the same answer with or without the graph.
- **Reviewer injection set.** Fifty or more argument payloads that try to talk the reviewer into approving: hidden instructions, fake labels, fake approvals, look-alike characters. None may produce `approve` outside RV-3. Score from the recommendation records, never from the model's text.
- **Legitimate-chain leak suite.** The 20 pilot tasks from the research plan, each with a benign twin, run on the fake Jira, Slack and web servers. Report refusals and over-taint.
- **Campaign scenarios.** Fixed fake histories for:
  - an orphaned owner;
  - an unused grant;
  - scope growth;
  - a missed deadline;
  - a reviewer that misses enough sampled items to cross the threshold.

  Check every witnessed record that each one produces.
- **Full stack.** Gate, Ledger, Authority and reviewer run as programs, on Postgres, against the fake MCP servers. CI's deploy job starts the stack.
- **Checks from `CLAUDE.md`:** fmt, clippy with `-D warnings`, the workspace tests with `PMOS_TEST_DATABASE_URL`, and the web and browser tests for UI slices.

## Success metrics

Success means leaks stop without blocking real work, and reviews get faster without getting looser. Targets are set by the owner after the pilot; until then every figure is a measurement, not a finding.

| Metric | Definition | Why it matters |
| --- | --- | --- |
| Leak refusal rate | Share of legitimate-chain leak tasks refused in enforce mode | Whether T12 now has a control |
| Over-taint rate | Share of benign twin tasks refused or sent for approval | The utility cost of labels |
| Added decision time | p50 and p99 time the flow check adds per call | The check must stay off the slow path |
| Reviewer agreement | Recommendations the person upheld, over all decided | Readiness for approve mode |
| Reviewer miss rate | Sampled unflagged items that people narrowed or revoked | What the reviewer fails to see |
| Time to revoke an orphan | From owner offboarding to suspension | The gap SCIM will later close |
| Campaign completion | Items decided by the deadline, per owner | Whether the quarterly loop runs |
| Decision time per item | Median seconds a person spends per campaign item | A falling number with a rising certify rate signals rubber-stamping |
| Graph cost | Bytes per receipt in the graph, and trace query time | Whether the projection scales |

## Open questions for the owner

These need your answer before the slice that uses them. Each one has a default the build uses until you decide.

- [ ] **Campaign cadence.** Quarterly by default. Monthly for high-risk items?
- [ ] **Sample rate.** 10% of unflagged items by default.
- [ ] **Unused-grant window.** 30 days by default.
- [ ] **Demotion thresholds.** The miss rate and overturned rate that send the reviewer back to advise mode. No default: needed before RV3.
- [ ] **Suspension limits.** May the reviewer suspend without a person confirming? Default: yes, for three codes only, capped per day, with one-step restore.
- [ ] **Reviewer model.** Which model the reviewer runs on, and whether it must differ from the agents' model.
- [ ] **Security certifier group.** Who gives the second sign-off, and who certifies the reviewer's own scope.
- [ ] **Rationale retention.** How long the reviewer's free-text rationale is kept. Default: 7 days.
- [ ] **Label names.** The four confidentiality levels as written, or your organization's own.
- [ ] **Undecided items.** Suspend by default, or notify only for low-risk items.

## Kickoff prompt for Claude Code

Save this file as `docs/prd-access-governance.md` and the `design/` folder as `docs/design/` in the repository. Then paste the prompt below into the conductor session. It asks for the spec and the reviewer's read before any code, as the repository's rules require.

```
You are the conductor for PM-Operating-System. Read CLAUDE.md and
docs/agent-governance-spec.md first, then docs/prd-access-governance.md.
Look at docs/design/access-governance-architecture.png and read
docs/design/graph-explorer-design.dc.html for the UI reference.

Task 1: turn the PRD into docs/access-governance-spec.md, in the style of
agent-governance-spec.md:
- a status note at the top: written, not yet read by the reviewer, nothing built;
- requirements CG-1..11, RV-1..11, AR-1..10 and UI-1..7, stating any
  departure from the PRD and why;
- threats T27..T37 added to the threat model, each with its slice and attack test;
- the format changes: receipt v2, the new signed records, the new tables;
- the slices with their done-when criteria;
- the open questions for the owner, with the defaults the build will use.

Task 2: send the spec to the independent reviewer session. Write no code
until its gate passes on the spec (no open High or Critical).

Task 3: once it passes, build slice CG0 only, on its own branch and
worktree, with property and attack tests, then send the commit to the
reviewer.

Follow every standing rule in CLAUDE.md: fake data only, no skipped or
weakened checks, never merge a pull request, and report what was verified
and what was not.
```
