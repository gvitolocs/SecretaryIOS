// Body-weight statistics kernel: the rounded mean used for every average
// (7-day moving average, weekly change, trends) is the floor of
// (sum + n/2) / n, and any such mean lies within the [min, max] of the
// values it averages.
use vstd::prelude::*;
macro_rules! verified_kernel {
    ($(pub fn $name:ident($($arg:ident: $ty:ty),*) -> ($ret:ident: $rty:ty)
       requires {$($pre:tt)*} ensures {$($post:tt)*} proof {$($proof:tt)*} body {$($body:tt)*})*) => {
        verus! { $(pub fn $name($($arg: $ty),*) -> ($ret: $rty)
            requires $($pre)*
            ensures $($post)*
            { $($proof)* $($body)* })* }
    };
}
include!("../../crates/secretary-body/src/kernel_shared.rs");

verus! {
/// If every averaged value lies in [lo, hi] (so n·lo ≤ sum ≤ n·hi), the
/// rounded mean returned by `rounded_mean` lies in [lo, hi] too.
pub proof fn mean_is_bounded(sum: int, count: int, lo: int, hi: int, result: int)
    requires
        count > 0, lo >= 0, lo <= hi,
        count * lo <= sum, sum <= count * hi,
        result >= 0,
        result * count <= sum + count / 2,
        sum + count / 2 < (result + 1) * count,
    ensures lo <= result <= hi,
{
    assert(count / 2 < count);
    assert(result <= hi) by(nonlinear_arith)
        requires count > 0, result * count <= sum + count / 2, sum <= count * hi, count / 2 < count;
    assert(lo <= result) by(nonlinear_arith)
        requires count > 0, count * lo <= sum, sum + count / 2 < (result + 1) * count, count / 2 >= 0;
}
} // verus!
fn main() {}
