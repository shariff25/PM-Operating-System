# Password sign-in with a required authenticator code

A second way to sign in to the web app, beside Entra ID: a **sign-in name and password, then
always a code from an authenticator app** (or one of ten single-use recovery codes). It is off
unless `PMOS_WEB_PASSWORD_LOGIN=1` is set on `pmos-web`, and it can run alone, or with Entra;
beside the loopback development sign-in only with `PMOS_WEB_DEV_LOGIN_WITH_PASSWORD=1` (a
developer's own machine). There is no way to sign in with a password alone and no setting that
turns the second step off.

The independent review of 2026-11-20 found two High and four Medium issues here, and some Lows;
what changed is in **What changed after review** at the end. Its re-check of `eb24d28`
(2026-11-21) found one High (a setup code could spend the shared hashing allowance), four
Mediums and nine Lows here and in the Entra first link; what changed is in **Re-check of
eb24d28**, at the end. The security gate at `04c175f` (2026-11-21) passed with no High or
Critical open and five Mediums (G1 to G5) and five Lows (GL1 to GL5) here; what changed is in
**Gate review of 04c175f**, at the end. The review of `8e8f8fe` (2026-11-21) found two Mediums
(M3, M4) and six Lows here; what changed is in **Review of 8e8f8fe**, at the end. Its re-check
of those fixes found one Medium (N2: `pmos-web` still locked a shared office address out) and
four Lows; what changed is in **Re-check of the 8e8f8fe fixes**, at the end.

There is no email. A person gets a password by a **one-time setup code** an Admin makes on the
Team page (or, for an Admin or Auditor, someone with host access makes with
`pmos-admin users password-setup <handle>`), passed on by hand. A forgotten password, a lost
phone or a suspected compromise is the same: a new setup code, which replaces everything.

The contract the front end builds from is `docs/web-api.md` ("Password sign-in"); the tool
server's routes are in `docs/toolserver.md` ("Password sign-in"); the code is
`crates/toolserver/src/password/`, `crates/toolserver/src/app/password.rs`,
`crates/toolserver/src/users/password_pg.rs` (migrations `0020_password_sign_in.sql`,
`0022_password_review_fixes.sql` and `0023_gate_fixes_2.sql`) and
`crates/web/src/app/password.rs`.

## Where the secrets live

**The tool server owns users, so it owns their password hash, their authenticator secret and
their recovery codes**, in its own tables (migrations 0020, 0022, 0023):

| Table | Holds | Never holds |
| --- | --- | --- |
| `password_credentials` | the Argon2id PHC string, the authenticator secret sealed by the vault, the last accepted authenticator step, the credential's **epoch**, the two account-wide failure counts, **who issued** the code it was set with (`issued_via`: `host` or `web`), and a **reset notice** (who, when) until the person dismisses it | a password, a secret in the clear |
| `password_source_counters` | per person **and per address**: the failure counters behind the waits, and when the person last fully signed in from there; the address only as a SHA-256 of `pmos-web`'s normalized text | an address |
| `password_recovery_codes` | SHA-256 of each code (bound to the handle), used or not | a code |
| `password_setup_codes` | SHA-256 of each setup or reset code, who issued it (web or host), its expiry, use and replacement; how many setups it started and passwords it set, and a plain count of wrong guesses tried at its person while it was live (it decides nothing); `voided_at`, set before the gate review of `04c175f` only | a code |
| `password_pending` | SHA-256 of each token between steps, its owner, purpose, expiry and tries; during a setup the chosen password's hash, the sealed secret being enrolled, and how many passwords and secrets this token made | a token |

- The **authenticator secret** is sealed with the vault's AES-256-GCM keys
  (`crate::vault::KeyRing`, `PMOS_VAULT_KEY`), bound by its additional data to the person and
  this use (`pmos-vault|<handle>|password|totp`): copied to another person's row it does not
  unseal. A secret sealed with an older key is sealed again at its next use.
- **`pmos-web` never stores or sees a hash or a secret.** It forwards what the person typed to
  the tool server with its own token (`PMOS_WEB_TOKEN`), and for a signed-in person their user
  assertion, and acts on the answer. The tokens between steps travel to the browser in
  `HttpOnly` cookies.
- **Only `pmos-web`'s token reaches the password routes**: the orchestrator's, the console's and
  the Slack bot's get `403`, no token `401` (tested).
- **The routes run only in assertion `require`** with no development override (`503
  assertion_required`), and `pmos-web` refuses to start with `PMOS_WEB_PASSWORD_LOGIN=1` unless
  it signs for the tool server (a `web:toolserver` key) and `PMOS_ASSERTION_MODE` is unset or
  `require`.
- Without `PMOS_VAULT_KEY` the tool server has no way to seal a secret, so password sign-in is
  off there (`501 not_configured`). The key is required with live connectors already.

## The flows

### Getting an account (setup), and a reset

1. **An Admin makes a setup code** for a person on the Team page
   (`POST /api/admin/users/{handle}/password-setup-code`). It is shown to the admin once. Rules,
   checked by the tool server in the person's transaction:
   - the actor is an active Admin (`403 forbidden`), and not the person (`403 self_change`:
     nobody issues a code for themselves);
   - the person exists (`404 not_found`), isn't the `evals` account (`409 evals_account`), is
     active (`409 not_active`) and has a **confirmed** sign-in name (`409 sign_in_unconfirmed`);
   - **for a person holding Admin or Auditor, only the host** may issue one (`403 host_only`):
     `pmos-admin users password-setup <handle>`, which records `PMOS_HOST_USER`, mirroring the
     Entra host-link rule.
   A new code replaces the person's live one (and ends any setup in progress with it). The code
   is a **setup** code for someone with no password yet and a **reset** code for someone with
   one. Recorded: `role_changes` `issue_password_code` (detail `setup` or `reset`) and the audit
   event `password.setup_code`.
   **A reset is never silent.** A reset code also puts a notice on the person's credential (who
   issued it, an admin's handle or `host`, and when). From their next sign-in it is shown at the
   top of every page and on "Sign-in and security": "Your sign-in was reset by <name> on
   <date>. If this was not you, tell an admin." It stays, through the reset itself, until they
   dismiss it (`password.reset_notice`, `dismissed`). The dismissal is also a `role_changes` row
   (`dismiss_reset_notice`: who, when), so it can't go unrecorded even if whoever holds the
   account dismisses it. The admin overview lists the week's resets, read from `role_changes`
   by time (`password_resets`: whose, by whom, when, and when the notice was dismissed).
2. **The person opens the setup page** and enters their sign-in name and the code
   (`…/setup/begin`). Every refusal before the code matched is one answer, `401 setup_failed`:
   no such name, no code, a wrong, expired, used or replaced code, or an account waiting out
   its delay. Once the code matched, a refusal can say why: deactivated (`403 not_a_member`),
   an unconfirmed name (`409 sign_in_unconfirmed`), an Admin's or Auditor's code that the host
   didn't issue (`403 host_only`; the person's roles are checked again at every step), or a code
   that gave all the work it allows (`409 setup_code_spent`, below). A setup token (15 minutes,
   never past the code's expiry) goes into the `__Host-pmos-setup` cookie. **A wrong guess never
   touches anyone's code or a setup in progress** (the gate review of `04c175f`, G2): it counts
   against the guesser's address, for that person (5, then waits, as below), and against the
   address's network block (100 in 15 minutes per IPv4 /24 or IPv6 /48). Past that, every
   refusal before the code matched is `429` for that block, whatever the name, and writes
   nothing; **a right code from that block still begins** (the review of `8e8f8fe`), so a flood
   of wrong guesses from an invitee's network can't shut the invitee out. What an over-limit
   attempt can tell is only whether its name and code are a right pair (a 100-bit code), and,
   by timing, whether the name exists, as below the limit (an unknown name costs one lookup, a
   real one a lookup and a short read); `pmos-web` counts each such `429` against the address
   and the name (10 per name and address, 100 per address, per 15 minutes), the tool server
   counts every one on `/readyz` (`password.setup_block_refusals`) and records them thinned:
   at most one audit row a minute per block, with no target, standing for the refusals since
   the block's last row (`block_limit; refused <n>`). A setup code is 100 bits, so it needs no guess
   cap of its own: voiding it protected nothing and let anyone who knew a sign-in name end the
   person's setup. An operator sees a plain count of wrong guesses on the tool server's
   `/readyz` (`password.setup_code_wrong_guesses`, since start-up), and each one in the audit
   log.
3. **A password** (`…/setup/password`): checked against the rules below, hashed, kept with the
   setup (not yet in force). **Bounded** (the re-check of eb24d28, H1): at most 3 per setup
   token (then `401 setup_expired`: start again with the code) and 6 per code across all its
   tokens; a code starts at most 5 setups. Past either, the code is spent (`409
   setup_code_spent`) and an admin issues a new one. Each call is counted before its hash,
   whatever comes of it, against the token, the code, the account's work (below) and, at
   `pmos-web`, the browser's address, **successes included**; its hash comes from the global
   limit's open allowance only, never the reserve. **A refusal by the shared limits costs the
   invitee nothing** (G3): the account's work and the open allowance are taken first, in the
   person's transaction, so a `429` from them writes nothing, and `pmos-web` gives the
   address's slot back for it (the review of `8e8f8fe`); and when every hashing slot is
   busy (`503 sign_in_busy`) no hash ran and the set is given back to the token, the code and
   the account. Only a set whose hash can run is counted, so one code still costs at most 6
   hashes (tested in parallel too).
4. **The authenticator** (`…/setup/authenticator`): the tool server makes a 160-bit secret and
   answers it once as base32 text and an `otpauth://` URI; the front end draws the QR code (the
   server has no image library). Calling it again makes a new secret, at most 5 times per setup
   token; each call counts against the address at `pmos-web`, successes included. The secret is
   shown only: never put in web storage, a readable cookie, the address or history (tested).
5. **A code from the app** (`…/setup/confirm`). Then, in one transaction: the password, the
   secret and ten new recovery codes **replace** whatever the person had; the setup code is
   spent; every pending token of theirs goes; the credential's **epoch** moves, so every
   password session of theirs ends; recorded `role_changes` `set_password` (`setup` or `reset`)
   and `password.setup_complete`. `pmos-web` also ends every session the person has (Entra and
   development ones included) and starts a new one in this browser. The recovery codes are
   shown once.

**Until a reset code is used, the old password keeps working**, by design: a reset is often for
someone who still signs in fine and only lost their recovery codes, and a code that is never
used changes nothing. To shut a person out at once, deactivate them (their sessions end within
10 seconds). An "ends at once" option on the reset was considered and not built: it isn't a
small change (it would end the credential before the person has a new one, which is what
deactivation is for).

**Considered and not built: requiring the person's second factor, or a second admin, for a
reset.** The reviewer suggested it so that a compromised admin (or `pmos-web`) can't take over
someone silently. It isn't built because a reset is exactly what a person who lost their phone
needs: if it took their authenticator, they could never be reset; and a second admin isn't
always there in a pilot. Instead a reset is never silent (above), every reset is recorded in
`role_changes` and the audit log with the actor and client, and **a reset for an Admin or
Auditor is host-only** (`403 host_only` from the web, checked at issue and again at every setup
step), so a compromised admin session or `pmos-web` can reset only people who hold neither.

### Signing in

1. **The password step** (`…/sign-in`): sign-in name and password. Every refusal is the same
   answer, `401 sign_in_failed`, with the same status, body and work: no such user, no password
   set, a wrong password, a deactivated user, an address waiting out its delay for this account,
   an Admin's or Auditor's web-issued credential (below). When there is no real hash to check, a
   hash of a random password is checked instead, so the time spent is the same class. What
   happened is in the audit log, by reason (`unknown_name`, `no_password`, `wrong_password`,
   `deactivated`, `locked`, `host_only`), never by value.
   **A web-issued credential doesn't survive a promotion.** The credential records who issued
   the code it was set with. When its person holds Admin or Auditor and an admin issued that code
   in the web app, the password step and the second step refuse it, uniformly, until someone with
   host access issues a setup code and the person sets up again; their status says
   `needs_host_setup`, `resolve` gives `pmos-web` no epoch for them, so their password sessions
   end at their next request (at once in the `pmos-web` that made the role change), and the Team
   page shows "Password needs host" (`password_needs_host_setup`, also in the role change's
   answer). A credential set before migration 0022 recorded nothing counts as web-issued unless
   the code it was set with is still on record as host-issued.
2. A correct password gives **only a pending token** (`__Host-pmos-pending` cookie): single
   purpose (the second step of this one person), five minutes, three tries, stored hashed,
   worthless after a reset or a password change (it carries the epoch). **It grants nothing
   else.** Correct password steps count against the account's work (10 in 15 minutes, then
   `429` for that account until the window moves), so whoever holds a password can't loop it
   to spend the shared allowance; a wrong guess never counts there. The room is checked before
   the hash (a full one costs no hash) and taken in one step with the pending token once the
   password proved right (GL1), so correct steps sent in parallel can't pass it together.
  **A full room answers `429` to every attempt on that account**, a right password or a wrong
  one, from any address. The review of `8e8f8fe` noted that this is not the uniform `401`; it
  is kept, as this document chose it: only the password's holder can fill the room (a wrong
  guess never counts, and a check that never ran because every hashing slot was busy, here or
  in a change, gives its piece back), so an outsider sees it only after the holder made 10
  correct password steps in 15 minutes. A `401` would mean either hashing every attempt on a
  full room, which lets the holder spend the shared allowance past `pmos-web`'s count (H1,
  since `pmos-web` doesn't count the tool server's `429`s), or telling the holder their right
  password is wrong.
