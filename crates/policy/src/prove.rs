//! `pmos policy prove`: OpenShell's policy prover beside `pmos policy check`
//! (`docs/openshell-prover-and-sandbox-audit-spec.md`, Part A, slices A1 and A2).
//!
//! For every bot's policy it builds the **network view** ([`network_view`]), runs the pinned
//! `openshell-prover check <view> --boundary <boundary> --output json --timeout 60`, kills it at
//! 90 seconds, and classifies the answer ([`classify`]). Only [`Status::Proved`] passes: exit
//! status 0, `result: within_boundary` and every one of the five [`DOMAINS`] in
//! `coverage.domains`. Everything else (exceeds, unsupported, inconclusive, an error, a
//! timeout, a missing or unpinned prover, another version) is reported as what it is and fails.
//!
//! Where the prover runs (PRV-1): only in CI and on the host as the OpenShell gateway's user
//! (`pmos-admin policy prove`), never in a sandbox, `pmos-web` or the tool server, and never
//! with a bot's credentials: the prover is started with an **empty environment** (only a fixed
//! `PATH`), so no token in the caller's environment reaches it.
//!
//! Every statement about the prover's command line and JSON here is from OpenShell's
//! documentation (v0.1.9, read 2026-11-24), not from a run. **Verify with the real pinned
//! prover (slice A2):** the `--version` flag and its output; the JSON field names `result`,
//! `reason_code`, `reason`, `coverage.domains` and the counterexample's key (read here as
//! `counterexample`, a string or an object); that `--timeout 60` is enough; that PM OS's policy
//! files parse with `credential_binding` and `mcp` present.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::check::{
    FS_LANDLOCK, FS_WORKDIR, FS_WRITABLE, NET_ALLOWED_IPS, NET_BINARIES, NET_EGRESS,
    NET_GATEWAY_ROUTES, NET_INSPECTION, NET_RUNTIME_ROUTES, PROC_IDENTITY,
};

/// The five domains a proof must cover (spec §A.2, P-NET to P-LL). A pass with any one missing
/// from `coverage.domains` is an [`Status::Error`], never a pass.
pub const DOMAINS: [&str; 5] = [
    "filesystem",
    "network_l4",
    "network_rest",
    "process",
    "landlock",
];

/// The boundary every bot is proved against, relative to the repository (spec §A.2). Hand
/// written and reviewed, never generated; a pull request that changes it may change nothing
/// else ([`boundary_alone`]).
pub const BOUNDARY_PATH: &str = "policies/boundary/pmos-bot.yaml";

/// The directory [`boundary_alone`] guards.
pub const BOUNDARY_DIR: &str = "policies/boundary/";

/// The pin: the prover's version and SHA-256s (spec §A.7).
pub const LOCK_PATH: &str = "deploy/openshell-prover.lock";

/// What `--timeout` the prover is given, in seconds (spec §A.3).
pub const PROVER_TIMEOUT_SECS: u64 = 60;

/// When the prover is killed if it hasn't finished (spec §A.3).
pub const KILL_AFTER: Duration = Duration::from_secs(90);

/// The most of the prover's output read; past it the answer is an error.
pub const MAX_OUTPUT: usize = 1 << 20;

/// The longest counterexample kept (spec §A.5: at most 256 characters).
pub const MAX_COUNTEREXAMPLE: usize = 256;

/// The longest reason or reason code kept.
pub const MAX_REASON: usize = 256;

/// The keys the network view removes, and only from an endpoint whose `protocol` is `mcp`
/// (spec §A.2). Removing request rules only widens what an endpoint allows, so a view within
/// the boundary means the real policy is too.
pub const VIEW_REMOVED_KEYS: [&str; 5] = ["protocol", "mcp", "rules", "path", "enforcement"];

/// What the screen and the report say when a proof ran on the view rather than the policy.
pub const VIEW_NOTE: &str = "proved on the network view (MCP rules removed, which only widens)";

/// What a prover pass never covers (spec §A.2). Always shown with a result, pass or not.
pub const NOT_PROVED: [&str; 6] = [
    "which paths are readable (the boundary allows reading /; `fs-readable` covers it)",
    "the MCP methods and tool allowlist (`net-tools`)",
    "credential bindings (`cred-binding`, `cred-providers`)",
    "uploads and the launch spec (`fs-uploads`, `spec-*`)",
    "name resolution (the prover does not resolve hostnames)",
    "what a running sandbox enforces",
];

/// A result, as PM OS names it (spec §A.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Exit 0, `within_boundary`, and all five domains covered. The only pass.
    Proved,
    /// Exit 1 and `exceeds_boundary`: the policy allows something the boundary doesn't.
    Exceeds,
    /// Exit 3 and `unsupported`: the prover can't check the policy's shape.
    Unsupported,
    /// Exit 3 and `inconclusive`: the solver gave up (limits, timeout).
    Inconclusive,
    /// Exit 2, any other exit, a timeout or kill, output that isn't the documented JSON, an exit
    /// code and result that disagree, or a missing domain.
    Error,
    /// No prover binary, nothing pinned, or its SHA-256 isn't the pinned one.
    NotAvailable,
    /// The binary's version isn't the pinned version (older or newer).
    VersionMismatch,
}

