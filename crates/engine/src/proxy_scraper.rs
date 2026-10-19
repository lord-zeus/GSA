//! # Proxy list scraper — F#24 (GSA "add free proxies" toolbelt).
//!
//! GSA SER's proxy list feature ingests `host:port` rows from raw text or
//! from free-proxy publisher HTML.  This module is the pure parsing half of
//! that (connectivity check stays in F#13's `proxy_test::probe`):
//!
//! * [`ParsedProxy`] — a validated `host:port` row.
//! * [`parse_line`] — one line / one cell → `host:port` (`None` if not a
//!   valid endpoint).  Accepts `host:port`, `user:pass@host:port`,
//!   `scheme://[user[:pass]@]host:port[...]`.  IPv4-style hosts (4 dotted
//!   all-digit octets) reject out-of-range octets.
//! * [`parse_proxy_list`] — whole text or HTML.  HTML covers both
//!   "endpoint per cell" and "host cell + port cell next to it" (the
//!   free-proxy-list.net shape).
//! * [`known_sources`] — the default source catalog (verified live
//!   2026-10-19 to be non-bot-walled).
//! * [`scrape`] — the network half, gated on the `http` feature (reqwest).
//!
//! Persistence is the API layer's job (bulk import, duplicate-tolerant);
//! the promotion path (scraped → live) is the existing F#13
//! `POST /api/proxies/{id}/test` + F#14 geo-verify.

use serde::Serialize;

/// One parsed, shape-validated proxy row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ParsedProxy {
    pub host: String,
    pub port: u16,
}

impl ParsedProxy {
    /// `host:port` endpoint form (GSA's `proxy list=` shape).
    pub fn endpoint(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

/// A proxy-list source: where to fetch + protocol + document format.
#[derive(Debug, Clone, Copy)]
pub struct Source {
    pub name: &'static str,
    pub url: &'static str,
    /// "http" | "socks5" | "any".
    pub proto: &'static str,
    /// "text" (one endpoint per line) | "html" (table/rows).
    pub format: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceInfo {
    pub name: String,
    pub url: String,
    pub proto: String,
    pub format: String,
}

impl From<&Source> for SourceInfo {
    fn from(s: &Source) -> Self {
        Self {
            name: s.name.into(),
            url: s.url.into(),
            proto: s.proto.into(),
            format: s.format.into(),
        }
    }
}

/// Sources verified reachable (non-bot-walled) on 2026-10-19.
pub fn known_sources() -> Vec<Source> {
    vec![
        Source {
            name: "free-proxy-list.net",
            url: "https://free-proxy-list.net/",
            proto: "any",
            format: "html",
        },
        Source {
            name: "TheSpeedX HTTP",
            url: "https://raw.githubusercontent.com/TheSpeedX/PROXY-List/master/http.txt",
            proto: "http",
            format: "text",
        },
        Source {
            name: "TheSpeedX SOCKS5",
            url: "https://raw.githubusercontent.com/TheSpeedX/PROXY-List/master/socks.txt",
            proto: "socks5",
            format: "text",
        },
    ]
}

/// Parse one line/cell of text.
/// Accepts (in order):
///  * `host:port` (bare)
///  * `user:pass@host:port`
///  * `scheme://[user[:pass]@]host:port[/path]`
/// Returns `None` when the line doesn't look like a proxy endpoint.
pub fn parse_line(input: &str) -> Option<ParsedProxy> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }
    // Drop the `scheme://` prefix if present.
    let rest = s.split("://").last().unwrap_or(s);
    // Drop optional `user[:pass]@` prefix.
    let hostpart = if let Some(atpos) = rest.rfind('@') {
        &rest[atpos + 1..]
    } else {
        rest
    };
    // Drop trailing path/fragment (keep query? no — endpoint never has one).
    let hostport = hostpart.split(['/', '#']).next().unwrap_or(hostpart);

    // Split off the port: for IPv6 `[..]:port` handle separately; else
    // `<host>:<port>` (rsplit on the LAST colon).
    let (host, port) = if hostport.starts_with('[') {
        let close = hostport.find(']')?;
        let host = hostport[1..close].to_string();
        let rest_after = &hostport[close + 1..];
        let portpart = rest_after.strip_prefix(':')?;
        (host, portpart.parse::<u16>().ok()?)
    } else {
        let (h, p) = hostport.rsplit_once(':')?;
        (h.to_string(), p.parse::<u16>().ok()?)
    };

    if port == 0 {
        return None;
    }
    if host.is_empty() || host.len() > 253 {
        return None;
    }
    // Hostname charset: a-z A-Z 0-9 '-' '_' '.'
    if !host.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_' || c == b'.') {
        return None;
    }
    if host == "." || host.starts_with('.') || host.ends_with('.') {
        return None;
    }
    if host.starts_with('-') || host.ends_with('-') {
        return None;
    }

