# Agent Governance: mandates, a gate and a ledger

Status: **slices G1 and G2 built and through the security gate; G3a built, not yet read by
the reviewer** (see "Status of the build" below); nothing else is built. Revision r1, 2026-11-26, written from `claude/web-app` at `c125014`. The independent
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

**G3a built (2026-11-26), on branch `build/gov-g3a`; the reviewer has not read it.** G3 is
split: G3a is the part of a gate every mode shares, and G3b is the front doors. The spec had
been reviewed and passed the gate before G3a was started.

- Built, in the new crate `crates/gov-gate` (a library; no listener, no credential): a
  receipt for each call into the gate's own numbered chain, an intent before the call and an
  outcome after; a spool on disk, each receipt written and synced before the gate goes on;
  recovery after a crash (an intent with no outcome gets `unknown` and the call is never made
  again; a receipt the state file missed is adopted, not overwritten); shipping to the
  Ledger in batches; and trimming the spool only when a checkpoint covers a receipt with the
  hash the gate has for it.
- In the Ledger: `ingest_batch`, which answers a batch with a signed statement of where the
  gate's chain now stands (`gov.ack.v1`).
- It runs in **observe** mode only. Every receipt says `observe`, and an agent's identity is
  recorded as claimed unless the caller says it was proven.
- Review findings built here: GR-10 (the Ledger's signed answer; the gate keeps its spool
  until a checkpoint covers it, and does not trim on the Ledger's answer alone, on an answer
  the Ledger did not sign, or on a checkpoint naming another hash), GR-16 and GR-42 (a gate
  never writes a time earlier than its last receipt's, tested with a clock that steps back).
- 6 tests, against a real Ledger in memory and a real witness directory, ending in `gov
  verify`'s check passing on the gate's chain. Eight controls were broken on purpose; each is
  caught.

Where G3a differs from the text below:
- **Two receipts per observed call, not one** (§5.2 says one): an intent before the call
  passes and an outcome after, as in enforce mode, so the record shows what the system
  answered and a crash in between shows as `unknown`.
- **The spool limit is checked when a call begins,** with room kept for the outcome of every
  call in flight, so an outcome is never the receipt that does not fit. When the spool is
  full the gate says so; what the front door does then in observe mode (let the call through
  unrecorded, and count it where an operator sees it) is G3b's, and must be stated on screen.
- (Changed after the sixth read.) The gate reads checkpoints from the witness store itself
  and cannot be handed one.
- Not in G3a: the MCP front door, the shim, the move of PM OS's MCP client layer into a
  shared crate, the connector map (with `kind`, the reversal class and `outward`), the
  Ledger's network listener and the bound on recorded refusals (GR-20), case folding for
  systems that need it. All G3b.

**G3b: plan, and one decision that departs from the proposal (conductor, 2026-11-26).**
Nothing of G3b is built.

- **PM OS's MCP client layer is not moved into a shared crate.** The proposal, and §5.3 and
  §9 below, said it would be. Measured before starting: it is 5,800 lines in
  `crates/toolserver/src/mcp_client/`, and it is bound to the tool server's own connectors,
  audit log, vault and user directory. It is also a different thing from what a gate needs.
  PM OS's layer makes one approved server look like a built-in connector, with per-person
  sign-in and a mapping of results. A gate in the path is a plain forwarder of the protocol's
  messages that records each tool call. Moving the layer would be a large change to PM OS for
  code the gate would mostly not use, and it would make Governance depend on PM OS's shapes.
  So the gate gets its own small forwarder, and the dependency rule stays without exceptions:
  no `gov-*` crate depends on a `pmos-*` crate. The one idea worth sharing, a digest over a
  tool's definition that pins it, is a few dozen lines and is written again in `gov-core`
  (over the whole definition since the eighth read). In the "both together" install the tool server still loads the gate
  as a library (G7); that needs nothing from this.
- **Order of G3b:** (1) the connector map in the signed bundle: per upstream tool, its pinned
  digest and its system, operation, kind, reversal class, `outward` flag, how the resource is
  built from the arguments, and whether identifiers fold case; (2) the forwarder: the gate as
  an MCP server to the agent and an MCP client to one upstream server, in observe mode, on
  the fake Jira MCP server; (3) the shim; (4) the Ledger's listener, with the bound on
  recorded refusals (GR-20), and the gate reading checkpoints from the witness itself.
- **To settle in (1), before code:** what the gate records for a tool the map does not know.
  Observe mode exists to show a customer what its agents do before any policy is written, so
  an unmapped call must still leave a receipt; but a receipt's `kind` and `operation` are
  meant to come from the signed map, never from the tool's own description. Proposed: an
  unmapped call is recorded with the operation named after the tool, a kind of `unknown`
  (a new value, valid only in observe and shadow receipts and refused in enforce), and the
  server as its resource. Policy drafted from such receipts must be completed by a person.

**G3b step 1 built (2026-11-26): the connector map,** on `build/gov-g3b-map`, in `gov-core`
(`connector.rs`). Not yet read by the reviewer.

- Per upstream tool, in the signed bundle: the pinned digest of its definition, its
  operation, kind, reversal class, the `outward` flag, and how its resource is built from the
  arguments. Per server: its system, and whether identifiers fold case.
- A call is what the map says it is, never what the tool says of itself.
- Settled as proposed: a tool the map does not know, a tool whose definition changed, a tool
  never listed in the session, and arguments the resource cannot be built from all give an
  action of kind `unknown`. It can be recorded in observe and shadow mode, so the customer
  sees everything its agents do. It is refused by policy whatever the rules say, no rule may
  name it, and an enforced receipt cannot carry it as allowed. The worst is assumed of it:
  not reversible, and outward.
- Three tests; six controls broken on purpose, each caught.

**G3b step 2 built (2026-11-26): the MCP forwarder in observe mode,** on
`build/gov-g3b-forward`, in `gov-gate` (`forward.rs`). Not yet read by the reviewer.

- The gate is an HTTP endpoint that stands between an agent's MCP client and one upstream MCP
  server. Every message passes through unchanged, in both directions, including a server's
  event stream. Around each `tools/call` it writes an intent receipt before the call goes and
  an outcome after, with the action taken from the connector map.
- It remembers the digest of each tool as the server last listed it, so a tool that was never
  listed, or has changed, is recorded as `unknown`.
- The agent's own credential passes through in the request's headers and is forwarded only:
  the test looks for it, for argument values and for the server's reply on the gate's disk,
  and finds none.
- **It blocks nothing.** When the spool is full, or a receipt cannot be written, the call
  still goes through and is counted as "not recorded"; the counters (recorded, not recorded,
  unmapped) are there for an operator's screen (G8) and for health checks.
- A batched request is refused: a batch could carry calls past the receipts, and the
  protocol no longer has batches.
- An agent's identity is what it says in the `x-gov-agent` header, recorded as **claimed**.
  That header is not forwarded.
- 3 tests against a small fake Jira MCP server written for them (PM OS's own fake is not
  used: no Governance crate depends on a PM OS crate), ending in `gov verify`'s check passing
  on the chain. Seven controls broken on purpose, each caught.
- Not in this step: any listener hardening (timeouts, limits on open streams, TLS); the shim
  and proven identity; the Ledger's network listener; a `gov-gate` program to run. The
  forwarder is a library with a router.

**G3b step 3 built (2026-11-26): the Ledger's listener and the gate's network link,** on
`build/gov-g3b-listener`. Not yet read by the reviewer.

- `gov-ledger` has a listener (`http.rs`): one route takes a shipment, and a health route.
  A shipment is a gate's **signed request** (`gov.ship.v1`: the gate, a fresh nonce, the
  count and a digest of the batch) and the receipts. The Ledger answers only a request
  signed by a key enrolled for the gate it names, that fits the batch it came with. Anything
  else gets a bare refusal and **leaves no row in the store**, so a stranger with the address
  can neither learn where a gate's chain stands nor fill the refusals (review findings GR-20
  and GR-55).
- `gov-gate` has the network link (`link.rs`), and signs the request with its own key.
- With it, the seventh read's five findings (below) are fixed.
- 14 gate tests, one of them over real HTTP between a gate and a Ledger. Eight controls
  broken on purpose, each caught.
- **After the eighth read** (`build/gov-review-8`, findings GR-61 to GR-73 below): the
  forwarder reads a request strictly and forwards its own writing-out of it; a call's
  receipts are closed whatever becomes of the client or the server; listings are per MCP
  session; the map cannot fail and derives a Jira project from the issue key; shipping no
  longer holds the gate. `gov-core` has 45 attack tests and the forwarder 7; 19 controls
  broken on purpose, each caught. Not yet re-read by the reviewer.
- Still not built after this step: TLS.

**G3b step 5 built (2026-11-26): the shim, and sessions at the gate,** on `build/gov-shim`,
from §5.3.1 after the reviewer cleared the design (eleventh read) and on the owner's "Build
the shim". Not yet read by the reviewer. With it G3b is built.

- **The gate** (`gov-gate`, `sessions.rs` and the forwarder) opens a session on a request
  signed by the key a mandate names (GR-97, built in from the start), and on every call
  checks a proof for the action it computed itself. A receipt says `proven`, `claimed` or
  `failed`; `failed` carries one of ten reasons and nothing else. The gate serves sessions
  only when it is given the issuers' public keys (`--mandate-issuers`).
- **`gov-shim`** stands beside an agent with the agent's key and mandate. It listens on its
  own machine only, with no flag to widen that; takes a request only with the secret it
  wrote at start, with no `Origin`, its own `Host`, and exactly one content type, JSON;
  reads strictly and forwards its own writing-out; sends tool calls one at a time with a
  fresh salt and a proof; and never sends a call twice.
- With the same step: the gate's operator address needs a secret (GR-98), and calls
  recorded at once are capped, past which a call goes through counted as not recorded
  (GR-99).
- **Stated limits.** There is no Authority, so the mandate in every test is issued by a test
  issuer whose public key the gate is given in a file: "proven" means "holds the key a
  mandate from that issuer names". **A proven receipt says who made the call. It does not
  say the mandate's grants cover the action: nothing checks that until shadow mode (G5), so
  a proven receipt is not an authorised one** (GR-106). Revocation is not checked. After a
  gate restart a shim's first call is a failed proof (`unknown_session`); the shim then
  forgets its tool listing, as the gate has, so the next call is proven again, with the tool
  recorded as `unknown` until the client lists the tools (GR-103, fixed after the twelfth
  read). One slow tool call holds an agent's other calls behind it, because the shim sends
  one at a time (GR-104); and the cap on calls in flight is shared by all agents, so one
  agent's slow calls can leave others' unrecorded for that time (GR-105): a share for each
  agent belongs with budgets in G6. The shim's secret and the operator's secret are
  files. The shim speaks HTTP to the agent, not stdio.
- Tests: four on sessions (who opens one; each way a proof fails; a session ending with its
  mandate and its calls; unproven sessions never pushing out a proven one), one on the
  receipt's rules, one on the cap, and the end-to-end test now runs three programs: an
  agent's call through the shim and the gate is recorded as proven by the agent its mandate
  names, and after the gate restarts the next call is forwarded once, recorded as a failed
  proof, and the one after is proven again.

**G3b step 4 built (2026-11-26): the two programs,** on `build/gov-g3b-programs`, in a new
crate `gov-run`. Built after the eighth read's fixes; not yet read by the reviewer.

- `gov-ledger` has `keygen`, `enrol` (a gate's public key, once; this stands in for the
  Authority until G4), `serve` (the listener, and a checkpoint to the witness on a timer) and
  `export` (the export and the public keys, for `gov verify`). Its database address comes
  from `GOV_LEDGER_DATABASE_URL`, never from the command line, and a failure to connect is
  reported without the address.
- `gov-gate` has `keygen` and `serve`: the forwarder in observe mode in front of one MCP
  server, shipping on a timer and trimming its spool to the witness. `/healthz` says the mode,
  that nothing is enforced, and the counts of recorded, not recorded and unmapped calls.
- **Keys are files for now** (§9: the file driver). A key file is created new, never over
  another, readable by its owner alone; a program refuses a key file anyone else can read or
  that is a link, prints nothing from it, and refuses to start at all unless
  `GOV_DATA_CLASS=fake`.
- **Neither program speaks TLS.** So by default each listens only on its own machine and
  sends plain HTTP only to its own machine; HTTPS to anywhere is allowed. An operator with a
  TLS terminator in front passes `--plain-http-on-network`. An address that carries a user
  name or password is refused.
- **Stated limits of this step:** the connector map and the Ledger's public keys are files
  the operator supplies to the gate, unsigned, until the Authority signs the bundle (G4):
  whoever can change those files can change what the gate records a call as. No pruning job
  is run. Nothing stops a program cleanly: it is killed, and the spool and the Ledger's
  intake are built to be safe against that.
- One test runs both programs as an operator would, against a fake MCP server and Postgres:
  keys, enrolment, calls through the gate, a restart of the gate, and the auditor's check of
  the export against the witness passing each time. A second test covers what each program
  refuses to start with.

**G4a, first part, built (2026-11-26): the Authority's registry, enrolment and admin
records,** on `build/gov-g4a`, from §7.6 after the thirteenth read. Not yet read by the
reviewer.

- `gov-core` has the admin record (`gov.admin.v1`): numbered and chained like a receipt, a
  closed list of fourteen actions, each with the kind of thing it is done to, the second
  thing it names and whether it carries a value; and the request to enrol (`gov.enrol.v1`),
  signed by the key it enrols.
