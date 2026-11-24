//! Configuration from environment variables, checked at start-up.

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

use chrono::Duration;
use pmos_assertion::{KeyRing, KeyUse};

use crate::entra::EntraConfig;
use crate::upstream::{Credentials, Signer};

/// pmos-web's client name: the issuer of its user assertions.
pub const ISSUER: &str = "web";

#[derive(Clone)]
pub struct Config {
    /// `PMOS_WEB_BIND`, default `127.0.0.1:8770`.
    pub bind: SocketAddr,
    /// `PMOS_BOTS_URL`, default `http://127.0.0.1:8750`.
    pub bots_url: String,
    /// `PMOS_WEB_BOTS_TOKEN`: the runtime's `web` client token (an entry of its
    /// `PMOS_BOTS_TOKENS`). Required, at least 32 characters. With a `web:bots` key in
    /// `PMOS_ASSERTION_KEYS`, calls for a PM also carry a signed user assertion.
    pub bots_token: Credentials,
    /// `PMOS_TOOLSERVER_URL`, default `http://127.0.0.1:8710`.
    pub toolserver_url: String,
    /// `PMOS_WEB_TOOLSERVER_TOKEN`: pmos-web's own tool server token (`PMOS_WEB_TOKEN`
    /// there), which reaches only the approvals routes and the read-only admin routes
    /// (`GET /internal/admin/*`). Older deployments put the service
    /// token here. Required, at least 32 characters, and different from the runtime token.
    /// With a `web:toolserver` key in `PMOS_ASSERTION_KEYS`, calls for a PM also carry a
    /// signed user assertion.
    pub toolserver_token: Credentials,
    /// `PMOS_CONTENT_ROOT`, default `content`: the deployed content tree the admin screens
    /// read.
    pub content_root: PathBuf,
    /// `PMOS_WEB_STATIC_DIR`: the built web app (`web/dist`). Optional; without it only
    /// `/api` and `/healthz` answer.
    pub static_dir: Option<PathBuf>,
    /// `PMOS_WEB_PUBLIC_URL`: the origin browsers use, such as `https://pmos.example.com`.
    /// Writes must come from it. Without it, `Origin` must match the request's `Host`.
    pub public_url: Option<String>,
    /// `PMOS_WEB_DEV_LOGIN=1`: development sign-in (`POST /api/auth/dev-login`), where
    /// anyone who can reach the port signs in as any member.
    pub dev_login: bool,
    /// Entra ID sign-in (`PMOS_WEB_ENTRA_*`, with `PMOS_WEB_PUBLIC_URL`): on when fully
    /// configured. The server starts with Entra, development sign-in, or both.
    pub entra: Option<EntraConfig>,
    /// `PMOS_WEB_PASSWORD_LOGIN=1`: email-and-password sign-in with a required
    /// authenticator code (`docs/password-sign-in.md`), the tool server deciding. Off by
    /// default; needs signed user assertions to the tool server (a `web:toolserver` key, and
    /// `PMOS_ASSERTION_MODE` `require` or unset).
    pub password_login: bool,
    /// `PMOS_WEB_TRUSTED_PROXIES`: the addresses of the reverse proxies in front of pmos-web,
    /// comma-separated. A request from one of them is counted, for password sign-in's
    /// per-address limit, by its `X-Forwarded-For` read from the right ([`crate::app`]'s
    /// `password::source`); any other request by its own peer address. With password sign-in
    /// on, it must be set unless `PMOS_WEB_DIRECT=1` says browsers reach pmos-web with no proxy
    /// (a loopback bind only); with it empty, a password request that carries a forwarded
    /// header is refused.
    pub trusted_proxies: Vec<IpAddr>,
    /// `PMOS_WEB_SESSION_IDLE_MINUTES`, default 480 (8 hours).
    pub session_idle: Duration,
    /// `PMOS_WEB_SESSION_MAX_MINUTES`, default 1440 (24 hours), from sign-in.
    pub session_max: Duration,
    /// `PMOS_WEB_INTERNAL_BIND`: a second listener for the server-to-server route
    /// (`PUT /internal/brief/{handle}`), which the browser-facing `bind` then doesn't serve.
    /// Without it, that route is on `bind`, refusing anything that looks like a browser.
    pub internal_bind: Option<SocketAddr>,
    /// The admin door (slice H10, `PMOS_WEB_ADMIN_*`): the admin-only listener, its origin,
    /// who may reach it and its session bounds. `None` without `PMOS_WEB_ADMIN_BIND`: then no
    /// admin route is served anywhere, and `pmos-web` itself refuses to start
    /// ([`crate::host::check_deployment`]).
    pub admin: Option<AdminDoor>,
    /// `PMOS_WEB_PASSWORD_CLIENTS` (decision D2 (a)): the networks password sign-in is served
    /// to, each no wider than a `/16` (IPv4) or a `/48` (IPv6). Empty: no allow-list (refused on
    /// `PMOS_EXPOSURE=public` with password sign-in on). Set: every `/api/auth/password/*`
    /// request from a client address outside it answers `403 password_sign_in_not_from_here`
    /// before the tool server is asked.
    pub password_clients: Vec<Cidr>,
}

/// Who may reach the admin door, by its URL's scheme (r7, X6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdminMode {
    /// An `http` admin URL on a loopback name: an SSH tunnel to a loopback publish. Only
    /// `PMOS_WEB_ADMIN_TUNNEL_PEER` (the `pmos` network's gateway in the kit) may connect, and
    /// `X-Forwarded-For` is never read.
    Tunnel { peer: IpAddr },
    /// An `https` admin URL: a TLS terminator in front. Only the peers in
    /// `PMOS_WEB_ADMIN_TRUSTED_PROXIES` may connect, and the client they name in
    /// `X-Forwarded-For` must be in loopback, RFC 1918, `100.64.0.0/10` or IPv6 unique-local.
    Proxy { trusted: Vec<IpAddr> },
}

/// The admin door's settings (slice H10, `docs/openshell-any-host-spec.md` §3.8, "The admin
/// listener's sign-in").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminDoor {
    /// `PMOS_WEB_ADMIN_BIND` (8772 in the kit): unspecified, loopback or a private address.
    pub bind: SocketAddr,
    /// `PMOS_WEB_ADMIN_URL`: the door's own origin, such as `https://pmos-admin.corp.example` or
    /// `http://pmos-prod.localhost:8772`. Writes must come from it.
    pub url: String,
    /// The authority every request's `Host` must equal: the URL's host (lower-cased) and its
    /// port as the URL writes it.
    pub authority: String,
    pub mode: AdminMode,
    /// `PMOS_WEB_ADMIN_SESSION_IDLE_MINUTES` (default 30, at most 60) and
    /// `PMOS_WEB_ADMIN_SESSION_MAX_MINUTES` (default 480, at most 720).
    pub idle: Duration,
    pub max: Duration,
    /// `PMOS_WEB_ADMIN_ENTRA=1`: `<PMOS_WEB_ADMIN_URL>/api/auth/callback` is registered as a
    /// second redirect URI in the Entra app registration, so the admin door offers Microsoft
    /// sign-in. Off by default: the admin door is then password-only (the owner's open question,
    /// §8.2 item 9; the spec's default).
    pub entra: bool,
}

/// The longest idle and absolute admin session `pmos-web` starts with (r7, X6).
pub const ADMIN_IDLE_LIMIT_MINUTES: i64 = 60;
pub const ADMIN_MAX_LIMIT_MINUTES: i64 = 12 * 60;

/// The narrowest an allow-list entry may be widened to (decision D2 (a)).
pub const PASSWORD_CLIENTS_V4_MIN: u8 = 16;
pub const PASSWORD_CLIENTS_V6_MIN: u8 = 48;

/// An IPv4 or IPv6 network, written `address/length` with no host bits set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cidr {
    net: IpAddr,
    len: u8,
}