impl Status {
    pub const ALL: [Status; 7] = [
        Status::Proved,
        Status::Exceeds,
        Status::Unsupported,
        Status::Inconclusive,
        Status::Error,
        Status::NotAvailable,
        Status::VersionMismatch,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Status::Proved => "proved",
            Status::Exceeds => "exceeds",
            Status::Unsupported => "unsupported",
            Status::Inconclusive => "inconclusive",
            Status::Error => "error",
            Status::NotAvailable => "not_available",
            Status::VersionMismatch => "version_mismatch",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == s)
    }

    /// Only a proof passes.
    pub fn passes(self) -> bool {
        self == Status::Proved
    }
}

/// One classified answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Outcome {
    pub status: Status,
    /// The prover's `reason_code`, or PM OS's own (`timeout`, `exit_code_mismatch`, …).
    pub reason_code: Option<String>,
    /// A short, cleaned reason.
    pub reason: Option<String>,
    /// `coverage.domains` as the prover reported it, sorted.
    pub coverage: Vec<String>,
    /// For `exceeds`: what the policy allows beyond the boundary (a path, host, port or route
    /// from the policy, never data), cleaned and at most 256 characters.
    pub counterexample: Option<String>,
}

impl Outcome {
    pub fn of(status: Status, code: &str, reason: impl Into<String>) -> Self {
        Self {
            status,
            reason_code: Some(code.to_string()),
            reason: Some(clean(&reason.into(), MAX_REASON)),
            coverage: Vec::new(),
            counterexample: None,
        }
    }

    fn error(code: &str, reason: impl Into<String>) -> Self {
        Self::of(Status::Error, code, reason)
    }
}

/// `s` as printable ASCII (anything else becomes `?`), single-spaced, at most `max` characters.
pub fn clean(s: &str, max: usize) -> String {
    let mapped: String = s
        .chars()
        .map(|c| match c {
            ' '..='~' => c,
            '\t' | '\n' | '\r' => ' ',
            _ => '?',
        })
        .collect();
    let joined = mapped.split_whitespace().collect::<Vec<_>>().join(" ");
    joined.chars().take(max).collect()
}

fn text_of(v: &Value) -> Option<String> {
    match v {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        other => Some(other.to_string()),
    }
}

/// Classify one run of the prover: its exit code (`None` when it was killed or died by a
/// signal), whether PM OS killed it at [`KILL_AFTER`], and its standard output. Only exit 0
/// with `result: within_boundary` and all five domains is [`Status::Proved`] (PRV-3).
pub fn classify(exit: Option<i32>, timed_out: bool, stdout: &[u8]) -> Outcome {
    if timed_out {
        return Outcome::error(
            "timeout",
            format!(
                "the prover didn't finish in {} seconds and was killed",
                KILL_AFTER.as_secs()
            ),
        );
    }
    let Some(code) = exit else {
        return Outcome::error("killed", "the prover was killed by a signal");
    };
    if stdout.len() > MAX_OUTPUT {
        return Outcome::error("output_too_large", "the prover's output was too large");
    }
    let parsed: Value = match serde_json::from_slice(stdout) {
        Ok(v @ Value::Object(_)) => v,
        _ => {
            return Outcome::error(
                "not_json",
                format!("exit {code}, and the output isn't the documented JSON"),
            )
        }
    };
    let Some(result) = parsed.get("result").and_then(Value::as_str) else {
        return Outcome::error(
            "no_result",
            format!("exit {code}, and the JSON has no `result`"),
        );
    };
    let reason_code = parsed
        .get("reason_code")
        .and_then(text_of)
        .map(|s| clean(&s, MAX_REASON));
    let reason = parsed
        .get("reason")
        .and_then(text_of)
        .map(|s| clean(&s, MAX_REASON));
    let mut coverage: Vec<String> = match parsed.get("coverage").and_then(|c| c.get("domains")) {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(|s| clean(s, 64))
            .collect(),
        // The text form is `domains=a,b,c`; accept a comma-separated string too.
        Some(Value::String(s)) => s.split(',').map(|d| clean(d.trim(), 64)).collect(),
        _ => Vec::new(),
    };
    coverage.sort();
    coverage.dedup();
    let counterexample = parsed
        .get("counterexample")
        .and_then(text_of)
        .map(|s| clean(&s, MAX_COUNTEREXAMPLE));
    let base = |status: Status| Outcome {
        status,
        reason_code: reason_code.clone(),
        reason: reason.clone(),
        coverage: coverage.clone(),
        counterexample: None,
    };
    let mismatch = || {
        let mut o = base(Status::Error);
        o.reason_code = Some("exit_code_mismatch".into());
        o.reason = Some(clean(
            &format!("exit {code} with result `{result}`: the exit code and the result disagree"),
            MAX_REASON,
        ));
        o
    };
    match (code, result) {
        (0, "within_boundary") => {
            let missing: Vec<&str> = DOMAINS
                .iter()
                .copied()
                .filter(|d| !coverage.iter().any(|c| c == d))
                .collect();
            if missing.is_empty() {
                base(Status::Proved)
            } else {
                let mut o = base(Status::Error);
                o.reason_code = Some("missing_domain".into());
                o.reason = Some(format!(
                    "within_boundary, but coverage.domains lacks {}",
                    missing.join(", ")
                ));
                o
            }
        }
        (0, _) => mismatch(),
        (1, "exceeds_boundary") => {
            let mut o = base(Status::Exceeds);
            o.counterexample = counterexample;
            o
        }
        (1, _) => mismatch(),
        (3, "unsupported") => base(Status::Unsupported),
        (3, "inconclusive") => base(Status::Inconclusive),
        (3, _) => mismatch(),
        (2, _) => {
            let mut o = base(Status::Error);
            o.reason_code = o.reason_code.or(Some("prover_error".into()));
            o
        }
        (other, _) => {
            let mut o = base(Status::Error);
            o.reason_code = Some("unexpected_exit".into());
            o.reason = Some(format!("the prover exited {other}"));
            o
        }
    }
}