- `gov-authority` is a new crate, a library with a store in memory and one in Postgres
  (Governance's own database, two tables). It names the first admin once; adds and offboards
  people; registers agents, each with an owner, and gates; suspends, resumes and disables;
  and enrols a key by a one-time code.
- **A record and what it changes are written in one step,** in one transaction on Postgres.
  When the record cannot be written, nothing has changed.
- **Enrolment** as §7.6 says: the code itself is presented and only its digest is stored;
  the request is signed by the key being enrolled and names who is enrolling; a wrong code
  is counted and the fifth cancels the pending code; the right code is spent by the first
  request that presents it, whether or not that request holds; the answer carries the root
  key and a value that pins it.
- Every record says its actor is **unverified**: there is no sign-in before G4e. "Only an
  admin acts" is checked against the name given, not against a person.
- **Not in this part:** shipping the Authority's chain to the Ledger and the rest of what
  follows from it, which is the second part, below.
- Ten tests on both stores (after the fourteenth read), and two in `gov-core`: who may act; enrolment; what does not
  enrol a key; an offboarded owner's agents stopping; two admins at once keeping one chain;
  and an action whose record cannot be kept changing nothing.

**G4a, second part, built (2026-11-26): the Authority's chain at the Ledger,** on
`build/gov-g4a2`. Built while the reviewer read the first part; not yet read.

- **A chain holds one kind of document** (GR-108). At the Ledger a key is enrolled for a
  gate's receipts or for an Authority's admin records, and every key of a chain is of that
  chain's kind. At intake a key is read as its own kind only, so the Authority's key on a
  receipt, or a gate's key on an admin record, is refused. `gov verify`'s trusted keys are
  kept per kind in the same way, and it follows either kind of chain.
- **The Authority ships its chain** to the Ledger as a gate does, under a request signed
  with its admin-record key, and checks the Ledger's signed answer against its own chain.
  The chain is then in the same checkpoints as the gates', and under the same witness.
- **A gate's key reaches the Ledger through the Authority's chain** (GR-107, and as the
  reviewer's fourteenth read advised). A key offered to the Ledger, by anyone, is enrolled
  only when the Ledger's own copy of the Authority's chain holds the record of its
  enrolment for a gate, with a key of that digest, and no later record disabling the gate or
  revoking the key. So the keys a Ledger takes receipts under are the ones the witness has
  seen enrolled. **A record that disables a gate retires its key at the Ledger:** what the
  gate sent stays and still verifies, and nothing more is taken (the open half of GR-20).
  The Ledger is given one key at installation: the Authority's admin-record key.
- **After the fourteenth read** (below): a one-time code is taken by exactly one request,
  and a key or key id is enrolled once; the code names the root key to pin and no value
  comes back; a key's id is under its name; cancelling a code after wrong guesses is
  recorded; a code enrols nothing for an agent or gate stopped since it was made; every
  check an action makes is on the chain's head it commits on, so two installers cannot both
  name a first admin.
- **Still not built in G4a:** nothing is served by the Authority on a listener and there is
  no `gov-authority` program, so none of this is yet reachable from the three programs,
  which still use the stand-ins of G3b. The Ledger finds an enrolment by reading the Authority's
  whole chain, which is fine at this size and is to be indexed before it is not.
- **The auditor's check of gate keys (GR-84), built 2026-11-26, not yet read by the
  reviewer.** When the keys file given to `gov verify` names an Authority, a gate's key is
  believed only if that Authority's verified chain holds a record of that key id's enrolment
  for that gate, with that key's digest, at a number a checkpoint covers; otherwise the
  check reports it. A receipt dated after the record that disabled its gate (or revoked its
  key), by more than the clocks may differ, is reported too. Limits: with no Authority named
  the check is not made, as before; if the Authority's chain were pruned past an enrolment,
  the key would be reported, so that chain is not to be pruned; the comparison of dates is
  between two clocks and is only as fine as `--clock-skew`.
- **G4b built (2026-11-26), from §7.6.1 after the sixteenth read; not yet read by the
  reviewer.** The signed policy bundle, in four steps on `build/gov-g4b`:
  - *the format* (`gov-core`): a number that rises, the online keys by role, a bundle that
    opens under the root key alone and may not list its own signer, the rule that a gate
    does not go back, and the list of what changed between two bundles;
  - *the release step* (`gov policy check`, `gov policy sign`; `gov-run`'s `policy_dir`): a
    policy directory of JSON files with sample calls for every connector map; the signer
    runs the check itself, is shown what changed since the bundle before, and the digest
    signed is printed;
  - *publishing* (`gov-authority`): only the next number, only under the root key, kept
    with a `PolicyPublished` record of its digest; and an agent and a gate may not share a
    name (GR-127);
  - *the programs*: `gov-gate` and `gov-shim` take `--bundle`, `--root-key`,
    `--bundle-state` and `--server`; the unsigned files work only with
    `--unsigned-policy-for-tests` and fake data; every receipt carries the bundle's number
    and the start of its digest, or `unsigned`.
  Run as three programs on a signed bundle with fake data, an agent's call through the shim
  is recorded as proven and the auditor's check passes; a gate started on an older bundle,
  or on another under the same number, does not start. Sixty controls removed one at a
  time; each removal fails a test, but one of two overlapping guards against writing over
  a signed bundle.
  **Limits, and what is not built:** the bundle reaches a gate as a file an operator
  copies, read at start (fetching it, and changing it without a restart, come with the
  lease in G4d); two bundles under one number stops a program from starting and is not yet
  counted anywhere an operator's dashboard reads; with no state file any validly signed
  bundle is taken (GR-132); the bundle's rules are still not evaluated against a call
  (G5); the `mandates` keys are a test issuer's until G4c; nothing checks that a bundle a
  gate ran on was published by the Authority (`gov verify` reporting a policy version with
  no `PolicyPublished` record is not written); the `gov` command now links the programs'
  library to read a key file, though no command of it opens a network connection.
- **G4c built (2026-11-27), from §7.6.2 after the eighteenth read; not yet read by the
  reviewer.** Mandates from the Authority, on `build/gov-g4c`:
  - *templates* are in the root-signed bundle, shown to the signer like rules, and a gate
    holds every chain's root mandate to the template of that name in the bundle it holds
    (GR-138); a child stays under its parent's template;
  - *issuing*: to the agent's owner or a person the template names, from the latest
    published bundle, signed with a key that bundle lists for mandates, checked as a gate
    would check it, and written with its `MandateIssued` record in one step. The mandate
    carries no session, since nobody is signed in before G4e;
  - *delegating*: at the signed request (`gov.delegate.v1`) of the agent holding the
    parent, narrower, to an agent whose owner is the person at the root or one the template
    names, once;
  - *listing* by ownership, *revoking* by the issuer, the owner or an admin, and the
    numbered *revocation feed* (`gov.revocations.v1`) signed with the leases key;
  - `gov verify`, with an Authority named, reports a proven receipt whose mandate no
    witnessed record says was issued.
  Run as three programs with fake data, the agent's mandate is now the Authority's and not
  a test issuer's: its call is recorded as proven, and a mandate signed with the same
  mandates key but wider than its template is not. About fifty controls removed one at a
  time; each removal fails a test, after two that only repeated the chain check were taken
  out.
  **Departures from §7.6.2, and limits:**
  - "The same request is never served twice" is kept by taking the child mandate's id from
    the request's own hash, not by remembering nonces: a second copy asks for an id that
    exists.
  - Stopping an agent revokes its mandates as separate records, not in one step with
    the record that stops it: once **before** that record, so a stop halfway leaves an
    agent with less authority than it should have, and once **after** it, for a mandate
    issued while the first round ran (GR-152, fixed after the twentieth read). Asking
    again for a stop that was interrupted finishes it.
  - The Authority is still a library. The gate does not read the revocation feed, so a
    revoked mandate still opens a session until G4d; nothing is served until G4e.
  - A call under a mandate the gate will not open a session for is forwarded, as
    everything is in observe mode, and recorded as a client's bare claim, not as a failed
    proof. The marker for such calls is G4d's (§7.6, choice 5).
  - Limits are carried and compared, not yet charged (G6). A mandate's grants are not
    yet checked against a call (G5).
- One more test on both of the Ledger's stores (a chain holds one kind of document), and
  two with an Authority and a Ledger together.
- **CM1, first map, and CM2's command built (2026-11-27), on `build/gov-cm1`; not yet read
  by the reviewer.** The library is `governance/maps/` (its `README.md` says what a map
  claims). A map is built, never written by hand: `gov map build` joins the server's own
  `tools/list` answer with a review file, and refuses unless every listed tool is reviewed
  or set aside by name with a reason. `gov map drift` names tools a later listing added,
  removed or changed in any field, and exits non-zero.
  - **Jira:** `mcp-atlassian` 0.24.8, 63 Jira tools, all mapped. The listing was got by
    starting the server with made-up settings; **no Jira was contacted and no tool has
    been run against one, so the map is not verified**, only reviewed against the listing.
  - Choices a reader should know: an issue is placed in the project its own key names;
    `jira_update_issue` is mapped as not undoable and reaching outside, because one call
    can also comment, move the issue through its workflow and attach files;
    `jira_move_issue` and `jira_delete_issue` are destructive; a search, and every tool
    that takes a list of issues, has a resource of its own outside any project, so a grant
    on one project never reaches it.
  - **Limits:** `jira_move_issue` names its target project in an argument the resource
    does not carry, and `jira_create_issue_link` and `jira_add_issues_to_sprint` can name
    issues of other projects: a grant on the source is enough for each. A resource holds
    one place; a rule on a second argument waits for G5.
  - Eleven controls of the builder and the drift check removed one at a time; each
    removal fails a test.
  - **GitHub:** `github-mcp-server` v1.14.0 with every toolset, 91 tools, all mapped.
    **Slack:** `@modelcontextprotocol/server-slack` 2025.4.25, 8 tools, all mapped. Both
    listings were got with a made-up token and no network: **neither map is verified.**
  - GitHub choices: a call is held to the repository its `owner` and `repo` name, and no
    finer, because an issue's or a pull request's number is not a string a resource can
    be built from; a tool with several methods is mapped as the widest (so
    `pull_request_review_write`, which can approve, and `actions_run_trigger`, which runs
    whatever a workflow holds, are destructive and reach outside); searches, projects,
    rulesets, gists and notifications have resources outside any repository. The review
    and the server's own read-only hint agree on all 91 tools, and a test holds them to it.
  - Slack choices: every tool that posts reaches people; a channel is its id, and a
    channel given by name is recorded as unknown.
  - **CM2's count:** the gate's operator address shows `unmapped_by_tool`: calls the map
    could not read, by the operation each was recorded under (`unmapped-` and a digest of
    the tool's name). `gov map drift` prints the same name beside each tool it reports,
    so an operator can tell which tool a count is for. The count holds 256 names and
    adds the rest together; it is lost at a restart (the receipts are not).
- **G4d-1 built (2026-11-27), from §7.6.3 points 1 to 3 after the twenty-second read; not
  yet read by the reviewer.** Libraries only, on `build/gov-g4d1`: nothing is served and
  no gate uses it yet.
  - *The renewal* (`gov-core/src/renew.rs`): `gov.renew.v1`, signed with the gate's key,
    with its name, the Authority's name, a fresh nonce, the bundle held and the
    revocation number read through. It carries no time.
  - *The Authority's answer* (`Authority::renew`): a lease with the request's nonce, the
    Authority's time, the latest bundle's version and the newest revocation number, good
    for the bundle's own `standard` limit; the feed from the number the gate named; the
    bundle unless the gate says it holds that very one. Only a key enrolled for a gate,
    asking in that gate's own name, for this Authority, while the gate is not disabled,
    is answered; anyone else gets a bare refusal. One answer a second for each gate. It
    writes nothing and makes no record.
  - *What a gate holds and checks* (`Standing`, `Standing::take`): the checks of point 2,
    in that order, with a closed list of reasons for the operator (`bundle`,
    `bundle_older`, `bundle_forked`, `wrong_bundle`, `lease`, `not_this_request`,
    `authority_behind`, `feed`). An answer that fails any check changes nothing. The
    gate's time is the lease's plus its own count since the request left; a lease is
    fresh for as long as it says.
  - Run together on both of the Authority's stores: a first renewal brings the bundle; a
    revoked mandate and a disabled agent reach the gate's revoked set; a new bundle
    arrives with its lease; a gate started again is told everything from nothing; a
    disabled gate is not answered. An Authority put back from an older copy, or with its
    clock set back, is refused as `authority_behind` and taken again once it has caught
    up. 23 controls removed one at a time; each removal fails a test.
  - **Departures and limits.** (1) "At most once a second" compares for the same second
    only: an Authority whose clock was set back still answers, so that the gate can say
    it is behind. (2) A gate that names a higher revocation number than the Authority
    has is answered from where the Authority stands, not refused, for the same reason.
    (3) The lease names the bundle by `<number>-<first 16 hex of its digest>`, the
    version a receipt carries, not by the whole digest; the bundle itself is opened
    under the root key before it is compared. (4) A gate's revoked key is not yet
    refused a renewal: no key can be revoked before G4d-4. (5) The limit of one answer a
    second is kept in memory and is each Authority process's own.
  - **How long "proven" lags a revocation** (GR-176). A lease is good for the bundle's
    `standard` limit, 120 seconds unless the bundle says otherwise. Until the gate's next
    renewal brings the feed, or the lease runs out, a revoked agent's calls are still
    recorded as proven: for at most that limit after the revocation, and in practice for
    the time between two renewals. A proven receipt is to be read with that delay in mind.
- **G4d-2 built (2026-11-27), from §7.6.3 points 4 to 6; not yet read by the reviewer.**
  On `build/gov-g4d2`. The gate program is not wired to an Authority yet (G4d-5): this is
  the gate's library, run in tests with answers made by hand.
  - *The gate's time.* With a lease held, "now" for sessions and receipts is the lease's
    time counted on by the gate's own clock, and every receipt from the first lease on
    carries `at_from_lease`. Before the first lease, and with no Authority, nothing
    changes: the machine's clock, and no mark. The clock that counts suspension is given
    to the forwarder as a function; the real one comes with the program (G4d-5).
  - *Proof under a lease.* With an Authority, a session is opened only under a complete,
    fresh lease and for a chain nothing in which is revoked (refusals counted as
    `no_lease` and `revoked`). A proof that holds is recorded as proven only under such a
    lease; otherwise as the agent's failed proof with the reason `no_lease`. Complete, at
    the forwarder, also means the lease names the bundle the forwarder's map came from.
  - *Revoked sessions.* When an answer is taken that revokes something, every open
    session's chain is checked again as a new one would be. One that no longer holds is
    kept as a marker: a later call whose proof holds is recorded as that agent's, failed,
    with `key_revoked`, `agent_revoked` or `mandate_revoked` (the nearest thing revoked:
    the session's key, then an agent of the chain, then a mandate or the key that signed
    one). A proof that does not hold keeps its own reason. A marker is never counted as
    proven, pushes out no proven session, and is swept when its session would have been.
  - Run with a gate and a shim in one process: a bare claim before the first lease; proven
    under it, at the Authority's time; `no_lease` once it is two minutes old; proven again
    after a renewal; `mandate_revoked`, twice, after the mandate is revoked, with the shim
    keeping its session. 18 controls removed one at a time; each removal fails a test.
  - **Limits.** (1) A bundle that changes while the gate runs is not put in use until
    G4d-3: the lease then names a bundle the forwarder does not decide from, so the gate
    has no complete lease and proves nothing until it is restarted on the new bundle.
    (2) The shim's clock and the Authority's must agree to within the time a session's
    opening allows (two minutes), since the gate's time is now the Authority's. (3) During
    a long loss of the Authority a session runs out; the shim's next opening is refused
    and its calls are then recorded as a client's bare claim, not as `no_lease`. (4) The
    new reasons are new words in the receipt's closed list: a `gov verify` built before
    this slice refuses a receipt that carries one.
- **G4d-3 built (2026-11-27), from §7.6.3 point 7; not yet read by the reviewer.** On
  `build/gov-g4d3`. Libraries and tests; the gate program is wired to an Authority in
  G4d-5.
  - *The gate changes bundle in place.* When an answer brings another bundle, the
    forwarder swaps its connector map and the version its receipts carry together, and
    the sessions take the new issuers' keys and templates. A call takes the map and the
    version as one thing when it starts, so no receipt names one bundle for a call read
    by another's map, and an outcome carries its intent's. A bundle with no map for the
    gate's server leaves every tool unknown.
  - *Sessions under the new bundle.* Each open session's chain is checked again as a new
    one would be. One that no longer holds (its template gone or narrower, its issuer's
    key no longer listed) is dropped, and its next call is an unknown session. A marker
    for something revoked is kept whatever the new bundle says.
  - *The shim follows* (GR-133). The gate names the bundle it decides from when a
    session opens and with every answer to the shim. A shim on another asks the gate for
    it (`POST /v1/bundle`, given only to a caller that names a session open at that
    gate), opens it under the root key the shim pinned itself, takes it only if it is the
    one the gate named and would not take the shim back or sideways, notes it in its
    state file, and uses it from its next call.
  - *A gate's standing keeps the bundle as signed* (to hand on), and starts only on a
    bundle the root signed for its own Authority.
  - *GR-175:* the Authority keeps the bundle it last opened and opens another only when
    the store's latest has another digest.
  - Run with a gate and a shim in one process: proven under the first bundle; the second
    arrives, the next call is read by the new map and its proof fails (`wrong_digest`)
    once; the shim follows and the call after is proven under the same session; a third
    bundle without the mandate's template drops the session. A shim that pinned another
    root, holds a later bundle, or holds another under the same number takes nothing. A
    stand-in for the gate that names one bundle and hands over another moves the shim
    nowhere. 18 controls removed one at a time; each removal fails a test.
  - **Departures and limits.** (1) The call that crosses a change is recorded as a failed
    proof with `wrong_digest`, as §7.6.3 says, but it is **not counted apart** on the
    operator's address: it is in the count of failed proofs, and the receipt has the
    reason. (2) A shim with no session is handed no bundle, so it cannot follow; its
    calls are bare claims then, and no map of its own matters to them. (3) The gate
    library does not write the bundle it took to its state file; the program does that
    in G4d-5, and until then a gate started again begins from the bundle file it is given
    and is brought forward by its first renewal. (4) The bundle is policy, and any agent
    with a mandate that opens a session can read it.
- **G4d-4, first part, built (2026-11-28), from §7.6.3 point 8; not yet read by the
  reviewer.** On `build/gov-g4d4a`, libraries and tests.
  - `Authority::revoke_key`: an admin revokes an enrolled key, an agent's or a gate's,
    once, with a record and an entry in the feed that never runs out. A revoked gate key
    is refused a renewal (the check the twenty-fourth read asked to come back, with its
    test). Nothing is issued to a revoked agent key: the chain check the Authority makes
    before it signs already refuses it.
  - `Gate::open_after`: a gate given a new key opens its spool under the keys it is told
    were its own before, closes what the old key left open, and goes on in one chain. A
    receipt under any other key still stops it (GR-59).
  - Six controls removed one at a time; each removal fails a test. Two checks added at
    first were taken out again, since another check already did their work.
  - **Not built here:** keeping the earlier keys as files and deleting the old private
    half once the Ledger has acknowledged its last receipt (the program, G4d-5); the
    Authority's own admin-record key (point 9) and the witness's order in `gov verify`
    (point 10), which are the second part.
- **G4d-4, the auditor's order, built (2026-11-28), from §7.6.3 point 10; not yet read by
  the reviewer.** On `build/gov-g4d4b`. `gov verify` no longer compares the gate's date on
  a receipt with the Authority's date on the record that retired its key. A receipt under
  a retired key counts as earlier only if a checkpoint before the first one covering the
  retirement already covered it; otherwise it is reported, as "not shown to be earlier"
  (GR-130). Tested for a disabled gate, with and without the earlier checkpoint; the same
  code serves a revoked key, which has no test of its own yet. `clock_skew_secs` no longer
  bears on this finding. Not built: replacing the Authority's admin-record key (point 9).

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

### Second read, 2026-11-26, at `0175ade`

**Verdict: the gate passes for the spec and for G1; it does not pass for G2.** The spec
passes on one condition: the signed class of approval stays unbuilt until its design is
reviewed. The first-round findings are closed except as noted here.

| Finding | Severity | What was wrong | What was done |
|---|---|---|---|
| GR-22 | High | The rest of GR-3. `gov verify` still passed a whole chain wiped at the witnessed head: with no retention given it said OK, and with one it measured age by the date the Ledger wrote in its own prune record | Fixed on `build/gov-review-2`. Age is measured from the first covering checkpoint in the witness to the verifier's own clock; a prune record dated after now is a finding; pruning with no retention given is a finding; with no clock nothing counts as old enough; the verdict no longer says "nothing removed" when something was pruned. What is left, and accepted by the reviewer as by design: an erasure that waits out the retention is a prune |
| GR-23 | Medium | A trusted gate listed with no receipts, no prune record and no checkpoint entry verified OK, which is what a gate erased before any checkpoint looks like | Fixed: a finding. A gate that has never run must be left out of the trusted keys. The real control is the gate keeping its spool until a checkpoint covers it (GR-10, G3) |
| GR-24 | Low | The store enrolled a key without the weak-key check | Fixed: both stores check |
| GR-25 | Low | One lease request could accept two leases | Fixed: accepting a lease uses the request up |
| GR-16 | Low | A receipt's time could go backwards along a chain | Fixed at intake and in `gov verify`. For G3: a gate never writes a time earlier than its last receipt's |
| GR-6 | | Two more spellings of the small-order points, harmless with this library | Added to the list |
| GR-26 | Low | "A receipt nothing covers" compares the gate's clock with the Ledger's | For G3, with the signed intake head of GR-10 |
| GR-27 | Info | The "issuer may not approve" check is an option the caller fills | For G6: the gate builds the requirement from the rule and the chain in one place, with a test |

The reviewer's conditions for the gate-served approval page (§4.4), all three adopted: the
passkey is registered to an address the Authority does not serve; the gate builds the
challenge itself from the bytes it will send, and its page loads no script from the
Authority; changes to the roster stay witnessed. It also noted that this makes the gate a web
server for people, which needs its own rows in the threat table before G6.

### Third read, 2026-11-26, at `404376e` (the two designs; before the GR-22 fix)

**Verdict: the gate passes for the spec and for G1** with both designs in. G2 was unchanged
at that commit, so GR-22 still stood. The reviewer set conditions: GR-29 to GR-32 must be
answered in §4.4 before G6 builds the signed approval, and GR-34 to GR-36 in §4.7 before G9
builds undo. They are answered in those sections now, as requirements; none is built.

| Finding | Severity | What it said | Where it is answered |
|---|---|---|---|
| GR-29 | Medium | Who may enrol a passkey at the gate was not said; a signed approval was only the gate's say-so afterwards | §4.4: enrolment needs an admin's approval or a delay with notice; every enrolment and removal is a receipt; the assertion, credential and challenge are kept so an approval can be re-checked |
| GR-30 | Medium | Several gates, or anything else, under one address brings GR-2 back | §4.4: one exact address per gate, used for nothing else, never a parent domain |
| GR-31 | Medium | The gate as a web server for people had no threat rows | §3.3, T23 to T26 |
| GR-32 | Medium | A compromised Authority can publish a policy that asks for no signed approval, or point the gate at its own identity provider | §4.4: a local floor in the gate's own configuration; identity-provider settings and the passkey address are never taken from the bundle |
| GR-33 | Low | The format does not tie the class to who issued it | §4.4, for G6 |
| GR-34 | Medium | Putting back is not harmless: an undo can grant access, expose, reopen or delete | §4.7: an undo is evaluated as the concrete operation it performs; there is no general "undo" that policy allows wholesale |
| GR-35 | Medium | "Approve only what cannot be undone" takes a person off every write the map calls reversible, though its effects may leave the system | §4.7: a second flag, `outward`; the default asks for a person when an operation cannot be undone **or** is outward; `reversible` never lowers a destructive operation |
| GR-36 | Medium | A plain digest of a small value gives the value away to anyone who can guess | Fixed in the formats: the arguments' digest and the before-digest are salted, the salt kept by the gate with the values |
| GR-37, GR-38 | Low | A lost update between the before-read and the write; a receipt saying `restore` when the before-read failed | §4.7 steps 1 and 1a |
| GR-39, GR-40 | Low | Rules missing on `reverses`; an outcome did not name its intent | Fixed in the formats |
| GR-41 | Info | The reversal class reaches `evaluate` and the receipt by two routes | For G6: one value, one place |

### Fourth read, 2026-11-26, at `e7adb6b`

**Verdict: the gate passes for the spec, for G1 and for G2. No High or Critical is open.**
The reviewer re-ran its attacks on `gov verify` and each is now reported; GR-22 to GR-25 and
GR-16 are closed. It is a pass on code that touches no real system. Before real data the open
Medium findings still need their answers built: GR-10 (G3), GR-8a and GR-29 to GR-32 (before
G6 builds the signed approval), GR-34 to GR-36 (before G9).

Two new Low findings, both fixed on `build/gov-review-3`, with the format fixes of the third
read (GR-36, GR-39, GR-40):

| Finding | What it said | What was done |
|---|---|---|
| GR-42 | Refusing a receipt dated before the one it follows ends that gate's chain, and an honest gate's clock can step back a second or two after a slow lease renewal | Intake takes the receipt and records the step back; `gov verify` reports it. For G3: a gate never writes a time earlier than its last receipt's, with a test that steps the lease clock back |
| GR-43 | The Ledger dates its own checkpoints, so one hostile from its first day could back-date them and prune at once | `gov verify` reports a checkpoint dated earlier, by more than the interval, than a receipt it covers. The full answer is the witness store's own write times, with the object-storage witness |

### Fifth read, 2026-11-26, at `2a5655a`

**Verdict: the gate passes for the spec, G1 and G2.** The answers to the third read's
conditions were "the ones I meant". One new Medium, to settle before the shim is built, and
three smaller findings; all are answered on `build/gov-review-4`.

| Finding | Severity | What it said | What was done |
|---|---|---|---|
| GR-44 | Medium | The salt of GR-36 was "drawn by the gate", but the agent's request signs the action's digest, which depends on the salt, before the gate has seen the call. Nothing said who draws it, how it travels or how long it is kept | Settled in §4.3: the shim draws it and sends it with the call; the gate refuses a salt seen before in the session and recomputes the digest; a waiting approval keeps it with the readable form; afterwards it goes with the arguments, so a receipt does not prove what the arguments were |
| GR-45 | Low | "A checkpoint dated before a receipt it covers" compared two clocks with only the checkpoint interval as allowance | Its own setting, `--clock-skew` (default 300 seconds); the message names both clocks |
| GR-46 | Low | Nothing checked that an outcome names an intent of its own chain for the same action, once | Intake refuses an outcome whose intent is not an intent for the same action; `gov verify` also reports a second outcome for one intent. Intake does not check "once" itself: that needs a look across the chain |
| GR-47 | Info | A receipt taken with a stepped-back time was written among the refusals | The row is marked `taken` |

Also added at the reviewer's word: the gate's approval challenge is single use and expires (T26).

### Sixth read, 2026-11-26, G3a at `5250505`

**Verdict: the gate passes for G3a.** No High or Critical. Three Medium findings were
conditions on G3b; all eight findings are fixed on `build/gov-g3a-fixes`, before G3b starts.

| Finding | Severity | What it said | What was done |
|---|---|---|---|
| GR-48 | Medium | A crash after an outcome was written, and before its intent was marked answered, made the gate write a second outcome of `unknown` on restart | On opening, an open intent that a held outcome already names is marked answered and nothing is written |
| GR-49 | Medium | The gate trimmed its spool on any checkpoint the Ledger signed and handed to it, so a Ledger could have it drop receipts that were never witnessed | The gate reads the witness store itself (`trim_to_witness`); it cannot be handed a checkpoint. It never trims past what the Ledger acknowledged, and keeps the checkpoint it trimmed on |
| GR-50 | Medium | The gate could not tell a Ledger that takes nothing from one with nothing to take; never sent again to a Ledger restored from an older copy; and took an honest Ledger that was ahead for a liar | `ship` answers one of four things: nothing to send, advanced, no progress, or the Ledger is behind (and the gate sends again from there). The acknowledged receipt is looked up in the whole spool. The Ledger answers even when it holds nothing |
| GR-51 | Low | The gate adopted spool files without checking them | Each is opened under the gate's own key and its number and hash compared |
| GR-52 | Low | A crash between moving the state and trimming left covered receipts behind | Opening the gate removes them |
| GR-53 | Low | One far-future clock reading pins every later receipt to that time | A receipt whose time was raised to the last one's says so (`at_clamped`) |
| GR-54 | Low | A receipt could say observe mode with custody coverage | Refused: custody belongs to enforce mode |
| GR-55 | Low | The Ledger's answer was not tied to one request, and intake does not know who is sending | The answer carries the batch's nonce and the gate refuses another. Authenticating the gate to the listener is G3b's |

For G3b, from the same read: "proven" on a receipt may be set only from what
`Session::accept` returns, and a proven receipt names its mandates; a full spool in observe
mode lets calls through unrecorded, which must be counted and shown as "not recorded".

### Seventh read, 2026-11-26, at `8863183`

**Verdict: the gate passes for the spec, G1, G2 and G3a.** Every Medium of the fifth and
sixth reads is closed. Five new Low findings, fixed on `build/gov-g3b-listener`:

| Finding | What it said | What was done |
|---|---|---|
| GR-56 | The gate stayed silent when the newest checkpoint left it out or showed its chain shorter than before | It raises the alarm itself |
| GR-57 | One unreadable checkpoint with the highest number stopped all trimming | The newest checkpoint that verifies is used; the bad ones are reported |
| GR-58 | An acknowledgement for a receipt already trimmed had its hash accepted unchecked, and an alarming answer was thrown away | It is checked against the checkpoint the spool was trimmed on; the Ledger's signed answer that raised an alarm is kept with the spool |
| GR-59 | After a key rotation the gate would refuse to start on its own older receipts | Not fixed: to be decided with key rotation in G4 |
| GR-60 | A refusal such as "outcome without intent" ends a gate's chain at the Ledger, and the gate saw only "no progress" | The Ledger's reason is passed on to the gate and shown. Stated here: an honest gate never writes such a receipt, so this bites only a gate with a fault, and then someone must act |

The reviewer's session cannot send messages without a person's approval, so the conductor
reads its verdicts from its transcript.

### Eighth read, 2026-11-26, G3b steps 1 and 2 at `b733bee`

**Verdict: the gate passes for the connector map and the forwarder, for observe mode only.**
No High or Critical while nothing is enforced. Seven Medium findings let a hostile agent or
server escape the record or falsify it; the reviewer said four of them (GR-61, 62, 64, 65)
would be High the moment anything is enforced, and that none of this code may back shadow or
enforce mode until they are closed. All but one line of this table is fixed on
`build/gov-review-8`; the reviewer's re-read is owed.

| Finding | What it said | What was done |
|---|---|---|
| GR-61 | Arguments or a tool name could make mapping fail, and a call that cannot be mapped left no receipt | Mapping cannot fail: it always gives an action, `unknown` at worst, with a digest of the arguments as they arrived when they have no canonical form |
| GR-62 | The gate read a request one way and forwarded the original bytes, so a body with a repeated member name was one call to the gate and another to the server | A body is parsed strictly (no repeated names, nothing after the value, no number that changes when read) or refused; what is forwarded is the gate's own writing-out of what it read. Only POST, GET and DELETE are taken, and GET and DELETE carry no body upstream |
| GR-63 | A client that went away mid-call, or a server that never answered, left an intent open for ever | The exchange and the outcome receipt run in a task that outlives the client, under a time limit; a call the gate cannot account for is closed as `unknown` |
| GR-64 | One list of tools for every client: one session's listing vouched for another's, a dropped tool stayed known, and the digest covered only the description and schema | Listings are kept per MCP session; a fresh listing replaces the old one (a later page adds to it); a definition with no digest is forgotten; the digest covers the whole definition. Clients that send no session header share one listing: stated, and settled with the shim |
| GR-65 | A resource built from two arguments (`project` and `key`) let a call name one project and act on another | A template may derive one part from another (`{key:before(-)}`), and the rule is written in the code and here: build a resource only from what the server itself uses to pick its target |
| GR-66 | For a system that folds case, a refusal written in upper case matched nothing | Refused by lint, for a rule's patterns and for the map's own template, before a bundle is signed and when a gate loads it |
| GR-67 | The gate was held while the Ledger was waited for, and a server's answer was read whole whatever its size | Shipping is split: collect the batch, send without holding the gate, apply the answer. A time limit on the Ledger. Answers are read with a running cap of 4 MB |
| GR-68 | The client's `accept-encoding` was forwarded, so a compressed answer was unreadable to the gate and recorded as failed | The gate asks the server for an uncompressed answer |
| GR-69 | A redirect from the server was passed to the client, which would follow it with its credential | A redirect is answered with the gate's own 502, and `location` is never copied |
| GR-70 | A tool call with no id had no answer to match | Refused |
| GR-71 | An unmapped tool's name went into the receipt, text the server or caller chose | The receipt carries `unmapped-` and 16 hex digits of a digest of the name |
| GR-72 | Receipts are stamped from the gate machine's clock | Not changed: stated in the code; the lease's time is used once there is an Authority (G4) |
| GR-73 | The count of unrecorded calls was lost at a restart | Kept in the spool's state as well |

### Ninth read, 2026-11-26, at `5c6c4e6`

**Verdict: the gate passes for the spec, G1, G2, G3a and G3b steps 1 to 3, in observe mode.**
No High or Critical. GR-61, 62 and 64 are closed; GR-65 is closed in the code with a standing
condition: **each connector map needs its own test that two arguments cannot name different
targets, and the reviewer reads each map.** GR-63 and GR-66 to GR-73 are closed (GR-72
accepted as stated, until G4). New findings:

| Finding | Severity | What it said | What was done |
|---|---|---|---|
| GR-74 | Medium | A refused receipt whose signature held is stored whole every time it arrives; a stuck gate resends its batch every round, so the refusals table grows without limit | A refusal is stored once per envelope and reason, with a count and the time it was last seen (`build/gov-review-9`) |
| GR-75 | Medium | Nothing requires an encrypted connection: receipts, and through the forwarder an agent's credential, could cross a network in the clear | The programs refuse plain HTTP beyond their own machine unless the operator passes `--plain-http-on-network` (G3b step 4) |
| GR-76 | Low | At 4,096 remembered sessions the whole table is emptied, so one agent can make every session's tools `unknown` | Only the session seen longest ago is forgotten. Not done: a cap per agent, because without the shim an agent's name is only what it says |
| GR-77 | Low | An anonymous request to the Ledger costs a database lookup, with no rate limit | Open: with the listener's hardening |
| GR-78 | Low | The Ledger's answer to the gate is read whole with no cap | Read with a cap of 64 KB |
| GR-79 | Low | Two rounds of shipping at once prepare the same batch | One round at a time in the forwarder; calls do not wait for it |
| GR-80 | Low | Only the last alarming answer is kept | Each is kept under the receipt number it speaks of, up to 64 |
| GR-81 | Low | A mandate's grants for a folded system are not linted | Open: a condition on G4 (the Authority lints at issue; the chain check refuses an upper-case grant for a folded system) |

The reviewer said GR-74 and GR-75 should be fixed before G3b is called done. Both are; the reviewer's read of the fixes, and of the programs (step 4), is owed.

### Tenth read, 2026-11-26, at `09dea18`, and the shim's design at `4fba4cf`

**Verdict: the gate passes for the spec, G1, G2, G3a and G3b steps 1 to 4, in observe mode.**
No High or Critical. GR-74 is closed; GR-75 is closed for the two programs, with a standing
condition: **the libraries still take any address, so any other embedder (PM OS's tool
server, G7) must make the same check.** GR-78 to GR-80 are closed. The reworked secrecy
checks in the tests were read and found sound.

| Finding | Severity | What it said | What is done |
|---|---|---|---|
| GR-76 | Low, partly closed | Evicting the oldest session one at a time still lets 4,096 made-up ids push every real session out | A listing is remembered only for a session id the server itself issued, or for no session at all |
| GR-82 | Medium | Every receipt written reads every file in the spool under the gate's one lock, and the trim reads the whole, ever-growing witness under it; a mode that must block nothing slows to a stop when the Ledger is down | The gate keeps an index of its spool in memory and reads one file per receipt it needs; the spool is read whole once, when the gate opens. A trim reads the newest 16 checkpoints, before the gate is taken. The forwarder does the gate's file work on threads that may block |
| GR-83 | Low | The gate's counters are served where agents can read them, so an agent learns when it is unrecorded | The counters are on a separate operator address (`--operator-listen`); the agents' address has no such route |
| GR-84 | Low | The keys file `gov-ledger export` writes comes from the Ledger being audited | The command says so in words; the real answer is a condition on G4 |
| GR-85 | Low | The serving database role also runs migrations | Open: a separate role when the stack file is written (G8) |
| GR-86 | Low | The name `localhost` is trusted without being resolved | A literal loopback address only |
| GR-87 | Low | A checkpoint or shipment that keeps failing shows only on standard error | The Ledger's health route answers 503 while checkpoints are failing, with the count; the rest with G8 |

**The shim's design (§5.3.1): may be built once four things in the text change** (all
Medium), which they now have, below in §5.3.1:

| Finding | What it said | What the design now says |
|---|---|---|
| GR-88 | A copied mandate chain could keep evicting the real shim's session | A session counts against an agent's cap only after its first valid proof; unproven sessions expire in seconds under their own small cap; a proven session is never closed to make room for an unproven one |
| GR-89 | "Opens another and sends the call again" could run a call twice | In observe mode an unknown session is a failed proof: the call is forwarded once and a response header tells the shim to reopen for the next call. The shim resends only when the gate says, distinctly, that the call was not forwarded |
| GR-90 | A loopback port that signs anything can be reached from a web page on that machine | A secret made at each start and required on every request; a request with an `Origin` header or a foreign `Host` is refused; only `application/json` is taken |
| GR-91 | A failed proof had no mark of its own: `claimed` is also what a stock client gets | A third proof value, `failed`, with a reason from a closed list; counted and reported apart; never counted as the agent's act |
| GR-92 | A counter that must rise fails honest calls made in parallel | The shim sends tool calls to the gate one at a time |
| GR-93 | A session can outlive its mandate | The gate checks the mandate's expiry on every call |
| GR-94 | The shim could sign one reading of a request and send another | The shim reads with the same strict reader and forwards its own writing-out |
| GR-95 | Arguments with no canonical form are digested over bytes, which only match on one build | Their proof is expected to fail and is recorded as `failed` with its own reason |
| GR-96 | The shim sends the credential and proofs to whatever its gate address is | The programs' rule: HTTPS, or plain HTTP only to a literal loopback address |

**Found by CI after the tenth read's fixes, 2026-11-26.** The check on `abbc640` failed, and
the failure was a real fault in the GR-63 fix: the intent receipt was written by the request's
own handler and only the rest of the call ran in a task of its own, so a client that left
between the two left an intent open for ever. Moving file work to other threads (GR-82)
made that moment long enough for CI's disk to hit it. Fixed on `build/gov-ci-open-intent`:
the whole call, intent receipt included, runs in the task. A test with a deliberately slow
spool fails without the fix and passes with it.

### Eleventh read, 2026-11-26, at `b97d8b3`

**Verdict: the gate passes for the spec, G1, G2, G3a and G3b steps 1 to 4, in observe mode;
G3b is done but for the shim, and the shim may be built from §5.3.1 as written.** No High or
Critical. GR-82, GR-76, GR-84, GR-86 and GR-87 are closed; GR-88 to GR-96 are answered in the
design; the fix for the fault CI found (the open intent) was read and found right, and
nothing else in the forwarder can be left half-done by a client that leaves.

| Finding | Severity | What it said | What is done |
|---|---|---|---|
| GR-97 | Low | Someone holding a copy of the chain can keep the unproven sessions full, and so stop the real shim opening one after a restart | Built into the shim's first version: the request to open a session is signed by the agent's key over the gate's name and a fresh value |
| GR-98 | Low | Any local process, an agent on the gate's own machine included, can read the operator address | With the shim's step: a secret on the operator address |
| GR-99 | Low | No cap on calls in flight: each holds up to 4 MB for up to the call's time limit | With the shim's step: a cap; past it, in observe mode, the call is forwarded and counted as not recorded |
| GR-100 | Low | If the witness's newest 16 checkpoints all fail to verify, nothing is trimmed until someone removes them | Stated here. A Ledger that can write to the witness can also stop publishing, so this adds little; the gate reports the bad checkpoints each round |

Open conditions on later slices, by the reviewer's list: GR-77 (listener hardening), GR-81
(G4), GR-85 (G8), a test for each connector map (GR-65), GR-8a and GR-29 to GR-32 (G6),
GR-34 to GR-36 (G9).

### Twelfth read, 2026-11-26, the shim at `543407a`

**Verdict: the gate passes for G3b as a whole, all five steps, in observe mode**, with the
spec, G1, G2 and G3a. No High or Critical. The reviewer's answers to the two questions put
to it: "proven" cannot be written for anything but a success of `Session::accept` on the
action the gate computed; and nothing that reaches the shim's port without its secret can
get a call signed. GR-97, GR-98 and GR-99 are closed.

| Finding | Severity | What it said | What is done |
|---|---|---|---|
| GR-101 | Low | `proof` was a field any caller of `Gate::begin` could set, so a later front door could write "proven" by mistake | Who made a call is a value with private fields, built only from a claim or from what `Sessions::prove` returned; a proven call has a private field of its own (`build/gov-review-12`) |
| GR-102 | Low | The time check on a request to open a session could overflow: a panic that poisoned the sessions' lock in a debug build, a wrapped value in a release build | Compared without arithmetic that can overflow; and a panic no longer leaves the sessions unusable |
| GR-103 | Low, before G6 | After a gate restart every honest call failed its proof until the client listed the tools again | The shim forgets its listing when the gate says the session is gone |
| GR-104 | Low | One slow call holds an agent's other calls, the cost of sending one at a time | Stated in the status note; a window of accepted counters would remove it |
| GR-105 | Low | The cap on calls in flight is shared by all agents | Stated; a share per agent belongs with budgets (G6) |
| GR-106 | Info | A proven receipt is not an authorised one | Stated in the status note; reports must say so |

Open conditions on later slices, by the reviewer's list: GR-77 (listener hardening), GR-81
(G4), GR-85 (G8), a test for each connector map (GR-65), GR-8a and GR-29 to GR-32 (G6),
GR-34 to GR-36 (G9).

### Thirteenth read, 2026-11-26, the G4 build design at `b2a0c74`

**Verdict: no High or Critical; G4a may be built from §7.6 once GR-107 and GR-108 are
answered in the text**, which they now are. GR-101 and GR-102 are closed. The fix for GR-103
opened a new fault, GR-112.

| Finding | Severity | What it said | What is done |
|---|---|---|---|
| GR-107 | Medium, before G4a | Enrolment "signed over the code's digest": if only the digest travels, the stored digest is the secret; if the code travels, the signature does not stop a race | §7.6, choice 3, rewritten: the code itself is presented, under the connection rule, used up by the first request; the signature covers the name and the key; wrong codes are counted; the answer pins the root key; how a gate's key reaches the Ledger is said |
| GR-108 | Medium, before G4a | Admin records needed typed chains, record before effect, and mandate issue recorded; no free text | §7.6, choice 1 |
| GR-109 | Medium, before G4b | One key for bundles and for leases puts the trust root on the busiest signer | §7.6, choice 2: a root key signs the bundle, which names the online keys by role |
| GR-110 | Low, before G4d | A revoked agent has proven its key, so one "failed proof" reason is untrue; and after its session closes its calls lose its name | §7.6, choice 5 |
| GR-111 | Low, G4a to G4d | Before sign-in exists, an admin record must not read as a person's proven act | §7.6, choice 8 |
| GR-112 | Medium | The shim forgot its listings on every `x-gov-reopen`, but only a restarted gate has lost its own; an idle session then broke every later call | Fixed on `build/gov-review-13`: the gate says whether it still holds a listing for that MCP session, and the shim forgets only when it does not |
| GR-113 | Low | After a gate restart a running MCP session's id is never issued again, so its listings are never remembered | Fixed: an id is taken as issued when the server answers a request under it successfully |

### Fourteenth read, 2026-11-26, G4a's first part at `36ded25`

**Verdict: no High or Critical. G4a's first part passes as a library that nothing serves
yet;** GR-107 and GR-108 are answered in the text and, but for two points, kept in the code.
GR-112 and GR-113 are closed. The reviewer read the first part only; the second part was
built while it read, and then changed to follow this verdict's advice.

| Finding | Severity | What it said | What was done (`build/gov-g4a2`) |
|---|---|---|---|
| GR-114 | Medium | Two requests with the right code at the same moment both enrolled; and a second key under an enrolled id silently replaced the first | The code is taken, not read: one statement removes and returns it, to one caller. Adding a key never replaces: a taken id or key fails the whole commit |
| GR-115 | Medium | The "pin" did not authenticate the root key: whoever answers has been sent the code | The code carries the digest of the root key; the enrolling party checks the key it is answered with against the code it was given out of band; nothing comes back |
| GR-116 | Low | A public key could be enrolled under two ids, and the enrolling party chose its own id | A key is enrolled once; its id is `<name>:<anything>` |
| GR-117 | Low | Five wrong guesses cancelled a code with no record | The cancellation is an admin record, with the Authority as its actor. Not done: a limit per source, which needs a listener (G4e) |
| GR-118 | Low | A code made before an agent or gate was disabled still enrolled a key | Checked again when enrolling; and stopping an agent or gate drops its code |
| GR-119 | Low | Checks were made before the commit, not in it: two bootstraps could both name a first admin | Every action reads the chain's head first, checks, and commits on that head; if the head moved, it starts again |
| GR-120 | Low | Taking a session id as issued when the server answers under it reopens GR-76 for a server that checks no ids | Stated here: such a server vouches for any id, and is one to fix |
| GR-121 | Low | The text promised less than the code does; an admin record's time could go back | The text is changed (§7.6, choice 1); a record's time is never earlier than the one before |

The reviewer's advice for the second part, all followed: a gate's key is enrolled at the
Ledger from the record of its enrolment, with the key beside it, and no separate signed
request; the Ledger takes a gate's receipts only after it holds that record, and a record
disabling the gate retires the key; `gov verify` applies an admin record's own rules and
refuses a document of the wrong kind in either chain. Still to do, with G4c: `gov verify`
reporting a mandate id that appears in a receipt and in no record of its issue.

### Fifteenth read, 2026-11-26, G4a's second part at `21645dd`

**Verdict: no High or Critical. The gate passes for G4a, both parts.** GR-114 to GR-119 are
closed; GR-120 and GR-121 are accepted as stated. The reviewer corrected one framing: G4a is
no longer "a library nothing serves", because the Ledger's listener routes `/v1/keys` to
anyone who can reach it. The reviewer ran the tests of four crates without Postgres; the
Postgres stores and migrations were read, not run. It also read the CI failure on `36ded25`
and agreed it was the test looking too early, with no way for a real open intent to result.
The auditor's check of gate keys (GR-84) was built after this read and is unread.

| Id | Severity | What | Done |
|---|---|---|---|
| GR-122 | Medium, before G4b | An Authority's chain could be pruned like a gate's, losing the records that say which keys are trusted | The Ledger does not prune a chain of admin records; a signed snapshot of the registry would have to be designed first |
| GR-123 | Medium, before G4b | A record disabling a gate was kept first and acted on second: a stop between the two left the keys in use, since the record sent again was "already held" | What a record ends is done before the record is kept, and again when it comes again; what it grants is done after it is kept, and again when it comes again |
| GR-124 | Medium, before G4b | `/v1/keys` made the Ledger read and verify every Authority chain for each request, from anyone | The Ledger notes each enrolment, and its end, as records are taken in (migration 0005); a key offered is one lookup. Limits per source wait with GR-77 |
| GR-125 | Low | "Witnessed" overstated it: the record a key is taken on is held, and covered by the next checkpoint, not necessarily by one already | The wording below and in the code says "held"; the auditor's check asks for a checkpoint's cover |
| GR-126 | Low | The Authority's admin key at the Ledger cannot be retired or replaced, and the Authority does not check that the witness covers its own chain | Open; with key rotation in G4d |
| GR-127 | Low | An agent and a gate of the same name share the `<name>:` prefix of a key id | Open; to be settled with the bundle's key ids in G4b |

Fixed on `build/gov-review-15`; nine controls removed one at a time, each caught by a test.
Not yet read by the reviewer. Limit: the note of an enrolment is written when its record is
kept or comes again; a Ledger restored from a copy older than its chain would need its
notes rebuilt from the chain, and nothing does that yet.

**To be settled in the bundle's format before G4b is built** (the reviewer's list): the root
key signs the bundle and nothing else; the bundle names the online keys by role, each with an
id, and a gate takes an online key only from a bundle it verified under the root; the bundle
carries a number that only rises, and a gate refuses one older than it holds; once the gate
and shim load the connector map and issuers' keys from the bundle, the unsigned files work
only under an explicit test flag; `gov policy sign` is usable off the Authority's machine,
and publishing a bundle is a `PolicyPublished` record carrying its digest. Still the
owner's: how the root key is replaced if lost or stolen (§11, decision 9).

### Sixteenth read, 2026-11-26, at `3ccfb5a`

**Verdict: no High or Critical; the gate still passes.** GR-122 to GR-124 are closed (the
reviewer walked the stops on each side of keeping a record and found none that leaves a
disabled gate's key in use). The auditor's check of gate keys is sound for what it checks.
The G4b design stands in all eight points; JSON for now, and the bundle read from a file at
start, are confirmed. **G4b may be built once GR-128 is answered in the text**, which it is.
Postgres was again read, not run, by the reviewer.

| Id | Severity | What | Done |
|---|---|---|---|
| GR-128 | Medium, before G4b | Keeping the root key off the Authority's machine only helps if what it signs does not come from that machine, and the text did not say where the signer's input comes from or what the signer sees | §7.6.1, point 4: the input is the reviewed repository; `sign` runs the check itself, shows what changed since the previous signed bundle, and prints the digest it signed |
| GR-129 | Low | A receipt carried the bundle's number only, so two bundles under one number could not be told apart | §7.6.1, point 5: the policy version is the number and the digest |
| GR-130 | Low | "A receipt after its gate was disabled" compares the gate's own date with the Authority's, so it catches only a gate that dates honestly | Fixed on `build/gov-g4d4b` (2026-11-28): the witness's order decides |
| GR-131 | Low | On a Ledger restored from an older copy, the notes of enrolments can disagree with the chain | Fixed on `build/gov-review-16`: the Ledger writes its notes again from the Authorities' chains before it serves, and a chain it cannot open stops the start. Tested as a library call; that the program makes the call is read, not tested |
| GR-132 | Low | A gate or shim with no saved state accepts any validly signed bundle, an old one included | Stated in §7.6.1, point 3, as a limit until the lease names the bundle (G4d) |
| GR-133 | Low | A gate and its shim started on different bundles fail proofs for every tool whose map differs | Stated in §7.6.1, point 7 |
| GR-134 | Low | The Ledger's public keys stay an unsigned file the gate trusts | §7.6.1, point 9: they stay local configuration, and why |

### Seventeenth and eighteenth reads, 2026-11-26: G4b at `04e5117`, the G4c design and §1.2, §8.4 at `1600134`

**Verdict: no High or Critical; the gate still passes. G4b is sound as built**, GR-128 is
closed in the text and the code, and GR-129 to GR-134 are answered. **G4c may be built from
§7.6.2 once GR-138 is in the text**, which it is. §1.2 and §8.4 weaken no control; each
claimed more than the design gives in one place, now reworded. The three items of the
owner's feedback that were not taken up are, in the reviewer's words, rightly refused.
Postgres was read, not run, by the reviewer.

| Id | Severity | What | Done |
|---|---|---|---|
| GR-135 | Low | The signer's list of changes is written field by field, so a field added later would show nothing | Fixed on `build/gov-review-17`: anything that differs and is not named is shown as "other changes" |
| GR-136 | Low | The bundle's time was the signer's clock, so nobody else could rebuild the same digest from the repository | Fixed: `sign` takes the time from `--issued-at`, which a second person repeats |
| GR-137 | Low | `--yes` skips the question | Kept; it still prints the changes, and its help says they must have been shown to a person elsewhere |
| GR-138 | Medium, before G4c | The template was checked only by the Authority at issue, so whoever takes the online mandates key could sign any grants | §7.6.2, point 10: the gate holds every chain's root mandate to the template in the bundle it holds |
| GR-139 | Low | A parent agent could hand authority to any enrolled agent, whose owner never asked | §7.6.2, point 4: the child has the root person as owner, or the template names who may receive; the request names the Authority |
| GR-140 | Low | A feed that only adds cannot undo a suspension | §7.6.2, point 6: suspending revokes mandates and never adds the agent's id; only disabling does |
| GR-141 | Medium, before the scene is shown | "Verifies the refusal" claimed more than an export shows, and with the library seam PM OS is the gate's operator | §1.2 reworded; the scene runs with a separate gate holding the credential, or says whose word the refusal is |
| GR-142 | Medium, before the scene is shown | Turning the seam off is PM OS's own setting, so it is not always a recorded act | §8.4 reworded: with the library seam the cover is in line, not custody, and PM OS's operator can go round it |
| GR-143 | Low | "Names both" does not fit a record with one `about` field | §7.6.2, point 4: the record is about the child agent; the parent is in the mandate, which the record's digest covers |

### Twentieth read, 2026-11-27: G4c at `d376e20`

**Verdict: no High or Critical; the gate passes for the main spec, G1, G2, G3a, G3b, G4a,
G4b and G4c.** The template check at the gate (GR-138), issuing, delegation, the feed and
the auditor's check were read and found sound. Of the three departures, the child's id
from the request's hash stands, recording a refused mandate's call as claimed is
acceptable until G4d, and the order of stopping an agent does not stand (GR-152). The
reviewer ran the tests of `gov-core`, `gov-authority` and `gov-ledger` without Postgres;
the Authority's Postgres store was read, not run, and `gov-gate`, `gov-run` and `gov-cli`
were not re-run. **GR-152 and GR-153 are to be fixed before G4d.**

| Id | Severity | What | Done |
|---|---|---|---|
| GR-152 | Medium, before G4d | An agent's mandates were revoked before the agent was marked stopped, so a mandate issued in between stayed live under a suspended agent | Fixed on `build/gov-review-20`: revoke, mark, revoke again. Once the agent is marked nothing is issued to it, so the second round leaves none. Stopping an agent already stopped makes the round again and still answers that it is in that state, which finishes a stop that was interrupted |
| GR-153 | Medium, before G4d | A leaver's mandates for agents owned by others stayed live | Fixed: offboarding revokes every live mandate the leaver is at the root of, and suspends their agents, before the person is marked and again after. The other owner's agent and its other mandates are left alone |
| GR-154 | Low | A delegation's record names an agent in the field that elsewhere holds a person: the two cannot be told apart | Fixed: the actor has a kind, `agent` for a delegation. It is left out for a person, so earlier records read as they did. `verified` stays false for both until G4e |
| GR-155 | Low | A refused attempt to open a session leaves no trace | Fixed: the gate counts refusals by why (`mandate`, `template`, `request`, `replay`, `busy`) and shows them on the operator's address as `sessions_refused`. The asker is still told nothing more. A count only: it is lost at a restart, and the record of such attempts is G4d's |
| GR-156 | Low | A template's grants for a system that folds case are not required to be lower case | Fixed: the bundle's lint refuses it, as it does a rule |
| GR-157 | Info | CP's design is not in the repository, so nothing of CP can count as through the gate until the reviewer is given it some other way | Noted; CP is not built |

What the reviewer asked to be settled in the text before G4d is built: what a gate does
before its first lease (GR-72); how the lease and the feed are checked together; that the
lease names the bundle (closing GR-132 and GR-133) and how a gate changes bundle without a
restart; separate reasons for closing a session (GR-110); key rotation (GR-59, GR-126); and
that `gov verify` orders receipts after a retirement by the witness, not by the gate's own
time (GR-130). CM1 and CM2 were called sound as slices; each map is read when it is sent.

### Twenty-first read, 2026-11-27: the connector-map library at `f18b857`

**Verdict: no High or Critical; CM1 and CM2's command pass, for observe mode. The three
maps may not back shadow or enforce mode until GR-158, GR-159 and GR-160 are fixed and the
fixed maps are read; GR-161 is to be designed before enforce mode.** The reviewer tabulated
all 162 tools against the listings and their arguments. Like the maps, its read is against
the listings, not a live system. The builder and the drift command were found sound.

| Id | Severity | What | Done |
|---|---|---|---|
| GR-158 | Medium, before shadow or enforce | A tool was marked as undoable (`inverse`) where the server has no tool to undo it with: six in Jira, four in GitHub, all three Slack writes | Fixed on `build/gov-review-21`: each is now `none`, and so are six more found the same way (a link whose comment stays, an epic that cannot be unlinked, sprints, a version). `gov map build` refuses a tool marked as one that can be put back unless its review names the tool that does it, which must be in the map and not a read. Jira keeps 8 such tools, GitHub 10, Slack none |
| GR-159 | Medium, before shadow or enforce | `jira_transition_issue` can set fields and post a comment and was not marked outward; `jira_create_issue_link` posts a comment too | Fixed: both are outward |
| GR-160 | Medium, before shadow or enforce | "Outward" had no tight rule and was applied unevenly | Fixed: the rule is in `governance/maps/README.md` (text people see, a person told, access widened, something started). Every write was checked against it: 13 more Jira tools and 3 GitHub tools are now outward. A test holds every write that takes free text to it |
| GR-161 | Medium, before enforce | A moved Jira issue answers to its old key and a renamed GitHub repository to its old name, so a call by a former name is placed where the thing no longer is | Stated as a limit in the README. **Open: to be designed before enforce mode** (G6): the gate compares the name it was given with the one the system answers under |
| GR-162 | Low | `jira_get_cross_project_dependencies` returns other projects' issues under one project's resource | Fixed: a resource of its own, `jira/dependencies` |
| GR-163 | Low | GitHub's secret-scanning alerts and job logs sit under the bare repository, so any read grant on it reaches them | Fixed: a repository's resource ends in `code`, `issues`, `pulls`, `actions`, `security`, `discussions` or `settings`; no tool is left on the bare repository |
| GR-164 | Low | `fork_repository` writes into an account named in a second argument | Added, in the README and the tool's note |
| GR-165 | Low | The listings were taken with made-up settings; if a listing depends on settings, every tool reads as unknown in a real deployment | Each `source` now says so, and the README |

### Twenty-second read, 2026-11-27: the G4d design and the twentieth-read fixes at `c1b2c0e`

**Verdict: no High or Critical; the gate still passes. G4d may be built from §7.6.3**, with
four additions to its text (GR-166 to GR-169), which are made. The reviewer confirmed the
one departure (point 11: the `gov-authority` program in G4d, with one listener for gates'
renewals). GR-152 to GR-156 are closed, and the unmapped-tool count was found sound. The
reviewer ran the tests of `gov-core`, `gov-authority` and `gov-gate` without Postgres.

| Id | Severity | What | Done |
|---|---|---|---|
| GR-166 | Low | After the record naming a new admin-record key, the old key must be refused for later records | §7.6.3, point 9 |
| GR-167 | Low | What happens to a disabled gate was not said | §7.6.3, point 1: no answer, so no fresh lease |
| GR-168 | Low | An Authority whose clock or database goes back has its leases refused by every gate; right, but unsaid and unshown | §7.6.3, point 2: stated, with a reason of its own on the operator's address |
| GR-169 | Low | The renewal listener needs the Ledger listener's hardening, and the text should say what is not on it | §7.6.3, point 11 |

### Twenty-third read, 2026-11-27: the revised maps and CG0a at `cc6f5b5`

**Verdict: no High or Critical; the gate still passes. GR-158, GR-159 and GR-160 are
closed, so the three maps may back shadow mode as well as observe mode. Enforce mode
still waits on GR-161.** CG0a passes, and the reviewer confirmed its three defaults and
the rule that the stricter of two entries wins. It compared every tool with the maps it
read before and checked each of the eighteen remaining undo claims against the listings;
as before, against the listings and not live systems. GR-162 to GR-165 are closed or
stated.

| Id | Severity | What | Done |
|---|---|---|---|
| GR-170 | Low | A `restore` needs the earlier value, and the build checks only that a tool exists to write it back. GitHub's two subscription tools are marked `restore` and the listing has no tool that reads a subscription | Fixed on `build/gov-review-23`: a `restore` also names, in `read_with`, a tool of the map that reads the old value, and `gov map build` refuses one that does not. The two subscription tools are `none`, and so are two more that can only be named by an id nothing reads (`update_issue_comment`, `jira_remove_issue_link`). What still claims an undo: Jira 7 tools, GitHub 7, Slack none |
| GR-171 | Low | A receipt's cause may point at receipt number 0; receipts are numbered from 1 | Fixed: refused |
| GR-172 | Low | A call whose proof failed is still forwarded in observe and shadow modes, so the agent may have read the data, yet its receipt raises no label: the shadow flow check under-counts such reads | Stated in the access governance spec's status note |
| GR-173 | Info | The library's maps carry no labels, so every read raises a mandate to restricted and untrusted until a deployment labels its resources: the shadow flow check will flag nearly everything at first | Noted in the access governance spec |

### Twenty-fourth read, 2026-11-27: G4d-1 and the fixes for GR-170 and GR-171 at `25f0801`

**Verdict: no High or Critical; the gate still passes. G4d-1 passes as built, and G4d-2
may be built.** The reviewer confirmed all five departures and limits of G4d-1's status
note: the limit that compares for the same second only; a gate naming a higher revocation
number being answered from where the Authority stands; the lease naming the bundle by
number and the first 16 hex digits of its digest ("enough, since only the root's holder
could make two"); a revoked gate key not yet refused a renewal, **which must return with
key rotation in G4d-4, with its test**; and the limit being each process's own. GR-170 and
GR-171 are closed. It ran the Authority's tests without Postgres.

| Id | Severity | What | Done |
|---|---|---|---|
| GR-174 | Low, with G4d-5 | The limit for each gate is applied after the key is looked up and the signature checked, so a caller with no valid key still costs a database read each time | Open: the listener limits by source before any of that work (G4d-5, with GR-169) |
| GR-175 | Low | Every renewal opens the latest bundle again and checks each online key again: the Authority's main cost once several gates renew every few seconds | Fixed on `build/gov-g4d3`: the bundle last opened is kept and used again while the store's latest has the same digest |
| GR-176 | Low | A lease is good for the bundle's `standard` limit, two minutes by default, so a revoked agent's calls can be recorded as proven for up to that long after the revocation, plus the time between renewals | Stated in G4d-1's status note |

### Twenty-fifth read, 2026-11-27: G4d-2 at `dcb71c1`, confirmed at `e64062b`

**Verdict: no High or Critical; the gate still passes. G4d-2 passes as built, and G4d-3
may be built.** The reviewer confirmed the two choices it was asked about: the order of
reasons for a marker, with "revoked" said ahead of "no lease" ("a revocation is a fact the
gate learned from an accepted feed; no lease is the gate not knowing"); and that a proof
that holds under no lease is still used up. It ran the gate's forwarder and session tests
and the two end-to-end tests, and nothing else. Told that the lint had failed unseen at
`dcb71c1`, it confirmed from the diff that `e64062b` changes one line of a test and no
behaviour, and noted that no verdict of its has ever vouched for the lint or the format:
its verdicts rest on its own test runs.

| Id | Severity | What | Done |
|---|---|---|---|
| GR-177 | Low | A receipt's time and the mark saying where it came from are set in two steps, so around the first lease a receipt can carry the lease's time without the mark, or the machine's time with it | Fixed on `build/gov-review-25`: a receipt's time is given to the gate together with where it came from (`When`), for the intent and for the outcome each; the separate switch is gone |
| GR-178 | Low | With a lease held, a shim's request to open is compared with the Authority's time while the shim stamps it from its own machine: a shim more than two minutes off opens no session | Stated in G4d-2's limits. Fixed: a request that is properly signed and too far from the gate's time is counted under `clock`, apart from `request` |
| GR-179 | Low | After a renewal brings a newer bundle and before it is swapped in, every proof is `no_lease`; to be said, so that it is not read as the Authority being down | No longer so since G4d-3: the bundle is swapped in the same step as the lease is taken |
| GR-180 | Low | A marker that had proven a call still counts among its key's proven sessions and can be the one closed to make room, after which its calls lose the agent's name | Fixed: a marker is not among the proven sessions closed to make room |

### Twenty-sixth read, 2026-11-27: G4d-3 at `1ddddc7`

**Verdict: no High or Critical; G4d-3 passes.** GR-133 and GR-175 are closed. The reviewer
ran the tests of `gov-core`, `gov-gate`, `gov-run` and `gov-authority` without Postgres;
its findings are from reading. **GR-181 and GR-182 are to be fixed before G4d-5.**

| Id | Severity | What | Done |
|---|---|---|---|
| GR-181 | Medium, before G4d-5 | A shim that followed its gate noted the new bundle but did not keep it, so on its next start its old bundle file was refused as older | Fixed on `build/gov-review-26`: the bundle taken is written over the shim's bundle file before it is noted |
| GR-182 | Low | Under a bundle that both revokes a chain and drops its issuer's key, the session was dropped and lost its reason | Fixed: revocation is checked under the old bundle before the new keys and templates replace it |
| GR-183 | Low | Two overlapping calls could put an older bundle back in the shim's state | Fixed: one follow at a time, each reading what is held after it has the lock. No test removes this one: it needs two bundle changes inside one call |
| GR-184 | Low | The bundle is handed to anyone who knows an open session's id, which is on every receipt | Accepted for this release on a condition now stated here: **a bundle never holds a secret, and every agent of the tenant may read all of it.** With more than one tenant, or a rule an agent should not learn, the request must carry a proof under the session's key |
| GR-185 | Info | The call that crosses a bundle change fails its proof; an operator cannot tell a publish from an attack without reading receipts | Open: before enforce mode the shim retries once after it follows, or the call gets a reason of its own |

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

### 1.2 PM OS is the reference application (owner, 2026-11-26)

The owner's feedback of 2026-11-26, "PM-OS as the reference application for Agent
Governance", and the conductor's reading of it. The owner's word: "Continue with the agreed
items". What follows is what was agreed, what that changes here, and what was not taken up.

**Agreed, and now part of this spec.**

- **PM OS is the first customer of Agent Governance, not a part of it.** PM OS creates and
  runs product work; Governance decides what its agents may do, on whose authority, and
  keeps the evidence. Governance must work as well for an agent that is not PM OS's. This
  was already the rule (§9: no `gov-*` crate depends on a `pmos-*` crate); it is now also
  the reason PM OS is the first thing wired to it (slice G7).
- **The demonstration the first release is built toward** is one scene, not a dashboard:
  a PM OS agent completes a piece of work, attempts one action outside its mandate because
  a document it read told it to, is refused at the gate before the action reaches the
  system, and a person outside verifies, from the Ledger's export with public keys alone,
  **that the gate recorded a refusal and that the record has not been altered or removed**,
  trusting neither PM OS's database nor the Ledger's operator. §10 lists it as what G7 is
  done against.
- **What that person cannot verify, said in the scene itself** (review finding GR-141). A
  receipt proves what the gate said, not what reached the system (T9). And where the gate
  is a library inside PM OS's tool server (§8.1), PM OS holds the credential and is the
  gate's operator: the refusal is then PM OS's own word, signed. So the scene is run with
  a **separate gate process holding the only credential**, where the party believed is the
  gate and not the application; run with the library seam, it is described as "in line",
  and the words "PM OS could not have done it" are not used.
- **No fallback.** For a system a deployment has put in enforce mode, PM OS has no
  authorization path of its own to fall back on when Governance cannot decide (§8.4).
- **Three ways to install** stay as §1 has them: PM OS alone, Governance alone, both.

**What the demonstration needs that is not built**, so that nothing is claimed early: an
Authority that issues the agent's mandate to a named person's account (G4c, G4e); the
gate's time and revocation (G4d); shadow mode (G5); enforce mode with the gate holding the
credential (G6); the seam (G7). Until G6 the gate blocks nothing, and the scene may not be
shown or described as working enforcement. In shadow mode it can be shown honestly as
"would have been refused".

**How the scene is written, so that the refusal is real.** The gate refuses by operation
and resource, as the connector map reads a call. An agent permitted to read feedback that
reads all of it, one call at a time, has done nothing its mandate forbids: "bulk export"
is not an operation unless a tool makes it one, and limits on volume are not designed
(budgets, G6, count calls; they do not know what a record is). So the scene's unauthorized
action is a different operation or resource from the permitted ones: sending to an outside
address, or reading a resource the mandate does not cover, such as another project's
issues. That is also the more honest picture of what a mandate stops.

**Not taken up, and why** (the owner asked for the agreed items only; these stay open for
the owner to press):

- *A decision interface* (`GovernanceRequest` in, `ALLOW`, `DENY` or `REQUIRE_APPROVAL`
  out). An application that asks and then acts with its own credential is governed only
  while it chooses to ask. The proposal's own first line, that an agent never holds a key,
  needs the gate in the path of the call, holding the credential, which is what is built.
- *Identity, mandate and delegation chain as text fields in a request.* Those are claims.
  Here the agent proves its key on every call and the mandate is a signed document the
  gate verifies; a receipt says `proven` or `claimed` for this reason.
- *Enforcement first.* Observe, then shadow, then enforce stays the order: the reviewer
  has made closing named findings a condition of enforcing, and a customer should see what
  would be refused before anything is.

**In the feedback's list and already here under another name:** signed receipts and the
check an outsider can run (built); agent identity as an enrolled key (built); human
approval as the countersignature (G6); revocation (G4d); the console (G8); undo (G9),
which the feedback does not mention and which answers a question it leaves open, what
happens after an agent was allowed to do the wrong thing.

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

Added on 2026-11-26 with the gate-served approval page (review finding GR-31). The gate
becomes a web server for people; each row needs its test in G6.

| # | Threat | Control | Slice |
|---|---|---|---|
| T23 | An argument an agent chose runs script on the approval page, and the script asks the passkey for anything | The page is built by the gate with strict encoding, a content policy with no inline and no outside script, and nothing else is served from that address | G6 |
| T24 | Display tricks hide what is approved: a very long value, direction-override characters, look-alike letters | The approve button is offered only when every argument is shown whole in a form with those characters made visible; if the gate cannot show an argument so, there is no signed approval for that action | G6 |
| T25 | A compromised Authority sends the approver a look-alike address | A passkey does not sign there. The sign-in could still be collected, so a sign-in alone never enrols a passkey (§4.4) and the product tells approvers the one address | G6 |
| T26 | Cross-site request forgery on "approve"; theft of the approver's session at the gate | The approval is the passkey's signature over the challenge, not a button press under a session; the challenge is single use and expires, so a signature collected once cannot be presented again; sessions are short, bound to the browser, and same-site only | G6 |

T12, with undo (review finding GR-35): undo does not make an injected agent's writes safe. An
action whose effects leave the system or widen who can see something asks for a person even
when it can be put back (§4.7).

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

- **The salt** (review findings GR-36 and GR-44). `args_digest` is over a random 32-byte salt
  and the arguments, because a plain digest of a small value gives the value away to anyone
  who can guess it. Rules:
  - Whoever first forms the action draws the salt, from the system's random source. With the
    shim that is the shim: it must sign the action's digest before the gate has seen the
    call, so it draws the salt and sends it with the call. A salt the agent chooses cannot
    weaken the binding: the gate recomputes the digest itself from the arguments and the salt
    it was sent, and the request proof must match that.
  - The gate refuses a salt it has already seen in that session. A salt is never fixed per
    session or per agent.
  - An action waiting for a countersignature keeps its salt with its readable form until it
    is decided, so the page the approver reads recomputes the same digest.
  - After the action the salt is deleted with the arguments. From then on the digest in a
    receipt cannot be matched to any arguments, by anyone. **A receipt therefore shows that
    an action of this shape on this resource happened, and binds an approval to it at the
    time; it does not prove afterwards what the arguments were.** The product says so.
    Keeping salts longer, sealed at the customer's gate for a stated time, is a setting to
    consider with undo (G9), which keeps before-values the same way.
  - PM OS's own `args_hash` is unsalted, so in the seam (G7) the two digests are unequal by
    construction and both are carried.
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
  The reviewer's three conditions hold in this design and are requirements on G6: the passkey
  is registered to the gate's address, never one the Authority serves (a passkey answers any
  challenge its registered address asks for); the gate builds the challenge from the bytes it
  will send and its page loads nothing from the Authority; roster changes stay witnessed.
  Requirements on G6 from the third read (GR-29 to GR-33):
  - **Enrolling a passkey takes more than a sign-in:** an admin's approval, or a waiting time
    with a notice to the person. Every enrolment and removal is a receipt in the gate's chain,
    so it is witnessed. The assertion, the credential id and the challenge of each signed
    approval are kept, so it can be checked again later against those receipts and is not
    only the gate's say-so.
  - **One exact address per gate, used for nothing else, never a parent domain.** A passkey
    answers any challenge its registered address asks for, so anything else under that
    address could collect a signature for a waiting action. If gates must share a front, that
    front is trusted like a gate and the product says so.
  - **A local floor.** The gate's own configuration, not the Authority's bundle, can list
    operations that always need a signed approval; the bundle can add to it and never take
    away. Without it a compromised Authority publishes a policy that asks for none, which is
    detectable (T18) and not prevented.
  - **The gate's identity-provider settings and passkey address are its own configuration,**
    never read from the bundle.
  - **Class is tied to issuer:** a signed countersignature is accepted only under this gate's
    own approval key (a key apart from its receipt key), an attested one only under the
    Authority's.
  - If the gate cannot show an argument whole and plainly, there is no signed approval for
    that action (T24). Proposed library:
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
- **`before_digest` on an intent receipt:** the salted digest of the target's values before
  the write (salted because a plain digest of one short field gives the field away to anyone
  who can guess; review finding GR-36. The arguments' digest is salted the same way). The values themselves stay in the gate's sealed store at the customer for the undo
  window; the receipt and the Ledger hold only the digest.
- **`reverses` on a receipt:** the gate, number and hash of the receipt an undo puts back.
- **`reversible` in a policy rule's match,** so the default rule is one line: an operation
  with no undo needs a countersignature.

How undo will work (G9):

1. At intent, for a `restore` or `inverse` operation, the gate reads the fields the write will
   change through the same connector it already uses to read `resource_version`, seals them,
   and writes their digest in the intent receipt. If that read fails, the action is treated as
   having no undo: policy is evaluated with `none`, and the receipt's `reversal` records
   `none`, the class actually in force, not the connector map's (GR-38). The before-read is
   made with the gate's credential and gets a receipt of its own.

   1a. An operation is `restore` only where its write is checked against the version the
   before-values were read at. Where a system has no version to check, someone else's change
   between the read and the write would be lost by a later undo, so the class is `none`
   (GR-37).
2. **Undo is a governed action, evaluated as what it does** (GR-34). Putting back is not
   harmless: the undo of "remove a member" grants access, of "make private" exposes, of
   "close" reopens, and the undo of "create" is a delete. So there is no general "undo"
   operation for policy to allow wholesale. An undo is the concrete operation it performs
   (the opposite operation, or the update, on that resource, with that operation's own kind),
   checked against the requester's own mandate and every rule that operation would meet,
   countersignature included. It has its own intent and outcome receipts with `reverses` set.
3. **It refuses rather than overwrite.** If the target's version is no longer the one the
   original write left, someone has changed it since; the undo stops and says so, as a version
   conflict does. Nothing is forced. This holds for `inverse` too: an issue others have since
   added to is not deleted.
4. **A task is undone as a whole:** every action under one mandate, newest first, stopping at
   the first conflict and reporting what was and was not put back.
5. The undo window (default seven days, a figure to decide) is how long the sealed values are
   kept. After it the receipts remain and the undo is gone.
6. Each system's page in the product shows its undo cover, as it shows custody: which
   operations restore, which invert, which cannot be put back.

**The default rule, corrected (GR-35).** "Can be put back" is not "safe without a person".
Many reversible writes have effects that are not put back: a notification or an e-mail, a
webhook or an automation, something made visible to more people for a while. An agent turned
by injected text would get all of those unapproved. So the connector map carries a second
flag per operation, `outward`: its effects leave the system or widen who can see something.
The default asks for a person when an operation cannot be undone **or** is outward, and
`reversible: true` never lowers a destructive operation by default. The customer's rule is:
approve what cannot be undone and what reaches outside; undo the rest. `outward` is not yet
in the formats; it joins the connector map in G3.

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

- **The MCP gate.** The gate stands between the agent's MCP client and one upstream MCP
  server, with its own small forwarder (the status notes say why PM OS's MCP client layer is
  not shared). What the gate reads is what the server gets: a request is parsed strictly and
  written out again, never forwarded as it arrived. A connector map builds a call's resource
  only from the arguments the server itself uses to pick its target.
- **The shim.** A small program the agent's runtime starts as a local MCP server. It holds the
  agent's private key and current mandate, opens a session with the gate (receiving the
  challenge), signs every call and forwards it. The agent's MCP client is pointed at the shim by
  configuration; the agent's code does not change. The key file is readable only by the shim's
  user. (T13)
- **Stock client, no shim.** Allowed in observe mode only. The receipt records `proof:
  claimed`.
- **The library.** The same decision code as a Rust crate with no network listener, for PM OS's
  tool server (§8).

#### 5.3.1 The shim's protocol (design of 2026-11-26; not built)

Written before code. The reviewer read it on 2026-11-26 and said it may be built once
findings GR-88 to GR-91 were answered in this text; they are, with GR-92 to GR-96, and each
is marked where it lands. It covers observe mode; what enforce mode adds is marked.

**What the shim is.** A program the agent's runtime starts beside the agent. To the agent it
is the MCP server (HTTP on this machine only, on a port of its own). It holds two files: the
agent's private key, and the agent's mandate chain. It forwards every message to the gate
and adds a proof to each tool call. The agent's own code and credential do not change.

**Who may use the shim** (GR-90). A port on this machine can be reached by more than the
agent: by any local process, and by a web page open in a browser here. So, in the first
build: the shim makes a random secret at each start and hands it only to the runtime that
started it, and every request must carry it; a request with an `Origin` header, or a `Host`
that is not the shim's own address, is refused; only `application/json` is taken. A socket
only the runtime's user can open follows later.

**Where the shim sends** (GR-96). To its configured gate only, under the programs' rule:
HTTPS, or plain HTTP only to a literal loopback address.

**Opening a session.** The shim posts its mandate chain to the gate (`POST /v1/sessions`).
The gate verifies the chain (signatures, tenant and environment, expiry, each link inside
its parent) against the issuer keys it trusts, makes a session id and a random challenge,
binds the session to the one agent key the leaf mandate names, and answers with both. One
session carries one mandate. Sessions live in the gate's memory: after a restart the gate
does not know the session (see "An unknown session" below). A session ends after an idle
time and after a fixed number of calls, and the gate checks the mandate's expiry on every
call, not only at opening (GR-93).

Opening a session proves nothing about who opened it: a mandate chain is not a secret.
Possession of the key is proven on every call, never before. So an opened session is
**unproven** until its first valid proof (GR-88):
- an unproven session expires in seconds, and unproven sessions have a small cap of their
  own per agent key;
- only a proven session counts against the agent's cap of sessions;
- a proven session is never closed to make room for an unproven one. Someone holding a copy
  of the chain can therefore fill the unproven sessions for a few seconds at a time, and
  cannot close the real shim's session.

**An unknown session** (GR-89). The shim never sends a call twice unless the gate has said,
in a way the shim can tell apart, that the call was not forwarded.
- In observe mode a call under a session the gate does not know is a failed proof: the call
  is forwarded once, the server's answer is returned, and a response header tells the shim
  to open a new session before its next call.
- In enforce mode (G6) the refusal comes before anything is forwarded and says so, and the
  shim opens a session and sends the call again.

**A tool call.** The shim reads the agent's request with the same strict reader the gate
uses and forwards its own writing-out of it, so that what it signs is what it sends
(GR-94). It sends tool calls to the gate one at a time, so the counter rises in the order
the gate sees (GR-92). Then it:

1. draws a fresh 32-byte salt (§4.3);
2. maps the call to a canonical action with **the same connector map the gate uses** and the
   tool's definition as the server last listed it in this MCP session (the shim sees the
   listing pass through it, as the gate does);
3. signs a request proof (`gov.request.v1`) over the gate, the session, the challenge, the
   next counter, the mandate's id and that action's digest;
4. sends the call to the gate with three headers: `x-gov-session`, `x-gov-proof` (the signed
   proof) and `x-gov-salt`.

The gate maps the call itself, from its own map, its own record of the listing, the
arguments it parsed and the salt it was sent, and gives the result to `Session::accept`.
**A receipt says `proven` only when `Session::accept` returned success for the action the
gate itself computed**; it then names the mandate chain's ids and takes the agent's id from
the mandate, never from a header. The gate refuses a salt it has seen in that session.
None of the `x-gov-*` headers is sent upstream.

**When the proof does not hold** (a bad signature, a counter that did not rise, a repeated
salt, a digest that differs because the shim's map or listing is not the gate's):
- in observe mode the call still goes through, because observe mode blocks nothing. Its
  receipt says **`failed`**, a third value beside `proven` and `claimed`, with a reason from
  a closed list (GR-91). A receipt's own rules require the reason exactly when the value is
  `failed`. `claimed` stays what a stock client with no shim gets; a failed proof is a
  different fact: someone presented this agent's session and could not prove the key. The
  receipt may name the mandate's agent, and nothing counts it as that agent's act. Failed
  proofs are counted and reported apart from claimed ones.
- arguments with no canonical form are digested over their bytes, which the shim and the
  gate only agree on when both are one build. Such a call is `unknown` in any case; its
  proof is expected to fail and is recorded as `failed` with a reason of its own, so that
  it is not read as an attack (GR-95).
- in enforce mode (G6) the call is refused at step 1 of §5.1.

**Where the map comes from.** Until the Authority exists (G4) the shim reads the connector
map from a file, as the gate does. From G4 both read it from the signed policy bundle. If
the two differ the proof fails for the tools that differ, and the receipts say so.

**Considered and not chosen:** the gate telling the shim what action to sign (one more round
trip per call, and the shim would sign whatever a gate, or something answering as one, told
it); and a proof over the tool's name and arguments instead of the canonical action (it
would change the request proof reviewed in G1, and an approval binds the action's digest).

**Stated limits.**
- The shim signs whatever reaches it with its secret. Whatever holds that secret acts as the
  agent: the runtime, and anything that can read the runtime's memory or environment. A
  socket only the runtime's user can open comes in a later step, and each integration's spec
  says where the secret lives (T13).
- The key and the mandate are files, under the rule of §9: fake data only.
- Until G4 the mandate is issued by a test issuer whose public key the gate is given in a
  file. "Proven" then means "holds the key a mandate from that test issuer names", no more.
- Revocation and the lease are not checked in observe mode before G4: there is nothing to
  check them against.
- A stock client with no shim still works, in observe mode only, and is recorded as claimed.

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

### 7.6 How G4 is built (design of 2026-11-26; not built)

Written before code, as §5.3.1 was for the shim. The reviewer read it on 2026-11-26
(thirteenth read) and said G4a may be built once GR-107 and GR-108 were answered in this
text; they are, with GR-109 to GR-111, each marked where it lands. §7.1 to §7.5 say what the
Authority is; this says how the slice is cut, what each step replaces, and the choices that
were open.

**What G4 replaces.** Three things in G3b are stand-ins, each stated as a limit: the gate's
connector map and the mandate issuers' keys are unsigned files; mandates come from a test
issuer; nothing is revoked and the gate's time is its own machine's. G4 ends each of them.

**Steps, each sent to the reviewer.**

| Step | Built | Ends which stand-in |
|---|---|---|
| G4a | `gov-authority` as a library with a store (memory and Postgres, in Governance's own database): the registry of agents and gates, enrolment by one-time code, and the Authority's own record of what its admins did | Keys reach the Ledger and the gates by enrolment, not by a file an operator copies |
| G4b | The policy bundle: `gov policy check` and `gov policy sign`, the bundle served to gates, and the gate and shim loading their connector map and the issuers' keys from it | The unsigned map and key files |
| G4c | Mandates: issued from a template to an enrolled agent by a person, delegated by a parent agent, listed and revoked; the numbered revocation feed | The test issuer |
| G4d | The lease: renewed by the gate with a fresh nonce, carrying the Authority's time, the bundle's version and the revocation number; the gate's time taken from it; a session refused or closed for a revoked agent, mandate or key; key rotation for a gate | The gate's own clock (GR-72); no revocation; GR-59 |
| G4e | People: sign-in against Entra, Governance's own roles and users table, the admin interface over HTTP, and the `gov-authority` program | Admin actions made by whoever can run a command |

**Choices made here, for the reviewer to confirm or refuse.**

1. **Admin records are their own document, not receipts** (a departure from §7.4's wording;
   the reviewer agreed, on the conditions of GR-108, which follow). A receipt is about an
   agent's action on a system: its rules (an action's digest, a reversal class, a mode, a
   proof) do not fit "Ana enrolled the agent `refund-bot`". Bending them would weaken the
   receipt's own rules. So: a signed document `gov.admin.v1` (who, what, on what, when, the
   policy version, and for a change the digest of the new value), numbered and chained like
   receipts, signed by the Authority's admin-record key, shipped to the Ledger into the
   Authority's chain, listed in the same checkpoints and checked by the same `gov verify`.
   §7.4's point stands: every admin action is covered by the witness. And (GR-108):
   - **Chains have a kind.** A chain at the Ledger takes one kind of document. A key is
     enrolled for receipts or for admin records, never both: a key enrolled for admin
     records is refused on a receipt, and a gate's key on an admin record.
   - **Record and effect together.** An admin action's record and what it changes are
     written in one step, so there is never one without the other (as built, and stronger
     than the "record first, completed at start" this text first said: GR-121). A record's
     time never goes back along the chain.
   - **Issuing a mandate is an admin action** and is recorded, with the mandate's id. A
     mandate id that appears in a gate's receipt and in no admin record is then something
     `gov verify` can report: a mandate the witness never saw issued.
   - **No free text.** "What" is one of a closed list of actions; "on what" and "who" are
     identifiers; anything longer is carried as a digest.
2. **The Authority's keys, by role** (GR-109, to be built in G4b). A **root key** signs the
   policy bundle and nothing else; it is used on release, not while serving, and can be kept
   off the Authority's machine. The bundle names the **online keys** by role: one for
   mandates, one for leases and the revocation feed. A gate is configured with the root's
   public key and takes the others from a bundle it has verified. Taking the Authority's
   machine then means forging leases and mandates until the bundle is reissued, not forging
   policy. A further key, enrolled at the Ledger for admin records only, signs those. All
   are files under the rule of §9 until a key service exists. Losing the root is losing the
   installation; the spec says so, and its rotation is designed in G4d with GR-59.
3. **Enrolment** (GR-107). An admin makes a code: 128 random bits, shown once, bound to one
   named agent or gate and to the admin who made it, good for 15 minutes and for one use.
   The Authority stores only its digest.
   - The request to enrol **presents the code itself**, never only its digest: the digest is
     what the Authority stores, so it must not be enough to enrol. The code therefore
     travels only under the programs' connection rule (HTTPS, or a literal loopback
     address), and is used up by the first request that presents it, right or wrong.
   - The request is signed with the key being enrolled, over the code's digest, the
     Authority's name, the agent's or gate's own name, and the public key. A key anyone
     could forge for is refused (GR-6), as is a key or key id already enrolled.
   - Wrong codes for one name are counted; after five the name's pending code is cancelled
     and an admin must make another.
   - **The code names the root key to pin** (GR-115, which replaced what this text first
     said). The code the admin hands over is `<secret>.<digest of the Authority's root
     key>`. The answer to a request to enrol carries the root key, and the gate or agent
     pins it only if its digest is the one in the code it was given out of band. No value
     computed from the code comes back: anything answering in the Authority's place has
     been sent the code and could compute it.
   - A key's id is `<name>:<anything>`, under the name it is for, and a public key is
     enrolled once, under one id (GR-116).
   - **A gate's key reaches the Ledger through the Authority's chain.** The Ledger is given
     the Authority's admin-record key at installation. A gate's key, offered to the Ledger
     by anyone, is taken only if the Ledger's own copy of the Authority's chain holds the
     record of that key's enrolment for a gate, with a key of exactly that digest, and no
     later record disabling the gate or revoking the key. No signature on the offer is
     asked for or needed: the record is signed, numbered and witnessed. A record that
     disables a gate, or revokes a key, retires that key at the Ledger when it arrives. The
     `gov-ledger enrol` command stays for an installation with no Authority.
4. **An agent has one owner, a person.** Offboarding the person suspends their agents and
   revokes those agents' mandates in one admin action with one record each.
5. **Revocation in observe mode** (with GR-110, to be built in G4d). The gate still blocks
   nothing. A call under a session whose agent, mandate or key is revoked is forwarded and
   recorded as a failed proof, with one reason for a revoked key and another for a revoked
   agent or mandate: the caller did hold the key, and the receipt must not say otherwise.
   The session is closed, and the gate keeps a marker for that agent key, so that the calls
   the shim goes on to make with no session are still recorded as that agent's, revoked,
   and not as an unnamed client's claim. Enforce mode (G6) refuses them.
6. **GR-81.** The Authority lints a mandate's grants for a folded system at issue, and the
   chain check refuses an upper-case grant for one.
7. **GR-84.** The trusted-keys record an auditor gives `gov verify` is written by the
   Authority from its registry at enrolment time, as admin records the witness covers, not by
   the Ledger being audited. The anchor: an auditor holds two public keys from outside the
   installation, the Authority's admin-record key and the Ledger's key; every other key
   follows from the verified admin chain.
8. **Sign-in comes last (G4e)** (with GR-111). Until then the admin interface is a library
   and the `gov` command, usable only by someone on the Authority's machine, and:
   - every admin record made before G4e marks its actor as **unverified**, as `claimed` does
     for an agent, so that none reads as a person's proven act;
   - no admin function is served on any listener until G4e;
   - a mandate issued in G4c names a person nobody signed in, so the rule that a mandate
     is issued only inside its issuer's live signed-in session (§2) does not hold until G4e. Whether `crates/entra` can be
   depended on without PM OS's roles (§7.2) is checked first in G4e; if it cannot, the
   verifier is written again in Governance, as the tool digest was.

#### 7.6.1 G4b, the policy bundle (design of 2026-11-26; not built; for the reviewer)

Written to settle the reviewer's list from the fifteenth read before any code.

1. **The bundle's format.** `gov.policy.v1` gains three things; a bundle without them is
   refused.
   - `number`: a whole number that rises by exactly one with each published bundle, from 1.
     `version` stays as the label a person reads; order is decided by `number` alone.
   - `keys`: the Authority's online keys by role, `mandates` and `leases` (the lease key
     also signs the revocation feed), each a list of public keys with ids. A key appears
     under one role only, and the root key under none: a bundle that lists its own signer,
     or one key under two roles, is refused.
   - `connectors`: as now, the connector map, which the gate and the shim both take from
     the bundle.
2. **The root key signs the bundle and nothing else** (GR-109). A gate, a shim and the
   Authority are each configured with the root's public key, and use it only to open a
   bundle: it is never placed in a key set that opens a mandate, a lease, a receipt or an
   admin record, and those documents' type tags differ from the bundle's, so a signature by
   the root over anything else opens nowhere. An online key is taken only from a bundle
   that opened under the root.
3. **No going back.** A gate and a shim each keep, beside their other state on disk, the
   number and digest of the newest bundle they have accepted, written before the bundle is
   used. A bundle with a lower number is refused. The same number with another digest is
   refused and counted: two bundles under one number means the root key signed twice or
   was copied. A gate or shim with no saved state accepts any validly signed bundle, an old
   one included, so wiping that state undoes the removal of an online key until the lease
   names the bundle a gate must hold (G4d): a stated limit of G4b (GR-132). A higher number
   is accepted whatever the gap (a gate may have been off for
   several releases); the Authority, which publishes, accepts only the next number.
4. **Three commands, and where each runs.**
   - `gov policy check <dir>`: reads the policy files, builds the bundle, and runs every
     check a gate would (rules, key roles, each connector map), needing no key. Each
     connector map in the directory must come with a file of sample calls and the action
     each is to be read as; the check runs them and fails without them (the standing
     condition of GR-65).
   - `gov policy sign --root-key <file> --number <n> --previous <signed bundle>` (GR-128).
     Its input is the policy directory **as checked out from the reviewed repository**
     (threat T18), never a directory or a built file carried from the Authority's machine:
     were it otherwise, whoever took that machine would add a key of their own under
     `mandates` and the root would sign it. So `sign` takes the directory, not a built
     bundle, and runs `check` itself. It reads the previous signed bundle (required for
     every number but 1), verifies it under the same root, requires the next number, and
     prints what changed before it signs: online keys added and removed, by role; rules
     added, removed and changed; connector maps added, removed and changed. It asks for a
     typed yes unless told `--yes`, and prints the digest it signed, which the person
     publishing compares with the digest the Authority reports. It opens no database and
     no network connection.
   - Publishing, on the Authority: a library function for now (no listener until G4e). It
     opens the signed bundle under the root key, takes only the next number, and writes the
     bundle and a `PolicyPublished` admin record carrying its number and digest in one
     step. A bundle a gate holds whose digest has no such record is then something
     `gov verify` can report, as with mandates.
5. **The unsigned files go.** `gov-gate` and `gov-shim` take `--bundle` and `--root-key`
   in place of `--map` and `--mandate-issuers`. The old options remain only with
   `--unsigned-policy-for-tests`, refused unless the data class is fake, and a gate started
   that way says so in every receipt's policy version (`unsigned`), so no record made
   without a bundle reads as made with one. A receipt otherwise carries the bundle's number
   and the start of its digest (`<number>-<16 hex digits>`), so that two bundles under one
   number, the case point 3 counts as an alarm, can be told apart in the record (GR-129).
6. **Policy source is JSON for now**, not the YAML §4.6 names: the strict reader the gate
   already uses for everything it parses refuses a repeated key and an unknown field, and a
   YAML reader would be a new dependency on the path that decides what is trusted. YAML,
   if wanted, is converted before `check` and is not what is signed.
7. **Delivery.** In G4b the gate and shim read the bundle from a file at start. Fetching
   it from the Authority, and changing it without a restart, come with the lease (G4d),
   which names the bundle a gate must hold. Until then a gate and its shim can be started
   on different bundles; every tool whose map differs then fails its proof, with a reason
   that says so, until both run the same one (GR-133).
8. **An agent and a gate may not share a name** (GR-127). A key's id is under the name it
   is for, so two things of one name could take each other's key ids. The registry refuses
   to register an agent under a gate's name, or a gate under an agent's, and the
   Authority's own name is refused for both.

9. **The Ledger's keys stay local configuration** (GR-134). The gate checks the Ledger's
   acknowledgements and checkpoints against public keys from a file. They are not put in
   the bundle: the Ledger is the party that checks on the Authority's records, and an
   auditor is meant to hold the Ledger's key from outside the installation (§7.6, choice
   7); a bundle naming the Ledger's key would let whoever signs policy choose who witnesses
   it. A wrong file there cannot make a receipt: it can only make a gate believe, or
   refuse to believe, that its receipts were taken.

