# User assertion: who a service is acting for

`crates/assertion` (`pmos-assertion`) is a small library. A calling service (the console,
`pmos-web`) uses it to sign a short-lived statement of the person it is acting for. The
receiving service (the bot runtime, the tool server) uses it to check that statement itself,
instead of trusting a plain header. This document covers the library, and how the four
services use it (**In the services**, below). The checklist at the end records what is done.

## The problem

Today a service that acts for a signed-in person calls the bot runtime or the tool server
with a powerful service token, and names the person in a plain field:

- `X-Pmos-User: ana` on bot runtime calls (`docs/bot-runtime-spec.md` §3, Auth).
- `user=` on tool-server routes, or `decided_by` in the body of
  `POST /internal/approvals/{id}/decision` (`docs/toolserver.md`).

The receiving service checks the token and then believes the name. Anyone who holds the token
can claim to be anyone: a bug, an SSRF, or a leaked environment in one caller becomes "act as
any PM, including an admin". This is a confused deputy.

## The fix

The caller adds an `X-Pmos-Assertion` header. The header carries a statement signed with a key
that the caller and the receiver share, and that no other party holds. The statement says:

- I am `web`.
- This is for `bots`.
- I act for `ana`, who has role `pm`.
- It is valid for the next 60 seconds.
- It covers only `POST /v1/tasks` with this body.

The receiver checks the signature, the issuer, the audience, the time, the request and the
nonce. Only then does it believe the name. The service token still says which client is
calling, and the assertion says for whom.

## Wire format (version 1)

The value is one line of printable ASCII, with no spaces and no quotes. It is a list of
`name=value` fields joined by `;`, always in this order (the values below are illustrative):

```text
v=1;kid=k2026a;iss=web;aud=bots;sub=ana;roles=pm,admin;sid=ses_9f2c;iat=1800000000;exp=1800000060;nonce=BwcHBwcHBwcHBwcHBwcHBw;req=6YkoG4-kWITz5ORbi2RvQCWbj7Jym5YmNORsUG4hfIA;sig=yJEk1jGoUQpzZCq5P1wekqL7Witr_iTo7Wa0APTx-7Y
```

| Field | Meaning | Rule |
| --- | --- | --- |
| `v` | Version | Exactly `1`. Any other value is refused as an unknown version. |
| `kid` | Key id | 1 to 32 characters: a lowercase letter or digit, then lowercase letters, digits, `.`, `_` or `-`. |
| `iss` | Issuer: the calling service's client name | Same rule as `kid`. |
| `aud` | Audience: the service that must accept it | Same rule as `kid`. |
| `sub` | Subject: the person's PM OS handle | `pmos_contract::model::valid_handle`, not empty. |
| `roles` | Their roles | Known role names only (`Role::parse`), comma-separated, no duplicates. They appear in `Role::KNOWN` order: `pm`, `skill-author`, `admin`, `reviewer`, `auditor`. The value may be empty (`roles=`). |
| `sid` | The caller's session id | Opaque. 1 to 64 characters of `A-Z a-z 0-9 _ -`. |
| `iat`, `exp` | Issued at and expires at, in unix seconds | Decimal, with no sign and no leading zero. `0 < exp - iat <= 120`. |
| `nonce` | 16 random bytes | Unpadded base64url, 22 characters. |
| `req` | The request binding, or `-` for none | Unpadded base64url of 32 bytes (see below). |
| `sig` | HMAC-SHA256 | Unpadded base64url, 43 characters. |

**Signature.** `sig` is `HMAC-SHA256(key[kid], "pmos-assertion v1\0" || S)`, where `S` is the
exact bytes before `;sig=`. The verifier compares it in constant time.

**Request binding.** `req` is
`SHA-256("pmos-assertion request v1\0" || METHOD || "\0" || TARGET || "\0" || SHA-256(body))`.

- `METHOD` is uppercase, for example `POST`.
- `TARGET` is the path and query exactly as the receiving service sees them, starting with
  `/`, in printable ASCII.
- `body` is the raw request body. Use an empty body when there is none.

**Strict parsing.** Every assertion has exactly one valid encoding, so the parser refuses
everything else:

