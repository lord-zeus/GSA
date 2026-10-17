//! # Proxy connectivity test — GSA `proxy test` / `proxy verify` (F#13).
//!
//! Real socket probes against a [`ProxySpec`]:
//!
//! * **HTTP proxies** — send `GET http://<target>/ HTTP/1.1` with `Host`
//!   and, when credentials exist, `Proxy-Authorization: Basic ***`  Any
//!   HTTP status line on the wire counts as "the proxy answered" (even
//!   `407` = auth required — the proxy is *up*; authentication is a
//!   config problem, reported in `detail`).
//! * **SOCKS5** — handshake `05 NMETHODS METHODS` expecting `05 00`
//!   (no-auth accept); with credentials, method `02` (username/password)
//!   and the RFC-1929 sub-negotiation.  A completed handshake = up.
//!
//! TLS (CONNECT / STARTTLS) is deferred — plain-TCP semantics are what
//! GSA's own `proxy test` exercises in its local toolbelt, and the
//! production path (reqwest) surfaces TLS failures through the normal
//! job-error attribution.
//!
//! Synchronous on purpose: called from `spawn_blocking` contexts (the
//! API handler) — one probe at a time, short timeout.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use crate::proxy::ProxySpec;

/// F#13 convenience: probe `p` end-to-end against Google (GSA's own
/// "proxy test" hit `https://www.google.com` as the reference target),
/// with a 6s connect deadline.  The API's `POST /proxies/{id}/test`
/// handler calls this; the runner's proxy pool can also call it to
/// pre-filter the list before routing jobs.
pub async fn test_proxy(p: &ProxySpec) -> ProbeResult {
    let p2 = p.clone();
    tokio::task::spawn_blocking(move || probe(&p2, "https://www.google.com", Duration::from_secs(6)))
    .await
    .unwrap_or(ProbeResult {
        ok: false,
        latency_ms: 0,
        detail: "probe task panicked".into(),
    })
}

/// Outcome of a connectivity probe (serde-friendly; the API layer
/// reshapes this for the `proxies.latency_ms` / `enabled` update).
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProbeResult {
    /// True when the proxy answered plausibly (status line / S5 handshake).
    pub ok: bool,
    /// Elapsed wall time of the probe.
    pub latency_ms: i64,
    /// Human detail: "proxy answered: HTTP/1.1 200 OK",
    /// "SOCKS5 ok (no-auth)", "connect: Connection refused", ...
    pub detail: String,
}

/// Probe `p` with `target` as the tunnel destination (used only in the
/// HTTP request line).
pub fn probe(p: &ProxySpec, target: &str, timeout: Duration) -> ProbeResult {
    let started = Instant::now();
    let elapsed = || started.elapsed().as_millis() as i64;

    let mut s: TcpStream = match (|| -> Result<TcpStream, String> {
        let addr = format!("{}:{}", p.host, p.port);
        let addr: std::net::SocketAddr =
            addr.parse().map_err(|e: std::net::AddrParseError| e.to_string())?;
        let s = TcpStream::connect_timeout(&addr, timeout).map_err(|e| format!("connect: {e}"))?;
        s.set_read_timeout(Some(timeout)).map_err(|e| format!("read timeout config: {e}"))?;
        s.set_write_timeout(Some(timeout)).map_err(|e| format!("write timeout config: {e}"))?;
        Ok(s)
    })() {
        Ok(s) => s,
        Err(msg) => return ProbeResult { ok: false, latency_ms: elapsed(), detail: msg },
    };

    let reply = if p.proxy_kind == "socks5" {
        probe_socks5(&mut s, p)
    } else {
        probe_http(&mut s, p, target)
    };
    ProbeResult { ok: reply.ok, latency_ms: elapsed(), detail: reply.detail }
}

#[derive(Debug)]
struct Reply {
    ok: bool,
    detail: String,
}