**Not built in G4b:** rules are still not evaluated against a call (G5); the lease and
revocation (G4d); mandates from the Authority (G4c), so the `mandates` keys in a bundle are
a test issuer's until then.

#### 7.6.2 G4c, mandates from the Authority (design of 2026-11-26; not built; for the reviewer)

Ends the test issuer. Written before any code; nothing here is served until G4e, so every
function below is a library call, usable only by someone on the Authority's machine.

1. **Templates are policy.** The bundle gains `templates`, a list. A template has an id,
   the grants a mandate from it may carry at most, the longest life (never more than the
   bundle's own cap, default 8 hours), limits, whether a mandate from it may be delegated,
   and who may ask for it: the agent's owner always, and beyond that only the people or
   roles the template names. A mandate says `template: "<id>@<bundle number>"`, and the
   signer is shown templates added, removed and changed like rules (§7.6.1, point 4).
2. **Issuing to a person's request.** The Authority issues a mandate when all of these hold,
   checked on the chain's head it then commits on:
   - the person is active, and is the agent's owner or one the template names. Being an
     admin is not enough: an admin manages the registry, and does not thereby hold every
     agent's authority;
   - the agent is usable (§7.1) and the mandate is bound to one of its enrolled keys, named
     in the request;
   - the template is in the **latest published bundle**; the grants asked for are inside
     the template's, by the same containment check a gate applies to a chain; the life
     asked for is within the template's;
   - the key the Authority signs with is listed under `mandates` in that same bundle. An
     Authority whose mandates key the root has not named issues nothing.
   The mandate's id is 128 random bits from the Authority and is never reused. The mandate
   and a `MandateIssued` record (the mandate's id, the agent, the digest of the signed
   mandate) are written in one step.
3. **Nobody is signed in until G4e**, and the mandate says so. `issued_by.session` is
   absent on every mandate issued before then, as the admin record's actor is unverified.
   A condition on G6, stated now: a gate in enforce mode refuses a mandate whose root has
   no session. So no mandate issued by whoever could run a command can ever back an
   enforced action.
4. **Delegation.** A parent agent asks, in a document of its own (`gov.delegate.v1`: the
   parent mandate's id, the child agent and its key id, the narrowed grants, the life, a
   nonce, the time), signed with the key the parent mandate is bound to. The Authority
   issues the child only if: the parent mandate verifies now and is not revoked; its
   template allows delegating; the child is another enrolled, usable agent; the child's
   grants and life are inside the parent's; the chain would be no deeper than 3; the nonce
   has not been seen and the time is within the clock allowance; and the request names
   this Authority, so it cannot be used at another. **The receiving side agrees** (review
   finding GR-139): the child agent's owner is the person at the root of the chain, unless
   the template names other owners or agents who may receive from it. The child embeds the
   parent's signed mandate, so a gate checks the chain without asking the Authority. A
   delegated mandate's `issued_by` is the parent agent; the person at the root is found by
   walking the chain. The `MandateIssued` record is about the child agent, as for any
   mandate; the parent is inside the mandate whose digest the record carries (GR-143).
5. **Listing.** A person sees the mandates of agents they own; an admin and an auditor see
   every mandate's id, agent, template, issuer, times and state, and never its grants'
   resources beyond what the template already says. (Listing is what the admin interface
   serves in G4e; here it is a library call with the same rule.)
6. **Revoking.** The person who issued a mandate, the agent's owner, or an admin may revoke
   it. One entry is enough for its descendants: a gate checks every link of a chain against
   the revocations it holds. Offboarding a person, or suspending or disabling an agent,
   revokes that agent's live mandates, each with its own `Revoked` record, in the same step
   as the change that caused it (§7.6, choice 4). The feed only ever adds, so it carries
   nothing that must later be undone (review finding GR-140): suspending an agent revokes
   its mandates and never puts the agent's id in the feed, and an agent resumed is issued
   new mandates; only disabling an agent puts its id there, for good.
7. **The revocation feed.** Every revocation takes the next number, from 1, with no gaps,
   and carries the kind, the id and the time after which the thing would have expired
   anyway. The feed a gate fetches is "everything after number N": a signed document
   (`gov.revocations.v1`: from, through, the entries, and the newest number there is),
   signed with the **leases key**, which the bundle names, and never the admin-record key.
   An entry is left out of the feed only once its expiry has passed by more than the clock
   allowance, and the numbers of dropped entries are never reused, so a gate that asks from
   an old number is told the range it missed was dropped rather than shown a shorter feed.
   The gate's use of the feed, and the lease that says how far a gate must have read, are
   G4d.
8. **`gov verify` reports a mandate nobody issued.** A receipt marked `proven` names its
   mandate ids. With an Authority named, an id with no witnessed `MandateIssued` record is
   reported, as a gate key with no enrolment record is (GR-84). This makes a forged or
   test-issued mandate visible to an auditor even in observe mode.
9. **Three keys at the Authority, three uses.** The admin-record key signs admin records;
   the mandates key signs mandates; the leases key signs the revocation feed (and leases,
   G4d). Each is refused for the others' documents by type tag and by key set, as the root
   is for anything but a bundle. All are files, for fake data only, until a key service
   exists.

10. **The gate holds a mandate to its template** (review finding GR-138). The mandates
    key is online, so a mandate's signature alone must not be worth more than the root
    allowed. When a gate verifies a chain it also checks the chain's root mandate against
    the template of that id **in the bundle the gate holds**: the grants inside the
    template's, the life within the template's, the limits no looser, and, if the chain
    has a child, that the template allows delegating. Taking the Authority's machine then
    yields only what the reviewed, root-signed templates already allow. The number in
    `template: "<id>@<number>"` records which bundle the Authority issued from; it does not
    choose which template governs. **The template in the held bundle governs**: narrowing a
    template tightens every live mandate from it at once, and removing a template ends
    them. In G4c this check is built into the chain check in `gov-core` and used by the
    gate's sessions; with no grants yet checked against a call (G5), what it changes today
    is which mandates a session is opened for.

**Not built in G4c:** anything served over HTTP (G4e); the gate reading the feed, closing
sessions, and its clock (G4d); grants checked against a call (G5); limits charged (G6); a
limit on how many mandates may be issued in a time, which belongs with the listener.

**Not in G4:** countersignatures and approvals (G6), shadow decisions (G5), the console's
screens (G8). A mandate's grants are still not checked against an action until G5, so a
proven receipt is still not an authorised one.

#### 7.6.3 G4d, the lease at the gate (design of 2026-11-27; read by the reviewer, who cleared it to be built; not built)

Makes revocation take effect and ends the gate's own clock. It settles what the reviewer's
twentieth read asked to be in the text first. The lease, the held lease and the freshness
check exist in `gov-core` since G1 (§4.6); nothing has used them yet.

1. **One request, one answer.** A gate renews with one signed document, `gov.renew.v1`:
   its name, the Authority's name, a fresh random nonce, the bundle it holds (number and
   digest, or none) and the revocation number it has read through. It is signed with a key
   enrolled for that gate. It carries no time: a gate may not have one yet.
   The Authority answers a gate that is not disabled, for a key that is its own and not
   revoked. **A disabled gate gets no answer** (GR-167): it then has no fresh lease, so
   its calls are recorded as `no_lease` in observe mode (point 5) and refused in enforce
   mode. The answer is three things made from one reading of its store:
   - a **lease** (§4.6), signed with a leases key of the latest published bundle, carrying
     the request's nonce, the Authority's time, that bundle's version (number and digest)
     and the newest revocation number;
   - the **revocation feed** after the number the gate named, always, even when empty;
   - the **bundle**, when the gate does not hold the one the lease names.
   A renewal changes nothing at the Authority and writes no record. A copy of a request
   played by someone else gets an answer bound to that nonce, which no gate is waiting for.
   The Authority answers one gate at most once a second.