impl Cidr {
    pub fn parse(s: &str) -> Result<Self, String> {
        let (addr, len) = s
            .split_once('/')
            .ok_or(format!("`{s}` isn't a network written address/length"))?;
        let net: IpAddr = addr
            .parse()
            .map_err(|_| format!("`{s}` isn't a network written address/length"))?;
        let max = if net.is_ipv4() { 32 } else { 128 };
        let len: u8 = match len.parse::<u8>() {
            Ok(n) if n <= max && (len == "0" || !len.starts_with('0')) => n,
            _ => return Err(format!("`{s}`: the length must be 0 to {max}")),
        };
        let c = Self { net, len };
        if c.masked(net) != net {
            return Err(format!(
                "`{s}` has bits set past its length: write the network's first address"
            ));
        }
        Ok(c)
    }

    fn masked(&self, ip: IpAddr) -> IpAddr {
        match ip {
            IpAddr::V4(v4) => {
                let bits = u32::from(v4);
                let mask = if self.len == 0 {
                    0
                } else {
                    u32::MAX << (32 - self.len)
                };
                IpAddr::V4((bits & mask).into())
            }
            IpAddr::V6(v6) => {
                let bits = u128::from(v6);
                let mask = if self.len == 0 {
                    0
                } else {
                    u128::MAX << (128 - self.len)
                };
                IpAddr::V6((bits & mask).into())
            }
        }
    }

    /// Is `ip` in this network? An IPv4-mapped IPv6 address counts as its IPv4 address.
    pub fn contains(&self, ip: IpAddr) -> bool {
        let ip = canonical_ip(ip);
        let net = canonical_ip(self.net);
        let len = match (self.net, net) {
            // `::ffff:a.b.c.d/n` written as IPv6: the IPv4 part's length.
            (IpAddr::V6(_), IpAddr::V4(_)) => self.len.saturating_sub(96),
            _ => self.len,
        };
        if ip.is_ipv4() != net.is_ipv4() {
            return false;
        }
        Cidr { net, len }.masked(ip) == net
    }

    /// The width as D2 (a) bounds it: an IPv4-mapped network counts as IPv4.
    fn too_wide(&self) -> bool {
        match (self.net, canonical_ip(self.net)) {
            (IpAddr::V4(_), _) => self.len < PASSWORD_CLIENTS_V4_MIN,
            (IpAddr::V6(_), IpAddr::V4(_)) => self.len < 96 + PASSWORD_CLIENTS_V4_MIN,
            (IpAddr::V6(v6), IpAddr::V6(_)) => {
                // `::ffff:0:0/96` and anything holding it is the whole IPv4 internet.
                let mapped = u128::from(v6) >> 32 == 0xffff;
                self.len < PASSWORD_CLIENTS_V6_MIN || (mapped && self.len < 96 + 16)
            }
        }
    }
}

impl std::fmt::Display for Cidr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.net, self.len)
    }
}

/// An IPv4-mapped IPv6 address as IPv4; anything else as it is.
pub fn canonical_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        v4 => v4,
    }
}

/// Loopback, RFC 1918, `100.64.0.0/10` (VPN overlays commonly use it) or IPv6 unique-local
/// (`fc00::/7`): where the admin door may be bound and its clients may be (§3.8).
pub fn in_admin_ranges(ip: IpAddr) -> bool {
    match canonical_ip(ip) {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_loopback()
                || o[0] == 10
                || (o[0] == 172 && (16..=31).contains(&o[1]))
                || (o[0] == 192 && o[1] == 168)
                || (o[0] == 100 && (64..=127).contains(&o[1]))
        }
        IpAddr::V6(v6) => v6.is_loopback() || (v6.segments()[0] & 0xfe00) == 0xfc00,
    }
}

/// [`in_admin_ranges`] plus link-local (`169.254.0.0/16`, `fe80::/10`): what a `private`
/// host's public URL may resolve to (§3.8, `PMOS_EXPOSURE=private` checked by the service).
pub fn in_private_host_ranges(ip: IpAddr) -> bool {
    in_admin_ranges(ip)
        || match canonical_ip(ip) {
            IpAddr::V4(v4) => v4.is_link_local(),
            IpAddr::V6(v6) => (v6.segments()[0] & 0xffc0) == 0xfe80,
        }
}

/// `PMOS_WEB_PASSWORD_CLIENTS`: comma-separated networks, each within D2 (a)'s bounds. The error
/// names the entry.
pub fn parse_password_clients(list: &str) -> Result<Vec<Cidr>, String> {
    let mut out = Vec::new();
    for entry in list.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let c = Cidr::parse(entry).map_err(|e| format!("PMOS_WEB_PASSWORD_CLIENTS: {e}"))?;
        if c.too_wide() {
            return Err(format!(
                "PMOS_WEB_PASSWORD_CLIENTS: `{entry}` is wider than decision D2 (a) allows \
                 (no shorter than /{PASSWORD_CLIENTS_V4_MIN} for IPv4 or \
                 /{PASSWORD_CLIENTS_V6_MIN} for IPv6): a list that holds the internet is not a \
                 list. Name the VPN's or the office's own ranges"
            ));
        }
        out.push(c);
    }
    Ok(out)
}

/// Is the host part of an `http` admin URL a loopback name (r7): `localhost`, a name ending in
/// `.localhost` (which Chromium and Firefox resolve to loopback and treat as a secure context),
/// `127.0.0.1` or `[::1]`.
fn loopback_name(host: &str) -> bool {
    let h = host.to_ascii_lowercase();
    h == "localhost" || h.ends_with(".localhost") || h == "127.0.0.1" || h == "[::1]"
}

/// Why the admin door won't start (slice H10). Each names the settings to fix.
pub const ADMIN_URL_REQUIRED: &str = "PMOS_WEB_ADMIN_BIND is set without PMOS_WEB_ADMIN_URL: \
     the admin door needs its own origin, such as https://pmos-admin.corp.example (a TLS \
     terminator on a private address) or http://<deployment>.localhost:8772 (an SSH tunnel), \
     docs/runbooks/install.md";
pub const ADMIN_HTTPS_NEEDS_PROXIES: &str = "PMOS_WEB_ADMIN_URL is https: pmos-web serves no TLS \
     itself, so a terminator is in front. Set PMOS_WEB_ADMIN_TRUSTED_PROXIES to the address it \
     reaches pmos-web from (in the deploy kit, the `pmos` network's gateway, PMOS_NETWORK_GATEWAY)";
pub const ADMIN_HTTP_WITH_PROXIES: &str = "PMOS_WEB_ADMIN_URL is http (the tunnel mode), and \
     PMOS_WEB_ADMIN_TRUSTED_PROXIES names a proxy: a tunnel has none, and X-Forwarded-For is never \
     read in that mode. Unset PMOS_WEB_ADMIN_TRUSTED_PROXIES, or put a TLS terminator in front \
     and use an https admin URL";
pub const ADMIN_HTTP_NOT_LOOPBACK: &str = "PMOS_WEB_ADMIN_URL is http on a name that isn't \
     loopback: browsers keep the admin door's Secure cookie only on https or a loopback name, and \
     an admin's password must never cross a network in the clear. Use \
     http://<deployment>.localhost:<port> through an SSH tunnel, or https through a TLS \
     terminator on a private address";
pub const ADMIN_TUNNEL_NEEDS_PEER: &str = "PMOS_WEB_ADMIN_URL is http (the tunnel mode): set \
     PMOS_WEB_ADMIN_TUNNEL_PEER to the one address the tunnel arrives from (in the deploy kit, \
     the `pmos` network's gateway, PMOS_NETWORK_GATEWAY, which deploy/compose.yaml passes)";
pub const ADMIN_NO_PROVIDER: &str = "the admin door has no sign-in: development sign-in is never \
     served there. Turn on password sign-in (PMOS_WEB_PASSWORD_LOGIN=1), or, with Entra ID \
     sign-in configured, register <PMOS_WEB_ADMIN_URL>/api/auth/callback as a second redirect URI \
     and set PMOS_WEB_ADMIN_ENTRA=1";

