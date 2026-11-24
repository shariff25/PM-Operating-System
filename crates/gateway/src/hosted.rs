//! `PMOS_MODEL_SOURCE=hosted` (slice H12, `docs/openshell-any-host-spec.md` §3.6, "The hosted
//! model as a production source"): one owner-approved hosted model, for real data.
//!
//! **The approved list** is `crates/gateway/approved-hosted.yaml`, compiled in with
//! `include_str!`, so changing it needs a reviewed, merged commit and a rebuilt image; it is
//! never read from a mount, `deploy/` or the environment. It holds names, digests and a pull
//! request number only (r7, X1): never a host, a URL, a model id, a vendor's name or a key. It
//! ships **empty**, so a gateway built from a fresh checkout refuses `hosted` at start ("no
//! approved entry"), and nothing can send real data anywhere until the owner's slice H13 adds
//! the first entry under the CODEOWNERS rule (`pmos_contract::codeowners`).
//!
//! **The URL and the model ids** come from the host (`PMOS_GATEWAY_HOSTED_URL`,
//! `PMOS_GATEWAY_HOSTED_MODELS` in `deploy/.env`), and are used only when they hash to the
//! entry's `url_sha256` and `model_sha256` ([`bind`]); every request is then built from the
//! values checked here, never read again from the environment.
//!
//! **The test-only list** (`approved-hosted.stack-test.yaml`, the `stack-test-list` Cargo
//! feature, built only by `deploy/test/Dockerfile.gateway-stack-test`) replaces the release
//! list in that build. Its one entry is named [`STACK_TEST_NAME`] and names only a local mock;
//! every service treats a gateway that reports `hosted:stack-test` as fake-data mode
//! (`pmos_contract::model_source::reported_fake_data_only`).

use std::collections::BTreeMap;
use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use pmos_contract::model::Sensitivity;
pub use pmos_contract::model_source::STACK_TEST_NAME;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::Url;
use serde::Deserialize;

use crate::config::{sha256_hex, Models, UpstreamKind};

/// The release list, as committed.
pub const RELEASE_LIST: &str = include_str!("../approved-hosted.yaml");

/// The test-only list (spec §3.6, "How H12's stack test runs `hosted`"): compiled in place of
/// the release list only with the `stack-test-list` feature, which no release build enables
/// (checked in CI and by `tests/stack_test_feature.rs`).
pub const STACK_TEST_LIST: &str = include_str!("../approved-hosted.stack-test.yaml");

/// Where the stop file's directory is mounted in the gateway's container (spec §3.6, "The
/// stop file", r7 V7): read-only, a directory so a file made after the start is seen. A
/// constant, never a setting.
pub const CONTROL_DIR: &str = "/run/pmos-hosted-control";

/// The stop file's name in [`CONTROL_DIR`].
pub const STOP_FILE: &str = "stopped";

/// Which list a build carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListKind {
    Release,
    StackTest,
}

impl ListKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Release => "release",
            Self::StackTest => "stack-test",
        }
    }
}

/// `tls_interception:` of an entry (spec §3.9, condition 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TlsInterception {
    /// The default: no run-time CA, only the compiled-in roots.
    Refused,
    /// The owner confirmed a re-signing proxy is part of the data path: the CA bundle in
    /// `PMOS_GATEWAY_CA_FILE` is added to the compiled-in roots.
    Allowed,
}

impl TlsInterception {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Refused => "refused",
            Self::Allowed => "allowed",
        }
    }
}

/// One approved entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub url_sha256: String,
    pub model_sha256: Vec<String>,
    pub record_sha256: String,
    pub pull: u64,
    pub max_data_class: Sensitivity,
    pub tls_interception: TlsInterception,
}

/// The compiled list: its kind, its entries and the SHA-256 of its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovedList {
    pub kind: ListKind,
    pub entries: Vec<Entry>,
    /// `approved_list_sha256`: the SHA-256 of the file's bytes, as `pmos-admin status`
    /// computes it from the merged branch.
    pub sha256: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EntrySpec {
    name: String,
    url_sha256: String,
    model_sha256: Vec<String>,
    record_sha256: String,
    pull: i64,
    #[serde(default)]
    max_data_class: Option<String>,
    #[serde(default)]
    tls_interception: Option<String>,
}

/// A lowercase 64-hex-digit SHA-256.
pub fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// An entry name: lowercase letters, digits and inner hyphens, at most 64 characters.
pub fn valid_entry_name(name: &str) -> bool {
    pmos_contract::model_source::valid_reported(&format!("hosted:{name}"))
}