- input longer than 1024 bytes, or any byte outside printable ASCII;
- an unknown version, judged before the other fields, so a later format reads as "unknown
  version";
- an unknown field, a duplicate field, a field out of order, or a missing field;
- a value that breaks its rule;
- a non-canonical spelling: a number with a leading zero, roles out of order, base64 with
  padding or with stray trailing bits.

As a final check, the parsed fields must re-encode to exactly the bytes that were signed.

**No free text.** Every field has a small alphabet and a length limit. Error messages name at
most a field and never repeat the input.

A typical assertion is about 230 to 300 bytes. The largest possible one is under 600 bytes.

## API

```rust
use pmos_assertion::*;

// Keys: 32 to 64 bytes each. Debug shows the id and "<redacted>", never the key bytes.
let ring = KeyRing::new(Key::from_base64("k2026b", &current_b64)?)
    .with_previous(Key::from_base64("k2026a", &previous_b64)?)?;

// Caller side: sign with the current key.
let claims = Claims::new("web", "bots", "ana", &[Role::Pm], "ses_9f2c")?;
let binding = RequestBinding::new("POST", "/v1/tasks", &body)?;
let header = ring.sign(&claims, unix_now(), 60, Some(&binding))?;

// Receiver side: verify with the current or the previous key.
let policy = VerifyPolicy::new("bots", &["web", "console"])?; // default skew 5 s; binding required
let cache = MemoryReplayCache::default();                    // or your own ReplayGuard
let who: Assertion = ring.verify(&header, &policy, unix_now(), Some(&binding), &cache)?;
```

`verify` runs its checks in this order and stops at the first failure. Each failure has its
own `VerifyError` variant:

1. **Form:** the strict parser above. Fails with `Malformed(_)`.
2. **Key id:** `kid` must name the current or the previous key. Fails with `UnknownKey`.
3. **Signature:** checked in constant time. Fails with `BadSignature`. The signature is checked
   before any claim, so a forged assertion learns nothing about which claim was wrong. The key
   id is already public, and the result never says whether the current or the previous key
   matched.
4. **Issuer:** `iss` must be in the policy's issuers. Fails with `UnknownIssuer`.
5. **Audience:** `aud` must equal the policy's audience. Fails with `WrongAudience`.
6. **Time:**
   - `iat > now + skew` fails with `NotYetValid`.
   - `now >= exp + skew` fails with `Expired`.
7. **Request binding:**
   - If the assertion has a binding, it must equal the request passed in. Fails with
     `RequestMismatch`.
   - If the assertion has no binding, it is refused with `RequestMismatch` unless the policy
     calls `allow_unbound()`.
8. **Replay:** the nonce is recorded until `exp + skew`.
   - A nonce already recorded fails with `Replay`.
   - A full or unreachable store fails with `ReplayStoreUnavailable` (fail closed).
   - Only assertions that pass every other check are recorded.

**Answer every failure with the same `401`, and log the variant.** The difference between the
variants is for operators. It is not for the sender.

`sign` enforces a lifetime of 1 to 120 seconds and draws a fresh nonce from the operating
system for each call (`getrandom`).

## Replay protection and replicas

`MemoryReplayCache` keeps nonces in one process.

- **Its size is bounded.** It holds 100,000 nonces by default. When it is full, it refuses new
  nonces until old ones expire. Only verified assertions are recorded, so only a holder of a
  signing key can fill it.
- **One replica.** A captured assertion cannot be replayed against the replica that already
  accepted it.
- **Several replicas.** Each replica has its own cache, so the same assertion can be accepted
  once by each replica. A restart also empties the cache.
- **What the binding limits.** With a request binding, such a replay can only repeat the
  identical request: the same method, path and body, within at most 120 + skew seconds.
  Without a binding, the replay can be any request.

To close this gap, implement `ReplayGuard` over a store that every replica shares. It needs an
atomic insert-if-absent that expires each entry at `retain_until`. Two examples:

- a Postgres table with a unique `nonce` column and a periodic delete of expired rows;
- Redis `SET nonce 1 NX EXAT retain_until`.

If the store is down, the guard must return `Unavailable` and must not succeed.