3. **The second step** (`…/second-step`): an authenticator code, accepted for the step before,
   the current step or the step after, and only for a step later than the last one accepted (a
   code works once), or one of the recovery codes (each works once). A wrong code is
   `401 code_failed` with `attempts_left`. Then the session, exactly as the other sign-ins make
   it (`__Host-pmos`, rotated, with its CSRF token), holding the credential's epoch; the sign-in
   event `web.sign_in` with `provider: password`.

### Signed in

- **Change the password**: the current password and a fresh authenticator code. The epoch
  moves: every other session of the person ends; this browser gets a new session. Its two
  hashes come from the open allowance only, and each call counts against the account's work
  and the browser's address, whatever its outcome (LA), except a call whose check never ran:
  with every hashing slot busy (`503 sign_in_busy`) the piece of work and the
  re-authentication failure counted ahead go back, and with no room for the second hash the
  piece goes back too (the review of `8e8f8fe`).
- **New recovery codes**: an authenticator code; the ten old ones stop working. No Argon2 hash;
  each call counts against the account's work and the address all the same.
- **Every request** of a password session resolves the person in the user database (at most 10
  seconds old) like an Entra session: not active, every session of theirs ends
  (`403 not_a_member`); the epoch moved (a reset, a change) or is missing (a held credential),
  the session ends (`401 signed_out`). The epoch is in `resolve`'s answer to `pmos-web` only.
