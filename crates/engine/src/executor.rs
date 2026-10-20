//! # Step executor — the shared contract between engine, driver, and worker.
//!
//! Turn a [engine::Engine] plus a project context (keyword, target URL,
//! optional anchor, seed, files, extra vars) into a [SubmissionPlan]:
//! one ordered list of [StepPlan]s, each carrying its **navigation**
//! (find_link / find_url / form_url / alternative_url), its **outcome
//! matchers** (submit success / failed / captcha failed / failed-retry),
//! its **verify config** (verify_url / verify_by / front-back delims),
//! and its **form fields** (fully-resolved, concrete, ready to be POSTed
//! or typed into a webview).
//!
//! [`execute_plan`] drives that plan through a [`driver::Driver`] (HTTP
//! in #6, browser in #7), applies [`classify_step`], extracts the created
//! link via [`extract_result_url`], and returns an [`ExecutionReport`].
//!
//! ## Determinism
//!
//! Every random draw flows through [engine::spin] (SplitMix64 + seed).
//! Same input + same seed ⇒ byte-for-byte the same plan.
//!
//! ## What we deliberately don't do (yet)
//!
//! * Network I/O — the `Driver` trait abstracts that; this module is pure.
//! * CAPTCHA solving — classified as [StepOutcome::CaptchaFailed], the
//!   caller decides whether to retry, fetch a solver, or mark failed.
//! * Multi-domain / multi-language — one engine, one target per run.

use std::collections::HashMap;

use serde::Serialize;

use super::enginespec::{Engine, EngineType, Step};
use super::spin::{Context, SpintaxResolver};

// ---------------------------------------------------------------------------
//  Public data types
// ---------------------------------------------------------------------------

/// A project-level context the executor needs.  The minimum the GSA app
/// always knows: the target URL and a keyword.  Everything else flows in
/// as [SubmissionInput::extra] or [SubmissionInput::files].
#[derive(Debug, Clone, Default)]
pub struct SubmissionInput {
    /// The site the backlink should point to (typically the project's URL).
    pub url: String,
    /// The keyword the project is optimising for.  GSA uses this as the
    /// fallback for `%anchor_text%` when no article-specific anchor is set.
    pub keyword: String,
    /// Optional explicit anchor (overrides the keyword fallback).
    pub anchor: Option<String>,
    /// Optional spintax seed.  `None` ⇒ [SpintaxResolver::default()] (i.e.
    /// `0xA11CE_5EED | 1` per submission; deterministic).
    pub seed: Option<u64>,
    /// Additional concrete values, looked up case-insensitively.
    pub extra: HashMap<String, String>,
    /// Spin data files, name → body (used by `%spinfile-NAME%`).
    pub files: HashMap<String, String>,
    /// Candidates for `%random_option%`.
    pub random_options: Vec<String>,
}

impl SubmissionInput {
    /// Resolve this input into a [Context] for the spintax resolver.
    ///
    /// GSA exposes **data sections** (like `[description_250]`, `[article]`,
    /// `[kw]`) as direct `%name%` variables — `description_250`, `article`,
    /// `kw` are all looked up in the *values* map.  Spintax **spinfiles** are
    /// addressed separately as `%spinfile-NAME%`.  We therefore promote the
    /// file contents into both `values` and `files` so both addressing
    /// styles resolve correctly.
    pub fn into_context(&self) -> Context {
        let mut ctx = Context {
            values: HashMap::new(),
            files: self.files.clone(),
            random_options: self.random_options.clone(),
        };
        // GSA's always-defined variables (name / keyword / url / e-mail).
        ctx.set("url", &self.url);
        if !self.keyword.is_empty() {
            ctx.set("keyword", &self.keyword);
            ctx.set("anchor_text", &self.anchor.clone().unwrap_or_else(|| self.keyword.clone()));
        }
        if let Some(a) = &self.anchor {
            ctx.set("anchor_text", a);
        }
        // Data sections → values (direct %name% addresses).
        for (k, v) in &self.files {
            ctx.set(k, v);
        }
        for (k, v) in &self.extra {
            ctx.set(k, v);
        }
        ctx
    }
}

/// A single step's **navigation contract**.  What the driver must do to
/// reach the form this step POSTs into.  Every field holds the raw raw
/// engine value; `.split_alts()` returns the already-split candidates as
/// a [`Vec<String>`].
#[derive(Debug, Clone, Default, Serialize)]
pub struct StepNav {
    /// How to find the form link — split by `|`.  Globs (`*...`) and
    /// substrings are both supported for matching.
    pub find_link: Vec<String>,
    /// URL the link points to — split by `|`.
    pub find_url: Vec<String>,
    /// Alternative target URL(s) — split by `|`.
    pub alternative_url: Vec<String>,
    /// The form's `action` / `form url` — split by `|`.
    pub form_url: Vec<String>,
    /// The form's `name=` — split by `|`.
    pub form_name: Vec<String>,
    /// `link type=Profile-URL` / `link type=URL` / etc. (raw).
    pub link_type: Option<String>,
    /// `modify url=...` — raw, usually the URL that must end with the
    /// submitted URL.
    pub modify_url: Option<String>,
}

impl StepNav {
    fn split(v: Option<&str>) -> Vec<String> {
        v.map(|s| s.split('|').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect())
            .unwrap_or_default()
    }

    /// Best-fit target URL for the driver to navigate to.
    ///
    /// Preference order: first absolute candidate (form_url → alternative_url
    /// → find_url → find_link), else the first relative candidate.
    /// If that relative candidate's `modify_url` is absolute, promote it
    /// (GSA INIs often put `form url=*/path` with `modify url=http://host/path`
    /// — CDP's `Page.navigate` needs the absolute form).
    pub fn navigation_target(&self) -> Option<String> {
        fn is_absolute(u: &str) -> bool {
            u.starts_with("http://") || u.starts_with("https://")
        }
        let all: [Option<&String>; 4] = [
            self.form_url.first(),
            self.alternative_url.first(),
            self.find_url.first(),
            self.find_link.first(),
        ];
        // First absolute candidate wins
        if let Some(s) = all.into_iter().flatten().find(|u| is_absolute(u)) {
            return Some(s.clone());
        }
        // Fall back to the first (relative) candidate
        let raw = self.form_url.first().cloned()
            .or_else(|| self.alternative_url.first().cloned())
            .or_else(|| self.find_url.first().cloned())
            .or_else(|| self.find_link.first().cloned())?;
        // Promote modify_url when it is absolute and raw is relative
        if let Some(m) = &self.modify_url {
            if is_absolute(m) {
                return Some(m.clone());
            }
        }
        Some(raw)
    }
}

/// How the response HTML should be classified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StepOutcome {
    /// The response matched this step's `submit success` (or no matcher).
    Success,
    /// The response matched this step's `submit failed` (or any known
    /// failure phrase — captcha / duplicate / invalid) and should be
    /// retried per `submit failed retry`.
    FailedRetry,
    /// The response matched `submit failed` (or any known failure phrase)
    /// and there is no retry strategy — caller should mark job failed.
    Failed,
    /// The response matched `captcha failed` — solver or human input needed.
    CaptchaFailed,
    /// The response matched none of the known matchers.
    Unknown,
}

/// Classification of one step's response.
#[derive(Debug, Clone, Serialize)]
pub struct StepOutcomeRef {
    pub outcome: StepOutcome,
    /// The specific matcher phrase that matched, for UI surfacing.
    pub matched_phrase: Option<String>,
}

/// A single resolved step (what the driver will actually do).
#[derive(Debug, Clone, Serialize)]
pub struct StepPlan {
    /// Human-facing name, e.g. `register_step1`, `step2`, `delete_step1`.
    pub name: String,
    /// The step's phase, for UI ordering.
    pub phase: StepPhase,
    /// Phase-specific step number within the engine (1 for step1).
    pub order: u32,
    /// Navigation contract.
    pub nav: StepNav,
    /// Form field values, fully resolved (concrete, no spintax).
    pub fields: HashMap<String, String>,
    /// Expected outcome matchers (raw strings; [classify_step] matches them).
    pub success: Vec<String>,
    pub failed: Vec<String>,
    pub failed_retry: Vec<String>,
    pub captcha_failed: Vec<String>,
    /// GSA `just download=1` — a navigate-only step (URL-shortener /
    /// redirect / delete).  The driver GETs the resolved `modify url`
    /// (which already embeds the target) and reads the result page; no
    /// form data is POSTed.
    pub just_download: bool,
}

/// The phase a [StepPlan] belongs to — drives ordering + UI labels.
/// Order = GSA's execution order: register → login → submit → extra → delete.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StepPhase {
    Register,
    Login,
    Submit,
    Extra,
    Delete,
}

