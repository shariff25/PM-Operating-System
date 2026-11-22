//! The limits on guessing and on work (`docs/password-sign-in.md`, "Limits").
//!
//! - **Per account and address** ([`SourceCounters`]): failed passwords and failed
//!   second-step codes count together, setup-code guesses separately, for one person **from
//!   one address** (pmos-web's view of the browser: an IPv4 address or an IPv6 /64,
//!   [`source_key`]). From the [`THRESHOLD`]th failure on, that address waits [`delay`] for that
//!   person: 30 seconds, doubling with each further failure, at most [`MAX_DELAY`] (15 minutes).
//!   An attempt from that address during its wait is refused without being checked or
//!   counted; failures older than [`FORGET_AFTER`] (24 hours) are forgotten. **Another
//!   address is not held up**: the person signs in from theirs while someone else's address
//!   waits (the review of 2026-11-20, PM1).
//! - **Per account, every address together** ([`over_ceiling`]): past [`ACCOUNT_CEILING`]
//!   failures (until 24 hours pass with none), an address the person hasn't fully signed in
//!   from in the last [`KNOWN_SOURCE_DAYS`] days is **rationed**: after each failure it waits
//!   [`MAX_DELAY`] at once ([`fail_sign_in`]). It is never refused outright, so the owner,
//!   from a new address, with the right password and code, always gets in (the re-check of
//!   eb24d28, M2), and a correct password is always checked.
//! - **Hashing across the tool server** ([`GlobalLimit`]): at most [`GLOBAL_PER_MINUTE`]
//!   password hashes in any rolling minute for anyone, plus a reserve of
//!   [`GLOBAL_RESERVE_PER_MINUTE`] that only a password step **from an address someone fully
//!   signed in from** in the last [`KNOWN_SOURCE_DAYS`] days may use. Whether an attempt may use
//!   the reserve is decided from the address alone, before the sign-in name is looked up, so an
//!   unknown name and a real account get the same answer under load (M3). Setup and the
//!   signed-in routes use the open allowance only, never the reserve (H1, LA). Routes that
//!   compute no hash (the second step, a setup's first step and confirmation) take nothing.
//!   **Shares** (the gate review of 04c175f, G1 and G5, as changed by the review of 8e8f8fe,
//!   M4): each network block ([`block_key`]: an IPv4 /24, an IPv6 /48) takes at most
//!   [`BLOCK_OPEN_PER_MINUTE`] (a fifth) of the open allowance in any rolling minute (the review
//!   of 8e8f8fe, a Low: before, one block took up to 246); once [`SHARES_FROM_PER_MINUTE`]
//!   hashes (80%) were taken in the minute, each block takes at most [`BLOCK_SHARE_PER_MINUTE`]
//!   of the last fifth too. So a busy office behind one address takes its 60 in a minute, and a
//!   flood from a few blocks can't fill the allowance (that takes fourteen); each address takes at most
//!   [`RESERVE_PER_ADDRESS_PER_MINUTE`] of the reserve and each block at most
//!   [`RESERVE_PER_BLOCK_PER_MINUTE`], so someone who signed in from many addresses can't fill
//!   the reserve either.
//! - **Setup-code guesses per block** ([`BlockGuesses`]): at most [`SETUP_GUESSES_PER_BLOCK`]
//!   wrong setup codes from one block in any [`SETUP_GUESS_WINDOW_MINUTES`] minutes, then
//!   every refusal of the setup's first step from that block is `429`; a right code still
//!   begins (the review of 8e8f8fe). A wrong guess never touches anyone's code (G2): a 100-bit
//!   code can't be guessed, so it needs no cap of its own.
//! - **Work per account** ([`AccountWork`]): at most [`ACCOUNT_WORK`] pieces of hashing work
//!   that follow an authentication of some kind (a correct password step, a setup's password,
//!   a password change, new recovery codes) per account in any [`ACCOUNT_WORK_MINUTES`]
//!   minutes, so one credential holder can't spend what everyone shares (H1, LA).
//! - **Work per setup token and per setup code** ([`SETUP_PASSWORD_SETS_PER_TOKEN`],
//!   [`SETUP_ENROLMENTS_PER_TOKEN`], [`SETUP_BEGINS_PER_CODE`],
//!   [`SETUP_PASSWORD_SETS_PER_CODE`]), kept in the database with the token and the code.
//! - The hashing itself is capped too (`super::PasswordService`, [`HASH_PERMITS`]).
//!
//! Every limit takes the time as an argument, so the tests drive them with a fake clock.

use std::collections::VecDeque;
use std::net::IpAddr;
use std::sync::Mutex;

use chrono::{DateTime, Duration, Utc};
use sha2::{Digest, Sha256};