/// Parses a list file. Comments and blank lines only, or `[]`, is the empty list. Every
/// entry is checked ([`check_entry`]), and a list of more than one entry is refused (r6, V2:
/// one gateway serves one source; a second provider is requirement i's reviewed change).
pub fn parse_list(kind: ListKind, text: &str) -> Result<ApprovedList, String> {
    let sha256 = sha256_hex(text);
    let has_content = text
        .lines()
        .any(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'));
    let specs: Vec<EntrySpec> = if has_content {
        serde_norway::from_str(text).map_err(|e| format!("the approved hosted list: {e}"))?
    } else {
        Vec::new()
    };
    if specs.len() > 1 {
        return Err(format!(
            "the approved hosted list has {} entries; it may have at most one (spec §3.6, V2)",
            specs.len()
        ));
    }
    let mut entries = Vec::new();
    for s in specs {
        entries.push(check_entry(kind, s)?);
    }
    Ok(ApprovedList {
        kind,
        entries,
        sha256,
    })
}

fn check_entry(kind: ListKind, s: EntrySpec) -> Result<Entry, String> {
    let fail = |why: &str| {
        Err(format!(
            "the approved hosted list, entry `{}`: {why}",
            s.name
        ))
    };
    if !valid_entry_name(&s.name) {
        return Err(
            "the approved hosted list: an entry's `name` must be lowercase letters, digits and \
             inner hyphens"
                .into(),
        );
    }
    match kind {
        ListKind::Release if s.name == STACK_TEST_NAME => {
            return fail("`stack-test` is the test-only list's name; the release list can't use it")
        }
        ListKind::StackTest if s.name != STACK_TEST_NAME => {
            return fail("the test-only list's one entry is named `stack-test`")
        }
        _ => {}
    }
    if !is_sha256_hex(&s.url_sha256) {
        return fail("`url_sha256` must be 64 lowercase hex digits");
    }
    if s.model_sha256.is_empty() {
        return fail("`model_sha256` needs at least one digest");
    }
    if s.model_sha256.iter().any(|d| !is_sha256_hex(d)) {
        return fail("each `model_sha256` must be 64 lowercase hex digits");
    }
    if !is_sha256_hex(&s.record_sha256) {
        return fail("`record_sha256` must be 64 lowercase hex digits");
    }
    if s.pull < 1 {
        return fail("`pull` must be the positive number of the pull request that adds it");
    }
    let max_data_class = match s.max_data_class.as_deref() {
        None => Sensitivity::Restricted,
        Some(c) => Sensitivity::parse(c).ok_or_else(|| {
            format!(
                "the approved hosted list, entry `{}`: `max_data_class` must be public, \
                 internal or restricted",
                s.name
            )
        })?,
    };
    let tls_interception = match s.tls_interception.as_deref() {
        None | Some("refused") => TlsInterception::Refused,
        Some("allowed") => TlsInterception::Allowed,
        Some(_) => return fail("`tls_interception` must be `refused` or `allowed`"),
    };
    Ok(Entry {
        name: s.name,
        url_sha256: s.url_sha256,
        model_sha256: s.model_sha256,
        record_sha256: s.record_sha256,
        pull: s.pull as u64,
        max_data_class,
        tls_interception,
    })
}

/// The list this build carries: the release list, or with the `stack-test-list` feature the
/// test-only one **instead**. Parsed at start; a list that doesn't parse stops the gateway.
pub fn compiled() -> Result<ApprovedList, String> {
    if cfg!(feature = "stack-test-list") {
        parse_list(ListKind::StackTest, STACK_TEST_LIST)
    } else {
        parse_list(ListKind::Release, RELEASE_LIST)
    }
}

/// What the build says about where it came from (spec §3.6, "Built only from committed code",
/// X5): compile-time values from `deploy/pmos-admin build`'s build arguments, never the
/// run-time environment.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BuildInfo {
    pub commit: Option<String>,
    pub merged_commit: Option<String>,
    pub list_checked_at: Option<String>,
}

impl BuildInfo {
    /// This binary's.
    pub fn compiled() -> Self {
        let get = |v: Option<&'static str>| v.filter(|s| !s.is_empty()).map(String::from);
        Self {
            commit: get(option_env!("PMOS_BUILD_COMMIT")),
            merged_commit: get(option_env!("PMOS_BUILD_MERGED_COMMIT")),
            list_checked_at: get(option_env!("PMOS_BUILD_LIST_CHECKED_AT")),
        }
    }
}

/// The normalised form of a hosted URL and the parsed value requests are built from (spec
/// §3.6, "Digests, not names" and "Hash and connect from the same parsed URL"): `https://` +
/// the host in lowercase ASCII (an internationalised name in its punycode form, a trailing dot
/// dropped) + `:<port>` only when not 443 + the path without a trailing `/`. Refused, never
/// normalised: whitespace, a scheme other than `https`, user information, a query, a fragment,
/// and a host that is an IP literal (it would skip the hosted resolver's checks). The messages
/// name the setting, never the URL.
pub fn normalise_url(text: &str) -> Result<(Url, String), String> {
    const S: &str = "PMOS_GATEWAY_HOSTED_URL";
    if text.is_empty() {
        return Err(format!("{S} is required with PMOS_MODEL_SOURCE=hosted"));
    }
    if text.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(format!("{S} must not contain spaces or control characters"));
    }
    let url = Url::parse(text).map_err(|_| format!("{S} is not a URL"))?;
    if url.scheme() != "https" {
        return Err(format!("{S} must use https"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(format!("{S} must not carry user information"));
    }
    if url.query().is_some() {
        return Err(format!("{S} must not have a query"));
    }
    if url.fragment().is_some() {
        return Err(format!("{S} must not have a fragment"));
    }
    let host = match url.host() {
        Some(url::Host::Domain(d)) => d.trim_end_matches('.').to_ascii_lowercase(),
        Some(_) => {
            return Err(format!(
                "{S} names its host by an IP address; give its DNS name (an IP literal would \
                 skip the hosted resolver's checks)"
            ))
        }
        None => return Err(format!("{S} has no host")),
    };
    if host.is_empty() || host.parse::<IpAddr>().is_ok() || host.split('.').any(str::is_empty) {
        return Err(format!("{S} has no usable DNS name"));
    }
    let port = match url.port() {
        None | Some(443) => String::new(),
        Some(p) => format!(":{p}"),
    };
    let path = url.path().trim_end_matches('/');
    let normalised = format!("https://{host}{port}{path}");
    // The request is built from this same text: it must parse back to the same host, port and
    // path, so no difference between two readings can make the digest match one host and the
    // request go to another.
    let again = Url::parse(&normalised).map_err(|_| format!("{S} doesn't normalise"))?;
    if again.host_str() != Some(host.as_str())
        || again.port().unwrap_or(443) != url.port().unwrap_or(443)
        || again.path().trim_end_matches('/') != path
    {
        return Err(format!("{S} doesn't normalise to one reading"));
    }
    Ok((again, normalised))
}

/// A model id (spec §3.6): 1 to 128 characters from `A-Z a-z 0-9 . _ : / -`, exact bytes.
pub fn valid_model_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'/' | b'-'))
}

