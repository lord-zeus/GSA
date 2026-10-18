//! # Geo / IP — GSA's per-proxy country + outgoing-IP verification (Feature #14).
//!
//! The original ships this vocabulary on two axes:
//!
//! * **Country targeting** — the proxy list carries a *Country* per entry
//!   (the proxy export columns are literally `Host, Port, Type, Speed,
//!   Country, Status, Source`), and a project can target a country
//!   (`For what country is this search engine?`, `country=`).  When a
//!   project sets a target country the runner routes its jobs **only**
//!   through proxies in that country.  This module is the pure half of
//!   that seam: IP/publicity helpers and the country-filter policy.
//!
//! * **Outgoing-IP verification** — GSA probes `http://api.ipify.org/
//!   ?format=raw` *through* each proxy so the returned address is the
//!   **egress** IP of that proxy (not the local one).  A private/loopback
//!   "egress" IP means the proxy did not actually change where the traffic
//!   leaves (down, or a broken transparent proxy) — exactly what an
//!   "IP verification" check is for.  The network half (`resolve_ip` over
//!   reqwest) lives in `geo_probe`; this file keeps the parse + validate +
//!   select logic dependency-free and unit-testable offline.

use serde::{Deserialize, Serialize};

/// The outcome of verifying one proxy's outgoing (egress) identity +
/// country.  `ip` is the bare public IP as seen by the probe service;
/// `country` is `Some` only when a geo source returned one (or the row had
/// one stored) — the original's bare `ipify` answer carries only an IP.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct IpInfo {
    /// Bare dotted-quad egress IP (empty when the probe never got one).
    pub ip: String,
    /// Country code/name, when known (`Some`) — `None` when unknown.
    pub country: Option<String>,
    /// Round-trip latency of the egress probe, ms (feeds the proxy
    /// `Speed` column).
    pub speed_ms: i64,
    /// Public (non-private-range) — the real "did the proxy change egress?"
    /// signal.  A loopback/private IP here ⇒ the proxy did not route.
    pub public: bool,
}

/// Parse a bare `ipify` `?format=raw` body (exactly `a.b.c.d\n`) into a
/// trimmed, whitespace-stripped IP string.  Returns `None` when the body is
/// empty or not text (services sometimes return a JSON error object on the
/// plain endpoint).
pub fn parse_ipify_raw(body: &str) -> Option<String> {
    let t = body.trim();
    if t.is_empty() {
        return None;
    }
    // Strip any JSON-quoted form some mirrors return (`"1.2.3.4"`).
    let t = t.trim_matches(|c| c == '"' || c == '\'');
    let t = t.trim();
    if is_ipv4(t) {
        Some(t.to_string())
    } else {
        None
    }
}

/// A strict dotted-quad IPv4 check (four decimal octets, each 0..=255, no
/// leading zeros allowed on the value but tolerated as input leniency).
pub fn is_ipv4(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() != 4 {
        return false;
    }
    for p in parts {
        if p.is_empty() || p.len() > 3 {
            return false;
        }
        let mut n = 0u32;
        for c in p.bytes() {
            if !c.is_ascii_digit() {
                return false;
            }
            n = n * 10 + (c - b'0') as u32;
        }
        if n > 255 {
            return false;
        }
    }
    true
}

/// Is an IPv4 address **public** (routable)?  Everything the original would
/// treat as "the traffic did not actually leave the local box" returns
/// `false`: loopback (127/8), the 10/8 + 172.16/12 + 192.168/16 private
/// blocks, `0.0.0.0`, `255.255.255.255`, and link-local 169.254/16.  This is
/// the core IP-verification predicate: a *proxied* session that egresses to
/// one of these is a proxy that isn't proxying.
pub fn is_public_ipv4(ip: &str) -> bool {
    let parts: Vec<u32> = ip.split('.').filter_map(|p| p.parse::<u32>().ok()).collect();
    if parts.len() != 4 {
        return false;
    }
    let (a, b) = (parts[0], parts[1]);
    match a {
        0 | 127 | 169 => false,                      // 0.0.0.0/8, 127/8, 169.254/16
        10 => false,                                  // 10.0.0.0/8
        172 if (b as u16) >= 16 && (b as u16) <= 31 => false,        // 172.16.0.0/12
        192 if b == 168 => false,                     // 192.168.0.0/16
        255 => false,                                 // broadcast
        _ => true,
    }
}

