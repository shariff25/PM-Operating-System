//! What the two programs share: reading a key file, and the rule about plain HTTP.
//!
//! Neither program speaks TLS yet. So by default each listens only on this machine, and
//! talks plain HTTP only to this machine: an agent's credential passes through the gate, and
//! must not cross a network in the clear because of a default. An operator who puts a TLS
//! terminator in front says so with `--plain-http-on-network`.

use std::io::Write;
use std::net::{IpAddr, SocketAddr};
use std::path::Path;

use anyhow::{bail, Context};
use gov_core::{Id, PublicKey, SigningKey};

/// Seconds since the epoch, by this machine's clock.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .unwrap_or(0)
}

/// Keys in files are for fake data only (spec §9): real data waits for a key service that is
/// not a file. So nothing here makes or reads a key file unless the operator has said the
/// data is fake.
fn file_keys_allowed() -> anyhow::Result<()> {
    if std::env::var("GOV_DATA_CLASS").as_deref() != Ok("fake") {
        bail!(
            "keys kept in files are for fake data only: set GOV_DATA_CLASS=fake, and put no real data through this installation"
        );
    }
    Ok(())
}

/// Read a private key from its file. The file must be a plain file that nobody but its owner
/// can read or write: a key any other account on the machine can read is not this program's
/// key alone (threat T13). Nothing from the file is ever printed, in an error or otherwise.
pub fn read_key(path: &Path) -> anyhow::Result<SigningKey> {
    file_keys_allowed()?;
    let meta = std::fs::symlink_metadata(path)
        .with_context(|| format!("the key file {} cannot be read", path.display()))?;
    if !meta.is_file() {
        bail!("the key file {} is not a plain file", path.display());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            bail!(
                "the key file {} can be read or written by others; make it the owner's alone (chmod 600)",
                path.display()
            );
        }
    }
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("the key file {} cannot be read", path.display()))?;
    SigningKey::from_file_text(&text)
        .map_err(|_| anyhow::anyhow!("the key file {} does not hold a key", path.display()))
}

/// Make a new key: write its file, readable by the owner alone and never over an existing
/// file, and return the public key to enrol.
pub fn keygen(kid: &str, out: &Path) -> anyhow::Result<PublicKey> {
    file_keys_allowed()?;
    let kid = Id::new(kid).map_err(|_| anyhow::anyhow!("the key id is not an identifier"))?;
    let (text, public) =
        gov_core::keys::new_key_file(kid).map_err(|_| anyhow::anyhow!("no key could be made"))?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(out).with_context(|| {
        format!(
            "{} cannot be created (it must not exist yet)",
            out.display()
        )
    })?;
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    Ok(public)
}

/// Where to listen. An address beyond this machine needs the operator's word.
pub fn listen_address(listen: &str, plain_http_on_network: bool) -> anyhow::Result<SocketAddr> {
    let addr: SocketAddr = listen
        .parse()
        .with_context(|| format!("{listen} is not an address and a port"))?;
    if !addr.ip().is_loopback() && !plain_http_on_network {
        bail!(
            "{listen} is reachable from other machines and this program speaks plain HTTP; listen on 127.0.0.1, or put TLS in front and pass --plain-http-on-network"
        );
    }
    Ok(addr)
}

/// An address this program will send to: HTTPS anywhere, plain HTTP only on this machine
/// unless the operator says otherwise. `what` names it in the refusal.
pub fn outbound_address(what: &str, url: &str, plain_http_on_network: bool) -> anyhow::Result<()> {
    let parsed = reqwest::Url::parse(url).with_context(|| format!("{what} is not a URL"))?;
    if !parsed.username().is_empty() || parsed.password().is_some() {
        bail!("{what} carries a user name or password; a URL here holds no secret");
    }
    let host = parsed.host_str().unwrap_or_default();
    let local = host == "localhost"
        || host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    match parsed.scheme() {
        "https" => Ok(()),
        "http" if local || plain_http_on_network => Ok(()),
        "http" => bail!(
            "{what} is plain HTTP to another machine; use https, or pass --plain-http-on-network"
        ),
        _ => bail!("{what} is neither http nor https"),
    }
}

/// Bind, and say where on standard output, so whoever started the program knows the port.
pub async fn bind(addr: SocketAddr) -> anyhow::Result<tokio::net::TcpListener> {
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("cannot listen on {addr}"))?;
    println!("listening on {}", listener.local_addr()?);
    std::io::stdout().flush()?;
    Ok(listener)
}