/// A `host:<ref>` reference: lowercase letters, digits and hyphens.
pub fn valid_ref(r: &str) -> bool {
    !r.is_empty()
        && r.len() <= 32
        && r.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// `PMOS_GATEWAY_HOSTED_MODELS`: `<ref>=<model id>` pairs separated by commas. A malformed
/// pair is refused by its ref (or its place), never by its id.
pub fn parse_hosted_models(text: &str) -> Result<BTreeMap<String, String>, String> {
    const S: &str = "PMOS_GATEWAY_HOSTED_MODELS";
    let mut out = BTreeMap::new();
    for (i, pair) in text.split(',').map(str::trim).enumerate() {
        if pair.is_empty() {
            continue;
        }
        let Some((r, id)) = pair.split_once('=') else {
            return Err(format!("{S}: entry {} is not `<ref>=<model id>`", i + 1));
        };
        if !valid_ref(r) {
            return Err(format!(
                "{S}: entry {}: the ref must be lowercase letters, digits and hyphens",
                i + 1
            ));
        }
        if !valid_model_id(id) {
            return Err(format!(
                "{S}: `{r}`: the model id must be 1 to 128 characters from A-Z a-z 0-9 . _ : / -"
            ));
        }
        if out.insert(r.to_string(), id.to_string()).is_some() {
            return Err(format!("{S}: `{r}` is listed twice"));
        }
    }
    Ok(out)
}

/// What the host gives a `hosted` gateway (read once at start).
#[derive(Clone, Default)]
pub struct HostedEnv {
    /// `PMOS_GATEWAY_HOSTED_URL`.
    pub url: Option<String>,
    /// `PMOS_GATEWAY_HOSTED_MODELS`.
    pub models: Option<String>,
    /// `PMOS_GATEWAY_HOSTED_KEY_FILE`.
    pub key_file: Option<PathBuf>,
    /// `PMOS_HOSTED_MAX_DATA_CLASS`: may only lower the entry's ceiling.
    pub max_data_class: Option<String>,
    /// `PMOS_GATEWAY_CA_FILE`: a run-time CA bundle, only with `tls_interception: allowed`.
    pub ca_file: Option<PathBuf>,
    /// Whether `SSL_CERT_FILE` or `SSL_CERT_DIR` is in the environment (refused under
    /// `hosted`: a sign of a CA meant for the hosted client, which trusts only compiled roots).
    pub ssl_cert_env: bool,
}

impl std::fmt::Debug for HostedEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Neither the URL nor a model id is printed.
        f.debug_struct("HostedEnv")
            .field("url", &self.url.as_ref().map(|_| "<set>"))
            .field("models", &self.models.as_ref().map(|_| "<set>"))
            .field("key_file", &self.key_file.as_ref().map(|_| "<set>"))
            .field("max_data_class", &self.max_data_class)
            .field("ca_file", &self.ca_file.as_ref().map(|_| "<set>"))
            .field("ssl_cert_env", &self.ssl_cert_env)
            .finish()
    }
}

impl HostedEnv {
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Self {
        let set = |k: &str| get(k).filter(|v| !v.is_empty());
        Self {
            url: set("PMOS_GATEWAY_HOSTED_URL"),
            models: set("PMOS_GATEWAY_HOSTED_MODELS"),
            key_file: set("PMOS_GATEWAY_HOSTED_KEY_FILE").map(PathBuf::from),
            max_data_class: set("PMOS_HOSTED_MAX_DATA_CLASS"),
            ca_file: set("PMOS_GATEWAY_CA_FILE").map(PathBuf::from),
            ssl_cert_env: get("SSL_CERT_FILE").is_some() || get("SSL_CERT_DIR").is_some(),
        }
    }
}

/// A `hosted` gateway's checked binding to its entry: what requests are built from and what
/// `/readyz` reports.
#[derive(Clone)]
pub struct Binding {
    pub name: String,
    pub list_kind: ListKind,
    pub list_sha256: String,
    /// The normalised URL every request is built from.
    pub url: String,
    /// The host in `url`, the only name the hosted resolver checks.
    pub host: String,
    pub key_file: PathBuf,
    /// The effective ceiling: the entry's, or `PMOS_HOSTED_MAX_DATA_CLASS` when lower.
    pub max_data_class: Sensitivity,
    pub tls_interception: TlsInterception,
    /// With `tls_interception: allowed` and a CA file: the bundle's certificates and its
    /// SHA-256.
    pub extra_ca: Option<(Vec<reqwest::Certificate>, String)>,
    /// The stop file's directory ([`CONTROL_DIR`] in the gateway; a temporary one in tests).
    pub control_dir: PathBuf,
    pub build: BuildInfo,
}

impl std::fmt::Debug for Binding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Binding")
            .field("name", &self.name)
            .field("list_kind", &self.list_kind)
            .field("max_data_class", &self.max_data_class)
            .field("tls_interception", &self.tls_interception)
            .finish_non_exhaustive()
    }
}

impl Binding {
    /// What every answer reports: `hosted:<name>`.
    pub fn source(&self) -> String {
        format!("hosted:{}", self.name)
    }

    /// Whether the stop file is there now (a `stat`, never cached). A directory that can't be
    /// read counts as stopped: fail closed.
    pub fn stopped(&self) -> bool {
        stopped(&self.control_dir)
    }
}

/// Whether `dir` holds the stop file, or isn't a readable directory (fail closed).
pub fn stopped(dir: &Path) -> bool {
    match std::fs::metadata(dir) {
        Ok(m) if m.is_dir() => std::fs::symlink_metadata(dir.join(STOP_FILE)).is_ok(),
        _ => true,
    }
}