/// The admin door's settings, or `None` when `PMOS_WEB_ADMIN_BIND` is unset (and no other
/// `PMOS_WEB_ADMIN_*` setting is either).
#[allow(clippy::too_many_arguments)]
fn admin_door(
    set: &dyn Fn(&str) -> Option<String>,
    flag: &dyn Fn(&str) -> Result<bool, String>,
    bind: SocketAddr,
    internal_bind: Option<SocketAddr>,
    public_url: Option<&str>,
    entra_on: bool,
    password_login: bool,
) -> Result<Option<AdminDoor>, String> {
    const OTHERS: [&str; 7] = [
        "PMOS_WEB_ADMIN_URL",
        "PMOS_WEB_ADMIN_TRUSTED_PROXIES",
        "PMOS_WEB_ADMIN_TUNNEL_PEER",
        "PMOS_WEB_ADMIN_SESSION_IDLE_MINUTES",
        "PMOS_WEB_ADMIN_SESSION_MAX_MINUTES",
        "PMOS_WEB_ADMIN_ENTRA",
        "PMOS_WEB_ADMIN_PUBLISH",
    ];
    let Some(admin_bind) = set("PMOS_WEB_ADMIN_BIND") else {
        // PMOS_WEB_ADMIN_PUBLISH is the kit's (compose publishes the port); it never reaches
        // pmos-web, so it isn't a half-set door here. The rest are.
        if let Some(k) = OTHERS[..6].iter().find(|k| set(k).is_some()) {
            return Err(format!(
                "{k} is set without PMOS_WEB_ADMIN_BIND: set the admin door's listener too, or \
                 unset every PMOS_WEB_ADMIN_* setting"
            ));
        }
        return Ok(None);
    };
    let admin_bind: SocketAddr = admin_bind
        .trim()
        .parse()
        .map_err(|e| format!("PMOS_WEB_ADMIN_BIND: {e}"))?;
    let ip = admin_bind.ip();
    if !(ip.is_unspecified() || in_admin_ranges(ip)) {
        return Err(format!(
            "PMOS_WEB_ADMIN_BIND {admin_bind}: the admin door is never on a public address. Bind \
             it to 0.0.0.0 in a container (the kit publishes it on PMOS_WEB_ADMIN_PUBLISH), to \
             loopback, or to an RFC 1918, 100.64.0.0/10 or IPv6 unique-local address"
        ));
    }
    if admin_bind == bind || internal_bind == Some(admin_bind) {
        return Err(
            "PMOS_WEB_ADMIN_BIND must differ from PMOS_WEB_BIND and PMOS_WEB_INTERNAL_BIND".into(),
        );
    }
    let url = set("PMOS_WEB_ADMIN_URL").ok_or(ADMIN_URL_REQUIRED)?;
    let url = origin("PMOS_WEB_ADMIN_URL", url)?;
    let parsed = reqwest::Url::parse(&url).map_err(|e| format!("PMOS_WEB_ADMIN_URL: {e}"))?;
    let host = parsed.host_str().unwrap_or_default().to_ascii_lowercase();
    let authority = match parsed.port() {
        Some(p) => format!("{host}:{p}"),
        None => host.clone(),
    };
    // No cookie of one door is sent to the other: the two origins' hosts differ (r6; on an
    // http admin URL too, the stricter reading: two doors on one loopback name would share its
    // cookie jar).
    let public_host = public_url
        .and_then(|u| reqwest::Url::parse(u).ok())
        .and_then(|u| u.host_str().map(str::to_ascii_lowercase));
    if public_host.as_deref() == Some(host.as_str()) {
        return Err(format!(
            "PMOS_WEB_ADMIN_URL's host `{host}` is PMOS_WEB_PUBLIC_URL's: cookies are kept by host \
             name, not by port, so the two doors' cookies would be sent to each other. Give the \
             admin door a name of its own"
        ));
    }
    let proxies = match set("PMOS_WEB_ADMIN_TRUSTED_PROXIES") {
        None => Vec::new(),
        Some(list) => list
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| {
                s.parse::<IpAddr>().map_err(|_| {
                    format!("PMOS_WEB_ADMIN_TRUSTED_PROXIES: `{s}` isn't an IP address")
                })
            })
            .collect::<Result<Vec<_>, _>>()?,
    };
    let mode = match parsed.scheme() {
        "https" => {
            if public_host.is_none() {
                return Err(
                    "PMOS_WEB_ADMIN_URL is https: set PMOS_WEB_PUBLIC_URL too, so \
                     pmos-web can check the two doors' host names differ"
                        .into(),
                );
            }
            if proxies.is_empty() {
                return Err(ADMIN_HTTPS_NEEDS_PROXIES.into());
            }
            AdminMode::Proxy { trusted: proxies }
        }
        _ => {
            if !proxies.is_empty() {
                return Err(ADMIN_HTTP_WITH_PROXIES.into());
            }
            if !loopback_name(&host) {
                return Err(ADMIN_HTTP_NOT_LOOPBACK.into());
            }
            if parsed.port().is_none() {
                return Err(
                    "PMOS_WEB_ADMIN_URL is http: write its port, as the tunnel's local \
                     end listens on it (http://<deployment>.localhost:8772)"
                        .into(),
                );
            }
            let peer = set("PMOS_WEB_ADMIN_TUNNEL_PEER").ok_or(ADMIN_TUNNEL_NEEDS_PEER)?;
            let peer: IpAddr = peer.trim().parse().map_err(|_| {
                format!(
                    "PMOS_WEB_ADMIN_TUNNEL_PEER: `{}` isn't an IP address",
                    peer.trim()
                )
            })?;
            AdminMode::Tunnel { peer }
        }
    };
    let minutes = |k: &str, default: i64, most: i64| -> Result<Duration, String> {
        match set(k) {
            None => Ok(Duration::minutes(default)),
            Some(v) => match v.trim().parse::<i64>() {
                Ok(n) if (1..=most).contains(&n) => Ok(Duration::minutes(n)),
                _ => Err(format!(
                    "{k} must be a whole number of minutes, 1 to {most} (the admin door's \
                     sessions are bounded, r7)"
                )),
            },
        }
    };
    let idle = minutes(
        "PMOS_WEB_ADMIN_SESSION_IDLE_MINUTES",
        30,
        ADMIN_IDLE_LIMIT_MINUTES,
    )?;
    let max = minutes(
        "PMOS_WEB_ADMIN_SESSION_MAX_MINUTES",
        8 * 60,
        ADMIN_MAX_LIMIT_MINUTES,
    )?;
    if idle > max {
        return Err("PMOS_WEB_ADMIN_SESSION_IDLE_MINUTES can't be longer than \
             PMOS_WEB_ADMIN_SESSION_MAX_MINUTES"
            .into());
    }
    let entra = flag("PMOS_WEB_ADMIN_ENTRA")?;
    if entra && !entra_on {
        return Err("PMOS_WEB_ADMIN_ENTRA=1 needs Entra ID sign-in configured \
             (PMOS_WEB_ENTRA_*)"
            .into());
    }
    if !entra && !password_login {
        return Err(ADMIN_NO_PROVIDER.into());
    }
    Ok(Some(AdminDoor {
        bind: admin_bind,
        url,
        authority,
        mode,
        idle,
        max,
        entra,
    }))
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("bind", &self.bind)
            .field("bots_url", &self.bots_url)
            .field("bots_token", &self.bots_token)
            .field("toolserver_url", &self.toolserver_url)
            .field("toolserver_token", &self.toolserver_token)
            .field("content_root", &self.content_root)
            .field("static_dir", &self.static_dir)
            .field("public_url", &self.public_url)
            .field("dev_login", &self.dev_login)
            .field("entra", &self.entra)
            .field("password_login", &self.password_login)
            .field("trusted_proxies", &self.trusted_proxies)
            .field("session_idle", &self.session_idle)
            .field("session_max", &self.session_max)
            .field("internal_bind", &self.internal_bind)
            .field("admin", &self.admin)
            .field("password_clients", &self.password_clients)
            .finish()
    }
}

/// Why development sign-in won't start beside Entra ID without the override.
pub const DEV_WITH_ENTRA: &str = "PMOS_WEB_DEV_LOGIN=1 with Entra ID sign-in configured: \
     development sign-in bypasses Entra entirely. Unset PMOS_WEB_DEV_LOGIN, or set \
     PMOS_WEB_DEV_LOGIN_WITH_ENTRA=1 on a development machine that nobody else can reach";

