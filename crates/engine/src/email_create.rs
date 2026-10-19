//! # E2 — Mailbox CreationRunner (GSA "SERE" creation flow over HTTP)
//!
//! GSA's answer to the register → email-verify → login wall is in two
//! halves. The **verify** half already runs ([`crate::email_exec`]: inbox
//! parse + verification-link visit). This module is the **create** half —
//! what the live app does in `add_new_email_to_vst` /
//! `fill_missing_email_data` before any of that:
//!
//! 1. Pick a provider with [`crate::email::select_provider`] (verifiable
//!    register state machine first — anonmails.de — then visit-style —
//!    hidebox.de — then WEB-inbox, then operator POP3 "Reuse account").
//! 2. Generate a fresh address + password (`random_local_part` /
//!    `random_password`, GSA's `random_email`/`rnd_email` shapes).
//! 3. Execute the provider's [`crate::email::MailboxCreation`] plan —
//!    `start url` GET (cookie priming) → register POST with resolved
//!    `%emailuser%`/`%password%` → require the `register success` marker.
//!
//! The runner is **synchronous** (like [`crate::email_exec::run_email_engine`])
//! so the runner crate drives it from `spawn_blocking`; each Driver call
//! bridges to a one-shot tokio runtime via
//! [`crate::email_exec::block_on_driver`].
//!
//! Copy + improvement over GSA:
//! * deterministic under caller seeds (reproducible tests / runs),
//! * per-step outcome reporting (GSA logs to a single grid cell),
//! * provider selection carries the `check_email_blacklist` rule
//!   (one disposable per provider domain per in-use list),
//! * `verified` is a distinct signal: for a verifiable provider it means
//!   the register marker was actually in the response body, so a caller
//!   can tell "created and confirmed" from "probably created" without
//!   trusting HTTP 200 alone.

use std::collections::HashMap;
use std::sync::Arc;

use serde::Serialize;

use crate::driver::Driver;
use crate::email::{
    CreateAction, EmailEngine, MailboxCreation, plan_mailbox_creation, provider_domain,
    random_local_part, random_password, select_provider,
};
use crate::email_exec::{block_on_driver, parse_form_body};

/// One wire action's outcome, in the [`CreationResult::steps`] report.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CreationStep {
    /// `"navigate"` or `"register"`.
    pub kind: String,
    /// The URL the action targeted.
    pub url: String,
    /// HTTP-level success (2xx-class response, readable body).
    pub ok: bool,
    /// Failure text, or — for a register step — confirmation of the
    /// success marker.
    pub detail: String,
}

/// Final outcome of one creation round.
#[derive(Debug, Clone, Serialize)]
pub struct CreationResult {
    /// `true` when the mailbox may be used: every step OK **and** the
    /// provider's verification marker was met (a verifiable provider) or,
    /// for a visit-style provider, the reservation page loaded.
    pub ok: bool,
    /// The register `register success` marker was found in the response
    /// (only meaningful when the provider has a register state machine).
    pub verified: bool,
    /// Engine name chosen by [`select_provider`] (operator's pin, if any).
    pub engine: String,
    /// The concrete mailbox address created.
    pub address: String,
    /// The mailbox password (GSA `fill_missing_email_data`: random).
    pub password: String,
    /// Provider domain (the `emails match` host).
    pub domain: String,
    /// Per-step report, in wire order.
    pub steps: Vec<CreationStep>,
}

/// Resolve a concrete creation from the parsed email-engine catalog:
/// provider pick → address + password generation → plan build. Pure.
///
/// * `pin_engine` — exact engine name (operator override,
///   `POST /api/email/create {"engine": ...}`); empty = auto-select.
/// * `in_use` — addresses already in use (GSA `check_email_blacklist`).
/// * `local_seed` / `pw_seed` — deterministic seeds for tests; `None` =
///   entropy from time/PID (GSA behavior).
pub fn resolve_creation(
    engines: &[EmailEngine],
    pin_engine: &str,
    in_use: &[String],
    local_seed: Option<u64>,
    pw_seed: Option<u64>,
) -> Option<(EmailEngine, MailboxCreation)> {
    let engine = if pin_engine.trim().is_empty() {
        select_provider(engines, in_use)?.0
    } else {
        engines
            .iter()
            .find(|e| e.name.eq_ignore_ascii_case(pin_engine.trim()))?
    };
    let domain = provider_domain(engine)?;
    let local = random_local_part(local_seed);
    let password = random_password(pw_seed, None);
    let address = format!("{local}@{domain}");
    let plan = plan_mailbox_creation(engine, &address, &password)?;
    Some((engine.clone(), plan))
}