fn probe_http(s: &mut TcpStream, p: &ProxySpec, target: &str) -> Reply {
    let mut req = format!(
        "GET http://{target}/ HTTP/1.1\r\nHost: {target}\r\nProxy-Connection: keep-alive\r\n"
    );
    if !p.login.is_empty() {
        req.push_str(&format!(
            "Proxy-Authorization: Basic {}\r\n",
            basic(&p.login, &p.password)
        ));
    }
    req.push_str("User-Agent: ser-replica-proxy-test\r\n\r\n");

    if s.write_all(req.as_bytes()).is_err() {
        return Reply { ok: false, detail: "write failed".into() };
    }
    let mut buf = [0u8; 1024];
    let n = match s.read(&mut buf) {
        Ok(n) => n,
        Err(e) => return Reply { ok: false, detail: format!("read: {e}") },
    };
    let head = String::from_utf8_lossy(&buf[..n]);
    let status = head.lines().next().unwrap_or("").to_string();
    if status.starts_with("HTTP/") {
        // Any HTTP/1.x status line = proxy is up (407 included: it's an
        // auth problem, not a down proxy — GSA treats "proxy seems down"
        // as *no reply at all*).
        Reply { ok: true, detail: format!("proxy answered: {status}") }
    } else if !head.is_empty() {
        Reply {
            ok: false,
            detail: format!("unexpected reply: {}", &head[..head.len().min(64)]),
        }
    } else {
        Reply { ok: false, detail: "empty reply".into() }
    }
}

fn probe_socks5(s: &mut TcpStream, p: &ProxySpec) -> Reply {
    // Greeting: VER=5, NMETHODS, methods = [0(no auth)] or [0, 2(user/pass)].
    let methods: Vec<u8> = if p.login.is_empty() { vec![0] } else { vec![0, 2] };
    let mut out = vec![5u8, methods.len() as u8];
    out.extend_from_slice(&methods);
    if s.write_all(&out).is_err() {
        return Reply { ok: false, detail: "socks5: write failed".into() };
    }
    let b0 = match read_byte(s) {
        Some(b) => b,
        None => return Reply { ok: false, detail: "socks5: no reply".into() },
    };
    let b1 = match read_byte(s) {
        Some(b) => b,
        None => return Reply { ok: false, detail: "socks5: no version".into() },
    };
    if b0 != 5 {
        return Reply { ok: false, detail: format!("socks5: bad version {b0}") };
    }
    match b1 {
        0 => Reply { ok: true, detail: "SOCKS5 ok (no-auth)".into() },
        2 if !p.login.is_empty() => {
            // RFC-1929 sub-negotiation.
            let mut sub = vec![1u8, p.login.len() as u8];
            sub.extend_from_slice(p.login.as_bytes());
            sub.push(p.password.len() as u8);
            sub.extend_from_slice(p.password.as_bytes());
            if s.write_all(&sub).is_err() {
                return Reply { ok: false, detail: "socks5: auth write failed".into() };
            }
            let v = match read_byte(s) {
                Some(v) => v,
                None => return Reply { ok: false, detail: "socks5: no auth result".into() },
            };
            let r = match read_byte(s) {
                Some(r) => r,
                None => return Reply { ok: false, detail: "socks5: no auth reason".into() },
            };
            if v == 1 && r == 0 {
                Reply { ok: true, detail: "SOCKS5 ok (user/pass)".into() }
            } else {
                Reply { ok: false, detail: format!("socks5: auth failed ({r})") }
            }
        }
        other => Reply {
            ok: false,
            detail: format!("socks5: auth method {other} not supported"),
        },
    }
}

fn read_byte(s: &mut TcpStream) -> Option<u8> {
    let mut b = [0u8; 1];
    s.read_exact(&mut b).ok().map(|_| b[0])
}