/// Every start-up check of `hosted` against `list` (spec §3.6, "The gateway refuses to start"),
/// then binds each hosted role to the checked URL and model id. Each refusal names the
/// setting and the role or `<ref>`, never the URL or a model id.
///
/// The class, `PMOS_GATEWAY_CLOUD_TEST`, the upstream form and the role labels are
/// `Config::check_source`'s; this is the list, the digests, the key file, the CA, the stop file
/// and the ceiling.
pub fn bind(
    models: &mut Models,
    list: &ApprovedList,
    env: &HostedEnv,
    control_dir: &Path,
    build: BuildInfo,
) -> Result<Binding, String> {
    let Some(entry) = list.entries.first() else {
        return Err(
            "PMOS_MODEL_SOURCE=hosted: there is no approved entry on this gateway's compiled \
             list (crates/gateway/approved-hosted.yaml ships empty); the first entry is slice \
             H13, added only by the owner with a recorded approval (spec §3.6, §3.9). Use \
             `local`, or `hosted-test` with fake data"
                .into(),
        );
    };
    // The one hosted name the config uses.
    let mut names: Vec<String> = models
        .roles
        .values()
        .flat_map(|r| r.hops())
        .filter(|h| h.kind == UpstreamKind::Hosted)
        .filter_map(|h| h.upstream.strip_prefix("hosted:").map(String::from))
        .collect();
    names.sort();
    names.dedup();
    let [name] = names.as_slice() else {
        return Err(
            "PMOS_MODEL_SOURCE=hosted: the gateway config must name exactly one `hosted:<name>` \
             upstream"
                .into(),
        );
    };
    if name != &entry.name {
        return Err(format!(
            "PMOS_MODEL_SOURCE=hosted: `hosted:{name}` is not on this gateway's compiled \
             approved list"
        ));
    }
    for r in models.roles.values() {
        if r.label.as_deref() == Some(entry.name.as_str()) {
            return Err(format!(
                "PMOS_MODEL_SOURCE=hosted: role `{}`'s label equals the approved entry's name; \
                 a role label must never read as naming an approved provider",
                r.name
            ));
        }
    }
    if env.ssl_cert_env {
        return Err(
            "PMOS_MODEL_SOURCE=hosted: SSL_CERT_FILE or SSL_CERT_DIR is set; the hosted client \
             trusts only its compiled-in roots, so a CA meant for it is refused (unset both)"
                .into(),
        );
    }
    let extra_ca = match (&env.ca_file, entry.tls_interception) {
        (Some(_), TlsInterception::Refused) => {
            return Err(
                "PMOS_MODEL_SOURCE=hosted: PMOS_GATEWAY_CA_FILE is set, but the approved entry \
                 says `tls_interception: refused`: no run-time CA"
                    .into(),
            )
        }
        (Some(path), TlsInterception::Allowed) => {
            let pem = std::fs::read(path)
                .map_err(|_| "PMOS_GATEWAY_CA_FILE can't be read".to_string())?;
            let certs = reqwest::Certificate::from_pem_bundle(&pem)
                .map_err(|_| "PMOS_GATEWAY_CA_FILE is not a PEM certificate bundle".to_string())?;
            if certs.is_empty() {
                return Err("PMOS_GATEWAY_CA_FILE holds no certificate".into());
            }
            Some((certs, sha256_bytes(&pem)))
        }
        (None, _) => None,
    };
    let (_, url) = normalise_url(env.url.as_deref().unwrap_or_default())?;
    if sha256_hex(&url) != entry.url_sha256 {
        return Err(
            "PMOS_MODEL_SOURCE=hosted: PMOS_GATEWAY_HOSTED_URL is not the approved entry's URL \
             (its digest differs from url_sha256)"
                .into(),
        );
    }
    let host = Url::parse(&url)
        .ok()
        .and_then(|u| u.host_str().map(String::from))
        .unwrap_or_default();
    let ids = parse_hosted_models(env.models.as_deref().unwrap_or_default())?;
    let max_data_class = match env.max_data_class.as_deref() {
        None => entry.max_data_class,
        Some(c) => {
            let c = Sensitivity::parse(c).ok_or(
                "PMOS_HOSTED_MAX_DATA_CLASS must be public, internal or restricted".to_string(),
            )?;
            if c > entry.max_data_class {
                return Err(format!(
                    "PMOS_HOSTED_MAX_DATA_CLASS ({c}) is above the approved entry's \
                     max_data_class ({}); it can only lower it",
                    entry.max_data_class
                ));
            }
            c
        }
    };
    let Some(key_file) = env.key_file.clone() else {
        return Err("PMOS_MODEL_SOURCE=hosted needs PMOS_GATEWAY_HOSTED_KEY_FILE".into());
    };
    match crate::keys::KeyStore::default().get(&key_file) {
        Ok(_) => {}
        Err(why) => {
            return Err(format!(
                "PMOS_MODEL_SOURCE=hosted: the hosted model's key file \
                 (PMOS_GATEWAY_HOSTED_KEY_FILE) is {}; `pmos-admin hosted-key set` writes it",
                why.as_str()
            ))
        }
    }
    match std::fs::metadata(control_dir) {
        Ok(m) if m.is_dir() => {}
        _ => {
            return Err(format!(
                "PMOS_MODEL_SOURCE=hosted: the stop file's directory is not mounted at {} \
                 (`sudo pmos-admin hosted init`, then deploy/compose.hosted.yaml mounts it)",
                control_dir.display()
            ))
        }
    }
    if stopped(control_dir) {
        return Err(
            "PMOS_MODEL_SOURCE=hosted: the hosted model is stopped (the stop file is present); \
             `sudo pmos-admin hosted start --reason \"...\"` clears it"
                .into(),
        );
    }
    // Every hosted role's `host:<ref>`, looked up and hashed; then bound.
    for role in models.roles.values_mut() {
        if role.kind != UpstreamKind::Hosted {
            continue;
        }
        let Some(r) = role.model.strip_prefix("host:").map(String::from) else {
            return Err(format!(
                "PMOS_MODEL_SOURCE=hosted: role `{}`'s model must be `host:<ref>`",
                role.name
            ));
        };
        let Some(id) = ids.get(&r) else {
            return Err(format!(
                "PMOS_MODEL_SOURCE=hosted: role `{}` names `host:{r}`, which \
                 PMOS_GATEWAY_HOSTED_MODELS doesn't list",
                role.name
            ));
        };
        if !entry.model_sha256.contains(&sha256_hex(id)) {
            return Err(format!(
                "PMOS_MODEL_SOURCE=hosted: role `{}`: the model id for `host:{r}` is not one \
                 the approved entry allows (its digest is in no model_sha256)",
                role.name
            ));
        }
        role.upstream = url.clone();
        role.model = id.clone();
        role.key_file = Some(key_file.clone());
    }
    Ok(Binding {
        name: entry.name.clone(),
        list_kind: list.kind,
        list_sha256: list.sha256.clone(),
        url,
        host,
        key_file,
        max_data_class,
        tls_interception: entry.tls_interception,
        extra_ca,
        control_dir: control_dir.to_path_buf(),
        build,
    })
}