/// Why development sign-in won't start on a reachable address.
pub const DEV_NOT_LOOPBACK: &str = "PMOS_WEB_DEV_LOGIN=1 needs PMOS_WEB_BIND on a loopback \
     address (127.0.0.1 or ::1): with development sign-in anyone who reaches the port signs \
     in as any member";

/// Development sign-in only on a loopback address, the listener's and the internal one's.
pub fn check_dev_bind(dev_login: bool, bind: SocketAddr) -> Result<(), String> {
    if dev_login && !bind.ip().is_loopback() {
        return Err(DEV_NOT_LOOPBACK.into());
    }
    Ok(())
}

/// Why the server won't start without a provider.
pub const NO_PROVIDER: &str = "no sign-in provider: set PMOS_WEB_ENTRA_TENANT_ID, \
     PMOS_WEB_ENTRA_CLIENT_ID, PMOS_WEB_ENTRA_CLIENT_SECRET and PMOS_WEB_PUBLIC_URL for Entra \
     ID sign-in, PMOS_WEB_PASSWORD_LOGIN=1 for password sign-in, or PMOS_WEB_DEV_LOGIN=1 for \
     development sign-in (never expose it)";

/// Why development sign-in won't start beside password sign-in without the override.
pub const DEV_WITH_PASSWORD: &str = "PMOS_WEB_DEV_LOGIN=1 with PMOS_WEB_PASSWORD_LOGIN=1: \
     development sign-in lets anyone who reaches the port in as any member, password or not. \
     Unset PMOS_WEB_DEV_LOGIN, or set PMOS_WEB_DEV_LOGIN_WITH_PASSWORD=1 on a development \
     machine that nobody else can reach";

/// Why password sign-in won't start without knowing where browsers' addresses come from.
pub const PASSWORD_NEEDS_PROXY: &str = "PMOS_WEB_PASSWORD_LOGIN=1 needs to tell browsers \
     apart for its per-address limit: set PMOS_WEB_TRUSTED_PROXIES to the address your reverse \
     proxy reaches pmos-web from (in the deploy kit, the `pmos` network's pinned gateway, \
     PMOS_NETWORK_GATEWAY, 10.201.77.1 unless changed: `deploy/pmos-admin up --web` checks \
     it), or, only when browsers reach pmos-web directly \
     on a loopback address with no proxy, PMOS_WEB_DIRECT=1 (docs/password-sign-in.md)";

/// Why `PMOS_WEB_DIRECT=1` won't start on a reachable address.
pub const DIRECT_NOT_LOOPBACK: &str = "PMOS_WEB_DIRECT=1 (browsers reach pmos-web with no \
     proxy) is only for a loopback PMOS_WEB_BIND (127.0.0.1 or ::1): anywhere else, put the \
     reverse proxy in front and name it in PMOS_WEB_TRUSTED_PROXIES";

/// Why `PMOS_WEB_DIRECT=1` and a proxy list can't go together.
pub const DIRECT_WITH_PROXIES: &str = "PMOS_WEB_DIRECT=1 says there is no proxy, and \
     PMOS_WEB_TRUSTED_PROXIES names one: set one of them";

/// Why password sign-in won't start on a reachable address without `https`.
pub const PASSWORD_NEEDS_HTTPS: &str = "PMOS_WEB_PASSWORD_LOGIN=1 on an address that isn't \
     loopback needs PMOS_WEB_PUBLIC_URL set to the https:// address browsers use (passwords \
     and codes must never cross the network in the clear)";

/// Password sign-in's start-up rules (the review of 2026-11-20, P2 and L5): the per-address
/// limit must be per real address, so either a trusted proxy is named, or `direct` says there
/// is none (loopback only); and a reachable bind needs an `https://` public URL.
pub fn check_password_reach(
    password_login: bool,
    bind: SocketAddr,
    trusted_proxies: &[IpAddr],
    direct: bool,
    public_url: Option<&str>,
) -> Result<(), String> {
    if direct && !trusted_proxies.is_empty() {
        return Err(DIRECT_WITH_PROXIES.into());
    }
    if direct && !bind.ip().is_loopback() {
        return Err(DIRECT_NOT_LOOPBACK.into());
    }
    if !password_login {
        return Ok(());
    }
    if trusted_proxies.is_empty() && !direct {
        return Err(PASSWORD_NEEDS_PROXY.into());
    }
    if !bind.ip().is_loopback() && !public_url.is_some_and(|u| u.starts_with("https://")) {
        return Err(PASSWORD_NEEDS_HTTPS.into());
    }
    Ok(())
}

/// Why password sign-in won't start without signed user assertions.
pub const PASSWORD_NEEDS_REQUIRE: &str = "PMOS_WEB_PASSWORD_LOGIN=1 needs signed user \
     assertions to the tool server in `require` mode: a web:toolserver key in \
     PMOS_ASSERTION_KEYS, and PMOS_ASSERTION_MODE unset or `require` \
     (docs/password-sign-in.md)";

