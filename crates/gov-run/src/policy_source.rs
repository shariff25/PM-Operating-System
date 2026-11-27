//! Where the gate and the shim get their connector map and the mandate issuers' keys: from
//! a policy bundle signed by the root key (spec §7.6.1), read from a file at start.
//!
//! The newest bundle accepted is noted in a state file before it is used, and a bundle
//! older than the one noted is refused, as is another bundle under the same number. With no
//! state file any validly signed bundle is taken, an old one included: a stated limit until
//! the lease names the bundle a gate must hold (review finding GR-132).
//!
//! The unsigned files of the first steps remain for tests only, behind a flag that works
//! only for fake data, and what is recorded then says `unsigned`.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{bail, ensure, Context};
use gov_core::connector::ServerMap;
use gov_core::policy::{may_replace, open_bundle, HeldBundle};
use gov_core::{Envelope, Error, Id, PublicKey, Scope};

/// What the unsigned path writes as the policy version in every receipt.
pub const UNSIGNED: &str = "unsigned";

/// The largest bundle or key file read.
const MAX_FILE: u64 = 16 * 1024 * 1024;

/// What a program needs from policy.
#[derive(Debug, Clone)]
pub struct Policy {
    /// The connector map of the one server this gate stands in front of.
    pub map: ServerMap,
    /// The keys mandates are issued under.
    pub mandate_issuers: Vec<PublicKey>,
    /// The bundle's templates, which every mandate is held to. `None` on the unsigned
    /// path: no bundle, so nothing to hold a mandate to, and the receipts say `unsigned`.
    pub templates: Option<Vec<gov_core::mandate::Template>>,
    /// What every receipt carries: the bundle's number and the start of its digest, or
    /// `unsigned`.
    pub version: Id,
    /// On the signed path: what a program needs to take a later bundle by the same rules
    /// while it runs (spec §7.6.3, point 7).
    pub signed: Option<Signed>,
}

/// The signed bundle a program started with, and how it would check the next.
#[derive(Debug, Clone)]
pub struct Signed {
    /// The bundle as the root key signed it.
    pub envelope: Envelope,
    /// The one key a bundle is opened under.
    pub root: PublicKey,
    pub held: HeldBundle,
    /// Where the newest bundle accepted is noted, before it is used.
    pub state: PathBuf,
    /// The server whose map this program uses.
    pub server: Id,
}

/// The options both programs take.
#[derive(clap::Args, Debug, Clone)]
pub struct PolicyArgs {
    /// The signed policy bundle.
    #[arg(long)]
    pub bundle: Option<PathBuf>,
    /// The root key's public half, as JSON: the one key a bundle is opened under.
    #[arg(long)]
    pub root_key: Option<PathBuf>,
    /// Where the number and digest of the newest bundle accepted are kept. A bundle older
    /// than the one noted there is refused.
    #[arg(long)]
    pub bundle_state: Option<PathBuf>,
    /// The server, as the bundle's connector maps name it.
    #[arg(long)]
    pub server: Option<String>,
    /// Take the connector map, and the mandate issuers' keys, from unsigned files. For tests
    /// with fake data only; every receipt then says the policy was unsigned.
    #[arg(long)]
    pub unsigned_policy_for_tests: bool,
    /// With --unsigned-policy-for-tests: the connector map, as JSON.
    #[arg(long)]
    pub map: Option<PathBuf>,
    /// With --unsigned-policy-for-tests: the mandate issuers' public keys, as a JSON list.
    #[arg(long)]
    pub mandate_issuers: Option<PathBuf>,
}

fn read<T: serde::de::DeserializeOwned>(path: &Path, what: &str) -> anyhow::Result<T> {
    let meta = std::fs::metadata(path)
        .with_context(|| format!("{what} ({}) cannot be read", path.display()))?;
    ensure!(
        meta.is_file() && meta.len() <= MAX_FILE,
        "{what} is not a file of a sane size"
    );
    let bytes = std::fs::read(path).with_context(|| format!("{what} cannot be read"))?;
    let value = gov_core::strict::parse(&bytes)
        .map_err(|_| anyhow::anyhow!("{what} is not strict JSON"))?;
    serde_json::from_value(value).with_context(|| format!("{what} is not in the form expected"))
}

