# Agent Governance: mandates, a gate and a ledger

Status: **slices G1 and G2 built before review** (see "Status of the build" below); nothing
else is built. Revision r1, 2026-11-26, written from `claude/web-app` at `c125014`. The independent
security reviewer has not read this spec or G1. The owner, 2026-11-26, after the spec was
pushed: "lets start building G1", and then "Go" for G2. The gate (no open High or Critical) still applies before
anything here touches real data.

Source of the requirement: the owner's architecture proposal of 2026-11-26, the published page
"Mandate Architecture" ("An agent never holds a key. It holds a mandate."), and the owner's
"lets start building" the same day. The proposal itself says: "This needs a written threat model
and the independent security review before any of it is built." This document is that threat
model and the build spec. Where it departs from the proposal it says so and why (§2).

Line numbers are not cited; file paths are. Where this spec is unsure of a fact it says "to be
confirmed". Mandate, gate, receipt, witness, Authority and Ledger are working names.

## Status of the build (2026-11-26)

**G1 built before review (owner's instruction of 2026-11-26)**, on branch `build/gov-g1`: the
crate `crates/gov-core`. It reads no file, clock or network and decides nothing about a real
system. 39 tests. Ten controls were each broken on purpose and the tests re-run: all ten are
caught (one, a receipt that skips a number but links to the right hash, was missed at first
and has a test now).

- Built: the envelope and its type tags; identifiers, resource paths and patterns, digests;
  RFC 8785 canonical JSON; the canonical action and its digest; grants and "inside"; the
  mandate and its chain check; the request proof and session counter; the countersignature
  and its binding checks; the receipt, its rules and the chain link; the lease, freshness and
  the gate's clock; the checkpoint; the policy bundle's rules and their evaluation; the
  dependency rule as a test.
- Threats with a test in G1: T1, T2, T3, T4, T5 (the digest and the request; the gate's
  recompute before sending is G6), T7 (binding and expiry; single use is G6), T8 (an attested
  approval never passes for a signed one; the passkey check itself is G6 and until then
  nothing can satisfy "signed"), T10, T11, T17, T19, T20.
- Confirmed (§12): Ed25519 through `aws-lc-rs` signs and verifies as used here. No RFC 8785
  crate was added; a small implementation is in `gov-core/src/jcs.rs`, tested on the RFC's
  number vectors, its sorting and serialisation examples, and 200,000 doubles read back.
- Two faults the RFC's vectors found while building, both fixed: Rust's shortest number form
  rounds an exact tie upwards where the RFC rounds to even; and `serde_json`'s default number
  parser reads some decimals one unit off.

Where the build differs from the text below, or chose the more refusing reading:
- **`serde_json`'s `float_roundtrip` feature is on** for `gov-core`. Cargo unifies features
  across the workspace, so PM OS's crates now parse decimal numbers exactly as well. The
  whole workspace's tests were re-run with it on. It can change PM OS's `args_hash` for an
  argument holding one of the rare decimals the old parser read wrongly.
- A verifier checks the signature first and the tenant and environment next (§4.1 says tenant
  first; nothing in a payload is read before its signature holds).
- "Depth is limited to 3" (§4.2) is built as: at most four mandates in a chain, a root and
  three delegations.
- Digests carry a type tag (`gov.args.v1`, `gov.action.v1`, `gov.countersign.challenge.v1`).
- An integer beyond ±2^53 in an action's arguments is refused, not digested.
- A pattern's `/*` stands for one or more further segments; it does not match its own prefix.
- A resource must begin with its system; so must every pattern in a grant.
- A mandate that sets `requires.sandbox` is refused (no adapter can confirm it yet).
- A rule's effect is written `then:`; a rule that allows or asks for a countersignature must
  name a system; every countersignature a matching rule asks for is required.
- Every payload refuses an unknown field.
- A checkpoint carries its own rising number, so a missing checkpoint shows.
- Not in G1: the connector map, templates and the approver roster in the bundle (G3, G4, G6);
  the revocation feed's document (G4).
- Added on 2026-11-26 for undo (§4.7): `reversal`, `before_digest` and `reverses` on a
  receipt, `reversible` in a rule's match, and `evaluate` takes the operation's reversal class.

**G2 built before review (owner's "Go" of 2026-11-26)**, on branch `build/gov-g2`: the crates
`crates/gov-ledger` (a library) and `crates/gov-cli` (the `gov` command). 10 tests, each of
the Ledger's run against a store in memory and against Postgres. Thirteen controls were broken
on purpose, one at a time, and every one is caught.

- Built: intake (a receipt must be signed by an enrolled gate key, name that key's gate, keep
  its own rules and follow the chain head exactly; a resent receipt is accepted as already
  held; everything else is refused and the refusal recorded); a chain head per gate; signed
  checkpoints over every head, numbered, written to the witness on each call; the directory
  witness, which never rewrites a file; pruning with a signed record; the export; and
  verification, as a pure function and as `gov verify`, which needs only files.
- What `gov verify` reports: a receipt that does not verify; a chain broken by a removed,
  reordered, repeated or replaced receipt; a checkpoint that does not verify or is missing; a
  checkpoint that covers a receipt the export lacks, or names another hash for it; a later
  checkpoint with a shorter chain; a Ledger that stopped publishing; a gate whose chain has
  not moved for longer than the caller allows. Its verdict says, in words, that it does not
  show a gate recorded everything it did (T9).
- Threats with a test in G2: T10 (all of it), T19 (no row is read or written without the
  tenant and environment, on both stores), and the detection half of T9 that needs no
  reconciliation (heartbeats, numbered chains).
- Agent Governance's own database: tests use `GOV_TEST_DATABASE_URL`, a database and role
  separate from PM OS's; CI creates them.

Where G2 differs from the text below, or chose the more refusing reading:
- **No network listener yet.** G2 is the library and the command. The endpoint gates ship
  receipts to is built with the gate (G3), when there is something to call it.
- **Nothing is pruned before a checkpoint covers it,** so the witness always has the hash of
  the last receipt deleted. §6.3 did not say this.
- A fork or a gap signed by a real gate is kept whole in the refusals, as evidence; a receipt
  whose signature did not hold is kept only as a hash.
- A checkpoint stored but not written to the witness (a crash in between) is written before
  the next one is made.
- The witness is strict: its checkpoints must start at 1 with no gap. Retiring old
  checkpoints from a witness is not designed.
- The trusted keys given to `gov verify` are a file the customer keeps. Until the Authority
  exists (G4) nothing issues that file; the Ledger can print the gate keys it holds.
- An "inclusion proof" (§7.3, Evidence) is not built; `gov verify` checks whole chains.
- The directory witness sets each file read-only. That is a guard against accident, not
  against whoever controls the directory; object storage with a lock is later work.

## Security review of 2026-11-26 (first read; the gate did not pass)

An independent reviewer session read this spec, G1 and G2 at `2d08d15`, read-only, and ran
probes against the built crates. **Verdict: the gate did not pass** for the spec, for G1 or for
G2: four findings of High severity, no Critical. Nothing here touches a real system. The fixes
below are on branch `build/gov-review-1`; **the reviewer has not yet re-read them, so the gate
is still not passed.**

| Finding | Severity | What was wrong | What was done |
|---|---|---|---|
| GR-1 | High | A lease carried no nonce and an older lease could replace a newer one: a replayed lease turned a gate's clock and its revocations back | Fixed. A lease answers one request (a nonce); its age counts from when the request left; a lease whose Authority time or revocation number is lower than the held one is refused |
| GR-2 | High | The claim that a compromised Authority cannot forge a **signed** approval was wrong: the Authority serves the approval page, so it can show action A and have the passkey sign action B | **Design changed, not built** (owner, 2026-11-26: "Go" on the conductor's recommendation). For a signed approval the **gate** serves the page, signs the person in and checks the passkey; the Authority only tells the person where to go (§4.4). The reviewer has not yet read the new design. Nothing of it is built; it is slice G6 |
| GR-3 | High | `gov verify` believed the Ledger's prune record on its key alone: an operator could delete every receipt, sign a record, and get "OK" | Fixed as far as a witness can: a record past what the witness covered, or whose hash ties to no checkpoint and no receipt, is a finding; `--retention-days` reports receipts pruned too soon after first being covered; the report always says what the Ledger claims to have deleted. An erasure that waits out the retention is, by design, a prune |
| GR-4 | High | `gov verify` said OK with no witness cover: an empty witness, a gate left out of every checkpoint, a gate dropped from the export | Fixed: each is a finding, as is a receipt older than the latest checkpoint that none covers |
| GR-5 | Medium | A revoked agent key kept its mandates; a revoked Authority key was not checked on a countersignature | Fixed |
| GR-6 | Medium | A small-order public key, for which anyone can forge, was accepted | Fixed: refused when parsed (the known encodings) and at enrolment (a check that needs no list); one key cannot be enrolled under two ids |
| GR-7 | Medium | "Refuse `X/*`" left `X` itself allowed | Fixed: a refusal's `X/*` covers `X`. A grant's and an allow's do not |
| GR-8 | Medium | (a) the approver's groups are the Authority's word even for "signed"; (b) "the issuer may not approve" could not be checked by the gate; (c) class and issue time were not in what a passkey signs | (b) and (c) fixed: a rule says `issuer_may_approve` (default no) and the gate compares. (a) is for G6: groups for "signed" come from the signed roster |
| GR-9 | Medium | Pruning went by the time the gate wrote in a receipt | Fixed: by the Ledger's own intake time |
| GR-10 | Medium | A gate gets no proof the Ledger kept what it shipped; checkpoint times are the Ledger's own | For G3: the Ledger returns a signed head on intake and the gate keeps its spool until a witnessed checkpoint covers it. `gov verify` now defaults `--now` to the clock. Using the witness store's own write times waits for the object-storage witness |
| GR-11 to GR-17, GR-19, GR-21 | Low | Unchecked arithmetic; look-alike witness file names and repeated numbers; a short file after a crash; pruning before the witness had the checkpoint; a gate listed twice in an export; an enforced allow with no mandate; a session not tied to a key; an action read from the wire without its rule; the dependency rule read from manifests | Fixed, each with a test |
| GR-18 | Low | Limits are per mandate, so children multiply a parent's limit | For G6: the gate charges an action to every mandate in the chain (§4.2) |
| GR-20 | Low | Anyone who reaches intake can fill the refusals table; a gate key cannot be retired | For G3, with the listener |

Each fix has a test, and each test was shown to fail with its fix taken out. Also noted by
the reviewer, for the slices named: the gate must send the arguments it digested,
re-serialised, never the bytes it received (G6); the gate's monotonic clock must count time
while suspended (G6); resource identifiers are case-sensitive, so a connector for a system
that is not must fold case (G3); "the old Authority key signs the new one" has no recovery if
the old key is stolen (open, §11).

## 1. What is being built

Agent Governance is a second product, separate from PM OS. It has three parts.

- **Authority.** Keeps the agent registry and the policy versions; issues mandates; routes
  approvals and issues countersignatures; publishes revocations and leases. It is never in the
  path of a call.
- **Gate.** The only holder of a system's credential. It checks the mandate and the agent's
  key, decides from a signed local copy of policy, runs the action and signs a receipt.
- **Ledger.** Takes in signed receipts, keeps a hash chain per gate, publishes checkpoints to a
  witness store the customer controls, and answers trace and replay questions.

A **mandate** is a signed statement of what one agent may do, on which resources, until a set
time. A **countersignature** is a person's approval of one exact action. A **receipt** is the
gate's signed record of one decision and its outcome.

Three ways to install it, unchanged from the proposal: PM OS only (no Governance code runs);
Agent Governance only (no PM OS service, database or content is installed); both, where PM OS's
tool server loads the gate as a library and the two products talk only through Governance's
public interfaces.

### 1.1 The first release

The proposal's first release has twelve integrations. This spec cuts it (owner's go-ahead of
2026-11-26 on the conductor's recommended plan; the owner named no other cut):

| In the first release | Left for later |
|---|---|
| Self-hosted, one tenant, one host | Hosted cells for many tenants |
| The MCP gate, and the gate as a Rust library | The HTTP gate driven by OpenAPI |
| One system: Jira, on the fake first | GitHub (second), Confluence, Slack, internal APIs |
| Entra sign-in for people | SCIM; other identity providers |
| A key pair per agent, enrolled by an admin | Workload identity (Kubernetes, SPIFFE) |
| A sealed credential store in the gate | Outside secret stores |
| Signing keys in files for a test stack; a key-service interface with one file-backed driver | A key service or hardware module driver (required before any real data, §9) |
| Witness: a directory, then object storage with write-once lock | |
| Policy as files in Git, signed on release | |
| The agent shim (§5.3); a TypeScript client for PM OS's harness only if slice G7 needs it | SDKs in TypeScript and Python as products |
| Admin interface and command line; console screens last (G8) | Slack approvals; the log-platform stream; licensing; runtime adapters |

The tenant and the environment are in every signed document and every table key from the first
slice, so hosting many tenants later changes no format.

Everything runs on fake data until the security gate passes (no open High or Critical) and the
owner says otherwise. Nothing in this spec enforces against a real system.

## 2. Where this spec departs from the proposal

Six places where the proposal contradicts itself or leaves a hole, and the rule this spec
adopts. Each is a decision the reviewer should challenge.

1. **Who signs an approval.** The proposal has the Authority sign for a signed-in approver in
   the first release, which lets whoever controls the Authority approve anything. This spec has
   two classes of countersignature (§4.4): **signed**, where the approver's own passkey signs the
   exact action and the gate checks that signature itself; and **attested**, where the Authority
   or PM OS vouches for a signed-in person. (Review finding GR-2: an approval page served by
   the Authority would let a compromised Authority show one action and collect a signature on
   another. So for a signed approval the gate serves the page; §4.4.) Policy says which class an action needs. A receipt
   records which was used. Approvals from a Slack card can only ever be attested.
2. **What the approver sees.** A person cannot approve a digest. In the self-hosted first
   release the gate sends the Authority the action's readable form for the approval screen,
   held only until the decision and a short time after (§4.4). Receipts still carry no argument
   values. A hosted Authority must not ship until the approval screen can be served without the
   values reaching the host; that design is not in this spec.
3. **Gates and shared state.** Single use of a countersignature and per-agent budgets need state.
   Rule: a countersignature names the one gate it is for, and that gate records its use on its
   own disk. Budgets are per gate. A budget shared across gates is later work.
4. **A stale lease.** The proposal says "refuses everything", then "freshness by risk", then
   "reading carries on". One rule (§4.6): every action has a freshness class from policy; a
   gate refuses an action when its lease is older than that class allows. Nothing is exempt. In
   PM OS only governed writes pass through the gate, so PM OS's reads and drafts are untouched,
   which is what the proposal's third statement meant.
5. **An owner who leaves.** Without SCIM the Authority does not hear of a removal. First-release
   rule: a mandate is issued only inside the issuer's live signed-in session, mandates are short
   (§4.2), and an admin action "offboard this person" revokes their mandates and suspends their
   agents. The delay between the directory and Governance is stated on screen; it is closed
   when SCIM lands.
6. **"No code change" and "every request is signed".** A stock MCP client cannot sign a
   request. Rule (§5.3): the agent's runtime runs a small **shim** that holds the agent's key and
   mandate and signs each call to the gate. Pointing a stock client straight at the gate works
   only in observe mode, and the receipt then records the agent's identity as **claimed**, not
   proven.

Gaps the proposal does not cover, with the section that does: a compromised gate (§3, T9, and
§7.5); approval of an action whose target changed afterwards (§4.3, `resource_version`); a crash
between the receipt and the write (§4.5); what a receipt holds and how a chain survives deletion
(§4.5, §6.3); what a "task" is in PM OS (§8.2); an agent turned by injected text (§3, T12).

## 3. Threat model

### 3.1 What is protected

- **System credentials** held by a gate (the keys to Jira and the rest).
- **Signing keys:** the Authority's mandate, policy and lease keys; each gate's receipt key;
  each agent's key; approvers' passkeys.
- **The decision itself:** that an action runs only when policy, mandate and, where required, a
  countersignature all agree.
- **The evidence:** that the receipts are complete and unaltered.
- **The content of actions** (argument values and system answers), which may be restricted data.

### 3.2 Who is assumed hostile

- An **agent**, including a correctly enrolled one turned by injected text or a bad model
  output.
- A **holder of a stolen document:** a mandate, a countersignature, a lease or a recorded request.
- A **network attacker** between any two parts.
- A **signed-in person** acting outside their role (a PM approving their own agent's action
  where policy wants someone else; an admin widening policy unseen).
- An **operator of the Authority or the Ledger**, or someone who has taken either host or its
  database.
- An **upstream MCP server** that returns hostile content or changes its tools.
- **A stolen gate host** is in the model, but only for detection, not prevention (T9).

Out of the model for the first release: a hostile kernel or hypervisor under the gate; a
compromised identity provider; denial of service beyond per-agent budgets; side channels.

### 3.3 Threats and controls

Each row names the slice that builds the control. Each control has a test that tries the
attack; a slice is not done until that test exists and fails without the control.

| # | Threat | Control | Slice |
|---|---|---|---|
| T1 | An agent claims another agent's identity | Identity is only a signature from the enrolled key over a challenge the gate issued (§5.3). A name in a request is ignored | G1, G4 |
| T2 | A stolen mandate is used by someone else | A mandate names the agent's public key; without the private key no request verifies | G1 |
| T3 | A recorded request is replayed | Each request signs the gate's session challenge and a counter that only rises; a session belongs to one gate | G1, G6 |
| T4 | An agent widens its own mandate, or a child gets more than its parent | The Authority signs every mandate; the gate re-checks that each link in the chain is inside its parent, with a scope language where "inside" is decidable (§4.2) | G1 |
| T5 | Approved arguments are swapped before the write | The countersignature is over the action's digest; the gate recomputes the digest from the arguments it is about to send, after every transformation | G1, G6 |
| T6 | The target changed after approval (new commits, an edited page) | The canonical action carries `resource_version` where the system has one; the connector checks it at write time (§4.3) | G6 |
| T7 | A countersignature is used twice, or after it expired | It names one gate, one digest, one mandate and an expiry; the gate records its use durably before the write | G6 |
| T8 | The Authority (or its operator) forges an approval | For actions policy marks **signed**, the gate serves the approval page itself, from the bytes it will send; signs the approver in against the customer's identity provider itself; and checks the passkey itself (§4.4, after review finding GR-2). The Authority only routes the person to the gate, so a compromised Authority can delay or withhold a signed approval but cannot obtain one. An **attested** approval is the Authority's word and it can forge one; policy says which actions accept that. Not built | G6 |
| T9 | A taken-over gate acts and writes no receipt | Not preventable: the gate holds both the credential and the receipt key. Detected three ways: heartbeat checkpoints make a silent gate visible (§6.2); numbered chains show a gap; reconciliation compares the system's own record of writes with the receipts (§7.5). Receipts prove what the gate said, never by themselves what happened | G2, later |
| T10 | A receipt is altered or removed in the Ledger | Receipts are signed by the gate and chained by number; checkpoints over all chain heads go to the witness on a timer. Anyone with the public keys and the witness can verify | G2 |
| T11 | Revocation does not arrive; a gate keeps acting | A lease states the revocation sequence it covers; a gate without every revocation up to that number treats the lease as invalid. A stale lease refuses (§4.6) | G4, G6 |
| T12 | An agent turned by injected text acts inside its mandate | Not stopped by this design: inside the mandate the agent has full authority. Bounded by short, narrow mandates, by countersignatures on marked actions, and by budgets. Stated in the product | G4, G6 |
| T13 | The agent's key is read by the model | The key is held by the runtime or the shim, in a process no model-callable tool can read. The spec for each integration states where the key lives | G3, G7 |
| T14 | An upstream MCP server changes a tool, or hides an instruction in a description | The connector map (tool to canonical action) is in the signed policy bundle and pins each tool's digest; an unmapped or changed tool is refused. PM OS's MCP client layer already does this for its own records | G3 |
| T15 | The database is stolen | Signing keys are behind the key-service interface, not in tables; credentials are sealed with a key from the same interface. A test stack's file driver gives none of this and says so at startup | G4, G6 |
| T16 | Observe or shadow mode is mistaken for protection | Every receipt records its mode; the interfaces and screens label the two modes "not enforced"; no report counts them as enforcement | G3, G5 |
| T17 | Restricted content leaks through receipts | A receipt holds identifiers and digests only (§4.5). A lint on the receipt type refuses a free-text field | G1, G2 |
| T18 | Policy is changed unseen | Policy is files in Git, signed on release; the bundle's version and signer are in every receipt; publishing a version is itself a receipt | G4 |
| T19 | One tenant or environment's documents are accepted in another | Tenant and environment are in every signed document and checked first; a mandate from `test` fails in `prod` | G1 |
| T20 | The gate's clock is wrong, so expired things live on | Expiry is measured from the Authority's time in the last lease plus the gate's monotonic clock since; no wall clock is trusted alone | G4 |
| T21 | Governance on PM OS weakens PM OS | Governance can only refuse. PM OS's own grant check, approval and audit run first and unchanged; with the switch off no Governance code path runs (§8) | G7 |
| T22 | An agent floods a system through the shared credential | Per-agent budget and a fair queue in the gate | G6 |

### 3.4 Stated limits

These go in the product's own documentation, not only here.

- Custody is real only where the gate holds the only credential. The product shows, per
  system, **custody**, **in line** or **reported**, and never claims more.
- Observe and shadow protect nothing.
- A compromised gate can act unrecorded; it can be detected, not prevented (T9).
- A compromised Authority can forge an **attested** approval. It cannot obtain a **signed**
  one, because the gate serves that page and checks that signature (T8). A compromised gate
  can do anything a gate can (T9); the signed approval does not protect against it.
- An agent acting inside its mandate under injected instructions is not stopped (T12).

## 4. The signed documents

### 4.1 Common rules

- **Signature:** Ed25519. Proposed library: `aws-lc-rs`, already in the lock through
  `jsonwebtoken` (to be confirmed at G1, with the reasons for not adding a second library).
- **Envelope:** `{ "v": 1, "kid": "...", "payload": "<base64url of the exact bytes>", "sig":
  "..." }`. The signature is over a type tag and the payload bytes. A verifier checks the
  signature on the bytes it received and only then parses them. Nothing is re-serialised before
  verification, so no canonical form is needed for signing.
- **The type tag** (`gov.mandate.v1`, `gov.policy.v1`, `gov.countersign.v1`, `gov.receipt.v1`,
  `gov.lease.v1`, `gov.checkpoint.v1`) is part of the signed bytes, so one kind of document
  never verifies as another.
- **Every payload** names `tenant`, `env`, `issuer`, `issued_at`, `expires_at` (where it
  expires) and its format version. A verifier checks tenant and environment before anything
  else and refuses an unknown version.
- **Keys** are named by `kid`. The Authority publishes its public keys in the policy bundle and
  at a well-known path; a gate pins the Authority key it enrolled with and accepts a new one
  only when the old key signed it.

Considered and not chosen, to revisit if the scope language grows: Biscuit tokens (offline
attenuation, but a Datalog evaluator in the decision path) and macaroons (shared-secret, so a
verifier could mint). JSON Web Tokens were not chosen because one library reads many algorithms
and the type confusion that follows is a known class of fault.

### 4.2 Mandate

```
tenant, env, id, issued_at, expires_at
agent: { id, key }                  the public key the request must be signed with
issued_by: { kind: person|agent, id, session }
template: "<name>@<policy version>"
grants: [ { system, operations: [...], resources: [pattern, ...] } ]
limits: { max_actions?, max_writes? }
requires: { sandbox? }              honoured only with a runtime adapter (later); refused until then
parent: <the parent's full signed mandate, or absent>
```

- **Scope language.** An operation is an exact name from the connector map. A resource pattern
  is an exact path or a path ending in `/*`, matched on whole segments
  (`jira/project/PAY/*`). There are no regular expressions. "B is inside A" is then a simple
  check, and the gate can verify a chain without trusting the Authority's arithmetic.
- **Ceiling.** A mandate is a named template, narrowed. Policy sets which people and agents may
  ask for which template, and the longest life (default 8 hours; a figure to measure, not a
  finding).
- **Delegation.** In the first release a child mandate is also issued by the Authority, at the
  parent agent's signed request, and embeds the parent's whole signed mandate. The gate checks
  each link: signature, tenant, expiry inside the parent's, grants inside the parent's, not
  revoked. Depth is limited to 3. Delegation without calling the Authority is later work.
- **Limits** are charged to every mandate in the chain, so a parent's limit bounds all its
  children together (review finding GR-18; built in G6). They are counted by the gate the
  session is on. They are a brake, not an exact quota,
  when an agent uses several gates.

### 4.3 Canonical action and its digest

Every connector maps a call to one shape:

```
system            "jira"
operation         "issue.create"         from the connector map, never the tool's own wording
kind              read | write | destructive      from the connector map
resource          "jira/project/PAY/issue/PAY-12"
resource_version  the system's version of the target, where it has one
args_digest       SHA-256 over the arguments in RFC 8785 canonical JSON
```

The action's **digest** is SHA-256 over the canonical JSON of those six fields. Policy and
mandates match on `system`, `operation`, `kind` and `resource`. Approvals bind the digest.

- The connector map is part of the signed policy bundle. It pins each upstream tool's digest
  (name, description, input schema), so a changed tool is refused until someone approves it.
- The arguments digested are the ones the gate will send, after its last transformation. PM OS
  strips draft source markers before a write (`execute_write`); in the seam the digest is taken
  after that step (§8.1).
- PM OS's own `args_hash` (`crates/toolserver/src/approvals.rs`) sorts keys and uses
  `serde_json`'s number form. Whether that equals RFC 8785 for PM OS's argument shapes is to be
  confirmed in G7; until then the seam carries both hashes and never assumes they match.
- `resource_version` is filled by a read through the same connector when the countersign
  request is made, and checked by the connector at write time. Where a system has no version
  the field is absent and the coverage note for that system says so.

### 4.4 Countersignature

```
tenant, env, id, issued_at, expires_at
gate               the one gate this is for
mandate            the mandate id
action_digest
approver: { id, groups_at_signing }
class              signed | attested
evidence           for signed: the passkey assertion and credential id
                   for attested: who vouched (the Authority, or "pmos") and the session reference
```

- The gate asks by sending the Authority a **countersign request**: the canonical action, the
  mandate, and the action's readable form (system, operation, resource, and the arguments as
  the approver should see them). The Authority shows that form, computes the digest from it
  itself and refuses to show a request whose digest does not match what the gate sent. The
  readable form is deleted once the request is decided or expires, plus a short grace time
  (default one hour, to be decided with the owner).
- **Signed** (design of 2026-11-26, after review finding GR-2; replaces the paragraph's first
  version, in which the Authority served the page). The Authority tells the approver that an
  action waits and gives them the gate's address; it is never shown the arguments. The
  **gate**, which the customer runs, signs the approver in against the customer's identity
  provider, shows the action rendered from the bytes it will send, and asks their passkey to
  sign the challenge. The gate checks the passkey assertion against its own record of that
  person's enrolled passkey, takes their groups from the sign-in it just did (not from the
  Authority; finding GR-8a), and issues the countersignature under its own key. Consequences:
  a passkey is enrolled at the customer's gate address, not ours; the argument values never
  leave the customer, which also settles departure 2 of §2 for signed approvals and makes a
  hosted Authority possible for them; the gate needs an identity-provider client of its own.
  To be designed in G6: enrolling passkeys across several gates under one address, and what
  the approver is shown when the gate cannot render an argument. Proposed library:
  `webauthn-rs` (to be confirmed at G6; tests use a software authenticator).
- **Attested.** The Authority states that a signed-in person in the right group pressed
  approve. The gate verifies the Authority's signature only.
- **Who may approve** is a policy rule on groups from the sign-in token. The default rule
  refuses the mandate's issuer as approver of their own agent's marked action; a policy may
  relax that per template (PM OS's own flow, where the PM approves their own draft, needs it).
- **Single use.** The gate writes "spent" for the countersignature's id to its durable spool in
  the same write as the intent receipt (§4.5), before the action runs.

### 4.5 Receipt

```
tenant, env, gate, seq, prev        seq rises by one; prev is the hash of the gate's last receipt
at                                  Authority time plus monotonic time since the last lease
phase                               decision | intent | outcome
mode                                observe | shadow | enforce
coverage                            custody | in_line | reported
agent: { id, proof: proven | claimed }
mandate: [ids, child first]         absent in observe mode without a mandate
policy_version
action: { system, operation, kind, resource, resource_version?, args_digest, digest }
decision: allow | refuse | wait_countersign       with a reason code
would_decide                        shadow mode only: what enforce would have done
countersign: { id, class }?
outcome: { status: ok | failed | unknown, upstream_ref?, response_digest? }
```

- **No content.** A receipt has no argument value, no response body and no free text. Reason
  codes are a closed list. `resource` is an identifier and may be personal data (a Slack user
  id); retention (§6.3) is the control for that.
- **Two receipts for an action that runs.** An **intent** receipt is written to the gate's
  local disk and synced before the write is sent; an **outcome** receipt after. A refusal is
  one **decision** receipt.
- **Crash between the two.** On restart the gate writes `outcome: unknown` for every intent
  with no outcome. It never re-runs the action. The countersignature was spent at intent, so a
  retry needs a new one. Where a system accepts an idempotency key, the gate sends the action's
  digest and receipt number as that key.
- **Spool.** Receipts are shipped to the Ledger in batches. If the Ledger is away the gate
  carries on until the spool reaches a set age or size (defaults to be measured), then refuses
  writes; reads follow the same rule in enforce mode.

### 4.6 Policy bundle, lease and revocation

- **Policy** is YAML files in Git: agents' labels, templates, rules, the connector map, the
  approver roster, freshness classes. A release step checks it (a lint, and replay over stored
  receipts when there are any) and the Authority signs the bundle. The first release keeps its
  own small rule format, evaluated in memory:
  `match { agent | label, system, operation, kind, resource } -> allow | countersign(class,
  group) | refuse`, refusals win, and no match means refuse. Cedar and Open Policy Agent were
  considered; the reason for not using them yet is to keep the decision path small enough to
  review whole. To revisit when a customer needs their own policy language.
- **Lease.** A signed document `{ gate, nonce, authority_time, policy_version,
  revocation_seq, valid_for }`, renewed on a timer. The gate sends a fresh random nonce with
  each renewal and accepts only the lease that carries it; a lease's age counts from when its
  request left; and a lease whose Authority time or revocation number is lower than the one
  held is refused (review finding GR-1). A gate may decide only while it holds the bundle the lease
  names and every revocation up to `revocation_seq`.
- **Freshness classes.** Policy gives each rule a class: `strict` or `standard`. Defaults, to
  be measured before they are called findings: strict 10 seconds, standard 120 seconds. An
  action is refused when the lease's age exceeds its class. Destructive actions are strict
  unless policy says otherwise; no class is "never".
- **Revocation.** A numbered feed of revoked agent ids, mandate ids, countersignature ids and
  keys. An entry is dropped once the thing it names would have expired anyway. The longest a
  revoked agent can keep acting is the freshness class of the action it attempts.
- **Time.** A gate takes "now" as the last lease's `authority_time` plus its own monotonic
  clock since receiving it. A gate that has never held a lease refuses everything.

### 4.7 Undo (design; fields reserved, slice G9)

Owner, 2026-11-26, on the conductor's proposal: every action that passes the gate is either
one that can be put back or one a person approved. The customer's rule becomes "approve only
what cannot be undone; undo everything else". Nothing of undo is built. Four things are
reserved in the formats now, while no format is released, and tested:

- **A reversal class per operation,** in the signed connector map beside `kind`: `restore`
  (the system keeps versions, and undo writes the earlier values back), `inverse` (there is an
  opposite operation: create and archive, add and remove) or `none` (a sent message, a hard
  delete, a payment). Every receipt states it (`reversal`).
- **`before_digest` on an intent receipt:** the digest of the target's values before the
  write. The values themselves stay in the gate's sealed store at the customer for the undo
  window; the receipt and the Ledger hold only the digest.
- **`reverses` on a receipt:** the gate, number and hash of the receipt an undo puts back.
- **`reversible` in a policy rule's match,** so the default rule is one line: an operation
  with no undo needs a countersignature.

How undo will work (G9):

1. At intent, for a `restore` or `inverse` operation, the gate reads the fields the write will
   change through the same connector it already uses to read `resource_version`, seals them,
   and writes their digest in the intent receipt. If that read fails, the action is treated as
   having no undo, and policy decides whether it may still run.
2. **Undo is a governed action.** A person with the right role asks for it. It has its own
   mandate, its own intent and outcome receipts with `reverses` set, and policy may ask for a
   countersignature on it like any other write.
3. **It refuses rather than overwrite.** If the target's version is no longer the one the
   original write left, someone has changed it since; the undo stops and says so, as a version
   conflict does. Nothing is forced.
4. **A task is undone as a whole:** every action under one mandate, newest first, stopping at
   the first conflict and reporting what was and was not put back.
5. The undo window (default seven days, a figure to decide) is how long the sealed values are
   kept. After it the receipts remain and the undo is gone.
6. Each system's page in the product shows its undo cover, as it shows custody: which
   operations restore, which invert, which cannot be put back.

What undo is not, to be said in the product:

- It is a net under mistakes, not a control against a hostile agent. What an agent read
  cannot be un-read.
- It does not reach outside the system: a notification already sent, a webhook already
  fired, a person who already acted on what they saw.
- An undo can fail, and then a person has to repair by hand; the receipts say exactly what.
- The sealed "before" values are content. They are restricted data at the customer's gate,
  under the undo window, and never in a receipt.

Threats this adds, for the reviewer: an undo used as a write the requester could not
otherwise make (it is governed, and writes only values that were there before); a sealed
store of before-values as a new place content lives (sealed, short-lived, at the customer);
a connector that claims `restore` for an operation it cannot restore (the connector map is
signed and reviewed; the undo cover page is drawn from what was actually put back in tests).

Not surveyed: whether another product already does this. To check before it is described as
new.

## 5. The gate

### 5.1 Order of checks, enforce mode

1. The session: the request's signature over this gate's challenge and a rising counter, by
   the key in the mandate. (T1 to T3)
2. The mandate chain: signatures, tenant and environment, expiry, each link inside its parent,
   nothing revoked. (T4, T11, T19)
3. The call is mapped to a canonical action by the signed connector map; an unmapped or changed
   tool is refused. (T14)
4. The lease is fresh enough for this action's class. (T11)
5. Policy permits the action for this agent.
6. The mandate's grants carry the action, and its limits are not spent.
7. If policy asks for a countersignature: one is presented that names this gate, this mandate
   and this digest, is of the class asked for, is unexpired and unspent. If none, answer
   `wait_countersign` and send the request to the Authority.
8. The agent's budget allows it now. (T22)
9. Write the intent receipt and mark the countersignature spent, synced to disk.
10. Recompute the digest from the bytes about to be sent; if it differs from step 3, refuse. (T5)
11. Run the action with the gate's credential; the connector checks `resource_version`. (T6)
12. Write the outcome receipt. Return the system's answer, never the credential.

A refusal at any step is one decision receipt with that step's reason code, and an answer that
names the reason and, where one exists, the template that would have allowed the action.

### 5.2 Modes, per system

- **Observe.** Calls pass through with the agent's own credential; one receipt each; no policy
  needed. The gate handles the agent's credential in transit and must not log or store it.
- **Shadow.** The gate decides and records `would_decide`; nothing is blocked.
- **Enforce.** The credential is in the gate's sealed store and the agent's old one has been
  rotated. Refusals are real.

Every interface that reports a system's state says "not enforced" for the first two. (T16)

### 5.3 Front doors

- **The MCP gate.** The gate is an MCP server to the agent and an MCP client to the upstream
  server. It reuses PM OS's MCP client layer, moved into a crate both products depend on
  (slice G3; `crates/toolserver/src/mcp_client/` today), with the tool server's own tests
  passing unchanged before and after the move.
- **The shim.** A small program the agent's runtime starts as a local MCP server. It holds the
  agent's private key and current mandate, opens a session with the gate (receiving the
  challenge), signs every call and forwards it. The agent's MCP client is pointed at the shim by
  configuration; the agent's code does not change. The key file is readable only by the shim's
  user. (T13)
- **Stock client, no shim.** Allowed in observe mode only. The receipt records `proof:
  claimed`.
- **The library.** The same decision code as a Rust crate with no network listener, for PM OS's
  tool server (§8).

### 5.4 Custody

Credentials are sealed with a key obtained through the key-service interface and are opened in
memory only for a call. The first release's driver keeps that key in a file and prints at
startup that it is for a test stack. There is no interface that returns a credential.

## 6. The Ledger

### 6.1 Intake and chains

The Ledger verifies each receipt's signature against the gate's enrolled key, and that `seq`
follows the last one it holds for that gate with the right `prev`. A receipt that does not fit
is refused and the refusal is recorded. One chain per gate, taken in parallel.

### 6.2 Checkpoints and the witness

On a timer (default 60 seconds) the Ledger signs a checkpoint: tenant, environment, time, and
for every gate its `seq` and head hash. It is written to the witness store whether or not
anything changed, so a gate that has gone quiet and a Ledger that has stopped are both visible
from outside. The first witness is a directory (test stacks); the second is object storage with
a write-once lock in the customer's account.

`gov verify` takes the public keys, the witness's checkpoints and an export of receipts, and
reports: every signature valid, every chain unbroken, every checkpoint matched, and any gap.

### 6.3 Retention and deletion

Retention is set by the customer. Pruning removes whole runs of receipts older than the limit
and leaves a signed "pruned through seq N, head hash H" record per gate, so verification starts
from the oldest witnessed checkpoint that is still held. A request to erase a person's data is
met by retention, because receipts hold identifiers and not content; whether that satisfies a
given customer's obligations is theirs to confirm. In the bundle, PM OS keeps its own 90-day
purge and stores only a receipt's reference.

### 6.4 Trace and replay

Trace by request, agent, mandate or resource. **Policy replay** evaluates a candidate bundle
over stored receipts and reports each decision that would change. Drafting a first policy from
observed receipts (the proposal's step 2) works from `system`, `operation`, `kind` and
`resource`, which is why those are in the clear.

## 7. The Authority

### 7.1 Registry and enrolment

An agent record: id, owner (a person), labels, public key, status. Enrolment: an admin issues a
one-time code that expires (default 15 minutes); the agent (or its shim) generates its key
pair and presents the code with the public key. Gates enrol the same way. Every agent has a
named owner; an agent whose owner is offboarded is suspended.

### 7.2 People

Sign-in is OpenID Connect against Entra, reusing `crates/entra`'s verifier (a dependency on a
PM OS crate that has no PM OS state; to be confirmed that it can be depended on without pulling
in `pmos-contract`'s roles). Roles in Governance are its own: admin, owner, approver, auditor.
Groups come from the sign-in token. Governance keeps its own users table; nothing is shared
with PM OS's user database.

### 7.3 The five interfaces

All under `/v1`, each versioned. People's calls carry a signed-in session; agents' and gates'
calls are signed with their enrolled keys.

| Interface | Calls (first release) |
|---|---|
| Admin | enrol or disable an agent or gate; publish a policy bundle; revoke; offboard a person; set a system's mode |
| Mandate | request a mandate from a template (person or parent agent); list and revoke one's own |
| Gate door | open a session (challenge); call a tool; poll a waiting countersignature |
| Gate link | fetch bundle; renew lease; fetch revocations since N; send a countersign request; ship receipts (to the Ledger) |
| Evidence | trace; export; inclusion proof for a receipt; checkpoints |

### 7.4 Admin actions are evidence

Enrolment, publishing policy, a roster change, a mode change, a revocation and an offboarding
are each written as a receipt by the Authority's own enrolled key into its own chain in the
Ledger, and so are covered by the witness. (T8, T18)

### 7.5 Reconciliation (designed here, built after the first release)

A job reads the system's own record of writes made with the gate's credential (Jira's audit
log, GitHub's audit log) and reports any write with no matching outcome receipt, and any
`outcome: unknown` it can now resolve. Until it exists the product states that an unrecorded
write by a compromised gate would be found only by hand.

## 8. The seam in PM OS

### 8.1 Where

One call, in `Service::execute_write` (`crates/toolserver/src/service.rs`), after PM OS's own
grant check has passed and source markers have been stripped, and before the connector's
`write`. With `PMOS_GOVERNANCE` unset the call is not made and no Governance code runs; a test
asserts the tool server's behaviour and audit output are byte-for-byte what they are today.

With it on, the tool server acts as a gate through the library: it holds the credentials (as
it does now), the lease and the bundle; it writes intent and outcome receipts; it adds the
receipt's reference to PM OS's own audit event. Governance can only add a refusal. (T21)

### 8.2 What a mandate is in PM OS

PM OS has no task object; a bot session is an open-ended conversation. The honest mapping is a
**session mandate**: this bot, for this PM, for this session, from a template `pmos-bot-session`
whose grants are the bot's write tools from the catalog, narrowed to what the PM may do, for
the session's life capped by policy. That is coarser than "one task". Narrowing to a task waits
for PM OS to have one. The runtime asks for the mandate when it opens the session, under the
PM's signed assertion.

The bot's key is held by the runtime, never by the harness process the model drives. (T13)

### 8.3 Approval

A PM's approval in the web app or on a Slack card is sent to the Authority as an **attested**
countersignature request for that draft's action digest, vouched for by PM OS under the signed
user assertion. A **signed** countersignature in PM OS needs a passkey step in the web app and
is later work; Slack can never give one. Policy for the PM OS templates therefore asks for
`attested`, and the Governance screens show that class.

### 8.4 Switching on

One setting. PM OS enrols its bots in bulk and starts in observe, then shadow, comparing its own
decision with Governance's for every write, the way `PMOS_MCP_COMPARE` compares connectors with
MCP servers. Fake data only until the gate passes review.

### 8.5 Not connected

The model gateway, evals, content, the daily brief, the Slack app's own messages, OpenShell
policy, the prover and the sandbox audit stay outside. Sending sandbox decisions to the Ledger
is later work.

## 9. Code layout and rules

- Crates, all new: `gov-core` (documents, canonical action, scope and policy evaluation,
  verification; no I/O, no clock, no network), `gov-ledger`, `gov-gate` (library and binary),
  `gov-authority`, `gov-cli` (the `gov` command: `verify`, `enrol`, `policy check|sign`,
  `mandate`).
- **Dependency rule,** checked by a test over `cargo metadata`: no `gov-*` crate depends on a
  `pmos-*` crate, except the extracted MCP client crate and, if confirmed (§7.2), the Entra
  verifier. Only `pmos-toolserver` may depend on `gov-gate` and `gov-core`.
- **Database:** its own database and role (`governance`), its own migrations directory. Every
  table's primary key starts with `(tenant, env)`. No query is built without them; a test tries
  to.
- **Configuration** is `GOV_*`. The first release takes one tenant id and one environment from
  configuration.
- **Before any real data:** a key-service driver that is not a file, the reviewer's gate, and
  the owner's go-ahead. The file driver refuses to start when `GOV_DATA_CLASS` is not `fake`.
- The standing rules apply: one slice, one agent, one worktree, one branch; the full workspace
  checks after each merge; no test, lint or accessibility rule skipped; no token printed.

## 10. Slices

Each slice ends with its attack tests (§3.3) passing and its commit sent to the reviewer.

| Slice | Built | Done when |
|---|---|---|
| G1 | `gov-core`: envelopes, the four documents plus lease and checkpoint, canonical action and digest, scope containment, chain check, policy evaluation, the request signature | Property tests: a child never verifies wider than its parent; any changed byte fails; cross-type, cross-tenant and cross-environment documents fail; RFC 8785 test vectors pass |
| G2 | `gov-ledger`: intake, chains, checkpoints to a directory witness, pruning, `gov verify`, export | Removing, altering or reordering a receipt is reported; a silent gate shows as a missing heartbeat |
| G3 | The MCP client layer moved to a shared crate; `gov-gate` in observe mode against the fake Jira MCP server; the shim | The tool server's tests pass unchanged; a stock client gets `claimed`, the shim gets `proven`; no credential appears in any receipt or log |
| G4 | `gov-authority`: registry, enrolment, Entra sign-in, policy check and signing, mandates, lease and revocation feed, admin interface, `gov` commands | A revoked agent is refused within its class; a gate that misses a revocation treats its lease as invalid; a wrong clock does not extend an expiry |
| G5 | Shadow mode; policy replay; a drafted first policy from observed receipts | Replay reports exactly the decisions a changed rule alters on a fixed history |
| G6 | Enforce: the sealed store, the twelve checks, countersignatures of both classes, the approval screen's interface, budgets, crash recovery | Every row T1 to T7, T11, T15 and T22 has a failing-without, passing-with test; a kill between intent and outcome yields `unknown` and no second write |
| G7 | The PM OS seam, off by default; bulk enrolment; the session mandate; attested approvals; the receipt reference in the audit log; side-by-side comparison | With the switch off, the tool server's output is identical; with it on, on the test stack, every write has an intent and an outcome receipt |
| G8 | Console screens; Governance's own stack file and the overlay for both; health and metrics (decision time, refusals, lease age, revocation delay) | CI's deploy job starts both stacks on a real Docker host |

| G9 | Undo (§4.7), after G6: before-values sealed at intent; the undo action and its receipts; refusal on a changed target; undo of a whole mandate; the undo cover page | On the fake Jira: forty issues changed by one mandate are put back by one undo with a receipt each; an issue someone edited in between is left alone and reported; a `none` operation cannot be undone and needed a person to run |

After the first release, in no fixed order: reconciliation (§7.5); GitHub with tokens minted
per repository; a key-service driver; object-storage witness with lock; SCIM; passkey approval
inside PM OS; the HTTP gate; SDKs; runtime adapters; the log-platform stream; hosted cells;
licensing.

## 11. Open, for the owner

1. Is the cut in §1.1 right, or does the first release need more of the proposal's twelve?
2. Same repository with separate crates (this spec), or a repository of its own from the start?
3. Jira first, then GitHub: agreed?
4. The default that a mandate's issuer may not approve their own agent's marked action (§4.4),
   relaxed for PM OS's templates: agreed?
5. How long the readable form of an action awaiting approval may be kept after the decision
   (§4.4).
6. The product's names: the six working names, and "Agent Governance" itself.
7. Who, besides the owner, may be a Governance admin on the first stack.
8. Decided by the owner on 2026-11-26: the gate serves the approval page for a signed
   approval (§4.4).
9. How an Authority key is replaced if the old one is stolen: today the old key signs the new
   one, so a thief can install their own.

## 12. To be confirmed before or during the build

- Ed25519 through `aws-lc-rs` covers signing and verification as used here (G1).
- An RFC 8785 implementation for Rust that passes the RFC's vectors, or a small one written and
  tested here (G1).
- Whether PM OS's `args_hash` equals the RFC 8785 digest for its argument shapes (G7).
- `webauthn-rs` verifies an assertion in the gate without a browser round trip to the gate, and
  can be driven by a software authenticator in tests (G6).
- `crates/entra` can be depended on without PM OS's contract types (G4).
- The MCP client layer separates from the tool server without changing its behaviour (G3).
- Every default figure in this spec (lease classes, mandate life, checkpoint interval, spool
  limits, enrolment code life). None has been measured.
