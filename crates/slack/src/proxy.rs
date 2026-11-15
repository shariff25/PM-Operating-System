//! The Socket Mode websocket through an HTTP proxy.
//!
//! The Slack Web API calls go through `reqwest`, which follows `HTTPS_PROXY` by itself. The
//! websocket does not, so on a network where outbound HTTPS must go through a proxy (a
//! corporate network, a cloud sandbox) it is opened here: an HTTP `CONNECT` tunnel to the
//! proxy, then TLS and the websocket handshake inside it. `NO_PROXY` is honoured, and a
//! plain `ws://` URL (the tests' fake Slack) always connects directly unless a proxy is
//! passed in explicitly.

use base64::Engine;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

pub type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// The proxy to use for `url`, from `HTTPS_PROXY` / `https_proxy` and `NO_PROXY` /
/// `no_proxy`. `None` for `ws://` URLs, hosts under `NO_PROXY`, or no proxy set.
pub fn proxy_for(url: &str, get: impl Fn(&str) -> Option<String>) -> Option<reqwest::Url> {
    let target = reqwest::Url::parse(url).ok()?;
    if target.scheme() != "wss" {
        return None;
    }
    let host = target.host_str()?.to_ascii_lowercase();
    let proxy = get("HTTPS_PROXY")
        .or_else(|| get("https_proxy"))
        .filter(|p| !p.is_empty())?;
    let no_proxy = get("NO_PROXY")
        .or_else(|| get("no_proxy"))
        .unwrap_or_default();
    for entry in no_proxy.split(',').map(|e| e.trim().to_ascii_lowercase()) {
        let entry = entry.trim_start_matches('.');
        if entry == "*"
            || (!entry.is_empty() && (host == entry || host.ends_with(&format!(".{entry}"))))
        {
            return None;
        }
    }
    reqwest::Url::parse(&proxy).ok()
}

/// Open the websocket at `url`, through `proxy` if given.
pub async fn connect(url: &str, proxy: Option<&reqwest::Url>) -> Result<Ws, String> {
    let Some(proxy) = proxy else {
        return tokio_tungstenite::connect_async(url)
            .await
            .map(|(ws, _)| ws)
            .map_err(|e| e.to_string());
    };
    if proxy.scheme() != "http" {
        return Err(format!(
            "proxy scheme `{}` is not supported; use http://",
            proxy.scheme()
        ));
    }
    let target = reqwest::Url::parse(url).map_err(|e| e.to_string())?;
    let host = target.host_str().ok_or("the websocket URL has no host")?;
    let port = target
        .port_or_known_default()
        .ok_or("the websocket URL has no port")?;
    let proxy_addr = format!(
        "{}:{}",
        proxy.host_str().ok_or("the proxy URL has no host")?,
        proxy.port_or_known_default().unwrap_or(80)
    );
    let mut stream = TcpStream::connect(&proxy_addr)
        .await
        .map_err(|e| format!("connecting to the proxy: {e}"))?;
    let mut request = format!("CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\n");
    if !proxy.username().is_empty() {
        let user = percent_decode(proxy.username());
        let pass = percent_decode(proxy.password().unwrap_or_default());
        let auth = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}"));
        request.push_str(&format!("Proxy-Authorization: Basic {auth}\r\n"));
    }
    request.push_str("\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|e| format!("writing to the proxy: {e}"))?;

    // Read the proxy's answer up to the blank line; nothing after it is ours to read.
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() > 8 * 1024 {
            return Err("the proxy's answer is too long".into());
        }
        let n = stream
            .read(&mut byte)
            .await
            .map_err(|e| format!("reading from the proxy: {e}"))?;
        if n == 0 {
            return Err("the proxy closed the connection".into());
        }
        head.push(byte[0]);
    }
    let status_line = String::from_utf8_lossy(&head);
    let status_line = status_line.lines().next().unwrap_or_default();
    let ok = status_line
        .split_whitespace()
        .nth(1)
        .is_some_and(|code| code == "200");
    if !ok {
        return Err(format!("the proxy refused the tunnel: {status_line}"));
    }
    tokio_tungstenite::client_async_tls(url, stream)
        .await
        .map(|(ws, _)| ws)
        .map_err(|e| e.to_string())
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use tokio::net::TcpListener;
    use tokio_tungstenite::tungstenite::Message;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn the_proxy_applies_to_wss_outside_no_proxy() {
        let e = [
            ("HTTPS_PROXY", "http://127.0.0.1:3128"),
            ("NO_PROXY", "localhost,.internal"),
        ];
        assert_eq!(
            proxy_for("wss://wss-primary.slack.com/link", env(&e))
                .unwrap()
                .as_str(),
            "http://127.0.0.1:3128/"
        );
        assert!(
            proxy_for("ws://127.0.0.1:9/link", env(&e)).is_none(),
            "plain ws is direct"
        );
        assert!(proxy_for("wss://localhost/x", env(&e)).is_none());
        assert!(proxy_for("wss://bots.pmos.internal/x", env(&e)).is_none());
        assert!(proxy_for("wss://wss-primary.slack.com/link", env(&[])).is_none());
        let lower = [("https_proxy", "http://p:1")];
        assert!(proxy_for("wss://slack.com", env(&lower)).is_some());
        assert_eq!(percent_decode("a%40b"), "a@b");
    }

    /// A CONNECT proxy that tunnels to `target`, recording what it was asked for.
    async fn fake_proxy(target: String) -> (String, tokio::sync::oneshot::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut client, _) = listener.accept().await.unwrap();
            let mut head = Vec::new();
            let mut b = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                client.read_exact(&mut b).await.unwrap();
                head.push(b[0]);
            }
            let _ = tx.send(String::from_utf8_lossy(&head).into_owned());
            let mut upstream = TcpStream::connect(&target).await.unwrap();
            client
                .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                .await
                .unwrap();
            let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
        });
        (format!("http://user:p%40ss@{addr}"), rx)
    }

    #[tokio::test]
    async fn a_websocket_opens_through_a_connect_tunnel() {
        // An echo websocket server.
        let ws_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ws_addr = ws_listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (s, _) = ws_listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(s).await.unwrap();
            while let Some(Ok(m)) = ws.next().await {
                if m.is_text() {
                    ws.send(m).await.unwrap();
                }
            }
        });
        let (proxy, asked) = fake_proxy(ws_addr.to_string()).await;
        let proxy = reqwest::Url::parse(&proxy).unwrap();
        let mut ws = connect(&format!("ws://{ws_addr}/link"), Some(&proxy))
            .await
            .unwrap();
        ws.send(Message::Text("hello".into())).await.unwrap();
        let back = ws.next().await.unwrap().unwrap();
        assert_eq!(back.into_text().unwrap().as_str(), "hello");
        let head = asked.await.unwrap();
        assert!(
            head.starts_with(&format!("CONNECT {ws_addr} HTTP/1.1\r\n")),
            "{head}"
        );
        let auth = base64::engine::general_purpose::STANDARD.encode("user:p@ss");
        assert!(
            head.contains(&format!("Proxy-Authorization: Basic {auth}")),
            "{head}"
        );
    }

    #[tokio::test]
    async fn a_refused_tunnel_is_an_error() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut c, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 512];
            let _ = c.read(&mut buf).await;
            c.write_all(b"HTTP/1.1 403 Forbidden\r\n\r\n")
                .await
                .unwrap();
        });
        let proxy = reqwest::Url::parse(&format!("http://{addr}")).unwrap();
        let err = connect("wss://wss-primary.slack.com/link", Some(&proxy))
            .await
            .unwrap_err();
        assert!(err.contains("403"), "{err}");
    }
}