/// Lowercase hex SHA-256 of bytes.
pub fn sha256_bytes(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// How the hosted resolver looks names up: the system resolver in the gateway, a fixed answer
/// in tests.
pub type Lookup = Arc<
    dyn Fn(String) -> Pin<Box<dyn Future<Output = std::io::Result<Vec<IpAddr>>> + Send>>
        + Send
        + Sync,
>;

fn system_lookup() -> Lookup {
    Arc::new(|name: String| {
        Box::pin(async move {
            let addrs = tokio::net::lookup_host((name.as_str(), 0)).await?;
            Ok(addrs.map(|a| a.ip()).collect())
        })
    })
}

/// The hosted client's resolver (spec §3.6, "Egress"): at every connection, for the hosted
/// host it refuses any answer that is not a public unicast address ([`public_address`]): so a
/// DNS answer can't send the key and the prompts to a service inside the host or the VPC. Any
/// other name the client resolves is the outbound proxy's (no redirect is followed), looked up
/// as the system does.
#[derive(Clone)]
pub struct HostedResolver {
    host: String,
    lookup: Lookup,
    /// Only for the test-only list and tests: a mock on loopback or the compose network.
    allow_private: bool,
}

impl HostedResolver {
    /// The gateway's: the system resolver, public addresses only.
    pub fn strict(host: &str) -> Self {
        Self {
            host: host.to_string(),
            lookup: system_lookup(),
            allow_private: false,
        }
    }

    /// With `lookup`, and private answers accepted when `allow_private` (only a gateway built
    /// with the `stack-test-list` feature, and tests).
    pub fn with_lookup(host: &str, lookup: Lookup, allow_private: bool) -> Self {
        Self {
            host: host.to_string(),
            lookup,
            allow_private,
        }
    }

    /// The addresses to connect to for `name`, or why not (no address in the text).
    pub async fn resolve_name(&self, name: &str) -> Result<Vec<IpAddr>, String> {
        let answers = (self.lookup)(name.to_string())
            .await
            .map_err(|_| "the hosted model's host did not resolve".to_string())?;
        if !name.eq_ignore_ascii_case(&self.host) {
            return Ok(answers);
        }
        if answers.is_empty() {
            return Err("the hosted model's host resolved to no address".into());
        }
        if !self.allow_private && answers.iter().any(|a| !public_address(*a)) {
            return Err(
                "the hosted model's host resolved to a loopback, link-local, private, metadata \
                 or otherwise non-public address; refused"
                    .into(),
            );
        }
        Ok(answers)
    }
}

impl Resolve for HostedResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let this = self.clone();
        let name = name.as_str().to_string();
        Box::pin(async move {
            match this.resolve_name(&name).await {
                Ok(ips) => {
                    let addrs: Addrs = Box::new(ips.into_iter().map(|ip| SocketAddr::new(ip, 0)));
                    Ok(addrs)
                }
                Err(why) => {
                    // The entry's kind only, never the host or an address.
                    tracing::warn!(upstream = "hosted", "{why}");
                    Err(why.into())
                }
            }
        })
    }
}

/// Whether `a` is a public unicast address: not loopback, unspecified, link-local, private
/// (RFC 1918, IPv6 unique-local), shared (`100.64.0.0/10`, which also holds a cloud's
/// `100.100.100.200`), a metadata address (`169.254.169.254`, `fd00:ec2::254`), multicast,
/// broadcast, documentation, benchmarking or reserved. An IPv4-mapped IPv6 answer is judged as
/// its IPv4 address.
pub fn public_address(a: IpAddr) -> bool {
    match a {
        IpAddr::V4(v4) => public_v4(v4),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => public_v4(v4),
            None => public_v6(v6),
        },
    }
}

fn public_v4(a: Ipv4Addr) -> bool {
    let o = a.octets();
    !(a.is_loopback()
        || a.is_unspecified()
        || a.is_private()
        || a.is_link_local()
        || a.is_multicast()
        || a.is_broadcast()
        || a.is_documentation()
        || o[0] == 0
        || (o[0] == 100 && (o[1] & 0xc0) == 64)
        || (o[0] == 192 && o[1] == 0 && o[2] == 0)
        || (o[0] == 198 && (o[1] & 0xfe) == 18)
        || o[0] >= 240)
}

fn public_v6(a: Ipv6Addr) -> bool {
    let s = a.segments();
    !(a.is_loopback()
        || a.is_unspecified()
        || a.is_multicast()
        || (s[0] & 0xfe00) == 0xfc00 // unique local, fd00:ec2::254 included
        || (s[0] & 0xffc0) == 0xfe80 // link-local
        || (s[0] & 0xffc0) == 0xfec0 // site-local (deprecated)
        || (s[0] == 0x2001 && s[1] == 0x0db8) // documentation
        || (s[0] == 0x0064 && s[1] == 0xff9b) // NAT64, which reaches IPv4 inside
        || s[0] == 0x2002 // 6to4, likewise
        || (s[0] == 0x2001 && s[1] == 0)) // Teredo
}

