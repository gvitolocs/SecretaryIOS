// The runtime macro erases contracts; the Verus macro verifies these SAME bodies.
verified_kernel! {
    pub fn set_volume(weight: u64, reps: u64, completed: bool, warmup: bool) -> (result: u64)
    requires { weight <= 2_000_000, reps <= 1_000 }
    ensures { result == if completed && !warmup { weight * reps } else { 0 }, result <= 2_000_000_000 }
    proof { assert(weight as int * reps as int <= 2_000_000_000) by(nonlinear_arith) requires weight <= 2_000_000, reps <= 1_000; }
    body { if completed && !warmup { weight * reps } else { 0 } }

    pub fn rest_remaining(deadline: u64, now: u64) -> (result: u64)
    requires { }
    ensures { (result == 0) == (now >= deadline), result <= deadline }
    proof { }
    body { if now >= deadline { 0 } else { (deadline - now - 1) / 1_000 + 1 } }

    pub fn rest_deadline(now: u64, seconds: u64) -> (result: u64)
    requires { now <= 18_446_744_073_705_951_615, seconds <= 3_600 }
    ensures { result >= now, result - now == seconds * 1_000 }
    proof { }
    body { now + seconds * 1_000 }

    pub fn best_record(previous: u64, candidate: u64) -> (result: u64)
    requires { }
    ensures { result >= previous, result >= candidate, result == previous || result == candidate }
    proof { }
    body { if candidate > previous { candidate } else { previous } }
}

