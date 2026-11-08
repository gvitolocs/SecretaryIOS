//! `real_copy <copy of a vault> <its golden from tools/vault-oracle>`: every check of
//! `tests/obsidian.rs` on a real vault, with timings. Exits 1 on any difference.
use lucina_vault::{Options, Vault};
use serde_json::Value;

#[path = "../tests/support/compare.rs"]
mod support;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let g: Value = serde_json::from_slice(&std::fs::read(&args[2]).unwrap()).unwrap();
    let t = std::time::Instant::now();
    let v = Vault::open(&args[1], Options::default()).unwrap();
    println!("open: {:?}, {:?}", t.elapsed(), v.counts());
    let d = support::compare(&v, &g);
    println!("{} checks, {} differ", d.checked, d.wrong.len());
    for w in d.wrong.iter().take(60) {
        println!("{w}");
    }
    let t = std::time::Instant::now();
    let hits = v.search("the", 50);
    println!("search 'the': {:?}, {} hits", t.elapsed(), hits.len());
    let t = std::time::Instant::now();
    let g2 = v.graph(&Default::default());
    println!(
        "graph: {:?}, {} nodes, {} edges",
        t.elapsed(),
        g2.nodes.len(),
        g2.edges.len()
    );
    let t = std::time::Instant::now();
    let mut blocks = 0;
    for n in v.notes() {
        blocks += lucina_vault::markdown::blocks(&v.read(&n.path).unwrap().text).len();
    }
    println!("blocks of every note: {:?}, {blocks} blocks", t.elapsed());
    std::process::exit(i32::from(!d.wrong.is_empty()));
}
