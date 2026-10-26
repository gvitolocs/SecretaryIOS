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

/// `[start, end)` in local hours; equal ends never open, start > end wraps.
pub open spec fn in_window_spec(start: int, end: int, hour: int) -> bool {
    if start == end {
        false
    } else if start < end {
        start <= hour && hour < end
    } else {
        hour >= start || hour < end
    }
}

/// CPU-path jobs need the window; no job starts when the temperature or load is over.
pub open spec fn may_start_spec(cpu_path: bool, in_window: bool, temp_ok: bool, load_ok: bool) -> bool {
    (!cpu_path || in_window) && temp_ok && load_ok
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

/// The CPU path never starts outside the night window.
pub proof fn cpu_path_never_starts_outside_the_window(temp_ok: bool, load_ok: bool)
    ensures !may_start_spec(true, false, temp_ok, load_ok),
{
}

/// Nothing starts while the temperature is over the limit.
pub proof fn nothing_starts_when_hot(cpu_path: bool, window: bool, load_ok: bool)
    ensures !may_start_spec(cpu_path, window, false, load_ok),
{
}

/// Nothing starts while the load is over the limit.
pub proof fn nothing_starts_when_loaded(cpu_path: bool, window: bool, temp_ok: bool)
    ensures !may_start_spec(cpu_path, window, temp_ok, false),
{
}

/// The start hour is inside a non-wrapping window.
pub proof fn window_start_is_in(start: int, end: int)
    requires 0 <= start < end <= 23,
    ensures in_window_spec(start, end, start),
{
}

/// The end hour is outside a non-wrapping window.
pub proof fn window_end_is_out(start: int, end: int)
    requires 0 <= start < end <= 23,
    ensures !in_window_spec(start, end, end),
{
}

/// An empty window (start == end) never opens.
pub proof fn empty_window_never_opens(start: int, hour: int)
    ensures !in_window_spec(start, start, hour),
{
}
} // verus!
fn main() {}