/// The failure from which an address waits for an account.
pub const THRESHOLD: i32 = 5;
/// The first wait.
pub const BASE_DELAY_SECS: i64 = 30;
/// The longest wait.
pub const MAX_DELAY_SECS: i64 = 15 * 60;
/// [`MAX_DELAY_SECS`] as a duration.
pub const MAX_DELAY: Duration = Duration::seconds(MAX_DELAY_SECS);
/// Failures older than this are forgotten at the next one.
pub const FORGET_AFTER: Duration = Duration::hours(24);
/// Failures on one account from every address together before an address the person hasn't
/// signed in from is rationed to one attempt per [`MAX_DELAY`] (until 24 hours pass with no
/// failure).
pub const ACCOUNT_CEILING: i32 = 50;
/// How long an address the person fully signed in from stays known.
pub const KNOWN_SOURCE_DAYS: i64 = 30;
/// Password hashes for anyone, in any rolling minute.
pub const GLOBAL_PER_MINUTE: usize = 300;
/// More hashes in the same minute, only for a password step from an address someone fully
/// signed in from in the last [`KNOWN_SOURCE_DAYS`] days.
pub const GLOBAL_RESERVE_PER_MINUTE: usize = 120;
/// Hashes of the open allowance taken in the last minute (80% of [`GLOBAL_PER_MINUTE`]) from
/// which the band shares apply (the review of 8e8f8fe, M4). Below it, a block takes up to
/// [`BLOCK_OPEN_PER_MINUTE`]: a 30-person office behind one address signs in during a quiet
/// minute, typos included.
pub const SHARES_FROM_PER_MINUTE: usize = GLOBAL_PER_MINUTE * 4 / 5;
/// The most one network block ([`block_key`]) takes of the open allowance in any rolling
/// minute, at any load (the review of 8e8f8fe, a Low): a fifth of [`GLOBAL_PER_MINUTE`]. A
/// 30-person office behind one address, each person mistyping once, takes 60 hashes: all of
/// them. Before, a block took hashes freely up to [`SHARES_FROM_PER_MINUTE`], so one IPv6 /48
/// could take 246 a minute (354,240 guesses a day against one account); now 60 (86,400).
pub const BLOCK_OPEN_PER_MINUTE: usize = GLOBAL_PER_MINUTE / 5;
/// The most one network block ([`block_key`]) takes, in any rolling minute, of the open
/// allowance's last fifth (the 60 past [`SHARES_FROM_PER_MINUTE`]): a tenth of it, so filling
/// the last fifth takes at least ten blocks however much one flood took below it (G1, M4), and
/// the whole allowance fourteen (four of [`BLOCK_OPEN_PER_MINUTE`] first).
pub const BLOCK_SHARE_PER_MINUTE: usize = 6;
/// The most of the reserve one address takes in any rolling minute (G1): someone who signed in
/// from many addresses can't fill it from a few.
pub const RESERVE_PER_ADDRESS_PER_MINUTE: usize = 2;
/// The most of the reserve one network block takes in any rolling minute: a fifth, so filling
/// it takes known addresses in at least five blocks (G1; for IPv6, where one /48 holds 65,536
/// /64s).
pub const RESERVE_PER_BLOCK_PER_MINUTE: usize = 24;
/// Wrong setup codes (or unknown sign-in names at the setup's first step) from one network
/// block in any [`SETUP_GUESS_WINDOW_MINUTES`] before that block's setups wait (G2).
pub const SETUP_GUESSES_PER_BLOCK: usize = 100;
/// The window [`SETUP_GUESSES_PER_BLOCK`] is counted in.
pub const SETUP_GUESS_WINDOW_MINUTES: i64 = 15;
/// How often, at most, a block's refusals past its setup guesses are recorded in the audit log
/// (the review of 8e8f8fe, a Low: before, they wrote nothing). One row stands for every refusal
/// since the block's last one.
pub const OVER_LIMIT_ROW_EVERY_MINUTES: i64 = 1;
/// The largest count one such row carries (an audit detail's count has at most six digits).
pub const OVER_LIMIT_ROW_MAX_COUNT: u32 = 999_999;
/// Hashing work that follows an authentication, per account, in any [`ACCOUNT_WORK_MINUTES`].
pub const ACCOUNT_WORK: usize = 10;
/// The window [`ACCOUNT_WORK`] is counted in.
pub const ACCOUNT_WORK_MINUTES: i64 = 15;
/// Passwords set with one setup token; past it the token is spent (start again with the code).
pub const SETUP_PASSWORD_SETS_PER_TOKEN: i32 = 3;
/// New authenticator secrets made with one setup token; past it the token is spent.
pub const SETUP_ENROLMENTS_PER_TOKEN: i32 = 5;
/// Setup tokens one setup code gives (its renewals); past it the code is spent.
pub const SETUP_BEGINS_PER_CODE: i32 = 5;
/// Passwords set with one setup code across its renewals; past it the code is spent. Two
/// tokens' worth: someone who mistypes, abandons a setup and starts again with the same code
/// still finishes; more is not a person setting up, and an admin's new code is a minute's work.
pub const SETUP_PASSWORD_SETS_PER_CODE: i32 = 6;
/// Hashes computed at once; more wait (at most [`HASH_WAIT_SECS`]).
pub const HASH_PERMITS: usize = 2;
/// How long a request waits for a hashing slot before `503 sign_in_busy`.
pub const HASH_WAIT_SECS: u64 = 10;
/// What a signed-in person's own re-authentication (change the password, new recovery codes)
/// is counted under: one bucket per person, apart from every address ([`session_key`]).
pub const SESSION_SOURCE: &str = "session";

/// How long an address waits after its `failures`th failure: nothing below [`THRESHOLD`],
/// then 30 s, 1 min, 2 min, 4 min, 8 min, and 15 min from the 10th on.
pub fn delay(failures: i32) -> Option<Duration> {
    if failures < THRESHOLD {
        return None;
    }
    let doublings = (failures - THRESHOLD).min(10) as u32;
    let secs = BASE_DELAY_SECS.saturating_mul(1i64 << doublings);
    Some(Duration::seconds(secs.min(MAX_DELAY_SECS)))
}

/// Failures of one kind: for one account from one address (with a wait), or for one account
/// from every address (a count only, [`Counter::count_only`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counter {
    pub count: i32,
    pub last_at: Option<DateTime<Utc>>,
    pub locked_until: Option<DateTime<Utc>>,
}

impl Counter {
    /// Is the address waiting at `now`? Until when.
    pub fn locked(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        self.locked_until.filter(|until| *until > now)
    }

    fn bump(&mut self, now: DateTime<Utc>) {
        let forgotten = self.last_at.is_some_and(|at| now - at > FORGET_AFTER);
        self.count = if forgotten {
            1
        } else {
            self.count.saturating_add(1)
        };
        self.last_at = Some(now);
    }

    /// Count one failure at `now`. True when this failure started (or extended) a wait.
    pub fn fail(&mut self, now: DateTime<Utc>) -> bool {
        self.bump(now);
        self.locked_until = delay(self.count).map(|d| now + d);
        self.locked_until.is_some()
    }

    /// Count one failure at `now` with no wait (the account-wide count).
    pub fn count_only(&mut self, now: DateTime<Utc>) {
        self.bump(now);
        self.locked_until = None;
    }

    /// The count at `now`: nothing once 24 hours passed since the last failure.
    pub fn live_count(&self, now: DateTime<Utc>) -> i32 {
        match self.last_at {
            Some(at) if now - at <= FORGET_AFTER => self.count,
            _ => 0,
        }
    }

    /// Take back one failure counted ahead of a check that then passed (the password step
    /// counts a failure before it hashes, so parallel guesses can't slip past the threshold).
    /// The failure was counted while the address wasn't waiting, so the wait it started ends.
    pub fn take_back(&mut self) {
        self.count = (self.count - 1).max(0);
        self.locked_until = None;
    }

    /// Give back one failure counted ahead of a check that never ran (every hashing slot was
    /// busy: G3). Unlike [`Self::take_back`] a wait stays as it is: an attempt that wasn't
    /// checked must never shorten one.
    pub fn give_back(&mut self) {
        self.count = (self.count - 1).max(0);
    }

    /// Back to zero (a full sign-in from that address; a finished setup; a password change).
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

/// One person's counters for one address ([`source_key`]), and when they last fully signed in
/// from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SourceCounters {
    /// Failed passwords and second-step codes (and re-authentication, for [`SESSION_SOURCE`]).
    pub sign_in: Counter,
    /// Failed setup-code guesses and setup confirmations.
    pub setup: Counter,
    /// The last full sign-in (or finished setup) from this address.
    pub last_success_at: Option<DateTime<Utc>>,
}

impl SourceCounters {
    /// Did the person fully sign in from here in the last [`KNOWN_SOURCE_DAYS`] days?
    pub fn known(&self, now: DateTime<Utc>) -> bool {
        self.last_success_at
            .is_some_and(|at| now - at <= Duration::days(KNOWN_SOURCE_DAYS))
    }