/// The full resolved plan for one engine run.
#[derive(Debug, Clone, Serialize)]
pub struct SubmissionPlan {
    pub engine_name: String,
    pub engine_type: String,
    pub target_url: String,
    pub anchor: Option<String>,
    pub seed: u64,
    /// Ordered steps, driver ready.  Register → Login → Submit → Extra → Delete.
    pub steps: Vec<StepPlan>,
    /// Which [`Driver`] implementation should execute this plan — decided by
    /// [`crate::routing::required_driver_kind`] from the engine's markers.
    /// The executor dispatches to `HttpDriver` or `BrowserDriver` based on
    /// this field (see F#8 queue + F#10 project runner).
    pub required_driver: crate::routing::DriverKind,
    /// Fields the plan had no value for (typically because the project did
    /// not set one) — useful for the UI to surface "fill me" prompts.
    pub unresolved_fields: Vec<String>,
}

/// The result of driving a [SubmissionPlan] with a [driver::Driver].
#[derive(Debug, Clone, Serialize)]
pub struct ExecutionReport {
    /// Number of steps that matched `submit success`.
    pub successful: usize,
    /// Number of steps that matched `submit failed` and had no retry.
    pub failed: usize,
    /// Number of steps that returned `submit failed retry`.
    pub retried: usize,
    /// Number of steps that hit a captcha.
    pub captcha_failed: usize,
    /// The created link (URL) — populated on success when the verify URL
    /// or the response itself exposes the anchor.
    pub submitted_url: Option<String>,
    /// The HTML fragment the created-link was read from (for UI debugging).
    pub submitted_url_source: Option<String>,
    /// Errors the driver reported (network, timeouts, …) — one per step.
    pub errors: Vec<String>,
    /// Per-step outcomes, in plan order.
    pub steps: Vec<StepOutcomeRef>,
}

// ---------------------------------------------------------------------------
//  Resolution: Engine + SubmissionInput -> SubmissionPlan
// ---------------------------------------------------------------------------

/// Resolve an engine + context into a [SubmissionPlan].
///
/// * Steps are ordered Register → Login → Submit → Extra → Delete.
/// * Each step's spintax is rendered **once** against the seeded context.
/// * Unresolved `%name%` tokens are preserved verbatim *and* recorded in
///   [SubmissionPlan::unresolved_fields] so the UI can flag them.
pub fn resolve_plan(engine: &Engine, input: &SubmissionInput) -> SubmissionPlan {
    // `seed = None` (direct API calls, test-engine runs) draws a fresh
    // per-submission seed from wall-clock + pid entropy — matching real
    // GSA, where every submission re-rolls its spintax.  Pools always pass
    // an explicit per-job seed, and tests fix one explicitly, so
    // byte-stable plans are preserved wherever determinism is required.
    let seed = input.seed.unwrap_or_else(|| {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        SpintaxResolver::default().seed() ^ nanos ^ (std::process::id() as u64).rotate_left(17)
    });
    let resolver = SpintaxResolver::new(seed);
    let mut ctx = input.into_context();
    // GSA-style shortener engines (`just download=1`) submit the *long* URL
    // to services that dedupe — rlu.ru mints the same short code for the
    // same long URL on every run (observed live 2026-10).  GSA copes by
    // shipping a `%random%` slug token in each INI's modify-url; many INIs
    // don't.  We make it engine-generic: append a seed-derived query
    // fragment to the submitted long URL.  The fragment never changes where
    // the redirect lands (browsers/services treat `https://site/?gsa_x` and
    // `https://site/` as the same destination) but it is a distinct URL, so
    // deduping shortener services issue a fresh code every submission.
    if engine
        .steps
        .iter()
        .any(|s| flag_truthy(s.field("just download")) && matches!(phase_and_order(&s.name).0, StepPhase::Submit))
        || engine.engine_type == EngineType::UrlShortener
    {
        if let Some(u) = ctx.get("url") {
            let sep = if u.contains('?') { "&" } else { "?" };
            let jittered = format!("{}{}gsa_{:06x}", u, sep, seed);
            ctx.set("url", &jittered);
        }
    }

    let mut ordered: Vec<StepPlan> = engine
        .steps
        .iter()
        .filter(|s| is_submission_step(&s.name))
        .map(|s| resolve_step(s, &resolver, &ctx))
        .collect();
    // Stable sort: Register < Login < Submit < Extra < Delete, then numeric.
    ordered.sort_by_key(|sp| (sp.phase, sp.order));

    let mut unresolved = Vec::new();
    for sp in ordered.iter() {
        for (k, v) in sp.fields.iter() {
            if value_is_unresolved(v) {
                unresolved.push(format!("{}: {}", k, v));
            }
        }
    }

    SubmissionPlan {
        engine_name: engine.name.clone(),
        engine_type: engine.engine_type.to_string(),
        target_url: engine.base_url(),
        anchor: input.anchor.clone(),
        seed,
        steps: ordered,
        required_driver: crate::routing::required_driver_kind(engine),
        unresolved_fields: unresolved,
    }
}

fn resolve_step(
    step: &Step,
    resolver: &SpintaxResolver,
    ctx: &Context,
) -> StepPlan {
    let (phase, order) = phase_and_order(&step.name);
    // `field()` returns `Option<&String>`; hand `split()` an `Option<&str>`.
    let f = |k: &str| step.field(k).map(|s| s.as_str());
    let nav = StepNav {
        find_link: StepNav::split(f("find link")),
        find_url: StepNav::split(f("find url")),
        alternative_url: StepNav::split(f("alternative url")),
        form_url: StepNav::split(f("form url")),
        form_name: StepNav::split(f("form name")),
        link_type: f("link type").map(str::to_string),
        modify_url: f("modify url").map(str::to_string),
    };

    let mut fields = HashMap::new();
    for (_k, f) in step.fields.iter() {
        let resolved = resolver.resolve(&f.value, ctx);
        let key_norm = f.key.to_string();
        fields.insert(key_norm, resolved);
    }

    StepPlan {
        name: step.name.clone(),
        phase,
        order,
        nav,
        fields,
        success: StepNav::split(f("submit success")),
        failed: StepNav::split(f("submit failed")),
        failed_retry: StepNav::split(f("submit failed retry")),
        captcha_failed: StepNav::split(f("captcha failed")),
        just_download: flag_truthy(step.field("just download")),
    }
}

/// GSA boolean flag (`just download=1`, `dofollow=1`, …): truthy on
/// `1` / `true` / `yes` / `y`, case-insensitive.
fn flag_truthy(v: Option<&String>) -> bool {
    match v {
        Some(s) => matches!(s.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "y"),
        None => false,
    }
}

/// Determine the (phase, order) tuple for a step by its name.
fn phase_and_order(name: &str) -> (StepPhase, u32) {
    let n = name.to_ascii_lowercase();
    let order = n.rsplit(|c: char| !c.is_ascii_digit()).next().and_then(|d| d.parse().ok()).unwrap_or(1);
    if n.starts_with("register") {
        (StepPhase::Register, order)
    } else if n.starts_with("login") {
        (StepPhase::Login, order)
    } else if n.starts_with("delete") {
        (StepPhase::Delete, order)
    } else if n.starts_with("extra") {
        (StepPhase::Extra, order)
    } else {
        (StepPhase::Submit, order)
    }
}

/// A section is a *submission step* only if it is a real GSA step
/// (`step1`, `register_step1`, `login_step1`, `delete_step1`,
/// `extra_step1`, …).  Metadata sections (`engine`, `setup`, `account`,
/// `verify`, data sections like `url`/`login`/`article`) are NOT steps and
/// must not be turned into driver actions.  Rule: after stripping
/// non-alphanumerics the name must contain `step`.
fn is_submission_step(name: &str) -> bool {
    let compact: String = name
        .to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    compact.contains("step")
}

/// Cheap heuristic for "this resolved value still has %name% in it".
fn value_is_unresolved(v: &str) -> bool {
    v.contains('%') && v.split('%').count() >= 3
}

// ---------------------------------------------------------------------------
//  Classification: response HTML -> StepOutcome
// ---------------------------------------------------------------------------

/// Classify a step's response HTML against its outcome matchers.
///
/// Matcher priority: **captcha failed** > **submit failed retry** >
/// **submit failed** > **submit success** > unknown.
///
/// Matching is case-insensitive substring; phrases are `|`-alternatives,
/// so the caller passes the already-split list of candidate phrases.
pub fn classify_step(step: &StepPlan, html: &str) -> StepOutcomeRef {
    let hay = html.to_ascii_lowercase();
    for (phrase, out) in step.captcha_failed.iter().flat_map(|p| p.split('|').map(|p| (p, StepOutcome::CaptchaFailed)))
        .chain(step.failed_retry.iter().flat_map(|p| p.split('|').map(|p| (p, StepOutcome::FailedRetry))))
        .chain(step.failed.iter().flat_map(|p| p.split('|').map(|p| (p, StepOutcome::Failed))))
        .chain(step.success.iter().flat_map(|p| p.split('|').map(|p| (p, StepOutcome::Success))))
    {
        let needle = phrase.trim().to_ascii_lowercase();
        if !needle.is_empty() && hay.contains(&needle) {
            return StepOutcomeRef {
                outcome: out,
                matched_phrase: Some(needle),
            };
        }
    }
    StepOutcomeRef { outcome: StepOutcome::Unknown, matched_phrase: None }
}

// ---------------------------------------------------------------------------
//  Result URL extraction (GSA `verify by=url` + `verify url=...`)
// ---------------------------------------------------------------------------

