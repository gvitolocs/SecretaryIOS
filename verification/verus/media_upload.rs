// Lucina media library: the upload, dedupe and backup-pacing kernels.
//
// Mirrors (simplified, same decisions):
//   accept_chunk / check_finish / ingest_decision <-> crates/lucina-media/src/protocol.rs
//                                                      (branch media-library, merged in ios-app)
//   after_error / pace                            <-> ios/Secretary/Features/Gallery/PhotoBackup.swift
//                                                      (run(): 429, transport, 410, other errors)
// The protocol around them (background-session resends, server restarts, p1's rate limit, two
// devices, the cancelled finish) is the Stateright model verification/models/src/media_backup.rs
// and its TLA+ twin formal/media/MediaBackup.tla.
//
// Proven:
//   (a) accept_chunk: an accepted chunk starts at or before the stored offset, never leaves a gap,
//       never passes the declared size; a chunk wholly below the offset is acknowledged, not
//       stored again; a finished upload takes nothing.
//   (b) the merge: stored bytes are always a prefix of the file, and appending what accept_chunk
//       keeps (the chunk minus the `skip` bytes already stored) gives exactly the longer prefix:
//       no gap, no duplicated byte, over any sequence of (re)sent chunks.
//   (c) check_finish: finishing requires every declared byte; with (b) the stored object is
//       the whole file, never a partial one.
//   (d) ingest_decision + the catalog: an edit of a known phone asset stays on that asset's item;
//       otherwise known bytes are a duplicate; a new item only for unknown bytes, so the catalog
//       never holds two items made from the same bytes by New, provided the decision and the
//       insert see the same catalog. The code decides and inserts under two separate lock
//       acquisitions (`ingest::ingest_file`): bug M3 in docs/verification.md, found by the model.
//   (e) after_error: a 429 pauses at least 10 s (Retry-After or 60 s), transport errors pause
//       120 s, a lost upload (410) restarts the asset, anything else skips only that asset; the
//       paced backup (4 s per asset, at most 3 requests each) stays within p1's 60 a minute.
use vstd::prelude::*;

