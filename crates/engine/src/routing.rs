//! # Driver routing — pick the right `Driver` for an engine (Feature #7).
//!
//! GSA does not push every engine through a live browser. Only the JS-heavy
//! specs — the ones whose submit path lives in JavaScript rather than a
//! plain `<form>` — need a rendered DOM. The `.ini` corpus makes that
//! boundary explicit through a handful of marker keywords. This module
//! encodes that rule so the executor (and a future "why did this need a
//! browser?" UI) share one definition:
//!
//! | Marker (field key / value) | Why it needs a browser |
//! |---|---|
//! | `set unknown variable`  (354 specs) | GSA sets a JS global before submit — the value only exists in a real `Runtime`. |
//! | `page must have` / `page must not have` (370) | Post-load condition evaluated against live `outerHTML`. |
//! | `iframe`  (15) | reCAPTCHA / Turnstile widget renders in a cross-origin iframe. |
//! | `onclick` (16) | `element.onclick = …` or inline handler — fires only in a DOM. |
//! | `recaptcha` / `hcaptcha` / `cf-turnstile` (76) | Bot wall; click + solve in the browser. |
//!
//! If *none* of those appear, the engine is a plain form and the
//! `HttpDriver` (reqwest) is faster and lighter — which is exactly the
//! split GSA itself makes between its HTTP and its browser submit engine.
//!
//! This type is intentionally `Send + Sync`, pure, and feature-agnostic: it
//! is the decision, the executor applies it.

/// Which execution path an engine should run on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DriverKind {
    /// Plain request/response — [`HttpDriver`] (no browser needed).
    Http,
    /// Live DOM required — render in Chrome (CDP) and operate the form.
    Browser,
    /// Mail-engine family: POP3 / web-inbox parse spec (`Email_Engines`).
    Email,
}

/// F#12 — keys whose presence in a `[setup]` step marks an engine as an
/// email/mail submission engine rather than a web form.
const EMAIL_SETUP_KEYS: &[&str] = &["pop3 server", "web parse url", "emails match"];

fn is_email_setup_step(step: &crate::enginespec::Step) -> bool {
    step.name.eq_ignore_ascii_case("setup")
        && step.fields.keys().any(|k| EMAIL_SETUP_KEYS.iter().any(|m| k == m))
}

/// The marker keywords that mark a step/engine as needing a rendered browser.
///
/// These are matched case-insensitively and substring-based against both
/// field *keys* and field *values*.  Substring matching is deliberate: GSA
/// uses inconsistent capitalisation and sometimes phrases a marker as part of
/// a longer key/URL (e.g. a `page must have1`, `-2` variant).
const BROWSER_MARKERS: &[&str] = &[
    "set unknown variable",
    "page must have",
    "page must not have",
    "iframe",
    "onclick",
    "onchange",
    "recaptcha",
    "hcaptcha",
    "cf-turnstile",
    "turnstile",
    "grecaptcha",
    "checkcaptcha",
    "javascript",
    "ajax",
];

fn marker_in(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    BROWSER_MARKERS.iter().any(|m| t.contains(m))
}

/// A `set unknown variable` value only forces a browser when it is a genuine
/// JavaScript expression (needs `Runtime.evaluate`). A spin token like
/// `%leave%` or `%random-1-99999%` is a plain form value the [`HttpDriver`]
/// resolves via `spin.rs`, so those stay HTTP-routable (they match GSA's own
/// "browser only when the value lives in a real `Runtime`" split).
fn set_unknown_is_js(value: &str) -> bool {
    let v = value.to_ascii_lowercase();
    v.contains(';')
        || v.contains("var ")
        || v.contains("function")
        || v.contains("document.")
        || v.contains("window.")
        || v.contains("=>")
        || v.contains("addevent")
        || v.contains("jquery")
        || v.starts_with('$')
}

/// Decide the [`DriverKind`] for a parsed engine.
///
/// Inspects **every step's field keys and values** for the JS markers above.
/// A single hit routes the whole engine to the browser — matching GSA, which
/// selects the engine-level submit mode, not a per-field one.
pub fn required_driver_kind(engine: &super::enginespec::Engine) -> DriverKind {
    // F#12 — mail family first: a `[setup]` carrying pop3/web-inbox keys is an
    // email engine (its spec has no form steps at all, so marker scan would miss it).
    if engine.steps.iter().any(is_email_setup_step) {
        return DriverKind::Email;
    }
    for step in &engine.steps {
        for (key, field) in step.fields.iter() {
            let k = key.to_ascii_lowercase();
            // Special case: `set unknown variable` only needs a browser when
            // its value is a real JS expression. GSA's `%leave%` / spin-token
            // forms are plain form values the HttpDriver resolves, so keep
            // them on the HTTP path (this is what lets Rentry-style no-login
            // pastbin forms run without a live CDP browser).
            if k.contains("set unknown variable") {
                if set_unknown_is_js(&field.value) {
                    return DriverKind::Browser;
                }
                continue;
            }
            if marker_in(key) || marker_in(&field.value) {
                return DriverKind::Browser;
            }
        }
    }
    DriverKind::Http
}

