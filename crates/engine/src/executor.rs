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
    /// T5 — base page for DISCOVERY engines (engines with no own
    /// `base_url()` — guestbooks / URL engines whose INI steps are relative
    /// to the *found* site).  Usually one of the project's searched sites.
    /// Ignored by engines that declare their own base URL (those win).
    pub site_url: Option<String>,
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
    /// T12 — `verify url=...` (GSA submit verification): the INI author's
    /// explicit pattern for the URL this step *creates*, e.g. Scribd's
    /// `find url=http://www.scribd.com/doc/[0-9]*[0-9]/*`.  Highest-priority
    /// input to [extract_result_url].  A `%name%` reference is resolved to
    /// the named section's `find url` / `find link` during planning.
    pub verify_url: Option<String>,
    /// T13 — GSA `type=extract` verify section (`front1=` / `front2=` /
    /// `back=`): capture the URL between the delimiters in the response
    /// HTML.  E.g. yy7's `[verifyurl] front1=<div class="resultLink">
    /// front2=href=" back="` yields the freshly-created short link — the
    /// verifiable permalink a professional backlinker reports.  Populated
    /// from the referenced section during planning when the section's
    /// `type` field is `extract`.
    pub verify_extract: Option<(String, String, String)>,
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
    // ── T20 — GSA directive suite (reverse-engineered from the 991-engine INI corpus) ──
    /// `anchor text=…` — step-level anchor override (replaces %anchor_text%).
    pub anchor_text: Option<String>,
    /// `upcase=1` — uppercase the step's anchor text.
    pub upcase: bool,
    /// `capitalize words=1` — title-case the step's anchor text.
    pub capitalize_words: bool,
    /// `min length` / `max length` — anchor length clamps (max truncates at a word boundary).
    pub max_len: Option<usize>,
    /// `page must have[N]` — post-submission page content the site must echo.
    pub page_must_haves: Vec<String>,
    /// `url must have[N]` — the created link must contain the phrase.
    pub url_must_haves: Vec<String>,
    /// `seconds to wait before submission[=N]` (+ optional `… condition=TEXT`).
    pub wait_before_secs: Option<u64>,
    pub wait_before_cond: Option<String>,
    /// GSA blog-search flow: drive a search first, then act on the first usable result.
    pub search: Option<StepSearch>,
}

/// GSA `use blog search=<url>` (+`search term`, `allow return`,
/// `skip content on`, `add keyword to search`, `extract keywords`,
/// `skip ext links on`) — one step's search flow, resolved.
#[derive(Debug, Clone, Serialize)]
pub struct StepSearch {
    pub url: String,
    pub term: String,
    pub add_keyword: bool,
    /// `allow return=1` permits results linking back to the submitted site.
    pub allow_return: bool,
    /// `skip content on=…` phrases disqualify a candidate (URL or label).
    pub skip_content: Vec<String>,
    /// `skip ext links on=1`: only same-host results.
    pub skip_ext: bool,
    /// `extract keywords=1`: search the project keyword, not the custom term.
    pub extract_keywords: bool,
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
    /// Operator pause gate shared with the run leader: while `true`,
    /// in-flight jobs wait between steps instead of advancing.
    /// The leader stops dispatching new engines meanwhile.
    #[serde(skip)]
    pub pause: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

/// The result of driving a [SubmissionPlan] with a [driver::Driver].
#[derive(Debug, Clone, Serialize)]
pub struct ExecutionReport {
    /// Number of steps that matched `submit success`.
    pub successful: usize,
    /// T13 — number of steps that confirmed **content creation**: a
    /// `Success` outcome in the Submit or Extra phase.  Login/Register
    /// successes are infrastructure — the operator cannot verify a link
    /// through them — so a professional backlinker counts only these.
    pub content_success: usize,
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
    // T12 — resolve `verify url=%name%` references to the named extract
    // section's pattern (Scribd: `%verify_url%` → the `[verify_url]`
    // section's `find url`).  Section names are case-insensitive in GSA.
    let section_vars: std::collections::HashMap<String, String> = engine
        .steps
        .iter()
        .filter_map(|s| {
            let pat = s.field("find url").cloned().or_else(|| s.field("find link").cloned())?;
            Some((s.name.to_ascii_lowercase(), pat))
        })
        .collect();
    // T13 — GSA `type=extract` sections (yy7 `[verifyurl] front1=...
    // front2=href=" back="`) are URL delimiters, not patterns: when a step
    // references one via `%name%`, resolve it to the (front1, front2, back)
    // tuple and let execute-time extraction capture the created link.
    let section_extractors: std::collections::HashMap<String, (String, String, String)> = engine
        .steps
        .iter()
        .filter(|s| {
            s.field("type")
                .map(|t| t.trim().eq_ignore_ascii_case("extract"))
                .unwrap_or(false)
        })
        .filter_map(|s| {
            // Old GSA syntax uses `front=`; new syntax splits it into
            // `front1=`/`front2=`.  Both arrive as (front1, front2, back).
            let front1 = s
                .field("front1")
                .cloned()
                .or_else(|| s.field("front").cloned())
                .unwrap_or_default();
            let front2 = s.field("front2").cloned().unwrap_or_default();
            let back = s.field("back").cloned().unwrap_or_default();
            if front1.is_empty() && front2.is_empty() {
                None
            } else {
                Some((s.name.to_ascii_lowercase(), (front1, front2, back)))
            }
        })
        .collect();
    for sp in ordered.iter_mut() {
        if let Some(v) = sp.nav.verify_url.as_ref() {
            let inner = v
                .trim()
                .strip_prefix('%')
                .and_then(|s| s.strip_suffix('%'))
                .map(|s| s.to_ascii_lowercase());
            if let Some(name) = inner {
                if let Some(x) = section_extractors.get(&name) {
                    sp.nav.verify_extract = Some(x.clone());
                    sp.nav.verify_url = None;
                } else if let Some(pat) = section_vars.get(&name) {
                    sp.nav.verify_url = Some(pat.clone());
                }
            }
        }
    }
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
        target_url: {
            // T5 — discovery engines (no own base URL) run against the
            // searched site that the runner passes in.
            let base = engine.base_url();
            if !base.is_empty() {
                base
            } else {
                input
                    .site_url
                    .clone()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_default()
            }
        },
        anchor: input.anchor.clone(),
        seed,
        steps: ordered,
        required_driver: crate::routing::required_driver_kind(engine),
        unresolved_fields: unresolved,
        // T19 — filled by the runner when a run leader provides the shared
        // pause gate; `None` = never paused.
        pause: None,
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
        verify_url: f("verify url").map(str::to_string),
        verify_extract: None,
    };

    // T20 — anchor directives: `anchor text` (+`upcase`/`capitalize words`/
    // `max length`) apply to THIS step's %anchor_text% token only.  The
    // final anchor is spintax-resolved first (so `{a|b}` picks a branch),
    // then transformed, then substituted before the rest of the value is
    // resolved.
    let upcase = flag_truthy(step.field("upcase"));
    let cap_words = flag_truthy(step.field("capitalize words"));
    let max_len = step.field("max length").and_then(|s| s.trim().parse().ok());
    let anchor_override = step
        .field("anchor text")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let anchor_final: Option<String> = if let Some(base) = anchor_override
        .clone()
        .or_else(|| ctx.get("anchor_text").map(|s| s.to_string()))
    {
        if base.is_empty() {
            None
        } else {
            let mut a = resolver.resolve(&base, ctx);
            if upcase {
                a = a.to_uppercase();
            }
            if cap_words {
                a = titlecase_words(&a);
            }
            if let Some(mx) = max_len {
                a = truncate_at_word(&a, mx);
            }
            Some(a)
        }
    } else {
        None
    };
    let mut fields = HashMap::new();
    for (_k, f) in step.fields.iter() {
        let raw = f.value.as_str();
        let resolved = if let Some(final_anchor) = &anchor_final {
            if raw.contains("%anchor_text%") {
                let substituted = raw.replace("%anchor_text%", final_anchor);
                resolver.resolve(&substituted, ctx)
            } else {
                resolver.resolve(raw, ctx)
            }
        } else {
            resolver.resolve(raw, ctx)
        };
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
        anchor_text: step
            .field("anchor text")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        upcase: flag_truthy(step.field("upcase")),
        capitalize_words: flag_truthy(step.field("capitalize words")),
        max_len: step.field("max length").and_then(|s| s.trim().parse().ok()),
        page_must_haves: collect_numbered(step, "page must have"),
        url_must_haves: collect_numbered(step, "url must have"),
        wait_before_secs: step
            .field("seconds to wait before submission")
            .and_then(|s| s.trim().parse::<u64>().ok()),
        wait_before_cond: step
            .field("seconds to wait before submission condition")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        search: step
            .field("use blog search")
            .map(|u| u.trim().to_string())
            .filter(|u| !u.is_empty())
            .map(|url| {
                let term = step
                    .field("search term")
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default();
                StepSearch {
                    url,
                    term,
                    add_keyword: flag_truthy(step.field("add keyword to search")),
                    allow_return: flag_truthy(step.field("allow return")),
                    skip_content: StepNav::split(step
                        .field("skip content on")
                        .map(|s| s.as_str()))
                        .into_iter()
                        .filter(|s| !s.is_empty())
                        .collect(),
                    skip_ext: flag_truthy(step.field("skip ext links on")),
                    extract_keywords: flag_truthy(step.field("extract keywords")),
                }
            }),
    }
}