verus! {

// ---------------------------------------------------------------- (a) chunks

pub enum ChunkOutcome {
    /// Skip `skip` bytes of the chunk (already stored), append the rest; offset becomes `new_offset`.
    Append { skip: u64, new_offset: u64 },
    /// The chunk is wholly below the stored offset (a retry): acknowledge, store nothing.
    AlreadyHave { offset: u64 },
    Finished,
    /// A gap: the chunk starts after the stored bytes.
    OffsetMismatch,
    TooLarge,
}

pub fn accept_chunk(declared: u64, received: u64, finished: bool, offset: u64, len: u64) -> (r: ChunkOutcome)
    requires
        received <= declared,
    ensures
        match r {
            ChunkOutcome::Append { skip, new_offset } => !finished && offset <= received && skip
                == received - offset && offset + len <= declared && new_offset == (if offset
                + len > received {
                offset + len
            } else {
                received as int
            }) && received <= new_offset <= declared,
            ChunkOutcome::AlreadyHave { offset: o } => !finished && o == received && len > 0
                && offset + len <= received,
            ChunkOutcome::Finished => finished,
            ChunkOutcome::OffsetMismatch => !finished && offset > received,
            ChunkOutcome::TooLarge => !finished && offset + len > declared,
        },
{
    if finished {
        return ChunkOutcome::Finished;
    }
    // checked_add + `end > declared`
    if offset > declared || len > declared - offset {
        return ChunkOutcome::TooLarge;
    }
    let end = offset + len;
    if offset > received {
        return ChunkOutcome::OffsetMismatch;
    }
    if end <= received && len > 0 {
        return ChunkOutcome::AlreadyHave { offset: received };
    }
    let new_offset = if end > received {
        end
    } else {
        received
    };
    ChunkOutcome::Append { skip: received - offset, new_offset }
}

// ---------------------------------------------------------------- (b) the merge

/// What the server keeps of a chunk `data[offset..offset+len]` when `received` bytes are stored.
pub open spec fn kept(data: Seq<u8>, received: int, offset: int, len: int) -> Seq<u8> {
    let skip = received - offset;
    if skip >= len {
        Seq::empty()
    } else {
        data.subrange(offset + skip, offset + len)
    }
}

proof fn lemma_merge_no_gap_no_duplicate(data: Seq<u8>, received: int, offset: int, len: int)
    requires
        0 <= offset <= received <= data.len(),
        0 <= len,
        offset + len <= data.len(),
    ensures
        data.subrange(0, received) + kept(data, received, offset, len) =~= data.subrange(
            0,
            if offset + len > received {
                offset + len
            } else {
                received
            },
        ),
{
    if received - offset < len {
        assert(data.subrange(0, received) + data.subrange(received, offset + len) =~= data.subrange(
            0,
            offset + len,
        ));
    }
}

/// Over any sequence of accepted chunks of the same file, the stored bytes stay a prefix of it.
proof fn lemma_stored_is_prefix(data: Seq<u8>, stored: Seq<u8>, offset: int, len: int)
    requires
        stored =~= data.subrange(0, stored.len() as int),
        stored.len() <= data.len(),
        0 <= offset <= stored.len(),
        0 <= len,
        offset + len <= data.len(),
    ensures
        ({
            let next = stored + kept(data, stored.len() as int, offset, len);
            next =~= data.subrange(0, next.len() as int) && next.len() >= stored.len()
        }),
{
    lemma_merge_no_gap_no_duplicate(data, stored.len() as int, offset, len);
}

// ---------------------------------------------------------------- (c) finish

pub enum FinishOutcome {
    Ok,
    Finished,
    Incomplete,
    HashMismatch,
}

pub fn check_finish(declared: u64, received: u64, finished: bool, computed: u64, expected: Option<u64>) -> (r: FinishOutcome)
    requires
        received <= declared,
    ensures
        r == FinishOutcome::Ok ==> !finished && received == declared && (expected.is_none()
            || expected == Some(computed)),
{
    if finished {
        return FinishOutcome::Finished;
    }
    if received < declared {
        return FinishOutcome::Incomplete;
    }
    match expected {
        Some(e) => if e != computed {
            FinishOutcome::HashMismatch
        } else {
            FinishOutcome::Ok
        },
        None => FinishOutcome::Ok,
    }
}

/// Finishing makes the stored object exactly the file: no partial object reaches the catalog.
proof fn lemma_finished_object_is_whole(data: Seq<u8>, stored: Seq<u8>)
    requires
        stored =~= data.subrange(0, stored.len() as int),
        stored.len() == data.len(),
    ensures
        stored =~= data,
{
}

// ---------------------------------------------------------------- (d) dedupe

pub enum Decision {
    Duplicate { item: u64 },
    NewVersion { item: u64 },
    New,
}

/// `by_asset`: the item recorded for the same phone asset and its current hash.
pub fn ingest_decision(by_hash: Option<u64>, by_asset: Option<(u64, u64)>, hash: u64) -> (r: Decision)
    ensures
        match by_asset {
            Some((item, current)) => if current == hash {
                r == (Decision::Duplicate { item })
            } else {
                r == (Decision::NewVersion { item })
            },
            None => match by_hash {
                Some(item) => r == (Decision::Duplicate { item }),
                None => r == Decision::New,
            },
        },
{
    match by_asset {
        Some((item, current)) => if current == hash {
            Decision::Duplicate { item }
        } else {
            Decision::NewVersion { item }
        },
        None => match by_hash {
            Some(item) => Decision::Duplicate { item },
            None => Decision::New,
        },
    }
}

/// The catalog's "first item made from these bytes" index (hash -> item).
pub open spec fn insert_new(cat: Map<u64, u64>, hash: u64, item: u64) -> Map<u64, u64> {
    cat.insert(hash, item)
}

/// New only happens for bytes the catalog does not know, so it never shadows an earlier item:
/// every hash keeps the one item it had, and the new hash maps to the new item.
proof fn lemma_new_never_duplicates(cat: Map<u64, u64>, by_asset: Option<(u64, u64)>, hash: u64, item: u64)
    requires
        by_asset.is_none(),
        !cat.dom().contains(hash),
    ensures
        ({
            let by_hash = if cat.dom().contains(hash) {
                Some(cat[hash])
            } else {
                None
            };
            let next = insert_new(cat, hash, item);
            by_hash.is_none() && next.dom() =~= cat.dom().insert(hash) && next[hash] == item
                && forall|h: u64| #[trigger] cat.dom().contains(h) ==> next[h] == cat[h]
        }),
{
}

// ---------------------------------------------------------------- (e) backoff and pacing

pub enum ErrorKind {
    /// 429, with Retry-After in seconds if the server sent it.
    RateLimited { retry_after: Option<u64> },
    /// Network, timeout, host not found, 502/503/504.
    Transport,
    /// 410/404 on a resumed upload: the server lost it.
    Gone,
    /// Anything else (a 500 about this asset, an export failure).
    Other,
}

pub enum Next {
    /// Stop the pass; kick again after this many seconds.
    PauseFor { seconds: u64 },
    /// Drop the upload and send this asset again from scratch (dedupe makes it cheap).
    Restart,
    /// Skip this asset; the next pass retries it.
    Skip,
}

pub const PACE_SECONDS: u64 = 4;
pub const REQUESTS_PER_ASSET: u64 = 3;
pub const DEVICE_LIMIT_PER_MINUTE: u64 = 60;

pub fn after_error(e: ErrorKind) -> (r: Next)
    ensures
        match e {
            ErrorKind::RateLimited { retry_after } => match r {
                Next::PauseFor { seconds } => seconds >= 10 && (retry_after.is_none() ==> seconds
                    == 60) && (retry_after.is_some() && retry_after.unwrap() >= 10 ==> seconds
                    == retry_after.unwrap()),
                _ => false,
            },
            ErrorKind::Transport => r == (Next::PauseFor { seconds: 120 }),
            ErrorKind::Gone => r == Next::Restart,
            ErrorKind::Other => r == Next::Skip,
        },
{
    match e {
        ErrorKind::RateLimited { retry_after } => {
            let wait = match retry_after {
                Some(s) => s,
                None => 60,
            };
            Next::PauseFor { seconds: if wait < 10 { 10 } else { wait } }
        },
        ErrorKind::Transport => Next::PauseFor { seconds: 120 },
        ErrorKind::Gone => Next::Restart,
        ErrorKind::Other => Next::Skip,
    }
}

/// Requests the backup can make in any 60 s window: at most one asset started every PACE_SECONDS
/// (plus the one in flight), each at most REQUESTS_PER_ASSET requests.
pub open spec fn requests_per_minute(pace: nat, per_asset: nat) -> nat {
    (60nat / pace + 1) * per_asset
}

proof fn lemma_paced_backup_leaves_room()
    ensures
        requests_per_minute(PACE_SECONDS as nat, REQUESTS_PER_ASSET as nat) == 48,
        requests_per_minute(PACE_SECONDS as nat, REQUESTS_PER_ASSET as nat) < DEVICE_LIMIT_PER_MINUTE,
{
}

fn main() {
}

} // verus!
