//! # ser-engine — core functionality engine of the Search Engine Ranker replica
//!
//! This crate contains the pure, framework-agnostic engines that the whole
//! application is built on.  Every field that GSA fills into every one of its
//! ~1,042 "engine" specs ultimately runs through:
//!
//!   * the **Spin/Spintax resolver** ([crate::spin]) — `{a|b}` alternatives,
//!     `[optional]` inclusion, and `%variable%` data substitution
//!     (e.g. `%anchor_text%`, `%article%`, `%random-18-40%`, `%spinfile-x.dat%`);
//!   * the **engine-spec parser** that reads GSA `.ini` engine files into a
//!     typed model ([crate::enginespec]);
//!   * extraction / verification helpers (`front`/`back`, `front1/front2/...`,
//!     `find url`, `find link`) that reproduce how GSA reads a result URL out of
//!     an HTML response.
//!
//! ## Determinism
//!
//! Every random draw (alternative choice, optional inclusion, numeric random)
//! flows through a single seeded [splitmix64::SplitMix64] PRNG injected into
//! the resolver.  Given the same input and the same seed the output is
//! byte-for-byte identical, which is what makes the test suite meaningful.

pub mod enginespec;
pub mod extraction;
pub mod spin;
pub mod splitmix64;
pub mod driver;
pub mod routing;
#[cfg(feature = "http")]
pub mod http_driver;
#[cfg(feature = "browser")]
pub mod browser_bridge;
pub mod executor;
pub mod keyword;
/// F#12 — email engine runner core (spec parse, address globs, token
/// resolution, inbox-message extraction) for GSA's 217 `Email_Engines`.
pub mod email;
/// F#12 — POP3 transport for the 75 mailbox-family email engines (real
/// `pop3 server/port/login/password/ssl`; the other 142 ship `pop3 server=WEB`
/// and use the HTTP path in `email.rs`).
pub mod pop3;
/// F#12 — email-engine execution pipeline (inbox source seam, POP3 over TCP
/// / TLS, WEB-inbox over `Driver`, and the shared fetch→extract→visit run
/// used by the runner for all 217 `Email_Engines` specs).
pub mod email_exec;
/// F#13 — proxy value types + round-robin rotation (GSA `proxy list/type`).
pub mod proxy;
pub mod anchor;
pub mod captcha;
/// F#22 — data-driven text generators cloned from GSA's shipped `.dat` pools
/// (titlegen / comments / user_agents / charspin / typos / stop-words) — the
/// "humanize" layer.  Wired into the spin resolver's `%...%` dispatch.
pub mod humanize;
/// F#23 — Site finder (GSA "Filter URLs / Find New Sites" — se.dat 2525-entry
/// catalog + query modes + result extraction + quality filters).  This module
/// is pure Rust; it owns the pipeline and the driver I/O boundary stays in
/// the executor (F#6/7 drivers).
pub mod site;
/// Stage B — multi-family backlink selection: classify every engine into
/// Article / Comment / Link and rank the strongest candidate per family so a
/// run posts the best backlink of each KIND rather than one engine on a
/// hunch.  Pure core — no `http` gate (testable offline).
pub mod families;
/// F#13 — real-socket proxy connectivity check (the `proxy test` endpoint's
/// engine; gated on `http` because it lives alongside the HTTP driver).
#[cfg(feature = "http")]
pub mod proxy_test;
/// F#24 — proxy-list scraper (GSA "add free proxies from list/source"):
/// pure `parse_line` / `parse_proxy_list` + `known_sources` + the
/// network `scrape` (reqwest, gated on `http`).
pub mod proxy_scraper;
/// F#14 — Geo/IP (GSA per-proxy country + egress-IP verification).  Pure
/// value types + parse/validate/select policy; ungated so the logic is
/// testable without the `http` feature.
pub mod geo;
/// F#14 — the network half of Geo/IP: reqwest egress probe (`ipify` through
/// the proxy) + pluggable country lookup.  Gated on `http` like `proxy_test`.
#[cfg(feature = "http")]
pub mod geo_probe;
/// F#15 — Global / profile settings + engine-override cascade + import/export
/// bundle format (pure core; DB/API/layers build on this).
pub mod settings;
pub mod stats;
/// F#17 — pure schedule core (GSA per-project scheduler): typed
/// `ScheduleSpec` + `next_run` cadence/window/weekday/gate algorithm.
/// Ungated so the cron semantics are testable without Postgres or `http`.
pub mod schedule;
/// F#18 — notification event model + classification (pure core; the API
/// writes this shape to Postgres, the SSE stream serializes it, and the
/// SMTP sender renders it — one event vocabulary across all channels).
pub mod notify;
/// F#18 — outbound SMTP delivery (the "email me an alert" channel).
/// Non-optional deps (`tokio-native-tls` / `native-tls` / `tokio` net) are
/// already in the tree via F#12/F#6, so this is ungated — the `log` mode
/// (tests) needs no socket.
pub mod smtp;

pub use keyword::{expand as expand_keyword, normalize as normalize_keyword, resolve_one as resolve_keyword_one};

pub use spin::{Context, SpintaxResolver, default_context, resolve};
pub use enginespec::{Engine, EngineType, Step, parse_engine_file, parse_engine_str};
pub use extraction::{extract_front_back, extract_front_back_variants};
pub use driver::{Driver, DriverResponse, MockDriver};
pub use routing::{DriverKind, required_driver_kind, required_driver_kind_from_text};
#[cfg(feature = "http")]
pub use http_driver::{HttpDriver, default_user_agent, form_urlencode};
#[cfg(feature = "browser")]
pub use browser_bridge::{CdpError, Transport, WebDriver, WsTransport};
pub use executor::{
    SubmissionInput, SubmissionPlan, StepPlan, StepNav, StepOutcome, StepOutcomeRef,
    StepPhase, ExecutionReport, resolve_plan, classify_step, extract_result_url, execute_plan,
};
pub use families::{CandidateEngine, Family, best_per_family, score as family_score};