// The pin.

/// `deploy/openshell-prover.lock`: what CI and `pmos-admin` may run (spec §A.7). One
/// `KEY=value` per line; `#` starts a comment. An empty value means not pinned yet, and
/// nothing then runs: every bot is [`Status::NotAvailable`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Lock {
    /// `PROVER_VERSION`, such as `0.1.9`.
    pub version: String,
    /// `PROVER_RELEASE_TAG`: the GitHub release CI downloads the archive from.
    pub release_tag: String,
    /// `PROVER_BINARY_SHA256_<ARCH>`: the extracted `openshell-prover` binary, by architecture
    /// (`x86_64`, `aarch64`).
    pub binary_sha256: BTreeMap<String, String>,
    /// `PROVER_ARCHIVE_SHA256_<ARCH>`: the release archive CI downloads.
    pub archive_sha256: BTreeMap<String, String>,
    /// `OPENSHELL_FORMAT_VERIFIED`: the OpenShell version the OCSF format was last verified on
    /// (spec §B.8); empty until a host run (slice B6).
    pub openshell_format_verified: String,
}

impl Lock {
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut lock = Lock::default();
        for (i, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                return Err(format!("line {}: not KEY=value", i + 1));
            };
            let value = value.trim();
            if !value
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'))
            {
                return Err(format!("line {}: `{key}` has an unexpected character", i + 1));
            }
            match key.trim() {
                "PROVER_VERSION" => lock.version = value.to_string(),
                "PROVER_RELEASE_TAG" => lock.release_tag = value.to_string(),
                "OPENSHELL_FORMAT_VERIFIED" => lock.openshell_format_verified = value.to_string(),
                k => {
                    let (map, arch) = if let Some(a) = k.strip_prefix("PROVER_BINARY_SHA256_") {
                        (&mut lock.binary_sha256, a)
                    } else if let Some(a) = k.strip_prefix("PROVER_ARCHIVE_SHA256_") {
                        (&mut lock.archive_sha256, a)
                    } else {
                        return Err(format!("line {}: unknown key `{k}`", i + 1));
                    };
                    if !value.is_empty() && !is_sha256(value) {
                        return Err(format!("line {}: `{k}` is not a SHA-256", i + 1));
                    }
                    map.insert(arch.to_ascii_lowercase(), value.to_ascii_lowercase());
                }
            }
        }
        Ok(lock)
    }

    pub fn read(path: &Path) -> Result<Self, String> {
        let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// The pinned binary hash for this machine's architecture, if pinned.
    pub fn binary_for(&self, arch: &str) -> Option<&str> {
        self.binary_sha256
            .get(arch)
            .map(String::as_str)
            .filter(|s| !s.is_empty())
    }
}

fn is_sha256(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// SHA-256 of `bytes`, lowercase hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// The hash a proof records for a policy: SHA-256 of its canonical form (parsed, keys sorted,
/// written by [`crate::yaml::to_yaml`]), so a policy that differs only in formatting or key
/// order (as `openshell sandbox get --policy-only` may print it) hashes the same as the file it
/// came from. Text that isn't YAML hashes as its bytes. Policy Admin compares it with the
/// checked-in file's to tell a stale proof (spec §A.5).
pub fn policy_hash(text: &str) -> String {
    match serde_norway::from_str::<Value>(text) {
        Ok(v) if v.is_object() => match crate::yaml::to_yaml(&v) {
            Ok(canonical) => sha256_hex(canonical.as_bytes()),
            Err(_) => sha256_hex(text.as_bytes()),
        },
        _ => sha256_hex(text.as_bytes()),
    }
}

/// `a.b.c` against `x.y.z`, numerically; `None` if either isn't a version.
fn compare_versions(a: &str, b: &str) -> Option<std::cmp::Ordering> {
    let parse = |s: &str| -> Option<Vec<u64>> {
        let core = s.trim_start_matches('v').split(['-', '+']).next()?;
        core.split('.').map(|p| p.parse().ok()).collect()
    };
    Some(parse(a)?.cmp(&parse(b)?))
}

/// The first `x.y.z` (with an optional leading `v`) in `text`.
pub fn find_version(text: &str) -> Option<String> {
    text.split(|c: char| c.is_whitespace() || c == ',' || c == '(' || c == ')')
        .map(|t| t.trim_start_matches('v'))
        .find(|t| {
            let core = t.split(['-', '+']).next().unwrap_or("");
            let parts: Vec<&str> = core.split('.').collect();
            parts.len() >= 2
                && parts
                    .iter()
                    .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
        })
        .map(String::from)
}

// Running the prover.

/// Facts about the prover binary that every result carries.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ProverFacts {
    pub path: String,
    pub pinned_version: String,
    /// The version it reported, if it ran.
    pub version: Option<String>,
    pub sha256: Option<String>,
}

