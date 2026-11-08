//! Differential test: `lucina_agents::turn_watch::verdict` against a plain-Rust copy of the
//! Verus kernel `verification/verus/acp_turn_watch.rs`, over every combination of
//! boundary values. The kernel's proven properties (W1-W5) then hold for production.

use lucina_agents::turn_watch::{verdict, Verdict, WatchFacts};

/// Plain-Rust copy of the Verus kernel.
mod kernel {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Facts {
        pub idle_ms: u64,
        pub has_stall: bool,
        pub stall_ms: u64,
        pub open_tools: u32,
        pub open_permissions: u32,
        pub log_limit: bool,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Verdict {
        Wait,
        Limit,
        Stalled,
    }

    pub fn verdict(f: &Facts) -> Verdict {
        if f.open_permissions > 0 {
            return Verdict::Wait;
        }
        if f.log_limit {
            return Verdict::Limit;
        }
        if f.has_stall && f.open_tools == 0 && f.idle_ms >= f.stall_ms {
            Verdict::Stalled
        } else {
            Verdict::Wait
        }
    }
}

fn k(v: kernel::Verdict) -> Verdict {
    match v {
        kernel::Verdict::Wait => Verdict::Wait,
        kernel::Verdict::Limit => Verdict::Limit,
        kernel::Verdict::Stalled => Verdict::Stalled,
    }
}

#[test]
fn production_matches_the_kernel_everywhere_near_the_edges() {
    let times = [0u64, 1, 299_999, 300_000, 300_001, u64::MAX];
    let stalls = [None, Some(0u64), Some(1), Some(300_000), Some(u64::MAX)];
    let counts = [0u32, 1, 2, u32::MAX];
    let mut n = 0;
    for idle_ms in times {
        for stall_ms in stalls {
            for open_tools in counts {
                for open_permissions in counts {
                    for log_limit in [false, true] {
                        let f = WatchFacts { idle_ms, stall_ms, open_tools, open_permissions, log_limit };
                        let kf = kernel::Facts { idle_ms, has_stall: stall_ms.is_some(), stall_ms: stall_ms.unwrap_or(0), open_tools, open_permissions, log_limit };
                        let v = verdict(&f);
                        assert_eq!(v, k(kernel::verdict(&kf)), "{f:?}");
                        // W1, W2 on production directly.
                        if open_permissions > 0 {
                            assert_eq!(v, Verdict::Wait, "{f:?}");
                        }
                        if open_tools > 0 {
                            assert_ne!(v, Verdict::Stalled, "{f:?}");
                        }
                        n += 1;
                    }
                }
            }
        }
    }
    assert_eq!(n, 6 * 5 * 4 * 4 * 2);
}