2. **The gate takes the whole answer or none of it**, checking in this order:
   - the bundle, if one came: it opens under the pinned root key and may replace the one
     held (§7.6.1, point 3): never a lower number, never another bundle under the same
     number;
   - the lease: it opens under a leases key **of the bundle it names**, which must now be
     the bundle in hand; it is for this gate and carries the nonce of the request the gate
     sent and has not yet used; its time and its revocation number are not lower than the
     held lease's (GR-1, as built);
   - the feed: it opens under a leases key of the same bundle; its issuer is the lease's;
     its `after` is the number the gate asked for; its `through` is not lower than the
     lease's revocation number; its entries are numbered in order, and entries and the
     count of those left out account for every number between `after` and `through`.
   Then the bundle is put in use (point 7), the entries are added to the gate's revoked
   set, and the lease is held. If any check fails nothing is changed, the gate keeps what
   it had, and the operator's address says which check failed.
   **An Authority that has gone back** (GR-168). If the Authority's clock is set back, or
   its database is put back from an older copy, its leases carry a time or a revocation
   number below what gates hold, and every gate refuses them until the Authority has
   caught up. That is meant: nothing may turn a gate's clock or its revocations back. The
   operator's address shows it under a reason of its own, `authority_behind`, so that it
   is not read as the Authority being down.
