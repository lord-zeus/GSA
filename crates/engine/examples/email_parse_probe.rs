
use std::path::Path;
fn main() {
    let dir = std::env::args().nth(1).unwrap();
    for name in ["1&1.ini", "07stees.online.ini"] {
        let p = Path::new(&dir).join(name);
        match engine::enginespec::parse_engine_file(&p) {
            Ok(e) => println!("OK {} | type={:?} url='{}' steps={} name='{}'",
                e.name, e.engine_type, e.url, e.steps.len(), e.name),
            Err(err) => println!("ERR {}: {}", p.display(), err),
        }
    }
}