## Threats

**Addressed:**

- **Holder of a service token, without the assertion key:** cannot act as anyone. They cannot
  mint an assertion, and the plain name header is no longer believed.
- **Captured assertion** (from a log, a proxy, or a mistaken redirect):
  - it works for at most 120 seconds plus the skew;
  - it works only at its audience, and only for the request it was bound to;
  - it works only once per replay store.
- **Assertion sent to the wrong service:** a `bots` assertion is refused by the tool server
  (`aud`).
- **Caller outside its role:** a client that is not in the receiver's issuer list (for
  example `slack` at a route only `web` may use) is refused (`iss`).
- **Edited fields:** a change to any byte breaks the signature. The test suite tries every
  single-byte change, deletion and insertion.
- **Parser confusion** (duplicate fields, a second spelling of the same values, oversized
  input): refused before any key is used.

**Not addressed:**

- **A compromised calling service can still mint assertions for its own users, within its
  issuer scope.** It holds the key, so it can sign for any handle as its own `iss`. What it
  can do is limited by what the receiver lets that issuer do. Receivers must still apply
  per-issuer rules, for example `web` creates tasks only with `source: "web"`. Receivers must
  also still check the subject and the roles against the user database (the tool server's,
  `docs/user-database-spec.md`), not trust the assertion's roles for anything that matters
  more than display.
- **Shared keys:** with a key shared between issuers, either issuer can sign as the other. Give
  each caller-receiver pair its own key where you can (see rotation).
- **Theft of the key itself.** Keep keys in the same secret store as the service tokens. Never
  put them in config files, code or tests.
- **Replay across replicas** with the in-memory cache. See the section above.
- **Request smuggling between a proxy and the receiver,** if they disagree on the path or the
  body. Bind to what the receiving application actually reads.
- **Confidentiality.** The assertion is signed, not encrypted: the handle and roles are
  readable. Send it only over TLS or a private network, which is already how service tokens
  travel.

## Key rotation

A `KeyRing` holds one current key, which signs and verifies, and at most one previous key,
which only verifies. Key ids must differ. To rotate without downtime:

1. Generate a new key, for example `k2026b`: 32 random bytes, base64 encoded, kept in the
   secret store.
2. Add the new key to every **receiver** as the previous key, next to the current key
   `k2026a`. Receivers now accept both.
3. Make the new key current on every **caller**, with the old key as previous. Callers now sign
   with `k2026b`.
4. Make the new key current on every receiver as well.
5. Wait at least 120 + skew seconds, then remove `k2026a` everywhere.

The ring is symmetric: it does not matter whether a key sits in the current or the previous
slot on the verifying side. Steps 2 and 4 can therefore be the same deploy, as long as every
receiver accepts both keys before any caller signs with the new one. A key that has leaked is
removed at once: assertions signed with it fail with `UnknownKey`.

In the services, `PMOS_ASSERTION_KEYS` names each key's pair, so one variable holds all of a
service's keys (see **Keys** below). A previous key is the optional second half of its pair's
entry.

## Clocks

- **Signing and verifying hosts must agree on the time within the skew.** The default skew is
  5 seconds and the maximum is 30. Run NTP (chrony or systemd-timesyncd) everywhere; on PM OS's
  host, all services share one clock.
- **The clock is injected.** `sign` and `verify` take `now` in unix seconds, and `unix_now()`
  reads the system clock. Tests pass fixed times.
- **Choose a short lifetime.** A caller should sign each request just before sending it with a
  lifetime of 30 to 60 seconds. 120 seconds is the ceiling, not the target. A retry signs a
  fresh assertion, because the replay guard refuses the old one.

## In the services

### Keys

Every service that signs or checks assertions reads `PMOS_ASSERTION_KEYS`: comma-separated
entries, one per caller→receiver pair.

```text
<issuer>:<audience>:<key id>:<base64 key>[:<key id>:<base64 key>]
```

- The first key is current; the optional second is the previous one, accepted while a rotation
  finishes. Keys are 32 to 64 bytes in standard base64 (`openssl rand -base64 32`).