3. **Entries left out.** The Authority leaves out an entry once what it names expired more
   than five minutes ago by its own clock. The gate's clock is the Authority's (point 4),
   so the gate already refuses that mandate as expired: nothing is missing. Entries for an
   agent and for a key name things that do not expire, and are never left out. A gate does
   not keep its revoked set across a restart: it has no lease then either, and asks from
   number 0.
4. **The gate's time** (GR-72). "Now" is the held lease's Authority time plus the gate's
   own count of time since that lease's request left (as built in `HeldLease`).
   - The count must go on while the machine is suspended. On Linux that is the
     `CLOCK_BOOTTIME` clock, read through a library with a safe interface; the standard
     library's `Instant` does not count suspension there. On any other system the gate
     program refuses to start with an Authority configured, except with fake data.
   - A receipt stamped this way says so: a new field, `at_from_lease`, true. It is left
     out when false, so every receipt written before G4d reads as what it was: stamped
     from the gate's own clock. A receipt's time still never goes back along the chain
     (`at_clamped`, as built).
   - **Before its first lease**, and with no Authority configured (tests, fake data), a
     gate in observe mode forwards and records as now, stamps from its own clock, and the
     receipt does not carry `at_from_lease`. In enforce mode (G6) it refuses everything.
   - Once a gate has held a lease in this run it keeps stamping from it, however old the
     lease is: an old lease still gives a better clock than the machine's.