    /// Nothing worth keeping: no live failure, no wait, and not known.
    pub fn is_idle(&self, now: DateTime<Utc>) -> bool {
        self.sign_in.live_count(now) == 0
            && self.setup.live_count(now) == 0
            && self.sign_in.locked(now).is_none()
            && self.setup.locked(now).is_none()
            && !self.known(now)
    }
}

/// Has the account's count from every address reached [`ACCOUNT_CEILING`]? Until when (the
/// count is forgotten 24 hours after its last failure).
pub fn over_ceiling(account: &Counter, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    (account.live_count(now) >= ACCOUNT_CEILING)
        .then(|| account.last_at.map(|at| at + FORGET_AFTER))
        .flatten()
}

/// May `source` try a password or a code for this account now? `Err(until)` while that
/// address waits. Never refused for the account's ceiling alone (M2): past it, an address the
/// person hasn't signed in from is rationed by its own wait ([`fail_sign_in`]).
pub fn may_try(source: &SourceCounters, now: DateTime<Utc>) -> Result<(), DateTime<Utc>> {
    match source.sign_in.locked(now) {
        Some(until) => Err(until),
        None => Ok(()),
    }
}

/// What a counted sign-in failure did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Failed {
    /// This failure started (or extended) the address's wait.
    pub started_wait: bool,
    /// This failure took the account to its ceiling.
    pub reached_ceiling: bool,
}

/// Count one failed password or second-step code at `now`: on the account (a count only) and
/// on the address. Past the account's ceiling, an address the person hasn't signed in from
/// waits [`MAX_DELAY`] after each failure: one attempt per 15 minutes per address (M2).
pub fn fail_sign_in(
    account: &mut Counter,
    source: &mut SourceCounters,
    now: DateTime<Utc>,
) -> Failed {
    let before = account.live_count(now);
    account.count_only(now);
    let mut started_wait = source.sign_in.fail(now);
    if over_ceiling(account, now).is_some() && !source.known(now) {
        source.sign_in.locked_until = Some(now + MAX_DELAY);
        started_wait = true;
    }
    Failed {
        started_wait,
        reached_ceiling: before < ACCOUNT_CEILING && account.live_count(now) >= ACCOUNT_CEILING,
    }
}

/// [`may_try`] for a setup code: that address's own wait only. There is no account-wide
/// ceiling for setup codes (M2), and a wrong guess never touches the person's code (G2):
/// guesses count against the address (here) and its block ([`BlockGuesses`]) only.
pub fn may_try_setup(source: &SourceCounters, now: DateTime<Utc>) -> Result<(), DateTime<Utc>> {
    match source.setup.locked(now) {
        Some(until) => Err(until),
        None => Ok(()),
    }
}

fn keyed(text: &str) -> String {
    let mut h = Sha256::new();
    h.update(b"pmos-password-source\0");
    h.update(text.as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// The address an attempt comes from, as stored: the SHA-256 of pmos-web's address text,
/// normalized (an IPv4 address; an IPv6 address as its /64; an IPv4-mapped IPv6 address as
/// the IPv4 one). Anything else, or nothing, is the one key `unknown` shares. The address
/// itself is never stored or logged (a hash of an IPv4 address can be reversed by trying
/// them all: it is pseudonymous, not secret).
pub fn source_key(source: Option<&str>) -> String {
    let normal = source.and_then(normalize_source);
    keyed(normal.as_deref().unwrap_or("unknown"))
}

/// Is `source` an address (not the shared key `unknown`)? Only an address can become a
/// person's known address: attempts with no address all share one key, and that key is never
/// anyone's (M1).
pub fn is_address(source: Option<&str>) -> bool {
    source.and_then(normalize_source).is_some()
}

/// The key a signed-in person's own re-authentication is counted under.
pub fn session_key() -> String {
    keyed(SESSION_SOURCE)
}

/// An address as pmos-web counts it: IPv4 whole, IPv6 by its /64 (`2001:db8:1:2::/64`), an
/// IPv4-mapped IPv6 address as the IPv4 one.
pub fn normalize_ip(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => v4.to_string(),
            None => {
                let s = v6.segments();
                let net = std::net::Ipv6Addr::new(s[0], s[1], s[2], s[3], 0, 0, 0, 0);
                format!("{net}/64")
            }
        },
    }
}

fn normalize_source(text: &str) -> Option<String> {
    let t = text.trim();
    if let Ok(ip) = t.parse::<IpAddr>() {
        return Some(normalize_ip(ip));
    }
    let ip = t.strip_suffix("/64")?.parse::<std::net::Ipv6Addr>().ok()?;
    Some(normalize_ip(IpAddr::V6(ip)))
}

/// The network block `source` is in, for the shares of the global limit and the setup-guess
/// count (G1, G2): an IPv4 address's /24 (`v4 203.0.113.0/24`), an IPv6 address's /48
/// (`v6 2001:db8:1::/48`). Anything else, or nothing, is the one block `unknown`. In memory
/// only: never stored or logged.
pub fn block_key(source: Option<&str>) -> String {
    let Some(normal) = source.and_then(normalize_source) else {
        return "unknown".to_string();
    };
    let ip = normal
        .strip_suffix("/64")
        .unwrap_or(&normal)
        .parse::<IpAddr>()
        .ok();
    match ip {
        Some(IpAddr::V4(v4)) => {
            let o = v4.octets();
            format!("v4 {}.{}.{}.0/24", o[0], o[1], o[2])
        }
        Some(IpAddr::V6(v6)) => {
            let s = v6.segments();
            let net = std::net::Ipv6Addr::new(s[0], s[1], s[2], 0, 0, 0, 0, 0);
            format!("v6 {net}/48")
        }
        None => "unknown".to_string(),
    }
}

/// Seconds until `until`, at least 1: a `Retry-After`.
pub fn retry_after(until: DateTime<Utc>, now: DateTime<Utc>) -> u64 {
    u64::try_from((until - now).num_seconds())
        .unwrap_or(0)
        .max(1)
}

/// A rolling minute of timestamps.
#[derive(Debug, Default)]
struct Window(VecDeque<DateTime<Utc>>);

impl Window {
    fn trim(&mut self, now: DateTime<Utc>) {
        let window = Duration::minutes(1);
        while self
            .0
            .front()
            .is_some_and(|t| now - *t >= window || *t > now)
        {
            self.0.pop_front();
        }
    }

    fn retry(&self, now: DateTime<Utc>) -> u64 {
        let oldest = self.0.front().copied().unwrap_or(now);
        retry_after(oldest + Duration::minutes(1), now)
    }
}

/// Rolling minutes by key (a block or an address), forgetting keys with nothing left in theirs.
#[derive(Debug, Default)]
struct Windows(std::collections::HashMap<String, Window>);

/// Keys [`Windows`] holds before it sweeps the empty ones (a live key holds at least one hash
/// of the last minute, so at most the global limit's total are ever live).
const SWEEP_AT: usize = 1_024;