- Each service accepts only its own pairs, and refuses to start on any other, so a key never
  ends up where it has no use:

  | Service | Its pairs |
  | --- | --- |
  | `pmos-web` (signs) | `web:bots`, `web:toolserver` |
  | `pmos-console` (signs) | `console:bots`, `console:toolserver` |
  | `pmos-bots` (checks) | `web:bots`, `console:bots` |
  | `pmos-toolserver` (checks) | `web:toolserver`, `console:toolserver` |

- A malformed entry, a key under 32 bytes, an unknown pair or a pair listed twice refuses to
  start, with the entry's number and pair in the message and never a key. Keys are never
  logged: start-up logs the key ids only. All four services read the list with the same
  parser, `pmos_assertion::parse_key_list`.

### Modes (receivers)

`PMOS_ASSERTION_MODE` on the runtime and on the tool server:

| Mode | Valid assertion | Bad assertion | No assertion |
| --- | --- | --- | --- |
| `off` | ignored | ignored | as before: the named person is believed |
| `verify` | believed | `401` | as before |
| `require` | believed | `401` | `401` if the call names a person, or reads one PM's item (an approval, or a task's detail, events, trace or drafts) |

**The secure state is the default, and the insecure one refuses to start** (W2). A receiver
with a client that acts for a person configured (the tool server: `PMOS_WEB_TOKEN` or
`PMOS_CONSOLE_TS_TOKEN`; the runtime: `web=` or `console=` in `PMOS_BOTS_TOKENS`):

- runs in `require` when `PMOS_ASSERTION_MODE` is unset or blank;
- needs a key for each such client in `PMOS_ASSERTION_KEYS` (`web:<aud>`, `console:<aud>`), or
  refuses to start naming the setting and the missing pair;
- refuses to start in `off` or `verify`, naming the token and the fix, unless
  `PMOS_ASSERTION_INSECURE_OFF=1`. That override is for **fake-data development stacks
  only**: anyone holding the client's token can then act as any PM. Every start-up with it
  logs a line beginning `INSECURE` at error level.
- refuses the override itself, and so refuses to start, on anything that holds real data:
  `PMOS_CONNECTORS=live`, or a database URL (`PMOS_DATABASE_URL`, `PMOS_BOTS_DATABASE_URL`) set
  without `PMOS_DEV=1` (`pmos_assertion::receiver::real_data`). The production
  `deploy/compose.yaml` doesn't pass the override at all, and `pmos-admin` refuses `off` or
  `verify` with a web or console token set whether or not it is in `deploy/.env`. The eval
  stack (`deploy/compose.evals.yaml`: memory stores, fake data, `PMOS_DEV=1`) keeps it.

Without such a client, an unset mode is `off`, as before. `require` is where every install
with the web app or the console runs; there is no override for a production one. `verify` and
`require` refuse to start without at least one key. The Slack client,
workers, evals, the orchestrator's service token and the Slack bot's token are never checked,
in any mode.

The mode, the start-up rule, the refusals and the verifier are shared code
(`pmos_assertion::receiver`); each receiver's `assertion.rs` only names itself.

### What each service does

- **`pmos-web`** signs every runtime call (each names the PM) and every tool-server call that
  names the PM (the inbox, one item, sources, decisions), when it has a key for that upstream: lifetime
  60 seconds, audience `bots` or `toolserver`, bound to the method, path, query and body as
  sent, a fresh assertion per call, next to the existing `X-Pmos-User` and `decided_by`. Roles
  come from the user database (`resolve`, at most 10 seconds old; slice U4c). `sid` is a reference to the PM's session: 16 hex characters of a
  SHA-256 over the session's stored hash under a label of its own (`session::assertion_ref`),
  never the session id or the cookie. Re-reading one item after a conflicting decision carries
  an assertion too. It calls the tool server with its own token (`PMOS_WEB_TOKEN` there), which
  reaches only the approvals routes and the read-only admin routes (`GET /internal/admin/*`:
  the audit log, which accounts members have linked as booleans, connector status, overview
  counts). Each admin call names the viewer (`user=`) and is signed as them.