5. **Proof needs a complete, fresh lease.** A lease is *complete* when the gate holds the
   bundle it names and has read the feed through its revocation number (`permits`, as
   built), and *fresh* while it is no older than its `valid_for_secs`. With an Authority
   configured:
   - a session is opened only under a complete, fresh lease, and only for a chain that
     holds against the revoked set. A refusal is counted by why, as since GR-155, with two
     more reasons: `no_lease` and `revoked`;
   - a call under an open session, with a proof that holds, is recorded as **proven** only
     under a complete, fresh lease. Otherwise it is forwarded, as everything is in observe
     mode, and recorded as a failed proof with a new reason, `no_lease`: the gate could not
     say whether the agent was revoked, and does not guess;
   - enforce mode (G6) refuses an action whose freshness class the lease's age exceeds
     (§4.6); nothing in G4d blocks.
6. **A revoked agent, mandate or key** (GR-110; §7.6, choice 5). When an accepted feed
   revokes any link of a session's chain, the agent a link names, or the session's key,
   the session is **closed to proof and kept as a marker**. A later call that presents
   it, with a proof that holds, is recorded as that agent's with a failed proof and one of
   three new reasons: `key_revoked`, `agent_revoked`, `mandate_revoked`. The caller did
   hold the key, and the reason says so; the receipt is not `claimed` and not an
   unknown session. A proof that does not hold keeps its own reason.
   - A marker lives as long as the session would have (its mandate's expiry, the idle
     limit), under the same limits for each key, and pushes out nothing proven.
   - No new session is opened for a revoked mandate, agent or key. The shim, refused, goes
     on presenting the session it has, so its calls stay named. Once the marker is gone
     they are recorded as an unknown session, with the agent as claimed: a stated limit.
   - Enforce mode (G6) refuses these calls.