- **When the tool server can't be reached, a password session fails closed** (`503
  users_unavailable`), even for the two reads other sessions may serve for up to 5 minutes on
  their last-known-good answer: a session's epoch can't be checked then, and a reset or a
  password change must end it within the 10 seconds above, not 5 minutes. So the bound is
  10 seconds whether the tool server answers or not. (Chosen over stating a 5-minute bound: it is
  one condition in `pmos-web`'s session check, and the cost, a password user seeing `503` on
  their home page during an outage, is what every write already does.)
- **A password session always carries an epoch**: a sign-in, setup or change whose answer has
  none starts no session (`502`), so no password session can go unchecked.

## The rules for a password

- **At least 12 characters, not counting whitespace at either end, counting each run of
  whitespace inside once and not counting characters that show nothing; at most 128 as typed**
  (Unicode scalar values), any characters, no composition rules. Twelve spaces, `password` and
  four spaces, or twelve zero-width spaces, is too short (the review of `8e8f8fe`, M3, and its
  re-check).
- Not the handle, not the sign-in name, not the part of the sign-in name before `@`, and not a
  common password, each compared case-insensitively with the password **as the rules read it**
  (characters that show nothing dropped, whitespace at either end dropped, each inner run one
  space, then in Unicode's compatibility composition, NFKC) **and with every space taken
  out**, so `password1234` padded, spelled `pass word 1234`, with a zero-width space inside, or
  in full-width letters (`ｐａｓｓｗｏｒｄ１２３４`), is refused: **46,146 entries**
  (`crates/toolserver/data/common-passwords.txt`, since the gate review of `04c175f`, G5),
  every entry of SecLists' list of the **1,000,000** most frequent passwords
  (`xato-net-10-million-passwords-1000000.txt`, MIT licence) that is 12 to 128 characters,
  trimmed and lower-cased, plus the 60 of the old built-in list (`hash.rs`, `COMMON`). Of the
  10,000 most common passwords only 10 are 12 characters or more, so the list goes a hundred
  times deeper. Source, derivation, both files' SHA-256 and the licence are in
  `crates/toolserver/data/common-passwords.NOTICE`. The check is one lookup in a hash set built
  once (SipHash with a random key), so its time depends on the candidate's length, not on
  whether or where it is in the list; the candidate is never logged.
- **What is normalised, and what isn't.** Whitespace is Unicode's `White_Space` (the space,
  tabs, line breaks, the no-break space, the ideographic space and the other Unicode spaces).
  **Characters that show nothing** are Unicode's `Default_Ignorable_Code_Point` (15.1),
  exactly: U+00AD (soft hyphen), U+034F (combining grapheme joiner), U+061C (Arabic letter
  mark), U+115F–U+1160 and U+3164, U+FFA0 (Hangul fillers), U+17B4–U+17B5, U+180B–U+180F
  (Mongolian free variation selectors and vowel separator), U+200B–U+200F (zero-width space,
  non-joiner, joiner, left-to-right and right-to-left marks), U+202A–U+202E (bidirectional
  embeddings and overrides), U+2060–U+206F (word joiner, invisible operators, bidirectional
  isolates, deprecated format characters), U+FE00–U+FE0F (variation selectors), U+FEFF (byte
  order mark), U+FFF0–U+FFF8, U+1BCA0–U+1BCA3 (shorthand format controls), U+1D173–U+1D17A
  (musical format controls) and U+E0000–U+E0FFF (tags, variation selectors supplement), unassigned code points in those ranges included
  (`hash::is_ignorable`). Format characters outside that property show a glyph and count (the
  Arabic and Kaithi number signs, U+0600–U+0605, U+06DD, U+070F, U+0890–U+0891, U+08E2,
  U+110BD, U+110CD). **NFKC** (from `unicode-normalization`, already in the build through
  sqlx) is used for the name and common-list comparisons only, not for the length. Only the
  rules read the password these ways. **What is hashed, and checked at sign-in, is
  exactly what was typed**, byte for byte, so every password set before this change signs in
  as it always did, inner spaces and all (tested, with one the rules now refuse), and a
  password with inner runs of spaces must be typed with them. Not normalised before the hash:
  canonical forms (NFC and NFD: a browser that sends an accented letter decomposed won't match
  a hash of it composed), and nothing else either; normalising before the hash would change
  what existing hashes were computed over, so it is left as it is. Not caught by the rules:
  letters that only look alike (a Cyrillic `а` for a Latin `a`; NFKC doesn't fold scripts).
- A refused one is `422 weak_password` with `reason`: `too_short`, `too_long`,
  `matches_name` or `common`.
- **Never logged, never in an audit detail, never in an error.** Inputs are held in zeroizing
  buffers in the tool server, and Argon2's own buffers are zeroized by the crate.

## Hashing

**Argon2id**, version 0x13, **m = 19,456 KiB (19 MiB), t = 2, p = 1**, a 16-byte random salt per
hash and a 32-byte output: OWASP's Password Storage Cheat Sheet's first-listed minimum ("Use
Argon2id with a minimum configuration of 19 MiB of memory, an iteration count of 2, and 1 degree
of parallelism"), checked on 2026-11-20 against the cheat sheet's source in the OWASP
CheatSheetSeries repository (the cheat-sheet site itself couldn't be reached from the build
session). The parameters are stored in the PHC string with the hash
(`$argon2id$v=19$m=19456,t=2,p=1$…`): raise them in `hash::CURRENT` and each person's hash is
recomputed at their next correct sign-in (`password; rehashed` in the audit log). The rehash is
a hash like any other (GL2): it draws from the global limit (the open allowance, or the reserve
for a known address, as its sign-in did). With no room, or no free hashing slot, it is put off,
never skipped silently: the sign-in goes on, the event says `password; rehash_deferred`, the
tool server's `/readyz` counts it (`password.rehashes_deferred`) and logs a warning, and the
next correct sign-in with room does it.

The work runs on the blocking pool, never on the async executor, **two hashes at a time**; a
request waits at most 10 seconds for a slot, then `503 sign_in_busy`.

## Limits

| Limit | Value | Where |
| --- | --- | --- |
| Password length | 12 to 128 characters: at least 12 without whitespace at either end, with each inner run counted once and without characters that show nothing, at most 128 as typed (M3 and its re-check) | tool server |
| Request body on the password routes | 4 KiB (`413` at `pmos-web`) | tool server and `pmos-web` |
| Failures per account **per address** before that address waits (passwords and second-step codes **counted together**) | 5 | tool server, per person and address (an IPv4 address, an IPv6 /64) |
| The wait | 30 s after the 5th failure, doubling with each further one (1, 2, 4, 8 min), **at most 15 min** from the 10th; **for that address only** | tool server |
| An attempt from that address during its wait | refused with the same answer, **not checked and not counted** | tool server |
| Failures on one account from **every** address together | **50**; past it (until 24 hours with no failure) an address the person hasn't fully signed in from in the last 30 days is **rationed**: after each failure it waits 15 minutes at once (one attempt per 15 minutes per address). It is never refused for the ceiling alone, so the right password and code from a new address always get in. Reaching it is audited (`password.lockout`, `ceiling`) | tool server, per person |
| Failures forgotten | 24 hours after the last one | tool server |
| Back to zero | a full sign-in (that address's count), a finished setup or reset (every address's), a password change (the person's re-authentication count) | tool server |
| Setup-code guesses | the same schedule per address (5, then 30 s doubling to 15 min), a counter of their own; **no** account-wide ceiling; **never** counted against the code, which no guess can void (G2); a new code clears every address's count | tool server, per person and address |
| Setup-code guesses per network block | **100** wrong codes (or unknown names) at the setup's first step from one IPv4 /24 or IPv6 /48 in any rolling 15 minutes; past it, until the window moves, every refusal before the code matched is `429` for that block whatever the name, changes nothing and counts at `pmos-web`, and **a right code still begins** (the review of `8e8f8fe`); those refusals are counted on `/readyz` and recorded, at most one audit row a minute per block (its re-check); a right code gives its slot back; idle blocks are forgotten every 5 minutes | tool server, per process |
| Re-authentication while signed in (change the password, new recovery codes) | the same schedule, in one bucket per person apart from every address | tool server, per person |
| Hashing work after an authentication (a correct password step, a setup's password, a password change, new recovery codes) | **10** per account in any rolling 15 minutes, then `429` for that account, to every attempt on it; a correct password step is checked before its hash and taken, in one step, once the password proved right (GL1: parallel ones can't pass together; a wrong guess by anyone never uses the room); the others are counted before their work, whatever its outcome, and given back when the shared allowance refuses it or no hashing slot was free (G3; for a change since the review of `8e8f8fe`, with its re-authentication failure); idle accounts are forgotten every 5 minutes | tool server, per person, per process |
| A setup token | **3** password sets and **5** new authenticator secrets; past either, the token is spent (`401 setup_expired`) | tool server, per token |
| A setup code, across its tokens | **5** setups started and **6** password sets (two tokens' worth: room for a typo or a setup abandoned and started again); past either, the code is spent (`409 setup_code_spent`) until an admin issues a new one; a set refused `429` by the shared limits, or `503` with no free hashing slot, isn't counted (G3) | tool server, per code |
| Password hashes, all callers together | **300** in any rolling minute, plus a **reserve of 120** that only a **password step from an address someone fully signed in from in the last 30 days** may use, decided from the address before the name is looked up (`429 too_many_attempts`); setup and the signed-in routes take from the 300 only; routes that compute no hash take none, and an invalid token is checked before anything; a rehash draws like any hash (GL2) | tool server, whole process |
| One network block's share of the 300 | **60** (a fifth) in any rolling minute per IPv4 /24 or IPv6 /48, at any load (the re-check of the `8e8f8fe` fixes: before, a block took freely up to 240); once 240 (80%) were taken in the minute, also at most **6** of the last 60 per block (G1, M4); so filling the 300 takes at least fourteen blocks (four of 60, then ten of 6); setup and signed-in work count under the account's own key instead (bounded per account above) | tool server, per process |
| One address's share of the reserve | **2** in any rolling minute (an IPv4 address or IPv6 /64), and **24** per block (G1) | tool server, per process |
| Hashing | 2 at a time; a 10-second wait at most (`503 sign_in_busy`) | tool server |
| Refused attempts per source address, and per subject from it | **10** about one subject (a sign-in name, trimmed and lower-cased, shared by the password step and a setup's first step; one pending or setup token; one signed-in person; or nothing readable) and **100** about anything, in any rolling 15 minutes (N2 of the re-check of the `8e8f8fe` fixes: before, 20 per address); a refusal for a subject past its 10 is `429` and **not counted against the address**, so a shared address waits only after refusals about at least ten subjects at their limit, or a hundred mistakes; **every** refusal counted (a wrong password or code, an unknown or expired token, a step out of order, a malformed body; not PM OS's own `5xx`, and not a `429` from a limit the tool server shares on `sign-in` and `setup/password`: M4 and a Low of the review of `8e8f8fe`), and **every** `setup/password`, `setup/authenticator`, `change` and `recovery-codes` call, successes included (except, for `setup/password`, a shared limit's `429`); a tool-server `429` about this address's own guessing (the second step's and a setup confirmation's wait, a setup's first step over its block's guesses) counts; then that subject (or, past 100, every password route) answers `429 too_many_attempts` with `Retry-After`; the check and both counts are one step | `pmos-web`, per process, per IPv4 address or IPv6 /64; at most 10,000 addresses and 50,000 (address, subject) pairs tracked, subjects held only as SHA-256; when full, the idle ones go, then the one seen longest ago, found from an index kept in time order rather than by scanning every entry under the lock (the review of `2efb73f`; which ones go is unchanged) |
| Pending token (after the password) | 5 minutes, 3 tries, single use | tool server |
| Setup token | 15 minutes (never past the code), 5 tries at the confirmation | tool server |
| Setup or reset code | 100 bits, 24 hours, single use; a new one replaces the old | tool server |
| Recovery codes | 10, 80 bits each, single use | tool server |
| Authenticator | TOTP (RFC 6238): HMAC-SHA-1, 6 digits, 30-second steps, the step before and after accepted, each step once; a 160-bit secret | tool server |
| Pending sign-ins per person | 5 at once (the oldest go) | tool server |

**No lockout an attacker can hold on someone.** The wait is per person **and per address**: ten
wrong guesses from one address make that address wait for that account, and the owner, from
their own address, signs in at once with the right password and a right code (tested). Nobody
else's guessing can make the owner wait from an address they have signed in from, and since the
re-check of eb24d28 (M2) not from a **new** address either: past the account's ceiling a new
address is rationed, never refused, so the owner's first attempt from it is checked and the
right password and code get in (tested, past the ceiling, from an address never used). A setup
code has no account-wide ceiling at all, and since the gate review of `04c175f` (G2) no guess
counts against the code either: nobody else's guessing can void a person's code or end their
setup (tested: 40 wrong guesses from eight networks while a setup is in progress, and it
finishes).

**Why guessing stays bounded** (stated again for the gate review of `04c175f`, G5). Per
account, from addresses the person has never fully signed in from: **50** wrong guesses from
anywhere until the count is forgotten (24 hours with no failure at all); past that, **one guess
per 15 minutes per address** (96 a day). Each guess is a hash. Since the re-check of the
`8e8f8fe` fixes a network block (an IPv4 /24 or IPv6 /48) takes at most **60** of the open 300
hashes a minute (**86,400 a day**; it was 246 a minute, 354,240 a day, after M4); the open
allowance is **432,000** a day for everyone together. So with A addresses (IPv4 addresses or
IPv6 /64s) in B blocks, an attacker checks at most **50 + min(96 × A, 86,400 × B, 432,000)**
passwords a day against one account. For example: one IPv4 /24 (256 addresses), 24,626 a day
(unchanged: the address count binds); one IPv6 /48 (65,536 /64s), **86,450** a day (354,290
before); two /48s, 172,850; the whole open allowance takes at least five blocks for one
account and fourteen to fill it. From the person's own recent addresses, each has its own schedule (about **104**
a day: 5, then waits of 30 s, 1, 2, 4 and 8 minutes, then one every 15 minutes), usable only from
that network. **Against the whole service**, at most **300** hashes a minute from the open
allowance (432,000 a day) and **120** from the reserve (172,800 a day), so at most **604,800**
password guesses a day across every account together; the reserve only for addresses someone
fully signed in from, at most 2 a minute each (2,880 a day) and 24 a minute per block. `pmos-web`
adds, for browsers, **10** refused attempts per address per account (sign-in name) and **100**
per address in all, per 15 minutes, on top (N2): from one address at most 960 refused attempts
a day reach the tool server for one account (its own schedule, about 104 a day, binds first)
and **9,600** a day across every account (spraying; it was 1,920 with the old 20, which locked
out an office behind one address). See "What holds against a compromised `pmos-web`". Every guess is behind Argon2id, against a 12-character
minimum with the 46,146 common passwords above refused; and a right password still needs the
authenticator code (3 tries per pending token, counted the same way), so a guessed password
alone signs no one in.

**What an attacker with a large address range can still do.** With addresses in fourteen or
more blocks (four to take the first 240, ten for the last 60) they can fill the open allowance
for as long as they keep sending: then a sign-in from
an address nobody has signed in from in 30 days (a new person on a new network) gets `429`
until the flood stops; a known address still has the reserve. To fill the reserve too they
need addresses someone really signed in from in at least five blocks and 60 addresses, which
takes an insider who has signed in from all of them. Against one account they get the bound
above, and still need the second factor.

**The global limit can't shut everyone out.** It bounds hashing only (the per-account and
per-address limits bound guessing, and two hashes at a time bound the CPU). The reserve of 120 is
for password steps **from an address someone fully signed in from** in the last 30 days, decided
from the address alone before the sign-in name is looked up (M3), so an unknown name and a real
account get the same answer under load (tested with the open allowance full: both `429` from a
new address, both the uniform `401` from a known one). A flood of junk can't stop a sign-in from
a known address (tested), and setup and the signed-in routes never use the reserve (H1, LA).
Junk sent from a known address may use the reserve too, but each address takes at most 2 of it
a minute and each block 24 (G1), so filling 120 a minute takes known addresses in five blocks.
**Shares** (G1, as changed by M4 of the review of `8e8f8fe` and by its re-check): each IPv4 /24
or IPv6 /48 takes at most 60 of the open 300 a minute, and once 240 were taken in the minute at
most 6 of the last 60, so a flood from a few blocks takes at most 60 a minute each and never
the last fifth, which is left for everyone else (tested: 600 unknown-name guesses from three
/24s take 180; a sign-in from a fourth block, from an address nobody has used, goes through). Before M4 the share was 30 from the first hash: a 30-person office behind
one address, signing in during a morning rush with a few typos, was refused `429`, and
`pmos-web` counted each `429` against the office's address until it waited 15 minutes. Now the
office takes up to 60 hashes a minute at any load (tested: 30 people, each mistyping once, 60
hashes from one address nobody had used, all sign in), `pmos-web` doesn't count the tool
server's shared-limit `429`s, and it counts wrong passwords per sign-in name as well as per
address (N2, tested at `pmos-web`: 30 people mistyping twice and three people stuck on a wrong
password, all from one address, and everyone but the three signs in). **The trade-off of 60**
(the re-check of the `8e8f8fe` fixes, which asked for a block's share well below 240): an
office whose sign-ins in one minute need more than 60 hashes (more than 30 people with one typo
each, or 60 without) gets `429 too_many_attempts` for the rest of that minute, with a
`Retry-After` of at most 60 seconds, not counted against the office's address at `pmos-web`;
its known addresses also have the reserve (2 a minute each, 24 per block). That is a wait of
under a minute, never the 15-minute lockout M4 fixed; in exchange, one /48 guesses against one
account at a quarter of the rate it could (86,450 a day, not 354,290). The choice of /24 and /48: the smallest networks commonly handed to one
customer or one site, so one subscriber's whole range is one block; the cost is that, while
the allowance is nearly full, a large office or a carrier-grade NAT behind one /24 shares 6
hashes a minute for sign-ins from addresses nobody signed in from (its known addresses also
have the reserve).
What it costs: under a flood from fourteen or more blocks, someone signing in from an address nobody
has signed in from (a new person on a new network) waits until the flood ends; a new person's
first sign-in is usually from the address they finished setup from, which is known. Routes that compute no hash
(the second step, a setup's first step and confirmation) don't touch it, and a made-up or
expired token is checked, with one lookup, before any work.

**No credential holder can spend what everyone shares** (H1). Each kind of work that follows an
authentication is bounded per token, per account and per address:

| Route | Per token | Per account | Per address (`pmos-web`) | Global |
| --- | --- | --- | --- | --- |
| `sign-in` (a correct password) | (n/a) | 10 correct steps / 15 min | refusals only, 10 per name and 100 in all (a NAT'd office's sign-ins don't share a cap, and its typos share only the 100) | open (60 a minute per block), or the reserve from a known address |
| `second-step` (no hash) | 3 tries | the failure counts above | every refusal | none |
| `setup/password` | 3 | 10 / 15 min; 6 per code | every call | open only |
| `setup/authenticator` (a seal) | 5 | 5 setups per code | every call | none |
| `setup/confirm` (no hash) | 5 tries | 5 setups per code | every refusal | none |
| `change` (two hashes) | (session) | 10 / 15 min; the re-authentication count | every call | open only, both |
| `recovery-codes` (no hash) | (session) | 10 / 15 min; the re-authentication count | every call | none |

**The per-address limit counts real addresses.** `pmos-web` refuses to start with password
sign-in on unless it knows where browsers' addresses come from: either
`PMOS_WEB_TRUSTED_PROXIES` names the reverse proxy, or `PMOS_WEB_DIRECT=1` says browsers reach
it directly with no proxy, which is allowed on a loopback `PMOS_WEB_BIND` only (the test stack).
With no trusted proxy, a password request that carries `X-Forwarded-For`, `Forwarded` or
`X-Real-IP` is refused (`403 proxy_not_trusted`): a proxy nobody named is in front, and every
browser would share its address. Behind a trusted proxy, `X-Forwarded-For` (every header, in
order) is read strictly from the right, **entry by entry on the raw bytes** (LI: an entry that
isn't text, or isn't an address, spoils only itself, so nginx's appending
`$proxy_add_x_forwarded_for`, which puts whatever the client sent to the left of the address
nginx saw, still names the client): each entry that is a trusted proxy is passed, the first
that isn't is the browser; `ip:port`, `[ipv6]` and `[ipv6]:port` are read; an entry that can't
be read ends the walk and is never skipped. **A wrong or stale setting is loud, never one shared
count** (M1): with a list set, a forwarded header from a peer that isn't on it (the proxy's
address moved), and a trusted proxy that names no client (no `X-Forwarded-For`, only proxies in
it, or an unreadable entry right after the trusted hops), are refused (`403
proxy_not_trusted`), counted on `/readyz` (`untrusted_proxy_refusals`, since start-up) and
logged at error level at most once a minute with the count and what to fix. A peer that isn't a
trusted proxy and forwards nothing is counted by its own address. An IPv6 address counts by its
/64.

**How the tool server knows the address** (M1). Only from `pmos-web`: the `source` field of a
password call, which `pmos-web` makes with its own token (`PMOS_WEB_TOKEN`; no other client
reaches these routes). The tool server never uses the peer address of the connection (that is
`pmos-web`'s own), and `pmos-web` never sends a proxy's address as anyone's: a request whose
client it can't name is refused before the tool server is asked. A call with no `source` (only
a `pmos-web` served without peer addresses: tests) shares one key, and that key **never** becomes
anyone's known address (tested), so it can't open the reserve or the ceiling's exemption. The
address is hashed before it is stored and never logged.

**What holds against a compromised `pmos-web`** (G4). The tool server checks `source` only as an
address: whoever holds `pmos-web`'s token (`PMOS_WEB_TOKEN`) chooses it freely. So **the
per-address and per-block rules protect against browsers**, the clients `pmos-web` counts
honestly; against a compromised `pmos-web`, or anyone with its token, they do not: such a caller
can present every guess as coming from a fresh address, a known one, or many blocks. **It can
also claim a person's own known address** (one they fully signed in from, if it learns it, for
example from a proxy's log): its guesses from there use the reserve, are spared the ceiling's
rationing for that person (that address keeps its own schedule, about 104 a day), and can make
that address wait for that account, so the person waits there while it goes on (from another
address they don't). What still
holds then is what doesn't depend on the address: the per-account rules (10 pieces of work
after an authentication per 15 minutes; the per-token and per-code counts; the ceiling's
rationing, which a caller who invents a new address for each guess passes, so guessing against
one account is then bounded only by the global limit, at most 604,800 a day), Argon2id behind
every guess, the global limit's 300 and 120 a minute, and **the second step**: a guessed password gives only
a pending token, and each account still needs its authenticator code (3 tries per token). A
compromised `pmos-web` can also reset anyone who holds neither Admin nor Auditor (threat table).

**Behind the deploy kit's reverse proxy.** The web app listens on `0.0.0.0:8770` in its
container and is published on the host's `127.0.0.1:8770`; the reverse proxy on the host reaches
it there, so its requests arrive from the `pmos` network's **gateway**. `deploy/compose.yaml`
**pins** that network's subnet and gateway (`PMOS_NETWORK_SUBNET`, default `10.201.77.0/24`;
`PMOS_NETWORK_GATEWAY`, default `10.201.77.1`), so the gateway doesn't move when the network is
made again; the default is outside Docker's own pools (`172.17`–`172.31.0.0/16`,
`192.168.0.0/16`), so Docker never gives it to another network, and a site that already routes
it sets both in `deploy/.env`. `deploy/env.example` sets `PMOS_WEB_TRUSTED_PROXIES=10.201.77.1`
to match. `deploy/pmos-admin up --web` refuses, before anything starts and with the exact fix:
while `PMOS_WEB_TRUSTED_PROXIES` is blank (it prints the gateway); while the network's real
gateway (`docker network inspect pmos_pmos`) isn't `PMOS_NETWORK_GATEWAY` (a network made before
it was pinned: `deploy/pmos-admin down && deploy/pmos-admin up --web` re-creates it); while an
address in `PMOS_WEB_TRUSTED_PROXIES` is neither the gateway nor inside the pinned subnet (a
proxy in a container on the `pmos` network has an address inside it); and while
`PMOS_WEB_PUBLIC_URL` isn't `https://`. The proxy must set `X-Forwarded-For` (Caddy does; nginx
with `proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for`). **Since the gate review of
`04c175f` (GL4) only the gateway is accepted by default**: an address inside the subnet (a proxy
in a container on the `pmos` network) is assigned by Docker and can pass to another container
after a restart, which would then be believed about which browser it speaks for; `up --web`
refuses it unless `deploy/.env` also sets `PMOS_WEB_TRUST_PROXY_IN_NETWORK=1`, and then warns
every time (pin that container's address with `ipv4_address`). An address outside the subnet
is refused whatever the setting. **Every process on the host that can connect to the published
port `127.0.0.1:8770` also arrives from the gateway** and is believed about its
`X-Forwarded-For`: it can name any browser address it likes, so it can step around the
per-address and per-block limits (and make another address wait for an account), exactly like
a compromised `pmos-web` above, with the second step still required. What a host operator
should do: treat the host as part of the sign-in's trust boundary; run only PM OS and the
reverse proxy there; give no untrusted user a shell on it; and, where other software must run
there, keep it from reaching `127.0.0.1:8770` (a host firewall rule that lets only the proxy's
user connect, for example `iptables -A OUTPUT -o lo -p tcp --dport 8770 -m owner ! --uid-owner
<proxy user> -j REJECT`). **Not verified on a real host:**
that the published port's traffic arrives from the gateway (it does with Docker's default
userland proxy and with its iptables rules). If it doesn't, nothing is shared silently any more:
every password request is refused `403 proxy_not_trusted`, `/readyz` counts them and carries
the warning `untrusted_proxy_refusals` for 15 minutes after the last one, and `pmos-admin
status` prints it as `WARN` with what to fix (GL3), so the first sign-in after setting it up
shows it.

