//! Runs every model case with the default (CI) bounds; `--features deep`
//! for the exhaustive runs. One test per protocol so failures are local.

use verification_models::{all_cases, Case};

fn run(protocol: &str) {
    let deep = cfg!(feature = "deep");
    let cases: Vec<Case> = all_cases(deep)
        .into_iter()
        .filter(|c| c.protocol == protocol)
        .collect();
    assert!(!cases.is_empty(), "no cases for {protocol}");
    let mut errors = Vec::new();
    for c in cases {
        let (r, res) = c.verify();
        eprintln!(
            "{protocol}/{}: {} states, depth {}, {:.1}s",
            c.name,
            r.unique_states,
            r.max_depth,
            r.elapsed.as_secs_f64()
        );
        if let Err(e) = res {
            errors.push(format!("{}: {e}", c.name));
        }
    }
    assert!(errors.is_empty(), "{}", errors.join("\n\n"));
}

#[test]
fn timeline() {
    run("timeline");
}

#[test]
fn ownership() {
    run("ownership");
}

#[test]
fn shopping() {
    run("shopping");
}

#[test]
fn oauth() {
    run("oauth");
}

#[test]
fn gates() {
    run("gates");
}

#[test]
fn ink_sync() {
    run("ink-sync");
}

#[test]
fn deploy() {
    run("deploy");
}

#[test]
fn game() {
    run("game");
}

#[test]
fn media_backup() {
    run("media-backup");
}

#[test]
fn memos() {
    run("memos");
}

#[test]
fn player() {
    run("player");
}

#[test]
fn share() {
    run("share");
}