7. **Changing bundle without a restart** (GR-132, GR-133). The connector map, the
   issuers' keys, the templates and the version are swapped together between calls. A
   call whose intent is already written finishes under the bundle its intent names.
   - Each open session's chain is checked again against the new issuers and templates;
     one that no longer holds is dropped, and its next call is an unknown session.
   - **A gate with no saved state** (GR-132) may be started with a bundle file, to have a
     map at once, but its lease is not complete until a lease names that very bundle. If
     the lease names a newer one, the answer carries it. If the lease names an older one
     than the gate holds, the gate keeps its own, has no complete lease, and says so on
     the operator's address: something handed it a newer bundle than its Authority
     publishes, or the Authority was put back.
   - **The shim follows the gate** (GR-133). The gate's answer to opening a session, and
     to every call the shim sends, names the version the gate holds, in a header the shim
     removes before the agent sees it. A shim holding another version asks the gate for
     the bundle, checks it under the root key it pinned itself, by the same rule against
     going back, and uses it from its next call. The call that crossed the change may be
     recorded as a failed proof (`wrong_digest`), once for each shim at each change: a
     stated limit, counted on the operator's address.
8. **Rotating a gate's key** (GR-59). A gate enrols a further key as it enrolled its first:
   a one-time code from an admin, a key id under its own name that was never used, a
   record. Both keys then open its receipts at the Ledger.
   - The chain goes on across the change: receipt *n* is signed with the old key, *n+1*
     with the new, linked as any two are.
   - The gate keeps the public half of every key it has signed with, a file each beside
     the spool, and opens its spool under that set. A receipt in its spool under any
     other key still stops it from starting. The old private half is deleted once the
     Ledger has acknowledged the last receipt signed with it.
   - An admin then revokes the old key (`Revoked`, of a key): the Ledger retires it, and
     the feed carries it, so no gate opens a session for an agent key revoked the same
     way.