/// How to reach the prover.
#[derive(Debug, Clone)]
pub struct Prover {
    pub path: PathBuf,
    pub lock: Lock,
    /// The architecture whose pin applies (`std::env::consts::ARCH`).
    pub arch: String,
    pub kill_after: Duration,
}

impl Prover {
    pub fn new(path: PathBuf, lock: Lock) -> Self {
        Self {
            path,
            lock,
            arch: std::env::consts::ARCH.to_string(),
            kill_after: KILL_AFTER,
        }
    }

    /// Check the binary before any proof: present, pinned, the pinned SHA-256, the pinned
    /// version. `Err` is the outcome every bot gets (PRV-7).
    pub fn verify(&self) -> (ProverFacts, Result<(), Outcome>) {
        let mut facts = ProverFacts {
            path: self.path.display().to_string(),
            pinned_version: self.lock.version.clone(),
            ..ProverFacts::default()
        };
        if self.lock.version.is_empty() {
            return (
                facts,
                Err(Outcome::of(
                    Status::NotAvailable,
                    "not_pinned",
                    format!("no prover version is pinned in {LOCK_PATH}"),
                )),
            );
        }
        let Some(pinned) = self.lock.binary_for(&self.arch) else {
            return (
                facts,
                Err(Outcome::of(
                    Status::NotAvailable,
                    "not_pinned",
                    format!(
                        "no binary SHA-256 is pinned for {} in {LOCK_PATH}",
                        self.arch
                    ),
                )),
            );
        };
        let bytes = match fs::read(&self.path) {
            Ok(b) => b,
            Err(_) => {
                return (
                    facts,
                    Err(Outcome::of(
                        Status::NotAvailable,
                        "no_binary",
                        "no prover binary at the given path",
                    )),
                )
            }
        };
        let found = sha256_hex(&bytes);
        facts.sha256 = Some(found.clone());
        if found != pinned {
            return (
                facts,
                Err(Outcome::of(
                    Status::NotAvailable,
                    "hash_mismatch",
                    "the prover binary's SHA-256 isn't the pinned one",
                )),
            );
        }
        let run = run_bounded(&self.path, &["--version"], self.kill_after);
        let text = String::from_utf8_lossy(&run.stdout);
        let version = if run.exit == Some(0) && !run.timed_out {
            find_version(&text)
        } else {
            None
        };
        facts.version = version.clone();
        let Some(version) = version else {
            return (
                facts,
                Err(Outcome::of(
                    Status::VersionMismatch,
                    "version_unknown",
                    format!(
                        "the prover's version couldn't be read; pinned {}",
                        self.lock.version
                    ),
                )),
            );
        };
        match compare_versions(&version, &self.lock.version) {
            Some(std::cmp::Ordering::Equal) => (facts, Ok(())),
            ord => {
                let how = match ord {
                    Some(std::cmp::Ordering::Less) => "too old",
                    Some(std::cmp::Ordering::Greater) => "newer",
                    _ => "different",
                };
                (
                    facts,
                    Err(Outcome::of(
                        Status::VersionMismatch,
                        if how == "too old" {
                            "version_too_old"
                        } else {
                            "version_different"
                        },
                        format!("found {version} ({how}), pinned {}", self.lock.version),
                    )),
                )
            }
        }
    }

    /// One proof of `view` against `boundary`.
    pub fn check(&self, view: &Path, boundary: &Path) -> Outcome {
        let timeout = PROVER_TIMEOUT_SECS.to_string();
        let view = view.to_string_lossy();
        let boundary = boundary.to_string_lossy();
        let run = run_bounded(
            &self.path,
            &[
                "check",
                &view,
                "--boundary",
                &boundary,
                "--output",
                "json",
                "--timeout",
                &timeout,
            ],
            self.kill_after,
        );
        if let Some(e) = run.spawn_error {
            return Outcome::of(Status::NotAvailable, "no_binary", e);
        }
        classify(run.exit, run.timed_out, &run.stdout)
    }
}

struct Run {
    exit: Option<i32>,
    timed_out: bool,
    stdout: Vec<u8>,
    spawn_error: Option<String>,
}

/// Run `program args` with an empty environment (a fixed `PATH` only: no token the caller
/// holds reaches the prover), no standard input, and a hard deadline.
fn run_bounded(program: &Path, args: &[&str], kill_after: Duration) -> Run {
    let child = Command::new(program)
        .args(args)
        .env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            return Run {
                exit: None,
                timed_out: false,
                stdout: Vec::new(),
                spawn_error: Some(format!("the prover couldn't be started: {}", e.kind())),
            }
        }
    };
    let (tx, rx) = mpsc::channel();
    if let Some(mut out) = child.stdout.take() {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = (&mut out)
                .take(MAX_OUTPUT as u64 + 1)
                .read_to_end(&mut buf);
            let _ = tx.send(buf);
        });
    }
    let deadline = Instant::now() + kill_after;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() >= deadline => {
                timed_out = true;
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => break None,
        }
    };
    // The reader ends when the pipe closes; a grandchild holding it open can't block us.
    let stdout = rx.recv_timeout(Duration::from_secs(2)).unwrap_or_default();
    Run {
        exit: status.and_then(|s| s.code()),
        timed_out,
        stdout,
        spawn_error: None,
    }
}