impl Windows {
    /// How many in `key`'s minute at `now`.
    fn count(&mut self, key: &str, now: DateTime<Utc>) -> usize {
        match self.0.get_mut(key) {
            Some(w) => {
                w.trim(now);
                w.0.len()
            }
            None => 0,
        }
    }

    fn retry(&self, key: &str, now: DateTime<Utc>) -> u64 {
        self.0.get(key).map_or(1, |w| w.retry(now))
    }

    fn push(&mut self, key: &str, now: DateTime<Utc>) {
        if self.0.len() >= SWEEP_AT && !self.0.contains_key(key) {
            self.0.retain(|_, w| {
                w.trim(now);
                !w.0.is_empty()
            });
        }
        self.0.entry(key.to_string()).or_default().0.push_back(now);
    }
}

/// What a hash is drawn for: the block it is counted under ([`block_key`] for a password step;
/// an account's own key, [`account_block`], for work after an authentication), and, for a
/// password step from a known address, that address ([`source_key`]), which may then use the
/// reserve.
#[derive(Debug, Clone, Copy)]
pub struct Draw<'a> {
    pub block: &'a str,
    pub reserve_for: Option<&'a str>,
}

impl<'a> Draw<'a> {
    /// The open allowance only.
    pub fn open(block: &'a str) -> Self {
        Self {
            block,
            reserve_for: None,
        }
    }
}

/// The key work after an authentication (setup, a change) is counted under in the global
/// limit's shares: the account's own, so one credential holder's work is never another's
/// block (it is bounded per account anyway, [`AccountWork`]).
pub fn account_block(handle: &str) -> String {
    format!("account {handle}")
}

#[derive(Debug, Default)]
struct Seen {
    open: Window,
    reserve: Window,
    /// Per block, the hashes it took while the open allowance was past
    /// [`GlobalLimit::shares_from`].
    band_by_block: Windows,
    /// Per block, every hash it took from the open allowance.
    open_by_block: Windows,
    reserve_by_block: Windows,
    reserve_by_address: Windows,
}

/// The rolling-minute limit on password hashes across the tool server: an open allowance, of
/// whose last fifth each block takes at most its share (below it, anyone takes one: M4), and a
/// reserve for password steps from known addresses, of which each address and each block takes
/// at most its share (G1).
#[derive(Debug)]
pub struct GlobalLimit {
    per_minute: usize,
    reserve: usize,
    /// The open hashes in the minute from which the block shares apply.
    shares_from: usize,
    /// Each block's share of the open allowance past `shares_from`.
    block_share: usize,
    /// Each block's share of the open allowance at any load.
    block_open: usize,
    reserve_per_address: usize,
    reserve_per_block: usize,
    seen: Mutex<Seen>,
}

impl Default for GlobalLimit {
    fn default() -> Self {
        Self::new(GLOBAL_PER_MINUTE, GLOBAL_RESERVE_PER_MINUTE)
    }
}

impl GlobalLimit {
    /// `per_minute` open and `reserve` reserved, with the shares at their values (never more
    /// than the whole).
    pub fn new(per_minute: usize, reserve: usize) -> Self {
        let per_minute = per_minute.max(1);
        let shares_from = per_minute * 4 / 5;
        Self {
            per_minute,
            reserve,
            shares_from,
            // A tenth of the last fifth; a last fifth too small to share out in ten (a
            // test's few hashes a minute) has no shares.
            block_share: match per_minute - shares_from {
                band if band >= 10 => BLOCK_SHARE_PER_MINUTE.min(band / 10),
                _ => per_minute,
            },
            // A fifth of the whole; an allowance too small to share out in five (a test's few
            // hashes a minute) has no such share.
            block_open: if per_minute >= 50 {
                BLOCK_OPEN_PER_MINUTE.min(per_minute / 5)
            } else {
                per_minute
            },
            reserve_per_address: RESERVE_PER_ADDRESS_PER_MINUTE.min(reserve),
            reserve_per_block: RESERVE_PER_BLOCK_PER_MINUTE.min(reserve),
            seen: Mutex::new(Seen::default()),
        }
    }

    /// With other shares (tests).
    pub fn with_shares(
        mut self,
        block: usize,
        reserve_per_address: usize,
        reserve_per_block: usize,
    ) -> Self {
        self.block_share = block.max(1);
        self.reserve_per_address = reserve_per_address;
        self.reserve_per_block = reserve_per_block;
        self
    }

    /// Take one hash at `now`: from the open allowance while it and the block's share of it
    /// have room and, once `shares_from` of it were taken in the minute, while the block's
    /// share of the rest has room too; otherwise, for a draw with `reserve_for`, from the reserve while it, the
    /// block's share of it and the address's have room. `Err(retry_after_secs)` when neither.
    pub fn take(&self, now: DateTime<Utc>, draw: Draw<'_>) -> Result<(), u64> {
        let mut seen = self.seen.lock().unwrap_or_else(|p| p.into_inner());
        let s = &mut *seen;
        s.open.trim(now);
        s.reserve.trim(now);
        let open_full = s.open.0.len() >= self.per_minute;
        // M4: the shares apply only once the allowance is nearly full.
        let in_band = s.open.0.len() >= self.shares_from;
        let block_open_full = s.open_by_block.count(draw.block, now) >= self.block_open;
        let band_full = in_band && s.band_by_block.count(draw.block, now) >= self.block_share;
        let block_full = block_open_full || band_full;
        if !open_full && !block_full {
            s.open.0.push_back(now);
            s.open_by_block.push(draw.block, now);
            if in_band {
                s.band_by_block.push(draw.block, now);
            }
            return Ok(());
        }
        let open_retry = if open_full {
            s.open.retry(now)
        } else if block_open_full {
            s.open_by_block.retry(draw.block, now)
        } else {
            s.band_by_block.retry(draw.block, now)
        };
        let Some(address) = draw.reserve_for.filter(|_| self.reserve > 0) else {
            return Err(open_retry);
        };
        let reserve_full = s.reserve.0.len() >= self.reserve;
        let rblock_full = s.reserve_by_block.count(draw.block, now) >= self.reserve_per_block;
        let address_full = s.reserve_by_address.count(address, now) >= self.reserve_per_address;
        if !reserve_full && !rblock_full && !address_full {
            s.reserve.0.push_back(now);
            s.reserve_by_block.push(draw.block, now);
            s.reserve_by_address.push(address, now);
            return Ok(());
        }
        let reserve_retry = if reserve_full {
            s.reserve.retry(now)
        } else if rblock_full {
            s.reserve_by_block.retry(draw.block, now)
        } else {
            s.reserve_by_address.retry(address, now)
        };
        Err(open_retry.min(reserve_retry))
    }
}

