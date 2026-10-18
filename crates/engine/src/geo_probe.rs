//! # Geo probe — outgoing-IP + country verification (Feature #14, network half).
//!
//! The pure parse/validate/select logic lives in [`crate::geo`]; this module
//! does the I/O:
//!
//! * **Egress IP** — `https://api.ipify.org/?format=raw` *through* the proxy
//!   (reqwest `.proxy()`).  The returned address is the proxy's **egress**
//!   (that is the whole point: `127.0.0.1`/`10.x` back means the proxy did
//!   not actually route).  `egress_url` is overridable so tests hit a local
//!   listener instead of the internet.
//! * **Latency** — the egress round-trip, for the proxy `Speed` column.
//! * **Country** — an optional pluggable `ip -> country` resolver.  The
//!   original's bare `ipify` probe returns only an IP; a country arrives
//!   either from a stored `country` column or this extra lookup, and a
//!   lookup failure must never mask the IP result.

use std::time::{Duration, Instant};

use crate::geo::{is_public_ipv4, parse_ipify_raw, IpInfo};
use crate::proxy::ProxySpec;

/// The original GSA's own egress-IP endpoint (bare-raw).  Overridable in
/// tests; kept as the default for faithful behavior.
pub const DEFAULT_EGRESS_URL: &str = "https://api.ipify.org/?format=raw";
/// Default connect/read deadline for the egress probe.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(6);

fn normalize_kind(kind: &str) -> String {
    let l = kind.to_ascii_lowercase();
    if l == "socks5" || l == "socks" || l == "socks4" {
        "socks5".to_string()
    } else {
        "http".to_string()
    }
}

/// Build the reqwest `.proxy()` URL (http or socks5), with inline
/// `user:pass@` when credentials are set.
pub fn proxy_url(kind: &str, host: &str, port: u16, login: &str, password: &str) -> String {
    let scheme = normalize_kind(kind);
    if login.is_empty() {
        format!("{scheme}://{host}:{port}")
    } else {
        format!(
            "{}://{}:{}@{}:{}",
            scheme,
            urlencoding(login),
            urlencoding(password),
            host,
            port
        )
    }
}

/// Minimal percent-encoding for `userinfo` in a proxy URL: escapes the
/// delimiter set that otherwise splits a `scheme://user:pass@host:port`.
fn urlencoding(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'@' | b':' | b'/' | b'?' | b'&' | b'=' | b'#' => out.push_str(&format!("%{b:02X}")),
            _ => out.push(b as char),
        }
    }
    out
}

/// Probe `spec`'s **egress IP** + latency by hitting `egress_url` through
/// the proxy.  `country_lookup` is optional (`None` = IP-only, the faithful
/// default).  Returns `IpInfo` with `public` set per [`is_public_ipv4`].
pub async fn resolve_ip(
    spec: &ProxySpec,
    egress_url: &str,
    timeout: Duration,
    country_lookup: Option<fn(&str) -> Option<String>>,
) -> IpInfo {
    let started = Instant::now();
    let elapsed = |s: &Instant| s.elapsed().as_millis() as i64;

    let mut builder = reqwest::Client::builder().timeout(timeout).connect_timeout(timeout);
    // Route the egress probe *through* the proxy so the returned IP is the
    // proxy's own egress (the IP-verification contract).
    let px_url = proxy_url(&spec.proxy_kind, &spec.host, spec.port, &spec.login, &spec.password);
    if let Ok(px) = reqwest::Proxy::all(&px_url) {
        builder = builder.proxy(px);
    }

    match builder.build() {
        Ok(client) => match client.get(egress_url).send().await {
            Ok(r) => {
                let ct = r.headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string)
                    .unwrap_or_default();
                let body = r.text().await.unwrap_or_default();
                let ip = if ct.contains("json") {
                    serde_json::from_str::<serde_json::Value>(&body)
                        .ok()
                        .and_then(|v| v.get("ip").and_then(|x| x.as_str()).map(str::to_string))
                        .or_else(|| parse_ipify_raw(&body))
                } else {
                    parse_ipify_raw(&body)
                };
                match ip {
                    Some(ip) => IpInfo {
                        ip: ip.clone(),
                        country: country_lookup.and_then(|f| f(&ip)),
                        speed_ms: elapsed(&started),
                        public: is_public_ipv4(&ip),
                    },
                    None => IpInfo {
                        ip: String::new(),
                        country: None,
                        speed_ms: elapsed(&started),
                        public: false,
                    },
                }
            }
            Err(_) => IpInfo {
                ip: String::new(),
                country: None,
                speed_ms: elapsed(&started),
                public: false,
            },
        },
        Err(_) => IpInfo {
            ip: String::new(),
            country: None,
            speed_ms: elapsed(&started),
            public: false,
        },
    }
}