// The network view.

/// The network view of a policy (spec §A.2, PRV-4): a copy in which every endpoint whose
/// `protocol` is `mcp` loses exactly [`VIEW_REMOVED_KEYS`] (`protocol`, `mcp`, `rules`,
/// `path`, `enforcement`) and keeps everything else (`host`, `port`, `ports`, `allowed_ips`, its
/// rule's binaries, any other key). Nothing is added or changed. The prover reports an MCP
/// endpoint `unsupported`; removing its request rules only widens what it allows, so a view
/// within the boundary means the real policy is too. The MCP rules themselves are not proved
/// (`net-tools` covers them). Returns the view and whether anything was removed.
pub fn network_view(policy: &Value) -> (Value, bool) {
    let mut view = policy.clone();
    let mut changed = false;
    if let Some(Value::Object(rules)) = view.get_mut("network_policies") {
        for rule in rules.values_mut() {
            let Some(Value::Array(endpoints)) = rule.get_mut("endpoints") else {
                continue;
            };
            for endpoint in endpoints.iter_mut() {
                let Value::Object(map) = endpoint else {
                    continue;
                };
                if map.get("protocol").and_then(Value::as_str) != Some("mcp") {
                    continue;
                }
                for key in VIEW_REMOVED_KEYS {
                    changed |= map.remove(key).is_some();
                }
            }
        }
    }
    (view, changed)
}

/// The view of a policy file's text, written deterministically. `Err` when it isn't a YAML map.
pub fn network_view_text(text: &str) -> Result<(String, bool), String> {
    let value: Value = serde_norway::from_str(text).map_err(|e| format!("not YAML: {e}"))?;
    if !value.is_object() {
        return Err("not a policy (a YAML map)".into());
    }
    let (view, changed) = network_view(&value);
    let yaml = crate::yaml::to_yaml(&view).map_err(|e| e.to_string())?;
    Ok((yaml, changed))
}

// Beside `pmos policy check` (spec §A.4).

/// The `pmos policy check` rules that claim what each domain proves.
pub fn mapped_rules(domain: &str) -> &'static [&'static str] {
    match domain {
        "network_l4" => &[NET_EGRESS, NET_ALLOWED_IPS, NET_BINARIES],
        "network_rest" => &[NET_GATEWAY_ROUTES, NET_RUNTIME_ROUTES, NET_INSPECTION],
        "filesystem" => &[FS_WRITABLE, FS_WORKDIR],
        "process" => &[PROC_IDENTITY],
        "landlock" => &[FS_LANDLOCK],
        _ => &[],
    }
}

/// Every mapped rule, across the five domains.
pub fn all_mapped_rules() -> BTreeSet<&'static str> {
    DOMAINS
        .iter()
        .flat_map(|d| mapped_rules(d).iter().copied())
        .collect()
}

/// The domain a counterexample names, when it says so plainly (an object with `domain`, or
/// text starting with a domain's word). `None` when it can't be told, and then every domain's
/// rules are compared (the more refusing reading).
pub fn exceeds_domain(counterexample: &str) -> Option<&'static str> {
    let lower = counterexample.to_ascii_lowercase();
    if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(counterexample) {
        if let Some(d) = map.get("domain").and_then(Value::as_str) {
            return DOMAINS.iter().copied().find(|x| *x == d);
        }
    }
    let first = lower.split_whitespace().next().unwrap_or("");
    match first {
        "filesystem" => Some("filesystem"),
        "process" => Some("process"),
        "landlock" => Some("landlock"),
        "network_l4" => Some("network_l4"),
        "network_rest" | "rest" => Some("network_rest"),
        _ => None,
    }
}

/// What comparing a result with the check says (spec §A.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Finding {
    /// The check passes every mapped rule of the domain while the prover says `exceeds`: PM
    /// OS's rules have a hole. Fails CI; a High finding until the reviewer triages it.
    Disagreement,
    /// A mapped rule fails while the prover says `proved`: the check is stricter than the
    /// boundary. The job fails on the check anyway; listed for review, not as an error.
    CheckStricter,
}

/// Compare one bot's prover outcome with the check rules its policy file failed.
pub fn compare(outcome: &Outcome, failed_rules: &BTreeSet<String>) -> Option<Finding> {
    match outcome.status {
        Status::Exceeds => {
            let rules: Vec<&str> = match outcome.counterexample.as_deref().and_then(exceeds_domain)
            {
                Some(d) => mapped_rules(d).to_vec(),
                None => all_mapped_rules().into_iter().collect(),
            };
            rules
                .iter()
                .all(|r| !failed_rules.contains(*r))
                .then_some(Finding::Disagreement)
        }
        Status::Proved => all_mapped_rules()
            .iter()
            .any(|r| failed_rules.contains(*r))
            .then_some(Finding::CheckStricter),
        _ => None,
    }
}

// The boundary-alone rule (spec §A.6, PRV-5).

