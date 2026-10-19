//! A little hands-on tour of Feature #1 — the spintax engine + result extraction.
//!
//! Run it with:
//!   cargo run -p ser-engine --example demo
//!
//! Type any spintax of your own on the last prompt and watch it spin.

use engine::enginespec::parse_engine_file;
use engine::extraction::{find_link, find_url};
use engine::spin::{Context, SpintaxResolver};

fn main() {
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(42);

    // Context with the real project fields GSA would supply.
    let mut ctx = Context::new();
    ctx.set("url", "https://my-site.example/");
    ctx.set("anchor_text", "buy running shoes online");
    ctx.set("website_title", "My Running Store");
    ctx.set("keyword", "running shoes");

    // A tiny spin file.  GSA's real .dat files (e.g. auto_anchor.dat) use a
    // top-level | as the item separator and may wrap lines mid-item; files
    // with no top-level | fall back to one-item-per-line.
    ctx.file(
        "adjectives.dat",
        "amazing | incredible | {best|top-rated} | fast",
    );

    let spin_examples: &[&str] = &[
        // Plain alternatives — GSA's core mechanism:
        "{I need |I want |Looking for } {new |better |cheaper } {shoes|sneakers |boots}",
        // Optional fragments (kept or dropped, 50/50 each):
        "Quality[ and affordable] [running] shoes",
        // Project variables + a numeric random draw:
        "%anchor_text% for only %random-49-129% dollars, ship today",
        // Built-in generators:
        "%random%{}%random%{} code: %random-1000-9999%",
        "%random_email%",
        // Your own spin file:
        "This is my %spinfile-adjectives.dat% deal of the day!",
    ];

    println!("=== 1) Spintax in action (seed {seed}) ===\n");
    for ex in spin_examples {
        let r = SpintaxResolver::new(seed);
        let once = r.resolve(ex, &ctx);
        let again = SpintaxResolver::new(seed).resolve(ex, &ctx);
        let other = SpintaxResolver::new(seed.wrapping_add(1)).resolve(ex, &ctx);
        println!("IN : {ex}\nOUT: {once}\n    (seed+1 gives a different draw: {other})\n    (same seed again → identical: {})\n",
            once == again);
    }

    println!("=== 2) Result extraction from a confirmation page ===");
    let html = r#"
        <html><body><h1>Submission successful!</h1>
        <p>Your link is live at
           <a href="https://directory.example/entry?id=84129">here</a>.</p>
        <a href="https://directory.example/entry?id=84129">View entry</a>
        </body></html>"#;
    println!(
        "find_url(html, \"directory.example\")  →  {:?}",
        find_url(html, "directory.example")
    );
    println!(
        "find_link(html, \"View entry\")         →  {:?}",
        find_link(html, "View entry")
    );
    println!(
        "find_url(html, \"no-such-site\")        →  {:?}   ← rejected when the site isn't confirmed",
        find_url(html, "no-such-site")
    );

    println!(
        "\n=== 3) A real GSA engine file parsed into our typed model ==="
    );
    let aardvark =
        r"C:\Users\kingsly\Downloads\Compressed\toCrack\ser_setup\install\Engines\Aardvark Topsites.ini";
    match parse_engine_file(std::path::Path::new(aardvark)) {
        Ok(e) => {
            let steps: Vec<String> = e.steps.iter().map(|s| s.name.clone()).collect();
            println!(
                "engine \"{}\"  type={:?}  register={}\nsteps: {}",
                e.name,
                e.engine_type,
                e.register,
                steps.join(", ")
            );
        }
        Err(err) => println!("(skipped — {err})"),
    }

    println!("\n=== 4) Your turn — type any spintax and press Enter (empty line = quit) ===");
    let stdin = std::io::stdin();
    let mut line = String::new();
    loop {
        print!("spin> ");
        use std::io::Write;
        let _ = std::io::stdout().flush();
        if !stdin.read_line(&mut line).unwrap_or(0) > 0 {
            break;
        }
        let t = line.trim().to_string();
        if t.is_empty() {
            break;
        }
        println!("      {}", SpintaxResolver::new(seed.wrapping_add(1234)).resolve(&t, &ctx));
        line.clear();
    }
    println!("bye.");
}