/// Normalize a country string for comparison: trim, collapse, lowercase,
/// strip surrounding punctuation.  `"Us"`, `"US "`, `"(USA)"` all compare
/// equal to `"us"`.  Empty/`"-"` normalizes to `None` (unknown).
pub fn normalize_country(c: &str) -> Option<String> {
    let t = c.trim();
    if t.is_empty() || t == "-" || t.eq_ignore_ascii_case("unknown") || t.eq_ignore_ascii_case("n/a") {
        return None;
    }
    // Drop surrounding parens / quotes that some proxy-list exports wrap.
    let t = t.trim_matches(|ch| ch == '(' || ch == ')' || ch == '"' || ch == '\'');
    let t = t.trim();
    let collapsed: String = t.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        None
    } else {
        Some(collapsed.to_ascii_lowercase())
    }
}

/// Case/punct-insensitive country equality (`None` never matches `Some`).
pub fn country_matches(a: Option<&str>, b: Option<&str>) -> bool {
    match (normalize_country(a.unwrap_or("")), normalize_country(b.unwrap_or(""))) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

/// Pure policy: given a set of proxies each with an (optional) stored
/// country and a project target country, select the indexes to use.
///
/// * target `None`  → **all** proxies (no geo constraint; previous behavior).
/// * target `Some`  → only proxies whose stored country matches (case-
///   insensitive).  A proxy with **no** stored country is *excluded* under a
///   geo target — we never assume a location we can't see (GSA's
///   "skip country list" — when the target is set and nothing matches,
///   the job simply runs direct rather than on a wrong-country proxy).
pub fn select_proxies_for_country<'a>(
    proxies: &'a [ProxyGeo],
    target_country: Option<&str>,
) -> Vec<&'a ProxyGeo> {
    match target_country {
        None => proxies.iter().collect(),
        Some(tok) => proxies
            .iter()
            .filter(|p| country_matches(Some(p.country.as_str()), Some(tok)))
            .collect(),
    }
}

/// A proxy annotated with its (optional) known country — the unit the
/// `select_proxies_for_country` policy filters over.  Kept here (pure)
/// rather than in `db` so the policy is testable without Postgres.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyGeo {
    pub host: String,
    pub port: u16,
    /// Known country (empty = unknown).  Populated from a stored `country`
    /// column or a geo lookup; `select_proxies_for_country` treats empty as
    /// "not a match under a geo target".
    pub country: String,
}

impl ProxyGeo {
    pub fn new(host: impl Into<String>, port: u16, country: impl Into<String>) -> Self {
        Self { host: host.into(), port, country: country.into() }
    }
}