/// Minimal RFC 2045 base64 for "user:pass" (no external crate needed).
fn basic(user: &str, pass: &str) -> String {
    let raw = format!("{user}:{pass}").into_bytes();
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((raw.len() + 2) / 3 * 4);
    for c in raw.chunks(3) {
        let b0 = c[0] as u32;
        let b1 = *c.get(1).unwrap_or(&0) as u32;
        let b2 = *c.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(T[(n >> 18 & 63) as usize] as char);
        out.push(T[(n >> 12 & 63) as usize] as char);
        out.push(if c.len() > 1 { T[(n >> 6 & 63) as usize] as char } else { '=' });
        out.push(if c.len() > 2 { T[(n & 63) as usize] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

    /// Raw-TCP listener: accepts one connection, reads the request,
    /// replies with a canned line, returns the received bytes.
    fn raw_server(reply: &str) -> (std::net::SocketAddr, thread::JoinHandle<Vec<u8>>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let reply = reply.to_string();
        let h = thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            s.set_read_timeout(Some(Duration::from_secs(2))).ok();
            let mut buf = [0u8; 2048];
            let n = s.read(&mut buf).unwrap_or(0);
            let req = buf[..n].to_vec();
            let _ = s.write_all(reply.as_bytes());
            let _ = s.flush();
            req
        });
        (addr, h)
    }

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
    fn basic_auth_encodes_known_vector() {
        assert_eq!(basic("Aladdin", "open sesame"), "QWxhZGRpbjpvcGVuIHNlc2FtZQ==");
        assert_eq!(basic("a", "b"), "YTpi");
    }

    #[test]
    fn http_probe_detects_up_proxy() {
        let (addr, h) = raw_server("HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
        let r = probe(&spec("127.0.0.1", addr.port(), "http"), "127.0.0.1", Duration::from_secs(3));
        assert!(r.ok, "{r:?}");
        assert!(r.detail.contains("200"), "{}", r.detail);
        let _ = h.join();
    }

    #[test]
    fn http_probe_reports_407_as_up_with_detail() {
        let (addr, h) = raw_server(
            "HTTP/1.1 407 Proxy Authentication Required\r\nProxy-Authenticate: Basic realm=\"x\"\r\nContent-Length: 0\r\n\r\n",
        );
        let r = probe(&spec("127.0.0.1", addr.port(), "http"), "127.0.0.1", Duration::from_secs(3));
        assert!(r.ok, "{r:?}");
        assert!(r.detail.contains("407"), "{}", r.detail);
        let _ = h.join();
    }

    #[test]
    fn http_probe_sends_basic_auth_when_creds_set() {
        let (addr, h) = raw_server("HTTP/1.1 200 OK\r\n\r\n");
        let p = ProxySpec {
            host: "127.0.0.1".into(),
            port: addr.port(),
            proxy_kind: "http".into(),
            login: "Aladdin".into(),
            password: "open sesame".into(),
        };
        let r = probe(&p, "127.0.0.1", Duration::from_secs(3));
        assert!(r.ok, "{r:?}");
        let req = h.join().unwrap();
        let got = String::from_utf8_lossy(&req);
        assert!(
            got.contains("Proxy-Authorization: Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ=="),
            "{}",
            got
        );
    }

    #[test]
    fn http_probe_refused_port_is_down() {
        // Port 9 (discard) on localhost is effectively always refused.
        let r = probe(&spec("127.0.0.1", 9, "http"), "127.0.0.1", Duration::from_secs(3));
        assert!(!r.ok);
        assert!(r.detail.contains("connect"), "{}", r.detail);
    }

    #[test]
    fn socks5_no_auth_handshake_succeeds() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let h = thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            s.set_read_timeout(Some(Duration::from_secs(2))).ok();
            // Greeting: VER(1) + NMETHODS(1) + NMETHODS bytes.
            let mut ver = [0u8; 1];
            s.read_exact(&mut ver).ok();
            let mut nm = [0u8; 1];
            s.read_exact(&mut nm).ok();
            let n = nm[0] as usize;
            let mut methods = vec![0u8; n];
            s.read_exact(&mut methods).ok();
            s.write_all(&[5u8, 0u8]).ok();
            s.flush().ok();
        });
        let r = probe(
            &spec("127.0.0.1", addr.port(), "socks5"),
            "127.0.0.1",
            Duration::from_secs(3),
        );
        assert!(r.ok, "{r:?}");
        assert!(r.detail.contains("no-auth"), "{}", r.detail);
        let _ = h.join();
    }

    #[test]
    fn socks5_user_pass_handshake_succeeds() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let h = thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut buf = [0u8; 4];
            s.read_exact(&mut buf).ok(); // greeting
            s.write_all(&[5u8, 2u8]).ok(); // pick user/pass
            s.flush().ok();
            let mut sub = [0u8; 256];
            let n = s.read(&mut sub).unwrap_or(0);
            assert!(n >= 3);
            s.write_all(&[1u8, 0u8]).ok();
            s.flush().ok();
        });
        let p = ProxySpec {
            host: "127.0.0.1".into(),
            port: addr.port(),
            proxy_kind: "socks5".into(),
            login: "u".into(),
            password: "pw".into(),
        };
        let r = probe(&p, "127.0.0.1", Duration::from_secs(3));
        assert!(r.ok, "{r:?}");
        assert!(r.detail.contains("user/pass"), "{}", r.detail);
        let _ = h.join();
    }
}