/// Execute a [`MailboxCreation`] plan against a [`Driver`] (sync — the
/// runner calls this from `spawn_blocking`).
///
/// Navigation failures are recorded but do not short-circuit the plan:
/// GSA's anonmails flow tolerates a failed cookie-priming GET and the
/// register POST can still succeed. A register step with a non-empty
/// `register success` marker that is missing from the response marks the
/// whole round failed (verified=false).
pub fn run_creation(
    plan: &MailboxCreation,
    engine_name: &str,
    driver: &Arc<dyn Driver>,
) -> CreationResult {
    let mut steps: Vec<CreationStep> = Vec::new();
    let mut verified = false;
    let has_register = plan
        .actions
        .iter()
        .any(|a| matches!(a, CreateAction::Register { .. }));

    for action in &plan.actions {
        match action {
            CreateAction::Navigate { url } => {
                let resp = block_on_driver(driver.as_ref(), url.clone(), HashMap::new());
                steps.push(CreationStep {
                    kind: "navigate".into(),
                    url: url.clone(),
                    ok: resp.ok,
                    detail: resp.error.unwrap_or_default(),
                });
            }
            CreateAction::Register { url, body, success } => {
                let fields = parse_form_body(body);
                let resp = block_on_driver(driver.as_ref(), url.clone(), fields);
                let marker = success.trim();
                let marker_ok =
                    marker.is_empty() || resp.html.to_lowercase().contains(&marker.to_lowercase());
                if resp.ok && marker_ok {
                    verified = true;
                }
                let detail = if !resp.ok {
                    resp.error.unwrap_or_else(|| "HTTP failure".into())
                } else if marker_ok {
                    format!("register success marker present: {marker}")
                } else {
                    format!(
                        "register success marker missing ({marker}); response: {}",
                        resp.html.chars().take(200).collect::<String>()
                    )
                };
                steps.push(CreationStep {
                    kind: "register".into(),
                    url: url.clone(),
                    ok: resp.ok && marker_ok,
                    detail,
                });
            }
        }
    }

    let all_ok = steps.iter().all(|s| s.ok);
    // Visit-style (no register step) providers "verify" when the page loads.
    let ok = if has_register { verified && all_ok } else { all_ok };

    let (_, domain) = plan.address.rsplit_once('@').unwrap_or(("", ""));
    CreationResult {
        ok,
        verified,
        engine: engine_name.to_string(),
        address: plan.address.clone(),
        password: plan.password.clone(),
        domain: domain.to_string(),
        steps,
    }
}

/// One round, end to end: resolve (pick provider + generate address) then
/// execute. Sync; the runner wraps in `spawn_blocking` + timeout.
pub fn create_mailbox(
    engines: &[EmailEngine],
    pin_engine: &str,
    in_use: &[String],
    local_seed: Option<u64>,
    pw_seed: Option<u64>,
    driver: &Arc<dyn Driver>,
) -> CreationResult {
    let (engine, plan) = match resolve_creation(engines, pin_engine, in_use, local_seed, pw_seed) {
        Some(v) => v,
        None => {
            return CreationResult {
                ok: false,
                verified: false,
                engine: pin_engine.to_string(),
                address: String::new(),
                password: String::new(),
                domain: String::new(),
                steps: vec![CreationStep {
                    kind: "resolve".into(),
                    url: String::new(),
                    ok: false,
                    detail: "no creatable email engine (provider pick failed)"
                        .into(),
                }],
            };
        }
    };
    run_creation(&plan, &engine.name, driver)
}

/// Load every parseable `*.ini` in GSA's `Email_Engines` directory
/// (`{root}/Email_Engines`). Unparseable files are skipped, not fatal —
/// GSA ships at least one spec it cannot read either.
pub fn load_email_engines(root: &std::path::Path) -> Vec<EmailEngine> {
    let dir = root.join("Email_Engines");
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    rd.filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .is_some_and(|x| x.eq_ignore_ascii_case("ini"))
        })
        .filter_map(|e| crate::email::parse_email_engine_file(&e.path()).ok())
        .collect()
}