/// A pull request that changes `policies/boundary/` may change no other file. `changed` is
/// the list of paths it changes (`git diff --name-only base...head`). `Err` names the others.
pub fn boundary_alone(changed: &[&str]) -> Result<(), String> {
    let changed: Vec<&str> = changed
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    let touches = changed.iter().any(|p| p.starts_with(BOUNDARY_DIR));
    if !touches {
        return Ok(());
    }
    let others: Vec<&str> = changed
        .iter()
        .copied()
        .filter(|p| !p.starts_with(BOUNDARY_DIR))
        .collect();
    if others.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "this change edits {BOUNDARY_DIR} and {} other file(s) ({}): a boundary change must come alone in its own pull request, under CODEOWNERS and POL-2 review",
            others.len(),
            others.join(", ")
        ))
    }
}

// The whole run.

/// One bot's result.
#[derive(Debug, Clone, Serialize)]
pub struct BotResult {
    pub bot: String,
    /// The file proved, as given.
    pub candidate: String,
    /// [`policy_hash`] of the candidate.
    pub candidate_sha256: String,
    /// SHA-256 of the view's text, when one was built.
    pub view_sha256: Option<String>,
    /// [`VIEW_NOTE`] when the view removed MCP rules.
    pub view: Option<&'static str>,
    #[serde(flatten)]
    pub outcome: Outcome,
    /// The `pmos policy check` rules this file failed, of those mapped to a domain.
    pub check_failed: Vec<String>,
    pub finding: Option<Finding>,
}

/// A whole run's report (`pmos policy prove --format json`).
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub prover: ProverFacts,
    pub boundary: String,
    pub boundary_sha256: Option<String>,
    pub results: Vec<BotResult>,
    pub not_proved: [&'static str; 6],
}

impl Report {
    /// Every bot proved, and no disagreement.
    pub fn passed(&self) -> bool {
        !self.results.is_empty()
            && self
                .results
                .iter()
                .all(|r| r.outcome.status.passes() && r.finding != Some(Finding::Disagreement))
    }
}

/// One policy to prove.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub bot: String,
    pub path: PathBuf,
    /// The label shown (the path as given).
    pub label: String,
    /// The mapped check rules this file failed (empty when the check wasn't run on it).
    pub check_failed: BTreeSet<String>,
}

/// Every bot's policy under `policies/bots/` (`<bot>.yaml`, not the launch specs), sorted.
pub fn candidates(policies: &Path) -> std::io::Result<Vec<(String, PathBuf)>> {
    let dir = policies.join("bots");
    let mut found = Vec::new();
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !entry.file_type()?.is_file() || name.ends_with(".sandbox.yaml") {
            continue;
        }
        if let Some(bot) = name.strip_suffix(".yaml") {
            found.push((bot.to_string(), entry.path()));
        }
    }
    found.sort();
    Ok(found)
}

/// Prove every candidate against `boundary`, writing each view into `scratch`.
pub fn prove_all(
    prover: &Prover,
    boundary: &Path,
    candidates: &[Candidate],
    scratch: &Path,
) -> Report {
    let (facts, ready) = prover.verify();
    let boundary_text = fs::read(boundary).ok();
    let boundary_sha256 = boundary_text.as_deref().map(sha256_hex);
    let mut sorted: Vec<&Candidate> = candidates.iter().collect();
    sorted.sort_by(|a, b| a.bot.cmp(&b.bot).then(a.label.cmp(&b.label)));
    let results = sorted
        .into_iter()
        .map(|c| {
            let text = fs::read_to_string(&c.path);
            let candidate_sha256 = match &text {
                Ok(t) => policy_hash(t),
                Err(_) => String::new(),
            };
            let mut r = BotResult {
                bot: c.bot.clone(),
                candidate: c.label.clone(),
                candidate_sha256,
                view_sha256: None,
                view: None,
                outcome: Outcome::error("unread", "the candidate couldn't be read"),
                check_failed: c.check_failed.iter().cloned().collect(),
                finding: None,
            };
            let Ok(text) = text else {
                return r;
            };
            if let Err(o) = &ready {
                r.outcome = o.clone();
                return r;
            }
            if boundary_text.is_none() {
                r.outcome = Outcome::error("no_boundary", "the boundary file couldn't be read");
                return r;
            }
            let (view, changed) = match network_view_text(&text) {
                Ok(v) => v,
                Err(e) => {
                    r.outcome = Outcome::error("not_a_policy", e);
                    return r;
                }
            };
            r.view_sha256 = Some(sha256_hex(view.as_bytes()));
            r.view = changed.then_some(VIEW_NOTE);
            let view_path = scratch.join(format!("{}.view.yaml", c.bot));
            if fs::write(&view_path, &view).is_err() {
                r.outcome = Outcome::error("scratch", "the view couldn't be written");
                return r;
            }
            r.outcome = prover.check(&view_path, boundary);
            r.finding = compare(&r.outcome, &c.check_failed);
            r
        })
        .collect();
    Report {
        prover: facts,
        boundary: boundary.display().to_string(),
        boundary_sha256,
        results,
        not_proved: NOT_PROVED,
    }
}

