//! Differential test: `notify::settings::Settings::{quiet_at, allows}` and
//! the hold rule of `notify::admit` against a plain-Rust copy of the Verus
//! kernel `verification/verus/quiet_hours.rs`. Settings are random (DND
//! before / after now, quiet hours on / off, valid and malformed HH:MM, equal
//! start and end, devices with and without overrides); the zone is UTC, so the
//! kernel's local minute is `now / 60000 mod 1440`.

use std::collections::BTreeMap;

use secretary_server::notify::settings::{parse_hhmm, DevicePrefs, Settings};
use secretary_server::notify::Category;

/// Plain-Rust copy of the Verus kernel (exec part).
mod kernel {
    pub struct Quiet {
        pub dnd_until: Option<i64>,
        pub enabled: bool,
        pub start: Option<u32>,
        pub end: Option<u32>,
    }

    pub fn quiet(q: &Quiet, now: i64, local: u32) -> bool {
        if let Some(until) = q.dnd_until {
            if until > now {
                return true;
            }
        }
        if !q.enabled {
            return false;
        }
        let (start, end) = match (q.start, q.end) {
            (Some(s), Some(e)) => (s, e),
            _ => return false,
        };
        if start == end {
            false
        } else if start < end {
            local >= start && local < end
        } else {
            local >= start || local < end
        }
    }

    pub fn held(q: &Quiet, now: i64, local: u32, reminder: bool, bypass: bool) -> bool {
        quiet(q, now, local) && !(reminder && bypass)
    }

    pub fn allows(cat_on: bool, dev: Option<(bool, Option<bool>)>) -> bool {
        if !cat_on {
            return false;
        }
        match dev {
            None => true,
            Some((enabled, ov)) => enabled && ov.unwrap_or(true),
        }
    }
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

const TIMES: &[&str] = &[
    "22:30", "07:30", "00:00", "23:59", "12:00", "07:30", "7:30", "24:00", "12:60", "", "ab:cd",
    "08:00", "22:30",
];

#[test]
fn quiet_hours_and_device_rules_agree_with_the_kernel() {
    let mut rng = Rng(0x0901_e7);
    let t0: i64 = 1_792_702_800_000; // 2026-10-22T21:00:00Z
    let mut quiet_count = 0;
    for _ in 0..100_000 {
        let mut s = Settings {
            time_zone: "UTC".into(),
            ..Settings::default()
        };
        let now = t0 + (rng.below(3 * 1440) as i64) * 60_000 + rng.below(60_000) as i64;
        s.dnd_until = match rng.below(4) {
            0 => Some(now + 1 + rng.below(3_600_000) as i64),
            1 => Some(now - rng.below(3_600_000) as i64),
            2 => Some(now),
            _ => None,
        };
        s.quiet_hours.enabled = rng.below(4) != 0;
        s.quiet_hours.start = TIMES[rng.below(TIMES.len() as u64) as usize].into();
        s.quiet_hours.end = TIMES[rng.below(TIMES.len() as u64) as usize].into();
        s.reminders_bypass_quiet = rng.below(2) == 0;
        let q = kernel::Quiet {
            dnd_until: s.dnd_until,
            enabled: s.quiet_hours.enabled,
            start: parse_hhmm(&s.quiet_hours.start),
            end: parse_hhmm(&s.quiet_hours.end),
        };
        let local = (now.div_euclid(60_000) % 1440) as u32;
        let want = kernel::quiet(&q, now, local);
        assert_eq!(s.quiet_at(now), want, "{:?} at {now}", s.quiet_hours);
        quiet_count += usize::from(want);

        // The hold rule of `admit`.
        for c in Category::ALL {
            let reminder = c == Category::Reminder;
            let got = s.quiet_at(now) && !(reminder && s.reminders_bypass_quiet);
            assert_eq!(
                got,
                kernel::held(&q, now, local, reminder, s.reminders_bypass_quiet)
            );
        }
    }
    assert!(quiet_count > 20_000, "not vacuous: {quiet_count}");
}

#[test]
fn device_rules_agree_with_the_kernel() {
    let mut rng = Rng(0xde71_ce);
    for _ in 0..20_000 {
        let mut s = Settings::default();
        s.categories.lucina_message = rng.below(4) != 0;
        s.categories.reminder = rng.below(4) != 0;
        s.categories.shopping = rng.below(2) == 0;
        let has_entry = rng.below(3) != 0;
        let mut entry = None;
        if has_entry {
            let enabled = rng.below(4) != 0;
            let mut cats = BTreeMap::new();
            for name in ["lucina_message", "reminder", "shopping"] {
                if rng.below(2) == 0 {
                    cats.insert(name.to_owned(), rng.below(2) == 0);
                }
            }
            s.devices.insert(
                "dev_1".into(),
                DevicePrefs {
                    enabled,
                    categories: cats.clone(),
                },
            );
            entry = Some((enabled, cats));
        }
        for c in [
            Category::LucinaMessage,
            Category::Reminder,
            Category::Shopping,
        ] {
            let dev = entry
                .as_ref()
                .map(|(en, cats)| (*en, cats.get(c.as_str()).copied()));
            assert_eq!(
                s.allows(c, "dev_1"),
                kernel::allows(s.categories.get(c), dev),
                "{c:?}"
            );
            // A device without an entry follows the global switch.
            assert_eq!(
                s.allows(c, "dev_other"),
                kernel::allows(s.categories.get(c), None)
            );
        }
    }
}