/// Wrong setup-code guesses per network block in a rolling [`SETUP_GUESS_WINDOW_MINUTES`] (in
/// this process): past [`SETUP_GUESSES_PER_BLOCK`], every refusal of the setup's first step
/// from that block is `429`, whatever the name (G2), so the answer still says nothing about
/// it. Each attempt takes its slot before it is checked ([`Self::take`]) and gives it back when
/// its code was right ([`Self::give_back`]), so guesses in parallel can't pass the limit
/// together.
///
/// Past its guesses, a block's first step is still checked (the review of 8e8f8fe): a right,
/// live code for that name begins the setup, so a flood of wrong guesses from an invitee's
/// network never shuts the invitee out; everything else answers `429`, writes nothing and is
/// counted against the address at `pmos-web` ([`Guess`]).
#[derive(Debug, Default)]
pub struct BlockGuesses {
    seen: Mutex<Timed>,
    /// Per block past its guesses: when its last audit row was written, and the refusals since
    /// ([`Self::note_over_limit`]).
    over: Mutex<std::collections::HashMap<String, (DateTime<Utc>, u32)>>,
}

/// Rolling windows of times by key, swept of idle keys on time (each
/// [`SWEEP_EVERY_MINUTES`]) and when they reach their most (the review of 8e8f8fe: before, only
/// at 10,000 keys, so idle ones stayed for as long as fewer were seen).
#[derive(Debug, Default)]
struct Timed {
    by_key: std::collections::HashMap<String, VecDeque<DateTime<Utc>>>,
    swept_at: Option<DateTime<Utc>>,
}

/// How often [`Timed`] forgets idle keys, at the latest.
pub const SWEEP_EVERY_MINUTES: i64 = 5;

impl Timed {
    /// Forget keys with nothing in the last `window`, when `max` is reached by a new `key` or
    /// [`SWEEP_EVERY_MINUTES`] passed since the last sweep.
    fn sweep(&mut self, key: &str, now: DateTime<Utc>, window: Duration, max: usize) {
        let full = self.by_key.len() >= max && !self.by_key.contains_key(key);
        let due = self
            .swept_at
            .is_none_or(|at| now - at >= Duration::minutes(SWEEP_EVERY_MINUTES) || at > now);
        if full || due {
            self.by_key
                .retain(|_, t| t.back().is_some_and(|at| now - *at < window && *at <= now));
            self.swept_at = Some(now);
        }
    }

    fn len(&self) -> usize {
        self.by_key.len()
    }
}

/// Blocks [`BlockGuesses`] tracks before it forgets the idle ones.
const MAX_GUESS_BLOCKS: usize = 10_000;

/// What [`BlockGuesses::take`] decided for an attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Guess {
    /// Within the block's guesses: a slot was taken (give it back unless the guess was wrong).
    Counted,
    /// The block used its guesses: only a right code passes; anything else is `429` with
    /// this `Retry-After`. Nothing was taken.
    OverLimit(u64),
}

impl BlockGuesses {
    fn window() -> Duration {
        Duration::minutes(SETUP_GUESS_WINDOW_MINUTES)
    }

    /// Check `block` and take a slot at `now`, in one step; [`Guess::OverLimit`] when the
    /// block has used its guesses.
    pub fn take(&self, block: &str, now: DateTime<Utc>) -> Guess {
        let mut seen = self.seen.lock().unwrap_or_else(|p| p.into_inner());
        seen.sweep(block, now, Self::window(), MAX_GUESS_BLOCKS);
        let times = seen.by_key.entry(block.to_string()).or_default();
        while times
            .front()
            .is_some_and(|t| now - *t >= Self::window() || *t > now)
        {
            times.pop_front();
        }
        if times.len() >= SETUP_GUESSES_PER_BLOCK {
            let oldest = times.front().copied().unwrap_or(now);
            return Guess::OverLimit(retry_after(oldest + Self::window(), now));
        }
        times.push_back(now);
        Guess::Counted
    }

    /// Give back the slot [`Self::take`] took at `at`: the code was right, or PM OS failed.
    pub fn give_back(&self, block: &str, at: DateTime<Utc>) {
        let mut seen = self.seen.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(times) = seen.by_key.get_mut(block) {
            if let Some(i) = times.iter().rposition(|t| *t == at) {
                times.remove(i);
            }
            if times.is_empty() {
                seen.by_key.remove(block);
            }
        }
    }

    /// Blocks tracked now (tests: the sweep).
    pub fn tracked(&self) -> usize {
        self.seen.lock().unwrap_or_else(|p| p.into_inner()).len()
    }

    /// Note one refusal past `block`'s guesses at `now`. `Some(n)` when an audit row is due,
    /// standing for `n` refusals (this one and those since the block's last row): the first
    /// refusal of a block, then the first one at least [`OVER_LIMIT_ROW_EVERY_MINUTES`] after
    /// its last row. So a flood writes at most one row a minute per block, fewer than the
    /// hundred wrong guesses (each a row) it took to get there. Refusals after a block's last
    /// row are in its next row, if one comes; the tool server's `/readyz` counts every one.
    pub fn note_over_limit(&self, block: &str, now: DateTime<Utc>) -> Option<u32> {
        let every = Duration::minutes(OVER_LIMIT_ROW_EVERY_MINUTES);
        let mut over = self.over.lock().unwrap_or_else(|p| p.into_inner());
        if over.len() >= MAX_GUESS_BLOCKS && !over.contains_key(block) {
            // Forget blocks whose last row is a window old (their guesses are forgotten too).
            over.retain(|_, (at, _)| now - *at < Self::window() && *at <= now);
            if over.len() >= MAX_GUESS_BLOCKS {
                // Every one is recent (each took a hundred recorded wrong guesses to get
                // here): this refusal is counted on `/readyz` only.
                return None;
            }
        }
        match over.get_mut(block) {
            Some((at, since)) if now - *at < every && *at <= now => {
                *since = since.saturating_add(1);
                None
            }
            Some((at, since)) => {
                let n = since.saturating_add(1).min(OVER_LIMIT_ROW_MAX_COUNT);
                *at = now;
                *since = 0;
                Some(n)
            }
            None => {
                over.insert(block.to_string(), (now, 0));
                Some(1)
            }
        }
    }
}

/// Hashing work that follows an authentication, per account, in a rolling
/// [`ACCOUNT_WORK_MINUTES`] (in this process, as the global limit is): a correct password
/// step, a setup's password, a password change, new recovery codes. Past [`ACCOUNT_WORK`], more
/// of that work for the account is refused (`429`) until the window moves on. Only someone who
/// already holds one of the account's credentials (its password, a live setup token, a
/// session) can fill it, and filling it shuts out only that account.
#[derive(Debug, Default)]
pub struct AccountWork {
    seen: Mutex<Timed>,
}

/// Accounts [`AccountWork`] tracks before it forgets the idle ones.
const MAX_WORK_ACCOUNTS: usize = 10_000;

