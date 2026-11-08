//! Small pure functions with Verus proofs: the bodies live in `kernel_shared.rs`, which
//! `verification/verus/media_stream_kernels.rs` includes with the contracts kept.
// The bodies stay in the plain arithmetic Verus reasons about.
#![allow(clippy::implicit_saturating_sub)]
macro_rules! verified_kernel {
    ($(pub fn $name:ident($($arg:ident: $ty:ty),*) -> ($ret:ident: $rty:ty)
       requires {$($pre:tt)*} ensures {$($post:tt)*} proof {$($proof:tt)*} body {$($body:tt)*})*) => {
        $(pub fn $name($($arg: $ty),*) -> $rty { $($body)* })*
    };
}
include!("kernel_shared.rs");
