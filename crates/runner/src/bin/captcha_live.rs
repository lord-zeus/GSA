//! Live F#29 test driver: `captcha-live <page-url> [--oracle <url>]`
//!
//! Opens the page in the CDP Edge on :9222, clicks the reCAPTCHA
//! checkbox, and — when the image grid appears — routes the screenshot
//! to the vision oracle (`tools/captcha_oracle.py`, :8090) where a human
//! clicks the tiles. Prints the resulting token (or the exact failure).

use engine::captcha::{CaptchaContext, CaptchaSolver};
use engine::captcha_cdp::CdpVisionSolver;
use std::error::Error;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let mut sitekey = String::new();
    let mut oracle: Option<String> = None;
    let Some(page) = args.next().filter(|a| !a.starts_with("--")) else {
        eprintln!("usage: captcha-live <page-url> [--oracle <url>] [--sitekey <k>]");
        std::process::exit(2);
    };
    while let Some(a) = args.next() {
        match a.as_str() {
            "--oracle" => oracle = args.next(),
            "--sitekey" => sitekey = args.next().unwrap_or_default(),
            _ => {}
        }
    }
    let solver = match oracle.as_deref() {
        Some(o) => CdpVisionSolver::new().with_oracle(o),
        None => CdpVisionSolver::new(),
    };
    let ctx = CaptchaContext {
        html: if sitekey.is_empty() {
            String::new()
        } else {
            // Fallback for direct runs: pretend the sitekey was found
            // in the page markup so the pre-check passes (the solver
            // re-discovers everything via CDP).
            format!("<div class=\"g-recaptcha\" data-sitekey=\"{sitekey}\"></div>")
        },
        url: Some(page.clone()),
        engine_name: Some("captcha-live".into()),
        ..Default::default()
    };
    println!("== captcha-live {page} (oracle={:?})", solver.name());
    match solver.solve(&ctx).await {
        Ok(tok) => {
            println!("\nTOKEN_OK len={} token={}", tok.len(), tok)
        }
        Err(e) => {
            eprintln!("\nSOLVE_ERR: {e:?}");
            std::process::exit(1);
        }
    }
    Ok(())
}