impl AccountWork {
    fn window() -> Duration {
        Duration::minutes(ACCOUNT_WORK_MINUTES)
    }

    fn trim(times: &mut VecDeque<DateTime<Utc>>, now: DateTime<Utc>) {
        while times
            .front()
            .is_some_and(|t| now - *t >= Self::window() || *t > now)
        {
            times.pop_front();
        }
    }

    fn full(times: &VecDeque<DateTime<Utc>>, now: DateTime<Utc>) -> Result<(), u64> {
        if times.len() >= ACCOUNT_WORK {
            let oldest = times.front().copied().unwrap_or(now);
            return Err(retry_after(oldest + Self::window(), now));
        }
        Ok(())
    }

    /// Has `handle` room for one more piece of work at `now`? `Err(retry_after_secs)` if not.
    pub fn check(&self, handle: &str, now: DateTime<Utc>) -> Result<(), u64> {
        let mut seen = self.seen.lock().unwrap_or_else(|p| p.into_inner());
        match seen.by_key.get_mut(handle) {
            Some(times) => {
                Self::trim(times, now);
                Self::full(times, now)
            }
            None => Ok(()),
        }
    }

    /// Check and count one piece of work for `handle` at `now`, in one step.
    pub fn take(&self, handle: &str, now: DateTime<Utc>) -> Result<(), u64> {
        let mut seen = self.seen.lock().unwrap_or_else(|p| p.into_inner());
        seen.sweep(handle, now, Self::window(), MAX_WORK_ACCOUNTS);
        let times = seen.by_key.entry(handle.to_string()).or_default();
        Self::trim(times, now);
        Self::full(times, now)?;
        times.push_back(now);
        Ok(())
    }

