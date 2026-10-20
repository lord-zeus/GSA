//! Decisive probe v2: scroll widget into view, click, read ANCHOR checkbox class + screenshots.

use engine::captcha_cdp::{close_tab, new_tab, b64_decode, Cdp};
use serde_json::json;
use std::time::Duration;

async fn shot(cdp: &mut Cdp, name: &str) {
    if let Ok(v) = cdp.send("Page.captureScreenshot", json!({"format":"png"})).await {
        if let Some(data) = v.get("data").and_then(|d| d.as_str()) {
            let bytes = b64_decode(data).unwrap_or_default();
            let path = std::env::temp_dir().join(format!("capv2_{name}.png"));
            std::fs::write(&path, bytes).ok();
            println!("shot -> {}", path.display());
        }
    }
}

async fn click(cdp: &mut Cdp, x: f64, y: f64) {
    cdp.send("Input.dispatchMouseEvent", json!({"type":"mouseMoved","x":x-30.0,"y":y-30.0})).await.ok();
    tokio::time::sleep(Duration::from_millis(100)).await;
    cdp.send("Input.dispatchMouseEvent", json!({"type":"mousePressed","x":x,"y":y,"button":"left","buttons":1,"clickCount":1})).await.ok();
    tokio::time::sleep(Duration::from_millis(80)).await;
    cdp.send("Input.dispatchMouseEvent", json!({"type":"mouseReleased","x":x,"y":y,"button":"left","buttons":0,"clickCount":1})).await.ok();
}

async fn cls_of(cdp: &mut Cdp, sess: &str) -> Option<serde_json::Value> {
    cdp.set_session(Some(sess.to_string()));
    let v = cdp.eval("((c)=>c?c.className:'none')(document.querySelector('#recaptcha-anchor')||document.querySelector('.rc-anchor-checkbox')||document.body)").await.ok();
    cdp.set_session(None);
    v
}

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

    // Scroll the widget to center, then report geometry.
    cdp.eval("JSON.stringify(((el)=>{if(!el) return {found:false}; el.scrollIntoView({block:'center',behavior:'instant'}); const r=el.getBoundingClientRect(); return {found:true,x:r.x,y:r.y,w:r.width,h:r.height, inViewport:(r.top>=0&&r.bottom<=innerHeight)};})(document.querySelector('.g-recaptcha')))").await?;
    tokio::time::sleep(Duration::from_millis(800)).await;
    let geom: serde_json::Value = cdp.eval("JSON.stringify(((el)=>{const r=el.getBoundingClientRect(); return {x:r.x,y:r.y,w:r.width,h:r.height};})(document.querySelector('.g-recaptcha')))").await?;
    println!("geom after scroll: {geom}");
    let g: serde_json::Value = serde_json::from_str(geom.as_str().unwrap_or("{}"))?;
    let x = g.get("x").and_then(|n| n.as_f64()).unwrap_or(380.0) + 28.0;
    let y = g.get("y").and_then(|n| n.as_f64()).unwrap_or(400.0) + g.get("h").and_then(|n| n.as_f64()).unwrap_or(78.0)/2.0;

    // Attach anchor session for class reads.
    let tg: serde_json::Value = cdp.send("Target.getTargets", json!({})).await?;
    let anchor_id = tg.get("targetInfos").and_then(|v| v.as_array()).unwrap_or(&Vec::new()).iter()
        .find(|t| t.get("url").and_then(|v| v.as_str()).map(|s| s.contains("recaptcha/api2/anchor")).unwrap_or(false))
        .and_then(|t| t.get("targetId").and_then(|v| v.as_str())).map(|s| s.to_string()).ok_or("no anchor")?;
    let attach: serde_json::Value = cdp.send("Target.attachToTarget", json!({"targetId": anchor_id, "flatten": true})).await?;
    let sess = attach.get("sessionId").and_then(|v| v.as_str()).unwrap_or_default().to_string();

    println!("checkbox class PRE: {:?}", cls_of(&mut cdp, &sess).await);
    shot(&mut cdp, "pre").await;

    println!("== clicking checkbox at {x} {y}");
    click(&mut cdp, x, y).await;

    for (i, d) in [(0, 900_u64), (1, 1200), (2, 1500), (3, 2500), (4, 3500), (5, 4000)] {
        tokio::time::sleep(Duration::from_millis(d)).await;
        let cls = cls_of(&mut cdp, &sess).await;
        let bf = cdp.send("Target.getTargets", json!({})).await.ok().and_then(|t| t.get("targetInfos").and_then(|v| v.as_array()).cloned()).map(|a| a.iter().any(|t| t.get("url").and_then(|v| v.as_str()).map(|s| s.contains("bframe")).unwrap_or(false))).unwrap_or(false);
        println!("t+{i}s cls={cls:?} bframe={bf}");
        shot(&mut cdp, &format!("a{i}")).await;
    }
    close_tab(ep, &tid).await;
    Ok(())
}