    // IPv4-style (all-digit octets separated by dots) — validate octets.
    if host.bytes().all(|c| c.is_ascii_digit() || c == b'.') && host.contains('.') {
        let octets: Vec<&str> = host.split('.').collect();
        if octets.len() != 4
            || octets.iter().any(|o| o.is_empty() || o.len() > 3 || o.parse::<u8>().is_err())
        {
            return None;
        }
    }

    Some(ParsedProxy { host, port })
}

/// Host is an all-numeric dotted form and parses to a valid IPv4 (4 octets,
/// each 0-255).  Used to gate HTML host/port pairing so untrusted footer
/// cells ("Total", "4258") can't masquerade as a proxy.
fn is_ipv4(host: &str) -> bool {
    let h = host.trim();
    if h.bytes().any(|c| !c.is_ascii_digit() && c != b'.') || !h.contains('.') {
        return false;
    }
    let octets: Vec<&str> = h.split('.').collect();
    octets.len() == 4
        && octets
            .iter()
            .all(|o| !o.is_empty() && o.len() <= 3 && o.parse::<u8>().is_ok())
}

/// Parse a full text/HTML document into unique endpoints (order-preserving).
pub fn parse_proxy_list(raw: &str, format: &str) -> Vec<ParsedProxy> {
    let cells: Vec<String> = if format == "html" {
        // Cell text = up to the next `</td>` or `<` after the opening tag;
        // collapse whitespace.
        let mut cells = Vec::new();
        for seg in raw.split("<td").skip(1) {
            let body = seg.split_once('>').map(|(_, x)| x).unwrap_or("");
            let cut = body.find("</td>").or_else(|| body.find('<')).unwrap_or(body.len());
            let c: String = body[..cut].split_whitespace().collect::<Vec<_>>().join(" ");
            if !c.is_empty() {
                cells.push(c);
            }
        }
        cells
    } else {
        raw.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect()
    };

    // Iterate cells; each is either a full endpoint (case a) or a
    // host-cell followed by a port-cell (case b).  Collect unique endpoints.
    let mut out: Vec<ParsedProxy> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut i = 0usize;
    while i < cells.len() {
        // (a) current cell is already a full endpoint
        if let Some(p) = parse_line(&cells[i]) {
            let e = p.endpoint();
            if seen.insert(e) {
                out.push(p);
            }
            i += 1;
            continue;
        }
        // (b) current cell + next cell = host:port.  In HTML tables the
        // adjacent cells are untrusted footer/stats cells too ("Total"
        // "4258"), so only pair when the host cell is an IPv4 address —
        // that is the shape real proxy-list tables use.
        if i + 1 < cells.len() && if format == "html" { is_ipv4(&cells[i]) } else { true } {
            if let Some(p) = parse_line(&format!("{}:{}", cells[i], cells[i + 1])) {
                let e = p.endpoint();
                if seen.insert(e) {
                    out.push(p);
                }
                i += 2;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Fetch a source and parse.  `url`/`format` come from the `Source`
/// catalog; empty result on bot-wall (no error, callers report 0 scraped).
#[cfg(feature = "http")]
pub async fn scrape(url: &str, format: &str) -> anyhow::Result<Vec<ParsedProxy>> {
    let client = reqwest::Client::builder()
        .user_agent("ser-replica/0.1 (+proxy scraper; local)")
        .timeout(std::time::Duration::from_secs(20))
        .build()?;
    let text = client.get(url).send().await?.text().await?;
    Ok(parse_proxy_list(&text, format))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_host_port() {
        let p = parse_line("1.2.3.4:8080").unwrap();
        assert_eq!(p.host, "1.2.3.4");
        assert_eq!(p.port, 8080);
    }

    #[test]
    fn scheme_and_path() {
        let p = parse_line("http://5.6.7.8:3128/foo?bar=1").unwrap();
        assert_eq!(p.endpoint(), "5.6.7.8:3128");
    }

    #[test]
    fn auth_prefix() {
        let p = parse_line("user:pass@9.9.9.9:1080").unwrap();
        assert_eq!(p.host, "9.9.9.9");
        assert_eq!(p.port, 1080);
    }

    #[test]
    fn hostname_supported() {
        let p = parse_line("my-proxy.example.com:8080").unwrap();
        assert_eq!(p.host, "my-proxy.example.com");
    }

    #[test]
    fn rejects_nonsense() {
        assert!(parse_line("").is_none());
        assert!(parse_line("nope").is_none());
        assert!(parse_line("1.2.3").is_none());
        assert!(parse_line("host:0").is_none());
        // Hostnames must start with alphanumeric (RHS-1035 / RFC 952 — no
        // leading/trailing hyphens); a leading hyphen rejects.
        assert!(parse_line("-bad:1234").is_none());
        // A hyphen in the middle is fine (my-proxy...).
        assert!(parse_line("my-proxy-1:8080").is_some());
        // Internal space / slash — not a single cell, so the caller won't
        // see this in practice; but parse_line itself must not accept them.
        assert!(parse_line("has space:1234").is_none());
    }

    #[test]
    fn ipv4_out_of_range_rejected() {
        assert!(parse_line("256.1.1.1:80").is_none());
        assert!(parse_line("1.2.3:80").is_none());
    }

    #[test]
    fn text_list_dedupes_preserves_order() {
        let raw = "1.1.1.1:80\n1.1.1.1:80\n2.2.2.2:81\nnot-a-proxy\n3.3.3.3:443\n";
        let v = parse_proxy_list(raw, "text");
        assert_eq!(v.len(), 3, "got {v:?}");
        assert_eq!(v[0].endpoint(), "1.1.1.1:80");
        assert_eq!(v[1].endpoint(), "2.2.2.2:81");
        assert_eq!(v[2].endpoint(), "3.3.3.3:443");
    }

    #[test]
    fn html_endpoints_per_cell() {
        let html = concat!(
            "<table><tr><th>endpoint</th></tr>",
            "<tr><td>47.85.161.37:3128</td><td>US</td><td>elite</td></tr>",
            "<tr><td>5.181.178.46:8080</td><td>ID</td><td>anon</td></tr>",
            "</table>",
        );
        let v = parse_proxy_list(html, "html");
        assert_eq!(v.len(), 2, "got {v:?}");
        assert_eq!(v[0].endpoint(), "47.85.161.37:3128");
        assert_eq!(v[1].endpoint(), "5.181.178.46:8080");
    }

    #[test]
    fn html_host_cell_then_port_cell() {
        let html = concat!(
            "<table><tr><th>ip</th><th>port</th></tr>",
            "<tr><td>47.85.161.37</td><td>3128</td><td>US</td><td>United States</td><td>elite</td><td>no</td><td>no</td><td>10s</td></tr>",
            "<tr><td>5.181.178.46</td><td>8080</td><td>ID</td><td>Indonesia</td><td>anon</td><td>no</td><td>yes</td><td>18s</td></tr>",
            "</table>",
        );
        let v = parse_proxy_list(html, "html");
        let eps: Vec<_> = v.iter().map(|p| p.endpoint()).collect();
        assert!(eps.iter().any(|e| e == "47.85.161.37:3128"), "got {eps:?}");
        assert!(eps.iter().any(|e| e == "5.181.178.46:8080"), "got {eps:?}");
    }

    #[test]
    fn html_footer_stats_cells_rejected() {
        // Footer row: "Total" "4258" "Google" "37" are stats, not proxies.
        let html = concat!(
            "<table>",
            "<tr><td>47.85.161.37</td><td>3128</td><td>US</td></tr>",
            "<tr><td>Total</td></tr>",
            "<tr><td>Total</td><td>4258</td></tr>",
            "<tr><td>Google</td><td>37</td></tr>",
            "<tr><td>Europe</td><td>913</td></tr>",
            "</table>",
        );
        let v = parse_proxy_list(html, "html");
        let eps: Vec<_> = v.iter().map(|p| p.endpoint()).collect();
        assert_eq!(eps, vec!["47.85.161.37:3128"], "got {eps:?}");
    }

    #[test]
    fn known_sources_listed() {
        let srcs = known_sources();
        assert!(srcs.len() >= 2);
        assert!(srcs.iter().any(|s| s.proto == "socks5"));
        assert!(srcs.iter().any(|s| s.proto == "any" || s.proto == "http"));
        assert!(srcs.iter().all(|s| s.url.starts_with("http")));
    }

    #[test]
    fn source_info_serializes() {
        let s = &known_sources()[0];
        let info = SourceInfo::from(s);
        let js = serde_json::to_string(&info).unwrap();
        assert!(js.contains("name"), "js={js}");
        assert!(js.contains("free-proxy-list.net"));
    }
}
