//! # Proxies — GSA's `proxy list/type/test` vocabulary (Feature #13).
//!
//! GSA SER routes submissions through user-supplied proxy servers:
//! `proxy type=` (http/socks5), `proxy list=`, `proxy test`, `rnd proxy
//! list`, `proxy seems down`.  This module is the **pure** half of the
//! feature — value types shared by db/runner/api and the proxy-rotation
//! logic — with no I/O.  Connectivity *testing* (real sockets) is a
//! `#[cfg(feature = "http")]` function in `proxy_test`, built on plain
//! TCP + minimal HTTP/1.1 and SOCKS5 frames so it works against any
//! listener, including the mock proxy used by the E2E proof.
//!
//! Rotation model (matches the original): the runner holds the enabled
//! proxy list and picks **round-robin per job** ([`RoundRobin::pick`]);
//! a job that fails with a proxy-level error is retried once on the
//! *next* proxy in rotation ("down proxy → skip to next").

use serde::{Deserialize, Serialize};

/// A single proxy endpoint as used at runtime (transport-neutral: the
/// runner hands this to the HTTP driver and to the connectivity test).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxySpec {
    /// Hostname or IP.
    pub host: String,
    /// Port.
    pub port: u16,
    /// "http" or "socks5" (normalized, case-insensitive input).
    pub proxy_kind: String,
    /// Optional BASIC-auth username.
    pub login: String,
    /// Optional BASIC-auth password.
    pub password: String,
}

impl ProxySpec {
    /// Normalize a user-supplied kind string to `http` or `socks5`
    /// (accepts `socks`, `SOCKS5`, ... the original's loose vocabulary).
    pub fn normalize_kind(s: &str) -> String {
        let l = s.trim().to_ascii_lowercase();
        if l == "socks5" || l == "socks" {
            "socks5".to_string()
        } else {
            "http".to_string()
        }
    }

    /// `url`-form for HTTP clients (e.g. `reqwest::Proxy`).
    pub fn url(&self) -> String {
        format!("{}://{}:{}", self.proxy_kind, self.host, self.port)
    }
}

/// Round-robin selector over a fixed proxy list.  `Pick` carries the
/// proxy *index* so an attempt that failed on proxy N can be retried on
/// `(N + 1) % len` — "down → next" exactly as GSA behaves.
#[derive(Debug, Default)]
pub struct RoundRobin {
    next: std::sync::atomic::AtomicUsize,
}

impl RoundRobin {
    pub fn new() -> Self {
        Self::default()
    }

    /// Pick the next proxy for a job; `None` when the list is empty.
    pub fn pick<'a>(&self, proxies: &'a [ProxySpec]) -> Option<(usize, &'a ProxySpec)> {
        if proxies.is_empty() {
            return None;
        }
        let i = self
            .next
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            % proxies.len();
        Some((i, &proxies[i]))
    }

    /// The proxy *after* a failed attempt at `failed_index`
    /// (wrap-around; same proxy when the list has only one entry).
    pub fn next_after<'a>(&self, proxies: &'a [ProxySpec], failed_index: usize) -> Option<&'a ProxySpec> {
        if proxies.is_empty() {
            return None;
        }
        Some(&proxies[(failed_index + 1) % proxies.len()])
    }
}

// ===========================================================================
//  Unit tests — rotation semantics only (the socket I/O is covered by
//  `proxy_test` tests and the runner E2E via the mock proxy in scripts/).
// ===========================================================================
#[cfg(test)]
mod tests {
    use super::*;

    fn spec(h: &str, p: u16, k: &str) -> ProxySpec {
        ProxySpec {
            host: h.into(),
            port: p,
            proxy_kind: k.into(),
            login: String::new(),
            password: String::new(),
        }
    }

    #[test]
    fn round_robin_cycles_in_order_and_wraps() {
        let rr = RoundRobin::new();
        let list = [spec("p1", 8080, "http"), spec("p2", 8081, "http"), spec("p3", 8082, "socks5")];
        let picks: Vec<_> = (0..7).map(|_| rr.pick(&list).map(|(i, _)| i).unwrap()).collect();
        assert_eq!(picks, vec![0, 1, 2, 0, 1, 2, 0]);
    }

    #[test]
    fn round_robin_empty_list_is_none() {
        let rr = RoundRobin::new();
        let list: Vec<ProxySpec> = vec![];
        assert_eq!(rr.pick(&list), None);
        assert_eq!(rr.next_after(&list, 0), None);
    }

    #[test]
    fn next_after_wraps_to_first() {
        let rr = RoundRobin::new();
        let list = [spec("p1", 1, "http"), spec("p2", 2, "http")];
        assert_eq!(rr.next_after(&list, 1).unwrap().host, "p1");
        assert_eq!(rr.next_after(&list, 0).unwrap().host, "p2");
    }

    #[test]
    fn next_after_single_entry_returns_same_proxy() {
        let rr = RoundRobin::new();
        let list = [spec("only", 9, "socks5")];
        assert_eq!(rr.next_after(&list, 0).unwrap().host, "only");
    }

    #[test]
    fn kind_normalization_is_lenient() {
        assert_eq!(ProxySpec::normalize_kind("SOCKS5"), "socks5");
        assert_eq!(ProxySpec::normalize_kind("socks"), "socks5");
        assert_eq!(ProxySpec::normalize_kind(" HTTP "), "http");
        assert_eq!(ProxySpec::normalize_kind(""), "http");
        let p = spec("h", 3128, "socks5");
        assert_eq!(p.url(), "socks5://h:3128");
    }

    #[test]
    fn spec_round_trips_through_serde() {
        let p = ProxySpec {
            host: "corp-proxy.example".into(),
            port: 8080,
            proxy_kind: "http".into(),
            login: "ops".into(),
            password: "s3cret".into(),
        };
        let j = serde_json::to_string(&p).unwrap();
        let back: ProxySpec = serde_json::from_str(&j).unwrap();
        assert_eq!(back, p);
    }
}
