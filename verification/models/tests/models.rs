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
fn deals() {
    run("deals");
}

/// The step semantics the deals cases rely on: the alert cap stops a run at `Load`, a
/// failed send writes nothing, and a lost response leaves the key on the server so the
/// identical retry is answered `duplicate` (and then acknowledged).
#[test]
fn deals_cap_failed_send_and_duplicate_retry() {
    use stateright::Model;
    use verification_models::deals::{Action, Deals, Kind, Outcome, Phase, State, RUST};
    let m = Deals {
        node: false,
        overlap: false,
        dedup: true,
        fix_pending: false,
        fix_ack_delivered: false,
        fix_min_price: false,
        max_runs: 6,
        max_edits: 2,
        max_faults: 2,
    };
    let step = |s: &State, a: Action| m.next_state(s, a).expect("enabled");
    let mut s = m.init_states().remove(0);
    for _ in 0..2 {
        s = step(&s, Action::Tick);
    }
    let mut capped = s.clone();
    capped.file.counts[0] = 4;
    assert!(m
        .next_state(&capped, Action::Load(RUST, Kind::Alert))
        .is_none());
    let file = s.file;
    for a in [
        Action::Load(RUST, Kind::Alert),
        Action::Poll(RUST, 0),
        Action::Decide(RUST),
        Action::Send(RUST, Outcome::Down),
    ] {
        s = step(&s, a);
    }
    assert_eq!(s.file, file);
    assert!(s.server.is_empty());
    for a in [
        Action::Load(RUST, Kind::Alert),
        Action::Poll(RUST, 0),
        Action::Decide(RUST),
        Action::Send(RUST, Outcome::Lost),
    ] {
        s = step(&s, a);
    }
    assert_eq!(s.file, file);
    assert_eq!(s.server.len(), 1);
    let received = s.recv;
    for a in [
        Action::Load(RUST, Kind::Alert),
        Action::Poll(RUST, 0),
        Action::Decide(RUST),
        Action::Send(RUST, Outcome::Delivered),
    ] {
        s = step(&s, a);
    }
    assert_eq!(s.server.len(), 1, "same key: duplicate");
    assert_eq!(s.recv, received);
    assert_eq!(s.run[RUST].phase, Phase::Sent);
    s = step(&s, Action::Save(RUST));
    assert_eq!(s.file.counts[0], 1);
    assert_eq!(
        s.file.notified,
        [1, 2, 0],
        "acknowledge records the alternative too"
    );
}

#[test]
fn otp() {
    run("otp");
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
fn drive_index() {
    run("drive-index");
}

#[test]
fn memos() {
    run("memos");
}

#[test]
fn meetings() {
    run("meetings");
}

#[test]
fn player() {
    run("player");
}

#[test]
fn share() {
    run("share");
}

#[test]
fn retirement() {
    run("retirement");
}
