// The runtime macro erases contracts; the Verus macro verifies these SAME bodies.
verified_kernel! {
    pub fn rounded_mean(sum: u64, count: u64) -> (result: u64)
    requires { count > 0, count <= 1_000_000, sum <= 1_000_000_000_000_000 }
    ensures {
        result as int * count as int <= sum as int + count as int / 2,
        sum as int + count as int / 2 < (result as int + 1) * count as int,
    }
    proof {
        assert((sum as int + count as int / 2) / count as int * count as int <= sum as int + count as int / 2) by(nonlinear_arith) requires count > 0;
        assert(sum as int + count as int / 2 < ((sum as int + count as int / 2) / count as int + 1) * count as int) by(nonlinear_arith) requires count > 0;
    }
    body { (sum + count / 2) / count }
}
