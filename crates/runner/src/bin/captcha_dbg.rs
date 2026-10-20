//! Decisive F#29 probe v3:
//!  - log ALL candidate widget rects, pick largest visible, scroll, re-measure
//!  - install a VALUE-SETTER TRAP on the g-recaptcha-response field +
//!    wrap grecaptcha.getResponse so every token write is logged, even if
//!    the page clears the field immediately
//!  - click, then read the trap log.
use engine::captcha_cdp::{close_tab, new_tab, b64_decode, Cdp};
use serde_json::json;
use std::time::Duration;

async fn click(cdp: &mut Cdp, x: f64, y: f64) {
    let _ = cdp.send("Input.dispatchMouseEvent", json!({"type":"mouseMoved","x":x-24.0,"y":y-24.0,"button":"none"})).await;
    tokio::time::sleep(Duration::from_millis(40)).await;
    for typ in ["mousePressed", "mouseReleased"] {
        let _ = cdp.send("Input.dispatchMouseEvent", json!({"type":typ,"x":x,"y":y,"button":"left","buttons":if typ=="mousePressed"{1}else{0},"clickCount":1})).await;
        tokio::time::sleep(Duration::from_millis(70)).await;
    }
}

/// Log every candidate rect (no mutation).
const RECS: &str = r#"JSON.stringify((()=>{
  const out=[];
  const seen=new Set();
  document.querySelectorAll('.g-recaptcha, [data-sitekey]').forEach(e=>{
    if (seen.has(e)) return; seen.add(e);
    const r=e.getBoundingClientRect();
    out.push({sel:(e.id||e.tagName||'')+'['+(e.className||'').toString().slice(0,40)+']', x:Math.round(r.x),y:Math.round(r.y),w:Math.round(r.width),h:Math.round(r.height), disp:getComputedStyle(e).display});
  });
  document.querySelectorAll('iframe').forEach(e=>{
    if (String(e.src||'').includes('recaptcha')) { const r=e.getBoundingClientRect(); out.push({sel:'iframe:'+String(e.name||e.src).slice(0,60), x:Math.round(r.x),y:Math.round(r.y),w:Math.round(r.width),h:Math.round(r.height)}); }
  });
  out.push({sel:'viewport', w:innerWidth, h:innerHeight, scrollY:window.scrollY});
  return out;
})())"#;

/// Pick the best widget, scroll it into view (returns {found,cx,cy}).
const PICK: &str = r#"(() => {
  const cands = Array.from(document.querySelectorAll('.g-recaptcha, [data-sitekey]'));
  let el = null, best = 1;
  for (const e of cands) {
    const r = e.getBoundingClientRect();
    if (r.width > 10 && r.height > 10 && r.width * r.height > best) { best = r.width * r.height; el = e; }
  }
  if (!el) return { found: false };
  el.scrollIntoView({block: 'center', behavior: 'instant'});
  return { found: true };
})()"#;

/// Re-measure after scroll settle (fresh eval).
const MEAS: &str = r#"(() => {
  const cands = Array.from(document.querySelectorAll('.g-recaptcha, [data-sitekey]'));
  let el = null, best = 1;
  for (const e of cands) {
    const r = e.getBoundingClientRect();
    if (r.width > 10 && r.height > 10 && r.width * r.height > best) { best = r.width * r.height; el = e; }
  }
  if (!el) return { found: false };
  const b = el.getBoundingClientRect();
  return { found: true, cx: b.x + 28, cy: b.y + b.h / 2, rect: [Math.round(b.x), Math.round(b.y), Math.round(b.width), Math.round(b.height)] };
})()"#;

/// Install the value-setter trap + grecaptcha.getResponse wrap.
const HOOK: &str = r#"(() => {
  window.__tokLog = window.__tokLog || [];
  window.__hooked = false;
  const inp = document.querySelector('textarea[name=g-recaptcha-response], input[name=g-recaptcha-response]');
  if (inp) {
    const set = function(v){ this.__v = String(v); window.__tokLog.push(String(v)); };
    const get = function(){ return this.__v || ''; };
    Object.defineProperty(inp, 'value', { get, set, configurable: true });
  }
  try {
    if (grecaptcha && grecaptcha.getResponse && !window.__grecWrapped) {
      const og = grecaptcha.getResponse;
      grecaptcha.getResponse = function(){ const r = og.apply(this, arguments); if (r) window.__tokLog.push(String(r)); return r; };
      window.__grecWrapped = true;
    }
  } catch (e) {}
  window.__hooked = true;
  return { hooked: true, inp: !!inp };
})()"#;