## Threats

| Threat | How the design answers it |
| --- | --- |
| Guessing passwords online | Argon2id per guess; 46,146 common passwords refused, with or without their spaces, and nothing shorter than 12 without its spaces (M3); 5 failures per account per address then a growing wait to 15 min; 50 per account from anywhere new until 24 quiet hours, then one guess per 15 minutes per new address, and at most 60 hashes a minute per network block (at most 50 + min(96 × A, 86,400 × B, 432,000) a day with A addresses in B blocks; at most 604,800 a day for the whole service); 10 refused attempts per account per address, and 100 per address, per 15 min at `pmos-web`; 2 hashes at a time; and a right password still needs the second step. The address rules hold against browsers, not against a compromised `pmos-web` (G4, above). Guesses in parallel can't pass the threshold: a failure is counted before the hash is checked and taken back when the password was right (tested with 12 at once); `pmos-web`'s check and count are one step. |
| Telling which accounts exist | One answer, status and work at the password step and the setup's first step for every reason; a dummy hash where there is no real one; a waiting address answers the same; **under load too**: whether an attempt may use the reserve is decided from the address alone, before the name is looked up (M3, tested with the open allowance full). An Auditor reading the audit log sees one reason (`sign_in_failed`, `setup_failed`) and no target for those refusals; an Admin sees each reason. The one other answer: an account whose correct password was given 10 times in 15 minutes answers `429`, which only the password's holder can cause (kept after the review of `8e8f8fe`: "Signing in", step 2). Past a block's setup guesses every refusal is `429` whatever the name; a right code still begins (the pair is 100 bits). |
| Shutting everyone out with junk | Each network block takes at most 60 of the open 300 hashes a minute, and once 240 are taken at most 6 of the rest, so filling it takes fourteen or more blocks (G1, M4, and the re-check of the `8e8f8fe` fixes); `pmos-web` doesn't count the tool server's shared-limit `429`s, so a full minute never locks an office's address out; the global limit has a reserve only password steps from known addresses may use, at most 2 a minute per address and 24 per block, so filling it takes known addresses in five blocks; an invalid token costs nothing; every refused attempt counts against its address and its subject at `pmos-web`, which counts real browser addresses (a trusted proxy is required, or a loopback-only direct setting); a few people's mistakes behind one address never lock the others there out (N2). **Residual:** fourteen or more blocks can hold off sign-ins from addresses nobody has signed in from while they flood; and someone behind a shared address who sends refused attempts about ten or more names (or made-up tokens) at their limit, or a hundred at different ones, in 15 minutes makes that address wait for everyone there until the window moves (the cost of bounding spraying per address). |
| A compromised `pmos-web`, or its token, or a process on the host reaching the published port (G4, GL4) | It chooses the address each attempt is counted under, so the per-address and per-block rules don't bind it, and it can claim a person's known address (the reserve, no rationing past the ceiling, and a wait for the person there); the per-account work and per-token and per-code counts, the global limit, Argon2id and the second step still do. `pmos-admin` trusts only the network's gateway by default; the host is part of the trust boundary (above). |
| A credential holder spending what everyone shares (H1) | A new invitee, someone with an intercepted setup code, a signed-in user or someone with a password: every route that hashes or makes a secret after an authentication is bounded per token, per account (10 pieces of work in 15 minutes) and per address (every call counted at `pmos-web`), a setup code gives at most 5 setups and 6 password sets, and setup and the signed-in routes hash from the open allowance only, never the reserve. The loop from the re-check is a test: one code sets 6 passwords in all, and a sign-in from another address goes through meanwhile; since the gate review of `04c175f` also run in parallel (ten setups at once, ten sets each: still at most 6), with a sign-in from an address nobody had used going through during it (GL5). A refusal by the shared limits never costs the invitee a try (G3). |
| A wrong or stale trusted-proxy setting (M1) | The `pmos` network's gateway is pinned; `pmos-admin up --web` refuses a network whose real gateway differs, a trusted address outside the subnet, and one inside it unless `PMOS_WEB_TRUST_PROXY_IN_NETWORK=1` (with a warning, GL4); `pmos-web` refuses a forwarded header from a peer not on the list and a trusted proxy that names no client, counts both on `/readyz` with an error in its log, and while one is under 15 minutes old `/readyz` carries the warning `untrusted_proxy_refusals`, which `pmos-admin status` shows as `WARN` (GL3). No setting makes every browser share one count silently. |
| Plain HTTP between the services inside the stack (LJ) | **Accepted, as a property of the single-host deploy kit.** `pmos-web` reaches the tool server at `http://toolserver.pmos.internal:8710` on the `pmos` Docker bridge network (`deploy/compose.yaml`), so its token, a forwarded ID token on a first link, and what a person typed into a password step cross that network unencrypted. Every container on it runs from the kit's own images on one host, the network isn't published, and the browser's connection to the reverse proxy is `https://` (required with password sign-in). Reading it takes root on the host or a container on the network, which already reaches the database and the vault key. **What would change it:** running the services on more than one host, or on a network shared with anything that isn't PM OS (another project's containers, an orchestrator's overlay), or a policy that needs encryption in transit everywhere: then each service needs TLS (or mutual TLS) on its internal listener, with the tool server's and the runtime's URLs moved to `https://`. Not built. |
| A stolen password | Useless alone: the second step is required, always, with no switch to turn it off. |
| A replayed authenticator code | Each step is accepted once (the last accepted step is stored); a code older than the last accepted one is refused too. |
| Guessing authenticator codes | Only after a correct password, 3 tries per pending token, counted with password failures toward the same wait. |
| Stolen recovery codes | Single use, 80 bits, stored as SHA-256 bound to the person; regenerating replaces all ten. |
| A copy of the database | Argon2id hashes; sealed secrets that need the vault key and don't unseal for another person; SHA-256 of high-entropy codes and tokens only. |
| A stolen setup code | 24 hours, single use, needs the person's sign-in name; an Admin's or Auditor's must come from the host; a new code replaces it. Whoever holds the code can do at most 5 setups and 6 password sets with it. |
| Guessing setup codes | A code is 100 bits: guessing it isn't a way in. Wrong guesses count against the guesser's address (the same waits) and network block (100 in 15 minutes), never against the code or a setup in progress (G2), so nobody can void someone's code by guessing. The residual of the re-check (voiding each new code from four addresses) is gone. |
| An admin taking over another admin | Web-issued codes never work for an Admin or Auditor (checked at issue and again at every setup step), so a compromised `pmos-web` or admin session can't reset an Admin's or Auditor's password. A credential set with a web-issued code stops working when its person becomes an Admin or Auditor (refused at the password step and the second step, password sessions ended) until a host-issued setup replaces it. Nobody issues a code for themselves. |
| A compromised `pmos-web`, or an admin, resetting someone | Only people who hold neither Admin nor Auditor; each reset recorded in `role_changes` with the actor and `client = web`, listed on the overview, and shown to the person until they dismiss it ("reset by <name> on <date>"). It never sees a hash, a secret or a stored code. Requiring the person's second factor or a second admin was considered and not built (a person who lost their phone could never be reset). |
| Another service's token | The password routes answer only `pmos-web`'s token. |
| Login CSRF and cross-site posts | Every password write needs the app's origin and a JSON body; the signed-in ones the session's CSRF token too; the step cookies are `SameSite=Strict`, `HttpOnly`, `__Host-`. |
| Session theft after a reset or change | The session carries the credential's epoch and ends when it moves (≤ 10 s; at once in the `pmos-web` that made the change, which also ends the person's other sessions). The bound holds while the tool server is unreachable too: a password session never runs on a last-known-good answer. A session without an epoch is never started. |
| A deactivated person | Can't sign in (the uniform refusal), can't finish a second step or a setup, and their sessions end within 10 seconds. At the end of the 24-hour grace their password credential is deleted with their linked accounts. |
| A lockout used to shut someone out | The wait is per address: someone else's guesses never make the owner wait from their own address. Past the account's ceiling a new address is rationed (one attempt per 15 minutes), never refused, so the owner gets in from anywhere with the right password and code (M2). A setup code has no account-wide ceiling, and no guess counts against it (G2). |
| Secrets in logs or the audit log | No password, code, token, secret or sign-in name is logged or audited; details are fixed words and counts (`password::is_safe_detail`), tested by capturing the log and reading every audit event. |
| Exhausting the CPU through sign-in | Two hashes at a time, a 10-second wait, the global and per-address limits. |
| The `evals` account | Can never have a password: refused at issue, and by the tables' checks. |

