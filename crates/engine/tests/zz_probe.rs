//! ONE-SHOT PROBE (not a real test) — delete after first run.
use std::path::Path;

#[test]
fn probe_all_gsa_files() {
    let root = r"C:\Users\kingsly\Downloads\GSA_SER_v18.20_FINAL\GSA_SER_v18.20_FINAL";
    for dir in ["Engines", "Email_Engines"] {
        let (mut ok, mut bad, mut samples) = (0u32, 0u32, Vec::new());
        for e in std::fs::read_dir(Path::new(root).join(dir)).unwrap() {
            let p = e.unwrap().path();
            if p.extension().map(|x| x.to_string_lossy().to_lowercase()).as_deref() != Some("ini") { continue; }
            match engine::parse_engine_file(&p) {
                Ok(_) => ok += 1,
                Err(e) => { bad += 1; if samples.len() < 6 { samples.push((p.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default(), e.to_string())); } }
            }
        }
        let total = ok as f64 + bad as f64;
        let rate = if total > 0.0 { 100.0 * (ok as f64) / total } else { 0.0 };
        println!("[{}] ok={} bad={} rate={:.1}%", dir, ok, bad, rate);
        for (n, m) in samples { println!("  FAIL {n}: {m}"); }
    }
}
