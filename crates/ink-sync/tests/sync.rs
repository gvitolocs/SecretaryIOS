//! End-to-end: devices with durable outboxes, crashes and torn writes, a
//! flaky network (drops, duplicate deliveries, reorders, partitions), and the
//! pure server. Plus property tests of `apply` (idempotency, commutativity).
use ink_library::model::{HighlightKind, ReadingStatus};
use ink_sync::client::Backoff;
use ink_sync::op::{Op, OpId, OpKind};
use ink_sync::state::{apply_mut, owner_set_status, SyncState, Verdict};
use ink_sync::transport::{
    serve, Faults, FlakyTransport, MemServer, Progressed, SplitMix64, SyncRequest, SyncResponse,
    Syncer, Transport, TransportError,
};
use proptest::prelude::*;
use std::collections::BTreeSet;
use std::io::Write;
use std::sync::{Arc, Mutex};

const BOOKS: [&str; 2] = ["bk_0000000000000001", "bk_0000000000000002"];

fn at(s: u64) -> String {
    chrono::DateTime::from_timestamp(1_790_000_000 + s as i64, 0)
        .unwrap()
        .to_rfc3339()
}

fn kind_for(r: u64) -> OpKind {
    let book = BOOKS[(r % 2) as usize].to_string();
    match (r / 2) % 7 {
        0 => OpKind::SetProgress {
            book_id: book,
            anchor: Some(format!("loc:{}", r % 50)),
            percent: Some((r % 101) as f32),
            location: None,
        },
        1 => OpKind::SetStatus {
            book_id: book,
            status: [
                ReadingStatus::Reading,
                ReadingStatus::Completed,
                ReadingStatus::Abandoned,
                ReadingStatus::NotStarted,
            ][(r / 14 % 4) as usize],
        },
        2 => OpKind::AddHighlight {
            book_id: book,
            kind: HighlightKind::Highlight,
            text: format!("passage {}", r % 3),
            anchor: format!("loc:{}", r % 3),
            note: r.is_multiple_of(5).then(|| format!("note {r}")),
        },
        3 => OpKind::EditNote {
            book_id: book.clone(),
            highlight_id: ink_sync::state::highlight_id(&ink_sync::state::highlight_key(
                &book,
                HighlightKind::Highlight,
                &format!("loc:{}", r % 3),
                &format!("passage {}", r % 3),
            )),
            note: (!r.is_multiple_of(4)).then(|| format!("edit {r}")),
        },
        4 => OpKind::DeleteHighlight {
            book_id: book,
            highlight_id: "hl_00000000000000aa".into(),
        },
        5 => OpKind::StartSession {
            book_id: book,
            session_id: format!("s{}", r % 3),
            location: Some((r % 40) as u32),
        },
        _ => OpKind::EndSession {
            book_id: book,
            session_id: format!("s{}", r % 3),
            location: Some((r % 40) as u32),
        },
    }
}

/// Applies `ops` in the given order to a fresh state.
fn run(ops: &[Op], owner_completed: bool) -> SyncState {
    let mut s = SyncState::default();
    if owner_completed {
        owner_set_status(&mut s, BOOKS[0], ReadingStatus::Completed, 0);
    }
    for o in ops {
        apply_mut(&mut s, o);
    }
    s
}

