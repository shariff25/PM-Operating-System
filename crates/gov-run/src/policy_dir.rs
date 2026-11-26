//! The policy directory, and the bundle made from it (spec §7.6.1).
//!
//! The directory is what is reviewed in the repository:
//!
//! ```text
//! policy.json                       who the bundle is for, the online keys, the rules
//! connectors/<server>.json          one server's connector map
//! connectors/<server>.samples.json  sample calls, and the action each is to be read as
//! ```
//!
//! `check` builds the bundle and runs every check a gate would, with no key. `prepare` is
//! what the signer runs: it checks the directory itself (it never takes a built bundle),
//! reads the bundle before, and lists what changed, for the signer to read before signing.
//! The directory given to the signer comes from the reviewed repository and never from the
//! Authority's machine (review finding GR-128): the root key is kept off that machine so
//! that whoever takes it cannot forge policy, which holds only if what the root signs was
//! not written there.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{bail, ensure, Context};
use gov_core::action::Kind;
use gov_core::connector::ServerMap;
use gov_core::lease::FreshnessLimits;
use gov_core::policy::{
    changes, open_bundle, Changes, OnlineKeys, OpenedBundle, PolicyBundle, Rule,
};
use gov_core::{seal, Envelope, Id, PublicKey, ResourcePath, Scope, SigningKey};
use serde::Deserialize;
use serde_json::Value;

/// The largest file read from a policy directory.
const MAX_FILE: u64 = 4 * 1024 * 1024;

/// `policy.json`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Source {
    tenant: Id,
    env: Id,
    issuer: Id,
    version: Id,
    #[serde(default)]
    freshness: FreshnessLimits,
    keys: OnlineKeys,
    rules: Vec<Rule>,
}

/// One sample call for a connector map.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Sample {
    tool: String,
    arguments: Value,
    expect: Expect,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Expect {
    operation: Id,
    kind: Kind,
    resource: ResourcePath,
}

/// A bundle built from a directory that passed every check.
#[derive(Debug)]
pub struct Checked {
    pub bundle: PolicyBundle,
    /// How many sample calls were run, over every connector map.
    pub samples: usize,
}

/// Read one file of the directory as strict JSON: a plain file of a sane size, with no
/// repeated key.
fn read<T: serde::de::DeserializeOwned>(path: &Path) -> anyhow::Result<T> {
    let meta = std::fs::symlink_metadata(path)
        .with_context(|| format!("{} cannot be read", path.display()))?;
    ensure!(meta.is_file(), "{} is not a plain file", path.display());
    ensure!(meta.len() <= MAX_FILE, "{} is too large", path.display());
    let bytes =
        std::fs::read(path).with_context(|| format!("{} cannot be read", path.display()))?;
    let value = gov_core::strict::parse(&bytes)
        .map_err(|_| anyhow::anyhow!("{} is not strict JSON", path.display()))?;
    serde_json::from_value(value)
        .with_context(|| format!("{} is not in the form expected", path.display()))
}

/// Build the bundle from `dir` and run every check a gate would, and each connector map's
/// sample calls. Needs no key.
pub fn check(dir: &Path, number: u64, now: i64) -> anyhow::Result<Checked> {
    let source: Source = read(&dir.join("policy.json"))?;
    let mut connectors: Vec<ServerMap> = Vec::new();
    let mut samples = 0;
    let maps = dir.join("connectors");
    if maps.exists() {
        let mut names: Vec<String> = Vec::new();
        for entry in std::fs::read_dir(&maps)
            .with_context(|| format!("{} cannot be listed", maps.display()))?
        {
            let name = entry?.file_name();
            let name = name
                .to_str()
                .with_context(|| {
                    format!("a file in {} has a name that is not text", maps.display())
                })?
                .to_string();
            names.push(name);
        }
        names.sort();
        let mut sampled = BTreeSet::new();
        for name in &names {
            if let Some(server) = name.strip_suffix(".samples.json") {
                sampled.insert(server.to_string());
            } else if !name.ends_with(".json") {
                // Nothing rides along unread in what is reviewed and signed.
                bail!(
                    "{name} in {} is neither a map nor its samples",
                    maps.display()
                );
            }
        }
        for name in &names {
            if name.ends_with(".samples.json") {
                continue;
            }
            let server = name.trim_end_matches(".json");
            let map: ServerMap = read(&maps.join(name))?;
            ensure!(
                map.server.as_str() == server,
                "{name} holds the map of another server, {}",
                map.server
            );
            // Every map comes with sample calls (the standing condition of GR-65).
            ensure!(
                sampled.remove(server),
                "the connector map {name} has no {server}.samples.json beside it"
            );
            samples += run_samples(&map, &maps.join(format!("{server}.samples.json")))?;
            connectors.push(map);
        }
        if let Some(orphan) = sampled.iter().next() {
            bail!("{orphan}.samples.json has no connector map beside it");
        }
    }
    let bundle = PolicyBundle {
        v: gov_core::VERSION,
        tenant: source.tenant,
        env: source.env,
        issuer: source.issuer,
        version: source.version,
        number,
        issued_at: now,
        keys: source.keys,
        freshness: source.freshness,
        rules: source.rules,
        connectors,
    };
    bundle
        .validate()
        .map_err(|e| anyhow::anyhow!("the policy does not pass its checks: {e}"))?;
    Ok(Checked { bundle, samples })
}

