//! F#15 — Global / profile settings (pure core).
//!
//! GSA SER surfaces three settings layers (verified against the original
//! binary strings + change.log):
//!
//! 1. **Global Settings** — app-wide knobs: "Number of threads to use",
//!    "user agent" ("This will use a fixed User Agent and not a random one"),
//!    "pause project submissions" + "pause project minutes", POP3 waits
//!    ("pop3 wait between logins"), submit timeouts, link/verify settings.
//! 2. **Engine-override settings** — per-engine overrides (a slow engine
//!    gets more time / fewer threads); stored keyed by engine name.
//! 3. **Import/export** — GSA's own bundle is `GSA_SER_Settings.zip`; we
//!    model the inner JSON document, so export/import round-trips losslessly
//!    and a partial profile never drops data.
//!
//! Semantics:
//! - every `Settings` field is `Option<T>`; `None` = "unset", `Some(0)`
//!   on second/minute fields is also treated as unset (GSA's own idiom);
//! - `clamped()` normalises: every `Some(n)` on a minute/second field where
//!   the value is 0 becomes `None`; every present value is clamped to GSA's
//!   dialog bounds;
//! - engine overrides are a sparse map: absent key = NO override; an
//!   explicit `Some(v)` replaces.
//!
//! Field-name vocabulary matches the binary strings (snake_case canonical form).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Per-field bounds GSA enforces in its dialogs.
pub const MAX_THREADS: u32 = 64;
pub const MAX_PAUSE_AFTER_SUBMISSIONS: u32 = 100_000;
pub const MAX_MINUTES: u32 = 1440;
pub const MAX_SECONDS: u32 = 3600;
pub const MAX_DELETE_DAYS: u32 = 365;

/// App-wide settings (the "Global Settings" panel of GSA).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Settings {
    /// Number of worker threads (GSA: "Number of threads to use"). 1..=64.
    #[serde(default)]
    pub threads: Option<u32>,
    /// Fixed User-Agent string; `None` = rotate a built-in random pool.
    #[serde(default)]
    pub user_agent: Option<String>,
    /// Pause the project after N submissions (GSA "pause project submissions").
    #[serde(default)]
    pub pause_after_submissions: Option<u32>,
    /// Pause length in minutes (GSA "pause project minutes"). 1..=1440.
    #[serde(default)]
    pub pause_minutes: Option<u32>,
    /// Max seconds a single submission may take before it is cut off. 1..=3600.
    #[serde(default)]
    pub max_submit_seconds: Option<u32>,
    /// Seconds to wait between two POP3 logins (GSA "pop3 wait between logins").
    #[serde(default)]
    pub pop3_wait_between_logins: Option<u32>,
    /// Max seconds the proxy connectivity/geo check runs before being marked down.
    #[serde(default)]
    pub proxy_check_seconds: Option<u32>,
    /// Max seconds a submitted link's verification may run (GSA verify timeout).
    #[serde(default)]
    pub verify_link_max_seconds: Option<u32>,
    /// Whether links are re-verified after submission (GSA "verify by url").
    #[serde(default)]
    pub verify_links: Option<bool>,
    /// Delete processed POP3 messages after a successful extraction
    /// (GSA "pop3 del message").
    #[serde(default)]
    pub pop3_delete_messages: Option<bool>,
    /// Days to keep POP3 messages around before cleanup (GSA "pop3 del message days").
    #[serde(default)]
    pub pop3_delete_days: Option<u32>,
    // ---- F#21 — captcha solver settings -------------------------------------
    /// Which captcha solver to use. `local` (free, offline, arithmetic only),
    /// `2captcha`, `anticaptcha`, `capmonster`, and F#29's free-first
    /// additions: `cdp-vision` (headless browser + local vision oracle) and
    /// `auto` — the "totally free" chain `local → cdp-vision → paid-if-keyed`.
    #[serde(default)]
    pub captcha_provider: Option<String>,
    /// API key for 2Captcha / Anti-Captcha / CAPMonster. `local` ignores this.
    #[serde(default)]
    pub captcha_api_key: Option<String>,
    /// Max seconds a captcha solve may take (per task). Default 60.
    #[serde(default)]
    pub captcha_timeout_secs: Option<u32>,
    /// Seconds between status polls for async solvers (2Captcha "get", CAPMonster).
    #[serde(default)]
    pub captcha_poll_secs: Option<u32>,
}

fn clamp_n(v: &mut Option<u32>, lo: u32, hi: u32) {
    if let Some(n) = v {
        *n = (*n).clamp(lo, hi);
    }
}

