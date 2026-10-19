//! # HTTP driver — `reqwest`-backed implementation of [`Driver`] (Feature #6).
//!
//! Covers the ~70% of GSA engines that are plain HTML forms / links:
//! form-POST with per-client cookie persistence, so multi-step plans
//! (login -> submit) keep their session cookies exactly like a browser,
//! automatic redirect following (the common "find link" case where the
//! engine answers a 302 to the created-link URL), a GSA-like User-Agent,
//! connect + overall timeouts, and a 10 MB body cap.
//!
//! The module is feature-gated (`http`) so the pure engine core stays
//! network-free for tests and other embedders.

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::StatusCode;

use super::driver::{Driver, DriverResponse};

/// Cap on the HTML body we are willing to buffer (10 MB); larger reads are
/// cut off and reported.
const MAX_BODY_BYTES: usize = 10 * 1024 * 1024;

/// A [`Driver`] backed by `reqwest`.
///
/// One driver instance owns one `reqwest::Client`, and the `cookies`
/// feature gives that client a built-in cookie store — so within a single
/// engine run, cookies set by a login response are automatically re-sent
/// on the submit request, mirroring browser behaviour.
pub struct HttpDriver {
    client: reqwest::Client,
    user_agent: String,
}

impl HttpDriver {
    /// Build a driver with GSA-like production defaults.
    pub fn new() -> Self {
        Self::with_ua(default_user_agent())
    }

    /// Build with a custom User-Agent.
    pub fn with_ua(ua: impl AsRef<str>) -> Self {
        let user_agent = ua.as_ref().to_string();
        let client = reqwest::Client::builder()
            .user_agent(&user_agent)
            .redirect(reqwest::redirect::Policy::limited(6))
            .cookie_provider(std::sync::Arc::new(reqwest::cookie::Jar::default()))
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(60))
            .build()
            .expect("reqwest client should build");
        Self { client, user_agent }
    }

    /// Settle a response (redirects already followed by the client) into a
    /// [`DriverResponse`]: non-2xx is `ok = false` with an HTTP status
    /// error; the body is buffered as lossy UTF-8 capped at
    /// [`MAX_BODY_BYTES`].
    async fn settle(r: reqwest::Response) -> DriverResponse {
        let status = r.status();
        let url = r.url().to_string();
        match r.bytes().await {
            Ok(b) => {
                let bytes = if (b.len() as usize) > MAX_BODY_BYTES {
                    &b[..MAX_BODY_BYTES]
                } else {
                    b.as_ref()
                };
                let ok = status.is_success() || status == StatusCode::NO_CONTENT;
                DriverResponse {
                    ok,
                    final_url: url,
                    html: String::from_utf8_lossy(bytes).into_owned(),
                    error: if ok { None } else { Some(format!("HTTP {}", status)) },
                }
            }
            Err(e) => DriverResponse {
                ok: false,
                final_url: url,
                html: String::new(),
                error: Some(format!("body read: {e}")),
            },
        }
    }
}

#[async_trait]
impl Driver for HttpDriver {
    /// GET a URL and return the settled page.
    async fn navigate(&self, url: String) -> DriverResponse {
        match self.client.get(&url).send().await {
            Ok(r) => Self::settle(r).await,
            Err(e) => DriverResponse {
                ok: false,
                final_url: url,
                html: String::new(),
                error: Some(e.to_string()),
            },
        }
    }

