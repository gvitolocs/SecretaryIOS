// The runtime macro erases contracts; the Verus macro verifies these SAME bodies
// (verification/verus/media_stream_kernels.rs).
verified_kernel! {
    // Where a finished playlist starts: the viewer's resume point, never before 0 and never in
    // the last second (AVPlayer would load the final segment and stop at once).
    pub fn clamp_start_ms(start_ms: i64, duration_ms: u64) -> (result: u64)
    requires { true }
    ensures {
        result as int <= start_max(duration_ms as int),
        0 <= start_ms as int <= start_max(duration_ms as int) ==> result as int == start_ms as int,
        start_ms <= 0 ==> result == 0,
    }
    proof {}
    body {
        let max = if duration_ms > 1000 { duration_ms - 1000 } else { 0 };
        if start_ms <= 0 {
            0
        } else if start_ms as u64 > max {
            max
        } else {
            start_ms as u64
        }
    }

    // The background preparation order: `a` goes no later than `b`. Items with an unfinished
    // position come first; within a group the newer goes first (`recency` is the position's
    // `updated_at` for the first group, the item's `added_at` for the rest).
    pub fn prepare_before(a_unfinished: bool, a_recency: u64, b_unfinished: bool, b_recency: u64) -> (result: bool)
    requires { true }
    ensures { result == prepare_before_spec(a_unfinished, a_recency as int, b_unfinished, b_recency as int) }
    proof {}
    body { (a_unfinished && !b_unfinished) || (a_unfinished == b_unfinished && a_recency >= b_recency) }
}
