//! The resolver a `class: production` gateway's HTTP clients use (slice H4,
//! `docs/openshell-any-host-spec.md` §3.6, "Which gateway may serve `local`: the names'
//! answers").
//!
//! It resolves each upstream name at **every** connection and accepts a name only when it is
//! one of [`PINNED_MODEL_SERVERS`] and every answer is that name's compiled address. Any other
//! name, no answer, or an answer with any other address (loopback, an address in the network's
//! range, another pinned service, a second address beside the right one) fails the connection,
//! which the caller sees as a `502`; the warning names the model server's name, never the
//! address it was given. So a resolver, a search domain or an `/etc/hosts` line that answers
//! the name with something else can't send prompts there.
//!
//! A `class: test` gateway keeps the system resolver: `hosted-test` reaches an endpoint on the
//! internet by its public name.

use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;

use reqwest::dns::{Addrs, Name, Resolve, Resolving};

use crate::config::{pinned_address, PINNED_MODEL_SERVERS};

/// How a name is looked up: the system's resolver in the gateway, a fixed answer in tests.
pub type Lookup = Arc<
    dyn Fn(String) -> Pin<Box<dyn Future<Output = std::io::Result<Vec<IpAddr>>> + Send>>
        + Send
        + Sync,
>;

/// Accepts only the pinned address for each pinned name ([`check_answer`]).
#[derive(Clone)]
pub struct PinnedResolver {
    lookup: Lookup,
}

impl Default for PinnedResolver {
    fn default() -> Self {
        Self::with_lookup(Arc::new(|name: String| {
            Box::pin(async move {
                let addrs = tokio::net::lookup_host((name.as_str(), 0)).await?;
                Ok(addrs.map(|a| a.ip()).collect())
            })
        }))
    }
}

impl PinnedResolver {
    /// A resolver that looks names up with `lookup` (tests).
    pub fn with_lookup(lookup: Lookup) -> Self {
        Self { lookup }
    }

    /// The address to connect to for `name`, or why not (no address in the text).
    pub async fn resolve_name(&self, name: &str) -> Result<IpAddr, String> {
        let Some(pinned) = pinned_address(name) else {
            return Err(format!(
                "`{name}` is not one of this gateway's model servers ({})",
                PINNED_MODEL_SERVERS
                    .iter()
                    .map(|(n, _)| *n)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        };
        let answers = (self.lookup)(name.to_string())
            .await
            .map_err(|_| format!("`{name}` did not resolve"))?;
        check_answer(name, IpAddr::V4(pinned), &answers)
    }
}

/// Every answer must be `pinned`, and there must be one: a list with any other address in it
/// is refused whole, so a second answer can't be the one a connection happens to use.
pub fn check_answer(name: &str, pinned: IpAddr, answers: &[IpAddr]) -> Result<IpAddr, String> {
    if answers.is_empty() {
        return Err(format!("`{name}` resolved to no address"));
    }
    if answers.iter().any(|a| canonical(*a) != pinned) {
        return Err(format!(
            "`{name}` resolved to an address other than its pinned one; refused"
        ));
    }
    Ok(pinned)
}

/// An IPv4-mapped IPv6 answer (`::ffff:10.201.77.13`) is read as its IPv4 address.
fn canonical(a: IpAddr) -> IpAddr {
    match a {
        IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(v6)),
        v4 => v4,
    }
}

impl Resolve for PinnedResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let this = self.clone();
        let name = name.as_str().to_string();
        Box::pin(async move {
            match this.resolve_name(&name).await {
                Ok(ip) => {
                    let addrs: Addrs = Box::new(std::iter::once(SocketAddr::new(ip, 0)));
                    Ok(addrs)
                }
                Err(why) => {
                    tracing::warn!(model_server = %name, "{why}");
                    Err(why.into())
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn fixed(answers: Vec<IpAddr>) -> PinnedResolver {
        PinnedResolver::with_lookup(Arc::new(move |_| {
            let a = answers.clone();
            Box::pin(async move { Ok(a) })
        }))
    }

    fn v4(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(a, b, c, d))
    }

    #[tokio::test]
    async fn each_name_takes_only_its_own_compiled_address() {
        let default = "vllm-default.pmos.internal";
        let extraction = "vllm-extraction.pmos.internal";
        assert_eq!(
            fixed(vec![v4(10, 201, 77, 13)]).resolve_name(default).await,
            Ok(v4(10, 201, 77, 13))
        );
        assert_eq!(
            fixed(vec![v4(10, 201, 77, 14)])
                .resolve_name(extraction)
                .await,
            Ok(v4(10, 201, 77, 14))
        );
        // IPv4-mapped is the same address.
        let mapped: IpAddr = "::ffff:10.201.77.13".parse().unwrap();
        assert!(fixed(vec![mapped]).resolve_name(default).await.is_ok());
        for wrong in [
            vec![v4(10, 201, 77, 14)],  // the other model server
            vec![v4(10, 201, 77, 12)],  // the runtime
            vec![v4(10, 201, 77, 15)],  // next to it
            vec![v4(10, 201, 77, 200)], // inside the ip_range
            vec![v4(10, 9, 0, 13)],     // outside the subnet
            vec![v4(127, 0, 0, 1)],     // loopback
            vec![v4(169, 254, 169, 254)],
            vec!["::1".parse().unwrap()],
            vec![v4(10, 201, 77, 13), v4(10, 201, 77, 200)], // two answers, one outside
            vec![],
        ] {
            let err = fixed(wrong.clone())
                .resolve_name(default)
                .await
                .unwrap_err();
            for a in &wrong {
                assert!(!err.contains(&a.to_string()), "{err}");
            }
        }
        // Any other name is refused before it is looked up.
        for name in [
            "vllm.pmos.internal",
            "VLLM-DEFAULT.pmos.internal",
            "vllm-default.pmos.internal.",
            "gateway.pmos.internal",
            "example.com",
        ] {
            assert!(fixed(vec![v4(10, 201, 77, 13)])
                .resolve_name(name)
                .await
                .is_err());
        }
        // A failed lookup is refused.
        let failing = PinnedResolver::with_lookup(Arc::new(|_| {
            Box::pin(async { Err(std::io::Error::other("no such host")) })
        }));
        assert!(failing.resolve_name(default).await.is_err());
    }

    #[test]
    fn the_compiled_addresses_are_the_compose_files_literals() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let compose = std::fs::read_to_string(root.join("deploy/compose.yaml")).unwrap();
        for (name, addr) in PINNED_MODEL_SERVERS {
            let want = format!("aliases: [{name}], ipv4_address: {addr} }}");
            assert!(
                compose.contains(&want),
                "deploy/compose.yaml doesn't pin {name} to {addr} (looked for `{want}`)"
            );
        }
        // And deploy/gateway.yaml names exactly these servers.
        let gw = std::fs::read_to_string(root.join("deploy/gateway.yaml")).unwrap();
        let models = crate::config::Models::from_yaml(&gw).unwrap();
        for r in models.roles.values() {
            assert!(pinned_address(&r.upstream_host()).is_some(), "{}", r.name);
        }
    }
}
