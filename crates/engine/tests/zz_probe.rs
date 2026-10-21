//! ONE-SHOT PROBE (not a real test) — delete after first run.
use std::path::Path;

#[test]
fn probe_3le_stage_a() {
    let root = Path::new(r"C:\Users\kingsly\GSA_SER_portable");
    let f = root.join("Engines/3le.ru.ini");
    println!("[probe] candidate exists? {}", f.exists());
    let e = engine::enginespec::parse_engine_file(&f).expect("parse");
    println!(
        "[probe] name='{}' type={:?} steps={} base_url()={:?}",
        e.name, e.engine_type, e.steps.len(), e.base_url()
    );
    let input = engine::executor::SubmissionInput {
        url: "https://nordiccomps.com/".to_string(),
        keyword: "nordiccomps shop".to_string(),
        site_url: None,
        anchor: None,
        seed: Some(0x5EED_5EED),
        extra: Default::default(),
        files: Default::default(),
        random_options: vec![],
    };
    let plan = engine::executor::resolve_plan(&e, &input);
    let kind = engine::routing::required_driver_kind(&e);
    println!(
        "[probe] required_driver={:?} plan.steps={} target_url={:?} unresolved={:?}",
        kind, plan.steps.len(), plan.target_url, plan.unresolved_fields
    );
    for sp in &plan.steps {
        println!(
            "  plan step '{}' phase={:?} just_download={} modify_url={:?} form_url={:?} fields={:?}",
            sp.name, sp.phase, sp.just_download, sp.nav.modify_url, sp.nav.form_url,
            sp.fields.iter().map(|(k,v)|format!("{}={}",k,v)).collect::<Vec<_>>()
        );
    }
}
