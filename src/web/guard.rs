//! Keep other web sites away from the API.
//!
//! There is no login, so a page from any site that the user opens could use
//! the browser to call jellydl:
//!
//! - DNS rebinding: a name of the attacker points to 127.0.0.1, and the page
//!   reads and calls the API as if it was on the same site. The `Host` header
//!   then has the name of the attacker: accept only local names, IP addresses
//!   and the names given with `--allow-host`.
//! - Cross-site POST: a form or `fetch` from another site cancels downloads.
//!   Browsers send `Origin` with a POST: it must be the page itself, or a name
//!   given with `--allow-host`. Tools such as curl send no `Origin`.

use std::net::IpAddr;
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

/// The host names that may reach the API, in addition to the local ones.
#[derive(Debug, Clone, Default)]
pub struct HostGuard {
    allowed: Vec<String>,
}

impl HostGuard {
    pub fn new(allowed: impl IntoIterator<Item = String>) -> Self {
        Self {
            allowed: allowed
                .into_iter()
                .map(|h| h.trim().trim_end_matches('.').to_ascii_lowercase())
                .filter(|h| !h.is_empty())
                .collect(),
        }
    }

    fn is_allowed_name(&self, host: &str) -> bool {
        self.allowed.iter().any(|a| a == host)
    }

    /// True when the `Host` header can come from a page of jellydl itself.
    /// A DNS rebinding attack always uses a name, never an IP address.
    pub fn host_ok(&self, authority: &str) -> bool {
        let host = hostname(authority);
        host == "localhost" || host.parse::<IpAddr>().is_ok() || self.is_allowed_name(&host)
    }

    /// True when the `Origin` of a request that changes something is the
    /// page itself (same host and port) or an allowed name.
    pub fn origin_ok(&self, origin: &str, authority: &str) -> bool {
        // "null" (sandboxed frames, some redirects) has no "://".
        let Some((scheme, rest)) = origin.split_once("://") else {
            return false;
        };
        if !matches!(scheme, "http" | "https") {
            return false;
        }
        rest.eq_ignore_ascii_case(authority) || self.is_allowed_name(&hostname(rest))
    }

    /// Why a request must be refused, if it must.
    pub fn check(&self, method: &Method, headers: &header::HeaderMap) -> Result<(), &'static str> {
        let authority = headers
            .get(header::HOST)
            .and_then(|h| h.to_str().ok())
            .unwrap_or("");
        if !self.host_ok(authority) {
            return Err("unknown Host: use --allow-host to add the name of this server");
        }
        let safe = matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS);
        if !safe && let Some(origin) = headers.get(header::ORIGIN) {
            let origin = origin.to_str().unwrap_or("");
            if !self.origin_ok(origin, authority) {
                return Err("request from another site");
            }
        }
        Ok(())
    }
}

/// The host name of `host[:port]` or `[v6]:port`, in lowercase.
fn hostname(authority: &str) -> String {
    let host = if let Some(rest) = authority.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else {
        match authority.rsplit_once(':') {
            Some((h, port)) if port.bytes().all(|b| b.is_ascii_digit()) => h,
            _ => authority,
        }
    };
    host.trim_end_matches('.').to_ascii_lowercase()
}

/// Middleware: refuse the requests that `HostGuard::check` refuses.
pub async fn guard(State(g): State<Arc<HostGuard>>, req: Request, next: Next) -> Response {
    match g.check(req.method(), req.headers()) {
        Ok(()) => next.run(req).await,
        Err(why) => {
            let host = req
                .headers()
                .get(header::HOST)
                .and_then(|h| h.to_str().ok())
                .unwrap_or("");
            let origin = req
                .headers()
                .get(header::ORIGIN)
                .and_then(|h| h.to_str().ok())
                .unwrap_or("");
            tracing::warn!(
                "refused {} {} (Host {host:?}, Origin {origin:?}): {why}",
                req.method(),
                req.uri().path()
            );
            (
                StatusCode::FORBIDDEN,
                axum::Json(serde_json::json!({ "error": why })),
            )
                .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_names() {
        assert_eq!(hostname("LocalHost:8080"), "localhost");
        assert_eq!(hostname("[::1]:8080"), "::1");
        assert_eq!(hostname("[::1]"), "::1");
        assert_eq!(hostname("10.0.0.2"), "10.0.0.2");
        assert_eq!(hostname("nas.local.:80"), "nas.local");
    }

    #[test]
    fn hosts() {
        let g = HostGuard::new(["NAS.local".to_string(), " ".to_string()]);
        for ok in [
            "127.0.0.1:8080",
            "localhost",
            "[::1]:8080",
            "192.168.1.2",
            "nas.local:8080",
        ] {
            assert!(g.host_ok(ok), "{ok}");
        }
        for bad in ["", "evil.net", "localhost.evil.net", "nas.local.evil.net"] {
            assert!(!g.host_ok(bad), "{bad}");
        }
    }

    #[test]
    fn origins() {
        let g = HostGuard::new(["jelly.example.com".to_string()]);
        assert!(g.origin_ok("http://127.0.0.1:8080", "127.0.0.1:8080"));
        assert!(g.origin_ok("http://LOCALHOST:8080", "localhost:8080"));
        assert!(g.origin_ok("https://jelly.example.com", "127.0.0.1:8080"));
        assert!(!g.origin_ok("http://127.0.0.1:9999", "127.0.0.1:8080"));
        assert!(!g.origin_ok("http://203.0.113.7", "127.0.0.1:8080"));
        assert!(!g.origin_ok("null", "127.0.0.1:8080"));
        assert!(!g.origin_ok("file://127.0.0.1:8080", "127.0.0.1:8080"));
    }

    #[test]
    fn only_changes_need_the_origin() {
        let g = HostGuard::default();
        let mut h = header::HeaderMap::new();
        h.insert(header::HOST, "127.0.0.1:8080".parse().unwrap());
        h.insert(header::ORIGIN, "https://evil.net".parse().unwrap());
        // A cross-site GET cannot read the reply (same-origin policy).
        assert!(g.check(&Method::GET, &h).is_ok());
        assert!(g.check(&Method::POST, &h).is_err());
        h.remove(header::ORIGIN);
        assert!(g.check(&Method::POST, &h).is_ok());
    }
}
