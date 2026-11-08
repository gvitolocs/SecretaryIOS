//! `exercise_copy <throwaway copy of a vault>`: writes through every operation, for
//! `tools/vault-oracle` to open afterwards with real Obsidian.
use lucina_vault::{Options, Vault};

fn main() {
    let root = std::env::args().nth(1).expect("a throwaway copy");
    assert!(!root.contains("/home/nes/Obsidian"), "never the real vault");
    let mut v = Vault::open(
        &root,
        Options {
            writable: true,
            ..Default::default()
        },
    )
    .unwrap();
    let paths: Vec<String> = v.notes().map(|n| n.path.clone()).collect();
    let t0 = std::time::Instant::now();
    let mut changed = 0;
    for p in paths.iter().step_by(25) {
        let n = v.read(p).unwrap();
        v.save(
            p,
            &format!("{}\nAdded by the write test. [[00 Home]]\n", n.text),
            &n.revision,
        )
        .unwrap();
        changed += 1;
    }
    println!("saved {changed} notes in {:?}", t0.elapsed());
    let created = v.new_note_path("Lucina vault write test", "");
    v.create(&created, b"Created by the write test. #lucina-vault-test\n")
        .unwrap();
    v.save_conflict_copy(&paths[3], "Conflict copy.\n", "2026-11-02", "test")
        .unwrap();
    let target = paths
        .iter()
        .rev()
        .find(|p| !v.backlinks(p).is_empty())
        .unwrap()
        .clone();
    let links = v.backlinks(&target).len();
    let plan = v
        .plan_rename(&target, "Moved by the write test.md")
        .unwrap();
    let t0 = std::time::Instant::now();
    let updated = v.rename(&plan).unwrap();
    println!(
        "renamed one with {links} links, {} notes updated in {:?}",
        updated.len(),
        t0.elapsed()
    );
    v.trash(&paths[5]).unwrap();
}