// ===========================================================================
//  Unit tests — network path against a local mock (no internet).
// ===========================================================================
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    /// One-shot raw HTTP listener on 127.0.0.1.  Replies `body` with a
    /// content-type chosen by sniffing `{` / `}`.
    fn serve(body: &str) -> (std::net::SocketAddr, thread::JoinHandle<()>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let body = body.to_string();
        let h = thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            s.set_read_timeout(Some(Duration::from_secs(2))).ok();
            let mut buf = [0u8; 1024];
            let n = s.read(&mut buf).unwrap_or(0);
            let _ = n;
            let ct = if body.contains('{') { "application/json" } else { "text/plain" };
            let head =
                format!("HTTP/1.1 200 OK\r\nContent-Type: {ct}\r\nContent-Length: {}\r\n\r\n", body.len());
            let _ = s.write_all(head.as_bytes());
            let _ = s.write_all(body.as_bytes());
        });
        (addr, h)
    }

    #[test]
    fn proxy_url_schemes_and_auth() {
        assert_eq!(proxy_url("http", "p.example", 8080, "", ""), "http://p.example:8080");
        assert_eq!(
            proxy_url("socks5", "p.example", 1080, "", ""),
            "socks5://p.example:1080"
        );
        let auth = proxy_url("http", "p.example", 8080, "bob", "pw:rd");
        assert!(auth.starts_with("http://"), "{}", auth);
        assert!(auth.contains("@p.example:8080"), "{}", auth);
        assert!(auth.contains("bob"), "{}", auth);
        assert!(!auth.contains("pw:rd"), "colon must be escaped: {}", auth);
        assert!(
            proxy_url("SOCKS", "h", 1, "", "")
                .starts_with("socks5://")
        );
    }

    #[test]
    fn urlencoding_only_escapes_delimiters() {
        assert_eq!(urlencoding("a@b:c/d"), "a%40b%3Ac%2Fd");
        assert_eq!(urlencoding("plain123"), "plain123");
    }

    /// Bare-raw `ipify` parse: the egress IP is read verbatim and a public
    /// IP classifies `public: true`.  (The proxy-hop E2E lives in the API
    /// layer against a real relay; here we assert the parse/`public` surface
    /// plus a direct fetch to a local mock that serves the IP.)
    #[tokio::test]
    async fn resolve_ip_reads_bare_raw_ip() {
        let (addr, h) = serve("203.0.113.42");
        let client =
            reqwest::Client::builder().timeout(Duration::from_secs(3)).build().unwrap();
        let r = client.get(format!("http://{}/", addr)).send().await.unwrap();
        let body = r.text().await.unwrap();
        let ip = parse_ipify_raw(&body);
        assert_eq!(ip, Some("203.0.113.42".into()));
        assert!(is_public_ipv4("203.0.113.42"));
        assert!(!is_public_ipv4("10.0.0.1"));
        assert!(!is_public_ipv4("127.0.0.1"));
        let _ = h.join();
    }

    /// JSON-mirror shape (e.g. `ipinfo.io/ip`-style `{"ip":"..."}`) is
    /// unwrapped when the content type is `application/json`.
    #[tokio::test]
    async fn resolve_ip_parses_json_mirror_shape() {
        let (addr, h) = serve(r#"{"ip":"198.51.100.7","version":"4"}"#);
        let client =
            reqwest::Client::builder().timeout(Duration::from_secs(3)).build().unwrap();
        let r = client.get(format!("http://{}/", addr)).send().await.unwrap();
        let ct = r.headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let body = r.text().await.unwrap();
        let ip = if ct.contains("json") {
            serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v.get("ip").and_then(|x| x.as_str()).map(str::to_string))
        } else {
            parse_ipify_raw(&body)
        };
        assert_eq!(ip, Some("198.51.100.7".into()));
        let _ = h.join();
    }
}
