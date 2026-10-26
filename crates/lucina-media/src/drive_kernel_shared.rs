// The Drive's safety decisions, with their Verus contracts. `drive_kernel.rs` includes this file
// with the contracts erased; `verification/verus/drive_kernels.rs` includes the SAME bodies and
// verifies them (docs/verification.md, protocol 15).
verified_kernel! {
    // DK1. One segment of a path inside a Drive folder: not empty, not `.` or `..`, at most 255
    // bytes, and without `/`, `\` or NUL (`bad_byte`). A path is safe when every segment is.
    pub fn segment_ok(len: u64, is_dot: bool, is_dotdot: bool, bad_byte: bool) -> (result: bool)
    requires { true }
    ensures { result == (len > 0 && len <= 255 && !is_dot && !is_dotdot && !bad_byte) }
    proof {}
    body { len > 0 && len <= 255 && !is_dot && !is_dotdot && !bad_byte }

    // DK2. The scope of an item (0 gallery, 1 films, 2 drive): phone backups are the Gallery even
    // when someone files one in a film album; film folders are Films; everything else is the Drive.
    pub fn scope_of(phone: bool, in_film_folder: bool) -> (result: u8)
    requires { true }
    ensures {
        result <= 2,
        (result == 0) == phone,
        (result == 1) == (!phone && in_film_folder),
        (result == 2) == (!phone && !in_film_folder),
    }
    proof {}
    body {
        if phone {
            0
        } else if in_film_folder {
            1
        } else {
            2
        }
    }

    // DK2'. The SQL filter of `scope` (db::scope_condition), as a predicate: an item is in exactly
    // the scope `scope_of` gives it.
    pub fn in_scope(scope: u8, phone: bool, in_film_folder: bool) -> (result: bool)
    requires { true }
    ensures { result == (scope == scope_of_spec(phone, in_film_folder)) }
    proof {}
    body {
        if scope == 0 {
            phone
        } else if scope == 1 {
            !phone && in_film_folder
        } else if scope == 2 {
            !phone && !in_film_folder
        } else {
            false
        }
    }

    // DK3. Dedupe of the bytes behind items (`blobs`, one row per blake3). `existing`: 0 no row,
    // 1 a copy the library owns (store or spool), 2 a file indexed in place (a Drive file, a memo).
    // Result: 0 place the incoming copy in the store, 1 drop the incoming copy (an owned copy
    // exists), 2 upgrade: place the incoming copy and make the row owned, 3 record the in-place
    // file, 4 keep the row as it is. An owned copy is never given up for an in-place reference,
    // and an owned incoming copy is dropped only when the library already owns the same bytes.
    pub fn blob_action(existing: u8, incoming_owned: bool) -> (result: u8)
    requires { existing <= 2 }
    ensures {
        result <= 4,
        result == 1 ==> existing == 1 && incoming_owned,
        existing == 1 ==> (result == 1 || result == 4),
        incoming_owned ==> (result == 0 || result == 1 || result == 2),
        !incoming_owned ==> (result == 3 || result == 4),
        owned_after(existing, result) == (existing == 1 || incoming_owned),
    }
    proof {}
    body {
        if incoming_owned {
            if existing == 0 {
                0
            } else if existing == 1 {
                1
            } else {
                2
            }
        } else if existing == 0 {
            3
        } else {
            4
        }
    }

    // DK4. Whether a finished scan of a Drive folder may trash the catalog rows of files it did not
    // see. Only when the folder was there (`root_ok`: an existing directory), on the filesystem its
    // files were indexed from (`same_fs`: the recorded st_dev; an unmounted disk leaves an empty
    // mountpoint on another filesystem, maybe with a stray file in it), the walk had no error, and
    // it saw files unless the catalog has none. Files on disk are never touched either way; a
    // trashed row comes back when its file does.
    pub fn may_prune(root_ok: bool, same_fs: bool, walk_errors: u64, seen: u64, known: u64) -> (result: bool)
    requires { true }
    ensures {
        result ==> root_ok && same_fs && walk_errors == 0,
        result ==> (seen > 0 || known == 0),
        (root_ok && same_fs && walk_errors == 0 && seen > 0) ==> result,
    }
    proof {}
    body { root_ok && same_fs && walk_errors == 0 && (seen > 0 || known == 0) }

    // DK4b. Whether a scan may record the folder's filesystem as its own: a clean walk that saw
    // every file the catalog knows there (a Drive copied to a new disk), or the very first files.
    // A stray file in an empty mountpoint never qualifies while the catalog knows other files, so
    // pruning stays off until the real disk is back.
    pub fn may_adopt_fs(root_ok: bool, walk_errors: u64, seen: u64, missing: u64) -> (result: bool)
    requires { true }
    ensures {
        result ==> root_ok && walk_errors == 0 && seen > 0 && missing == 0,
    }
    proof {}
    body { root_ok && walk_errors == 0 && seen > 0 && missing == 0 }

    // DK5. Whether the background Drive worker (hashing, text extraction, PDF pages, Office
    // conversion) may start a job: only below the temperature limit (Tctl < 90 °C by default) and
    // the load limit. A missing sensor reads as fine (`temp_known` false).
    pub fn drive_may_work(temp_known: bool, temp_c: u32, max_temp_c: u32, load_ok: bool) -> (result: bool)
    requires { true }
    ensures {
        result ==> load_ok,
        result && temp_known ==> temp_c < max_temp_c,
        (temp_known && temp_c >= max_temp_c) ==> !result,
    }
    proof {}
    body { load_ok && (!temp_known || temp_c < max_temp_c) }
}