## What is recorded

Audit events (`client` `web`, or `host` for a host-issued code; `task_id` `password`), metadata
only:

| `action` | `actor` | `outcome` | `target` | `detail` |
| --- | --- | --- | --- | --- |
| `password.sign_in` | `anonymous` | `ok` / `denied` | the person, or none for an unknown name | `password` (`; rehashed`, or `; rehash_deferred` when the allowance had no room) / `unknown_name`, `no_password`, `wrong_password`, `deactivated`, `locked`, `host_only`, `stale` |
| `password.second_step` | the person / `anonymous` | `ok` / `denied` | the person | `authenticator`, `recovery_code; recovery_left 9` / `wrong_code`, `locked`, `deactivated`, `expired`, `host_only`, `stale` |
| `password.reset_notice` | the person | `ok` | the person | `dismissed` |
| `password.lockout` | `anonymous` | `denied` | the person | `failures 5` (`; setup` for setup-code guesses); `ceiling` when the account reaches 50 failures |
| `password.setup_code` | the admin / `host` / `anonymous` | `ok` / `denied` | the person | `setup` or `reset` (`host_user <name>; …` from the host) / the refusal's code (before `04c175f` also `voided; failures 20`, a code void after wrong guesses; no longer made) |
| `password.setup_begin` | `anonymous` | `ok` / `denied` | the person, or none | `begin; setup` / `unknown_name`, `wrong_code`, `expired`, `locked`, `deactivated`, `spent`, …; `block_limit; refused <n>` (no target: refusals past a block's guesses, at most one row a minute per block, each standing for the refusals since the block's last row) |
| `password.setup_complete` | the person / `anonymous` | `ok` / `denied` | the person | `setup` or `reset` / `wrong_code`, `locked`, `host_only`, … |
| `password.change` | the person | `ok` / `denied` | the person | `change` / `reauth_failed`, `locked` |
| `password.recovery_codes` | the person | `ok` / `denied` | the person | `regenerated` / `reauth_failed`, `locked` |
| `web.sign_in`, `web.sign_out` | the person | `ok` | none | `provider: password` (from `pmos-web`) |

**What an Auditor sees** (L15): a refused `password.sign_in`, and a refused
`password.setup_begin` before its code matched, show the one reason `sign_in_failed` (or
`setup_failed`) and no target, so the log can't tell an Auditor whether a sign-in name they
tried exists. An Admin (and a person holding both roles) sees each reason and target.

**How reliable the records are** (L14). Every `password.*` event is written by the tool server
in the same transaction as the decision it records: if the audit log can't take it, nothing
changes and the request fails (`503`). `web.sign_in` and `web.sign_out` come from `pmos-web`
afterwards and are **best effort**: a sign-in is not refused because its event couldn't be
recorded (a session alone reaches nothing; everything it does is audited where it is done).
A failure is never silent: `pmos-web` logs `AUDIT: a sign-in event wasn't recorded` at error
level and counts it in `/readyz` (`unrecorded_sign_in_events`, since start-up); the tool
server's own `password.second_step` event records the same sign-in in any case.