9. **Replacing the Authority's admin-record key** (GR-126). A planned change is a record
   in the Authority's own chain, signed with the old key, that names the new one; records
   after it are signed with the new key, and the Ledger and `gov verify` follow the chain
   across it. **From that record on, the old key is refused for every later record**
   (GR-166), by the Ledger and by `gov verify`: otherwise whoever still holds it could
   continue the chain beside the new one. A **taken** admin-record key cannot be repaired this way, since whoever
   holds it can write the same record: that is repaired by the operator at the Ledger and
   by giving auditors the new key out of band, like the first installation, and is stated
   as a limit beside the loss of the root key (owner's decision 9).
   The Authority also checks, when it ships, that a witnessed checkpoint covers its own
   chain's head within a set time, and counts it for its operator when none does.
10. **After a retirement, the witness's order decides** (GR-130). `gov verify` no longer
    compares the gate's date on a receipt with the Authority's date on the record. A
    receipt under a retired key, or of a disabled gate, counts as written before the
    retirement only if a witnessed checkpoint earlier than the first one covering the
    retirement record already covered that receipt. Any other is reported. A receipt
    written honestly before and shipped late is reported too; the report says "not shown
    to be earlier", not "written after".
11. **What is served** (a departure from §7.6's table, for the reviewer to confirm or
    refuse). The table put the `gov-authority` program in G4e. G4d cannot be run as
    programs without one, so its last step adds the program with **one listener, for
    gates only**: the renewal of point 1 and nothing else. Every request on it is a
    document signed with an enrolled gate key; it takes no admin action and no sign-in,
    which stay with G4e (choice 8). It follows the programs' connection rule (HTTPS, or a
    literal loopback address) and their key-file rule (fake data only).
    It is hardened as the Ledger's listener is (GR-169, with GR-77): a sender it does not
    know gets a bare refusal and nothing is written; the request and the answer have a
    size cap; and a limit for each source address applies before any signature is
    checked. **Enrolment, issuing and delegation are not on this listener**: they stay
    library calls on the Authority's machine, so a gate on another machine cannot enrol
    until G4e.

**Steps.** G4d-1: the renewal document, the Authority's answer and the gate's check of it
(points 1 to 3), as libraries. G4d-2: the gate's time, proof under a lease, and revoked
sessions (points 4 to 6). G4d-3: the bundle changed in place and the shim following
(point 7). G4d-4: key rotation and the auditor's order (points 8 to 10). G4d-5: the
program and the gate's renewal loop (point 11), run as four programs with fake data.

**Not in G4d:** people and sign-in (G4e); anything blocked (G6); whether a gate may ask
the Authority at the moment of an outward call (access governance spec §11; the owner's
decision).

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

**No fallback (owner, 2026-11-26; to be built in G7).** Once a system is in enforce mode,
a write to it that Governance cannot decide is refused: no lease or a stale one, no bundle,
a mandate that cannot be checked, a receipt that cannot be written. PM OS's own grant check
still runs first and can refuse; it can never be what allows a write that Governance did
not. The tool server does not notice Governance is unwell and carry on without it. Taking
a system out of enforce mode is an admin action at the Authority, recorded (`ModeSet`), and
never something the running service does for itself.

**What this does not cover** (review finding GR-142). The seam's own switch is PM OS's
setting. With the library seam PM OS still holds the system's credential, so whoever
operates PM OS can unset the switch and write directly, with no record at the Authority:
the cover is **in line, not custody** (§5.2), and receipts say so. "No fallback" is a rule
about what the running service does, not a control on its operator. The stronger claim
holds only where a separate gate holds the only credential. What the Ledger can show
afterwards is the trace of going round: a gate in enforce mode that goes quiet while the
system's own log shows writes, which is reconciliation (§7.5), not built.

 In observe and shadow modes nothing is refused by
Governance, by definition; what cannot be recorded is counted and shown.

### 8.5 Not connected

The model gateway, evals, content, the daily brief, the Slack app's own messages, OpenShell
policy, the prover and the sandbox audit stay outside. Sending sandbox decisions to the Ledger
is later work.

## 9. Code layout and rules

- Crates, all new: `gov-core` (documents, canonical action, scope and policy evaluation,
  verification; no I/O, no clock, no network), `gov-ledger`, `gov-gate` (libraries), `gov-run`
  (the programs `gov-ledger` and `gov-gate`), `gov-authority`, `gov-cli` (the `gov` command: `verify`, `enrol`, `policy check|sign`,
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
| G3 (built as G3a, then G3b) | The MCP client layer moved to a shared crate; `gov-gate` in observe mode against the fake Jira MCP server; the shim | The tool server's tests pass unchanged; a stock client gets `claimed`, the shim gets `proven`; no credential appears in any receipt or log |
| G4 | `gov-authority`: registry, enrolment, Entra sign-in, policy check and signing, mandates, lease and revocation feed, admin interface, `gov` commands | A revoked agent is refused within its class; a gate that misses a revocation treats its lease as invalid; a wrong clock does not extend an expiry |
| G5 | Shadow mode; policy replay; a drafted first policy from observed receipts | Replay reports exactly the decisions a changed rule alters on a fixed history |
| G6 | Enforce: the sealed store, the twelve checks, countersignatures of both classes, the approval screen's interface, budgets, crash recovery | Every row T1 to T7, T11, T15 and T22 has a failing-without, passing-with test; a kill between intent and outcome yields `unknown` and no second write |
| G7 | The PM OS seam, off by default; bulk enrolment; the session mandate; attested approvals; the receipt reference in the audit log; side-by-side comparison; no fallback in enforce mode (§8.4) | With the switch off, the tool server's output is identical; with it on, on the test stack, every write has an intent and an outcome receipt; in enforce mode a write is refused when Governance cannot decide; and the scene of §1.2 runs end to end on fake data: a PM OS bot finishes a task, a planted instruction makes it attempt an operation outside its mandate, the gate refuses it before the fake system is called, and `gov verify` passes on an export holding the refusal |
| G8 | Console screens; Governance's own stack file and the overlay for both; health and metrics (decision time, refusals, lease age, revocation delay) | CI's deploy job starts both stacks on a real Docker host |

| G9 | Undo (§4.7), after G6: before-values sealed at intent; the undo action and its receipts; refusal on a changed target; undo of a whole mandate; the undo cover page | On the fake Jira: forty issues changed by one mandate are put back by one undo with a receipt each; an issue someone edited in between is left alone and reported; a `none` operation cannot be undone and needed a person to run |

**Added by the owner on 2026-11-27** ("Keep the existing feature and build and add the new
things you suggested"). Nothing above is dropped or reordered: G4c to G9 and the access
governance slices stand as they are. Three things are added, from the conductor's answer
on what would make the product hard to replace.

| Slice | Built | Done when |
|---|---|---|
| CM1 | **A library of connector maps** for the three systems to start with: Jira, GitHub and Slack. Each map is a directory in `governance/maps/<system>/` holding the map for one named MCP server at one named version, its sample calls (as `gov policy check` requires), and a note of what it was checked against. A map says, for every tool, the operation, its kind, whether and how it can be put back, whether it reaches outside, and the resource it acts on | `gov policy check` passes on each; every tool of the server is mapped or listed as deliberately unmapped with a reason; the reviewer has read each map (the standing condition of GR-65). A map made from a server's published tool definitions, and not yet run against that server, says so, and is not called verified |
| CM2 | **Drift**: a command, `gov map drift`, that takes a server's current tool listing and a map and reports tools added, removed and changed (by the digest of the definition) since the map was reviewed; and the gate's count of calls to tools the map does not know, by tool digest, on the operator's address | A changed description, a new tool and a removed tool are each reported; a tool whose definition is unchanged is not |
| CP | **Proof about the record**, beyond what `gov verify` shows today. Its design is **not written in this repository**: the owner is deciding whether to file for a patent first, and what is written here is published. It is built after that decision | Not stated here, for the same reason |

Where they sit: CM1 and CM2 need nothing that is not built, so they are built beside G4c
to G4e, one system at a time, Jira first. CP waits for the owner. Custody (G6), undo (G9)
and access reviews (the access governance spec) were in the conductor's answer too and
are already slices.

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