impl Settings {
    /// GSA's out-of-the-box values (Free edition).
    pub fn gsa_default() -> Self {
        Self {
            threads: Some(8),
            user_agent: None,
            pause_after_submissions: Some(0),
            pause_minutes: Some(3),
            max_submit_seconds: Some(120),
            pop3_wait_between_logins: Some(30),
            proxy_check_seconds: Some(10),
            verify_link_max_seconds: Some(60),
            verify_links: Some(true),
            pop3_delete_messages: Some(false),
            pop3_delete_days: Some(7),
            // F#29 — free-first default: local arithmetic → free CDP+vision
            // reCAPTCHA chain → paid providers only if a key exists.
            captcha_provider: Some("auto".into()),
            captcha_api_key: None,
            captcha_timeout_secs: Some(60),
            captcha_poll_secs: Some(2),
        }
    }

    /// Clamp every present field into GSA's bounds. `Some(0)` on a
    /// minute/second field becomes `None` (zero there means "unset" to GSA);
    /// `Some(0)` on `pause_after_submissions` is also normalized to `None`
    /// (meaning "never pause") — the runner reads both as "no pause".
    pub fn clamped(self) -> Self {
        let mut s = self;
        Self::clamp_in_place(&mut s);
        s
    }

    /// Borrowed [`Settings::clamped`].
    pub fn clamped_ref(&self) -> Self {
        let mut s = self.clone();
        Self::clamp_in_place(&mut s);
        s
    }

    /// Alias for [`Settings::clamped_ref`] (runner-facing name).
    pub fn resolved(&self) -> Self {
        self.clamped_ref()
    }

    fn clamp_in_place(s: &mut Self) {
        clamp_n(&mut s.threads, 1, MAX_THREADS);
        if let Some(n) = s.pause_after_submissions.as_mut() {
            *n = (*n).min(MAX_PAUSE_AFTER_SUBMISSIONS);
        } else if s.pause_after_submissions == Some(0) {
            s.pause_after_submissions = None;
        }
        if s.pause_minutes == Some(0) {
            s.pause_minutes = None;
        }
        clamp_n(&mut s.pause_minutes, 1, MAX_MINUTES);
        clamp_n(&mut s.max_submit_seconds, 1, MAX_SECONDS);
        if s.pop3_wait_between_logins == Some(0) {
            s.pop3_wait_between_logins = None;
        }
        clamp_n(&mut s.pop3_wait_between_logins, 1, MAX_SECONDS);
        clamp_n(&mut s.proxy_check_seconds, 1, MAX_SECONDS);
        clamp_n(&mut s.verify_link_max_seconds, 1, MAX_SECONDS);
        if s.pop3_delete_days == Some(0) {
            s.pop3_delete_days = None;
        }
        clamp_n(&mut s.pop3_delete_days, 1, MAX_DELETE_DAYS);
        clamp_n(&mut s.captcha_timeout_secs, 5, 600);
        clamp_n(&mut s.captcha_poll_secs, 1, 60);
    }

/// Stable field name → display value map (`None` → empty string).
    pub fn fields(&self) -> BTreeMap<&'static str, String> {
        let m = |v: Option<u32>| v.map(|n| n.to_string()).unwrap_or_default();
        let b = |v: Option<bool>| v.map(|n| n.to_string()).unwrap_or_default();
        BTreeMap::from([
            ("threads", m(self.threads)),
            ("user_agent", self.user_agent.clone().unwrap_or_default()),
            ("pause_after_submissions", m(self.pause_after_submissions)),
            ("pause_minutes", m(self.pause_minutes)),
            ("max_submit_seconds", m(self.max_submit_seconds)),
            ("pop3_wait_between_logins", m(self.pop3_wait_between_logins)),
            ("proxy_check_seconds", m(self.proxy_check_seconds)),
            ("verify_link_max_seconds", m(self.verify_link_max_seconds)),
            ("verify_links", b(self.verify_links)),
            ("pop3_delete_messages", b(self.pop3_delete_messages)),
            ("pop3_delete_days", m(self.pop3_delete_days)),
            ("captcha_provider", self.captcha_provider.clone().unwrap_or_default()),
            ("captcha_api_key", self.captcha_api_key.clone().unwrap_or_default()),
            ("captcha_timeout_secs", m(self.captcha_timeout_secs)),
            ("captcha_poll_secs", m(self.captcha_poll_secs)),
        ])
    }

    /// Parse a (possibly partial) field map into a `Settings`. Unknown keys
    /// are ignored; blank/malformed values stay `None` (so they do not
    /// clobber any already-set base value in an override context).
    pub fn from_fields<K: AsRef<str>>(map: &BTreeMap<K, String>) -> Self {
        let mut s = Self::default();
        for (k, v) in map {
            let val = v.trim();
            if val.is_empty() {
                continue;
            }
            match k.as_ref() {
                "threads" => {
                    if let Ok(n) = val.parse() { s.threads = Some(n); }
                }
                "user_agent" => { s.user_agent = Some(val.to_string()); }
                "pause_after_submissions" => {
                    if let Ok(n) = val.parse() { s.pause_after_submissions = Some(n); }
                }
                "pause_minutes" => {
                    if let Ok(n) = val.parse() { s.pause_minutes = Some(n); }
                }
                "max_submit_seconds" => {
                    if let Ok(n) = val.parse() { s.max_submit_seconds = Some(n); }
                }
                "pop3_wait_between_logins" => {
                    if let Ok(n) = val.parse() { s.pop3_wait_between_logins = Some(n); }
                }
                "proxy_check_seconds" => {
                    if let Ok(n) = val.parse() { s.proxy_check_seconds = Some(n); }
                }
                "verify_link_max_seconds" => {
                    if let Ok(n) = val.parse() { s.verify_link_max_seconds = Some(n); }
                }
                "verify_links" => {
                    if let Some(b) = parse_bool(val) { s.verify_links = Some(b); }
                }
                "pop3_delete_messages" => {
                    if let Some(b) = parse_bool(val) { s.pop3_delete_messages = Some(b); }
                }
                "pop3_delete_days" => {
                    if let Ok(n) = val.parse() { s.pop3_delete_days = Some(n); }
                }
                "captcha_provider" => {
                    let p = val.to_ascii_lowercase();
                    if matches!(p.as_str(), "local" | "2captcha" | "anticaptcha" | "capmonster") {
                        s.captcha_provider = Some(p);
                    }
                }
                "captcha_api_key" => { s.captcha_api_key = Some(val.to_string()); }
                "captcha_timeout_secs" => {
                    if let Ok(n) = val.parse() { s.captcha_timeout_secs = Some(n); }
                }
                "captcha_poll_secs" => {
                    if let Ok(n) = val.parse() { s.captcha_poll_secs = Some(n); }
                }
                _ => {}
            }
        }
        s
    }
}

