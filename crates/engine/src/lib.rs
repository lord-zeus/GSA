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
/// F#13 — real-socket proxy connectivity check (the `proxy test` endpoint's
/// engine; gated on `http` because it lives alongside the HTTP driver).
#[cfg(feature = "http")]
pub mod proxy_test;

pub use keyword::{expand as expand_keyword, normalize as normalize_keyword, resolve_one as resolve_keyword_one};

pub use spin::{Context, SpintaxResolver, default_context, resolve};
pub use enginespec::{Engine, EngineType, Step, parse_engine_file, parse_engine_str};
pub use extraction::{extract_front_back, extract_front_back_variants};
pub use driver::{Driver, DriverResponse, MockDriver};
pub use routing::{DriverKind, required_driver_kind, required_driver_kind_from_text};
#[cfg(feature = "http")]
pub use http_driver::{HttpDriver, default_user_agent, form_urlencode};
pub use executor::{
    SubmissionInput, SubmissionPlan, StepPlan, StepNav, StepOutcome, StepOutcomeRef,
    StepPhase, ExecutionReport, resolve_plan, classify_step, extract_result_url, execute_plan,
};