/// The roots the hosted client trusts (spec §3.6, "Which roots the hosted client trusts"):
/// Mozilla's, compiled in from `webpki-root-certs`, never the system store.
pub fn compiled_roots() -> Vec<reqwest::Certificate> {
    webpki_root_certs::TLS_SERVER_ROOT_CERTS
        .iter()
        .filter_map(|c| reqwest::Certificate::from_der(c.as_ref()).ok())
        .collect()
}

/// How the hosted clients are built. The gateway's is [`HostedNet::strict`]; tests replace
/// the roots, the lookup and the proxy.
#[derive(Clone)]
pub struct HostedNet {
    /// The roots in place of [`compiled_roots`] (tests: a test CA, standing in for Mozilla's).
    pub roots: Option<Vec<reqwest::Certificate>>,
    pub lookup: Option<Lookup>,
    pub allow_private: bool,
    /// Ignore `HTTPS_PROXY` (tests, whose mock is reached directly).
    pub no_proxy: bool,
}

impl HostedNet {
    pub fn strict() -> Self {
        Self {
            roots: None,
            lookup: None,
            allow_private: false,
            no_proxy: false,
        }
    }
}

/// One hosted client: no redirects, the hosted resolver, and only the compiled-in roots (plus
/// the run-time bundle when the entry allows interception). `whole`: the client for answers
/// that arrive whole (a total timeout), else for streams (a silence timeout).
pub fn client(
    binding: &Binding,
    net: &HostedNet,
    timeouts: &crate::app::Timeouts,
    whole: bool,
) -> reqwest::Client {
    let mut roots = net.roots.clone().unwrap_or_else(compiled_roots);
    if let Some((extra, _)) = &binding.extra_ca {
        roots.extend(extra.iter().cloned());
    }
    let resolver = match &net.lookup {
        Some(l) => HostedResolver::with_lookup(&binding.host, l.clone(), net.allow_private),
        None if net.allow_private => {
            HostedResolver::with_lookup(&binding.host, system_lookup(), true)
        }
        None => HostedResolver::strict(&binding.host),
    };
    let mut b = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .https_only(true)
        .tls_certs_only(roots)
        .dns_resolver(Arc::new(resolver));
    if net.no_proxy {
        b = b.no_proxy();
    }
    b = if whole {
        b.connect_timeout(std::time::Duration::from_secs(10))
            .timeout(timeouts.whole_answer)
    } else {
        b.connect_timeout(std::time::Duration::from_secs(5))
            .read_timeout(timeouts.stream_silence)
    };
    b.build().expect("the hosted client builds")
}

/// `pmos-gateway --digest-from-stdin` (spec §3.6, "A reviewer can check an entry's digests"):
/// the digest of one URL read from standard input, normalised exactly as the gateway does.
/// One trailing newline is dropped; anything else the gateway refuses is refused, with its
/// message. The URL never reaches a command line or a log.
pub fn url_digest_from(input: &str) -> Result<String, String> {
    let text = input.strip_suffix('\n').unwrap_or(input);
    let text = text.strip_suffix('\r').unwrap_or(text);
    normalise_url(text).map(|(_, n)| sha256_hex(&n))
}