// ---------------------------------------------------------------------------
// Tests — the two shipped creation protocols (anonmails / hidebox) grounded.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::MockDriver;
    use crate::email::parse_email_engine_str;

    const ANONMAILS: &str = "[setup]\nsignup url=https://anonmails.de/index.php?language=en\n\
emails match=*@anonmails.de\npop3 server=WEB\n\
start url=https://anonmails.de/\n\
register url=https://anonmails.de/account-add.php\n\
register url post data=emailAddr=%emailuser%&emailPass=%password%&emailPassConfirm=%password%&emailExpire=50&method=add\n\
register success=status\":\"OK\"\n\
web parse url=https://anonmails.de/inbox.php?check=1\nenabled=1\n";

    const HIDEBOX: &str = "[setup]\nsignup url=http://hidebox.de\nemails match=*@hidebox.de\n\
pop3 server=WEB\nweb parse url=http://hidebox.de/box/%emailuser%\nenabled=1\n";

    fn anon_engine() -> EmailEngine {
        parse_email_engine_str(ANONMAILS, "anonmails.de").unwrap()
    }

    #[test]
    fn anonmails_full_chain_verifies_when_marker_present() {
        let drv: Arc<dyn Driver> = Arc::new(MockDriver::with_html(
            "<html>{\"status\":\"OK\",\"mailbox\":\"x@anonmails.de\"}</html>",
        ));
        let res = create_mailbox(&[anon_engine()], "", &[], None, None, &drv);
        assert!(res.ok, "steps: {:?}", res.steps);
        assert!(res.verified);
        assert!(res.address.ends_with("@anonmails.de"));
        assert_eq!(res.steps.len(), 2);
        assert_eq!(res.steps[0].kind, "navigate");
        assert_eq!(res.steps[1].kind, "register");
        assert!(res.password.chars().count() >= 10);
    }

    #[test]
    fn anonmails_register_marker_missing_fails() {
        let drv: Arc<dyn Driver> = Arc::new(MockDriver::with_html("<html>server busy</html>"));
        let res = create_mailbox(&[anon_engine()], "", &[], None, None, &drv);
        assert!(!res.ok);
        assert!(!res.verified);
        assert_eq!(res.steps[1].ok, false);
        assert!(res.steps[1]
            .detail
            .contains("marker missing"), "detail: {}", res.steps[1].detail);
    }

    #[test]
    fn anonmails_http_failure_fails() {
        let drv: Arc<dyn Driver> = Arc::new(MockDriver::fail_with("connect reset"));
        let res = create_mailbox(&[anon_engine()], "", &[], None, None, &drv);
        assert!(!res.ok);
        assert!(res
            .steps
            .iter()
            .all(|s| !s.ok), "steps: {:?}", res.steps);
    }

    #[test]
    fn hidebox_visit_flow_ok_when_page_loads() {
        let engines = vec![parse_email_engine_str(HIDEBOX, "hidebox.de").unwrap()];
        let drv: Arc<dyn Driver> = Arc::new(MockDriver::with_html("<html>hidebox inbox</html>"));
        let res = create_mailbox(&engines, "", &[], None, None, &drv);
        assert!(res.address.ends_with("@hidebox.de"));
        assert!(res.ok, "steps: {:?}", res.steps);
        // visit-style: verified is the all-steps-OK signal here
        assert_eq!(res.steps.iter().all(|s| s.ok), res.ok);
    }

    #[test]
    fn in_use_provider_is_blacklisted_for_disposables() {
        let drv: Arc<dyn Driver> = Arc::new(MockDriver::with_html("x"));
        let res = create_mailbox(
            &[anon_engine()],
            "",
            &["someone@anonmails.de".to_string()],
            None,
            None,
            &drv,
        );
        assert_eq!(res.steps.len(), 1, "no provider left: {:?}", res.steps);
        assert!(res.steps[0].detail.contains("no creatable"));
    }

    #[test]
    fn pin_engine_bypasses_selection() {
        let drv: Arc<dyn Driver> = Arc::new(MockDriver::with_html("{ \"status\":\"OK\" }"));
        let res = create_mailbox(
            &[anon_engine()],
            "anonmails.de",
            &["someone@anonmails.de".to_string()],
            None,
            None,
            &drv,
        );
        assert!(res.ok, "steps: {:?}", res.steps);
    }

    #[test]
    fn deterministic_seeds_reproduce_address_and_password() {
        let drv: Arc<dyn Driver> = Arc::new(MockDriver::with_html("{ \"status\":\"OK\" }"));
        let a = create_mailbox(&[anon_engine()], "", &[], Some(42), Some(43), &drv);
        let b = create_mailbox(&[anon_engine()], "", &[], Some(42), Some(43), &drv);
        assert_eq!(a.address, b.address);
        assert_eq!(a.password, b.password);
    }

    #[test]
    fn load_email_engines_reads_gsa_catalog() {
        let root = std::env::var("GSA_INSTALL_ROOT")
            .ok()
            .map(std::path::PathBuf::from)
            .filter(|p| p.join("Email_Engines").is_dir());
        let Some(root) = root else {
            return; // catalog not present in this CI env — skip
        };
        let engines = load_email_engines(&root);
        assert!(engines.len() >= 100, "catalog count: {}", engines.len());
        assert!(engines.iter().any(|e| e.name == "anonmails.de"));
        assert!(engines.iter().any(|e| e.name == "hidebox.de" || e.name == "hidebox.org"));
    }
}
