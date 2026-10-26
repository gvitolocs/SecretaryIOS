// Media streaming kernels (crates/secretary-media-stream/src/kernel_shared.rs):
//   S1  the HLS start offset for a resume (a u64, so never negative) lies in [0, max(duration - 1 s, 0)] and is the
//       requested position itself whenever that is already inside the range;
//   P1  the background preparation order (unfinished positions first, newer first within a
//       group) is a total preorder: reflexive, transitive and total, so sorting by it is
//       well defined.
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
include!("../../crates/secretary-media-stream/src/kernel_shared.rs");

verus! {
/// The last start a playlist of `duration` ms may have.
pub open spec fn start_max(duration: int) -> int {
    if duration > 1000 { duration - 1000 } else { 0 }
}

pub open spec fn prepare_before_spec(a_unfinished: bool, a_recency: int, b_unfinished: bool, b_recency: int) -> bool {
    (a_unfinished && !b_unfinished) || (a_unfinished == b_unfinished && a_recency >= b_recency)
}

pub proof fn prepare_order_reflexive(u: bool, r: int)
    ensures prepare_before_spec(u, r, u, r),
{
}

pub proof fn prepare_order_transitive(au: bool, ar: int, bu: bool, br: int, cu: bool, cr: int)
    requires
        prepare_before_spec(au, ar, bu, br),
        prepare_before_spec(bu, br, cu, cr),
    ensures prepare_before_spec(au, ar, cu, cr),
{
}

pub proof fn prepare_order_total(au: bool, ar: int, bu: bool, br: int)
    ensures prepare_before_spec(au, ar, bu, br) || prepare_before_spec(bu, br, au, ar),
{
}

/// An unfinished item always goes before a finished or unwatched one, whatever their ages.
pub proof fn unfinished_goes_first(ar: int, br: int)
    ensures
        prepare_before_spec(true, ar, false, br),
        !prepare_before_spec(false, br, true, ar),
{
}
} // verus!
fn main() {}