/// Extract the created link from the response HTML, per GSA's verify
/// contract:
///
/// 1. If the step specifies `modify url` (or a known "result URL" shape),
///    try `front`/`back` extraction between them.
/// 2. Else call [engine::extraction::find_url] to pull the first absolute
///    href that contains any of the step's `find url` candidates.
/// 3. Else, if a [SubmissionInput::url] is in the response, extract it.
pub fn extract_result_url(step: &StepPlan, html: &str, target: Option<&str>) -> Option<String> {
    use super::extraction::{find_bare_url, find_url};

    // 0. `just download=1` shorteners commonly return the created (short)
    //    link as the ENTIRE body — a bare absolute URL with no markup
    //    (e.g. rlu.ru -> `http://rlu.ru/5i1b6`).  Return it directly when
    //    its host differs from the submitted target (that is the short
    //    link, not an echo of our own URL).
    if step.just_download {
        let t = html.trim();
        if t.starts_with("http://") || t.starts_with("https://") {
            if let (Some(uh), Some(th)) = (host_of(t), target.and_then(|x| host_of(x))) {
                if uh != th {
                    return Some(t.to_string());
                }
            }
        }
    }

    // 1. Every navigation hint (`find url`, `alternative url`, `modify url`)
    //    is a matcher: pull the first absolute href on the page that contains
    //    it (after stripping glob/star/dot prefixes GSA uses for matches).
    let mut candidates: Vec<&String> =
        step.nav.find_url.iter().chain(step.nav.alternative_url.iter()).collect();
    if let Some(m) = &step.nav.modify_url {
        candidates.push(m);
    }
    for candidate in candidates.iter() {
        let c = candidate.trim().trim_start_matches('*').trim_start_matches('.');
        if c.is_empty() {
            continue;
        }
        if let Some(u) = find_url(html, c) {
            return Some(u);
        }
    }
    // 2. Fallback: the project URL itself (GSA: the submitted `%url%`).
    if let Some(t) = target {
        let bare = t
            .trim()
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .trim_start_matches("www.");
        if !bare.is_empty() {
            if let Some(u) = find_url(html, bare) {
                return Some(u);
            }
        }
    }
    // 3. Bare-text fallback: GSA also records created links that sit in
    //    confirmation text rather than an `<a href>`. Try each navigation
    //    hint, then the project URL, as a substring scan over the whole body.
    for hint in candidates.iter() {
        let h = hint.trim().trim_start_matches('*').trim_start_matches('.');
        if !h.is_empty() {
            if let Some(u) = find_bare_url(html, h) {
                return Some(u);
            }
        }
    }
    if let Some(t) = target {
        let b = t
            .trim()
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .trim_start_matches("www.");
        if !b.is_empty() {
            if let Some(u) = find_bare_url(html, b) {
                return Some(u);
            }
        }
    }
    // 4. URL-shortener / redirect engines (`just download=1`): the created
    //    (short) link is returned in a text input — `<input value="https://
    //    short.tld/abc">` — rather than in an `<a href>`.  Prefer an input
    //    whose value's host differs from the target's host (that's the
    //    short link, not the echoed long URL); fall back to the first
    //    absolute-URL input value.
    extract_input_value_url(html, target)
}

/// The navigation target for a `just download=1` step: the *resolved*
/// `modify url` (which already carries the `%url%` target), else the raw
/// navigation hint, else the plan's base URL.
fn step_target_from_modify(step: &StepPlan, base: &str) -> String {
    if let Some(m) = step.fields.get("modify url") {
        let t = m.trim();
        if !t.is_empty() {
            return t.to_string();
        }
    }
    if let Some(n) = step.nav.navigation_target() {
        if !n.is_empty() {
            return n;
        }
    }
    if !base.is_empty() {
        return base.to_string();
    }
    String::new()
}

/// Resolve a possibly-relative step target against the plan's base URL
/// (the engine's `fixed url`).
///
/// GSA INIs routinely ship *relative* targets:
///
/// * `modify url=/?save=y&url=%url%`      (tighturl.com, ux.nu, ity.im)
/// * `form url=*create.php`                (0rz.tw, v.gd, tiny.cc — the
///                                           leading `*` is a GSA glob)
///
/// A relative target handed to reqwest/CDP is a "builder error" / invalid
/// navigation.  Join it against the absolute base; pass absolute targets
/// straight through.
fn resolve_against_base(raw: &str, base: &str) -> String {
    let t = raw.trim();
    if t.is_empty() {
        return base.trim().to_string();
    }
    if t.starts_with("http://") || t.starts_with("https://") {
        return t.to_string();
    }
    let b = base.trim().trim_end_matches('/');
    if b.is_empty() {
        return t.to_string();
    }
    let path = t.strip_prefix('*').unwrap_or(t);
    if path.starts_with('/') {
        format!("{}{}", b, path)
    } else {
        format!("{}/{}", b, path)
    }
}

/// Pull the first absolute-URL `<input value="...">` (double or single
/// quoted) out of `html`.  For a URL-shortener that is the created short
/// link.  Avoids echo of the submitted (target) URL by preferring a value
/// whose host differs from the target host.
fn extract_input_value_url(html: &str, target: Option<&str>) -> Option<String> {
    let target_host = target.and_then(|t| host_of(t));
    let mut first_any: Option<String> = None;
    for q in ['"' as char, '\'' as char] {
        let needle = format!("value={q}");
        let mut from = 0usize;
        while let Some(pos) = html[from..].find(&needle) {
            let start = from + pos + needle.len();
            if let Some(e) = html[start..].find(q) {
                let v = html[start..start + e].trim();
                if v.starts_with("http://") || v.starts_with("https://") {
                    if let Some(th) = &target_host {
                        if host_of(v) == Some(th.clone()) {
                            if first_any.is_none() {
                                first_any = Some(v.to_string());
                            }
                        } else {
                            return Some(v.to_string());
                        }
                    } else if first_any.is_none() {
                        first_any = Some(v.to_string());
                    }
                }
                from = start + e + 1;
            } else {
                break;
            }
        }
    }
    first_any
}

/// Bare host (`host[:port]`) of an absolute URL, lowercased.
fn host_of(u: &str) -> Option<String> {
    let rest = u.split_once("://")?.1;
    let host = rest.split('/').next()?.to_string();
    if host.is_empty() {
        None
    } else {
        Some(host.to_ascii_lowercase())
    }
}