- **The console** signs every runtime call as its viewer: the bot list, the task and routine
  lists, one task's detail, events and trace, and routine pause, resume and delete (an admin
  acting on someone else's routine signs as the admin). Under `require` the runtime refuses an
  unsigned console call. From the viewer it decides what they see (R1): a non-admin gets only
  their own tasks and routines; an admin (in the user directory, on a snapshot at most 30 seconds old) gets everyone's,
  with the ask, result, error, checklist and prompt of anyone else's left out (`hidden: true`;
  the console shows "text hidden"); one task's page answers its PM or an admin; a cancel is the
  task's PM's or an admin's, recorded as them. To the tool server it signs as the PM whose
  inbox it reads: for an admin's view of every inbox, as each member in turn. Its `sid` is the fixed
  `unknown`: the SSO proxy's session isn't visible to it. It calls the tool server with its own
  token (`PMOS_CONSOLE_TS_TOKEN` there), which may only read approvals.
- **The runtime** checks calls from `web` and `console`: the policy for each allows exactly one
  issuer, the client whose token was presented. The subject must equal every person the request
  names (`X-Pmos-User`, `requested_by=`, `owner=`, `/v1/members/{handle}`), and becomes the PM.
  The `web` client's own rules (`source: "web"` only, its PM's own tasks, routines and settings)
  still apply on top, so a body naming someone else is `403`.
- **The tool server** checks calls with the web and console tokens on the approvals routes. The
  subject must equal `user=` and `decided_by`; one item read with an assertion must be the
  subject's (`404` otherwise), except that the console's admin view reads any item when the
  subject is an active admin in the tool server's own user database. Under `require`, every read of
  one item needs an assertion, from the console too. The console's token never decides,
  whatever it carries (the route and the service both refuse it). Decisions still check the
  item's user and the user database. On the
  admin routes (web token only) the subject must equal `user=`, the viewer, whose role the tool
  server then checks in its own user database (`403 forbidden` without it). The admin proposal
  and event routes (`POST /internal/admin/proposals/open`, `GET /internal/admin/proposals`,
  `GET /internal/admin/protection`, `POST /internal/admin/actions`) need the assertion **in
  every mode, `off` included**, bound to the body (up to 24 MiB for a proposal): opening a pull
  request with an admin's own GitHub token is never done on a token's word alone.
- **Every failure** is the same `401 {"code": "bad_assertion"}`. The log has the client and the
  kind of failure (`missing`, `bad signature`, `expired`, `replayed`, `subject is not the person
  the request names`, …), never the assertion, a key or a handle.
- **Replay:** one `MemoryReplayCache` per receiving process.

### What this does and does not fix

**Fixed, with both receivers in `require`:**

- A stolen or leaked runtime or tool-server token, without the key, acts for no one: every call
  that names a person needs an assertion.
- `pmos-web` and the console no longer hold the tool server's service token. Their own tokens
  reach only the four approvals routes (and, for `pmos-web`, the four read-only admin routes,
  which serve audit metadata and booleans, never a credential), so they cannot start sessions,
  record grants, link or list accounts' credentials, or reindex, whatever happens to them.
  `crates/toolserver/tests/admin_routes.rs` checks that the web token still gets `403` on every
  other route.
- A captured assertion (a log, a proxy) is good for one identical request, once, within about
  65 seconds, at one audience.
- One caller cannot speak for the other: the web key does not verify as `console`, and the
  web token cannot present the console's assertion.

**Not fixed:**

- **A compromised `pmos-web` can still act for any team member, not only those signed in.** It
  holds the `web:bots` and `web:toolserver` keys, and can sign for any handle. The same holds
  for the console and its keys. To limit a compromised `pmos-web` to the people who actually
  signed in, the key must live outside its process, with something that checks the sign-in
  (a separate signer, or identity-provider tokens the receivers verify). That is not built.
