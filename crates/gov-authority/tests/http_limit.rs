//! The limit for each source on the Authority's listener for gates (GR-169, GR-190).

use std::net::IpAddr;

use gov_authority::http::{Listener, SourceLimit};
use gov_authority::{Authority, MemoryStore};
use gov_core::{Id, Scope, SigningKey};

fn listener(per_second: u32, sources: usize) -> Listener<MemoryStore> {
    let key =
        |kid: &str, seed: u8| SigningKey::from_seed(Id::new(kid).unwrap(), &[seed; 32]).unwrap();
    let authority = Authority::new(
        MemoryStore::default(),
        Scope::new("acme", "prod").unwrap(),
        Id::new("authority").unwrap(),
        key("authority-admin-1", 60),
        key("authority-root-1", 61).public(),
    );
    Listener::new(
        std::sync::Arc::new(authority),
        || 0,
        SourceLimit {
            per_second,
            sources,
            late_per_second: 3,
        },
    )
}

fn ip(s: &str) -> IpAddr {
    s.parse().unwrap()
}

#[test]
fn a_full_table_leaves_late_sources_a_shared_allowance() {
    let l = listener(2, 2);
    assert!(l.within_limit(ip("10.0.0.1"), 5));
    assert!(l.within_limit(ip("10.0.0.1"), 5));
    assert!(!l.within_limit(ip("10.0.0.1"), 5));
    assert!(l.within_limit(ip("10.0.0.2"), 5));
    // The table is full. Three more requests are answered, from whichever late sources.
    assert!(l.within_limit(ip("10.0.0.3"), 5));
    assert!(l.within_limit(ip("10.0.0.4"), 5));
    assert!(l.within_limit(ip("10.0.0.4"), 5));
    assert!(!l.within_limit(ip("10.0.0.5"), 5));
    // Those already in it keep their own.
    assert!(l.within_limit(ip("10.0.0.2"), 5));
    // The next second starts again.
    assert!(l.within_limit(ip("10.0.0.5"), 6));
}

#[test]
fn one_ipv6_network_is_one_source() {
    let l = listener(2, 100);
    assert!(l.within_limit(ip("2001:db8:0:1::1"), 5));
    assert!(l.within_limit(ip("2001:db8:0:1::2"), 5));
    assert!(!l.within_limit(ip("2001:db8:0:1:ffff::3"), 5));
    assert!(l.within_limit(ip("2001:db8:0:2::1"), 5));
    // An IPv4 address written as IPv6 is that IPv4 address.
    assert!(l.within_limit(ip("192.0.2.1"), 5));
    assert!(l.within_limit(ip("::ffff:192.0.2.1"), 5));
    assert!(!l.within_limit(ip("192.0.2.1"), 5));
}