fn ops_strategy() -> impl Strategy<Value = Vec<Op>> {
    // Up to 3 devices, small clock range so stamps interleave across devices.
    prop::collection::vec((0u8..3, 0u64..20, any::<u64>()), 1..24).prop_map(|v| {
        let mut counters = [0u64; 3];
        v.into_iter()
            .map(|(d, t, r)| {
                counters[d as usize] += 1;
                Op {
                    op_id: OpId::new(format!("dev{d}"), counters[d as usize]),
                    at: at(t),
                    kind: kind_for(r),
                }
            })
            .collect()
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// Convergence: any permutation, with any duplicate deliveries, gives the
    /// same content; replaying everything changes nothing (idempotency).
    #[test]
    fn apply_converges_under_reordering_and_duplicates(
        ops in ops_strategy(),
        perm_seed in any::<u64>(),
        dups in prop::collection::vec(any::<prop::sample::Index>(), 0..8),
        owner_completed in any::<bool>(),
    ) {
        let reference = run(&ops, owner_completed);
        let mut shuffled = ops.clone();
        let mut rng = SplitMix64(perm_seed);
        for i in (1..shuffled.len()).rev() {
            shuffled.swap(i, (rng.next_u64() % (i as u64 + 1)) as usize);
        }
        for d in &dups {
            let op = d.get(&ops).clone();
            let pos = d.index(shuffled.len() + 1);
            shuffled.insert(pos, op);
        }
        let other = run(&shuffled, owner_completed);
        prop_assert_eq!(reference.content(), other.content());
        prop_assert_eq!(reference.clock, ops.len() as u64);
        let mut again = other.clone();
        for o in &ops {
            prop_assert_eq!(apply_mut(&mut again, o).verdict, Verdict::Duplicate);
        }
        prop_assert_eq!(again, other);
        if owner_completed {
            prop_assert!(reference.books.get(BOOKS[0]).is_none_or(|b| b.status.is_none()));
        }
    }

    /// The highlight set only grows; device ops never confirm a deletion.
    #[test]
    fn highlights_only_grow(ops in ops_strategy()) {
        let mut s = SyncState::default();
        let mut seen: BTreeSet<String> = BTreeSet::new();
        for o in &ops {
            apply_mut(&mut s, o);
            let now: BTreeSet<String> = s.highlights.keys().cloned().collect();
            prop_assert!(seen.is_subset(&now));
            prop_assert!(s.deleted.is_empty());
            seen = now;
        }
    }
}

/// Watches every request: after the client durably recorded an ack, the op
/// must never be sent again; and every response it delivers is remembered.
struct Spy<T> {
    inner: T,
    acked_seen: Arc<Mutex<BTreeSet<OpId>>>,
}

impl<T: Transport> Transport for Spy<T> {
    fn send(&mut self, req: &SyncRequest) -> Result<SyncResponse, TransportError> {
        {
            let acked = self.acked_seen.lock().unwrap();
            for op in &req.ops {
                assert!(!acked.contains(&op.op_id), "re-sent acked op {}", op.op_id);
            }
        }
        let r = self.inner.send(req);
        if let Ok(resp) = &r {
            self.acked_seen
                .lock()
                .unwrap()
                .extend(resp.acked.iter().cloned());
        }
        r
    }
}

type Dev = Syncer<Spy<FlakyTransport<MemServer>>>;

fn open(
    dir: &std::path::Path,
    d: usize,
    server: &MemServer,
    seed: u64,
    spy: &Arc<Mutex<BTreeSet<OpId>>>,
) -> Dev {
    let faults = Faults {
        drop_request: 15,
        drop_response: 15,
        duplicate: 15,
        delay: 10,
        partition: 5,
    };
    Syncer::open(
        dir.join(format!("dev{d}.jsonl")),
        &format!("dev{d}"),
        Spy {
            inner: FlakyTransport::new(server.clone(), seed, faults),
            acked_seen: spy.clone(),
        },
        3,
        Backoff {
            base_ms: 10,
            cap_ms: 1_000,
        },
    )
    .unwrap()
}

#[test]
fn devices_converge_through_crashes_and_a_flaky_network() {
    let mut failures = 0;
    for seed in 0..16u64 {
        let dir = tempfile::tempdir().unwrap();
        let server = MemServer::default();
        let mut rng = SplitMix64(seed.wrapping_mul(7919));
        let spies: Vec<_> = (0..2)
            .map(|_| Arc::new(Mutex::new(BTreeSet::new())))
            .collect();
        let mut devs: Vec<Dev> = (0..2)
            .map(|d| open(dir.path(), d, &server, seed * 10 + d as u64, &spies[d]))
            .collect();
        let mut recorded: Vec<Op> = Vec::new();
        let mut now = 0u64;
        for _ in 0..120 {
            now += 7;
            let d = (rng.next_u64() % 2) as usize;
            match rng.next_u64() % 10 {
                0..=3 => {
                    let op = devs[d]
                        .record(
                            at(now / 1000 + rng.next_u64() % 5),
                            kind_for(rng.next_u64()),
                        )
                        .unwrap();
                    recorded.push(op);
                }
                4 => {
                    // Crash: drop the process state; maybe a torn append.
                    let seed2 = rng.next_u64();
                    let path = dir.path().join(format!("dev{d}.jsonl"));
                    drop(devs.remove(d));
                    if seed2.is_multiple_of(2) {
                        let mut f = std::fs::OpenOptions::new()
                            .append(true)
                            .open(&path)
                            .unwrap();
                        f.write_all(br#"{"r":"op","op":{"opId":"#).unwrap();
                    }
                    devs.insert(d, open(dir.path(), d, &server, seed2, &spies[d]));
                }
                _ => {
                    let r = rng.next_u64();
                    if devs[d].step(now, r).unwrap() == Progressed::Failed {
                        failures += 1;
                    }
                }
            }
        }
        // Heal the network, deliver held-back requests, run to quiescence.
        for dev in devs.iter_mut() {
            dev.transport.inner.flush_delayed();
            dev.transport.inner.enabled = false;
        }
        for _ in 0..10_000 {
            now += 1_000;
            let mut idle = true;
            for dev in devs.iter_mut() {
                let r = rng.next_u64();
                if dev.step(now, r).unwrap() != Progressed::Idle {
                    idle = false;
                }
            }
            if idle {
                break;
            }
        }
        let s = server.state();
        for dev in &devs {
            assert!(
                dev.outbox.pending().is_empty(),
                "seed {seed}: unacked ops left"
            );
        }
        // No acked op lost, each applied once: the server's clock counts
        // exactly the recorded ops, and every one of them is in its watermark.
        for op in &recorded {
            assert!(s.is_applied(&op.op_id), "seed {seed}: {} lost", op.op_id);
        }
        assert_eq!(s.clock, recorded.len() as u64, "seed {seed}");
        // Convergence: a second server applying the same ops in log order
        // (instead of arrival order, with duplicates) holds the same content.
        let mut fresh = SyncState::default();
        let req = SyncRequest {
            device: "dev0".into(),
            ops: recorded
                .iter()
                .filter(|o| o.op_id.device == "dev0")
                .cloned()
                .collect(),
        };
        serve(&mut fresh, &req).unwrap();
        let req = SyncRequest {
            device: "dev1".into(),
            ops: recorded
                .iter()
                .filter(|o| o.op_id.device == "dev1")
                .cloned()
                .collect(),
        };
        serve(&mut fresh, &req).unwrap();
        assert_eq!(fresh.content(), s.content(), "seed {seed}");
    }
    assert!(
        failures > 20,
        "the network was not flaky enough: {failures} failures"
    );
}