    /// POST `fields` as `application/x-www-form-urlencoded` and return the
    /// confirmation page — this is what the executor drives for every
    /// register/login/submit step.
    async fn submit(&self, url: String, fields: HashMap<String, String>, _timeout: Duration) -> DriverResponse {
        let mut body = String::with_capacity(256);
        for (k, v) in fields.iter() {
            body.push_str(&form_urlencode(k));
            body.push('=');
            body.push_str(&form_urlencode(v));
            body.push('&');
        }
        match self
            .client
            .post(&url)
            .header(reqwest::header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(body)
            .send()
            .await
        {
            Ok(r) => Self::settle(r).await,
            Err(e) => DriverResponse {
                ok: false,
                final_url: url,
                html: String::new(),
                error: Some(e.to_string()),
            },
        }
    }

    /// F#13 — same driver, but every request is routed through `spec`
    /// (http or socks5 proxy, optional basic auth). A fresh reqwest client
    /// per proxy (the proxy is a builder-level setting) with identical
    /// GSA-like defaults and the same User-Agent, so behavior is
    /// indistinguishable from the direct driver apart from the egress.
    /// `None` → this spec could not be attached (e.g. socks feature not
    /// enabled) and the caller must fall back to a direct connection.
    fn proxy_driver(&self, spec: &crate::proxy::ProxySpec) -> Option<std::sync::Arc<dyn Driver>> {
        let url = spec.url();
        let proxy = reqwest::Proxy::all(&url).ok()?;
        let proxy = if spec.login.is_empty() {
            proxy
        } else {
            proxy.basic_auth(&spec.login, &spec.password)
        };
        let client = reqwest::Client::builder()
            .user_agent(&self.user_agent)
            .redirect(reqwest::redirect::Policy::limited(6))
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(60))
            .cookie_provider(std::sync::Arc::new(reqwest::cookie::Jar::default()))
            .proxy(proxy)
            .pool_max_idle_per_host(4)
            .build()
            .ok()?;
        Some(std::sync::Arc::new(Self { client, user_agent: self.user_agent.clone() }) as std::sync::Arc<dyn Driver>)
    }
}

/// GSA's default desktop User-Agent (Chromium on Windows 10).
pub fn default_user_agent() -> String {
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36".to_string()
}