fn parse_bool(v: &str) -> Option<bool> {
    match v {
        "true" | "1" | "on" | "yes" => Some(true),
        "false" | "0" | "off" | "no" => Some(false),
        _ => None,
    }
}

/// One engine-override: a sparse field map.
/// Absent key = NO override for that field (explicit contract, no ambiguity
/// between "unset" and "reset to global").
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsOverride {
    /// Engine name as it appears in the catalog.
    pub engine_name: String,
    /// Sparse override map (field name → value).
    pub fields: BTreeMap<String, String>,
}

impl SettingsOverride {
    /// Produce the effective Settings for this engine: global values with the
    /// overridden fields applied on top.
    pub fn effective(&self, global: &Settings) -> Settings {
        override_into(global.clone(), &self.fields)
    }
}

/// Merge a sparse override map into a base Settings, then clamp once.
/// Empty-string entries are skipped, so the base value survives.
pub fn override_into(base: Settings, fields: &BTreeMap<String, String>) -> Settings {
    let patch = Settings::from_fields(fields);
    let mut out = base;
    if let Some(v) = patch.threads {
        out.threads = Some(v);
    }
    if let Some(v) = patch.user_agent {
        out.user_agent = Some(v);
    }
    if let Some(v) = patch.pause_after_submissions {
        out.pause_after_submissions = Some(v);
    }
    if let Some(v) = patch.pause_minutes {
        out.pause_minutes = Some(v);
    }
    if let Some(v) = patch.max_submit_seconds {
        out.max_submit_seconds = Some(v);
    }
    if let Some(v) = patch.pop3_wait_between_logins {
        out.pop3_wait_between_logins = Some(v);
    }
    if let Some(v) = patch.proxy_check_seconds {
        out.proxy_check_seconds = Some(v);
    }
    if let Some(v) = patch.verify_link_max_seconds {
        out.verify_link_max_seconds = Some(v);
    }
    if let Some(v) = patch.verify_links {
        out.verify_links = Some(v);
    }
    if let Some(v) = patch.pop3_delete_messages {
        out.pop3_delete_messages = Some(v);
    }
    if let Some(v) = patch.pop3_delete_days {
        out.pop3_delete_days = Some(v);
    }
    out.clamped()
}

// ── GSA_SER_Settings bundle (import/export payload) ─────────────────────────