impl Config {
    pub fn from_env() -> Result<Self, String> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let set = |k: &str| get(k).filter(|v| !v.trim().is_empty());
        let token = |k: &str| -> Result<String, String> {
            let t = set(k).ok_or(format!("{k} is required"))?;
            if t.len() < 32 {
                return Err(format!("{k} must be at least 32 characters"));
            }
            Ok(t)
        };
        let dev_login = match get("PMOS_WEB_DEV_LOGIN").as_deref().map(str::trim) {
            None | Some("") | Some("0") => false,
            Some("1") => true,
            Some(other) => {
                return Err(format!(
                    "PMOS_WEB_DEV_LOGIN must be `1` or `0`, not `{other}`"
                ))
            }
        };
        let password_login = match get("PMOS_WEB_PASSWORD_LOGIN").as_deref().map(str::trim) {
            None | Some("") | Some("0") => false,
            Some("1") => true,
            Some(other) => {
                return Err(format!(
                    "PMOS_WEB_PASSWORD_LOGIN must be `1` or `0`, not `{other}`"
                ))
            }
        };
        let public_url = set("PMOS_WEB_PUBLIC_URL")
            .map(|u| origin("PMOS_WEB_PUBLIC_URL", u))
            .transpose()?;
        let entra = EntraConfig::from_lookup(set, public_url.as_deref())?;
        if !dev_login && entra.is_none() && !password_login {
            return Err(NO_PROVIDER.into());
        }
        let trusted_proxies = match set("PMOS_WEB_TRUSTED_PROXIES") {
            None => Vec::new(),
            Some(list) => list
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| {
                    s.parse::<IpAddr>()
                        .map_err(|_| format!("PMOS_WEB_TRUSTED_PROXIES: `{s}` isn't an IP address"))
                })
                .collect::<Result<_, _>>()?,
        };
        let flag = |k: &str| -> Result<bool, String> {
            match get(k).as_deref().map(str::trim) {
                None | Some("") | Some("0") => Ok(false),
                Some("1") => Ok(true),
                Some(other) => Err(format!("{k} must be `1` or `0`, not `{other}`")),
            }
        };
        let dev_with_entra = flag("PMOS_WEB_DEV_LOGIN_WITH_ENTRA")?;
        if dev_login && entra.is_some() && !dev_with_entra {
            return Err(DEV_WITH_ENTRA.into());
        }
        if dev_login && password_login && !flag("PMOS_WEB_DEV_LOGIN_WITH_PASSWORD")? {
            return Err(DEV_WITH_PASSWORD.into());
        }
        let direct = flag("PMOS_WEB_DIRECT")?;
        let bots_token = token("PMOS_WEB_BOTS_TOKEN")?;
        let toolserver_token = token("PMOS_WEB_TOOLSERVER_TOKEN")?;
        if bots_token == toolserver_token {
            return Err("PMOS_WEB_BOTS_TOKEN and PMOS_WEB_TOOLSERVER_TOKEN must differ".into());
        }
        let content_root: PathBuf = set("PMOS_CONTENT_ROOT")
            .unwrap_or_else(|| "content".into())
            .into();
        let mut keys = match set("PMOS_ASSERTION_KEYS") {
            None => BTreeMap::new(),
            Some(spec) => parse_assertion_keys(&spec, "pmos-web", |iss, aud| {
                iss == ISSUER && (aud == "bots" || aud == "toolserver")
            })?,
        };
        let mut credentials = |token: String, audience: &str| {
            let c = Credentials::new(token);
            match keys.remove(&(ISSUER.to_string(), audience.to_string())) {
                Some(ring) => c.with_signer(Signer::new(ISSUER, audience, ring)),
                None => c,
            }
        };
        let bots_token = credentials(bots_token, "bots");
        let toolserver_token = credentials(toolserver_token, "toolserver");
        if password_login {
            let mode_ok = matches!(
                set("PMOS_ASSERTION_MODE").as_deref().map(str::trim),
                None | Some("require")
            );
            if !toolserver_token.signs() || !mode_ok {
                return Err(PASSWORD_NEEDS_REQUIRE.into());
            }
        }
        let minutes = |k: &str, default: i64| -> Result<Duration, String> {
            match set(k) {
                None => Ok(Duration::minutes(default)),
                Some(v) => match v.trim().parse::<i64>() {
                    Ok(n) if (1..=60 * 24 * 30).contains(&n) => Ok(Duration::minutes(n)),
                    _ => Err(format!("{k} must be a whole number of minutes, 1 to 43200")),
                },
            }
        };
        let session_idle = minutes("PMOS_WEB_SESSION_IDLE_MINUTES", 8 * 60)?;
        let session_max = minutes("PMOS_WEB_SESSION_MAX_MINUTES", 24 * 60)?;
        if session_idle > session_max {
            return Err(
                "PMOS_WEB_SESSION_IDLE_MINUTES can't be longer than PMOS_WEB_SESSION_MAX_MINUTES"
                    .into(),
            );
        }
        let bind: SocketAddr = set("PMOS_WEB_BIND")
            .unwrap_or_else(|| "127.0.0.1:8770".into())
            .parse()
            .map_err(|e| format!("PMOS_WEB_BIND: {e}"))?;
        check_dev_bind(dev_login, bind)?;
        check_password_reach(
            password_login,
            bind,
            &trusted_proxies,
            direct,
            public_url.as_deref(),
        )?;
        let internal_bind: Option<SocketAddr> = set("PMOS_WEB_INTERNAL_BIND")
            .map(|v| v.parse())
            .transpose()
            .map_err(|e| format!("PMOS_WEB_INTERNAL_BIND: {e}"))?;
        if internal_bind.is_some_and(|i| i == bind) {
            return Err("PMOS_WEB_INTERNAL_BIND must differ from PMOS_WEB_BIND".into());
        }
        let admin = admin_door(
            &set,
            &flag,
            bind,
            internal_bind,
            public_url.as_deref(),
            entra.is_some(),
            password_login,
        )?;
        let password_clients = match set("PMOS_WEB_PASSWORD_CLIENTS") {
            None => Vec::new(),
            Some(list) => parse_password_clients(&list)?,
        };
        Ok(Self {
            admin,
            password_clients,
            bind,
            internal_bind,
            bots_url: http_url(
                "PMOS_BOTS_URL",
                set("PMOS_BOTS_URL").unwrap_or_else(|| "http://127.0.0.1:8750".into()),
            )?,
            bots_token,
            toolserver_url: http_url(
                "PMOS_TOOLSERVER_URL",
                set("PMOS_TOOLSERVER_URL").unwrap_or_else(|| "http://127.0.0.1:8710".into()),
            )?,
            toolserver_token,
            content_root,
            static_dir: set("PMOS_WEB_STATIC_DIR").map(PathBuf::from),
            public_url,
            dev_login,
            entra,
            password_login,
            trusted_proxies,
            session_idle,
            session_max,
        })
    }
}

/// `PMOS_ASSERTION_KEYS`: the keys this service signs user assertions with
/// (`docs/user-assertion.md`; `pmos_assertion::parse_key_list`, shared by every service that
/// signs or checks). Only pairs `wanted` accepts may appear, so a service never holds a key it
/// has no use for. Errors name the entry and the pair, never a key.
pub fn parse_assertion_keys(
    spec: &str,
    service: &str,
    wanted: impl Fn(&str, &str) -> bool,
) -> Result<BTreeMap<(String, String), KeyRing>, String> {
    pmos_assertion::parse_key_list(spec, service, KeyUse::Signs, wanted)
}

/// An `http` or `https` base URL, without a trailing slash.
fn http_url(key: &str, value: String) -> Result<String, String> {
    let url = reqwest::Url::parse(&value).map_err(|e| format!("{key}: {e}"))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(format!("{key} must be an http or https URL"));
    }
    Ok(value.trim_end_matches('/').to_string())
}