- **On the single-box deployment** (PM OS's host, `deploy/compose.yaml`), every key and token is
  in one `deploy/.env` on one host. Each container is given only its own keys, so a compromise
  of one container's process (say, `pmos-web`) does not hand over the console's keys or the
  service token. A compromise of the host, or of anything that can read `deploy/.env` or
  another container's environment, has every key, and the assertion stops nothing.
- **Within its scope, a caller still acts as freely as before:** `web` creates `web` tasks
  and decides approvals for the person it signs for (an edit may change a draft's text, never
  where it goes: `422 target_changed`), and the console reads, pauses and cancels what its
  signed viewer may.
- **One item read by the console:** under `require` it needs an assertion for the item's PM
  or an admin; in `off` and `verify` (only with the development override once the console is
  configured) it still reads any item by id. The same holds for one task's detail, events,
  trace and drafts on the runtime. From the web token a read of one item must name the PM
  (`?user=`, else `404`), so every `pmos-web` read of one item (`ToolServer::approval_for`:
  the first read when a draft is opened and the re-read after a conflicting decision) names
  the PM and is signed for them.
- **What the console sees on its token alone:** under `require`, nothing (every call needs
  the viewer's assertion). In `off` and `verify` (only with the development override once the
  console is configured), an unsigned list is metadata for everyone: who asked, which bot,
  status and times, never an ask, a result or a routine prompt; it can't cancel. It signs as
  each member whose inbox an admin views.
- **Replicas and restarts:** the replay cache is per process (see **Replay protection**). It
  is a `MemoryReplayCache` in each receiver. The `ReplayGuard` trait is synchronous, so a
  Postgres-backed guard would have to block an async runtime thread on every assertion; that
  is not small or safe, so it is not built. On the single-box deployment there is one
  runtime and one tool server, and a restart only re-opens the at most 125-second window of
  assertions issued just before it, for the identical bound request. Before running more
  than one replica of either, give the trait an async form and a shared store.
- **Audit:** the tool server's audit records the client (`web`, `console`), not yet the
  assertion's `iss`, `sub` and `sid`.

### Turning it on (upgrade)

1. Run `deploy/setup.sh` again. It keeps every existing value and adds the web and console
   tool-server tokens (`PMOS_TS_WEB_TOKEN`, `PMOS_CONSOLE_TS_TOKEN`) and one key per pair
   (`PMOS_AK_WEB_BOTS`, `PMOS_AK_WEB_TOOLSERVER`, `PMOS_AK_CONSOLE_BOTS`,
   `PMOS_AK_CONSOLE_TOOLSERVER`, each `<key id>:<base64>`). `compose.yaml` refuses to start
   until they exist. Keep the new `deploy/.env` in the password manager.
2. `deploy/setup.sh` also writes `PMOS_ASSERTION_MODE=require` into `deploy/.env` when it is
   blank. The mode lives there, not in one command's environment, so `pmos-admin up` (with or
   without `--web`), `add-bot`, and the `up` after a `restore` all start the runtime and the
   tool server in `require`. Each of those checks first and refuses (exit 2, before anything
   is stopped, started or written) when the mode is `off` or `verify` while a web or console
   token is set, or when a set token lacks its key.
3. `deploy/pmos-admin up …` as before. The web app and the console use their own tool-server
   tokens and sign with their keys. `up --web` and `up --console` also check that every
   setting their client needs is there. Without the web app and console settings at all, the
   default profile starts with neither client configured and assertions off
   (`deploy/compose.yaml` passes those tokens and keys only when they are set).
4. To rotate a key: give the receivers `<new id>:<new>:<old id>:<old>` for that pair, then the
   caller `<new id>:<new>`, then drop the old half everywhere after two minutes.

`scripts/claude-test-stack.sh` with `PMOS_TEST_WEB=1` does the same for the test stack: fresh
keys for `web:bots` and `web:toolserver`, pmos-web's own tool-server token, and `require` on
both receivers. Without it, neither receiver has a web or console client and assertions are
off. The eval stack (`deploy/compose.evals.yaml`) gives its runtime a `console` token only for
the benchmark runner's trace reads, on fake data in memory with fresh tokens each run, and so
sets `off` with the development override, said so in the file.

## Adoption checklist

Roll out in this order: shared pieces, then callers sending, then receivers enforcing.

**Shared**

- [x] Generate one key per caller-receiver pair: `web→bots`, `web→toolserver`,
      `console→bots` and `console→toolserver`. Store them like the service tokens. A single key
      for all four pairs is acceptable to start with, but it widens what one compromised
      service can sign as.