`role_changes`: `issue_password_code` and `set_password`, each with detail `setup` or `reset`,
the actor, the client and, from the host, `host_user`; and `dismiss_reset_notice` (detail
`reset`, the person's own session, when) since migration 0023. The usual retention applies: audit
events go at 90 days, `role_changes` rows 90 days after the person's deactivation. Pending
tokens and setup codes are deleted once past their expiry, an address's counters once they hold
no failure from the last 24 hours and no sign-in from the last 30 days; a deactivated person's
credential (and their counters) at the end of the 24-hour grace.

## Configuration

| Variable | Service | What it does |
| --- | --- | --- |
| `PMOS_WEB_PASSWORD_LOGIN=1` | `pmos-web` | Turns password sign-in on (off by default); `GET /api/auth/providers` then lists `password`. Refuses to start without a `web:toolserver` signing key or with `PMOS_ASSERTION_MODE` other than `require`; without `PMOS_WEB_TRUSTED_PROXIES` or `PMOS_WEB_DIRECT=1`; and on a bind that isn't loopback without an `https://` `PMOS_WEB_PUBLIC_URL`. |
| `PMOS_WEB_TRUSTED_PROXIES` | `pmos-web` | **Required with password sign-in** unless `PMOS_WEB_DIRECT=1`: the address(es) the reverse proxy reaches `pmos-web` from, comma-separated. Their `X-Forwarded-For` names the browser; a forwarded header from any other peer, or a trusted proxy that names no client, is refused and counted on `/readyz` (`untrusted_proxy_refusals`). In the deploy kit: the `pmos` network's pinned gateway, `10.201.77.1` by default (above). |
| `PMOS_WEB_TRUST_PROXY_IN_NETWORK=1` | `pmos-admin` only | Lets `PMOS_WEB_TRUSTED_PROXIES` name an address inside the `pmos` subnet (a reverse proxy in a container on the network, its address pinned); without it `up --web` accepts only the gateway (GL4), and with it warns every time. Never trusts an address outside the subnet. |
| `PMOS_NETWORK_SUBNET`, `PMOS_NETWORK_GATEWAY` | deploy kit (`deploy/compose.yaml`) | The `pmos` network's subnet and gateway, pinned (default `10.201.77.0/24`, `10.201.77.1`). Change both only if the site already routes that range; then re-create the network (`deploy/pmos-admin down`, then `up`) and set `PMOS_WEB_TRUSTED_PROXIES` to the new gateway. `pmos-admin up --web` checks all three. |
| `PMOS_WEB_DIRECT=1` | `pmos-web` | Browsers reach `pmos-web` with no proxy: each is counted by its own address, and a forwarded header is refused. Only on a loopback `PMOS_WEB_BIND`, and never with `PMOS_WEB_TRUSTED_PROXIES` (both refuse to start). The test stack uses it. |
| `PMOS_WEB_DEV_LOGIN_WITH_PASSWORD=1` | `pmos-web` | Lets development sign-in run beside password sign-in (a loud warning at start-up). Without it, both on refuses to start: development sign-in lets anyone in without a password. A developer's own machine only. |
| `PMOS_VAULT_KEY` | tool server | The vault's keys; password sign-in needs them to seal authenticator secrets. Required with live connectors; optional with fake ones. |
| `PMOS_DEV_PASSWORD_SETUP_FOR`, `PMOS_DEV_PASSWORD_SETUP_FILE` | tool server | **Development only**: at start-up, issue a setup code for that seeded user, as the host would, into a new 0600 file. Refused with a database, live connectors, no seed or no vault key. `scripts/claude-test-stack.sh` uses it for `PMOS_TEST_PASSWORD_LOGIN=1`. |

In the deploy kit, `deploy/compose.yaml` passes `PMOS_WEB_PASSWORD_LOGIN`,
`PMOS_WEB_TRUSTED_PROXIES`, `PMOS_WEB_PUBLIC_URL` and `PMOS_ASSERTION_MODE` to the web app (never
`PMOS_WEB_DIRECT`), and `deploy/pmos-admin up --web` starts the web app with password sign-in as
its provider too, once `PMOS_WEB_TRUSTED_PROXIES` and an `https://` `PMOS_WEB_PUBLIC_URL` are set
(it says what to set otherwise). `pmos-admin users password-setup <handle>` issues a code on the
host.

## The screens

Built in `web/` from `docs/web-api.md`: the sign-in card with the code step, the setup page
(`#/setup`, the QR code drawn in the browser), "Sign-in and security" (`#/security`: change
password, new recovery codes), the reset notice at the top of every page until dismissed, the
Team page's setup or reset code action and its "Password needs host" tag, and the overview's
list of the week's resets.

## What changed after review

The independent review of `b469d5b` (2026-11-20). Each change has a test that failed with the
fix taken out.

| Finding | What changed |
| --- | --- |
| P1 (High): anyone could exhaust the shared limit | The token is checked before any work; routes that compute no hash take nothing from the global limit; the global limit has a reserve for real accounts below their threshold; `pmos-web` counts **every** refused attempt against the address, not three codes. |
| P2 (High): the per-address limit was one limit for everyone behind the kit's proxy | `pmos-web` refuses to start with password sign-in on unless `PMOS_WEB_TRUSTED_PROXIES` is set or `PMOS_WEB_DIRECT=1` (loopback only); a forwarded header with no trusted proxy is refused; `pmos-admin up --web` says what to set (the `pmos` network's gateway). |
| PM1: a lockout an attacker could hold | The wait is per account and address; an account-wide ceiling of 50 past which only known addresses may try. The "known residual" paragraph is gone. |
| PM2: a silent reset | A reset leaves the person a notice until they dismiss it, and the overview lists resets. A second factor or second admin was considered and not built (above). |
| PM3: a web-issued credential survived promotion | The credential records `issued_via`; a web-issued one is refused for an Admin or Auditor until a host-issued setup, their password sessions end, the Team page says so. |
| PM4: a session could outlive a reset by 5 minutes | A password session never uses the last-known-good answer: 10 seconds is the bound either way. |
| L1 | `X-Forwarded-For` read strictly from the right; `ip:port` and `[ipv6]:port` read; an unreadable entry ends the walk. |
| L2 | The per-address check and count are one step. |
| L3 | IPv6 counts by /64, at `pmos-web` and at the tool server. |
| L4 | Development sign-in beside password sign-in needs `PMOS_WEB_DEV_LOGIN_WITH_PASSWORD=1`. |
| L5 | Password sign-in on a bind that isn't loopback needs an `https://` public URL. |
| L6 | 4 KiB body limit on `pmos-web`'s password routes too. |
| L7 | A password sign-in whose answer has no epoch starts no session. |
| L12 | The types that hold a hash, a sealed secret or a token's hash write their own `Debug`, redacting them (tested). |
| L14 | No code change: the tool server's events were already in the decision's transaction, and `pmos-web`'s best-effort sign-in events already logged and counted their failures; this document now says so. |
| L15 | An Auditor sees one reason, and no target, for refused password steps. |

## Re-check of eb24d28

The independent re-check of `eb24d28` (2026-11-21). Each change has a test that reproduces the
finding; the ones that use behaviour or tables that didn't exist before fail by construction
against `eb24d28`, and the purge-trigger one (LG) was also run against the old migrations and
failed there. Migration `0023_gate_fixes_2.sql`. Findings about the Entra first link (M4, LB to
LE) are in `docs/entra-sign-in.md`, "Re-check of eb24d28".

| Finding | What changed | The test |
| --- | --- | --- |
| H1 (High): one setup token or code could fill the shared hashing allowance and its reserve | Per setup token: 3 password sets, 5 authenticator secrets, then the token is spent. Per code across renewals: 5 setups started, 6 password sets, then `409 setup_code_spent` until an admin issues a new one (6 = two tokens' worth: a typo or an abandoned setup still finishes). Setup hashes come from the open allowance only. `pmos-web` counts every `setup/password` and `setup/authenticator` call against the address, successes included. Every route that hashes after any authentication is bounded per account: 10 pieces of work in 15 minutes (a correct password step, a setup's password, a change, new recovery codes). | `one_setup_code_cannot_spend_the_shared_allowance` (the loop: 6 sets in all, then spent; with the open allowance full of setup work a sign-in from another address goes through); `correct_password_steps_are_bounded_per_account`; `setup_passwords_count_against_the_address_even_when_they_succeed` (`pmos-web`) |
| LA: `change` and `recovery-codes` hashed outside the global limit | `change` takes both hashes from the open allowance before either; both routes count against the account's work and, at `pmos-web`, the address on every call. `recovery-codes` computes no Argon2 hash (SHA-256 only), so it takes nothing from the global limit. | `change_and_recovery_codes_are_bounded`; the `pmos-web` test above |
| M1: a wrong or stale `PMOS_WEB_TRUSTED_PROXIES` silently restored one shared limit | The `pmos` network's subnet and gateway are pinned (`10.201.77.0/24`, `10.201.77.1`, overridable). `pmos-admin up --web` refuses a network whose real gateway differs (re-create it) and a trusted address neither the gateway nor in the subnet, with the exact line to set. `pmos-web` refuses a forwarded header from a peer not on the list and a trusted proxy that names no client (`403 proxy_not_trusted`), counts them on `/readyz` (`untrusted_proxy_refusals`) and logs an error at most once a minute. The tool server takes the address only from `pmos-web`'s token-authenticated `source`, `pmos-web` never sends a proxy's address, and the shared no-address key never becomes a known address. The test stack (loopback, `PMOS_WEB_DIRECT=1`) is unchanged. | `a_wrong_or_stale_proxy_setting_is_refused_and_counted` (`pmos-web`); `a_wrong_or_stale_proxy_setting_is_refused_not_shared` (unit); `no_address_never_becomes_a_known_address`; `scripts/test-pmos-admin.sh` (stale network, wrong address, container proxy, override, the rendered subnet) |
| M2: the account ceiling was a lockout an attacker could hold, for sign-in and for setup codes | Past the ceiling an unknown address is rationed (one attempt per 15 minutes), never refused: the right password and code always get in. Bound: 50 + 96 × A guesses a day with A addresses, then the second step. Setup codes have no account-wide ceiling: each wrong guess counts against the live code, void at 20 (audited, listed on the admin overview as `password_codes_voided`); a new code starts clean and isn't held up. The residual (a determined guesser can void each new code) is in the threat table. | `guesses_from_one_address_never_hold_the_owner_out` (the owner from a new address past the ceiling); `setup_code_guesses_void_the_code_not_the_person`; `an_address_waits_alone_and_past_the_ceiling_new_addresses_are_rationed` (unit) |
| M3: under load an unknown name got `429` and a real account `401` | Whether a password step may use the reserve is decided from the address alone, before the name is looked up: an address someone fully signed in from in the last 30 days. Every refusal on that path draws from the same allowance. | `under_load_an_unknown_name_and_a_real_account_answer_alike` (open allowance full: both `429` from a new address, both `401` from a known one) |
| LF: the reset notice could be dismissed unrecorded by whoever holds the account; the overview read only the last 500 role changes | A dismissal is a `role_changes` row (`dismiss_reset_notice`: who, when). The overview reads resets and dismissals from `role_changes` by time and shows each reset's `dismissed_at`. | `a_dismissal_is_recorded_and_resets_are_read_by_time` (520 later role changes; the reset is still listed) |
| LG: the purge triggers matched a function name in PG_CONTEXT, which a quoted name containing a newline could spoof | Confirmed: identifiers are cut at 63 bytes, so the spoof fits beside a short schema name (role history's frame with a schema of up to 10 characters, `public` included; the audit log's with one), and with the old triggers the crafted function deleted old rows. Now the purge functions mark their own transaction in `purge_marks` (owner-only; EXECUTE on the purges revoked from PUBLIC) and the triggers look for the mark; no name is read. | `postgres_a_crafted_function_name_cannot_pass_as_the_purge` (a one-letter schema; failed against migration 0022, passes with 0023) |
| LH: `errorText` showed the server's message for an unknown code | It shows the caller's fixed sentence for any code the app doesn't know (and the app's own sentence for "no answer"); `messageOf` does the same for unknown codes. | `web/src/errors.test.ts` |
| LI: with nginx's appending header, a non-text client value failed the whole header and the client counted as the proxy | `X-Forwarded-For` is split and parsed entry by entry on the raw bytes, from the right; the first unreadable or untrusted entry is the boundary, and a walk that reads only trusted hops is refused (M1), never the proxy's address. | `a_malformed_client_entry_with_an_appending_proxy_is_its_own_entry` (non-UTF-8 bytes, `unknown`, bad ports, empty entries) |
| LJ: plain HTTP between the services inside the stack | Not changed: documented in the threat table as a property of the single-host deploy kit, with what would change it. | (none) |
| Missing tests | The authenticator secret is absent from web storage, readable cookies, the address and history on the setup page, before and after it finishes. | `keeps the authenticator secret out of web storage, cookies and history` (`Setup.test.tsx`) |

