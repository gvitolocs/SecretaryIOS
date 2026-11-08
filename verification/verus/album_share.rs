// Album share links: what a public request under /v1/media/a/{token} may see
// (crates/lucina-media/src/share_kernel_shared.rs; docs/verification.md, inventory row 16). The body
// is the SAME source the server compiles: this file includes it with its contracts,
// `crates/lucina-media/src/share.rs` includes it with the contracts erased, and every public handler
// (`share_http::gate`) asks it before reading anything of the album.
//
// Proven (AS1, decide_code):
//   - any access (the page, an item, an original) needs a known token whose share is not revoked
//     and not expired;
//   - an item or its original needs the item to be in the share's album at request time;
//   - an original needs the share to allow originals; a rendition is never an original;
//   - an unknown token is "not found", a revoked or expired share is "gone", whatever was asked;
//   - the result is exactly decide_spec, a total function of the request.
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

verus! {

/// The rule: 0 page, 1 item (a rendition), 2 original, 3 not found, 4 gone, 5 forbidden.
pub open spec fn decide_spec(
    known: bool,
    revoked: bool,
    has_expiry: bool,
    expires_at: u64,
    now: u64,
    asks_item: bool,
    item_in_album: bool,
    want_original: bool,
    allow_original: bool,
) -> u8 {
    if !known {
        3
    } else if revoked || (has_expiry && expires_at <= now) {
        4
    } else if !asks_item {
        if want_original { 3 } else { 0 }
    } else if !item_in_album {
        3
    } else if !want_original {
        1
    } else if allow_original {
        2
    } else {
        5
    }
}

} // verus!

include!("../../crates/lucina-media/src/share_kernel_shared.rs");

verus! {

/// A revoke takes effect for everything: once revoked, no request of any kind gets access.
proof fn revoke_closes_everything(has_expiry: bool, expires_at: u64, now: u64, asks_item: bool, item_in_album: bool, want_original: bool, allow_original: bool)
    ensures decide_spec(true, true, has_expiry, expires_at, now, asks_item, item_in_album, want_original, allow_original) == 4,
{
}

/// Expiry is monotone: a share that is gone at `now` stays gone later.
proof fn expired_stays_gone(expires_at: u64, now: u64, later: u64, asks_item: bool, item_in_album: bool, want_original: bool, allow_original: bool)
    requires
        expires_at <= now,
        now <= later,
    ensures decide_spec(true, false, true, expires_at, later, asks_item, item_in_album, want_original, allow_original) == 4,
{
}

fn main() {
}

} // verus!