// ===========================================================================
//  Unit tests — pure policy only.  The reqwest egress probe (network) is
//  covered by `geo_probe` tests against a local mock, mirroring F#13.
// ===========================================================================
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ipify_raw_accepts_bare_and_quoted() {
        assert_eq!(parse_ipify_raw("203.0.113.7"), Some("203.0.113.7".into()));
        assert_eq!(parse_ipify_raw("  8.8.8.8\n"), Some("8.8.8.8".into()));
        assert_eq!(parse_ipify_raw("\"192.0.2.10\""), Some("192.0.2.10".into()));
        assert_eq!(parse_ipify_raw("  "), None);
        assert_eq!(parse_ipify_raw(""), None);
        assert_eq!(parse_ipify_raw("error: too many requests"), None);
        assert_eq!(parse_ipify_raw("999.0.0.1"), None, "octet >255 is not an ipv4");
    }

    #[test]
    fn is_ipv4_strict_octets() {
        assert!(is_ipv4("1.2.3.4"));
        assert!(is_ipv4("0.0.0.0"));
        assert!(is_ipv4("255.255.255.255"));
        assert!(!is_ipv4("1.2.3"));
        assert!(!is_ipv4("1.2.3.4.5"));
        assert!(!is_ipv4("1.2.3.abc"));
        assert!(!is_ipv4("1.2.3.256"));
        assert!(!is_ipv4("abc"));
        assert!(!is_ipv4(""));
    }

    #[test]
    fn is_public_ipv4_classifies_rfcs() {
        assert!(is_public_ipv4("8.8.8.8"));
        assert!(is_public_ipv4("1.1.1.1"));
        assert!(is_public_ipv4("203.0.113.7"));   // 198.51.100.0/24-ish public
        assert!(!is_public_ipv4("127.0.0.1"));    // loopback
        assert!(!is_public_ipv4("10.0.0.5"));     // 10/8
        assert!(!is_public_ipv4("172.16.0.1"));   // 172.16/12
        assert!(!is_public_ipv4("172.31.255.255"));   // 172.16/12 high edge
        assert!(is_public_ipv4("172.32.0.1"), "172.32/12 and beyond is public");
        assert!(!is_public_ipv4("192.168.1.1"));  // 192.168/16
        assert!(!is_public_ipv4("169.254.10.10"));// link-local
        assert!(!is_public_ipv4("0.0.0.0"));
        assert!(!is_public_ipv4("255.255.255.255"));
        assert!(!is_public_ipv4("not-an-ip"));
    }

    #[test]
    fn is_public_ipv4_reflects_on_ipinfo() {
        // The egress-IP contract: a proxy that reports a private IP means it
        // did not change egress — the whole point of IP verification.
        let ipinfo = IpInfo { ip: "10.2.0.3".into(), country: Some("us".into()), speed_ms: 5, public: is_public_ipv4("10.2.0.3") };
        assert!(!ipinfo.public, "private egress ip must read as not-public");
        let ok = IpInfo { ip: "74.125.143.101".into(), country: Some("us".into()), speed_ms: 12, public: is_public_ipv4("74.125.143.101") };
        assert!(ok.public, "genuine public egress ip must read as public");
    }

    #[test]
    fn normalize_country_is_lenient() {
        assert_eq!(normalize_country("US"), Some("us".into()));
        assert_eq!(normalize_country("  (USA)  "), Some("usa".into()));
        assert_eq!(normalize_country("United States"), Some("united states".into()));
        assert_eq!(normalize_country(""), None);
        assert_eq!(normalize_country("-"), None);
        assert_eq!(normalize_country("unknown"), None);
        assert_eq!(normalize_country("  "), None);
    }

    #[test]
    fn country_matching_is_case_and_punct_insensitive() {
        assert!(country_matches(Some("US"), Some("us")));
        assert!(country_matches(Some("(USA)"), Some("usa")));
        assert!(!country_matches(Some("US"), Some("UK")));
        assert!(!country_matches(None, Some("us")), "unknown never matches a target");
        assert!(!country_matches(Some("us"), None), "unknown never matches a target");
    }

    #[test]
    fn select_all_when_no_target() {
        let p = vec![
            ProxyGeo::new("a", 1, "US"),
            ProxyGeo::new("b", 2, "UK"),
            ProxyGeo::new("c", 3, ""),
        ];
        assert_eq!(select_proxies_for_country(&p, None).len(), 3);
    }

    #[test]
    fn select_only_matching_country_when_target_set() {
        let p = vec![
            ProxyGeo::new("a", 1, "US"),              // alias — not exact-equal to target
            ProxyGeo::new("b", 2, "UK"),
            ProxyGeo::new("c", 3, "united states"), // case variant of the exact target
            ProxyGeo::new("d", 4, ""),              // unknown → excluded
        ];
        let sel = select_proxies_for_country(&p, Some("United States"));
        let hosts: Vec<_> = sel.iter().map(|x| x.host.as_str()).collect();
        // Matching is case/punct-insensitive exact-equality on the normalized
        // string: "united states" == "United States", but the "US" *alias* is
        // a different string and is not expanded (GSA stores the full name or
        // the same token, not a canonical alias table).
        assert_eq!(hosts, vec!["c"], "only the exact case-variant US matches; the US alias is a different token: {hosts:?}");
    }

    #[test]
    fn select_empty_when_no_proxy_matches_target() {
        let p = vec![ProxyGeo::new("a", 1, "FR")];
        assert!(select_proxies_for_country(&p, Some("jp")).is_empty(),
            "GSA 'skip country list': no matching proxy → job runs direct");
    }

    #[test]
    fn ipinfo_round_trips_serde() {
        let i = IpInfo { ip: "1.2.3.4".into(), country: Some("de".into()), speed_ms: 42, public: true };
        let j = serde_json::to_string(&i).unwrap();
        let b: IpInfo = serde_json::from_str(&j).unwrap();
        assert_eq!(b, i);
    }
}