/// The report as text, one block per bot, deterministic.
pub fn render_text(report: &Report) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "prover {} (pinned {}), sha256 {}\nboundary {} sha256 {}\n",
        report.prover.version.as_deref().unwrap_or("unknown"),
        if report.prover.pinned_version.is_empty() {
            "nothing"
        } else {
            &report.prover.pinned_version
        },
        report.prover.sha256.as_deref().unwrap_or("-"),
        report.boundary,
        report.boundary_sha256.as_deref().unwrap_or("-"),
    ));
    for r in &report.results {
        let mut line = format!("{}: {}", r.bot, r.outcome.status.as_str());
        if let Some(code) = &r.outcome.reason_code {
            line.push_str(&format!(" [{code}]"));
        }
        if let Some(reason) = &r.outcome.reason {
            line.push_str(&format!(" {reason}"));
        }
        if let Some(cx) = &r.outcome.counterexample {
            line.push_str(&format!(" counterexample: {cx}"));
        }
        out.push_str(&line);
        out.push('\n');
        out.push_str(&format!(
            "  candidate {} sha256 {}\n",
            r.candidate, r.candidate_sha256
        ));
        if let Some(v) = &r.view_sha256 {
            out.push_str(&format!("  view sha256 {v}\n"));
        }
        if let Some(note) = r.view {
            out.push_str(&format!("  {note}\n"));
        }
        if !r.outcome.coverage.is_empty() {
            out.push_str(&format!(
                "  coverage: domains={}\n",
                r.outcome.coverage.join(",")
            ));
        }
        match r.finding {
            Some(Finding::Disagreement) => out.push_str(
                "  DISAGREEMENT: `pmos policy check` passes the rules mapped to this domain while the prover says exceeds; treat as a High finding until the reviewer triages it\n",
            ),
            Some(Finding::CheckStricter) => out.push_str(&format!(
                "  check-stricter: proved, but `pmos policy check` fails {} (the check is stricter than the boundary; for review)\n",
                r.check_failed.join(", ")
            )),
            None => {}
        }
    }
    out.push_str("never proved, whatever the result:\n");
    for n in NOT_PROVED {
        out.push_str(&format!("  - {n}\n"));
    }
    let proved = report
        .results
        .iter()
        .filter(|r| r.outcome.status.passes())
        .count();
    out.push_str(&format!(
        "{proved} of {} bots proved; {}\n",
        report.results.len(),
        if report.passed() {
            "passed"
        } else {
            "FAILED (only `proved` passes)"
        }
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ALL: &str = r#""coverage":{"domains":["filesystem","network_l4","network_rest","process","landlock"]}"#;

    fn out(s: &str) -> Vec<u8> {
        s.as_bytes().to_vec()
    }

    #[test]
    fn only_exit_zero_within_with_every_domain_is_proved() {
        let within = format!(r#"{{"result":"within_boundary",{ALL}}}"#);
        assert_eq!(
            classify(Some(0), false, &out(&within)).status,
            Status::Proved
        );
        // Exit 1 with within_boundary, exit 0 with exceeds: the two disagree, an error.
        assert_eq!(
            classify(Some(1), false, &out(&within)).status,
            Status::Error
        );
        let exceeds = r#"{"result":"exceeds_boundary","counterexample":"filesystem write /sandbox"}"#;
        let o = classify(Some(0), false, &out(exceeds));
        assert_eq!(o.status, Status::Error);
        assert_eq!(o.reason_code.as_deref(), Some("exit_code_mismatch"));
        let o = classify(Some(1), false, &out(exceeds));
        assert_eq!(o.status, Status::Exceeds);
        assert_eq!(o.counterexample.as_deref(), Some("filesystem write /sandbox"));
        // A missing domain.
        let partial = r#"{"result":"within_boundary","coverage":{"domains":["filesystem","network_l4","process","landlock"]}}"#;
        let o = classify(Some(0), false, &out(partial));
        assert_eq!(o.status, Status::Error);
        assert_eq!(o.reason_code.as_deref(), Some("missing_domain"));
        assert!(o.reason.unwrap().contains("network_rest"));
        // No coverage at all.
        assert_eq!(
            classify(Some(0), false, &out(r#"{"result":"within_boundary"}"#)).status,
            Status::Error
        );
    }

    #[test]
    fn every_other_answer_is_what_it_is_and_never_a_pass() {
        let cases = [
            (
                Some(3),
                r#"{"result":"unsupported","reason_code":"unsupported_policy_shape","reason":"mcp rules"}"#,
                Status::Unsupported,
            ),
            (
                Some(3),
                r#"{"result":"inconclusive","reason_code":"solver_timeout"}"#,
                Status::Inconclusive,
            ),
            (Some(3), r#"{"result":"within_boundary"}"#, Status::Error),
            (Some(2), r#"{"result":"error","reason":"bad yaml"}"#, Status::Error),
            (Some(2), "not json", Status::Error),
            (Some(0), "not json", Status::Error),
            (Some(0), "[1,2]", Status::Error),
            (Some(0), r#"{"no_result":1}"#, Status::Error),
            (Some(4), r#"{"result":"within_boundary"}"#, Status::Error),
            (None, r#"{"result":"within_boundary"}"#, Status::Error),
        ];
        for (exit, body, want) in cases {
            let o = classify(exit, false, &out(body));
            assert_eq!(o.status, want, "{exit:?} {body}");
            assert!(!o.status.passes());
        }
        let within = format!(r#"{{"result":"within_boundary",{ALL}}}"#);
        let o = classify(Some(0), true, &out(&within));
        assert_eq!(o.status, Status::Error, "a timeout is never a pass");
        assert_eq!(o.reason_code.as_deref(), Some("timeout"));
        for s in Status::ALL {
            assert_eq!(s.passes(), s == Status::Proved);
            assert_eq!(Status::parse(s.as_str()), Some(s));
        }
    }

    #[test]
    fn counterexamples_and_reasons_are_cleaned_and_capped() {
        let long = "x".repeat(1000);
        let body = json!({"result": "exceeds_boundary", "counterexample": format!("<script>\u{1b}[31m\n{long}")});
        let o = classify(Some(1), false, body.to_string().as_bytes());
        let cx = o.counterexample.unwrap();
        assert!(cx.len() <= MAX_COUNTEREXAMPLE);
        assert!(cx.chars().all(|c| (' '..='~').contains(&c)));
        assert!(!cx.contains('\u{1b}') && !cx.contains('\n'));
        let obj = json!({"result": "exceeds_boundary", "counterexample": {"domain": "network_l4", "host": "evil.example"}});
        let o = classify(Some(1), false, obj.to_string().as_bytes());
        assert!(o.counterexample.unwrap().contains("evil.example"));
    }

    #[test]
    fn the_lock_parses_and_refuses_anything_unexpected() {
        let lock = Lock::parse(
            "# pin\nPROVER_VERSION=0.1.9\nPROVER_BINARY_SHA256_X86_64=ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef0123456789\nPROVER_BINARY_SHA256_AARCH64=\nOPENSHELL_FORMAT_VERIFIED=\n",
        )
        .unwrap();
        assert_eq!(lock.version, "0.1.9");
        assert_eq!(
            lock.binary_for("x86_64"),
            Some("abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789")
        );
        assert_eq!(lock.binary_for("aarch64"), None);
        assert!(Lock::parse("PROVER_VERSION=$(id)").is_err());
        assert!(Lock::parse("SOMETHING=1").is_err());
        assert!(Lock::parse("PROVER_BINARY_SHA256_X86_64=abc").is_err());
        assert!(Lock::parse("no equals sign").is_err());
    }

    #[test]
    fn versions_are_found_and_compared() {
        assert_eq!(
            find_version("openshell-prover 0.1.9").as_deref(),
            Some("0.1.9")
        );
        assert_eq!(find_version("v0.1.10 (abc)").as_deref(), Some("0.1.10"));
        assert_eq!(find_version("no version here"), None);
        assert_eq!(
            compare_versions("0.1.10", "0.1.9"),
            Some(std::cmp::Ordering::Greater)
        );
        assert_eq!(
            compare_versions("0.1.8", "0.1.9"),
            Some(std::cmp::Ordering::Less)
        );
        assert_eq!(
            compare_versions("0.1.9", "0.1.9"),
            Some(std::cmp::Ordering::Equal)
        );
    }

    #[test]
    fn a_disagreement_and_a_stricter_check_are_findings() {
        let exceeds = Outcome {
            status: Status::Exceeds,
            reason_code: None,
            reason: None,
            coverage: vec![],
            counterexample: Some("filesystem write /sandbox".into()),
        };
        let none = BTreeSet::new();
        assert_eq!(compare(&exceeds, &none), Some(Finding::Disagreement));
        let fs_failed: BTreeSet<String> = [FS_WRITABLE.to_string()].into();
        assert_eq!(compare(&exceeds, &fs_failed), None);
        // A network rule failing doesn't explain a filesystem exceed.
        let net_failed: BTreeSet<String> = [NET_EGRESS.to_string()].into();
        assert_eq!(compare(&exceeds, &net_failed), Some(Finding::Disagreement));
        // An unreadable counterexample: any mapped rule failing explains it.
        let vague = Outcome {
            counterexample: Some("something odd".into()),
            ..exceeds.clone()
        };
        assert_eq!(compare(&vague, &net_failed), None);
        assert_eq!(compare(&vague, &none), Some(Finding::Disagreement));
        let proved = Outcome {
            status: Status::Proved,
            counterexample: None,
            ..exceeds
        };
        assert_eq!(compare(&proved, &none), None);
        assert_eq!(compare(&proved, &net_failed), Some(Finding::CheckStricter));
        // A rule outside the five domains (net-tools) isn't a stricter check of the boundary.
        let tools: BTreeSet<String> = ["net-tools".to_string()].into();
        assert_eq!(compare(&proved, &tools), None);
    }

    #[test]
    fn a_boundary_change_must_come_alone() {
        assert!(boundary_alone(&["policies/boundary/pmos-bot.yaml"]).is_ok());
        assert!(boundary_alone(&["policies/bots/docs.yaml", "crates/policy/src/lib.rs"]).is_ok());
        assert!(boundary_alone(&[]).is_ok());
        let err = boundary_alone(&[
            "policies/boundary/pmos-bot.yaml",
            "policies/bots/docs.yaml",
        ])
        .unwrap_err();
        assert!(err.contains("policies/bots/docs.yaml"), "{err}");
        assert!(boundary_alone(&["policies/boundary/pmos-bot.yaml", "  ", ""]).is_ok());
    }

    #[test]
    fn policy_hash_ignores_formatting_and_key_order() {
        let a = "version: 1\nprocess:\n  run_as_user: sandbox\n  run_as_group: sandbox\n";
        let b = "process: {run_as_group: sandbox, run_as_user: sandbox}\nversion: 1\n";
        assert_eq!(policy_hash(a), policy_hash(b));
        assert_ne!(policy_hash(a), policy_hash("version: 1\n"));
    }
}