/// Some engines (Rentry, other pastbin/article sites) have no `submit
/// success` marker that matches the response, but the driver is redirected
/// to the *newly-created* resource after the POST (e.g. `rentry.co/` 302s to
/// `rentry.co/<slug>`, the created paste).  That final URL — the ground
/// truth of "where this submission landed" — is a strong success signal.
/// Return it only when it is an absolute URL distinct from the submitted
/// target (a bare base / same-page response does not count as created).
fn final_url_created(final_url: &str, target: &str) -> Option<String> {
    let f = final_url.trim();
    let t = target.trim();
    if (f.starts_with("http://") || f.starts_with("https://")) && !f.is_empty() && f != t {
        Some(f.to_string())
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
//  Execution loop — drive a [SubmissionPlan] through a [driver::Driver]
// ---------------------------------------------------------------------------

use super::driver::{Driver, DriverResponse};
use crate::captcha::{CaptchaContext, CaptchaSolver};

/// Drive `plan.steps` through `driver`, applying [classify_step] and
/// [extract_result_url] at each step.
///
/// This is the *shared contract* between the executor and the (as-yet
/// unimplemented) HTTP (#6) / browser (#7) drivers — they implement
/// [Driver], the executor just consumes the response.
pub async fn execute_plan(
    plan: &SubmissionPlan,
    driver: &dyn Driver,
) -> ExecutionReport {
    execute_plan_with_solver(plan, driver, None).await
}

/// As [`execute_plan`], but on a `CaptchaFailed` outcome we hand the
/// driver's page body to `solver` and — if an answer comes back — retry
/// the step once with the answer injected into the first text input.
///
/// This is the GSA parity path: GSA detects "captcha failed" in the
/// response, asks its solver (local arithmetic or 2Captcha/Anti-Captcha),
/// re-types the answer into the field named by the engine INI (`code`,
/// `code2`, `answer`… here we pick the first non-hidden / non-submit
/// input on the page, which is the common-case field) and resubmits.
/// A step that succeeds on the retry is counted as `successful`, not
/// `captcha_failed` — GSA's stats page shows the same.
pub async fn execute_plan_with_solver(
    plan: &SubmissionPlan,
    driver: &dyn Driver,
    solver: Option<&dyn CaptchaSolver>,
) -> ExecutionReport {
    let mut report = ExecutionReport {
        successful: 0,
        failed: 0,
        retried: 0,
        captcha_failed: 0,
        submitted_url: None,
        submitted_url_source: None,
        errors: Vec::new(),
        steps: Vec::new(),
    };
    for step in plan.steps.iter() {
        // GSA `verify submission` steps are named `DELETE_*` and run *after*
        // the real submission to prove ownership.  On pastbin-style article
        // engines (Rentry) that verification is *destructive*: DELETE_STEP2
        // re-saves the paste with `text={delete me...}` and a new `-delete-me`
        // slug, erasing the article we just posted.  If a submission already
        // succeeded in this plan, the verify is redundant — skip its steps so
        // the created backlink stays live (the usable-MVP behaviour), and
        // record them as handled-without-error (no counter change).
        if report.successful > 0 && step.name.to_ascii_lowercase().starts_with("delete") {
            report.steps.push(StepOutcomeRef {
                outcome: StepOutcome::Success,
                matched_phrase: Some("verify-skipped (submission already confirmed)".to_string()),
            });
            continue;
        }
        // A step that carries form fields is a *submit* step (register /
        // login / step-N); a step without fields is a read-only navigate
        // (e.g. `delete_step1` typically has `just download=1` and no form).
        //
        // GSA `just download=1` (URL-shortener / redirect engines): the
        // resolved `modify url` already embeds the target URL, the engine
        // expects a plain GET ("download") of it, and the returned page
        // carries the created (short) link.  Navigate — never POST.
        let mut fields = step.fields.clone();
        let target = if step.just_download {
            resolve_against_base(&step_target_from_modify(step, &plan.target_url), &plan.target_url)
        } else {
            resolve_against_base(
                &step
                    .nav
                    .navigation_target()
                    .unwrap_or_else(|| plan.target_url.clone()),
                &plan.target_url,
            )
        };
        let mut res: DriverResponse = if step.just_download || fields.is_empty() {
            driver.navigate(target.clone()).await
        } else {
            driver.submit(target.clone(), fields.clone(), std::time::Duration::from_secs(30)).await
        };
        if !res.ok {
            report.errors.push(format!("{}: {}", step.name, res.error.unwrap_or_default()));
            report.failed += 1;
            report.steps.push(StepOutcomeRef {
                outcome: StepOutcome::Failed,
                matched_phrase: None,
            });
            continue;
        }
        let mut outcome = classify_step(step, &res.html);
        // GSA `just download=1` (URL shorteners): the INI's `submit success`
        // marker is only a hint, and the real signal is "a short link came
        // back".  If the markers didn't classify the step but we extracted a
        // distinct short link (host != submitted target), call it a success —
        // that is exactly what GSA does for the shortener family.
        if outcome.outcome == super::StepOutcome::Unknown && step.just_download {
            if let Some(u) = extract_result_url(step, &res.html, Some(&plan.target_url)) {
                if let (Some(uh), Some(th)) = (host_of(&u), host_of(&plan.target_url)) {
                    if uh != th {
                        report.submitted_url = report.submitted_url.clone().or(Some(u.clone()));
                        report.submitted_url_source = Some(format!("{}: {}", step.name, u));
                        outcome = StepOutcomeRef {
                            outcome: StepOutcome::Success,
                            matched_phrase: Some("short-link-returned".to_string()),
                        };
                    }
                }
            }
        }
        // Generalized "a resource was created" (GSA `verify submission`):
        // many article / pastbin engines have no `submit success` marker that
        // matches the response, but the driver was redirected to the
        // newly-created page.  If classification came back Unknown, the submit
        // was ok, and we can name a created resource (the redirect target),
        // call it a successful submission.  This is what makes an "article
        // was posted" run read as success rather than "unknown outcome".
        if outcome.outcome == super::StepOutcome::Unknown && !step.just_download && res.ok {
            if let Some(u) = final_url_created(&res.final_url, &target) {
                report.submitted_url = report.submitted_url.clone().or(Some(u.clone()));
                report.submitted_url_source = Some(format!("{}: {} (created)", step.name, u));
                outcome = StepOutcomeRef {
                    outcome: StepOutcome::Success,
                    matched_phrase: Some("created-resource".to_string()),
                };
            }
        }
        // F#21 — captcha pipeline: if this step hit a captcha wall and
        // we have a solver, solve it + re-submit once with the answer.
        //
        // GSA parity (`adcopy_response=%captcha%`): many 2015-era engines
        // answer the challenge *page itself* — e.g. InsaneJournal returns
        // a challenge form (mode/ssl/code) in place of a success/failure
        // marker, i.e. the step classifies as `Unknown`.  When the page
        // carries a locally solvable artifact (inline arithmetic or a
        // captcha image), treat Unknown as a wall too and take the same
        // solve-and-resubmit path.  Pages with no artifact keep the
        // previous behaviour (no solver round-trip, clean Unknown).
        let should_solve = solver.is_some()
            && (outcome.outcome == super::StepOutcome::CaptchaFailed
                || (outcome.outcome == StepOutcome::Unknown
                    && page_has_captcha_artifact(&res.html)));
        if should_solve {
            let solver_ref = solver.expect("checked above");
            if let Some(answer) = solve_captcha(step, &res.html, solver_ref).await {
                // Inject into the captcha answer field. reCAPTCHA widgets use
                // a fixed hidden field name (`g-recaptcha-response`, the
                // standard v2 field — GSA SerLib's `insertSitekeyInText` does
                // the same); other walls fall back to the first non-hidden
                // input on the page (the common-case field).
                let field_name = if super::captcha::extract_sitekey(&res.html).is_some() {
                    "g-recaptcha-response".to_string()
                } else {
                    infer_captcha_field(&res.html)
                };
                if !fields.contains_key(&field_name) {
                    fields.entry(field_name).or_insert_with(|| answer.clone());
                }
                match driver
                    .submit(target.clone(), fields.clone(), std::time::Duration::from_secs(30))
                    .await
                {
                    retry if retry.ok => {
                        res = retry;
                        report.retried += 1;
                        outcome = classify_step(step, &res.html);
                    }
                    retry => {
                        report.errors.push(format!("{}: captcha retry failed: {}", step.name, retry.error.unwrap_or_default()));
                        report.captcha_failed += 1;
                        report.failed += 1;
                        report.steps.push(StepOutcomeRef {
                            outcome: StepOutcome::CaptchaFailed,
                            matched_phrase: outcome.matched_phrase,
                        });
                        continue;
                    }
                }
            }
        }
        let nav = extract_result_url(step, &res.html, Some(&plan.target_url));
        // Preference order for the recorded link: a real deep link from a nav
        // hint (engine-specific, strongest) → the redirect target (the created
        // resource, e.g. the new paste) → the original extraction even if it is
        // only the bare base (better than nothing).  Never overrides a link an
        // earlier step already recorded (`or()` keeps the first winner).
        let chosen = nav
            .clone()
            .filter(|n| !n.trim().is_empty() && n.trim() != target.trim())
            .or_else(|| final_url_created(&res.final_url, &target))
            .or(nav);
        if let Some(u) = chosen {
            report.submitted_url = report.submitted_url.clone().or(Some(u.clone()));
            report.submitted_url_source = Some(format!("{}: {}", step.name, u));
        }
        match outcome.outcome {
            StepOutcome::Success => report.successful += 1,
            StepOutcome::Failed => report.failed += 1,
            StepOutcome::FailedRetry => report.retried += 1,
            StepOutcome::CaptchaFailed => report.captcha_failed += 1,
            StepOutcome::Unknown => {
                report.errors.push(format!("{}: unknown outcome", step.name));
                report.failed += 1;
            }
        }
        report.steps.push(outcome);
    }
    report
}

/// GSA parity helper: does this page expose a challenge artifact the local
/// solver (or a keyed remote one) could act on — inline arithmetic
/// (`scan_arithmetic`) or a captcha image (`extract_image_url`)?  Used as a
/// cheap *gate* before dispatching a solver on an `Unknown`-classified step,
/// so plain 404s / missing-markers pages never cost a solver round-trip.
///
/// Deliberately top-document only: cross-origin challenge iframes (IJ's
/// `kr-frame`) are invisible here and report `false` — correct, since the
/// local solver cannot reach into them either.
fn page_has_captcha_artifact(html: &str) -> bool {
    super::captcha::scan_arithmetic(html).is_some()
        || super::captcha::extract_text_challenge(html).is_some()
        || super::captcha::extract_image_url(html).is_some()
        || super::captcha::extract_sitekey(html).is_some()
}

/// Ask a solver for the captcha answer on the given page body.
async fn solve_captcha(
    step: &StepPlan,
    html: &str,
    solver: &dyn CaptchaSolver,
) -> Option<String> {
    let ctx = CaptchaContext {
        html: html.to_string(),
        image_url: None,
        field_name: infer_captcha_field(html),
        engine_name: None,
        url: step.nav.navigation_target(),
        page_title: None,
        sitekey: super::captcha::extract_sitekey(html),
    };
    solver.solve(&ctx).await.ok()
}

/// Find the name of the first non-hidden `<input>` that looks like a
/// captcha answer field. Falls back to `captcha` when nothing is found.
///
/// Deliberately simple single-pass scan — no DOM. The GSA engine INIs name
/// the field (`code` / `code2` / `answer` / `captcha`), but re-parsing the
/// INI here would couple executor to enginespec parsing; the first
/// non-hidden / non-submit input is the right field in the common case.
fn infer_captcha_field(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    for tag_start in lower.match_indices("<input ").map(|(i, _)| i) {
        let tag_end = lower[tag_start..]
            .find('>')
            .map(|e| tag_start + e)
            .unwrap_or(lower.len());
        let tag = &lower[tag_start..tag_end];
        // Skip non-text widgets.
        let t = tag.strip_prefix("<input").unwrap_or(tag);
        let is_textish = !t.contains("type=\"hidden")
            && !t.contains("type=\"submit\"")
            && !t.contains("type=\"button\"")
            && !t.contains("type=\"file\"")
            && !t.contains("type=\"checkbox\"")
            && !t.contains("type=\"radio\"")
            && !t.contains("type=\"password\"")
            && !t.contains("type=\"image\"")
            && !t.contains("type=\"reset\"");
        if !is_textish {
            continue;
        }
        let ns = match t.find("name=\"") {
            Some(i) => i + 6,
            None => match t.find("name='") {
                Some(i) => i + 5,
                None => continue,
            },
        };
        let close = match t[ns..].find('"') {
            Some(c) => c,
            None => match t[ns..].find('\'') {
                Some(c) => c,
                None => continue,
            },
        };
        let name = t[ns..ns + close].to_string();
        if name.is_empty()
            || name.starts_with('_')
            || name.contains("csrf")
            || name.contains("token")
            || matches!(name.as_str(), "email" | "url" | "site" | "title" | "subject")
        {
            continue;
        }
        return name;
    }
    "captcha".to_string()
}
// ===========================================================================
//  Tests: real engine files, classification, extraction, ordering,
//  determinism, and the execute_plan loop with a stub driver.
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::MockDriver;
    use crate::enginespec::parse_engine_str;
    use std::collections::HashMap;

    #[test]
    #[ignore = "diagnostic: needs GSA portable INI on this machine"]
    fn debug_rentry_resolution() {
        let raw = std::fs::read_to_string(r"C:\Users\kingsly\GSA_SER_portable\Engines\Rentry.co.ini").unwrap();
        let eng = parse_engine_str(&raw, "Rentry").unwrap();
        let article = "Nordic Comps is a trusted source for premium, durable computer accessories. See: https://nordiccomps.com/";
        let mut extra = HashMap::new();
        for k in ["article","article_text","post_text","post_body","text","body","content","description","description_250"] { extra.insert(k.to_string(), article.to_string()); }
        let title = "Nordic Comps Review";
        for k in ["article_title","blog_title","post_title","title"] { extra.insert(k.to_string(), title.to_string()); }
        extra.insert("article_title_mod".to_string(), "nordic-comps-review".to_string());
        extra.insert("password".to_string(), "serpw12345".to_string());
        extra.insert("pop3_password".to_string(), "serpw12345".to_string());
        let input = SubmissionInput {
            url: "https://nordiccomps.com/".to_string(),
            keyword: "backlinks".to_string(),
            anchor: None,
            seed: Some(12345),
            extra,
            files: HashMap::new(),
            random_options: vec![],
        };
        let plan = resolve_plan(&eng, &input);
        eprintln!(">>> target_url = {}", plan.target_url);
        eprintln!(">>> required_driver = {:?}", plan.required_driver);
        for sp in &plan.steps {
            eprintln!(">>> STEP {} nav_target={:?} just_download={} nfields={}", sp.name, sp.nav.navigation_target(), sp.just_download, sp.fields.len());
            for (k,v) in &sp.fields {
                let sv = if v.len()>100 { v.chars().take(100).collect::<String>()+"…" } else { v.clone() };
                eprintln!("    {k} = {sv}");
            }
        }
    }

    // -- helpers -------------------------------------------------------------

    fn test_input() -> SubmissionInput {
        SubmissionInput {
            url: "https://mysite.example/".to_string(),
            keyword: "buy shoes online".to_string(),
            anchor: None,
            seed: Some(0xC0FFEE),
            extra: HashMap::from([
                ("login".to_string(), "serlogin1".to_string()),
                ("password".to_string(), "serpass123".to_string()),
                ("captcha".to_string(), "abc123".to_string()),
            ]),
            files: HashMap::from([
                ("description_250".to_string(), "A really {long|short} description about shoes.".to_string()),
            ]),
            random_options: Vec::new(),
        }
    }

    fn aardvark_engine() -> Engine {
        let raw = std::fs::read(
            r"C:\Users\kingsly\GSA_SER_portable\Engines\Aardvark Topsites.ini",
        )
        .expect("Aardvark Topsites.ini must be on this machine");
        // File is Windows-1252; lossy-decode like the real import pipeline does.
        let raw = String::from_utf8_lossy(&raw).into_owned();
        parse_engine_str(&raw, "Aardvark Topsites").unwrap()
    }

    // -- resolve_plan --------------------------------------------------------

    #[test]
    fn resolve_plan_orders_register_before_login_before_submit() {
        let eng = aardvark_engine();
        let plan = resolve_plan(&eng, &test_input());
        let names: Vec<_> = plan.steps.iter().map(|s| s.name.to_ascii_lowercase()).collect();
        let idx_register = names.iter().position(|n| n.starts_with("register")).expect("has register");
        let idx_login = names.iter().position(|n| n.starts_with("login")).expect("has login");
        let idx_submit = names.iter().position(|n| n == "step1").expect("has step1");
        let idx_delete = names.iter().position(|n| n.starts_with("delete")).expect("has delete");
        assert!(idx_register < idx_login, "register {} must come before login {}", idx_register, idx_login);
        assert!(idx_login < idx_submit, "login {} must come before submit {}", idx_login, idx_submit);
        assert!(idx_submit < idx_delete, "submit {} must come before delete {}", idx_submit, idx_delete);
    }

    #[test]
    fn resolve_plan_resolves_spintax_in_fields() {
        let eng = aardvark_engine();
        let plan = resolve_plan(&eng, &test_input());
        // The Aardvark register step fills `url=%url%` → must become the
        // concrete target URL after resolve.
        let register = plan
            .steps
            .iter()
            .find(|s| s.name.to_ascii_lowercase().starts_with("register"))
            .expect("register step");
        let url_field = register
            .fields
            .iter()
            .find(|(k, _)| k.to_ascii_lowercase() == "url")
            .map(|(_, v)| v.clone());
        // GSA's Aardvark register uses `url=%url%` which resolves to the
        // project URL.  Aardvark's STEP1 is a `just download=1` GET-link
        // submit that dedupes repeated links, so resolve_plan appends the
        // deterministic `?gsa_<seed>` anti-dedup fragment to %url%
        // (test_input seed 0xC0FFEE ⇒ `?gsa_c0ffee`).  Same redirect
        // target, distinct submission URL.
        assert_eq!(url_field.as_deref(), Some("https://mysite.example/?gsa_c0ffee"));
        // description=%description_250% → the file's random item (spintax
        // resolved; one of "A really long description about shoes." or
        // "A really short description about shoes.").
        let desc = register
            .fields
            .iter()
            .find(|(k, _)| k.to_ascii_lowercase() == "description")
            .map(|(_, v)| v.clone())
            .expect("description field");
        assert!(
            desc == "A really long description about shoes."
                || desc == "A really short description about shoes.",
            "got: {desc}"
        );
    }

    #[test]
    fn resolve_plan_records_unresolved_fields() {
        let raw = r#"
[engine]
name=Test
engine type=Directory
url=http://example.com

[step 1]
url=%url%
title=%anchor_text%
custom=%totally_undefined_variable%
"#;
        let eng = parse_engine_str(raw, "Test").unwrap();
        let input = SubmissionInput {
            url: "https://target.example/".into(),
            keyword: "k".into(),
            anchor: Some("anchor".into()),
            seed: Some(1),
            ..Default::default()
        };
        let plan = resolve_plan(&eng, &input);
        assert_eq!(plan.steps.len(), 1);
        let step = &plan.steps[0];
        assert_eq!(step.fields.get("url").map(|s| s.as_str()), Some("https://target.example/"));
        assert!(plan.unresolved_fields.iter().any(|f| f.contains("totally_undefined_variable")));
    }

    #[test]
    fn resolve_plan_is_deterministic_with_same_seed() {
        let eng = aardvark_engine();
        let a = resolve_plan(&eng, &test_input());
        let b = resolve_plan(&eng, &test_input());
        // Every field, every step, in order.
        assert_eq!(a.steps.len(), b.steps.len());
        for (sa, sb) in a.steps.iter().zip(b.steps.iter()) {
            assert_eq!(sa.name, sb.name);
            assert_eq!(sa.fields, sb.fields);
            assert_eq!(sa.nav.find_link, sb.nav.find_link);
        }
    }

    #[test]
    fn resolve_plan_preserves_unknown_variables_verbatim() {
        let raw = "[step 1]\r\nfoo=%leave%\r\nbar=%never_defined%";
        let eng = parse_engine_str(raw, "X").unwrap();
        let plan = resolve_plan(&eng, &test_input());
        let s = &plan.steps[0];
        assert_eq!(s.fields.get("foo").map(|v| v.as_str()), Some(""));
        assert_eq!(s.fields.get("bar").map(|v| v.as_str()), Some("%never_defined%"));
    }

    // -- classify_step -------------------------------------------------------

    fn step_with(success: &str, failed: &str, captcha: &str) -> StepPlan {
        StepPlan {
            name: "x".into(),
            phase: StepPhase::Submit,
            order: 1,
            nav: StepNav::default(),
            fields: HashMap::new(),
            success: StepNav::split(Some(success)),
            failed: StepNav::split(Some(failed)),
            failed_retry: Vec::new(),
            captcha_failed: StepNav::split(Some(captcha)),
            just_download: false,
        }
    }

    #[test]
    fn classify_precedence_captcha_beats_failed_beats_success() {
        let step = step_with("thank you for submitting", "username taken", "captcha does not match");
        let html = "<h1>thank you for submitting</h1>";
        assert_eq!(classify_step(&step, html).outcome, StepOutcome::Success);
        let html = "username taken on this domain";
        assert_eq!(classify_step(&step, html).outcome, StepOutcome::Failed);
        let html = "the captcha does not match the image";
        assert_eq!(classify_step(&step, html).outcome, StepOutcome::CaptchaFailed);
        // Captcha takes precedence over success even when both are present.
        let html = "thank you for submitting — but the captcha does not match";
        assert_eq!(classify_step(&step, html).outcome, StepOutcome::CaptchaFailed);
        // Unknown when none match.
        let html = "completely different page";
        assert_eq!(classify_step(&step, html).outcome, StepOutcome::Unknown);
    }

    #[test]
    fn classify_is_case_insensitive() {
        let step = step_with("Thank You", "", "");
        assert_eq!(classify_step(&step, "thAnK yOu").outcome, StepOutcome::Success);
        assert_eq!(classify_step(&step, "THANK YOU FOR JOINING").outcome, StepOutcome::Success);
    }

    // -- extract_result_url --------------------------------------------------

    #[test]
    fn extract_prefers_modify_url_when_set() {
        let mut step = step_with("", "", "");
        step.nav.modify_url = Some("https://site.example/result/".into());
        let html = r#"<a href="https://site.example/result/?id=abc123">ok</a>"#;
        // modify url is just a prefix hint; find_url will find an absolute
        // href containing that substring.
        assert_eq!(
            extract_result_url(&step, html, None),
            Some("https://site.example/result/?id=abc123".to_string())
        );
    }

    #[test]
    fn extract_falls_back_to_target_url_when_no_matcher_set() {
        let step = step_with("", "", "");
        let html = r#"<a href="https://target.example/p?id=xyz">Link</a>"#;
        assert_eq!(
            extract_result_url(&step, html, Some("https://target.example/")),
            Some("https://target.example/p?id=xyz".to_string())
        );
    }

    #[test]
    fn extract_returns_none_when_nothing_matches() {
        let step = step_with("ok", "", "");
        let html = "<p>a page with no links</p>";
        assert_eq!(extract_result_url(&step, html, Some("https://elsewhere.example/")), None);
    }

    // -- execute_plan --------------------------------------------------------

    #[tokio::test]
    async fn execute_plan_reports_success_and_extracts_url() {
        let raw = r#"
[engine]
name=Fake Directory
engine type=Directory
url=http://dir.example/

[step 1]
url=%url%
title=%anchor_text%
submit success=thank you for joining
captcha failed=
find url=dir.example/profile
"#;
        let eng = parse_engine_str(raw, "Fake Directory").unwrap();
        let plan = resolve_plan(&eng, &test_input());
        let driver = MockDriver::with_html(
            "<html><body>thank you for joining! <a href=\"https://dir.example/profile/abc123\">your profile</a></body></html>",
        );
        let report = execute_plan(&plan, &driver).await;
        assert_eq!(report.successful, 1, "expected 1 success; report = {report:?}");
        assert_eq!(report.failed, 0);
        assert_eq!(
            report.submitted_url.as_deref(),
            Some("https://dir.example/profile/abc123")
        );
    }

    #[tokio::test]
    async fn execute_plan_reports_captcha_and_unknown() {
        let raw = r#"
[engine]
name=F
engine type=Directory
url=http://d.example/

[step 1]
url=%url%
submit success=ok
submit failed=error
captcha failed=captcha
"#;
        let eng = parse_engine_str(raw, "F").unwrap();
        let plan = resolve_plan(&eng, &test_input());
        let driver = MockDriver::with_html("the captcha does not match");
        let report = execute_plan(&plan, &driver).await;
        assert_eq!(report.captcha_failed, 1);
        assert_eq!(report.steps[0].outcome, StepOutcome::CaptchaFailed);
        assert!(report
            .steps[0]
            .matched_phrase
            .as_deref()
            .unwrap_or("")
            .contains("captcha"));
    }

    #[tokio::test]
    async fn execute_plan_reports_driver_error() {
        let raw = r#"
[engine]
name=E
engine type=url-shortener
url=http://s.example/

[step 1]
url=%url%
submit success=ok
"#;
        let eng = parse_engine_str(raw, "E").unwrap();
        let plan = resolve_plan(&eng, &test_input());
        let driver = MockDriver::fail_with("connection reset");
        let report = execute_plan(&plan, &driver).await;
        assert_eq!(report.failed, 1);
        assert!(!report.errors.is_empty());
        assert!(report.errors[0].contains("connection reset"));
    }

    // -- F#21: captcha pipeline (solve → retry → success) -------------------

    /// A driver whose first `submit` returns the captcha-wall page and whose
    /// second `submit` succeeds, but ONLY when the answer field is present
    /// (proving the resolver injected it).
    struct FirstCaptchaWallThenOk {
        second_fields: std::sync::Mutex<std::collections::HashMap<String, String>>,
        calls: std::sync::atomic::AtomicUsize,
    }
    impl FirstCaptchaWallThenOk {
        fn new() -> Self {
            Self {
                second_fields: std::sync::Mutex::new(HashMap::new()),
                calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }
        fn submit_fields(&self, fields: std::collections::HashMap<String, String>) -> DriverResponse {
            use std::sync::atomic::Ordering::SeqCst;
            let n = self.calls.fetch_add(1, SeqCst) + 1;
            if n == 1 {
                // Engine wall: "2 + 2 = ?" (the LocalSolver's arithmetic) +
                // the field the executor must inject into.
                DriverResponse {
                    ok: true,
                    final_url: "mock".into(),
                    html: "2 + 2 = ? <form><input type=\"text\" name=\"code\"> <b>the captcha does not match</b></form>"
                        .into(),
                    error: None,
                }
            } else {
                *self.second_fields.lock().unwrap() = fields;
                DriverResponse {
                    ok: true,
                    final_url: "mock".into(),
                    html: "thank you for submitting".into(),
                    error: None,
                }
            }
        }
    }
    #[async_trait::async_trait]
    impl Driver for FirstCaptchaWallThenOk {
        async fn navigate(&self, _url: String) -> DriverResponse {
            self.submit_fields(HashMap::new())
        }
        async fn submit(
            &self,
            _url: String,
            fields: HashMap<String, String>,
            _timeout: std::time::Duration,
        ) -> DriverResponse {
            self.submit_fields(fields)
        }
    }

    #[test]
    fn infer_field_from_real_form() {
        let html = r#"
<form method="post">
  <input type="hidden" name="_token" value="x">
  <input type="text" name="code">
  <input type="submit" value="Go">
</form>"#;
        assert_eq!(infer_captcha_field(html), "code");
        // Falls back to `captcha` when there is no suitable input.
        assert_eq!(infer_captcha_field("<p>no form here</p>"), "captcha");
    }

    #[tokio::test]
    async fn execute_plan_with_solver_solves_captcha_and_retries() {
        let solver = crate::captcha::LocalSolver::default();
        let raw = r#"
[step 1]
url=http://s.example/
submit success=thank you for submitting
captcha failed=captcha does not match
"#;
        let eng = parse_engine_str(raw, "E").unwrap();
        let plan = resolve_plan(&eng, &test_input());
        let driver = FirstCaptchaWallThenOk::new();
        let report = execute_plan_with_solver(&plan, &driver, Some(&solver)).await;
        // The wall was retried once and then classified as a success.
        assert_eq!(report.successful, 1, "expected 1 success, got {:?}", report);
        assert_eq!(report.captcha_failed, 0);
        // And the retry actually carried the answer into field `code`.
        let second = driver.second_fields.lock().unwrap();
        assert!(second.contains_key("code"), "answer field missing in retry: {:?}", second);
    }

    /// GSA parity (`adcopy_response=%captcha%`): a register step whose first
    /// response is a *challenge page with no success/failure marker* — the
    /// step classifies as `Unknown`, not `CaptchaFailed`.  If that page
    /// exposes inline arithmetic (the classic `code` wall) the solver must
    /// still fire, the answer land in the field, and the re-submit succeed.
    /// `page_has_captcha_artifact` is the gate that makes this branch live.
    struct FirstChallengeUnknownThenOk {
        second_fields: std::sync::Mutex<std::collections::HashMap<String, String>>,
        calls: std::sync::atomic::AtomicUsize,
    }
    impl FirstChallengeUnknownThenOk {
        fn new() -> Self {
            Self {
                second_fields: std::sync::Mutex::new(HashMap::new()),
                calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }
        fn submit_fields(&self, fields: HashMap<String, String>) -> DriverResponse {
            let n = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let html = if n == 0 {
                // No success/failure marker — just an arithmetic challenge.
                "<html><body>What is 12 + 5?\n\
                 <form><input type=\"text\" name=\"code\"></form></body></html>"
                    .to_string()
            } else {
                if fields.get("code").map(String::as_str) != Some("17") {
                    "<html><body>challenge did not match</body></html>".to_string()
                } else {
                    "<html><body>thank you for submitting</body></html>".to_string()
                }
            };
            if n > 0 {
                *self.second_fields.lock().unwrap() = fields;
            }
            DriverResponse {
                ok: true,
                final_url: "mock".into(),
                html,
                error: None,
            }
        }
    }
    #[async_trait::async_trait]
    impl Driver for FirstChallengeUnknownThenOk {
        async fn navigate(&self, _url: String) -> DriverResponse {
            self.submit_fields(HashMap::new())
        }
        async fn submit(
            &self,
            _url: String,
            fields: HashMap<String, String>,
            _timeout: std::time::Duration,
        ) -> DriverResponse {
            self.submit_fields(fields)
        }
    }

    #[tokio::test]
    async fn unknown_outcome_with_arithmetic_challenge_is_solved_and_retried() {
        let solver = crate::captcha::LocalSolver::default();
        // NOTE: no `captcha failed=` marker — this is what makes the wall
        // classify as Unknown (the IJ / sharecg shape), exercising the
        // `page_has_captcha_artifact` gate, not the old CaptchaFailed path.
        let raw = r#"
[step 1]
url=http://s.example/
submit success=thank you for submitting
"#;
        let eng = parse_engine_str(raw, "E").unwrap();
        let plan = resolve_plan(&eng, &test_input());
        let driver = FirstChallengeUnknownThenOk::new();
        let report = execute_plan_with_solver(&plan, &driver, Some(&solver)).await;
        assert_eq!(report.successful, 1, "expected 1 success, got {:?}", report);
        assert_eq!(report.failed, 0);
        assert_eq!(report.retried, 1, "solver should have re-submitted once");
        let second = driver.second_fields.lock().unwrap();
        // 12 + 5 = 17 — the LocalSolver's arithmetic answer, injected by name.
        assert_eq!(second.get("code").map(String::as_str), Some("17"),
            "answer must ride the retry; got {:?}", second);
    }

    // -- Article / pastbin engine (Rentry shape): no `submit success` marker,
    //    so the step classifies Unknown — but the driver was redirected to the
    //    newly-created resource.  Creation must be treated as success and the
    //    created URL must be captured from the redirect (final_url), not from
    //    an HTML hint.
    #[tokio::test]
    async fn article_submit_redirected_to_created_resource_is_success() {
        let raw = r#"
[engine]
engine type=Article
url=https://paste.example/

[step 1]
url=https://paste.example/
text=%article%
"#;
        let eng = parse_engine_str(raw, "Paste").unwrap();
        assert_eq!(eng.base_url(), "https://paste.example/");
        let input = test_input();
        let plan = resolve_plan(&eng, &input);
        assert_eq!(plan.target_url, "https://paste.example/");
        // The response body carries no success marker; the driver reports the
        // created paste as the post-redirect URL (final_url), distinct from
        // the submission target.
        let mut driver = MockDriver::with_html("<html><body>no explicit marker</body></html>");
        driver.final_url = Some("https://paste.example/98172-buy-shoes-online".to_string());
        let report = execute_plan(&plan, &driver).await;
        assert_eq!(report.successful, 1, "created resource must count as success: {:?}", report);
        assert_eq!(report.failed, 0, "{:?}", report);
        assert!(
            report.errors.iter().all(|e| !e.contains("unknown outcome")),
            "no 'unknown outcome' error expected; got {:?}",
            report.errors
        );
        assert_eq!(
            report.submitted_url.as_deref(),
            Some("https://paste.example/98172-buy-shoes-online")
        );
    }

    // -- The destructive GSA `verify submission` (DELETE_*) steps: once the
    //    create-step succeeded, they must be *skipped* (not re-saved), so the
    //    posted article is not erased.  Assert the DELETE step is marked
    //    verify-skipped, did not add a failure, and only the create counts.
    #[tokio::test]
    async fn destructive_delete_verify_steps_are_skipped_after_success() {
        let raw = r#"
[engine]
engine type=Article
url=https://paste.example/

[step 1]
url=https://paste.example/
text=%article%

[delete step 2]
url=https://paste.example/
text=delete me
new_url=%random-1-99999%-delete-me
"#;
        let eng = parse_engine_str(raw, "Paste2").unwrap();
        let input = test_input();
        let plan = resolve_plan(&eng, &input);
        let names: Vec<_> = plan.steps.iter().map(|s| s.name.to_ascii_lowercase()).collect();
        assert_eq!(plan.steps.len(), 2, "expect create + delete step; got {:?}", names);
        assert!(names.iter().any(|n| n.starts_with("delete")), "a DELETE_* step must be present: {:?}", names);

        let mut driver = MockDriver::with_html("<html><body>no marker</body></html>");
        driver.final_url = Some("https://paste.example/777-live-article".to_string());
        let report = execute_plan(&plan, &driver).await;

        assert_eq!(report.successful, 1, "only the create step should count as success: {:?}", report);
        assert_eq!(report.failed, 0, "no step should fail once the article was created: {:?}", report);
        assert_eq!(report.submitted_url.as_deref(), Some("https://paste.example/777-live-article"));
        // The delete step must be recorded as skipped, not executed-as-unknown.
        let skipped = report.steps.iter().any(|s| s.matched_phrase.as_deref() == Some("verify-skipped (submission already confirmed)"));
        assert!(skipped, "DELETE_* step should be verify-skipped; steps={:?}", report.steps);
    }


    // -- F#27 (Stage A) — GSA `just download=1` URL-shortener family ---------

    /// 3le.ru — a real GSA URL-shortener engine.  The executor must:
    ///  * detect `just download=1` (navigate-only, no form POST),
    ///  * target the *resolved* `modify url` (carrying the project URL),
    ///  * base the plan on `fixed url` (3le.ru has no `url=`),
    ///  * classify `submit success` and extract the created short link
    ///    from the returned `<input value="...">`.
    #[tokio::test]
    async fn just_download_shortener_resolves_navigates_and_extracts_short_link() {
        let bytes =
            std::fs::read(r"C:\Users\kingsly\GSA_SER_portable\Engines\3le.ru.ini")
                .expect("3le.ru.ini must be on this machine");
        let raw = String::from_utf8_lossy(&bytes).into_owned();
        let eng = parse_engine_str(&raw, "3le.ru").unwrap();

        // `fixed url` is the site base; `url=` is empty, so base_url() falls back.
        assert_eq!(eng.base_url(), "https://3le.ru/");
        assert!(eng.dofollow, "3le.ru is dofollow");
        assert_eq!(
            eng.engine_type,
            crate::enginespec::EngineType::UrlShortener
        );

        let input = SubmissionInput {
            url: "https://nordiccomps.com/".to_string(),
            keyword: "nordiccomps".to_string(),
            anchor: None,
            seed: Some(0xC0FFEE),
            extra: HashMap::new(),
            files: HashMap::new(),
            random_options: Vec::new(),
        };
        let plan = resolve_plan(&eng, &input);
        assert_eq!(plan.target_url, "https://3le.ru/");
        let step = plan.steps.iter().find(|s| s.name.eq_ignore_ascii_case("step1"))
            .expect("3le.ru has STEP1");
        assert!(step.just_download, "STEP1 is just download=1");

        // The navigation target must be the resolved `modify url`, which
        // already embeds the project URL (this is what a browser GETs).
        // Seed 0xC0FFEE ⇒ deterministic `?gsa_c0ffee` anti-dedup fragment
        // (shortener improvement: distinct submission per run, same landing).
        let expect_target = "https://3le.ru/includes/actions.php?long_url=https://nordiccomps.com/?gsa_c0ffee";
        assert_eq!(step_target_from_modify(step, &plan.target_url), expect_target);

        // The shortener answers with the created short link in a text input.
        let driver = MockDriver::with_html(
            "<html><body>\
             <input type=\"text\" value=\"https://3le.ru/xyZ9\" />\
             <a href=\"https://3le.ru/xyZ9\">your short link</a>\
             </body></html>",
        );
        let report = execute_plan(&plan, &driver).await;
        assert_eq!(report.successful, 1, "submit success must classify Success: {:?}", report);
        assert_eq!(report.failed, 0);
        assert_eq!(report.submitted_url.as_deref(), Some("https://3le.ru/xyZ9"));
    }

    /// Regression: the "STEP1: builder error" came from POSTing to an empty
    /// target (fixed-url shortener had no `form url`).  Now a just-download
    /// step navigates the populated modify-url, so the driver never sees "".
    #[tokio::test]
    async fn just_download_step_never_targets_empty_url() {
        let bytes =
            std::fs::read(r"C:\Users\kingsly\GSA_SER_portable\Engines\3le.ru.ini")
                .expect("3le.ru.ini must be on this machine");
        let raw = String::from_utf8_lossy(&bytes).into_owned();
        let eng = parse_engine_str(&raw, "3le.ru").unwrap();
        let input = SubmissionInput {
            url: "https://nordiccomps.com/".to_string(),
            keyword: "nordiccomps".to_string(),
            anchor: None, seed: Some(1), extra: HashMap::new(),
            files: HashMap::new(), random_options: Vec::new(),
        };
        let plan = resolve_plan(&eng, &input);
        let step = &plan.steps[0];
        let t = step_target_from_modify(step, &plan.target_url);
        assert!(!t.is_empty(), "just-download target must never be empty");
        assert!(t.starts_with("https://"), "target must be absolute: {t}");
        // base fallback also non-empty so the plan target_url is usable.
        assert!(!plan.target_url.is_empty());
        let _ = eng;
    }

    /// F#28 — the shortener family where the site returns the created short
    /// link as the ENTIRE bare body (no `<a href>`, no `<input>`), e.g.
    /// rlu.ru -> `http://rlu.ru/5i1b6`.  The INI's `submit success` marker
    /// (`http://rlu.ru/?*`) no longer matches the bare body, so the
    /// just-download "a distinct short link came back" rule must classify
    /// it as a success and extract the link.
    #[tokio::test]
    async fn bare_body_shortener_classifies_success_and_extracts_link() {
        let bytes =
            std::fs::read("C:/Users/kingsly/GSA_SER_portable/Engines/rlu.ru.ini")
                .expect("rlu.ru.ini must be on this machine");
        let raw = String::from_utf8_lossy(&bytes).into_owned();
        let eng = parse_engine_str(&raw, "rlu.ru").unwrap();
        assert!(eng.dofollow, "rlu.ru is dofollow");

        let input = SubmissionInput {
            url: "https://nordiccomps.com/".to_string(),
            keyword: "nordiccomps".to_string(),
            anchor: None, seed: Some(7), extra: HashMap::new(),
            files: HashMap::new(), random_options: Vec::new(),
        };
        let plan = resolve_plan(&eng, &input);
        let step = plan.steps.iter().find(|s| s.just_download).expect("rlu.ru STEP1 is just download");
        // Its `submit success` marker should NOT substring-match the bare body —
        // this is exactly the case the new fallback covers.
        let bare = "http://rlu.ru/5i1b6";
        let outcome = classify_step(step, bare);
        assert_eq!(outcome.outcome, StepOutcome::Unknown,
            "marker alone must not classify the bare body: {:?}", outcome);

        let driver = MockDriver::with_html(bare);
        let report = execute_plan(&plan, &driver).await;
        assert_eq!(report.successful, 1, "bare-body shortener must succeed: {:?}", report);
        assert_eq!(report.failed, 0);
        assert_eq!(report.submitted_url.as_deref(), Some("http://rlu.ru/5i1b6"));
        let _ = bare;
    }

    /// Regression: CDP `Page.navigate` rejects relative URLs but GSA INIs
    /// routinely use `form url=*/path` alongside `modify url=http://host/path`.
    /// `navigation_target()` must prefer the absolute candidate or promote
    /// `modify_url` when all `form_url` entries are relative.
    #[tokio::test]
    async fn navigation_target_prefers_absolute_over_relative() {
        let nav = StepNav {
            find_link: vec![],
            find_url: vec![],
            alternative_url: vec!["http://thedatahub.org/user/login".into()],
            form_url: vec!["*/user/register".into()],
            form_name: vec![],
            link_type: None,
            modify_url: Some("http://thedatahub.org/user/register".into()),
        };
        // absolute alternative_url should win over relative form_url
        let t = nav.navigation_target().unwrap();
        assert!(t.starts_with("http://"), "got {t}");
        assert!(t.contains("thedatahub.org"), "got {t}");

        // when ONLY form_url is relative, modify_url (absolute) must be returned
        let nav2 = StepNav {
            find_link: vec![],
            find_url: vec![],
            alternative_url: vec![],
            form_url: vec!["*/user/register".into()],
            form_name: vec![],
            link_type: None,
            modify_url: Some("http://thedatahub.org/user/register".into()),
        };
        let t2 = nav2.navigation_target().unwrap();
        assert!(t2.starts_with("http://"), "must be absolute: {t2}");
        assert!(t2.contains("thedatahub.org/user/register"), "got {t2}");

        // all relative + no modify_url → fall back to the relative candidate
        let nav3 = StepNav {
            find_link: vec![],
            find_url: vec![],
            alternative_url: vec![],
            form_url: vec!["/relative/path".into()],
            form_name: vec![],
            link_type: None,
            modify_url: None,
        };
        let t3 = nav3.navigation_target().unwrap();
        assert_eq!(t3, "/relative/path");
    }
    // ── relative-target joining (FIX: "STEP1: builder error") ────────────

    #[test]
    fn resolve_against_base_joins_relative_modify_url() {
        // tighturl.com shape: modify url=/?save=y&url=%url%
        assert_eq!(
            resolve_against_base("/?save=y&url=x", "http://tighturl.com/"),
            "http://tighturl.com/?save=y&url=x"
        );
        // ux.nu shape: modify url=/api/short?url=%url%
        assert_eq!(
            resolve_against_base("/api/short?url=x", "https://ux.nu"),
            "https://ux.nu/api/short?url=x"
        );
    }

    #[test]
    fn resolve_against_base_strips_glob_form_url() {
        // 0rz.tw shape: form url=*create.php
        assert_eq!(
            resolve_against_base("*create.php", "http://0rz.tw/"),
            "http://0rz.tw/create.php"
        );
        // v.gd shape: form url=*create.php with base without trailing slash
        assert_eq!(
            resolve_against_base("*create.php", "http://v.gd"),
            "http://v.gd/create.php"
        );
    }

    #[test]
    fn resolve_against_base_passthrough_and_empty() {
        assert_eq!(
            resolve_against_base("https://is.gd/create.php?x=1", "http://is.gd/"),
            "https://is.gd/create.php?x=1"
        );
        assert_eq!(resolve_against_base("", "http://b.example/"), "http://b.example/");
    }

    #[test]
    fn form_step_with_glob_form_url_resolves_absolute() {
        // 0rz.tw shape INI: form POST to *create.php — the plan's submit
        // target must be absolute after the join fix.
        let ini = r#"
[setup]
enabled=1
url must have=http://0rz.tw/*
engine type=URL Shortener
fixed url=http://0rz.tw/

[STEP1]
submit success=id="link" value="http://0rz.tw/
form id=redirect-form
form url=*create.php
url=%url%
"#;
        let eng = parse_engine_str(ini, "0rz").unwrap();
        let input = SubmissionInput {
            url: "https://nordiccomps.com/".to_string(),
            ..Default::default()
        };
        let plan = resolve_plan(&eng, &input);
        let step = &plan.steps[0];
        let raw = step.nav.navigation_target().unwrap();
        let target = resolve_against_base(&raw, &plan.target_url);
        assert_eq!(target, "http://0rz.tw/create.php", "glob form url must join against fixed url");
    }

    // ── unique-target jitter (FIX: same-URL re-submissions) ──────────────

    #[test]
    fn shortener_plan_jitters_target_for_deduping_services() {
        // rlu.ru shape: just-download GET of modify url=...nam=%url%
        let ini = r#"
[setup]
enabled=1
engine type=URL Shortener
fixed url=http://rlu.ru/

[URL]
type=url

[STEP1]
submit success=http://rlu.ru/?*
modify url=http://rlu.ru/index.sema?a=ujax&sa=links_add&nam=%url%
just download=1
"#;
        let eng = parse_engine_str(ini, "rlu").unwrap();
        let mk = |seed: u64| SubmissionInput {
            url: "https://nordiccomps.com/".to_string(),
            seed: Some(seed),
            ..Default::default()
        };
        let p1 = resolve_plan(&eng, &mk(0x111));
        let p2 = resolve_plan(&eng, &mk(0x222));
        let url1 = p1.steps[0].fields.get("modify url").cloned().unwrap();
        let url2 = p2.steps[0].fields.get("modify url").cloned().unwrap();
        assert_eq!(url1, "http://rlu.ru/index.sema?a=ujax&sa=links_add&nam=https://nordiccomps.com/?gsa_000111");
        assert_eq!(url2, "http://rlu.ru/index.sema?a=ujax&sa=links_add&nam=https://nordiccomps.com/?gsa_000222");
        assert_ne!(url1, url2, "same long URL must yield distinct submissions");
    }

    #[test]
    fn jitter_does_not_touch_non_shortener_engines() {
        // Article-style engine (POST form, no just download) keeps %url% as-is.
        let ini = r#"
[setup]
enabled=1
engine type=Article
fixed url=http://blog.example/

[STEP1]
submit success=Posted
form url=*post.php
url=%url%
title=T
"#;
        let eng = parse_engine_str(ini, "art").unwrap();
        let input = SubmissionInput {
            url: "https://nordiccomps.com/".to_string(),
            seed: Some(0x333),
            ..Default::default()
        };
        let plan = resolve_plan(&eng, &input);
        let url = plan.steps[0].fields.get("url").cloned().unwrap();
        assert_eq!(url, "https://nordiccomps.com/");
    }

}
