//! Small pieces every PM OS service shares, so each service doesn't carry its own copy.

use subtle::ConstantTimeEq;

/// Compare a presented token with the expected one in constant time, so a token can't be
/// guessed byte by byte from response timing. Unequal lengths are rejected without
/// comparing contents (the length of a token is not a secret worth protecting).
pub fn token_eq(presented: &str, expected: &str) -> bool {
    presented.len() == expected.len() && bool::from(presented.as_bytes().ct_eq(expected.as_bytes()))
}

/// The token in an `Authorization: Bearer …` header value, if that is what it is.
pub fn bearer_value(header: &str) -> Option<&str> {
    header.strip_prefix("Bearer ").filter(|t| !t.is_empty())
}

/// Resolves when the process is asked to stop: Ctrl-C (SIGINT) or SIGTERM, which is what
/// `docker compose stop` and systemd send. Each service drains in-flight work after this,
/// rather than being killed mid-write after the grace period.
pub async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut term) => {
                term.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_tokens() {
        assert!(token_eq("abc", "abc"));
        assert!(!token_eq("abc", "abd"));
        assert!(!token_eq("abc", "abcd"));
        assert!(!token_eq("", "abc"));
    }

    #[test]
    fn reads_bearer_headers() {
        assert_eq!(bearer_value("Bearer t0k"), Some("t0k"));
        assert_eq!(bearer_value("Bearer "), None);
        assert_eq!(bearer_value("Basic abc"), None);
    }
}