/// An origin: scheme, host and port only.
fn origin(key: &str, value: String) -> Result<String, String> {
    let value = http_url(key, value)?;
    let url = reqwest::Url::parse(&value).map_err(|e| format!("{key}: {e}"))?;
    if url.path() != "/" || url.query().is_some() {
        return Err(format!(
            "{key} must be an origin such as https://pmos.example.com, with no path"
        ));
    }
    Ok(url.origin().ascii_serialization())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| {
            pairs
                .iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.clone())
        }
    }

    const BOTS: &str = "web-client-token-0123456789abcdef0123";
    const TOOLS: &str = "service-token-0123456789abcdef012345";

    fn base() -> Vec<(&'static str, &'static str)> {
        vec![
            ("PMOS_WEB_DEV_LOGIN", "1"),
            ("PMOS_WEB_BOTS_TOKEN", BOTS),
            ("PMOS_WEB_TOOLSERVER_TOKEN", TOOLS),
        ]
    }

    #[test]
    fn reads_defaults_and_redacts_tokens() {
        let cfg = Config::from_lookup(lookup(&base())).unwrap();
        assert_eq!(cfg.bind.to_string(), "127.0.0.1:8770");
        assert_eq!(cfg.bots_url, "http://127.0.0.1:8750");
        assert_eq!(cfg.toolserver_url, "http://127.0.0.1:8710");
        assert_eq!(cfg.content_root, PathBuf::from("content"));
        assert_eq!(cfg.session_idle, Duration::hours(8));
        assert_eq!(cfg.session_max, Duration::hours(24));
        assert!(cfg.dev_login && cfg.static_dir.is_none() && cfg.public_url.is_none());
        let debug = format!("{cfg:?}");
        assert!(!debug.contains(BOTS) && !debug.contains(TOOLS));
    }

    #[test]
    fn refuses_to_start_without_a_provider() {
        let mut pairs = base();
        pairs.retain(|(k, _)| *k != "PMOS_WEB_DEV_LOGIN");
        let err = Config::from_lookup(lookup(&pairs)).unwrap_err();
        assert!(err.contains("no sign-in provider"), "{err}");
        pairs.push(("PMOS_WEB_DEV_LOGIN", "0"));
        assert!(Config::from_lookup(lookup(&pairs)).is_err());
        let mut yes = base();
        yes[0] = ("PMOS_WEB_DEV_LOGIN", "yes");
        assert!(Config::from_lookup(lookup(&yes))
            .unwrap_err()
            .contains("`1` or `0`"));
    }

    #[test]
    fn starts_with_entra_dev_or_both_but_never_half_configured() {
        const SECRET: &str = "entra-client-secret-value";
        let entra = vec![
            (
                "PMOS_WEB_ENTRA_TENANT_ID",
                "11111111-2222-3333-4444-555555555555",
            ),
            (
                "PMOS_WEB_ENTRA_CLIENT_ID",
                "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            ),
            ("PMOS_WEB_ENTRA_CLIENT_SECRET", SECRET),
            ("PMOS_WEB_PUBLIC_URL", "https://pmos.example.com"),
        ];
        let mut pairs = base();
        pairs.retain(|(k, _)| *k != "PMOS_WEB_DEV_LOGIN");
        let only_entra =
            Config::from_lookup(lookup(&[pairs.clone(), entra.clone()].concat())).unwrap();
        assert!(only_entra.entra.is_some() && !only_entra.dev_login);
        assert!(!format!("{only_entra:?}").contains(SECRET));
        // Both only with the explicit development override.
        let err = Config::from_lookup(lookup(&[base(), entra.clone()].concat())).unwrap_err();
        assert!(err.contains("PMOS_WEB_DEV_LOGIN_WITH_ENTRA"), "{err}");
        assert!(!err.contains(SECRET));
        let both = Config::from_lookup(lookup(
            &[
                base(),
                entra.clone(),
                vec![("PMOS_WEB_DEV_LOGIN_WITH_ENTRA", "1")],
            ]
            .concat(),
        ))
        .unwrap();
        assert!(both.entra.is_some() && both.dev_login);
        // Half-configured refuses even with development sign-in on.
        for drop in 0..entra.len() {
            let mut half = entra.clone();
            half.remove(drop);
            let err = Config::from_lookup(lookup(&[base(), half].concat())).unwrap_err();
            assert!(
                err.contains("half-configured") || err.contains("PMOS_WEB_PUBLIC_URL is required"),
                "{err}"
            );
            assert!(!err.contains(SECRET), "{err}");
        }
    }

    #[test]
    fn refuses_weak_tokens_bad_urls_and_bad_durations() {
        let cases: Vec<(Vec<(&str, &str)>, &str)> = vec![
            (
                vec![
                    ("PMOS_WEB_DEV_LOGIN", "1"),
                    ("PMOS_WEB_TOOLSERVER_TOKEN", TOOLS),
                ],
                "PMOS_WEB_BOTS_TOKEN is required",
            ),
            (
                vec![
                    ("PMOS_WEB_DEV_LOGIN", "1"),
                    ("PMOS_WEB_BOTS_TOKEN", "short"),
                    ("PMOS_WEB_TOOLSERVER_TOKEN", TOOLS),
                ],
                "at least 32",
            ),
            (
                vec![
                    ("PMOS_WEB_DEV_LOGIN", "1"),
                    ("PMOS_WEB_BOTS_TOKEN", BOTS),
                    ("PMOS_WEB_TOOLSERVER_TOKEN", BOTS),
                ],
                "must differ",
            ),
            (
                [base(), vec![("PMOS_BOTS_URL", "ftp://bots")]].concat(),
                "http or https",
            ),
            (
                [
                    base(),
                    vec![("PMOS_WEB_PUBLIC_URL", "https://pmos.example.com/app")],
                ]
                .concat(),
                "no path",
            ),
            (
                [base(), vec![("PMOS_WEB_SESSION_IDLE_MINUTES", "0")]].concat(),
                "whole number",
            ),
            (
                [
                    base(),
                    vec![
                        ("PMOS_WEB_SESSION_IDLE_MINUTES", "600"),
                        ("PMOS_WEB_SESSION_MAX_MINUTES", "60"),
                    ],
                ]
                .concat(),
                "can't be longer",
            ),
        ];
        for (pairs, want) in cases {
            let err = Config::from_lookup(lookup(&pairs)).unwrap_err();
            assert!(err.contains(want), "{want}: {err}");
            assert!(!err.contains(BOTS) && !err.contains(TOOLS));
        }
    }

    #[test]
    fn assertion_keys_attach_a_signer_per_upstream() {
        let key = "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=";
        let keys = format!("web:bots:k1:{key}");
        let cfg = Config::from_lookup(lookup(
            &[base(), vec![("PMOS_ASSERTION_KEYS", keys.as_str())]].concat(),
        ))
        .unwrap();
        assert!(cfg.bots_token.signs() && !cfg.toolserver_token.signs());
        let debug = format!("{cfg:?}");
        assert!(!debug.contains(key) && !debug.contains(BOTS), "{debug}");
        assert!(debug.contains("k1"), "{debug}");
        // Without keys nothing is signed.
        let plain = Config::from_lookup(lookup(&base())).unwrap();
        assert!(!plain.bots_token.signs() && !plain.toolserver_token.signs());
        // Another service's pair, or a short key, refuses to start without showing the key.
        let short = "AQEBAQEBAQEBAQEBAQEBAQ==";
        for (spec, want) in [
            (format!("console:bots:k1:{key}"), "not a pair"),
            (format!("web:bots:k1:{short}"), "32 to 64 bytes"),
            (format!("web:bots:{key}"), "expected issuer:audience"),
        ] {
            let err = Config::from_lookup(lookup(
                &[base(), vec![("PMOS_ASSERTION_KEYS", spec.as_str())]].concat(),
            ))
            .unwrap_err();
            assert!(err.contains(want), "{err}");
            assert!(!err.contains(key) && !err.contains(short), "{err}");
        }
    }

    #[test]
    fn development_sign_in_only_on_a_loopback_bind() {
        for bind in ["0.0.0.0:8770", "10.0.0.2:8770", "[::]:8770"] {
            let err =
                Config::from_lookup(lookup(&[base(), vec![("PMOS_WEB_BIND", bind)]].concat()))
                    .unwrap_err();
            assert!(err.contains("loopback"), "{bind}: {err}");
        }
        for bind in ["127.0.0.1:8770", "[::1]:8770"] {
            assert!(
                Config::from_lookup(lookup(&[base(), vec![("PMOS_WEB_BIND", bind)]].concat()))
                    .is_ok()
            );
        }
        // The internal listener: its own address.
        let cfg = Config::from_lookup(lookup(
            &[base(), vec![("PMOS_WEB_INTERNAL_BIND", "127.0.0.1:8771")]].concat(),
        ))
        .unwrap();
        assert_eq!(cfg.internal_bind.unwrap().to_string(), "127.0.0.1:8771");
        assert!(Config::from_lookup(lookup(
            &[base(), vec![("PMOS_WEB_INTERNAL_BIND", "127.0.0.1:8770")]].concat()
        ))
        .unwrap_err()
        .contains("differ"));
    }

    #[test]
    fn password_sign_in_needs_signed_assertions_in_require() {
        let key = "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=";
        let keys = format!("web:toolserver:k1:{key}");
        let mut alone = base();
        alone.retain(|(k, _)| *k != "PMOS_WEB_DEV_LOGIN");
        alone.push(("PMOS_WEB_PASSWORD_LOGIN", "1"));
        alone.push(("PMOS_WEB_DIRECT", "1"));
        // Without a key for the tool server: refused.
        let err = Config::from_lookup(lookup(&alone)).unwrap_err();
        assert!(err.contains("PMOS_WEB_PASSWORD_LOGIN"), "{err}");
        // With one: password sign-in alone is a provider.
        let with_key = [alone.clone(), vec![("PMOS_ASSERTION_KEYS", keys.as_str())]].concat();
        let cfg = Config::from_lookup(lookup(&with_key)).unwrap();
        assert!(cfg.password_login && !cfg.dev_login && cfg.entra.is_none());
        // A mode other than require refuses; require is fine.
        for mode in ["off", "verify"] {
            let pairs = [with_key.clone(), vec![("PMOS_ASSERTION_MODE", mode)]].concat();
            assert!(Config::from_lookup(lookup(&pairs))
                .unwrap_err()
                .contains("require"));
        }
        let pairs = [with_key.clone(), vec![("PMOS_ASSERTION_MODE", "require")]].concat();
        assert!(Config::from_lookup(lookup(&pairs)).is_ok());
        // Beside development sign-in only with its override (L4); and off by default.
        let both = [
            base(),
            vec![
                ("PMOS_WEB_PASSWORD_LOGIN", "1"),
                ("PMOS_WEB_DIRECT", "1"),
                ("PMOS_ASSERTION_KEYS", keys.as_str()),
            ],
        ]
        .concat();
        let err = Config::from_lookup(lookup(&both)).unwrap_err();
        assert!(err.contains("PMOS_WEB_DEV_LOGIN_WITH_PASSWORD"), "{err}");
        let both = [both, vec![("PMOS_WEB_DEV_LOGIN_WITH_PASSWORD", "1")]].concat();
        assert!(Config::from_lookup(lookup(&both)).unwrap().password_login);
        assert!(!Config::from_lookup(lookup(&base())).unwrap().password_login);
        let bad = [base(), vec![("PMOS_WEB_PASSWORD_LOGIN", "yes")]].concat();
        assert!(Config::from_lookup(lookup(&bad))
            .unwrap_err()
            .contains("`1` or `0`"));
        // Trusted proxies are addresses.
        let proxies = [base(), vec![("PMOS_WEB_TRUSTED_PROXIES", "10.0.0.1, ::1")]].concat();
        assert_eq!(
            Config::from_lookup(lookup(&proxies))
                .unwrap()
                .trusted_proxies
                .len(),
            2
        );
        let proxies = [base(), vec![("PMOS_WEB_TRUSTED_PROXIES", "proxy.local")]].concat();
        assert!(Config::from_lookup(lookup(&proxies)).is_err());
    }

    /// P2 and L5: password sign-in starts only when its per-address limit is per real address
    /// (a trusted proxy named, or `PMOS_WEB_DIRECT=1` on loopback) and, on a reachable address,
    /// with an `https://` public URL.
    #[test]
    fn password_sign_in_refuses_to_start_without_knowing_the_browsers_address() {
        let key = "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=";
        let keys = format!("web:toolserver:k1:{key}");
        let mut on = base();
        on.retain(|(k, _)| *k != "PMOS_WEB_DEV_LOGIN");
        on.push(("PMOS_WEB_PASSWORD_LOGIN", "1"));
        on.push(("PMOS_ASSERTION_KEYS", keys.as_str()));
        let start = |extra: &[(&'static str, &'static str)]| {
            Config::from_lookup(lookup(&[on.clone(), extra.to_vec()].concat()))
        };
        // Neither a proxy nor direct: refused, naming both settings.
        let err = start(&[]).unwrap_err();
        assert!(
            err.contains("PMOS_WEB_TRUSTED_PROXIES") && err.contains("PMOS_WEB_DIRECT"),
            "{err}"
        );
        // Direct on loopback: the test stack. Direct anywhere else: refused.
        assert!(start(&[("PMOS_WEB_DIRECT", "1")]).is_ok());
        let err =
            start(&[("PMOS_WEB_DIRECT", "1"), ("PMOS_WEB_BIND", "0.0.0.0:8770")]).unwrap_err();
        assert!(err.contains("loopback"), "{err}");
        // Direct beside a proxy list: refused.
        let err = start(&[
            ("PMOS_WEB_DIRECT", "1"),
            ("PMOS_WEB_TRUSTED_PROXIES", "127.0.0.1"),
        ])
        .unwrap_err();
        assert!(err.contains("set one of them"), "{err}");
        // A proxy, as in the deploy kit (0.0.0.0 in a container): needs an https public URL.
        let kit = [
            ("PMOS_WEB_TRUSTED_PROXIES", "172.18.0.1"),
            ("PMOS_WEB_BIND", "0.0.0.0:8770"),
        ];
        let err = start(&kit).unwrap_err();
        assert!(err.contains("https://"), "{err}");
        let err = start(
            &[
                kit.as_slice(),
                &[("PMOS_WEB_PUBLIC_URL", "http://pmos.example.com")],
            ]
            .concat(),
        )
        .unwrap_err();
        assert!(err.contains("https://"), "{err}");
        let cfg = start(
            &[
                kit.as_slice(),
                &[("PMOS_WEB_PUBLIC_URL", "https://pmos.example.com")],
            ]
            .concat(),
        )
        .unwrap();
        assert_eq!(
            cfg.trusted_proxies,
            vec!["172.18.0.1".parse::<IpAddr>().unwrap()]
        );
        // Without password sign-in none of this is asked; a bad flag still is.
        assert!(Config::from_lookup(lookup(&base())).is_ok());
        let err = start(&[("PMOS_WEB_DIRECT", "yes")]).unwrap_err();
        assert!(err.contains("`1` or `0`"), "{err}");
    }

    #[test]
    fn normalizes_urls() {
        let cfg = Config::from_lookup(lookup(
            &[
                base(),
                vec![
                    ("PMOS_BOTS_URL", "http://bots.pmos.internal:8750/"),
                    ("PMOS_WEB_PUBLIC_URL", "https://PMOS.example.com/"),
                ],
            ]
            .concat(),
        ))
        .unwrap();
        assert_eq!(cfg.bots_url, "http://bots.pmos.internal:8750");
        assert_eq!(cfg.public_url.as_deref(), Some("https://pmos.example.com"));
    }

    // --- Slice H10: the admin door and D2 (a)'s allow-list -------------------------------------

    type DoorCase = (
        Vec<(&'static str, &'static str)>,
        Vec<&'static str>,
        &'static str,
    );

    const AK: &str = "web:toolserver:k1:AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=";

    /// Password sign-in on, behind the kit's proxy, with an admin door in tunnel mode.
    fn door_base() -> Vec<(&'static str, &'static str)> {
        vec![
            ("PMOS_WEB_BOTS_TOKEN", BOTS),
            ("PMOS_WEB_TOOLSERVER_TOKEN", TOOLS),
            ("PMOS_WEB_PASSWORD_LOGIN", "1"),
            ("PMOS_ASSERTION_KEYS", AK),
            ("PMOS_WEB_BIND", "0.0.0.0:8770"),
            ("PMOS_WEB_TRUSTED_PROXIES", "10.201.77.1"),
            ("PMOS_WEB_PUBLIC_URL", "https://pmos.example.com"),
            ("PMOS_WEB_ADMIN_BIND", "0.0.0.0:8772"),
            ("PMOS_WEB_ADMIN_URL", "http://pmos-prod.localhost:8772"),
            ("PMOS_WEB_ADMIN_TUNNEL_PEER", "10.201.77.1"),
        ]
    }

    fn door(extra: &[(&'static str, &'static str)], drop: &[&str]) -> Result<Config, String> {
        let mut pairs: Vec<(&str, &str)> = door_base()
            .into_iter()
            .filter(|(k, _)| !drop.contains(k) && !extra.iter().any(|(e, _)| e == k))
            .collect();
        pairs.extend_from_slice(extra);
        Config::from_lookup(lookup(&pairs))
    }

    #[test]
    fn the_admin_door_in_tunnel_mode() {
        let cfg = door(&[], &[]).unwrap();
        let a = cfg.admin.unwrap();
        assert_eq!(a.url, "http://pmos-prod.localhost:8772");
        assert_eq!(a.authority, "pmos-prod.localhost:8772");
        assert_eq!(
            a.mode,
            AdminMode::Tunnel {
                peer: "10.201.77.1".parse().unwrap()
            }
        );
        assert_eq!((a.idle, a.max), (Duration::minutes(30), Duration::hours(8)));
        assert!(!a.entra, "password-only by default");
        // Without the door, nothing of it is set; half of it refuses.
        let none = door(
            &[],
            &[
                "PMOS_WEB_ADMIN_BIND",
                "PMOS_WEB_ADMIN_URL",
                "PMOS_WEB_ADMIN_TUNNEL_PEER",
            ],
        )
        .unwrap();
        assert!(none.admin.is_none());
        let err = door(&[], &["PMOS_WEB_ADMIN_BIND"]).unwrap_err();
        assert!(err.contains("without PMOS_WEB_ADMIN_BIND"), "{err}");
        // Loopback names: localhost, *.localhost, 127.0.0.1, [::1].
        for url in [
            "http://localhost:8772",
            "http://127.0.0.1:8772",
            "http://[::1]:8772",
            "http://PMOS.localhost:9000",
        ] {
            assert!(door(&[("PMOS_WEB_ADMIN_URL", url)], &[]).is_ok(), "{url}");
        }
    }

    #[test]
    fn the_admin_door_refuses_every_unsafe_shape() {
        let cases: Vec<DoorCase> = vec![
            // An http admin URL on a name that isn't loopback.
            (
                vec![("PMOS_WEB_ADMIN_URL", "http://10.0.0.5:8772")],
                vec![],
                "isn't loopback",
            ),
            (
                vec![("PMOS_WEB_ADMIN_URL", "http://pmos-admin.corp:8772")],
                vec![],
                "isn't loopback",
            ),
            (
                vec![("PMOS_WEB_ADMIN_URL", "http://localhost.evil.example:8772")],
                vec![],
                "isn't loopback",
            ),
            // http with a proxy list; https without one.
            (
                vec![("PMOS_WEB_ADMIN_TRUSTED_PROXIES", "10.201.77.1")],
                vec![],
                "tunnel mode",
            ),
            (
                vec![("PMOS_WEB_ADMIN_URL", "https://pmos-admin.corp.example")],
                vec![],
                "PMOS_WEB_ADMIN_TRUSTED_PROXIES",
            ),
            // The tunnel's peer, and an http URL's port, required.
            (
                vec![],
                vec!["PMOS_WEB_ADMIN_TUNNEL_PEER"],
                "PMOS_WEB_ADMIN_TUNNEL_PEER",
            ),
            (
                vec![("PMOS_WEB_ADMIN_URL", "http://pmos-prod.localhost")],
                vec![],
                "write its port",
            ),
            // The public URL's host, on either scheme.
            (
                vec![
                    ("PMOS_WEB_ADMIN_URL", "https://pmos.example.com:8443"),
                    ("PMOS_WEB_ADMIN_TRUSTED_PROXIES", "10.201.77.1"),
                ],
                vec!["PMOS_WEB_ADMIN_TUNNEL_PEER"],
                "PMOS_WEB_PUBLIC_URL's",
            ),
            (
                vec![
                    ("PMOS_WEB_PUBLIC_URL", "http://localhost:8770"),
                    ("PMOS_WEB_DIRECT", "1"),
                ],
                vec!["PMOS_WEB_TRUSTED_PROXIES", "PMOS_WEB_BIND"],
                "",
            ),
            // A path, no URL.
            (
                vec![("PMOS_WEB_ADMIN_URL", "https://a.corp/x")],
                vec![],
                "no path",
            ),
            (vec![], vec!["PMOS_WEB_ADMIN_URL"], "PMOS_WEB_ADMIN_URL"),
            // A public bind; the same bind as the public door.
            (
                vec![("PMOS_WEB_ADMIN_BIND", "203.0.113.7:8772")],
                vec![],
                "never on a public address",
            ),
            (
                vec![("PMOS_WEB_ADMIN_BIND", "0.0.0.0:8770")],
                vec![],
                "must differ",
            ),
            // Session bounds.
            (
                vec![("PMOS_WEB_ADMIN_SESSION_IDLE_MINUTES", "61")],
                vec![],
                "1 to 60",
            ),
            (
                vec![("PMOS_WEB_ADMIN_SESSION_MAX_MINUTES", "721")],
                vec![],
                "1 to 720",
            ),
            (
                vec![
                    ("PMOS_WEB_ADMIN_SESSION_IDLE_MINUTES", "60"),
                    ("PMOS_WEB_ADMIN_SESSION_MAX_MINUTES", "30"),
                ],
                vec![],
                "can't be longer",
            ),
            // Entra on the door without Entra; no provider at all.
            (
                vec![("PMOS_WEB_ADMIN_ENTRA", "1")],
                vec![],
                "needs Entra ID",
            ),
        ];
        for (extra, drop, want) in cases {
            match door(&extra, &drop) {
                Err(e) => assert!(e.contains(want), "{extra:?}: {e}"),
                // The one case above with a blank `want` is the http public URL on
                // `localhost` beside an admin door on `pmos-prod.localhost`: allowed, since
                // the two hosts differ.
                Ok(_) => assert!(want.is_empty(), "{extra:?} {drop:?} started"),
            }
        }
        // The same host on http: refused (stricter than r7 asks: one cookie jar).
        let err = door(
            &[
                ("PMOS_WEB_PUBLIC_URL", "http://localhost:8770"),
                ("PMOS_WEB_ADMIN_URL", "http://localhost:8772"),
                ("PMOS_WEB_DIRECT", "1"),
                ("PMOS_WEB_BIND", "127.0.0.1:8770"),
            ],
            &["PMOS_WEB_TRUSTED_PROXIES"],
        )
        .unwrap_err();
        assert!(err.contains("PMOS_WEB_PUBLIC_URL's"), "{err}");
        // No provider for the door: development sign-in is never served there.
        let mut dev = vec![
            ("PMOS_WEB_DEV_LOGIN", "1"),
            ("PMOS_WEB_BOTS_TOKEN", BOTS),
            ("PMOS_WEB_TOOLSERVER_TOKEN", TOOLS),
            ("PMOS_WEB_ADMIN_BIND", "127.0.0.1:8772"),
            ("PMOS_WEB_ADMIN_URL", "http://pmos-dev.localhost:8772"),
            ("PMOS_WEB_ADMIN_TUNNEL_PEER", "127.0.0.1"),
        ];
        let err = Config::from_lookup(lookup(&dev)).unwrap_err();
        assert!(err.contains("the admin door has no sign-in"), "{err}");
        dev.retain(|(k, _)| !k.starts_with("PMOS_WEB_ADMIN"));
        assert!(Config::from_lookup(lookup(&dev)).unwrap().admin.is_none());
    }

    #[test]
    fn the_admin_door_in_proxy_mode() {
        let cfg = door(
            &[
                ("PMOS_WEB_ADMIN_URL", "https://PMOS-Admin.corp.example"),
                ("PMOS_WEB_ADMIN_TRUSTED_PROXIES", "10.201.77.1, fd00::1"),
                ("PMOS_WEB_ADMIN_SESSION_IDLE_MINUTES", "60"),
                ("PMOS_WEB_ADMIN_SESSION_MAX_MINUTES", "720"),
            ],
            &[],
        )
        .unwrap();
        let a = cfg.admin.unwrap();
        assert_eq!(a.url, "https://pmos-admin.corp.example");
        assert_eq!(a.authority, "pmos-admin.corp.example");
        assert!(matches!(a.mode, AdminMode::Proxy { ref trusted } if trusted.len() == 2));
        assert_eq!(
            (a.idle, a.max),
            (Duration::minutes(60), Duration::hours(12))
        );
        // Its bind: unspecified, loopback or private only.
        for bind in [
            "127.0.0.1:8772",
            "10.1.2.3:8772",
            "100.64.0.9:8772",
            "[fd00::5]:8772",
            "[::]:8772",
        ] {
            assert!(
                door(&[("PMOS_WEB_ADMIN_BIND", bind)], &[]).is_ok(),
                "{bind}"
            );
        }
        for bind in [
            "8.8.8.8:8772",
            "100.128.0.1:8772",
            "[2001:db8::1]:8772",
            "169.254.1.1:8772",
        ] {
            assert!(
                door(&[("PMOS_WEB_ADMIN_BIND", bind)], &[]).is_err(),
                "{bind}"
            );
        }
    }

    #[test]
    fn the_password_allow_list_is_bounded() {
        for ok in [
            "10.1.0.0/16",
            "192.168.4.0/24",
            "203.0.113.7/32",
            "2001:db8:1::/48",
            "2001:db8:1:2::/64",
            "::ffff:10.1.0.0/112",
        ] {
            assert!(parse_password_clients(ok).is_ok(), "{ok}");
        }
        for bad in [
            "0.0.0.0/0",
            "::/0",
            "::ffff:0:0/96",
            "10.0.0.0/15",
            "2001:db8::/47",
            "::ffff:10.0.0.0/111",
        ] {
            let err = parse_password_clients(bad).unwrap_err();
            assert!(err.contains(bad) && err.contains("D2 (a)"), "{bad}: {err}");
        }
        for bad in [
            "10.1.0.0",
            "10.1.0.1/16",
            "10.1.0.0/33",
            "10.1.0.0/016",
            "pmos/16",
        ] {
            assert!(parse_password_clients(bad).is_err(), "{bad}");
        }
        let list = parse_password_clients("10.1.0.0/16, 2001:db8:1::/48").unwrap();
        assert_eq!(list.len(), 2);
        assert!(list[0].contains("10.1.9.9".parse().unwrap()));
        assert!(list[0].contains("::ffff:10.1.9.9".parse().unwrap()));
        assert!(!list[0].contains("10.2.0.0".parse().unwrap()));
        assert!(list[1].contains("2001:db8:1:ffff::1".parse().unwrap()));
        assert!(!list[1].contains("2001:db8:2::1".parse().unwrap()));
        let cfg = door(&[("PMOS_WEB_PASSWORD_CLIENTS", "10.1.0.0/16")], &[]).unwrap();
        assert_eq!(cfg.password_clients.len(), 1);
        assert!(door(&[("PMOS_WEB_PASSWORD_CLIENTS", "0.0.0.0/0")], &[]).is_err());
    }
}
