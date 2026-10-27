// Album share links: what a public request may see. `share.rs` includes this file with the
// contracts erased; `verification/verus/album_share.rs` includes the SAME body and verifies it
// (docs/verification.md, inventory row 16).
verified_kernel! {
    // AS1. The access a request under `/v1/media/a/{token}` gets. `known`: the token names a share
    // row; `revoked`: its `revoked_at` is set; `has_expiry`/`expires_at`/`now`: Unix ms;
    // `asks_item`: the request names an item (else it is the page or its item list);
    // `item_in_album`: that item is in the share's album right now (not trashed, not hidden);
    // `want_original`: the request is for the original file.
    // Result: 0 page, 1 item (a rendition), 2 original, 3 not found, 4 gone, 5 forbidden.
    pub fn decide_code(known: bool, revoked: bool, has_expiry: bool, expires_at: u64, now: u64, asks_item: bool, item_in_album: bool, want_original: bool, allow_original: bool) -> (result: u8)
    requires { true }
    ensures {
        result <= 5,
        result == decide_spec(known, revoked, has_expiry, expires_at, now, asks_item, item_in_album, want_original, allow_original),
        // Any access at all needs a known, live share.
        result <= 2 ==> known && !revoked && !(has_expiry && expires_at <= now),
        // An item or its original: the item is in the share's album.
        (result == 1 || result == 2) ==> asks_item && item_in_album,
        result == 1 ==> !want_original,
        result == 2 ==> want_original && allow_original,
        result == 0 ==> !asks_item && !want_original,
        // Unknown: not found; revoked or expired: gone, whatever was asked.
        !known ==> result == 3,
        known && (revoked || (has_expiry && expires_at <= now)) ==> result == 4,
    }
    proof {}
    body {
        if !known {
            3
        } else if revoked || (has_expiry && expires_at <= now) {
            4
        } else if !asks_item {
            if want_original {
                3
            } else {
                0
            }
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
}