/// Run a map's sample calls: every tool of the map has at least one, and each is read as
/// the action it says.
fn run_samples(map: &ServerMap, path: &Path) -> anyhow::Result<usize> {
    let samples: Vec<Sample> = read(path)?;
    let mut covered = BTreeSet::new();
    for (n, sample) in samples.iter().enumerate() {
        let at = || format!("sample {} of {}", n + 1, path.display());
        let Some(tool) = map.tools.iter().find(|t| t.tool.as_str() == sample.tool) else {
            bail!("{}: the map has no tool of that name", at());
        };
        // As a gate would, for a tool listed with the definition that was reviewed.
        let mapped = map.map(
            &sample.tool,
            Some(&tool.digest),
            &sample.arguments,
            &[0; 32],
        );
        ensure!(
            mapped.known,
            "{}: the call cannot be read as an action of this map",
            at()
        );
        let a = &mapped.action;
        ensure!(
            a.operation == sample.expect.operation
                && a.kind == sample.expect.kind
                && a.resource == sample.expect.resource,
            "{}: read as {} ({:?}) on {}, not what the sample expects",
            at(),
            a.operation,
            a.kind,
            a.resource.as_str()
        );
        covered.insert(&tool.tool);
    }
    if let Some(bare) = map.tools.iter().find(|t| !covered.contains(&t.tool)) {
        bail!(
            "{}: the tool {} has no sample call",
            path.display(),
            bare.tool
        );
    }
    Ok(samples.len())
}

/// What a signer is about to sign, and what differs from the bundle before.
#[derive(Debug)]
pub struct Prepared {
    pub bundle: PolicyBundle,
    pub changes: Changes,
    pub samples: usize,
}

/// Check `dir` and compare it with the bundle before. The first bundle is number 1 and has
/// none before it; every other needs the signed bundle before it, which must open under
/// the same root and carry the number just below.
pub fn prepare(
    dir: &Path,
    root: &PublicKey,
    number: u64,
    previous: Option<&Envelope>,
    now: i64,
) -> anyhow::Result<Prepared> {
    let checked = check(dir, number, now)?;
    let scope = Scope::new(checked.bundle.tenant.as_str(), checked.bundle.env.as_str())
        .map_err(|e| anyhow::anyhow!("the policy's tenant or environment: {e}"))?;
    checked
        .bundle
        .not_signed_by_its_own(root)
        .map_err(|e| anyhow::anyhow!("the policy does not pass its checks: {e}"))?;
    let before = match (number, previous) {
        (1, None) => None,
        (1, Some(_)) => bail!("bundle 1 is the first: there is none before it"),
        (_, None) => bail!("bundle {number} needs the signed bundle before it (--previous)"),
        (_, Some(envelope)) => {
            let opened = open_bundle(envelope, root, &scope).map_err(|e| {
                anyhow::anyhow!("the bundle before does not open under this root key: {e}")
            })?;
            ensure!(
                opened.bundle.number.checked_add(1) == Some(number),
                "the bundle before is number {}, so the next is {}, not {number}",
                opened.bundle.number,
                opened.bundle.number.saturating_add(1)
            );
            Some(opened.bundle)
        }
    };
    let changes = changes(before.as_ref(), &checked.bundle);
    Ok(Prepared {
        bundle: checked.bundle,
        changes,
        samples: checked.samples,
    })
}

/// Sign what was prepared, and open the result as a gate would before handing it back.
pub fn sign(prepared: &Prepared, root: &SigningKey) -> anyhow::Result<(Envelope, OpenedBundle)> {
    let scope = Scope::new(
        prepared.bundle.tenant.as_str(),
        prepared.bundle.env.as_str(),
    )
    .map_err(|e| anyhow::anyhow!("the policy's tenant or environment: {e}"))?;
    let envelope = seal(&prepared.bundle, root).map_err(|e| anyhow::anyhow!("signing: {e}"))?;
    let opened = open_bundle(&envelope, &root.public(), &scope)
        .map_err(|e| anyhow::anyhow!("the signed bundle does not open as one: {e}"))?;
    Ok((envelope, opened))
}

/// The lines a signer reads before signing. Identifiers only.
pub fn describe(changes: &Changes) -> Vec<String> {
    let mut out = Vec::new();
    let role = |r| match r {
        gov_core::policy::KeyRole::Mandates => "mandates",
        gov_core::policy::KeyRole::Leases => "leases",
    };
    for (r, kid) in &changes.keys_added {
        out.push(format!("key added, for {}: {kid}", role(*r)));
    }
    for (r, kid) in &changes.keys_removed {
        out.push(format!("key removed, for {}: {kid}", role(*r)));
    }
    for (what, ids) in [
        ("rule added", &changes.rules_added),
        ("rule removed", &changes.rules_removed),
        ("rule changed", &changes.rules_changed),
        ("connector map added", &changes.maps_added),
        ("connector map removed", &changes.maps_removed),
        ("connector map changed", &changes.maps_changed),
    ] {
        for id in ids {
            out.push(format!("{what}: {id}"));
        }
    }
    if changes.freshness_changed {
        out.push("the freshness limits changed".into());
    }
    if out.is_empty() {
        out.push("nothing changed but the number".into());
    }
    out
}