/// `page must have` / `url must have` + their numbered forms (`…1`, `…2` …).
fn collect_numbered(step: &Step, base: &str) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(v) = step.field(base) {
        out.push(v.trim().to_string());
    }
    for i in 1..=10 {
        let k = format!("{}{}", base, i);
        if let Some(v) = step.field(&k) {
            out.push(v.trim().to_string());
        }
    }
    out.into_iter().filter(|s| !s.is_empty()).collect()
}

/// `capitalize words` — title-case each spintax branch independently (braces/
/// separators stay intact).
fn titlecase_words(s: &str) -> String {
    s.split('|')
        .map(|seg| {
            seg.split(' ')
                .map(|w| {
                    let mut seen_alpha = false;
                    w.chars()
                        .map(|ch| {
                            if ch.is_alphabetic() {
                                let r = if seen_alpha {
                                    ch.to_lowercase().to_string()
                                } else {
                                    seen_alpha = true;
                                    ch.to_uppercase().to_string()
                                };
                                r
                            } else {
                                ch.to_string()
                            }
                        })
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>()
        .join("|")
}

/// `max length=N` — truncate the anchor to whole words that fit within N chars.
fn truncate_at_word(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out = String::new();
    for w in s.split_inclusive(' ') {
        if (out.chars().count() + w.chars().count()) > max && !out.is_empty() {
            break;
        }
        out.push_str(w);
    }
    out.trim_end().to_string()
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

    // 1. `verify url=...` (T12) is the INI author's explicit pattern for the
    //    URL THIS step creates (Scribd: `scribd.com/doc/[0-9]*[0-9]/*`).  It
    //    names the created resource precisely, so it outranks the generic nav
    //    hints — which can self-reference the form page (upload/login) — and
    //    every "project URL echo" fallback below.
    if let Some(v) = step.nav.verify_url.as_deref() {
        let v = v.trim().trim_start_matches('*').trim_start_matches('.');
        if !v.is_empty() {
            if let Some(u) = find_url(html, v).or_else(|| find_bare_url(html, v)) {
                return Some(u);
            }
        }
    }
    // 1.5 (T13) — `type=extract` verify section (yy7 / yuarel / old-GSA
    //    `front=` style): the INI author's own delimiters for the created
    //    link.  This is the most precise evidence the response carries.
    if let Some((f1, f2, b)) = &step.nav.verify_extract {
        if let Some(u) = extract_front_back_url(html, f1, f2, b) {
            return Some(u);
        }
    }
    // 1.6 (T13) — "creation echo": the engine anchored the submitted
    //    content's title in an `<a href>` in the response (Scribd's upload
    //    step matches `submit success=>%article_title%</a>` — the title
    //    string sits directly inside the created page's link).  Walk back
    //    from the marker text to its href.  Outranks the nav hints below
    //    because those can self-reference the form/upload page.
    if let Some(u) = creation_echo_href(step, html) {
        return Some(u);
    }
    // 2. Every navigation hint (`find url`, `alternative url`, `modify url`)
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

/// T13 — GSA `type=extract` delimiters: locate `front1` in the response,
/// then from there find `front2`, and capture up to `back` (or the next
/// `<`).  When `front1` is empty, `front2` acts as the front anchor.
/// Returns the first captured token that is a plausible absolute URL.
pub fn extract_front_back_url(html: &str, front1: &str, front2: &str, back: &str) -> Option<String> {
    let f1 = front1.trim();
    let f2 = front2.trim();
    let b = back.trim();
    if f1.is_empty() && f2.is_empty() {
        return None;
    }
    let mut cursor = 0usize;
    loop {
        let anchor = if f1.is_empty() { f2 } else { f1 };
        let Some(p) = html[cursor..].find(anchor) else {
            return None;
        };
        let after1 = cursor + p + anchor.len();
        let after2 = if f1.is_empty() {
            after1
        } else if f2.is_empty() {
            after1
        } else {
            let Some(o) = html[after1..].find(f2) else {
                cursor = after1 + 1;
                continue;
            };
            after1 + o + f2.len()
        };
        let seg = &html[after2..];
        let end = if b.is_empty() {
            seg.find('<').unwrap_or(seg.len())
        } else {
            let e = seg.find(b).unwrap_or(seg.len());
            seg.find('<').map(|t| e.min(t)).unwrap_or(e)
        };
        let raw = seg[..end].trim();
        // Old-GSA front/back pairs can leave a leftover quote/bracket in
        // the capture (`front=input.value=["` → capture ends `..."`).
        let raw = raw.trim_matches(|c| c == '"' || c == '\'' || c == '[' || c == ']' || c == '`');
        if (raw.starts_with("http://") || raw.starts_with("https://")) && raw.len() >= 10 {
            return Some(raw.to_string());
        }
        cursor = after2 + 1;
    }
}

/// T13 — "creation echo" fallback (Scribd-style): the engine's response
/// anchors the submitted content in an `<a href>` — the step's
/// `submit success` marker literally reads `>%article_title%</a>`, so the
/// echoed text sits directly inside the created page's link.  Find the
/// marker text in the response and walk back to the nearest `href`.
/// The first absolute, non-auth/wall href wins.
fn creation_echo_href(step: &StepPlan, html: &str) -> Option<String> {
    let mut texts: Vec<String> = Vec::new();
    for m in &step.success {
        let m = m.trim();
        let Some(gt) = m.find('>') else { continue; };
        let Some(close) = m[gt + 1..].rfind('<') else { continue; };
        let t = m[gt + 1..gt + 1 + close].trim();
        if t.chars().count() >= 3 {
            texts.push(t.to_string());
        }
    }
    for (k, v) in &step.fields {
        if k.to_ascii_lowercase().contains("title") {
            let t = v.trim();
            if t.chars().count() >= 3 && !t.contains('%') {
                texts.push(t.to_string());
            }
        }
    }
    let hay = html.to_ascii_lowercase();
    for t in &texts {
        let needle = t.to_ascii_lowercase();
        let mut from = 0usize;
        while let Some(pos) = hay[from..].find(&needle) {
            let abs = from + pos;
            let window_start = abs.saturating_sub(400);
            let window = &html[window_start..abs];
            if let Some(hp) = window.rfind("href=") {
                let after_byte = window_start + hp + 5;
                let rest = &html[after_byte..abs];
                let val = if let Some(q) = rest.chars().next() {
                    if q == '"' || q == '\'' {
                        match rest[q.len_utf8()..].find(q) {
                            Some(i) => Some(rest[q.len_utf8()..q.len_utf8() + i].to_string()),
                            None => None,
                        }
                    } else {
                        rest.split(|c: char| c.is_whitespace() || c == '>' || c == '"' || c == '\'')
                            .next()
                            .map(|s| s.to_string())
                    }
                } else {
                    None
                };
                if let Some(v) = val {
                    let v = v.trim().to_string();
                    if (v.starts_with("http://") || v.starts_with("https://"))
                        && !is_unverifiable_url(&v)
                    {
                        return Some(v);
                    }
                }
            }
            from = abs + 1;
        }
    }
    None
}

/// T13 — an auth / anti-bot wall page that an operator cannot open to
/// verify a post (login, signup, human-check, captcha, challenge…).  A
/// "success" result row must never carry one of these URLs — if that is all
/// we captured, the job was login-only and must be reported as a failure.
pub fn is_unverifiable_url(u: &str) -> bool {
    let lower = u.trim().to_ascii_lowercase();
    let path = lower
        .split(['?', '#'])
        .next()
        .unwrap_or(lower.as_str());
    const WALLS: &[&str] = &[
        "login",
        "log-in",
        "signin",
        "sign-in",
        "sign_up",
        "signup",
        "sign-up",
        "register",
        "registration",
        "logout",
        "human-check",
        "captcha",
        "recaptcha",
        "challenge",
        "confirm-email",
        "verify-email",
    ];
    WALLS.iter().any(|w| path.contains(w))
}

/// T13b — normalize a URL for "is this the same page" comparisons: strip
/// scheme, leading `www.`, trailing slash, query and fragment, lowercase.
/// Query/fragment are stripped so a form page and the same page re-shown with
/// a `?return=…` variant still compare equal (that is the point of the
/// form-echo demotion — the *page* is what matters, not the query).
fn url_same_page(a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        let mut t = s.trim().to_ascii_lowercase();
        loop {
            if let Some(r) = t.strip_prefix("https://") {
                t = r.to_string();
                continue;
            }
            if let Some(r) = t.strip_prefix("http://") {
                t = r.to_string();
                continue;
            }
            if let Some(r) = t.strip_prefix("www.") {
                t = r.to_string();
                continue;
            }
            break;
        }
        // Drop query / fragment — only the path identifies the page.
        if let Some(i) = t.find(['?', '#']) {
            t.truncate(i);
        }
        while t.ends_with('/') {
            t.pop();
        }
        t
    };
    let (na, nb) = (norm(a), norm(b));
    !na.is_empty() && na == nb
}

/// T13c — is `u` one of this plan's *declared created-link patterns*
/// (`find url` / `verify url`)?  When it is, a match is exactly the evidence
/// of creation the INI author promised — not a form echo — even though, for
/// relative `form url`s, [`StepNav::navigation_target`] promotes the
/// absolute `find url` to the POST target and the T13b form-echo set would
/// otherwise swallow the very link it declares.  (GSA parity: an author who
/// declared `find url=X` is saying "the created link lives like X" — a
/// response naming X confirms the post.)
fn url_matches_created_pattern(u: &str, plan: &SubmissionPlan) -> bool {
    let nu: String = {
        let mut t = u.trim().to_ascii_lowercase();
        loop {
            if let Some(r) = t.strip_prefix("https://") { t = r.to_string(); continue; }
            if let Some(r) = t.strip_prefix("http://") { t = r.to_string(); continue; }
            if let Some(r) = t.strip_prefix("www.") { t = r.to_string(); continue; }
            break;
        }
        if let Some(i) = t.find(['?', '#']) { t.truncate(i); }
        while t.ends_with('/') { t.pop(); }
        t
    };
    if nu.is_empty() {
        return false;
    }
    plan.steps.iter().any(|s| {
        let mut any = false;
        let patterns: Vec<&str> = std::iter::once(s.nav.verify_url.as_deref().unwrap_or(""))
            .chain(s.nav.find_url.iter().map(|x| x.as_str()))
            .collect();
        for p in patterns {
            let mut cc = p.trim().trim_start_matches('*').trim_start_matches('.').trim().to_ascii_lowercase();
            loop {
                if let Some(r) = cc.strip_prefix("https://") { cc = r.to_string(); continue; }
                if let Some(r) = cc.strip_prefix("http://") { cc = r.to_string(); continue; }
                if let Some(r) = cc.strip_prefix("www.") { cc = r.to_string(); continue; }
                break;
            }
            if let Some(i) = cc.find(['?', '#']) { cc.truncate(i); }
            while cc.ends_with('/') { cc.pop(); }
            if cc.is_empty() {
                continue;
            }
            if cc.contains('*') || cc.contains('[') {
                // Scribd-style glob: the static leading segment is the match
                // anchor (`scribd.example/doc/…`).
                let static_prefix = cc
                    .split(|ch: char| ch == '*' || ch == '[')
                    .next()
                    .unwrap_or("");
                any |= !static_prefix.is_empty() && nu.starts_with(static_prefix);
            } else {
                any |= nu == cc || nu.starts_with(&format!("{cc}/")) || cc.starts_with(&format!("{nu}/"));
            }
        }
        any
    })
}

/// T13b — `true` when `u` is a bare site root (`host/` or `host`): no path
/// beyond the domain.  A short link (`rlu.ru/5i1b6`) or any real page
/// (`/doc/123`, `/profile/abc`) is not a bare root.
fn is_bare_root(u: &str) -> bool {
    let rest = match u.strip_prefix("https://").or_else(|| u.strip_prefix("http://")) {
        Some(r) if !r.is_empty() => r,
        _ => return false,
    };
    // First '/' (if any) splits host from path.
    let path = match rest.find('/') {
        Some(i) => &rest[i + 1..],
        None => "",
    };
    // Strip query/fragment for the "root" test.
    let path = path.split(['?', '#']).next().unwrap_or("");
    path.is_empty()
}

/// T13b — did the INI author declare where a created resource's URL lives
/// (GSA `find url` / `verify url` / a named extraction)?  When they did,
/// the engine's own form/nav pages are *known non-creation* locations: a
/// real created link would have been extracted first (those patterns
/// outrank the nav hints in [extract_result_url]), so landing back on one
/// of the engine's own pages means the creation was never confirmed.
/// Engines with no such contract (a plain blog-comment form) may still
/// legitimately report the post page they posted to.
fn plan_has_url_contract(plan: &SubmissionPlan) -> bool {
    plan.steps.iter().any(|s| {
        !s.nav.find_url.is_empty()
            || s.nav.verify_url.is_some()
            || s.nav.verify_extract.is_some()
            || !s.nav.alternative_url.is_empty()
    })
}

/// T13b — every form/nav target the plan posts to (one per step), resolved
/// against the base URL exactly like the executor computes each step's
/// `target`.  The union of these is the set of the engine's *own* pages —
/// where forms live, not where new resources get their permalinks.
fn plan_form_targets(plan: &SubmissionPlan) -> Vec<String> {
    plan
        .steps
        .iter()
        .map(|s| {
            if s.just_download {
                resolve_against_base(&step_target_from_modify(s, &plan.target_url), &plan.target_url)
            } else {
                resolve_against_base(
                    &s.nav
                        .navigation_target()
                        .unwrap_or_else(|| plan.target_url.clone()),
                    &plan.target_url,
                )
            }
        })
        .collect()
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

/// T20 — first usable `<a href>` in a blog-search result page: resolves
/// relative links, skips `skip content on` phrases (in URL or label),
/// other hosts (`skip ext links on`), and self-returns unless
/// `allow return=1`.
fn search_candidates(html: &str, base: &str, sr: &StepSearch, target_site: &str) -> Option<String> {
    let base_host = host_of(base);
    let site_host = host_of(target_site);
    let mut from = 0usize;
    while true {
        let pos = match html[from..].find("href=") {
            Some(p) => p,
            None => break,
        };
        let p = from + pos;
        let after = &html[p + 5..];
        // capture the quoted (or bare) URL
        let (v, next) = if let Some(c) = after.chars().next() {
            if c == '"' || c == '\'' {
                match after[1..].find(c) {
                    Some(e) => (after[1..1 + e].trim().to_string(), p + 6 + e + 1),
                    None => break,
                }
            } else {
                match after.find(|ch: char| ch.is_whitespace() || ch == '>') {
                    Some(e) => (after[..e].trim().to_string(), p + 6 + e),
                    None => break,
                }
            }
        } else {
            break;
        };
        from = next;
        if v.is_empty() || v.starts_with("mailto:") || v.starts_with("javascript:") {
            continue;
        }
        let abs = if v.starts_with("http://") || v.starts_with("https://") {
            v.clone()
        } else if v.starts_with("//") {
            format!("https:{}", v)
        } else {
            resolve_against_base(&v, base)
        };
        // anchor label: text between the closing '>' of this tag and the next '<'.
        let gt = html[p + 6..].find('>').map(|g| p + 6 + g).unwrap_or(html.len());
        let le = html[gt + 1..].find('<').map(|e| gt + 1 + e).unwrap_or(html.len());
        let label = html[gt + 1..le].to_ascii_lowercase();
        // `skip content on` — disqualify by URL or label.
        if sr
            .skip_content
            .iter()
            .any(|ph| abs.to_ascii_lowercase().contains(&ph.to_ascii_lowercase())
                || label.contains(&ph.to_ascii_lowercase()))
        {
            continue;
        }
        // self-return: results pointing at the submitted site's home are the
        // classic "bounced back" wall — allowed only with `allow return=1`.
        if !sr.allow_return {
            if let (Some(lh), Some(sh)) = (host_of(&abs), site_host.clone()) {
                if lh == sh {
                    let path = abs
                        .split_once("//")
                        .map(|(_, r)| r)
                        .unwrap_or("")
                        .split('/')
                        .nth(1)
                        .unwrap_or("")
                        .trim_end_matches('/');
                    if path.is_empty() {
                        continue;
                    }
                }
            }
        }
        // external hosts
        if sr.skip_ext {
            if let (Some(lh), Some(bh)) = (host_of(&abs), base_host.clone()) {
                if lh != bh {
                    continue;
                }
            }
        }
        return Some(abs);
    }
    None // no candidate left
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
        content_success: 0,
        failed: 0,
        retried: 0,
        captcha_failed: 0,
        submitted_url: None,
        submitted_url_source: None,
        errors: Vec::new(),
        steps: Vec::new(),
    };
    let mut continue_after_wait: Option<DriverResponse> = None;
    for step in plan.steps.iter() {
        // T19 — operator pause gate: while the run is paused, in-flight
        // plans wait here between steps; the leader meanwhile stops
        // dispatching new engines, so the run goes fully quiet without
        // losing any already-created link.
        pause_gate(plan).await;
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
        let mut target = if step.just_download {
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
        // T20 — GSA blog-search flow: `use blog search=<url>` drives a
        // search first; this step then acts on the first usable result
        // (skipping `skip content on`/external/self-returns unless
        // `allow return=1`).
        if step.phase == StepPhase::Submit {
            if let Some(sr) = &step.search {
                let base = plan.target_url.trim().to_string();
                let search_url = resolve_against_base(&sr.url, &base);
                let keyword = plan
                    .anchor
                    .clone()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_default();
                let term = if sr.extract_keywords && !keyword.is_empty() {
                    keyword
                } else if sr.term.is_empty() {
                    keyword.clone()
                } else if sr.add_keyword && !keyword.is_empty() {
                    format!("{} {}", sr.term, keyword)
                } else {
                    sr.term.clone()
                };
                if !term.is_empty() {
                    let mut found: Option<String> = None;
                    for fname in ["q", "search", "s", "keyword", "query", "txtsearch"] {
                        let mut sf = std::collections::HashMap::new();
                        sf.insert(fname.to_string(), term.clone());
                        let r = driver
                            .submit(search_url.clone(), sf, std::time::Duration::from_secs(30))
                            .await;
                        if !r.ok {
                            break;
                        }
                        found = search_candidates(&r.html, &base, sr, &plan.target_url);
                        break;
                    }
                    match found {
                        Some(link) => {
                            target = resolve_against_base(&link, &base);
                        }
                        None => {
                            report.errors.push(format!("{}: no usable result from blog search {}", step.name, search_url));
                            report.failed += 1;
                            report.steps.push(StepOutcomeRef { outcome: StepOutcome::Failed, matched_phrase: None });
                            continue;
                        }
                    }
                }
            }
            // T20 — `seconds to wait before submission[ condition=TEXT]`:
            // load the step page, wait only when the condition (if any) is
            // present, then proceed.
            if let Some(secs) = step.wait_before_secs.filter(|s| *s > 0) {
                let pre = driver.navigate(target.clone()).await;
                let cond_ok = match &step.wait_before_cond {
                    Some(c) if !c.is_empty() => {
                        pre.ok && pre.html.to_ascii_lowercase().contains(&c.to_ascii_lowercase())
                    }
                    _ => pre.ok,
                };
                if cond_ok && secs > 0 {
                    tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
                }
                // A no-fields step consumes the pre-loaded page; submit
                // steps re-POST after the wait (GSA order: GET → wait → POST).
                if fields.is_empty() {
                    let mut res: DriverResponse = pre;
                    if !res.ok {
                        report.errors.push(format!("{}: {}", step.name, res.error.unwrap_or_default()));
                        report.failed += 1;
                        report.steps.push(StepOutcomeRef { outcome: StepOutcome::Failed, matched_phrase: None });
                        continue;
                    }
                    continue_after_wait = Some(res);
                }
            }
        }
        let mut res: DriverResponse = match continue_after_wait.take() {
            Some(r) => r,
            None => if step.just_download || fields.is_empty() {
                driver.navigate(target.clone()).await
            } else {
                driver.submit(target.clone(), fields.clone(), std::time::Duration::from_secs(30)).await
            },
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
                        record_created_url(&mut report, format!("{}: {}", step.name, u), &u);
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
                record_created_url(&mut report, format!("{}: {} (created)", step.name, u), &u);
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
        // T14' — was a *real* link found on the page (vs the post-target
        // fallback below)?  The form-echo demotion must only fire on real
        // evidence: when the page merely said "success" and no link exists,
        // the post target is the last-resort permalink GSA itself records —
        // demoting it would kill every engine that returns a bare
        // confirmation (the `PHRASE OK` shape).
        let found_link = nav
            .as_deref()
            .map(|n| !n.trim().is_empty() && n.trim() != target.trim())
            .unwrap_or(false)
            || final_url_created(&res.final_url, &target).is_some();
        let chosen = nav
            .clone()
            .filter(|n| !n.trim().is_empty() && n.trim() != target.trim())
            .or_else(|| final_url_created(&res.final_url, &target))
            .or(nav)
            // T12 — comment engines POST the comment to the post itself
            // (same-page submit, no dedicated permalink for the comment).
            // The target IS the verifiable page — use it as last resort so
            // the operator can open the post and see the comment there.
            .or_else(|| {
                if !step.just_download && !fields.is_empty() {
                    Some(target.clone())
                } else {
                    None
                }
            });
        if let Some(ref u) = chosen {
            // T12 — best-wins: a later step's deeper permalink correctly
            // replaces an earlier step's login/form page.
            record_created_url(&mut report, format!("{}: {}", step.name, u), u);
        }
        match outcome.outcome {
            StepOutcome::Success => {
                report.successful += 1;
                // T13 — only Submit/Extra-phase successes prove a backlink
                // was created; Login/Register successes are infrastructure.
                // T13b — and the step's own evidence must be real:
                //   * a wall page (login / human-check) is not a created link;
                //   * the form we posted to, echoing back what we sent, is
                //     not a created link (Scribd-style upload forms);
                //   * a bare foreign site root is not a created link (yy7-style
                //     `alt="success"` false positives landing on / ).
                if step.phase == StepPhase::Submit || step.phase == StepPhase::Extra {
                    let u = chosen.as_deref().unwrap_or("");
                    let wall = !u.is_empty() && is_unverifiable_url(u);
                    // The link we pinned down is one of the engine's OWN
                    // form/nav pages, i.e. the page(s) the plan POSTs to —
                    // not a created resource.  Two shapes:
                    //   (a) this step is an *upload* step (carries a `file=`
                    //       field) and the link is the very form it posted
                    //       to — the conservative same-page rule; and
                    //   (b) the INI author declared where a created URL lives
                    //       (`find url` / `verify url`, a real permalink), so
                    //       we know the engine's form pages are NOT created
                    //       resources — yet the best link we can name is one
                    //       of those form pages.  Scribd's real case: every
                    //       step returns the same static shell whose self-link
                    //       is `upload/supload`; that link is attributed to a
                    //       step whose own target is `login/upload/update_docs`,
                    //       so a per-step same-page test misses it, but it IS
                    //       one of the plan's own form pages and the plan
                    //       declares a `find url`/`verify url` contract.
                    // Same-page comment engines (no url contract, no file)
                    // stay legitimate — the post IS the verifiable page.
                    let u_is_own_form = !u.is_empty() && url_same_page(u, &target);
                    let u_is_any_plan_form = {
                        if u.is_empty() {
                            false
                        } else {
                            plan_form_targets(plan).iter().any(|t| url_same_page(u, t))
                        }
                    };
                    // T13c — an echo of an author-declared created-link pattern
                    // (`find url` / `verify url`) is the confirmation itself,
                    // never a form echo (the T13b false positive where a
                    // relative `form url` + absolute `find url=X` put X both
                    // in the POST target and the "own form pages" set).
                    // Two shapes, one rule each:
                    //  (a) an *upload* step (carries `file=`) that landed back
                    //      on its own form — demote even when no link resolved
                    //      (a bare "success" on the upload form proves nothing);
                    //  (b) a REAL link was found and it is one of the engine's
                    //      own form pages — unless it matches the author's
                    //      declared created-link pattern (`find url` /
                    //      `verify url`): then it IS the confirmation.
                    let form_echo = u_is_own_form
                        && step.fields.keys().any(|k| k.to_ascii_lowercase() == "file")
                        || found_link
                            && plan_has_url_contract(plan)
                            && u_is_any_plan_form
                            && !url_matches_created_pattern(u, plan);
                    // A bare site root (`host/`) can never be a created
                    // permalink — the yy7 `alt="success"` false positive.
                    let bare_root = !u.is_empty() && is_bare_root(u);
                    // T20 — `page must have` (post-submission content the site
                    // must echo) + `url must have` (created-link phrase).  An
                    // engine that declares them but gets neither back did not
                    // really post — demote like the other evidence gaps.
                    let html_lc = res.html.to_ascii_lowercase();
                    let page_must_fail = step
                        .page_must_haves
                        .iter()
                        .any(|ph| !html_lc.contains(&ph.to_ascii_lowercase()));
                    let u_lc = u.to_ascii_lowercase();
                    let url_must_fail = step
                        .url_must_haves
                        .iter()
                        .any(|ph| !u_lc.contains(&ph.to_ascii_lowercase()));
                    if wall {
                        report.errors.push(format!(
                            "{}: matched success, but the site returned a wall page ({u}) — post not confirmable",
                            step.name
                        ));
                    } else if form_echo {
                        report.errors.push(format!(
                            "{}: the page only echoed one of the engine's own form pages ({u}) — the created link was not confirmed",
                            step.name
                        ));
                    } else if page_must_fail {
                        let missing = step
                            .page_must_haves
                            .iter()
                            .filter(|ph| !html_lc.contains(&ph.to_ascii_lowercase()))
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(", ");
                        report.errors.push(format!(
                            "{}: matched success, but the page was missing required content ({missing})",
                            step.name
                        ));
                    } else if url_must_fail {
                        let missing = step
                            .url_must_haves
                            .iter()
                            .filter(|ph| !u_lc.contains(&ph.to_ascii_lowercase()))
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(", ");
                        report.errors.push(format!(
                            "{}: matched success, but the created link did not contain the required phrase ({missing}) at {u}",
                            step.name
                        ));
                    } else if bare_root {
                        report.errors.push(format!(
                            "{}: matched success, but only a bare site root came back ({u}) — the created link was not pinned down",
                            step.name
                        ));
                    } else {
                        report.content_success += 1;
                    }
                }
            }
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

/// T12 — rank candidate "created" URLs by specificity: deep permalinks
/// score highest, auth/form pages (login, account, signup) lowest.  The
/// winner across all steps is decided by score, NOT by step order — the
/// first successful step is almost always just *login*, and its page must
/// not shadow the actual created permalink that a later step reveals.
fn url_score(u: &str) -> i32 {
    let p = u.trim();
    let path = p
        .strip_prefix("https://")
        .or_else(|| p.strip_prefix("http://"))
        .unwrap_or(p)
        .split('/')
        .nth(1)
        .unwrap_or("");
    let mut s = 0;
    for seg in path.split('/').filter(|seg| !seg.is_empty()) {
        s += 1;
        let ls = seg.to_ascii_lowercase();
        if matches!(
            ls.as_str(),
            "login"
                | "log-in"
                | "sign-in"
                | "signin"
                | "signup"
                | "sign-up"
                | "signout"
                | "logout"
                | "register"
                | "registration"
                | "account"
                | "accounts"
                | "user"
                | "users"
                | "home"
                | "index"
                | "index.html"
                | "main"
                | "start"
                | "dashboard"
        ) {
            s -= 8;
        }
        // Long digit runs usually denote a created item's id / permalink
        // (/doc/123456789/, /p/88/, /u/42/, /entry/918372/).
        if seg.chars().filter(|c| c.is_ascii_digit()).count() >= 4 {
            s += 6;
        }
    }
    s
}

/// T12 — record `u` as the run's created link only when it beats the current
/// winner (first set always wins when the pool is empty).  The *highest*
/// specificity wins — a deep permalink recorded by a later step correctly
/// replaces a login/form page recorded by an earlier one.
fn record_created_url(report: &mut ExecutionReport, source: String, u: &str) -> bool {
    let c = u.trim();
    if c.is_empty() {
        return false;
    }
    let wins = match report.submitted_url.as_deref() {
        None => true,
        Some(cur) => url_score(c) > url_score(cur),
    };
    if wins {
        report.submitted_url = Some(c.to_string());
        report.submitted_url_source = Some(source);
    }
    wins
}

/// T19 — operator pause gate. While the shared pause flag is set, an
/// in-flight plan waits here (between steps) until the operator resumes —
/// or the run is cancelled, in which case the job's own timeout/cancel
/// accounting ends it. Polling keeps the gate deadlock-free with the
/// leader (no async runtime dependency on the flag setter).
async fn pause_gate(plan: &SubmissionPlan) {
    if let Some(p) = plan.pause.as_deref() {
        while p.load(std::sync::atomic::Ordering::Acquire) {
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        }
    }
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
            site_url: None,
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
            site_url: None,
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
            anchor_text: None,
            upcase: false,
            capitalize_words: false,
            max_len: None,
            page_must_haves: Vec::new(),
            url_must_haves: Vec::new(),
            wait_before_secs: None,
            wait_before_cond: None,
            search: None,
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
            site_url: None,
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
            site_url: None,
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
            site_url: None,
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
            verify_url: None,
            verify_extract: None,
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
            verify_url: None,
            verify_extract: None,
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
            verify_url: None,
            verify_extract: None,
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
            site_url: None,
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

    // ── T5 — site_url (discovery engines run against the searched site) ──

    #[test]
    fn site_url_becomes_target_for_discovery_engine() {
        // Discovery-style engine: no `url=` / `fixed url=` of its own, so
        // base_url() is empty — GSA's guestbook / URL-engine shape.  The
        // searched site must become the plan's base page.
        let ini = "title=T\r\n\n[STEP1]\r\nlink type=Comment-URL\r\njust download=1\r\nmodify url=/sign?c=%blog_comment%\r\nsubmit success=OK\r\n";
        let eng = parse_engine_str(ini, "disco").unwrap();
        assert!(eng.base_url().is_empty(), "test premise: engine has no base url");
        let input = SubmissionInput {
            url: "https://backlink.example/".to_string(),
            keyword: "kw".to_string(),
            seed: Some(1),
            site_url: Some("https://found-site.example/".to_string()),
            ..Default::default()
        };
        let plan = resolve_plan(&eng, &input);
        assert_eq!(plan.target_url, "https://found-site.example/");
    }

    #[test]
    fn site_url_ignored_when_engine_has_own_base() {
        // Fixed-URL engines (agentsnipe / rentry shape) keep their own base.
        let ini = "title=T\r\n\r\n[setup]\r\nurl=https://fixed.example/\r\n\r\n[STEP1]\r\nlink type=Comment-URL\r\njust download=1\r\nmodify url=/sign\r\nsubmit success=OK\r\n";
        let eng = parse_engine_str(ini, "fixed").unwrap();
        assert_eq!(eng.base_url(), "https://fixed.example/");
        let input = SubmissionInput {
            url: "https://backlink.example/".to_string(),
            keyword: "kw".to_string(),
            seed: Some(1),
            site_url: Some("https://found-site.example/".to_string()),
            ..Default::default()
        };
        let plan = resolve_plan(&eng, &input);
        assert_eq!(plan.target_url, "https://fixed.example/");
    }

    #[test]
    fn empty_site_url_behaves_like_absent() {
        let ini = "title=T\r\n\r\n[STEP1]\r\njust download=1\r\nmodify url=/sign\r\nsubmit success=OK\r\n";
        let eng = parse_engine_str(ini, "disco2").unwrap();
        let input = SubmissionInput {
            url: "https://backlink.example/".to_string(),
            seed: Some(1),
            site_url: Some(String::new()),
            ..Default::default()
        };
        let plan = resolve_plan(&eng, &input);
        assert!(plan.target_url.is_empty(), "blank site_url must not leak in");
    }

    // ── T12: created-link accuracy ─────────────────────────────────━━━━━━━

    /// A driver that serves one canned page per call — enough to simulate
    /// "login page first, then the upload-success page reveals the new
    /// document permalink".
    struct TwoPageDriver {
        pages: Vec<String>,
        calls: std::sync::Mutex<usize>,
    }
    impl TwoPageDriver {
        fn new(pages: Vec<String>) -> Self {
            Self { pages, calls: std::sync::Mutex::new(0) }
        }
        fn next(&self) -> DriverResponse {
            let mut c = self.calls.lock().unwrap();
            let html = self.pages.get(*c).cloned().unwrap_or_default();
            *c += 1;
            DriverResponse {
                ok: true,
                final_url: "mock".into(),
                html,
                error: None,
            }
        }
    }
    #[async_trait::async_trait]
    impl Driver for TwoPageDriver {
        async fn navigate(&self, _u: String) -> DriverResponse {
            self.next()
        }
        async fn submit(
            &self,
            _u: String,
            _f: HashMap<String, String>,
            _t: std::time::Duration,
        ) -> DriverResponse {
            self.next()
        }
    }

    /// The exact shape of the reported bug: a Scribd-like engine whose
    /// *first* successful step is the login page (its URL gets captured),
    /// then the upload step that reveals the created document's permalink
    /// via the `[verify_url] find url` glob.  The recorded link MUST be the
    /// document URL — the only one the operator can open to confirm the
    /// upload exists.
    #[tokio::test]
    async fn t12_verify_url_permalink_beats_login_echo() {
        let raw = r#"
[engine]
name=ScribdLike
engine type=Document Sharing
fixed url=http://www.scribdlike.com/login

[verify_url]
find link=*.pdf
find url=http://www.scribdlike.com/doc/[0-9]*[0-9]/*

[LOGIN_STEP1]
modify url=http://www.scribdlike.com/login
submit success=>Log Out<
form url=http://www.scribdlike.com/login
login_or_email=me
login_password=x

[EXTRA_STEP1]
modify url=http://www.scribdlike.com/upload/supload
submit success=>My Doc</a>
verify url=%verify_url%
form name=*Upload
form url=*/upload/supload
file=doc.pdf
"#;
        let eng = parse_engine_str(raw, "ScribdLike").unwrap();
        let plan = resolve_plan(&eng, &test_input());
        // Sanity: the %verify_url% reference resolved to the glob pattern.
        assert_eq!(
            plan.steps[1].nav.verify_url.as_deref(),
            Some("http://www.scribdlike.com/doc/[0-9]*[0-9]/*")
        );

        let login_page = r#"<html><body>Logged in as
            <a href="http://www.scribdlike.com/login">me</a>
            <span>>Log Out<</span></body></html>"#.to_string();
        let upload_page = r#"<html><body>Thank you.
            <a href="http://www.scribdlike.com/doc/123456789/my-case-study">My Doc</a>
            </body></html>"#.to_string();
        let driver = TwoPageDriver::new(vec![login_page, upload_page]);

        let report = execute_plan_with_solver(&plan, &driver, None).await;
        assert_eq!(report.successful, 2, "both steps should succeed: {report:?}");
        assert_eq!(
            report.submitted_url.as_deref(),
            Some("http://www.scribdlike.com/doc/123456789/my-case-study"),
            "the created permalink must win over the login page: {report:?}"
        );
        assert!(
            report
                .submitted_url_source
                .as_deref()
                .map(|s| s.contains("EXTRA_STEP1"))
                .unwrap_or(false)
        );
    }

    /// Comment engines POST the comment to the post page itself — the
    /// created comment has no permalink of its own.  Spec: report the *post*
    /// URL so the operator can open it and verify the comment is there.
    #[tokio::test]
    async fn t12_comment_engine_falls_back_to_post_url() {
        let raw = r#"
[engine]
name=Blog
engine type=Blog Comments
fixed url=http://www.blogroot.example/

[step 1]
comment=%anchor_text%
submit success=your comment was added
form url=http://post.example/entry/10086
"#;
        let eng = parse_engine_str(raw, "Blog").unwrap();
        let plan = resolve_plan(&eng, &test_input());
        // No permalink on the response page — only the success phrase.
        let driver = MockDriver::with_html(
            "<html><body><div>your comment was added</div></body></html>",
        );

        let report = execute_plan_with_solver(&plan, &driver, None).await;
        assert_eq!(report.successful, 1, "{report:?}");
        assert_eq!(
            report.submitted_url.as_deref(),
            Some("http://post.example/entry/10086"),
            "comment engines must report the post URL: {report:?}"
        );
    }

    /// Scoring sanity: deep permalinks outrank form/auth pages; short links
    /// still beat bare domains.
    #[test]
    fn t12_url_score_ranks_permalinks_above_forms() {
        assert!(url_score("http://www.scribd.com/doc/123456789/my-doc")
            > url_score("http://www.scribd.com/login"));
        assert!(url_score("http://www.scribd.com/upload/supload")
            > url_score("http://www.scribd.com/login"));
        assert!(url_score("http://rlu.ru/5i1b6") > url_score("http://rlu.ru/"));
        assert!(url_score("https://dir.example/profile/abc123")
            > url_score("https://dir.example/"));
    }

    // ── T13: professional-backlink semantics ─────────────────────────────

    /// Login-only success is infrastructure, not a backlink.  A job whose
    /// only successful step is the login form must report
    /// `content_success == 0` so the runner counts it as a failure.
    #[tokio::test]
    async fn t13_login_only_success_is_not_content_success() {
        let raw = r#"
[engine]
name=CommentBlog
engine type=Comment
fixed url=http://post.example/entry/10086

[LOGIN_STEP1]
form url=http://post.example/login
submit success=>Logged in<
login_or_email=me
login_password=x

[COMMENT_STEP1]
form url=http://post.example/entry/10086
submit success=your comment was added
comment body=hello
comment name=A
comment email=a@b.co
"#;
        let eng = parse_engine_str(raw, "CommentBlog").unwrap();
        let plan = resolve_plan(&eng, &test_input());
        // Both pages: login confirms, comment step does NOT match.
        let driver = TwoPageDriver::new(vec![
            "<html><span>>Logged in<</span></html>".to_string(),
            "<html><body>page</body></html>".to_string(),
        ]);
        let report = execute_plan_with_solver(&plan, &driver, None).await;
        assert_eq!(report.successful, 1, "login step matched: {report:?}");
        assert_eq!(report.content_success, 0, "login success is not content success: {report:?}");

        // And when the comment step DOES confirm, content_success counts.
        let driver2 = TwoPageDriver::new(vec![
            "<html><span>>Logged in<</span></html>".to_string(),
            "<html><div>your comment was added</div></html>".to_string(),
        ]);
        let report2 = execute_plan_with_solver(&plan, &driver2, None).await;
        assert_eq!(report2.successful, 2, "{report2:?}");
        assert_eq!(report2.content_success, 1, "the created comment counts: {report2:?}");
    }

    /// GSA `type=extract` verify section (YY7 shape): the created short link
    /// sits between `front1`/`front2` and `back` — capture it and report it,
    /// not the bare engine domain.
    #[tokio::test]
    async fn t13_verify_extract_section_yields_created_link() {
        let raw = r#"
[engine]
name=Yy7Sh
engine type=URL Shortener
fixed url=http://yy7.example/

[verifyurl]
front1=<div class="resultLink">
front2=href="
back="
type=extract

[ARTICLE_STEP1]
form url=http://yy7.example/
verify url=%verifyurl%
submit success=your shortened url
submit failed=invalid url
url=%url%
"#;
        let eng = parse_engine_str(raw, "Yy7Sh").unwrap();
        let plan = resolve_plan(&eng, &test_input());
        assert!(plan.steps[0].nav.verify_extract.is_some(),
            "%verifyurl% must resolve to the extract tuple: {:?}", plan.steps[0].nav.verify_extract);
        let driver = MockDriver::with_html(
            "<html><div class=\"resultLink\"><a href=\"https://yy7.example/Ab3xY9\" target=\"_blank\">https://yy7.example/Ab3xY9</a></div><div>your shortened url</div></html>",
        );
        let report = execute_plan_with_solver(&plan, &driver, None).await;
        assert_eq!(report.successful, 1, "{report:?}");
        assert_eq!(
            report.submitted_url.as_deref(),
            Some("https://yy7.example/Ab3xY9"),
            "the created short link must be reported: {report:?}"
        );
    }

    /// Old-GSA `front=` alias (Scribd `[hset]` shape) also works.
    #[test]
    fn t13_extract_front_back_old_syntax() {
        let html = r#"<input name="x" input.value=["http://x.example/created/42"] type="text">"#;
        assert_eq!(
            extract_front_back_url(html, "input.value=[\"", "", "]").as_deref(),
            Some("http://x.example/created/42")
        );
    }

    /// Creation-echo (Scribd upload shape): the response anchors the
    /// submitted title in an `<a href>`; with NO `find url`/`verify url`
    /// anywhere in the INI, that href is the created permalink.
    #[tokio::test]
    async fn t13_creation_echo_title_href_is_reported() {
        let raw = r#"
[engine]
name=ScribdEcho
engine type=Document Sharing
fixed url=http://www.scribdlike.com/login

[EXTRA_STEP1]
form url=*/upload/supload
submit success=>My Case Study</a>
file=doc.pdf
"#;
        let eng = parse_engine_str(raw, "ScribdEcho").unwrap();
        let plan = resolve_plan(&eng, &test_input());
        let driver = MockDriver::with_html(
            "<html><body>Thank you for uploading.\n<a href=\"https://www.scribdlike.com/doc/987654321/my-case-study\">My Case Study</a></body></html>",
        );
        let report = execute_plan_with_solver(&plan, &driver, None).await;
        assert_eq!(report.successful, 1, "{report:?}");
        assert_eq!(
            report.submitted_url.as_deref(),
            Some("https://www.scribdlike.com/doc/987654321/my-case-study"),
            "the created document page must be reported: {report:?}"
        );
    }

    #[test]
    fn t13_unverifiable_url_detects_walls() {
        assert!(is_unverifiable_url("https://rentry.co/login"));
        assert!(is_unverifiable_url("https://www.plurk.com/Users/showLogin"));
        assert!(is_unverifiable_url("http://www.yuarel.com/human-check.php"));
        assert!(is_unverifiable_url("https://x.co/signup?next=/a"));
        assert!(!is_unverifiable_url("http://www.scribd.com/doc/1234/abc"));
        assert!(!is_unverifiable_url("https://yy7.com/"));
        assert!(!is_unverifiable_url("https://rentry.co/article/slug42"));
    }

    // ── T13b: the reported link must be the *created* link, not a wall ─

    /// `is_bare_root` / `url_same_page` helpers.
    #[test]
    fn t13b_helpers_bare_root_and_same_page() {
        assert!(is_bare_root("https://yy7.game/"));
        assert!(is_bare_root("http://yy7.game"));
        assert!(is_bare_root("https://yy7.game/?x=1"));
        assert!(!is_bare_root("https://yy7.game/Ab3xY9"));
        assert!(!is_bare_root("https://rentry.co/article/slug42"));
        assert!(url_same_page("http://a.com/x/", "https://www.a.com/x?p=1"));
        assert!(!url_same_page("https://a.com/x", "https://b.com/x"));
        assert!(!url_same_page("", "https://a.com/x"));
    }

    /// A Submit-step success whose only evidence is a **wall page** (login /
    /// human-check) must NOT count as content — the operator can't verify a
    /// backlink that lands on a login wall.  This is the yuarel/plurk shape:
    /// the `submit success` marker matched, but the recorded URL is a wall.
    #[tokio::test]
    async fn t13b_wall_success_is_not_content_success() {
        let raw = r#"
[engine]
name=WallEngine
engine type=Article
fixed url=http://wall.example/

[ARTICLE_STEP1]
form url=http://wall.example/submit
submit success=your url has been created
url=%url%
"#;
        let eng = parse_engine_str(raw, "WallEngine").unwrap();
        let plan = resolve_plan(&eng, &test_input());
        // The response confirms success but the created URL we can pin down
        // is a login wall (the site redirected to one).
        let driver = MockDriver::with_html(
            "<html><body>your url has been created \
             <a href=\"https://wall.example/login\">sign in</a></body></html>",
        );
        let report = execute_plan_with_solver(&plan, &driver, None).await;
        assert_eq!(report.successful, 1, "the marker matched: {report:?}");
        let u = report.submitted_url.as_deref().unwrap_or("");
        assert!(
            is_unverifiable_url(u),
            "recorded URL should be the wall: {u:?} — {report:?}"
        );
        assert_eq!(
            report.content_success, 0,
            "a wall-page success is not a verifiable backlink: {report:?}"
        );
    }

    /// A Submit-step success that merely **echoed the form we posted to**
    /// (marker carries a spintax token, recorded URL == the form) is not a
    /// created link — the scribd `/upload/supload` shape.
    #[tokio::test]
    async fn t13b_form_echo_success_is_not_content_success() {
        let raw = r#"
[engine]
name=EchoEngine
engine type=Document Sharing
fixed url=http://echo.example/

[EXTRA_STEP1]
form url=*/upload/supload
submit success=>My Case Study</a>
file=doc.pdf
"#;
        let eng = parse_engine_str(raw, "EchoEngine").unwrap();
        let plan = resolve_plan(&eng, &test_input());
        // The page echoes our title but links back to the SAME form, not a
        // created permalink — the marker text resolves to a %article_title%.
        let driver = MockDriver::with_html(
            "<html><body>Uploading… <a href=\"https://echo.example/upload/supload\">My Case Study</a></body></html>",
        );
        let report = execute_plan_with_solver(&plan, &driver, None).await;
        assert_eq!(report.successful, 1, "marker matched: {report:?}");
        assert_eq!(
            report.submitted_url.as_deref(),
            Some("https://echo.example/upload/supload"),
            "recorded the echo URL: {report:?}"
        );
        assert_eq!(
            report.content_success, 0,
            "a form-echo is not a created link: {report:?}"
        );
    }

    /// The live-Scribd shape (job of 2026-10-23): the engine serves the SAME
    /// static shell for every step, and that shell's self-link (`/upload/
    /// supload`) is what gets recorded — but it is attributed to a LATER step
    /// whose own target is a different form (`/login/upload/update_docs`), so
    /// a per-step same-page test misses it.  The plan declares a `verify url`
    /// contract, so any of the engine's own form pages is a known
    /// non-creation location → not a content success.
    #[tokio::test]
    async fn t13b_cross_step_form_echo_is_not_content_success() {
        let raw = r#"\
[engine]
name=ScribdCross
engine type=Document Sharing
fixed url=http://www.scribd.example/login

[EXTRA_STEP1]
form url=http://www.scribd.example/upload/supload
submit success=Your file is ready
file=doc.pdf
verify url=http://www.scribd.example/doc/[0-9]*[0-9]/*

[EXTRA_STEP2]
form url=http://www.scribd.example/login/upload/update_docs
submit success=docs updated
"#;
        let eng = parse_engine_str(raw, "ScribdCross").unwrap();
        let plan = resolve_plan(&eng, &test_input());
        // One static shell for every step: success markers match, but the
        // only link on the page is the shell's self-link — step1's form.
        let driver = MockDriver::with_html(
            "<html><body>done <a href=\"https://www.scribd.example/upload/supload\">upload</a> <span>Your file is ready</span> <span>docs updated</span></body></html>",
        );
        let report = execute_plan_with_solver(&plan, &driver, None).await;
        assert_eq!(report.successful, 2, "both markers matched: {report:?}");
        let u = report.submitted_url.as_deref().unwrap_or("");
        assert!(
            u.contains("scribd.example"),
            "recorded the shell self-link, got: {u:?} — {report:?}"
        );
        assert_eq!(
            report.content_success, 0,
            "a shell self-link is one of the engine's own form pages, not a created link: {u:?} — {report:?}"
        );
    }

    /// …and the same two-step shape WITHOUT a url contract is a plain
    /// "report the page we posted to" engine (comment-style) — legitimate.
    #[tokio::test]
    async fn t13b_cross_step_echo_without_contract_counts() {
        let raw = r#"\
[engine]
name=EchoNoContract
engine type=Comment
fixed url=http://forum.example/entry/10086

[STEP1]
form url=http://forum.example/entry/10086
submit success=comment added

[STEP2]
form url=http://forum.example/entry/10086/notify
submit success=notified
"#;
        let eng = parse_engine_str(raw, "EchoNoContract").unwrap();
        let plan = resolve_plan(&eng, &test_input());
        let driver = MockDriver::with_html(
            "<html><body><a href=\"http://forum.example/entry/10086\">permalink</a> <span>comment added</span> <span>notified</span></body></html>",
        );
        let report = execute_plan_with_solver(&plan, &driver, None).await;
        assert_eq!(report.successful, 2, "both markers matched: {report:?}");
        assert!(
            report.content_success >= 1,
            "no url contract → the post page is the verifiable link: {report:?}"
        );
    }

    /// A Submit-step success where only a **bare foreign root** came back is
    /// not a created link — the yy7 `alt="success"` false-positive shape.
    #[tokio::test]
    async fn t13b_bare_foreign_root_success_is_not_content_success() {
        let raw = r#"
[engine]
name=Yy7Like
engine type=URL Shortener
fixed url=http://yy7.example/

[ARTICLE_STEP1]
form url=http://yy7.example/
submit success=<div class="resultLink">|alt="success"
url=%url%
"#;
        let eng = parse_engine_str(raw, "Yy7Like").unwrap();
        let plan = resolve_plan(&eng, &test_input());
        // alt="success" matched, but there is NO real resultLink div in the
        // response — the settled page is the bare site root.
        let driver = MockDriver::with_html(
            "<html><body><img alt=\"success\" src=\"/ok.gif\"></body></html>",
        );
        let report = execute_plan_with_solver(&plan, &driver, None).await;
        assert_eq!(report.successful, 1, "alt=success matched: {report:?}");
        let u = report.submitted_url.as_deref().unwrap_or("");
        assert!(
            is_bare_root(u),
            "recorded URL should be the bare root: {u:?} — {report:?}"
        );
        assert_eq!(
            report.content_success, 0,
            "a bare site root is not a created link: {report:?}"
        );
    }

    /// Counter-proof: a genuine created permalink (deep, non-wall, ≠ form)
    /// still counts as content success — T13b must not over-demote.
    #[tokio::test]
    async fn t13b_real_permalink_still_counts() {
        let raw = r#"
[engine]
name=GoodEngine
engine type=Article
fixed url=http://good.example/

[ARTICLE_STEP1]
form url=http://good.example/submit
submit success=your url has been created
url=%url%
"#;
        let eng = parse_engine_str(raw, "GoodEngine").unwrap();
        let plan = resolve_plan(&eng, &test_input());
        let driver = MockDriver::with_html(
            "<html><body>your url has been created \
             <a href=\"https://good.example/article/42-created\">view</a></body></html>",
        );
        let report = execute_plan_with_solver(&plan, &driver, None).await;
        assert_eq!(report.successful, 1, "{report:?}");
        assert_eq!(
            report.submitted_url.as_deref(),
            Some("https://good.example/article/42-created"),
            "{report:?}"
        );
        assert_eq!(
            report.content_success, 1,
            "a real permalink counts as content: {report:?}"
        );
    }

    // ── T20 — GSA directive suite (reverse-engineered from the 991-engine corpus) ──

    #[test]
    fn t20_anchor_override_upcase_capitalize_maxlen() {
        let raw = "\
[engine]
name=DirEngine
engine type=Article
fixed url=http://d.example/

[STEP1]
form url=http://d.example/submit
title=Buy %anchor_text% now
submit success=posted
anchor text={green door|blue door}
upcase=1

[STEP2]
form url=http://d.example/submit2
title=%anchor_text%
submit success=posted
capitalize words=1
max length=11
";
        let eng = parse_engine_str(raw, "DirEngine").unwrap();
        let plan = resolve_plan(&eng, &test_input());
        let a1 = plan.steps.iter().find(|s| s.name.to_ascii_lowercase() == "step1").unwrap();
        let a2 = plan.steps.iter().find(|s| s.name.to_ascii_lowercase() == "step2").unwrap();
        let t1 = a1.fields.get("title").unwrap();
        // upcase=1 on a1: spintax branch uppercased, embedded in the value
        assert!(
            t1 == "Buy GREEN DOOR now" || t1 == "Buy BLUE DOOR now",
            "upcase: got {t1}"
        );
        let t2 = a2.fields.get("title").unwrap();
        // capitalize words + max length=11 on "buy shoes online"
        assert_eq!(t2, "Buy Shoes", "cap+maxlen: got {t2}");    }

    #[tokio::test]
    async fn t20_page_must_have_gate_counts_and_demotes() {
        let raw = "\
[engine]
name=PMH
engine type=Article
fixed url=http://m.example/

[STEP1]
form url=http://m.example/submit
submit success=posted
url=%url%
page must have=your article is live
";
        let eng = parse_engine_str(raw, "PMH").unwrap();
        // Present → counts as content.
        let plan = resolve_plan(&eng, &test_input());
        let ok = MockDriver::with_html(
            "<html><body>posted <a href=\"https://m.example/article/42\">x</a> your article is live</body></html>",
        );
        let rep = execute_plan_with_solver(&plan, &ok, None).await;
        assert_eq!(rep.successful, 1, "{rep:?}");
        assert_eq!(rep.content_success, 1, "phrase present must count: {rep:?}");
        // Absent → demoted (success marker alone is not enough).
        let plan = resolve_plan(&eng, &test_input());
        let bad = MockDriver::with_html(
            "<html><body>posted <a href=\"https://m.example/article/42\">x</a></body></html>",
        );
        let rep = execute_plan_with_solver(&plan, &bad, None).await;
        assert_eq!(rep.successful, 1, "{rep:?}");
        assert_eq!(rep.content_success, 0, "missing phrase must demote: {rep:?}");
        assert!(
            rep.errors.iter().any(|e| e.contains("required content")),
            "{:?}",
            rep.errors
        );
    }

    #[tokio::test]
    async fn t20_url_must_have_gate_counts_and_demotes() {
        let raw = "\
[engine]
name=UMH
engine type=Article
fixed url=http://m.example/

[STEP1]
form url=http://m.example/submit
submit success=posted
url=%url%
url must have=article/
";
        let eng = parse_engine_str(raw, "UMH").unwrap();
        let plan = resolve_plan(&eng, &test_input());
        let ok = MockDriver::with_html(
            "<html><body>posted <a href=\"https://m.example/article/42\">x</a></body></html>",
        );
        let rep = execute_plan_with_solver(&plan, &ok, None).await;
        assert_eq!(rep.content_success, 1, "link phrase present must count: {rep:?}");
        let plan = resolve_plan(&eng, &test_input());
        let bad = MockDriver::with_html(
            "<html><body>posted <a href=\"https://m.example/other/9\">x</a></body></html>",
        );
        let rep = execute_plan_with_solver(&plan, &bad, None).await;
        assert_eq!(rep.successful, 1, "{rep:?}");
        assert_eq!(rep.content_success, 0, "link phrase missing must demote: {rep:?}");
    }

    #[test]
    fn t20_search_candidates_skips_ads_ext_and_self_return() {
        let sr = StepSearch {
            url: "https://s.example/search".into(),
            term: "cool cats".into(),
            add_keyword: false,
            allow_return: false,
            skip_content: vec!["sponsored".into(), "ads".into()],
            skip_ext: true,
            extract_keywords: false,
        };
        let html = "\
<a href=\"https://partner.example/go\">partner</a>
<a href=\"https://s.example/\">home</a>
<a href=\"https://s.example/sponsored-ads-7\">deal</a>
<a href=\"https://s.example/article/77\">cool cats post</a>";
        let got = search_candidates(html, "https://s.example/", &sr, "https://s.example/").unwrap();
        assert_eq!(got, "https://s.example/article/77");
        // allow return=1 lets the self-home link through
        let sr2 = StepSearch { allow_return: true, ..sr.clone() };
        let html2 = "<a href=\"https://s.example/\">home</a>";
        assert_eq!(
            search_candidates(html2, "https://s.example/", &sr2, "https://s.example/").unwrap(),
            "https://s.example/"
        );
    }

    #[test]
    fn t20_titlecase_and_truncate_helpers() {
        assert_eq!(
            titlecase_words("{green door|Blue Sky}"),
            "{Green Door|Blue Sky}"
        );
        assert_eq!(titlecase_words("buy shoes online"), "Buy Shoes Online");
        assert_eq!(truncate_at_word("Buy Shoes Online", 11), "Buy Shoes");
        assert_eq!(truncate_at_word("buy", 11), "buy");
    }
}