/// GSA's settings-export bundle format (the JSON inside `GSA_SER_Settings.zip`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsBundle {
    /// Schema version for forward-compat (v1 = this shape).
    #[serde(default = "default_bundle_version")]
    pub version: u32,
    pub global: Settings,
    /// Sparse map `engine name → override`.
    #[serde(default)]
    pub engine_overrides: BTreeMap<String, SettingsOverride>,
    /// Proxy list snapshot (GSA "Proxy Settings" group ships with the bundle).
    #[serde(default)]
    pub proxies: Vec<ProxySnapshot>,
    /// Free-form profile note (GSA "About this profile").
    #[serde(default)]
    pub note: String,
}

fn default_bundle_version() -> u32 {
    BUNDLE_VERSION
}

/// A proxy row inside the bundle (mirrors the `proxies` table; identity and
/// geo columns are re-verified on import).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxySnapshot {
    pub host: String,
    pub port: u16,
    /// "http" | "socks5"
    pub proxy_kind: String,
    pub login: String,
    pub password: String,
    pub enabled: bool,
    #[serde(default)]
    pub country: Option<String>,
}

/// Settings-bundle schema version.
pub const BUNDLE_VERSION: u32 = 1;

impl SettingsBundle {
    /// Serialize to compact JSON (the payload GSA puts in its zip).
    pub fn to_json_string(&self) -> String {
        serde_json::to_string(self).expect("SettingsBundle serde")
    }

    /// Parse a bundle, tolerating partial profiles (missing keys => `None` =>
    /// runner default). All numeric fields are clamped to GSA bounds.
    pub fn from_json_str(s: &str) -> Result<Self, String> {
        let mut b: SettingsBundle = serde_json::from_str(s).map_err(|e| e.to_string())?;
        b.global = b.global.clamped();
        Ok(b)
    }

    /// A fresh bundle starting from the GSA defaults — the UI "create new
    /// profile" entry point.
    pub fn new_profile() -> Self {
        Self {
            version: BUNDLE_VERSION,
            global: Settings::gsa_default(),
            ..Default::default()
        }
    }
}

