//! After-green-check state dump: grecaptcha keys, window token-like keys, widget div.
use engine::captcha_cdp::{close_tab, new_tab, Cdp};
use serde_json::json;
use std::time::Duration;

async fn click(cdp: &mut Cdp, x: f64, y: f64) {
    cdp.send("Input.dispatchMouseEvent", json!({"type":"mouseMoved","x":x-30.0,"y":y-30.0})).await.ok();
    tokio::time::sleep(Duration::from_millis(100)).await;
    cdp.send("Input.dispatchMouseEvent", json!({"type":"mousePressed","x":x,"y":y,"button":"left","buttons":1,"clickCount":1})).await.ok();
    tokio::time::sleep(Duration::from_millis(80)).await;
    cdp.send("Input.dispatchMouseEvent", json!({"type":"mouseReleased","x":x,"y":y,"button":"left","buttons":0,"clickCount":1})).await.ok();
}

const DUMP_JS: &str = r#"JSON.stringify((()=>{
  const out={};
  out.t = typeof grecaptcha;
  if (typeof grecaptcha==='object' && grecaptcha) {
    out.keys = Object.keys(Object(grecaptcha));
    for (const k of out.keys) {
      const v = grecaptcha[k];
      if (typeof v!=='function') { try { out['v_'+k]=String(v).slice(0,120); } catch(e){} }
    }
  }
  const wins = Object.getOwnPropertyNames(window).filter(k=>/recap|captcha|token|grec|sitekey/i.test(k));
  out.winkeys = wins;
  for (const k of wins) {
    try { const v=window[k]; if (typeof v!=='function' && v!=null) out['w_'+k]=String(v).slice(0,160); } catch(e){}
  }
  const w=document.querySelector('.g-recaptcha');
  out.wdiv = w ? {cls:w.className, attrs:Array.from(w.attributes).map(a=>a.name+'='+a.value.slice(0,80)), html:w.innerHTML.slice(0,400)} : null;
  out.respInp = Array.from(document.querySelectorAll('textarea,input')).filter(e=>/recaptcha|captcha|response/i.test((e.name||'')+' '+(e.id||''))).map(e=>({name:e.name,id:e.id,len:(e.value||'').length}));
  return out;
})())"#;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let ep = "http://127.0.0.1:9222";
    let page = "https://localendar.com/calendar/signup";
    let (tid, ws) = new_tab(ep, page).await?;
    let mut cdp = Cdp::connect_ws(&ws).await?;
    cdp.send("Page.enable", json!({})).await?;
    for _ in 0..30 {
        if matches!(cdp.eval("document.readyState").await.ok(), Some(serde_json::Value::String(s)) if s == "complete") { break; }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    tokio::time::sleep(Duration::from_millis(2500)).await;

    let g: serde_json::Value = cdp.eval(
        r#"(() => { const el = document.querySelector('.g-recaptcha'); if (!el) return "none"; el.scrollIntoView({block:'center',behavior:'instant'}); const r = el.getBoundingClientRect(); return JSON.stringify({x:r.x,y:r.y,w:r.width,h:r.height}); })()"#,
    ).await?;
    let g: serde_json::Value = serde_json::from_str(g.as_str().unwrap_or("{}")).map_err(|e| e.to_string())?;
    let x = g.get("x").and_then(|n| n.as_f64()).unwrap_or(380.0) + 28.0;
    let y = g.get("y").and_then(|n| n.as_f64()).unwrap_or(400.0) + g.get("h").and_then(|n| n.as_f64()).unwrap_or(78.0) / 2.0;
    println!("clicking at ({x}, {y})");
    click(&mut cdp, x, y).await;
    for i in 0..20 {
        tokio::time::sleep(Duration::from_millis(800)).await;
        let st: serde_json::Value = cdp.eval(
            r#"(() => {
              const a = document.querySelector('.rc-anchor-checkbox');
              const inp = document.querySelector('input[name=g-recaptcha-response], textarea[name=g-recaptcha-response]');
              let gr = ''; try { gr = String(grecaptcha.getResponse()); } catch(e) { gr = 'err'; }
              return JSON.stringify({cls: a ? a.className : 'none', inplen: inp ? (inp.value||'').length : -1, inpv: inp && (inp.value||'').slice(0,30), grlen: gr.length, gr: gr.slice(0,30)});
            })()"#,
        ).await?;
        let st: serde_json::Value = serde_json::from_str(st.as_str().unwrap_or("{}")).unwrap_or(json!({}));
        let inplen = st.get("inplen").and_then(|n| n.as_i64()).unwrap_or(-1);
        let grlen = st.get("grlen").and_then(|n| n.as_i64()).unwrap_or(0);
        println!("{i}: {st}");
        if inplen > 20 || grlen > 20 {
            println!("TOKEN SEEN!");
            break;
        }
    }
    let dump = cdp.eval(DUMP_JS).await?;
    println!("\n== DUMP final ==\n{}", dump.as_str().unwrap_or("<js value>"));
    close_tab(ep, &tid).await;
    Ok(())
}
