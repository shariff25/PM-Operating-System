# Agent access governance: context graph, reviewer agent, access reviews

An extension of `docs/agent-governance-spec.md` ("the main spec"). Everything there holds
here: the threat model T1 to T26, the signed documents, the gate's order of checks, fake
data only, and a control is not built until removing it fails a test.

## Status (2026-11-27)

- **Written from the owner's PRD of 2026-11-26** ("Agent access governance"), which the
  owner pasted into the conductor's session with a design for the graph explorer. Asked to
  check it, the conductor found four requirements that do not work as written; the owner's
  answer was "You assess where it make sense". This spec is that assessment: the PRD's
  requirements, the departures from it (§2), and where the work sits among the slices
  still open (§8).
- **Not yet read by the reviewer. Nothing is built.**
- The PRD's text, its architecture image and the design's source file are **not in the
  repository**: they exist in the owner's session only. Requirement numbers (CG, RV, AR,
  UI) are the PRD's, so the two can be laid side by side when the owner commits it.
- The PRD's table of what is built was out of date. As of this writing: G1 to G4b are
  built and have passed the reviewer's gate (G3 in observe mode); the design of G4c is
  cleared; G4c to G9 are not built. The gate blocks nothing.

## 1. What this adds

1. **Context graph.** Receipts say which session an action was in, which rule matched and
   what the session had read by then. The Ledger projects receipts into a graph that
   answers who acted, for whom, on what, where, when and why. A label that rises with
   everything read lets the gate refuse a sequence of individually allowed actions that
   would carry data to an audience not cleared for it. This is the first control for
   threat T12 (an agent turned by injected text, acting inside its mandate).
2. **Reviewer agent approvals.** A third countersignature class, `agent`, with which an
   enrolled reviewer agent may approve a narrow class of reversible, internal actions. It
   starts by advising a person.
3. **Access reviews.** The reviewer agent finds risky standing access continuously and may
   only narrow it. People certify all standing access each quarter, starting from what it
   flagged and a random sample of the rest.
4. **Console screens.** A graph explorer, a review queue and a campaign screen.

Never in scope: approval "because similar cases were approved"; the reviewer agent
approving an outward, irreversible or declassifying action; real data.

## 2. Departures from the PRD

Each is a place where the PRD, built as written, would not do what it says.