    /// Give back the piece [`Self::take`] took at `at`: the work wasn't done (the shared
    /// allowance refused it, or every hashing slot was busy: G3).
    pub fn give_back(&self, handle: &str, at: DateTime<Utc>) {
        let mut seen = self.seen.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(times) = seen.by_key.get_mut(handle) {
            if let Some(i) = times.iter().rposition(|t| *t == at) {
                times.remove(i);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-11-20T09:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
            + Duration::seconds(secs)
    }

    #[test]
    fn the_delay_grows_from_the_fifth_failure_and_is_capped() {
        let want = [
            (1, None),
            (4, None),
            (5, Some(30)),
            (6, Some(60)),
            (7, Some(120)),
            (8, Some(240)),
            (9, Some(480)),
            (10, Some(900)),
            (11, Some(900)),
            (1000, Some(900)),
            (i32::MAX, Some(900)),
        ];
        for (n, secs) in want {
            assert_eq!(delay(n), secs.map(Duration::seconds), "{n}");
        }
    }

    #[test]
    fn a_counter_waits_takes_back_and_forgets() {
        let mut c = Counter::default();
        for i in 0..4 {
            assert!(!c.fail(at(i)));
            assert_eq!(c.locked(at(i)), None);
        }
        assert!(c.fail(at(10)));
        assert_eq!(c.locked(at(10)), Some(at(40)));
        assert_eq!(c.locked(at(40)), None, "the wait ends on its own");
        // A correct password counted ahead takes its failure back, and the wait it started.
        c.take_back();
        assert_eq!((c.count, c.locked(at(11))), (4, None));
        // Old failures are forgotten.
        c.fail(at(10));
        c.fail(at(10) + FORGET_AFTER + Duration::seconds(1));
        assert_eq!(c.count, 1);
        c.clear();
        assert_eq!(c, Counter::default());
        assert_eq!(retry_after(at(40), at(10)), 30);
        assert_eq!(retry_after(at(10), at(40)), 1);
        // The account-wide count never waits.
        let mut a = Counter::default();
        for i in 0..20 {
            a.count_only(at(i));
        }
        assert_eq!((a.live_count(at(20)), a.locked(at(20))), (20, None));
        assert_eq!(
            a.live_count(at(19) + FORGET_AFTER + Duration::seconds(1)),
            0
        );
    }

    #[test]
    fn the_global_limit_is_a_rolling_minute_with_a_reserve() {
        let g = GlobalLimit::new(3, 2);
        let open = Draw::open("v4 192.0.2.0/24");
        let known = |a: &'static str| Draw {
            block: "v4 198.51.100.0/24",
            reserve_for: Some(a),
        };
        assert!(g.take(at(0), open).is_ok());
        assert!(g.take(at(10), open).is_ok());
        assert!(g.take(at(20), open).is_ok());
        assert_eq!(g.take(at(30), open), Err(30));
        // Junk filled the open allowance: an attempt from a known address still gets a hash.
        assert!(g.take(at(30), known("a")).is_ok());
        assert!(g.take(at(31), known("b")).is_ok());
        assert!(
            g.take(at(32), known("c")).is_err(),
            "the reserve is bounded too"
        );
        assert!(g.take(at(60), open).is_ok(), "the first left the window");
        assert!(g.take(at(61), open).is_err());
        let d = GlobalLimit::default();
        assert_eq!(d.block_open, BLOCK_OPEN_PER_MINUTE);
        assert_eq!(
            GlobalLimit::new(3, 2).block_open,
            3,
            "no fifth of a test's few"
        );
        assert_eq!(
            (
                d.per_minute,
                d.reserve,
                d.block_share,
                d.reserve_per_address,
                d.reserve_per_block
            ),
            (
                GLOBAL_PER_MINUTE,
                GLOBAL_RESERVE_PER_MINUTE,
                BLOCK_SHARE_PER_MINUTE,
                RESERVE_PER_ADDRESS_PER_MINUTE,
                RESERVE_PER_BLOCK_PER_MINUTE
            )
        );
    }

    #[test]
    fn an_address_waits_alone_and_past_the_ceiling_new_addresses_are_rationed() {
        let mut account = Counter::default();
        let mut theirs = SourceCounters::default();
        let mut attacker = SourceCounters::default();
        for i in 0..10 {
            fail_sign_in(&mut account, &mut attacker, at(i));
        }
        assert!(may_try(&attacker, at(10)).is_err());
        assert!(may_try(&theirs, at(10)).is_ok(), "another address");
        let mut reached = false;
        for i in 10..ACCOUNT_CEILING as i64 {
            let mut other = SourceCounters::default();
            reached |= fail_sign_in(&mut account, &mut other, at(i)).reached_ceiling;
        }
        assert!(reached, "the ceiling is reported once it is reached");
        // M2: past the ceiling a new address may still try: never refused for the ceiling.
        assert!(over_ceiling(&account, at(60)).is_some());
        assert!(may_try(&theirs, at(60)).is_ok(), "a new address may try");
        // But each failure from it rations it to one attempt per 15 minutes.
        let f = fail_sign_in(&mut account, &mut theirs, at(60));
        assert!(f.started_wait && !f.reached_ceiling);
        assert_eq!(may_try(&theirs, at(61)), Err(at(60) + MAX_DELAY));
        assert!(may_try(&theirs, at(60) + MAX_DELAY).is_ok());
        // A known address keeps its own schedule (no wait until its 5th failure).
        let mut known = SourceCounters {
            last_success_at: Some(at(0) - Duration::days(KNOWN_SOURCE_DAYS - 1)),
            ..Default::default()
        };
        assert!(!fail_sign_in(&mut account, &mut known, at(70)).started_wait);
        assert!(may_try(&known, at(71)).is_ok(), "a known address");
        assert!(may_try_setup(&theirs, at(60)).is_ok(), "no setup ceiling");
        let mut fresh = SourceCounters::default();
        assert!(
            !fail_sign_in(
                &mut account,
                &mut fresh,
                at(71) + FORGET_AFTER + Duration::seconds(1)
            )
            .started_wait,
            "the ceiling is forgotten after a quiet day"
        );
        let mut idle = SourceCounters::default();
        assert!(idle.is_idle(at(0)));
        idle.sign_in.fail(at(0));
        assert!(!idle.is_idle(at(1)));
        assert!(idle.is_idle(at(0) + FORGET_AFTER + Duration::seconds(1)));
    }

    #[test]
    fn sources_are_keyed_by_address_and_ipv6_by_its_64() {
        let k = |s: &str| source_key(Some(s));
        assert_eq!(k("203.0.113.7"), k(" 203.0.113.7 "));
        assert_ne!(k("203.0.113.7"), k("203.0.113.8"));
        assert_eq!(k("2001:db8:1:2::1"), k("2001:db8:1:2:ffff::9"));
        assert_eq!(k("2001:db8:1:2::1"), k("2001:db8:1:2::/64"));
        assert_ne!(k("2001:db8:1:2::1"), k("2001:db8:1:3::1"));
        assert_eq!(k("::ffff:203.0.113.7"), k("203.0.113.7"));
        assert_eq!(k("not an address"), source_key(None));
        assert_ne!(session_key(), source_key(None));
        assert_eq!(k("1.2.3.4").len(), 64);
        assert!(is_address(Some("203.0.113.7")) && is_address(Some("2001:db8:1:2::/64")));
        assert!(!is_address(None) && !is_address(Some("not an address")));
    }

    #[test]
    fn work_per_account_is_bounded_in_a_rolling_window() {
        let w = AccountWork::default();
        for i in 0..ACCOUNT_WORK as i64 {
            assert!(w.take("ana", at(i)).is_ok());
        }
        assert_eq!(w.take("ana", at(30)), Err(15 * 60 - 30));
        assert!(w.check("ana", at(30)).is_err());
        assert!(w.take("ben", at(30)).is_ok(), "another account has its own");
        assert!(
            w.take("ana", at(15 * 60)).is_ok(),
            "the first left the window"
        );
        assert!(w.check("cy", at(0)).is_ok());
        // Work that wasn't done goes back.
        for i in 0..ACCOUNT_WORK as i64 {
            w.take("dee", at(i)).unwrap();
        }
        assert!(w.take("dee", at(20)).is_err());
        w.give_back("dee", at(3));
        assert!(w.take("dee", at(20)).is_ok());
    }

    #[test]
    fn blocks_are_ipv4_24s_and_ipv6_48s() {
        let b = |s: &str| block_key(Some(s));
        assert_eq!(b("203.0.113.7"), "v4 203.0.113.0/24");
        assert_eq!(b("203.0.113.7"), b("203.0.113.250"));
        assert_ne!(b("203.0.113.7"), b("203.0.114.7"));
        assert_eq!(b("::ffff:203.0.113.7"), b("203.0.113.9"));
        assert_eq!(b("2001:db8:1:2::1"), "v6 2001:db8:1::/48");
        assert_eq!(b("2001:db8:1:2::/64"), b("2001:db8:1:ffff::9"));
        assert_ne!(b("2001:db8:1::1"), b("2001:db8:2::1"));
        assert_eq!(block_key(None), "unknown");
        assert_eq!(b("not an address"), "unknown");
        assert_ne!(account_block("ana"), b("203.0.113.7"));
    }

    /// Fill the open allowance at `now` from blocks nobody else uses, each up to its shares.
    fn fill_open(g: &GlobalLimit, now: DateTime<Utc>) {
        for net in 0..=255 {
            let block = block_key(Some(&format!("10.99.{net}.1")));
            while g.take(now, Draw::open(&block)).is_ok() {}
            if g.seen.lock().unwrap().open.0.len() >= GLOBAL_PER_MINUTE {
                return;
            }
        }
        panic!("the open allowance didn't fill");
    }

    // G1, as changed by M4 and a Low of the review of 8e8f8fe: a flood from many addresses in
    // one block takes at most a fifth of the open allowance; another block still gets hashes.
    // Before M4 this asserted the flood got 30 (a share from the first hash, which also held
    // up a busy office); after M4 240 + 6 (one /48 could take 246 a minute); it now asserts 60.
    #[test]
    fn one_block_takes_only_its_share_of_the_open_allowance() {
        let g = GlobalLimit::default();
        let flood = block_key(Some("203.0.113.1"));
        let mut granted = 0;
        for host in 1..=250 {
            let addr = format!("203.0.113.{host}");
            let block = block_key(Some(&addr));
            assert_eq!(block, flood);
            if g.take(at(0), Draw::open(&block)).is_ok() {
                granted += 1;
            }
        }
        assert_eq!(granted, BLOCK_OPEN_PER_MINUTE);
        assert_eq!(BLOCK_OPEN_PER_MINUTE, 60);
        let elsewhere = block_key(Some("198.51.100.66"));
        assert!(g.take(at(1), Draw::open(&elsewhere)).is_ok());
        // Filling it takes fourteen blocks: four of 60 to 80%, then ten of 6.
        let mut blocks = 1;
        let mut taken = BLOCK_OPEN_PER_MINUTE + 1;
        for net in 0..20 {
            let block = block_key(Some(&format!("192.0.{net}.1")));
            let mut got = 0;
            while g.take(at(2), Draw::open(&block)).is_ok() {
                got += 1;
            }
            taken += got;
            if got > 0 {
                blocks += 1;
            }
            if taken >= GLOBAL_PER_MINUTE {
                break;
            }
        }
        assert_eq!(g.seen.lock().unwrap().open.0.len(), GLOBAL_PER_MINUTE);
        // The flood's block and `elsewhere` (one hash) count among them.
        assert!(blocks >= 14, "{blocks}");
        let fresh = block_key(Some("100.64.0.1"));
        assert!(g.take(at(3), Draw::open(&fresh)).is_err());
        let addr = source_key(Some("100.64.0.1"));
        assert!(g
            .take(
                at(3),
                Draw {
                    block: &fresh,
                    reserve_for: Some(&addr),
                }
            )
            .is_ok());
        assert!(g.take(at(61), Draw::open(&flood)).is_ok(), "a minute on");
    }

    // M4 (the review of 8e8f8fe): in a quiet minute one block, even one address, takes what it
    // needs up to its fifth: a 30-person office behind one address, each mistyping once, takes
    // 60 hashes and none is refused (a Low of the same review capped a block at that fifth at
    // any load; before, it asserted the office could go on to 80%). Once the allowance is
    // nearly full, G1 holds: each block then takes at most 6 more, so filling the rest takes
    // ten blocks, and a block nobody used still gets a hash while two other floods run.
    #[test]
    fn a_quiet_minute_has_no_shares_and_a_nearly_full_one_does() {
        let g = GlobalLimit::default();
        let office = block_key(Some("203.0.113.7"));
        for i in 0..60 {
            assert!(g.take(at(i % 60), Draw::open(&office)).is_ok(), "{i}");
        }
        assert!(
            g.take(at(59), Draw::open(&office)).is_err(),
            "a block's fifth, even in a quiet minute"
        );
        // Junk from three other blocks brings it to 80%.
        for net in 0..3 {
            let b = block_key(Some(&format!("192.0.{net}.1")));
            for _ in 0..BLOCK_OPEN_PER_MINUTE {
                assert!(g.take(at(59), Draw::open(&b)).is_ok());
            }
        }
        assert_eq!(g.seen.lock().unwrap().open.0.len(), SHARES_FROM_PER_MINUTE);
        // Past it, a block that took little before gets its share of the rest and no more.
        let flood = block_key(Some("192.0.20.1"));
        let granted = (0..50)
            .filter(|_| g.take(at(59), Draw::open(&flood)).is_ok())
            .count();
        assert_eq!(granted, BLOCK_SHARE_PER_MINUTE);
        let granted = (0..50)
            .filter(|_| g.take(at(59), Draw::open(&office)).is_ok())
            .count();
        assert_eq!(granted, 0, "the office already took its fifth");
        // Two more floods take theirs; a fourth block still gets in.
        for net in [10, 11] {
            let b = block_key(Some(&format!("192.0.{net}.1")));
            for _ in 0..50 {
                g.take(at(59), Draw::open(&b)).ok();
            }
        }
        let newcomer = block_key(Some("100.64.0.1"));
        assert!(g.take(at(59), Draw::open(&newcomer)).is_ok());
        // A minute after the office's first hashes, they left the window: room again.
        assert!(g.take(at(120), Draw::open(&office)).is_ok());
        assert_eq!(GlobalLimit::default().shares_from, SHARES_FROM_PER_MINUTE);
        assert_eq!(SHARES_FROM_PER_MINUTE, 240);
    }

    // The review of 8e8f8fe: the setup-guess map and the work map forget idle keys on time,
    // not only once they hold 10,000.
    #[test]
    fn the_guess_and_work_maps_forget_idle_keys_on_time() {
        let g = BlockGuesses::default();
        for n in 0..50 {
            assert_eq!(
                g.take(&block_key(Some(&format!("192.0.{n}.1"))), at(0)),
                Guess::Counted
            );
        }
        assert_eq!(g.tracked(), 50);
        let later = at(0) + Duration::minutes(SETUP_GUESS_WINDOW_MINUTES);
        assert_eq!(
            g.take(&block_key(Some("198.51.100.1")), later),
            Guess::Counted
        );
        assert_eq!(g.tracked(), 1, "the idle blocks went");
        let w = AccountWork::default();
        for n in 0..50 {
            w.take(&format!("p{n}"), at(0)).unwrap();
        }
        w.take("ana", at(0) + Duration::minutes(ACCOUNT_WORK_MINUTES))
            .unwrap();
        assert_eq!(w.seen.lock().unwrap().len(), 1);
    }

    // G1: one address takes at most two of the reserve a minute, one block at most its share,
    // so someone who signed in from many addresses can't fill it.
    #[test]
    fn the_reserve_is_shared_by_address_and_by_block() {
        let g = GlobalLimit::default();
        // The open allowance is full (each block up to its shares).
        fill_open(&g, at(0));
        let known = |a: &str| (block_key(Some(a)), source_key(Some(a)));
        let (block, addr) = known("198.51.100.7");
        let draw = |block: &str, addr: &str| {
            g.take(
                at(1),
                Draw {
                    block,
                    reserve_for: Some(addr),
                },
            )
        };
        assert!(draw(&block, &addr).is_ok());
        assert!(draw(&block, &addr).is_ok());
        assert!(draw(&block, &addr).is_err(), "two a minute per address");
        // Other addresses in the same block: up to the block's share.
        let mut granted = 2;
        for host in 8..=200 {
            let (b, a) = known(&format!("198.51.100.{host}"));
            if draw(&b, &a).is_ok() {
                granted += 1;
            }
        }
        assert_eq!(granted, RESERVE_PER_BLOCK_PER_MINUTE);
        let (b, a) = known("203.0.113.9");
        assert!(draw(&b, &a).is_ok(), "another block's known address");
    }

    // The review of 8e8f8fe, a Low: refusals past a block's guesses are recorded, at most one
    // row a minute per block, each standing for the refusals since the last.
    #[test]
    fn refusals_past_a_blocks_guesses_are_recorded_thinned() {
        let g = BlockGuesses::default();
        let a = block_key(Some("203.0.113.5"));
        let b = block_key(Some("198.51.100.5"));
        assert_eq!(g.note_over_limit(&a, at(0)), Some(1), "the first");
        let rows: Vec<u32> = (1..60)
            .filter_map(|i| g.note_over_limit(&a, at(i)))
            .collect();
        assert!(rows.is_empty(), "none within the minute: {rows:?}");
        assert_eq!(
            g.note_over_limit(&b, at(30)),
            Some(1),
            "another block's own"
        );
        assert_eq!(
            g.note_over_limit(&a, at(60)),
            Some(60),
            "59 since, and this one"
        );
        assert_eq!(g.note_over_limit(&a, at(61)), None);
        assert_eq!(g.note_over_limit(&a, at(200)), Some(2));
    }

    #[test]
    fn setup_guesses_are_bounded_per_block_and_given_back_when_right() {
        let g = BlockGuesses::default();
        let block = block_key(Some("203.0.113.5"));
        for i in 0..SETUP_GUESSES_PER_BLOCK as i64 {
            assert_eq!(g.take(&block, at(i)), Guess::Counted);
        }
        assert_eq!(g.take(&block, at(200)), Guess::OverLimit(15 * 60 - 200));
        g.give_back(&block, at(0));
        assert_eq!(
            g.take(&block, at(200)),
            Guess::Counted,
            "a right code gives its slot back"
        );
        assert_eq!(
            g.take(&block_key(Some("198.51.100.1")), at(200)),
            Guess::Counted
        );
        assert_eq!(
            g.take(&block, at(200) + Duration::minutes(15)),
            Guess::Counted
        );
    }
}