/// Note `held` in `path`, whole or not at all, before the bundle is used.
pub fn note(path: &Path, held: &HeldBundle) -> anyhow::Result<()> {
    let tmp = path.with_extension("tmp");
    let mut file = std::fs::File::create(&tmp)
        .with_context(|| format!("{} cannot be written", tmp.display()))?;
    file.write_all(&serde_json::to_vec(held)?)?;
    file.sync_all()?;
    std::fs::rename(&tmp, path).with_context(|| format!("{} cannot be written", path.display()))?;
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::File::open(dir)?.sync_all()?;
    }
    Ok(())
}

impl PolicyArgs {
    /// Load the policy as the options say. The signed bundle is the way; the unsigned
    /// files are refused unless asked for by name, for fake data.
    pub fn load(&self, scope: &Scope) -> anyhow::Result<Policy> {
        if self.unsigned_policy_for_tests {
            return self.unsigned();
        }
        ensure!(
            self.map.is_none() && self.mandate_issuers.is_none(),
            "--map and --mandate-issuers are unsigned files: the connector map and the issuers' keys come from the signed bundle (--bundle, --root-key, --bundle-state, --server)"
        );
        let (Some(bundle), Some(root), Some(state), Some(server)) = (
            &self.bundle,
            &self.root_key,
            &self.bundle_state,
            &self.server,
        ) else {
            bail!("--bundle, --root-key, --bundle-state and --server are all needed");
        };
        let root: PublicKey = read(root, "the root key")?;
        let envelope: Envelope = read(bundle, "the policy bundle")?;
        let opened = open_bundle(&envelope, &root, scope).map_err(|e| {
            anyhow::anyhow!("the policy bundle does not open under the root key: {e}")
        })?;
        let held: Option<HeldBundle> = if state.exists() {
            Some(read(state, "the bundle state")?)
        } else {
            None
        };
        match may_replace(held.as_ref(), &opened.held()) {
            Ok(()) => {}
            Err(Error::BundleOlder) => bail!(
                "the policy bundle is number {}, older than number {} already accepted here",
                opened.bundle.number,
                held.map_or(0, |h| h.number)
            ),
            Err(Error::BundleForked) => bail!(
                "ALARM: another policy bundle was accepted here under number {}; two bundles under one number means the root key signed twice or was copied",
                opened.bundle.number
            ),
            Err(e) => bail!("the policy bundle is refused: {e}"),
        }
        note(state, &opened.held())?;
        let map = opened
            .bundle
            .connectors
            .iter()
            .find(|m| m.server.as_str() == server)
            .cloned()
            .with_context(|| {
                format!("the policy bundle has no connector map for the server {server}")
            })?;
        Ok(Policy {
            map,
            mandate_issuers: opened.bundle.keys.mandates.clone(),
            templates: Some(opened.bundle.templates.clone()),
            version: opened.policy_version(),
            signed: Some(Signed {
                envelope,
                root,
                held: opened.held(),
                state: state.clone(),
                server: Id::new(server)
                    .map_err(|_| anyhow::anyhow!("the server's name is not an identifier"))?,
            }),
        })
    }

    fn unsigned(&self) -> anyhow::Result<Policy> {
        ensure!(
            std::env::var("GOV_DATA_CLASS").as_deref() == Ok("fake"),
            "--unsigned-policy-for-tests is for fake data only: set GOV_DATA_CLASS=fake, and put no real data through this installation"
        );
        ensure!(
            self.bundle.is_none() && self.root_key.is_none(),
            "--unsigned-policy-for-tests takes no bundle: choose one or the other"
        );
        let map = self
            .map
            .as_ref()
            .context("--unsigned-policy-for-tests needs --map")?;
        let map: ServerMap = read(map, "the connector map")?;
        let mandate_issuers = match &self.mandate_issuers {
            Some(path) => read(path, "the mandate issuers' public keys")?,
            None => Vec::new(),
        };
        Ok(Policy {
            map,
            mandate_issuers,
            templates: None,
            version: Id::new(UNSIGNED).expect("a word is an identifier"),
            signed: None,
        })
    }
}