/// `application/x-www-form-urlencoded` encoding: alphanumerics and `-_.~`
/// pass through, space becomes `+`, everything else is percent-encoded.
pub fn form_urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ===========================================================================
//  Tests — a minimal threaded HTTP server on a random localhost port, so
//  the suite needs no external network and is deterministic.
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Arc;

    /// Canned responses keyed by "METHOD PATH".
    struct TestServer {
        addr: std::net::SocketAddr,
        routes: Arc<std::sync::Mutex<HashMap<String, (u16, String)>>>,
    }

    impl TestServer {
        fn start() -> Self {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = l.local_addr().unwrap();
            let routes = Arc::new(std::sync::Mutex::new(HashMap::new()));
            let r = routes.clone();
            std::thread::spawn(move || {
                for s in l.incoming() {
                    let Ok(mut s) = s else { break };
                    let r = r.clone();
                    std::thread::spawn(move || {
                        let mut buf = [0u8; 4096];
                        let n = s.read(&mut buf).unwrap_or(0);
                        let req = String::from_utf8_lossy(&buf[..n]);
                        let first = req.lines().next().unwrap_or("");
                        let mut parts = first.split_whitespace();
                        let method = parts.next().unwrap_or("");
                        let path = parts.next().unwrap_or("");
                        let key = format!("{method} {path}");
                        let (code, body) = {
                            let m = r.lock().unwrap();
                            m.get(&key).cloned().unwrap_or((200, "<html>default</html>".to_string()))
                        };
                        let reason = if (200..300).contains(&code) { "OK" } else { "Error" };
                        let text = format!(
                            "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n{}",
                            code, reason, body.len(), body
                        );
                        s.set_read_timeout(Some(Duration::from_secs(2))).ok();
                        s.write_all(text.as_bytes()).ok();
                        s.flush().ok();
                    });
                }
            });
            TestServer { addr, routes }
        }

        fn route(&self, key: &str, code: u16, body: &str) {
            self.routes.lock().unwrap().insert(key.to_string(), (code, body.to_string()));
        }

        fn base(&self) -> String {
            format!("http://{}", self.addr)
        }
    }

    #[tokio::test]
    async fn navigate_gets_200_html() {
        let srv = TestServer::start();
        srv.route("GET /page", 200, "<html><body>hello</body></html>");
        let d = HttpDriver::new();
        let r = d.navigate(format!("{}/page", srv.base())).await;
        assert!(r.ok, "expected ok, got: {r:?}");
        assert_eq!(r.html, "<html><body>hello</body></html>");
        assert_eq!(r.final_url, format!("{}/page", srv.base()));
    }

    #[tokio::test]
    async fn navigate_non_2xx_is_error_with_http_status() {
        let srv = TestServer::start();
        srv.route("GET /gone", 404, "not found anywhere");
        let d = HttpDriver::new();
        let r = d.navigate(format!("{}/gone", srv.base())).await;
        assert!(!r.ok);
        assert!(r.error.as_deref().unwrap_or("").contains("404"), "err: {:?}", r.error);
    }

    #[tokio::test]
    async fn submit_posts_urlencoded_body_and_gets_confirmation() {
        let srv = TestServer::start();
        srv.route("POST /submit", 200, "<html><body>You submitted</body></html>");
        let d = HttpDriver::new();
        let fields = HashMap::from([
            ("field1".to_string(), "a b".to_string()),
            ("url".to_string(), "https://mysite.example/".to_string()),
        ]);
        let r = d.submit(format!("{}/submit", srv.base()), fields, Duration::from_secs(10)).await;
        assert!(r.ok, "expected ok, got: {r:?}");
        assert!(r.html.contains("You submitted"));
    }

    #[tokio::test]
    async fn submit_non_2xx_is_error() {
        let srv = TestServer::start();
        srv.route("POST /submit", 500, "internal boom");
        let d = HttpDriver::new();
        let r = d.submit(format!("{}/submit", srv.base()), HashMap::new(), Duration::from_secs(10)).await;
        assert!(!r.ok);
        assert!(r.error.as_deref().unwrap_or("").contains("500"));
    }

    #[tokio::test]
    async fn connection_refused_is_graceful_error() {
        let d = HttpDriver::new();
        let started = std::time::Instant::now();
        let r = d.navigate("http://127.0.0.1:9/unreachable".to_string()).await;
        assert!(!r.ok);
        assert!(r.error.is_some());
        assert!(started.elapsed() < Duration::from_secs(30));
    }

    #[test]
    fn form_urlencode_encodes_spaces_and_symbols() {
        assert_eq!(form_urlencode("hello world"), "hello+world");
        assert_eq!(form_urlencode("a=b&c=d"), "a%3Db%26c%3Dd");
        assert_eq!(form_urlencode("keep-_.~chars"), "keep-_.~chars");
        assert_eq!(form_urlencode("https://x/"), "https%3A%2F%2Fx%2F");
    }

    #[test]
    fn default_user_agent_looks_like_chrome_on_windows() {
        let ua = default_user_agent();
        assert!(ua.contains("Windows NT 10.0"));
        assert!(ua.contains("Chrome"));
    }

    /// End-to-end: executor::execute_plan driving a real HTTP submission
    /// against the local test server — proves the driver/executor contract
    /// works over genuine transport, as it will against real engines.
    #[tokio::test]
    async fn full_plan_submission_over_real_http() {
        use crate::executor::{execute_plan, resolve_plan, SubmissionInput};
        use crate::enginespec::parse_engine_str;

        let srv = TestServer::start();
        // The submit step lands on a page containing the created link.
        srv.route(
            "POST /step1",
            200,
            "<html><body>success <a href=\"https://engine-x.example/entry/9001\">profile</a></body></html>",
        );

        let ini = concat!(
            "[engine]\r\n",
            "engine type=url-shortener\r\n",
            "url=\r\n",
            "\r\n",
            "[step1]\r\n",
            "form url=/step1\r\n",
            "description_250=%description_250%\r\n",
            "url=%url%\r\n",
            "find url=https://engine-x.example/entry\r\n",
            "submit success=success\r\n",
        );
        let engine = parse_engine_str(ini, "Fake Engine").unwrap();
        let input = SubmissionInput {
            url: "https://mysite.example/".into(),
            keyword: "buy shoes".into(),
            anchor: None,
            seed: Some(42),
            extra: Default::default(),
            files: HashMap::from([("description_250".to_string(), "a great site".to_string())]),
            random_options: Vec::new(),
        };
        let mut plan = resolve_plan(&engine, &input);
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].fields.get("description_250").map(String::as_str), Some("a great site"));
        assert!(plan.steps[0].fields.get("url").map(String::as_str).unwrap().contains("mysite.example"));
        // Rewrite the relative form url to the local server's absolute URL
        // so the test is fully self-contained (no external network).
        for s in plan.steps.iter_mut() {
            s.nav.form_url = vec![format!("{}/step1", srv.base())];
        }
        let d = HttpDriver::new();
        let report = execute_plan(&plan, &d).await;
        assert_eq!(report.successful, 1, "report: {:?}", report);
        assert_eq!(report.failed, 0, "report: {:?}", report);
        assert!(report.errors.is_empty(), "errors: {:?}", report.errors);
        assert_eq!(report.submitted_url.as_deref(), Some("https://engine-x.example/entry/9001"));
    }
}