## Gate review of 04c175f

The security gate at `04c175f` (2026-11-21) passed: no High or Critical open. Its five Mediums
and five Lows on password sign-in, and what was done (branch `claude/gate-fix-3`). No migration:
`password_setup_codes.voided_at` stays as a column no code sets any more.

| Finding | Confirmed? | What changed | The test |
| --- | --- | --- | --- |
| G1: a flood from a few hundred addresses filled the open allowance, and an insider signed in from many addresses could fill the reserve | Yes (the open allowance had no per-network share; the reserve was open to any known address without a cap) | Each IPv4 /24 or IPv6 /48 takes at most 30 of the open 300 hashes a minute; each address at most 2 of the reserve's 120 a minute and each block at most 24. Setup and signed-in work count under the account's own key. | `a_flood_from_a_few_blocks_leaves_room_for_another` (600 guesses from three /24s; a sign-in from a fourth block, from an unused address, goes through); `one_address_takes_at_most_two_of_the_reserve_a_minute`; unit: `one_block_takes_only_its_share_of_the_open_allowance`, `the_reserve_is_shared_by_address_and_by_block`, `blocks_are_ipv4_24s_and_ipv6_48s` |
| G2: 20 wrong guesses from 4 addresses voided each new setup code and killed a setup in progress | Yes (`setup_begin` counted every wrong guess against the person's live code and deleted their setup tokens at 20) | No voiding: a wrong guess counts against the guesser's address (per person, as before) and its block (100 in 15 minutes, then `429`, decided before the name lookup). The overview's `password_codes_voided` list is gone from the tool server and the app (types, Overview, copy); the code keeps a plain per-code count, and the tool server's `/readyz` shows `password.setup_code_wrong_guesses` since start-up. A 100-bit code needs no guess cap of its own. | `wrong_setup_guesses_never_void_a_code_or_end_a_setup` (40 wrong guesses while a setup is in progress; it finishes; nothing voided; the count is 80); `setup_code_guesses_are_bounded_per_block`; unit: `setup_guesses_are_bounded_per_block_and_given_back_when_right` |
| G3: a `429` still cost the invitee's token and code (six and the code was spent) | Yes (the counts were committed before the shared limits were taken; the test asserted it) | The account's work and the open allowance are taken inside the counting transaction, before the counts: a `429` writes nothing. A `503 sign_in_busy` (no hashing slot) gives the set back to the token, the code and the account; only sets whose hash can run are counted, so H1's 6 hashes per code still hold. The same for the password step: a busy check gives back the failure it counted ahead (a wait it started stays). | `one_setup_code_cannot_spend_the_shared_allowance` (rewritten: twenty `429`s, then the same token still sets 3 and the code 6 in all, then spent); `one_setup_code_in_parallel_still_hashes_at_most_six_times`. The `503` give-back path has no test of its own (it needs every hashing slot held for 10 seconds). |
| G4: the per-address rules hold against browsers only | Yes (the tool server validates `source` only as an address) | Documented: "What holds against a compromised `pmos-web`" and the threat table. No code change. | (none) |
| G5: the guess budget 50 + 96 × A was unbounded with a /48; the common list had 60 entries | Yes | The block share bounds it: 50 + min(96 × A, 43,200 × B) a day per account, 604,800 a day for the service (above). The common list is 46,146 entries from SecLists' top 1,000,000 (MIT), with its source, derivation, hashes and licence in `crates/toolserver/data/common-passwords.NOTICE`, checked by one hash-set lookup. | `the_common_list_is_a_real_one` (unit: size, shape, entries refused as `common` in any case and with spaces) |
| GL1: correct password steps were checked before the hash and counted after, so about 20 in parallel passed | Yes | The room is still checked before the hash (a full one costs no hash), and is taken, in one step, in the transaction that starts the pending token. Counting it before the hash was considered and not done: a wrong guess in flight would then hold the owner's room, and an attacker sending ten at once would get `429` for a real account and `401` for an unknown name, an answer that tells which accounts exist (M3). `pmos-web` still gives a success's address slot back (a NAT'd office's sign-ins shouldn't share a cap); the account's room is what bounds them. | `parallel_correct_password_steps_are_bounded_per_account` (20 at once from 5 addresses: exactly 10 pass, the rest `429`) |
| GL2: a rehash took nothing from the shared allowance | Yes | It draws like any hash; with no room (or no free slot) it is put off: the sign-in goes on, `password; rehash_deferred` is recorded, `/readyz` counts it, the next correct sign-in with room does it. | `a_rehash_draws_from_the_shared_allowance` |
| GL3: a wrong proxy setting answered everyone `403 proxy_not_trusted` while `/readyz` stayed ready with no warning | Yes | `/readyz` carries `warnings: ["untrusted_proxy_refusals"]` for 15 minutes after the last such refusal (still ready: everything else serves); `pmos-admin status` prints it as `WARN` with what to fix. | `a_wrong_or_stale_proxy_setting_is_refused_and_counted` (`pmos-web`, with the clock moved on); `readyz_checks_the_store_and_both_upstreams`; `scripts/test-pmos-admin.sh` (status with and without a warning) |
| GL4: `pmos-admin` accepted any address in the subnet; any host process reaching the loopback port appears as the gateway | Yes | Only the gateway by default; an address inside the subnet needs `PMOS_WEB_TRUST_PROXY_IN_NETWORK=1` and is warned about every time; outside the subnet is refused always. The host trust boundary, and what an operator should do, are documented above. | `scripts/test-pmos-admin.sh` (refused without the override, with the fix named; started with it, with the warning; outside the subnet refused with it) |
| GL5: the H1 test ran sequentially | Yes | A parallel case and a legitimate user on a new address during the loop. | `one_setup_code_in_parallel_still_hashes_at_most_six_times` |

**Every limit's value after this review:** 5 failures per account per address, then 30 s
doubling to 15 min; 50 per account from every address, then one attempt per 15 minutes per new
address; 24 hours to forget; setup-code guesses: the same per address, and 100 per block per 15
minutes; 10 pieces of work per account per 15 minutes; 3 password sets and 5 secrets per setup
token; 5 setups and 6 password sets per code; 300 hashes a minute open, 30 of them per block;
a reserve of 120, 2 per address and 24 per block; 2 hashes at a time, 10 seconds' wait; 20
refused attempts per address per 15 minutes at `pmos-web`; 46,146 common passwords.

## Review of 8e8f8fe

The review of `8e8f8fe` (2026-11-21): two Mediums and the password Lows, and what was done
(branch `claude/gate-fix-4-password`). No migration. Each finding was checked in the code
first, and each new or changed test was run against the code with the fix taken out and
failed there.

| Finding | Confirmed? | What changed | The test |
| --- | --- | --- | --- |
| M3: whitespace passwords (twelve spaces passed; `password` and four spaces passed; a test asserted `"            a"` was accepted) | Yes (`hash.rs`: the length counted the untrimmed text, the list check trimmed it) | The rules read the password with whitespace at either end dropped and each inner run of Unicode whitespace made one space (`hash::collapsed`): the length (at least 12) is counted on that, the most (128) on the text as typed; the name and common-list checks compare it lower-cased, and again with every space taken out. **What is hashed is unchanged**: exactly what was typed, so every existing password signs in as before. Not normalised, and why: "The rules for a password". The test that asserted `"            a"` was accepted now asserts it is too short. | `new_password_rules` (unit: twelve spaces, padded and spaced-out common ones, Unicode spaces, what isn't normalised); `a_password_set_before_the_new_rules_still_signs_in` (a stored hash of `password` and four spaces signs in typed as set, not trimmed; a new password with inner runs is hashed as typed; setup refuses the M3 cases) |
| M4: the block share was a new lockout (30 hashes a minute per /24 from the first, and `pmos-web` counted every tool-server `429`) | Yes (`limits.rs`; `pmos-web`'s `answered`) | The shares apply only once 240 (80%) of the open 300 were taken in the minute; then each block takes at most 6 of the last 60, so filling the allowance takes ten blocks besides the one that took the first 240 (G1 holds where it matters, at the top). `pmos-web` no longer counts a tool-server `429` on `sign-in` or `setup/password` (shared limits, which checked nothing); it still counts the `429`s that are about the address's own guessing (second step, setup confirmation, a setup's first step over its block's guesses). The guessing bound is now 50 + min(96 × A, 354,240 × B, 432,000) a day (it was 43,200 × B): the cost of not locking an office out. The tests that asserted the old share (30, and 3 × 30 of 600) now assert the new one. | `a_busy_office_behind_one_address_signs_in_in_a_quiet_minute` (30 people, each mistyping once, 60 hashes from one new address, all sign in; then a flood takes 80% and only 6 past it, and a new block signs in); `a_flood_from_a_few_blocks_leaves_room_for_another` (now 258 of 600 taken); unit: `a_quiet_minute_has_no_shares_and_a_nearly_full_one_does`, `one_block_takes_only_its_share_of_the_open_allowance`; `the_tool_servers_shared_limit_refusals_are_not_counted_against_the_address` (`pmos-web`: thirty `429`s, then thirty sign-ins from the same address; wrong passwords still count) |
| Low: a right setup code was refused when its block had 100 wrong guesses (a test asserted it) | Yes (`setup_begin` refused before the lookup) | Past the block's guesses the first step is still checked; only a right, live code for the name goes on. Every other outcome is `429` whatever the name and writes nothing, so G2 holds: the answer says nothing about the name, and no guess touches a code. What an over-limit attempt still learns: whether its name and code are a right pair (100 bits) and, by timing, whether the name exists (an unknown name is one lookup, a real one a lookup and a short read: the same as below the limit); bounded by `pmos-web` counting each of these `429`s against the address (20 per 15 minutes). The test that asserted the right code was refused now asserts it begins. | `setup_code_guesses_are_bounded_per_block` (past 100: an unknown name and a wrong code `429` and nothing written; Ana's right code from the block begins and her setup goes on) |
| Low: a shared-limit refusal cost the invitee's `pmos-web` slot | Yes (`answered_with_work` kept the slot for every `4xx`) | A `429` on `setup/password` gives the slot back (the tool server did no work and counted nothing, G3). | `the_tool_servers_shared_limit_refusals_are_not_counted_against_the_address` (thirty `429`s, then the set goes through) |
| Low: `change()` gave nothing back on a busy check | Yes (the piece of work and the re-authentication failure stayed) | Busy at the check: both go back (a wait stays). Busy at the new hash after a right check: the failure goes back, the work stays (a hash ran). No room for the second hash: the piece goes back. The password step's G3 give-back, untested before, is tested here too. Test hooks: `PasswordService::with_hash_wait` and `hold_hashing_slots`. | `busy_hashing_gives_back_what_it_counted` (fifteen busy changes and twelve busy sign-ins, then both go through) |
| Low: a full work room answers `429`, not `401` | Yes, as documented | Kept, as this document chose it, and explained ("Signing in", step 2): only the holder fills the room (wrong guesses never count, and busy checks now give back), and a `401` would mean hashing every attempt on a full room, which a holder could loop past `pmos-web`'s count, or calling a right password wrong. | (none new: `correct_password_steps_are_bounded_per_account`, `parallel_correct_password_steps_are_bounded_per_account`) |
| Low: the setup-guess map swept only at 10,000 keys | Yes | It, and the per-account work map, forget idle keys at least every 5 minutes, and at 10,000 as before. | `the_guess_and_work_maps_forget_idle_keys_on_time` (unit) |
| Low: the G4 text omitted that a web-token holder can claim a person's known address | Yes | "What holds against a compromised `pmos-web`" and the threat table say it: the reserve, no rationing past the ceiling, and a wait for the person at that address. | (none: documentation) |

**Every limit's value after this review:** 12 to 128 characters (12 counted without
whitespace at either end and with each run once); 5 failures per account per address, then
30 s doubling to 15 min; 50 per account from every address, then one attempt per 15 minutes per
new address; 24 hours to forget; setup-code guesses: the same per address, and 100 per block per
15 minutes (past it only a right code begins); 10 pieces of work per account per 15 minutes; 3
password sets and 5 secrets per setup token; 5 setups and 6 password sets per code; 300 hashes
a minute open, no share below 240, then 6 of the last 60 per block; a reserve of 120, 2 per
address and 24 per block; 2 hashes at a time, 10 seconds' wait; 20 refused attempts per address
per 15 minutes at `pmos-web` (the tool server's shared-limit `429`s not counted); 46,146 common
passwords; idle guess and work entries forgotten every 5 minutes.

## Re-check of the 8e8f8fe fixes

The reviewer's re-check of branch `claude/gate-fix-4-password` (2026-11-22): one Medium and four
Lows, and what was done (branch `claude/gate-fix-5-password`). No migration. N2's office test
was run with the old limits put back (twenty per address, no subject count) and failed there;
the other new or changed tests assert behaviour or values the code before the fix didn't have
(a `429` at the 11th refusal, a refused zero-width password, an audit row, a share of 60), so
they fail against it by construction.

| Finding | Confirmed? | What changed | The test |
| --- | --- | --- | --- |
| N2 (Medium): `pmos-web` still locked a shared office address out | Yes (`crates/web/src/app/password.rs`: every wrong password counted against the client address, 20 per 15 minutes, then every password route answered `429` for everyone behind it; the tool server's office test covered only the tool server) | `pmos-web` counts each refused attempt against the address **and** what it is about from there (`Subject`: a sign-in name as the tool server reads it, one pending or setup token, one signed-in person, or nothing readable): **10** per subject and **100** per address in any 15 minutes. A subject past its 10 answers `429` and isn't counted against the address, so one person's mistakes add at most 10 to the address's count and only refusals about ten subjects at their limit, or a hundred mistakes, make the address wait. Successes, PM OS's own `5xx` and the tool server's shared-limit `429`s still give both slots back; `setup/password`, `setup/authenticator`, `change` and `recovery-codes` still count every call (H1, LA). The authenticator step is unchanged and still required. **Worst cases from one browser address**, per 15 minutes: 10 refused attempts about one account (960 a day; the tool server's own per-account, per-address schedule, about 104 checked guesses a day, binds first) and 100 across every account (spraying: 9,600 a day, from 1,920). Against a compromised `pmos-web` nothing here binds (G4). | `a_busy_office_behind_one_address_is_not_locked_out_by_a_few_peoples_mistakes` (30 people behind one address mistype twice each and sign in; three stuck people wait alone after 10 each; everyone else, and Ana, still sign in); `guessing_at_one_account_and_spraying_from_one_address_stay_bounded` (10 at one name then `429` without asking the tool server, the name spelled another way and a setup's first step at it included; 100 names from one address, then every route from it `429`, the right password too; other addresses unaffected; the window moves on); `every_refused_attempt_counts_against_the_address` (rewritten: 10 for one made-up token, 10 for nothing readable, 100 for the address); unit: `an_address_waits_after_a_hundred_refusals_and_a_subject_after_ten`, `concurrent_attempts_cannot_pass_the_limit_together` |
| Low (M3's residual): zero-width and other format characters passed the length rule | Yes (`split_whitespace` doesn't treat U+200B as whitespace; twelve of them were accepted) | The rules drop Unicode's `Default_Ignorable_Code_Point` characters (the exact list is in "The rules for a password") before measuring and comparing; and, since `unicode-normalization` was already in the build (through sqlx's `stringprep`) and the change is a few lines, the name and common-list checks compare in NFKC too, so full-width and other compatibility forms of a common password are refused. What is hashed is unchanged. The unit test that asserted `ｐａｓｓｗｏｒｄ１２３４` and `password` + U+200B + `1234` were accepted now asserts they are refused. | `new_password_rules` (unit: twelve and 128 zero-width spaces, joiners, a variation selector, a tag, a Hangul filler, a bidi override, full-width forms, a full-width sign-in name; still accepted: a Cyrillic look-alike, a visible password with an invisible character in it) |
| Low: a setup's first step refused past its block's 100 wrote no audit row | Yes | Counted on `/readyz` (`password.setup_block_refusals`, every one) and recorded thinned: the first refusal of a block, then the first one at least a minute after its last row, writes `password.setup_begin` `denied` with no target and `block_limit; refused <n>` (the refusals since the block's last row). So a flood writes at most one row a minute per block, fewer than the hundred rows its wrong guesses wrote to get there. Refusals after a block's last row appear in its next row, if one comes, and on `/readyz` always; past 10,000 blocks over their limit at once, a new block's refusals are on `/readyz` only. | `setup_code_guesses_are_bounded_per_block` (changed: it asserted nothing was written; now one row for three refusals, none for fifty more within the minute, then one standing for 53; the `/readyz` count); unit: `refusals_past_a_blocks_guesses_are_recorded_thinned` |
| Low: with `PMOS_WRITE_FIXTURES` set, the H1 fixture test wrote the fixture and compared it with itself | Yes (`crates/web/tests/bots.rs`) | The test refuses the variable while `CI` is set (it fails, saying the fixture is written on a developer's machine and committed). | `the_real_bots_answer_carries_the_preview_label_the_app_shows` |
| Low: one IPv6 /48 could take 246 of the 300 open hashes a minute (about 354,000 guesses a day against one account) | Yes | Each block takes at most **60** of the open allowance a minute at any load (`BLOCK_OPEN_PER_MINUTE`, a fifth), the band share of 6 past 240 stays: one /48 now checks at most 86,450 passwords a day against one account; filling the allowance takes fourteen blocks. The busy office (30 people, each one typo, 60 hashes in a minute) still fits; the trade-off past 60 is in "The global limit can't shut everyone out". The tests that asserted the old figures (240 + 6, 258 of 600, 186 for the flood after the office) now assert 60, 180 and 60. | `one_block_takes_only_its_share_of_the_open_allowance` (60, then fourteen or more blocks to fill); `a_quiet_minute_has_no_shares_and_a_nearly_full_one_does` (the office's 60 and no 61st); `a_flood_from_a_few_blocks_leaves_room_for_another`; `a_busy_office_behind_one_address_signs_in_in_a_quiet_minute` (tool server) |

**Every limit's value after this re-check:** 12 to 128 characters (12 counted without
whitespace at either end, with each run once and without characters that show nothing; name and
common checks in NFKC); 5 failures per account per address, then 30 s doubling to 15 min; 50 per
account from every address, then one attempt per 15 minutes per new address; 24 hours to
forget; setup-code guesses: the same per address, and 100 per block per 15 minutes (past it only
a right code begins; refusals recorded at most once a minute per block); 10 pieces of work per
account per 15 minutes; 3 password sets and 5 secrets per setup token; 5 setups and 6 password
sets per code; 300 hashes a minute open, at most 60 per block, and past 240 at most 6 of the
last 60 per block; a reserve of 120, 2 per address and 24 per block; 2 hashes at a time, 10
seconds' wait; at `pmos-web`, 10 refused attempts per address per subject and 100 per address
per 15 minutes (the tool server's shared-limit `429`s not counted); 46,146 common passwords;
idle guess and work entries forgotten every 5 minutes.

**Not verified:** a real office behind a real NAT (the tests drive pmos-web with
`X-Forwarded-For` from a trusted loopback proxy); the 60-hash share under real morning traffic
(only the 30-person case is tested); and NFKC's effect on passwords typed through real
keyboards and input methods (only the rule's unit test).

## Not built

- **Self-service reset by email**, by design: there is no email sending.
- **WebAuthn or passkeys**; only TOTP and recovery codes.
- **Breached-password lookups** (for example Have I Been Pwned): only the built-in list, so no
  password, or part of its hash, leaves PM OS.
- **Shared limits across several `pmos-web` or tool server processes**: `pmos-web`'s
  per-address limit and the tool server's global limit and per-account work are per process (one
  of each in the deploy kit; a restart empties them). The per-account, per-address waits, the
  account-wide ceiling and the per-token and per-code counts are in the database and hold across
  processes and restarts.
- **"End the old credential at once" on a reset** (above).
- **Not verified:** a real authenticator app scanning the URI (the codes are tested against
  RFC 6238's vectors and the URI against the Key Uri Format); a deployment behind a real reverse
  proxy, and the address it appears from on the host (the `pmos` network's gateway, by Docker's
  documented behaviour).