const READLOG: &str = r#"JSON.stringify(window.__tokLog || [])"#;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let ep = "http://127.0.0.1:9222";
    let page = "https://localendar.com/calendar/signup";
    let (tid, ws) = new_tab(ep, page).await?;
    let mut cdp = Cdp::connect_ws(&ws).await?;
    let _ = cdp.send("Page.enable", json!({})).await;
    let _ = cdp.send("Runtime.enable", json!({})).await;
    let _ = cdp.send("Page.navigate", json!({ "url": page })).await;
    for _ in 0..20 {
        let rs = cdp.eval("document.readyState").await.ok().and_then(|v| v.as_str().map(|s| s.to_string()));
        if rs.as_deref() == Some("complete") { break; }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    // Wait for ANY widget candidate to render.
    let mut recs = json!({});
    for k in 0..25 {
        tokio::time::sleep(Duration::from_millis(600)).await;
        let raw = cdp.eval(RECS).await?;
        let arr: serde_json::Value = serde_json::from_str(raw.as_str().unwrap_or("[]")).unwrap_or(json!([]));
        let n = arr.as_array().map(|v| v.len()).unwrap_or(0).saturating_sub(1); // minus viewport row
        if n > 0 { println!("candidates present after ~{}ms", k as u64 * 600); recs = arr.clone(); break; }
    }
    if let Some(a) = recs.as_array() {
        println!("RECTS:");
        for r in a { println!("  {r}"); }
    }

    // Pick + scroll.
    for _ in 0..5 {
        let _ = cdp.eval(PICK).await?;
        tokio::time::sleep(Duration::from_millis(300)).await;
        let m = cdp.eval(MEAS).await?;
        if m.get("found").and_then(|v| v.as_bool()).unwrap_or(false) { break; }
    }
    let m: serde_json::Value = cdp.eval(MEAS).await?;
    println!("MEAS raw: {m}");
    let cx = m.get("cx").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let cy = m.get("cy").and_then(|v| v.as_f64()).unwrap_or(0.0);
    println!("click target: cx={cx} cy={cy} rect={:?}", m.get("rect"));

    // Install token traps BEFORE clicking.
    let h: serde_json::Value = cdp.eval(HOOK).await?;
    println!("hook: {h:?}");

    click(&mut cdp, cx, cy).await;
    println!("clicked. watching trap log...");
    let mut tok_log: Vec<String> = Vec::new();
    for i in 0..75 {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let raw = cdp.eval(READLOG).await?;
        let v: Vec<serde_json::Value> = serde_json::from_str(raw.as_str().unwrap_or("[]")).unwrap_or_default();
        let longs: Vec<String> = v.iter().filter(|s| s.as_str().map(|x| x.chars().count() > 20).unwrap_or(false)).map(|s| s.to_string()).collect();
        if !longs.is_empty() {
            tok_log = longs;
            println!("t={}ms TOKEN CAPTURED ({} writes, final len={})", i as u64*200, v.len(), tok_log.last().map(|s| s.chars().count()).unwrap_or(0));
            break;
        }
        if i % 15 == 0 { println!("t={}ms log={}", i as u64*200, v.len()); }
    }
    if tok_log.is_empty() { tok_log = { let raw = cdp.eval(READLOG).await?; serde_json::from_str::<Vec<String>>(raw.as_str().unwrap_or("[]")).unwrap_or_default() }; }

    let shot = cdp.send("Page.captureScreenshot", json!({"format":"png"})).await?;
    if let Some(d) = shot.get("data").and_then(|v| v.as_str()) {
        let b = b64_decode(d).unwrap_or_default();
        let p = std::env::temp_dir().join("capdbg3.png");
        std::fs::write(&p, b).ok();
        println!("screenshot -> {}", p.display());
    }
    if !tok_log.is_empty() {
        println!("\nFINAL TOKEN: {}", tok_log.last().unwrap());
    } else {
        println!("\nNO TOKEN captured — hook log empty");
    }
    println!("\nhook final log: {tok_log:?}");
    close_tab(ep, &tid).await;
    Ok(())
}
