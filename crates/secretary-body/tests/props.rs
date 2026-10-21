//! Property checks: every average is bounded by the values it averages.
use proptest::prelude::*;
use secretary_body::{stats, Entry, MAX_WEIGHT, MIN_WEIGHT};

const DAY: u64 = stats::DAY_MS;

fn entries(raw: &[(u64, u64)]) -> Vec<Entry> {
    raw.iter()
        .enumerate()
        .map(|(i, &(ms, grams))| Entry {
            id: format!("e{i:03}"),
            weight_grams: grams,
            measured_ms: ms,
            note: String::new(),
            created_ms: 0,
            updated_ms: 0,
        })
        .collect()
}

proptest! {
    #[test]
    fn rounded_mean_lies_within_min_and_max(values in prop::collection::vec(MIN_WEIGHT..=MAX_WEIGHT, 1..200)) {
        let mean = stats::rounded_mean(&values).unwrap();
        prop_assert!(mean >= *values.iter().min().unwrap());
        prop_assert!(mean <= *values.iter().max().unwrap());
    }

    #[test]
    fn moving_average_is_bounded_by_its_window_and_global_extremes(
        raw in prop::collection::vec((0..200 * DAY, MIN_WEIGHT..=MAX_WEIGHT), 1..80),
        now_offset in 0..100 * DAY,
    ) {
        let entries = entries(&raw);
        let global_min = entries.iter().map(|e| e.weight_grams).min().unwrap();
        let global_max = entries.iter().map(|e| e.weight_grams).max().unwrap();
        let series = stats::Timeline::new(&entries).series();
        prop_assert_eq!(series.len(), entries.len());
        for p in &series {
            let window: Vec<u64> = entries
                .iter()
                .filter(|e| e.measured_ms <= p.measured_ms && e.measured_ms + 7 * DAY > p.measured_ms)
                .map(|e| e.weight_grams)
                .collect();
            prop_assert!(!window.is_empty());
            prop_assert!(p.avg7_grams >= *window.iter().min().unwrap());
            prop_assert!(p.avg7_grams <= *window.iter().max().unwrap());
            prop_assert!(p.avg7_grams >= global_min && p.avg7_grams <= global_max);
            prop_assert_eq!(Some(p.avg7_grams), stats::rounded_mean(&window));
        }
        let now = 200 * DAY + now_offset;
        let s = stats::stats(&entries, Some(80_000), now);
        let avg = s.avg7_grams.unwrap();
        prop_assert!(avg >= global_min && avg <= global_max);
        let span = (global_max - global_min) as i64;
        for d in [s.weekly_change_grams, s.trend30_grams, s.trend90_grams].into_iter().flatten() {
            prop_assert!(d.abs() <= span);
        }
        prop_assert_eq!(s.goal_distance_grams, Some(avg as i64 - 80_000));
    }
}