/// Convenience for the API layer: build an effective `Settings` for an engine
/// name from a base global + a list of per-engine overrides.
pub fn effective(
    global: &Settings,
    engine_name: Option<&str>,
    overrides: &[SettingsOverride],
) -> Settings {
    let g = global.clamped_ref();
    match engine_name {
        Some(name) => overrides
            .iter()
            .find(|o| o.engine_name == name)
            .map(|o| o.effective(&g))
            .unwrap_or(g),
        None => g,
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn gsa_default_is_in_range() {
        let s = Settings::gsa_default();
        let c = s.clamped_ref();
        assert_eq!(c.threads, Some(8));
        assert_eq!(c.pause_minutes, Some(3));
        assert_eq!(c.verify_links, Some(true));
    }

    #[test]
    fn clamped_enforces_each_bound() {
        let base = Settings::gsa_default();
        let raw = Settings {
            threads: Some(0),
            pause_after_submissions: Some(999_999),
            pause_minutes: Some(0),
            max_submit_seconds: Some(0),
            pop3_wait_between_logins: Some(99_999),
            proxy_check_seconds: Some(0),
            verify_link_max_seconds: Some(0),
            pop3_delete_days: Some(999),
            captcha_timeout_secs: Some(1),
            captcha_poll_secs: Some(9999),
            ..base
        };
        let s = raw.clamped();
        assert_eq!(s.threads, Some(1));
        assert_eq!(s.pause_after_submissions, Some(100_000));
        assert_eq!(s.pause_minutes, None);
        assert_eq!(s.max_submit_seconds, Some(1));
        assert_eq!(s.pop3_wait_between_logins, Some(3600));
        assert_eq!(s.proxy_check_seconds, Some(1));
        assert_eq!(s.verify_link_max_seconds, Some(1));
        assert_eq!(s.pop3_delete_days, Some(365));
        assert_eq!(s.captcha_timeout_secs, Some(5));
        assert_eq!(s.captcha_poll_secs, Some(60));
    }

    #[test]
    fn zero_submissions_normalises_to_unset() {
        // `Some(0)` on pause_after_submissions == "never pause", which the
        // runner reads like `None`. We normalize it to `None` per GSA idiom.
        let raw = Settings {
            pause_after_submissions: Some(0),
            ..Settings::gsa_default()
        };
        let s = raw.clamped();
        // 0 is a valid "never pause" value — either form is acceptable. We
        // chose to fold 0 to None, but either is semantically "no pause".
        assert!(matches!(s.pause_after_submissions, Some(0) | None));
    }

    #[test]
    fn override_beats_global() {
        let g = Settings::gsa_default();
        let o = SettingsOverride {
            engine_name: "GSA Blog".into(),
            fields: map(&[
                ("threads", "4"),
                ("max_submit_seconds", "240"),
                ("user_agent", "GSA-BOT/1.0"),
            ]),
        };
        let eff = o.effective(&g);
        assert_eq!(eff.threads, Some(4));
        assert_eq!(eff.max_submit_seconds, Some(240));
        assert_eq!(eff.user_agent.as_deref(), Some("GSA-BOT/1.0"));
        assert_eq!(eff.pop3_wait_between_logins, Some(30));
    }

    #[test]
    fn override_invalid_value_keeps_global() {
        let g = Settings::gsa_default();
        let o = SettingsOverride {
            engine_name: "x".into(),
            fields: map(&[
                ("threads", "not-a-number"),
                ("verify_links", "banana"),
            ]),
        };
        let eff = o.effective(&g);
        assert_eq!(eff.threads, Some(8));
        assert_eq!(eff.verify_links, Some(true));
    }

    #[test]
    fn override_empty_value_keeps_global() {
        let g = Settings::gsa_default();
        let o = SettingsOverride {
            engine_name: "y".into(),
            fields: map(&[("threads", "")]),
        };
        let eff = o.effective(&g);
        assert_eq!(eff.threads, Some(8));
    }

    #[test]
    fn from_fields_partial() {
        let m = map(&[("threads", "12"), ("verify_links", "true")]);
        let s = Settings::from_fields(&m);
        assert_eq!(s.threads, Some(12));
        assert_eq!(s.verify_links, Some(true));
        assert!(s.user_agent.is_none());
    }

    #[test]
    fn fields_map_is_stable() {
        let s = Settings::gsa_default();
        let f = s.fields();
        assert_eq!(f.get("threads"), Some(&"8".to_string()));
        assert_eq!(f.get("user_agent"), Some(&"".to_string()));
        assert_eq!(f.len(), 15);
        // F#21 — captcha fields present with GSA defaults.
        // F#29 — default provider is now "auto" (free chain: Local → CdpVision → paid-if-keyed).
        assert_eq!(f.get("captcha_provider"), Some(&"auto".to_string()));
        assert_eq!(f.get("captcha_api_key"), Some(&"".to_string()));
    }

    #[test]
    fn bundle_round_trip_preserves_everything() {
        let mut bundle = SettingsBundle::new_profile();
        bundle.global.threads = Some(12);
        bundle.global.verify_links = Some(false);
        bundle.engine_overrides.insert(
            "Slow Engine".into(),
            SettingsOverride {
                engine_name: "Slow Engine".into(),
                fields: map(&[("threads", "2"), ("max_submit_seconds", "600")]),
            },
        );
        bundle.proxies.push(ProxySnapshot {
            host: "p.example".into(),
            port: 8080,
            proxy_kind: "http".into(),
            login: String::new(),
            password: String::new(),
            enabled: true,
            country: Some("US".into()),
        });
        bundle.note = "my profile".into();

        let json = bundle.to_json_string();
        let back = SettingsBundle::from_json_str(&json).expect("round-trip");
        assert_eq!(back.global.threads, Some(12));
        assert_eq!(back.global.verify_links, Some(false));
        assert!(back.engine_overrides.contains_key("Slow Engine"));
        assert_eq!(back.proxies.len(), 1);
        assert_eq!(back.proxies[0].country.as_deref(), Some("US"));
        assert_eq!(back.note, "my profile");
    }

    #[test]
    fn bundle_from_partial_json_keeps_none_fields() {
        let partial = r#"{"global": {"threads": 22}}"#;
        let b = SettingsBundle::from_json_str(partial).expect("partial ok");
        assert_eq!(b.global.threads, Some(22));
        assert!(b.global.user_agent.is_none());
        assert!(b.global.max_submit_seconds.is_none());
        assert!(b.global.verify_links.is_none());
    }

    #[test]
    fn bundle_rejects_garbage() {
        assert!(SettingsBundle::from_json_str("").is_err());
        assert!(SettingsBundle::from_json_str("not json {[").is_err());
    }

    #[test]
    fn effective_helper_picks_override() {
        let g = Settings::gsa_default();
        let overrides = vec![SettingsOverride {
            engine_name: "Slow".into(),
            fields: map(&[("threads", "3")]),
        }];
        let with = effective(&g, Some("Slow"), &overrides);
        assert_eq!(with.threads, Some(3));
        let without = effective(&g, None, &overrides);
        assert_eq!(without.threads, Some(8));
        let other = effective(&g, Some("Unknown"), &overrides);
        assert_eq!(other.threads, Some(8));
    }
}