- **D1. The label travels with the agent; a gate's memory is not enough** (PRD CG-5). A
  session is with one gate. The PRD's own example reads at three gates and posts at a
  fourth, which never saw the reads. So the label is carried: each gate answers a call
  with a **label token**, a document signed by that gate (`gov.label.v1`: the root
  mandate's id, the label, a number that rises, the time), and the shim presents the
  newest token it holds with every call, to any gate. A gate takes the join of its own
  record and the token, and a token is accepted only from a gate key enrolled in the
  bundle. An agent that withholds a token keeps a lower label at the next gate: that is
  the stated limit of §9, and the reason gates also report labels to the Authority with
  the lease (below).
- **D2. The label belongs to the root mandate, not to the session** (PRD CG-5, CG-11). A
  session ends on a gate restart or by expiry, and a new one would start at "public" while
  the agent still holds what it read. So the label is kept per **root mandate id**, for the
  mandate's life, on disk beside the spool, and recovered from the gate's own receipts
  after a restart. A new mandate for the same agent still starts clean, so the PRD's
  CG-11 (labels that persist beyond a session, "a later slice") is moved up: before the
  flow check may enforce (slice CG3), the Authority issues a mandate carrying the agent's
  **standing label**, the join of what gates reported for that agent's mandates over a
  window policy sets. Until that is built the flow check may run in shadow mode only.
- **D3. Everything under one root mandate shares one label** (not in the PRD). The gate
  cannot see what one agent passes to another. In the PRD's example a sub-agent fetches an
  untrusted page and the parent's label drops; that happens only if a child's reads raise
  the label of the whole chain. So the label's key is the root mandate, and a delegated
  mandate's calls read and raise the same one.
- **D4. Suspension is not written to the revocation feed** (PRD AR-4). The feed only adds,
  so a suspension in it could not be undone (review finding GR-140, main spec §7.6.2,
  point 6). The reviewer agent suspends by asking the Authority to suspend the agent,
  which revokes the agent's mandates; an admin's restore resumes the agent, which is then
  issued new mandates.
- **D5. Cause is one reference, not a list** (PRD CG-1, CG-2). "Every earlier read in the
  session" on every receipt grows with the square of a session's length. A receipt carries
  the gate, number and hash of the **previous receipt of the same root mandate at this
  gate**, and the digest of the label token it was shown. The projection walks back along
  these; the graph is the same, and a receipt stays one size.
- **D6. One format version, changed in place** (PRD CG-1, slice CG0's "both versions
  verify"). Nothing is released and the code refuses an unknown version. The receipt
  format changes once, in place, as the PRD's own data-model section says.
- **D7. Slice CG0 is cut in two.** The receipt and bundle fields are added early (CG0a).
  The `agent` class waits (CG0b, with RV1): it sits on the approval design, whose open
  review conditions (GR-8a, GR-29 to GR-32) must be closed before G6 builds any approval.
- **D8. The reviewer agent holds no admin power of its own.** Opening a finding, asking
  for a suspension and proposing a narrowing are requests to the Authority, signed with
  the reviewer's enrolled key and checked there against its template; they are recorded
  as admin records whose actor is the agent. Nothing lets an agent's key do what the main
  spec reserves to a signed-in person (§7.6, choice 8).

## 3. Context graph (CG)

- **CG-1, receipt fields** (changed by D5, D6). A receipt gains: `session`; `cause` (the
  previous receipt of the same root mandate at this gate, and the digest of the label
  token presented); `rule` (the id of the rule that decided, absent when none matched);
  `labels` (the label of what this call reads or writes, and the mandate's label after
  it), each a pair of closed lists. Unknown fields stay refused.
- **CG-2, the gate computes them.** Cause and labels come from the gate's own record and
  from tokens signed by enrolled gates. Anything of the kind sent by the agent, the shim
  or a header, other than a valid token, has no effect, and a test shows it.
- **CG-3, reads get receipts.** They already do in observe mode. The PM OS seam (G7)
  records reads or shows flow cover as partial.
- **CG-4, labels come from the signed bundle.** The connector map gains a confidentiality
  label per resource pattern, an integrity label per tool's results, and an audience
  clearance per resource pattern written to. `outward` is already there. A value that
  looks like a label in arguments or results is data.
- **CG-5, the mandate's label** (changed by D1 to D3). Confidentiality: public, internal,
  confidential, restricted. Integrity: trusted, untrusted. The label is the join of
  everything read under the root mandate, at any gate whose token was presented. It never
  goes down.
- **CG-6, the flow check**, between policy and grants in the main spec's §5.1. It asks
  for a countersignature or refuses, as policy says, when an outward write's audience
  clearance is below the label's confidentiality, or when a destructive or outward action
  runs under an untrusted label. New reasons: `label_exceeds_clearance`,
  `untrusted_control`. In shadow mode the answer goes in `would_decide`.
- **CG-7, declassifying** needs a countersignature bound to the action's digest, of class
  `signed` or `attested`, never `agent`.
- **CG-8, the projection.** The Ledger projects receipts into node and edge tables. It is
  idempotent, rebuildable from receipts, pruned with them, and never read by `gov verify`.
- **CG-9, trace.** `/v1/trace` and `gov trace` answer four questions with bounded depth
  and paging: why one action; everything an agent or mandate touched; everything
  downstream of a receipt or resource; everything done for one person.
- **CG-10, the "why" of an action:** the rule and bundle; the mandate chain to the root
  person; the countersignature and its class; the earlier receipts; the labels; the
  receipt and checkpoint. Never the model's own account.
- **CG-11, the standing label** (moved up by D2): required before CG3 enforces.

## 4. Reviewer agent approvals (RV)

RV-1 to RV-11 as the PRD has them, with D7 and D8, and these made exact:

- **RV-1.** `agent` is a third class. It verifies only under a key enrolled for an agent
  whose template is `gov-reviewer` in the bundle the gate holds (the template check of the
  main spec's §7.6.2, point 10, does this).
- **RV-2.** A rule lists the classes it accepts; `agent` nowhere by default. A gate's own
  floor may forbid it, and the bundle can add to the floor, never take from it.
- **RV-3, limits no policy can lift.** An `agent` countersignature is refused when the
  action is destructive, is outward, has reversal class `none`, declassifies, or changes
  policy, enrolment or rosters; when the requester's chain includes the reviewer; when the
  requester's root person is the reviewer's owner; **and when the mandate's label is
  untrusted** (added here: an action shaped by injected text is the case a second model
  is worst placed to judge, and the PRD's own design shows one approved).
- **RV-4 to RV-11:** governed by its own mandate and receipts; deterministic checks before
  any model call; structured input with argument values in a quoted envelope and no
  transcript; a closed output (`approve`, `escalate`, `refuse`, a reason from a closed
  list), with any free text kept outside signed records, marked untrusted, shown to
  approvers only and deleted after a time the owner sets; two modes, `advise` by default,
  each change a witnessed admin record; no answer or an error goes to a person; a
  different model from the agents it reviews, recorded as a hash of the model id; every
  recommendation recorded and chained.

## 5. Access reviews (AR)

AR-1 to AR-10 as the PRD has them, with D4 and D8. What "standing access" is, in this
build: an agent's registration, status and owner; the templates in the bundle and who may
ask for each; each template's grants and whether it may be delegated; the approver roster;
the reviewer's own template. Usage facts are recomputed at each checkpoint and carry it.
The eight finding codes are a closed list: `unused_grant`, `scope_growth`,
`refusal_spike`, `proof_failures`, `label_exposure`, `orphaned_owner`, `dormant_agent`,
`tool_drift`. Only three may lead to a suspension the reviewer asks for (`orphaned_owner`,
`dormant_agent`, `proof_failures`), capped per day, each a witnessed record, restored by
an admin in one step. The reviewer may never widen access, enrol, change a roster, merge
or approve its own proposal, or act on its own access; CI refuses a pull request from its
identity whose diff widens anything. Campaigns, deadlines (an undecided item is suspended,
not deleted), the second certifier for high-risk items, and the measures of the reviewer
(upheld, overturned, missed) with demotion to `advise` on a threshold, are as the PRD has
them.

## 6. Console screens (UI)

UI-1 to UI-7 as the PRD has them, built with G8. One correction to the design: it shows
labels crossing gates and a sub-agent's fetch lowering its parent's integrity, which is
true only under D1 and D3; the screens say which gates' reads a label covers, and mark a
label "partial" when a gate reported reads the token did not carry.

## 7. Threats added

T27 to T37 as the PRD lists them, each with an attack test in the slice named there, and
four the departures bring:

| # | Threat | Control | Slice |
|---|---|---|---|
| T38 | An agent sheds its label by opening a new session, or after a gate restart | The label is per root mandate, on disk, recovered from the gate's receipts (D2) | CG1 |
| T39 | An agent sheds its label by getting a new mandate | The standing label at issue (CG-11); until then the flow check does not enforce | CG3 |
| T40 | An agent withholds or replays an old label token to keep a low label at another gate | Tokens carry a rising number per root mandate; gates report labels to the Authority, which puts the newest number in the lease, and a gate refuses an older token in enforce mode | CG1, CG3 |
| T41 | A forged label token lowers or raises a label | Signed by an enrolled gate key, with its own type tag; a token can only raise; one for another root mandate is refused | CG1 |

## 8. Where this sits

The owner left the order to the conductor. The Authority comes first: G4c is cleared and
its formats are in flight, and almost everything here needs mandates from the Authority
(root mandate ids, templates, owners) or enforce mode.

| Order | Slice | Builds | Needs |
|---|---|---|---|
| now | this spec | read by the reviewer while G4c is built | |
| 1 | G4c | mandates from the Authority (main spec) | cleared |
| 2 | CG0a | receipt fields and bundle label fields; no behaviour changes | this spec through the gate |
| 3 | G4d, G4e | lease, clock, revocation, sign-in (main spec) | |
| 4 | CG1 | labels per root mandate, label tokens, cause, in observe mode | CG0a, G4d |
| 5 | CG2 | the projection, `/v1/trace`, `gov trace` | CG1 |
| 6 | AR1, AR2 | inventory, usage facts, findings with no action taken | CG2, G4e |
| 7 | G5, G6 | shadow, then enforce (main spec) | |
| 8 | CG3 | the flow check in shadow, the standing label, then enforce; declassifying | CG1, G5, G6 |
| 9 | CG0b, RV1, RV2 | the `agent` class and its limits; the reviewer in advise mode | G6 |
| 10 | AR3, AR4 | suspensions, narrowing proposals, campaigns | AR2, RV2 |
| 11 | RV3 | approve mode, behind a policy switch | RV2, AR4 |
| 12 | UI1 to UI3 | the three screens | G8 |

"Done when" for each slice is the PRD's, with: CG1 also done only when a restart, a new
session and a withheld token each leave the label where it was or are recorded as
partial; CG3 only when a new mandate does not clear the label.

## 9. Stated limits

- A label is as fine as the connector map's patterns. One confidential read marks the
  whole chain of mandates, so most working sessions will carry a high label and many
  outward writes will ask for a person. The over-taint rate is measured, and is expected
  to be high until labels can be set per document.
- The gate sees calls, not what a model does with what it read. Data that leaves by a
  path with no gate is not seen.
- In observe and shadow modes a withheld label token costs the agent nothing; the
  receipts show which gates' reads a label covered.
- The graph is a convenience. Only receipts and checkpoints are evidence.

## 10. Open, for the owner

The PRD's ten questions stand, with its defaults used until answered: campaign cadence
(quarterly), sample rate (10%), unused-grant window (30 days), the thresholds that demote
the reviewer (no default; needed before RV3), whether the reviewer may suspend without a
person (yes, three codes, capped, one-step restore), the reviewer's model, who the
security certifiers are, how long the reviewer's free text is kept (7 days), the label
names, and what happens to undecided items (suspended). Added here: the window over which
a standing label is kept (CG-11), and whether the PRD and its design source are to be
committed beside this spec.