- [x] Add `pmos-assertion` to each adopting crate. Parse `PMOS_ASSERTION_KEYS` into a
      `KeyRing` at start-up, and refuse to start if a key is under 32 bytes. Never log the
      ring's contents (its `Debug` output is already redacted).

**`pmos-web` (caller)**

- [x] For every runtime or tool-server call made for the signed-in PM, sign `Claims::new("web",
      <aud>, handle, roles from the user database, session id)`. Use a 60-second lifetime and bind
      `RequestBinding::new(method, path_and_query, body_bytes)`. Send the result as
      `X-Pmos-Assertion`, next to the existing `X-Pmos-User` during the transition.
- [x] Sign after the body is serialised. Never reuse an assertion, including on a retry.

**Console (caller)**

- [x] Do the same for calls made for the console viewer, with `iss=console`. This covers
      routine pause, resume and delete, task cancel, and anything that names `decided_by`.
      The console signs every runtime call as its viewer, and every tool-server call that
      names a person as that person; it decides nothing and cancels nothing today. Since
      U4d the roles are the viewer's from the user database's `resolve` answer for the
      request (none for an admin's read of another PM's inbox). There is no team file
      (`team.yaml` was retired in slice U7).
- [x] ~~For reads that are "read everything" (no person), send no assertion.~~ Replaced (R1):
      there are no reads of everything any more. Every runtime call is signed for the viewer,
      and the runtime answers what that viewer may see (others' tasks and routines as
      metadata, for an admin only).

**Bot runtime (receiver, `aud=bots`)**

- [x] Build `VerifyPolicy::new("bots", &["web", "console"])` and keep one replay guard per
      process; replace it with a shared store before running more than one replica.
- [x] In the auth layer, after the client token: for clients `web` and `console`, when the
      route acts for a person, read `X-Pmos-Assertion` and compute the binding from the
      request the handler will see (buffer the body, which is small). Verify it with
      `iss` equal to the client name of the token that was presented, so `web` cannot present
      `console`'s assertion.
- [x] Use `assertion.subject()` as the PM. If `X-Pmos-User` is also present it must equal
      the subject (built as the same `401` as every other assertion failure).
- [x] Still check the subject against the user directory and keep the existing per-client rules
      (`source: "web"` only, own tasks only).
- [x] Answer every `VerifyError` with one `401 {"code":"bad_assertion"}`, and log the variant
      and the issuer.
- [x] Add a setting that moves from "verify if present" to "required for `web` and `console`";
      turn it on once both callers send assertions.
- [x] Leave out of scope for now: client `slack` (it acts on Slack-signed events and is a
      separate question), and the `X-Pmos-User` that the runtime forwards to the gateway.

**Tool server (receiver, `aud=toolserver`)**

- [x] Do the same with `VerifyPolicy::new("toolserver", &[...callers that act for people])`.
- [ ] Routes that name a person: `user=` on `/internal/approvals`, `/internal/credentials`,
      `/internal/memory/repo`, `/internal/onboarding` and `/internal/oauth/*/start`, and
      `decided_by` on the approval and member decisions. The named person must equal
      `assertion.subject()`. *Done for the approvals routes and the read-only admin routes
      (`/internal/admin/*`, web token only),* the only ones `pmos-web` and the console call;
      their tokens reach no other route. The rest stay on the service and bot tokens.
- [x] Admin-only decisions still check `admin` in the user database, not the assertion's roles.
- [x] Service-token calls that act for no person (the orchestrator, the runtime's own session
      creation and approval polling) send no assertion and are unchanged.
- [ ] Record the assertion's `iss`, `sub` and `sid` in the audit next to the token's client
      name. *Not yet:* the audit records the client (`web`, `console`).

**Done when**

- [x] With the setting on, a call from `web` or `console` that names a person without a valid
      assertion is `401`, and contract tests cover a forged one, an expired one, a replayed
      one, the wrong audience and a request mismatch.
- [x] `docs/bot-runtime-spec.md` §3 and `docs/toolserver.md` describe the header, and
      `X-Pmos-User` is documented as advisory for these clients.