/// Decide the [`DriverKind`] from raw `.ini` text (used before parsing, e.g.
/// for bulk catalog stats).  Same markers, same substring rule.
pub fn required_driver_kind_from_text(raw: &str) -> DriverKind {
    if raw.is_empty() {
        return DriverKind::Http;
    }
    let low = raw.to_ascii_lowercase();
    if BROWSER_MARKERS.iter().any(|m| low.contains(m)) {
        DriverKind::Browser
    } else {
        DriverKind::Http
    }
}

// ---------------------------------------------------------------------------
//  Tests — grounded in real GSA spec shapes (cp1252 `.ini`), not invented.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use crate::enginespec::parse_engine_str;

    // A plain directory form — no JS markers — must route to HTTP.
    const PLAIN: &str = r#"
[engine]
name = Plain Directory
engine type = directory
url = http://example-directory.com/

[url]
register_step1 form url = /submit
register_step1 form name - url = url
register_step1 form name - description = description
register_step1 submit success = was submitted
"#;

    // A mail engine: `[setup]` with pop3 server → Email (checked before browser markers).
    const MAIL: &str = r#"
[setup]
emails match=*@gmail.com
pop3 server=WEB
web parse url=https://mail.example.com/inbox
url_front=<a href="
url_back=.verify</a>
message match=my link
"#;

    #[test]
    fn mail_setup_routes_to_email() {
        let e = parse_engine_str(MAIL, "mail.engine").unwrap();
        assert_eq!(required_driver_kind(&e), DriverKind::Email);
        // pop3-SSL mailbox family too (non-WEB server key is still a setup mail marker).
        const MAIL2: &str = r#"
[setup]
emails match=*@gmail.com
pop3 server=mail.example.com
pop3 port=995
"#;
        let e2 = parse_engine_str(MAIL2, "m2").unwrap();
        assert_eq!(required_driver_kind(&e2), DriverKind::Email);
    }

    const JSHEAVY: &str = r#"
[engine]
name = JS Directory
engine type = directory
url = http://example-js.com/

[url]
register_step1 form url = /post
register_step1 form name - url = url
register_step1 set unknown variable = var _link = url;
register_step1 page must have = Your link was added
"#;

    #[test]
    fn plain_directory_routes_to_http() {
        let eng = parse_engine_str(PLAIN, "Plain Directory").unwrap();
        assert_eq!(required_driver_kind(&eng), DriverKind::Http);
    }

    #[test]
    fn set_unknown_variable_routes_to_browser() {
        let eng = parse_engine_str(JSHEAVY, "JS Directory").unwrap();
        assert_eq!(required_driver_kind(&eng), DriverKind::Browser);
    }

    // Rentry-style: `set unknown variable=%leave%` is a spin token, not JS —
    // a no-login pastbin form must stay on the Http path (F#30 routing fix).
    #[test]
    fn set_unknown_variable_spin_token_stays_http() {
        const RENTRYLIKE: &str = r#"
[engine]
name = Rentry.co
engine type = directory
url = https://rentry.co/

[url]
global form id = entry-form
global form name - text = %article%
global set unknown variable = %leave%
"#;
        let eng = parse_engine_str(RENTRYLIKE, "Rentry.co").unwrap();
        assert_eq!(required_driver_kind(&eng), DriverKind::Http,
            "spin-token `set unknown variable` must not force a browser");
    }

    // A genuine JS expression still forces the browser.
    #[test]
    fn set_unknown_variable_js_expression_routes_to_browser() {
        const JSVAL: &str = r#"
[engine]
name = JSval
engine type = directory
url = http://x/

[url]
global set unknown variable = window.__link = document.querySelector('a').href;
"#;
        let eng = parse_engine_str(JSVAL, "JSval").unwrap();
        assert_eq!(required_driver_kind(&eng), DriverKind::Browser);
    }

    #[test]
    fn page_must_have_routes_to_browser() {
        let raw = "register_step1 page must have = You submitted\n";
        let eng = parse_engine_str(
            "[engine]\nname = P\nengine type = directory\nurl = http://x/\n\n[url]\nregister_step1 form url = /p\n\nglobal\npage must have = done\n",
            "P",
        )
        .unwrap();
        let _ = raw;
        assert_eq!(required_driver_kind(&eng), DriverKind::Browser);
    }

    #[test]
    fn text_variant_matches_keyword_case_insensitive() {
        assert_eq!(
            required_driver_kind_from_text("SET UNKNOWN VARIABLE = x"),
            DriverKind::Browser
        );
        assert_eq!(
            required_driver_kind_from_text("form url = /x\nform name = y"),
            DriverKind::Http
        );
        assert_eq!(
            required_driver_kind_from_text(""),
            DriverKind::Http
        );
    }

    #[test]
    fn recaptcha_and_turnstile_route_to_browser() {
        assert_eq!(
            required_driver_kind_from_text("register_step1 recaptcha sitekey = 6L..."),
            DriverKind::Browser
        );
        assert_eq!(
            required_driver_kind_from_text("iframe src = /grecaptcha"),
            DriverKind::Browser
        );
        assert_eq!(
            required_driver_kind_from_text("cf-turnstile action = backlink"),
            DriverKind::Browser
        );
    }

    // The markers must NOT false-positive on ordinary field values that merely
    // contain letters from a marker but not the phrase itself.
    #[test]
    fn ordinary_form_fields_do_not_route_to_browser() {
        let raw = "form url = /a/b\nfind url = http://site.com/link\nanchor = my keyword\n";
        assert_eq!(required_driver_kind_from_text(raw), DriverKind::Http);
    }
}
