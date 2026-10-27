//! Differential test: `view::free_slots` against a plain-Rust copy of the
//! Verus kernel `verification/verus/free_slots.rs` (the per-day sweep), with
//! the code's day loop around it, in UTC (local midnight = UTC midnight).
//! Random timed and all-day events over three days, random working hours,
//! windows, lengths and slot limits; the slots must be identical. The
//! kernel's theorems FS1-FS3 are re-checked on every answer, and
//! completeness (not proven in Verus) is checked against a five-minute grid:
//! every free, long enough gap inside the hours and the window is covered by
//! a slot unless the limit was reached.

use chrono::{DateTime, Duration, NaiveDate, TimeZone, Utc};
use secretary_calendar::{free_slots, Event, SlotQuery, Source};

/// Plain-Rust copy of the Verus kernel (exec part).
mod kernel {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Span {
        pub s: i64,
        pub e: i64,
    }

    pub fn day_slots(busy: &[Span], open: i64, close: i64, need: i64, left: usize) -> Vec<Span> {
        let mut out: Vec<Span> = Vec::new();
        if left == 0 {
            return out;
        }
        let mut cursor = open;
        let mut i = 0;
        while i < busy.len() {
            if cursor >= close {
                break;
            }
            let b = busy[i];
            if b.e <= cursor {
                i += 1;
                continue;
            }
            if b.s >= close {
                break;
            }
            let end = if b.s < close { b.s } else { close };
            if b.s > cursor && end - cursor >= need {
                out.push(Span { s: cursor, e: end });
                if out.len() >= left {
                    return out;
                }
            }
            if b.e > cursor {
                cursor = b.e;
            }
            i += 1;
        }
        if cursor < close && close - cursor >= need {
            out.push(Span {
                s: cursor,
                e: close,
            });
        }
        out
    }
}

use kernel::Span;

const H: i64 = 3_600_000;
const MIN: i64 = 60_000;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> i64 {
        (self.next() % n) as i64
    }
}

fn event(i: usize, start_ms: i64, end_ms: i64, all_day: bool) -> Event {
    Event {
        attendees: vec![],
        id: format!("t:{i}"),
        source: Source::Google,
        source_id: format!("{i}"),
        calendar: None,
        title: "x".into(),
        start: String::new(),
        end: String::new(),
        all_day,
        start_ms,
        end_ms,
        location: None,
        description: None,
        url: None,
        recurring: false,
        editable: false,
    }
}

fn ms(t: DateTime<Utc>) -> i64 {
    t.timestamp_millis()
}

/// The code's day loop in UTC around the kernel.
fn kernel_free_slots(events: &[Event], q: &SlotQuery) -> Vec<Span> {
    let need = q.minutes.max(1) * MIN;
    let mut busy: Vec<(i64, i64)> = events
        .iter()
        .filter(|e| !e.all_day && e.end_ms > e.start_ms)
        .map(|e| (e.start_ms, e.end_ms))
        .collect();
    busy.sort_unstable();
    let busy: Vec<Span> = busy.into_iter().map(|(s, e)| Span { s, e }).collect();
    let h0 = q.day_start_hour.min(23);
    let h1 = q.day_end_hour.clamp(h0 + 1, 24);
    let mut out = Vec::new();
    let mut day = q.from.date_naive();
    let last = q.to.date_naive();
    while day <= last && out.len() < q.max {
        let midnight = ms(Utc.from_utc_datetime(&day.and_hms_opt(0, 0, 0).unwrap()));
        let open = (midnight + i64::from(h0) * H).max(ms(q.from));
        let close = (midnight + i64::from(h1) * H).min(ms(q.to));
        out.extend(kernel::day_slots(
            &busy,
            open,
            close,
            need,
            q.max - out.len(),
        ));
        day += Duration::days(1);
    }
    out.truncate(q.max);
    out
}

#[test]
fn random_calendars_agree_with_the_kernel() {
    let mut rng = Rng(0xf2ee_5107);
    let base = Utc.from_utc_datetime(
        &NaiveDate::from_ymd_opt(2026, 11, 2)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap(),
    );
    let b0 = ms(base);
    let (mut slots_seen, mut limited) = (0usize, 0usize);
    for case in 0..3_000 {
        let n = rng.below(12) as usize;
        let events: Vec<Event> = (0..n)
            .map(|i| {
                let s = b0 + rng.below(72 * 12) * 5 * MIN;
                let len = match rng.below(5) {
                    0 => 0,
                    1 => -15 * MIN,
                    _ => (1 + rng.below(36)) * 5 * MIN,
                };
                event(i, s, s + len, rng.below(8) == 0)
            })
            .collect();
        let from = base + Duration::minutes(rng.below(36 * 60));
        let to = from + Duration::minutes(rng.below(60 * 60));
        let q = SlotQuery {
            from,
            to,
            minutes: [0, 15, 30, 45, 60, 120][rng.below(6) as usize],
            day_start_hour: rng.below(26) as u32,
            day_end_hour: rng.below(27) as u32,
            max: [0, 1, 2, 3, 5, 50][rng.below(6) as usize],
        };
        let got: Vec<Span> = free_slots(&events, &q, chrono_tz::UTC)
            .into_iter()
            .map(|(a, b)| Span { s: ms(a), e: ms(b) })
            .collect();
        let want = kernel_free_slots(&events, &q);
        assert_eq!(got, want, "case {case}: {q:?}");
        slots_seen += got.len();

        // FS1-FS3 on the answer.
        let need = q.minutes.max(1) * MIN;
        let busy: Vec<(i64, i64)> = events
            .iter()
            .filter(|e| !e.all_day && e.end_ms > e.start_ms)
            .map(|e| (e.start_ms, e.end_ms))
            .collect();
        for (j, s) in got.iter().enumerate() {
            assert!(
                s.e - s.s >= need && s.s >= ms(q.from) && s.e <= ms(q.to),
                "case {case}"
            );
            assert!(
                busy.iter().all(|&(bs, be)| s.e <= bs || be <= s.s),
                "case {case}"
            );
            if j > 0 {
                assert!(got[j - 1].e <= s.s);
            }
        }

        // Completeness on a five-minute grid (when the limit was not reached).
        if got.len() >= q.max {
            limited += 1;
            continue;
        }
        let h0 = i64::from(q.day_start_hour.min(23));
        let h1 = i64::from(q.day_end_hour.clamp(q.day_start_hour.min(23) + 1, 24));
        let mut t = (ms(q.from) / (5 * MIN)) * 5 * MIN;
        while t + need <= ms(q.to) {
            let day0 = t.div_euclid(24 * H) * 24 * H;
            let inside = t >= ms(q.from)
                && t >= day0 + h0 * H
                && t + need <= day0 + h1 * H
                && busy.iter().all(|&(bs, be)| t + need <= bs || be <= t);
            if inside {
                assert!(
                    got.iter().any(|s| s.s <= t && t + need <= s.e),
                    "case {case}: free gap at {t} missed, {q:?}"
                );
            }
            t += 5 * MIN;
        }
    }
    assert!(
        slots_seen > 1_000 && limited > 100,
        "not vacuous: {slots_seen} {limited}"
    );
}