/// `pmos-gateway --model-digest-from-stdin`: the digest of one model id from standard input,
/// its exact bytes after one trailing newline is dropped.
pub fn model_digest_from(input: &str) -> Result<String, String> {
    let text = input.strip_suffix('\n').unwrap_or(input);
    let text = text.strip_suffix('\r').unwrap_or(text);
    if !valid_model_id(text) {
        return Err(
            "a model id is 1 to 128 characters from A-Z a-z 0-9 . _ : / - (nothing else)".into(),
        );
    }
    Ok(sha256_hex(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    const D: &str = "0000000000000000000000000000000000000000000000000000000000000000";

    fn entry(name: &str) -> String {
        format!(
            "- name: {name}\n  url_sha256: \"{D}\"\n  model_sha256: [\"{D}\"]\n  record_sha256: \
             \"{D}\"\n  pull: 7\n"
        )
    }

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    /// The list's own rules for one list file, as the shipped list's test applies them: at most
    /// one entry (by parsing), each well formed, no role label of `deploy/gateway.hosted.yaml`
    /// equal to an entry name, and any entry only with `.github/owner-handle` and
    /// `.github/CODEOWNERS` holding the owner-only lines (r7, X2; the same parser as the lint).
    fn check_list_for_repo(text: &str, root: &Path) -> Result<ApprovedList, Vec<String>> {
        let list = parse_list(ListKind::Release, text).map_err(|e| vec![e])?;
        if list.entries.is_empty() {
            return Ok(list);
        }
        pmos_contract::codeowners::hosted_list_may_have_entries(root)?;
        let cfg = std::fs::read_to_string(root.join("deploy/gateway.hosted.yaml"))
            .map_err(|e| vec![format!("deploy/gateway.hosted.yaml: {e}")])?;
        let models = Models::from_yaml(&cfg).map_err(|e| vec![e])?;
        for e in &list.entries {
            if models
                .roles
                .values()
                .any(|r| r.label.as_deref() == Some(&e.name))
            {
                return Err(vec![format!(
                    "a role label equals the entry name `{}`",
                    e.name
                )]);
            }
        }
        Ok(list)
    }

    /// The shipped list's own test (spec §3.6): it parses, has at most one entry, each well
    /// formed, and has none unless the CODEOWNERS rule holds. As shipped it is empty.
    #[test]
    fn the_shipped_list_passes_its_own_rules_and_is_empty() {
        let list = check_list_for_repo(RELEASE_LIST, &repo_root()).unwrap();
        assert!(list.entries.len() <= 1);
        // Slice H12 ships it empty; H13 (the owner's) adds the first entry, and that pull
        // request changes this line together with the CODEOWNERS files.
        assert!(
            list.entries.is_empty(),
            "the release list ships empty (H12)"
        );
        // Never a URL, a host or a key: only names, digests and links.
        for line in RELEASE_LIST
            .lines()
            .filter(|l| !l.trim_start().starts_with('#'))
        {
            assert!(!line.contains("://") && !line.contains("key"), "{line}");
        }
    }

    #[test]
    fn a_one_entry_list_needs_the_codeowners_lines() {
        let one = entry("owner-hosted");
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("deploy")).unwrap();
        std::fs::copy(
            repo_root().join("deploy/gateway.hosted.yaml"),
            root.join("deploy/gateway.hosted.yaml"),
        )
        .unwrap();
        // Without .github/CODEOWNERS and .github/owner-handle: refused.
        assert!(check_list_for_repo(&one, root).is_err());
        std::fs::create_dir_all(root.join(".github")).unwrap();
        std::fs::write(root.join(".github/owner-handle"), "@the-owner\n").unwrap();
        // With the handle but without the lines: refused.
        std::fs::write(
            root.join(".github/CODEOWNERS"),
            "/crates/gateway/ @org/team\n",
        )
        .unwrap();
        assert!(check_list_for_repo(&one, root).is_err());
        let lines: String = pmos_contract::codeowners::OWNER_ONLY_PATHS
            .iter()
            .map(|p| format!("{p} @the-owner\n"))
            .collect();
        std::fs::write(
            root.join(".github/CODEOWNERS"),
            format!("/crates/gateway/ @org/team\n{lines}"),
        )
        .unwrap();
        assert_eq!(check_list_for_repo(&one, root).unwrap().entries.len(), 1);
        // The empty list needs neither.
        let bare = tempfile::tempdir().unwrap();
        assert!(check_list_for_repo("# empty\n", bare.path()).is_ok());
        // A role label equal to the entry name is refused.
        let cfg = std::fs::read_to_string(root.join("deploy/gateway.hosted.yaml")).unwrap();
        let label = Models::from_yaml(&cfg)
            .unwrap()
            .roles
            .values()
            .find_map(|r| r.label.clone())
            .unwrap();
        assert!(check_list_for_repo(&entry(&label), root).is_err());
    }

    #[test]
    fn entries_are_checked_field_by_field() {
        assert_eq!(parse_list(ListKind::Release, "").unwrap().entries.len(), 0);
        assert_eq!(
            parse_list(ListKind::Release, "[]\n").unwrap().entries.len(),
            0
        );
        let ok = parse_list(ListKind::Release, &entry("owner-hosted")).unwrap();
        let e = &ok.entries[0];
        assert_eq!(e.max_data_class, Sensitivity::Restricted);
        assert_eq!(e.tls_interception, TlsInterception::Refused);
        // Two entries.
        let two = format!("{}{}", entry("a"), entry("b"));
        assert!(parse_list(ListKind::Release, &two)
            .unwrap_err()
            .contains("at most one"));
        let bad = |from: &str, to: &str| {
            parse_list(ListKind::Release, &entry("owner-hosted").replace(from, to)).is_err()
        };
        assert!(bad("owner-hosted", "Owner"));
        assert!(bad("owner-hosted", "stack-test"));
        assert!(bad(&format!("url_sha256: \"{D}\""), "url_sha256: \"abc\""));
        assert!(bad(
            &format!("record_sha256: \"{D}\""),
            "record_sha256: \"\""
        ));
        assert!(bad(&format!("model_sha256: [\"{D}\"]"), "model_sha256: []"));
        assert!(bad(
            &format!("model_sha256: [\"{D}\"]"),
            "model_sha256: [\"ABC\"]"
        ));
        assert!(bad("pull: 7", "pull: 0"));
        assert!(bad("pull: 7", "pull: -1"));
        assert!(bad("pull: 7", "pull: x"));
        assert!(bad("pull: 7\n", ""));
        assert!(bad("pull: 7", "pull: 7\n  max_data_class: secret"));
        assert!(bad("pull: 7", "pull: 7\n  tls_interception: maybe"));
        // A field that isn't one of the list's (a URL, say) is refused.
        assert!(bad("pull: 7", "pull: 7\n  url: \"https://x.example\""));
        let ceil = entry("owner-hosted").replace(
            "pull: 7",
            "pull: 7\n  max_data_class: internal\n  tls_interception: allowed",
        );
        let e = parse_list(ListKind::Release, &ceil).unwrap().entries[0].clone();
        assert_eq!(e.max_data_class, Sensitivity::Internal);
        assert_eq!(e.tls_interception, TlsInterception::Allowed);
        // The test-only list takes `stack-test` only.
        assert!(parse_list(ListKind::StackTest, &entry("owner-hosted")).is_err());
        assert!(parse_list(ListKind::StackTest, &entry("stack-test")).is_ok());
    }

    #[test]
    fn the_stack_test_list_names_only_the_mock() {
        let list = parse_list(ListKind::StackTest, STACK_TEST_LIST).unwrap();
        let [e] = list.entries.as_slice() else {
            panic!("one entry")
        };
        assert_eq!(e.name, STACK_TEST_NAME);
        let root = repo_root();
        let url = std::fs::read_to_string(root.join("deploy/test/hosted-stack-test.url")).unwrap();
        assert_eq!(url_digest_from(&url).unwrap(), e.url_sha256);
        let model =
            std::fs::read_to_string(root.join("deploy/test/hosted-stack-test.model")).unwrap();
        assert_eq!(vec![model_digest_from(&model).unwrap()], e.model_sha256);
        let record = std::fs::read(root.join("deploy/test/hosted-stack-test-record.md")).unwrap();
        assert_eq!(sha256_bytes(&record), e.record_sha256);
        assert!(
            url.contains(".invalid") || url.contains(".test"),
            "a mock's name only"
        );
        // Without the feature the build carries the release list.
        if !cfg!(feature = "stack-test-list") {
            assert_eq!(compiled().unwrap().kind, ListKind::Release);
        }
    }

    #[test]
    fn urls_normalise_to_one_digest() {
        let n = |s: &str| normalise_url(s).map(|(_, n)| n);
        assert_eq!(
            n("https://Models.Example.TEST/v1/").unwrap(),
            "https://models.example.test/v1"
        );
        assert_eq!(
            n("https://models.example.test:443/v1").unwrap(),
            "https://models.example.test/v1"
        );
        assert_eq!(
            n("https://models.example.test./v1").unwrap(),
            "https://models.example.test/v1"
        );
        assert_eq!(
            n("https://models.example.test:8443/a/b/").unwrap(),
            "https://models.example.test:8443/a/b"
        );
        assert_eq!(
            n("https://models.example.test").unwrap(),
            "https://models.example.test"
        );
        // An internationalised name hashes in its ASCII form.
        assert_eq!(
            n("https://bücher.example.test/v1").unwrap(),
            "https://xn--bcher-kva.example.test/v1"
        );
        assert_eq!(
            url_digest_from("https://MODELS.example.test./v1/\n").unwrap(),
            sha256_hex("https://models.example.test/v1")
        );
        for bad in [
            "",
            "http://models.example.test/v1",
            "https://user:pw@models.example.test/v1",
            "https://user@models.example.test/v1",
            "https://models.example.test/v1?x=1",
            "https://models.example.test/v1#f",
            "https://127.0.0.1/v1",
            "https://[::1]/v1",
            "https://10.0.0.1./v1",
            "https://0x7f000001/v1",
            " https://models.example.test/v1",
            "https://models.example.test/v1 ",
            "ftp://models.example.test",
            "models.example.test/v1",
        ] {
            let err = n(bad).unwrap_err();
            assert!(!err.contains("example") && !err.contains("127.0"), "{err}");
        }
    }

    #[test]
    fn model_ids_and_refs_are_checked_without_naming_the_id() {
        let m = parse_hosted_models("main=vendor/model-1.5:latest, fast=small_2").unwrap();
        assert_eq!(m["main"], "vendor/model-1.5:latest");
        assert_eq!(m["fast"], "small_2");
        for bad in [
            "main",
            "Main=x",
            "main=a b",
            "main=x,main=y",
            "main=",
            &format!("main={}", "a".repeat(129)),
        ] {
            let err = parse_hosted_models(bad).unwrap_err();
            assert!(!err.contains("a b") && !err.contains("aaaa"), "{err}");
        }
        assert_eq!(model_digest_from("m-1\n").unwrap(), sha256_hex("m-1"));
        assert!(model_digest_from("m 1").is_err());
        assert!(model_digest_from("").is_err());
    }

    #[test]
    fn the_hosted_resolver_takes_public_answers_only() {
        for public in ["1.1.1.1", "8.8.4.4", "2606:4700::1111", "::ffff:1.1.1.1"] {
            assert!(public_address(public.parse().unwrap()), "{public}");
        }
        for private in [
            "127.0.0.1",
            "0.0.0.0",
            "10.201.77.13",
            "10.0.0.1",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "169.254.170.2",
            "100.64.0.1",
            "100.100.100.200",
            "224.0.0.1",
            "255.255.255.255",
            "192.0.0.1",
            "198.18.0.1",
            "240.0.0.1",
            "::1",
            "::",
            "fd00:ec2::254",
            "fc00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "64:ff9b::a00:1",
            "2002:a00:1::",
            "2001:db8::1",
        ] {
            assert!(!public_address(private.parse().unwrap()), "{private}");
        }
    }

    #[tokio::test]
    async fn the_resolver_checks_the_hosted_host_and_names_no_address() {
        let fixed = |ips: Vec<IpAddr>| -> Lookup {
            Arc::new(move |_| {
                let ips = ips.clone();
                Box::pin(async move { Ok(ips) })
            })
        };
        let host = "models.example.test";
        let one = |ip: &str| vec![ip.parse::<IpAddr>().unwrap()];
        let r = HostedResolver::with_lookup(host, fixed(one("1.1.1.1")), false);
        assert!(r.resolve_name(host).await.is_ok());
        for bad in [
            "127.0.0.1",
            "10.201.77.12",
            "169.254.169.254",
            "192.168.0.1",
            "fd00:ec2::254",
        ] {
            let r = HostedResolver::with_lookup(host, fixed(one(bad)), false);
            let err = r.resolve_name(host).await.unwrap_err();
            assert!(!err.contains(bad) && !err.contains(host), "{err}");
            // One bad answer beside a good one refuses the whole answer.
            let mut two = one("1.1.1.1");
            two.extend(one(bad));
            let r = HostedResolver::with_lookup(host, fixed(two), false);
            assert!(r.resolve_name(host).await.is_err());
            // Tests and the stack-test build may take a mock's private address.
            let r = HostedResolver::with_lookup(host, fixed(one(bad)), true);
            assert!(r.resolve_name(host).await.is_ok());
        }
        let r = HostedResolver::with_lookup(host, fixed(vec![]), false);
        assert!(r.resolve_name(host).await.is_err());
        // The proxy's own name is not the hosted host: looked up as the system does.
        let r = HostedResolver::with_lookup(host, fixed(one("10.0.0.5")), false);
        assert!(r.resolve_name("proxy.corp.test").await.is_ok());
    }

    #[test]
    fn the_stop_file_is_read_every_time_and_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!stopped(dir.path()));
        std::fs::write(dir.path().join(STOP_FILE), "2026-11-24 test\n").unwrap();
        assert!(stopped(dir.path()));
        std::fs::remove_file(dir.path().join(STOP_FILE)).unwrap();
        assert!(!stopped(dir.path()));
        assert!(stopped(&dir.path().join("not-mounted")));
    }

    #[test]
    fn the_compiled_roots_are_mozillas() {
        assert!(compiled_roots().len() > 100);
    }
}
