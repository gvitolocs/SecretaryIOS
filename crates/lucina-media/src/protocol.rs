//! The pure core of the upload protocol, kept free of I/O so it can be model-checked
//! (`verification/models/src/media_upload.rs`) and unit-tested.
//!
//! An upload is a declared size plus an append-only byte stream. A chunk must start at or before
//! the current offset and must not leave a gap: bytes the server already has are skipped (iOS
//! background sessions retry a cut-off PUT from the start), the rest is appended, and a chunk
//! entirely below the offset is acknowledged, not appended twice. Finishing is allowed once, when every byte is in and the content hash matches.
//! Ingest then either dedupes, records a new version of the phone asset, or adds a new item.
//! None of these decisions ever removes an original or touches the phone.

/// Server-side state of one upload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UploadState {
    pub declared: u64,
    pub received: u64,
    pub finished: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChunkOutcome {
    /// Skip the first `skip` bytes of the chunk (already stored), append the rest at `received`;
    /// the new offset is `offset + len`.
    Append { skip: u64, new_offset: u64 },
    /// The chunk is entirely below the current offset: a retry of bytes already stored.
    AlreadyHave { offset: u64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ChunkError {
    #[error("upload already finished")]
    Finished,
    #[error("chunk at {offset} does not match the current offset {expected}")]
    OffsetMismatch { offset: u64, expected: u64 },
    #[error("chunk would exceed the declared size")]
    TooLarge,
}

/// Decide what to do with a chunk of `len` bytes sent at `offset`.
pub fn accept_chunk(s: UploadState, offset: u64, len: u64) -> Result<ChunkOutcome, ChunkError> {
    if s.finished {
        return Err(ChunkError::Finished);
    }
    let end = offset.checked_add(len).ok_or(ChunkError::TooLarge)?;
    if end > s.declared {
        return Err(ChunkError::TooLarge);
    }
    if offset > s.received {
        return Err(ChunkError::OffsetMismatch {
            offset,
            expected: s.received,
        });
    }
    if end <= s.received && len > 0 {
        return Ok(ChunkOutcome::AlreadyHave { offset: s.received });
    }
    Ok(ChunkOutcome::Append {
        skip: s.received - offset,
        new_offset: end.max(s.received),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum FinishError {
    #[error("upload already finished")]
    Finished,
    #[error("{missing} bytes still missing")]
    Incomplete { missing: u64 },
    #[error("content hash does not match")]
    HashMismatch,
}

/// Whether an upload may be finished. `expected` is the client's hash (if it sent one).
pub fn check_finish(
    s: UploadState,
    computed: &str,
    expected: Option<&str>,
) -> Result<(), FinishError> {
    if s.finished {
        return Err(FinishError::Finished);
    }
    if s.received < s.declared {
        return Err(FinishError::Incomplete {
            missing: s.declared - s.received,
        });
    }
    match expected {
        Some(e) if !e.eq_ignore_ascii_case(computed) => Err(FinishError::HashMismatch),
        _ => Ok(()),
    }
}

/// What the catalog already knows about the incoming content.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Known {
    /// The item that already holds exactly these bytes.
    pub by_hash: Option<String>,
    /// The item recorded for the same phone asset (device + asset id), with its current hash.
    pub by_asset: Option<(String, String)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IngestDecision {
    /// Same bytes are already in the library.
    Duplicate {
        item_id: String,
    },
    /// The phone edited an asset we have: the item keeps its id and gets a new current version.
    /// The previous original stays in the store.
    NewVersion {
        item_id: String,
    },
    New,
}

pub fn ingest_decision(known: &Known, hash: &str) -> IngestDecision {
    if let Some((item_id, current)) = &known.by_asset {
        if current == hash {
            return IngestDecision::Duplicate {
                item_id: item_id.clone(),
            };
        }
        // Edited on the phone. If another item already has these exact bytes we still version
        // the asset's own item, so edits never jump between items.
        return IngestDecision::NewVersion {
            item_id: item_id.clone(),
        };
    }
    match &known.by_hash {
        Some(item_id) => IngestDecision::Duplicate {
            item_id: item_id.clone(),
        },
        None => IngestDecision::New,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(declared: u64, received: u64) -> UploadState {
        UploadState {
            declared,
            received,
            finished: false,
        }
    }

    #[test]
    fn chunks_append_in_order() {
        assert_eq!(
            accept_chunk(st(10, 0), 0, 4),
            Ok(ChunkOutcome::Append {
                skip: 0,
                new_offset: 4
            })
        );
        assert_eq!(
            accept_chunk(st(10, 4), 4, 6),
            Ok(ChunkOutcome::Append {
                skip: 0,
                new_offset: 10
            })
        );
    }

    #[test]
    fn retried_chunk_is_acknowledged_not_duplicated() {
        assert_eq!(
            accept_chunk(st(10, 8), 4, 4),
            Ok(ChunkOutcome::AlreadyHave { offset: 8 })
        );
    }

    #[test]
    fn a_whole_file_retried_after_a_cut_skips_what_is_stored() {
        // iOS background session: PUT of the whole file at 0 after 131 071 bytes arrived.
        assert_eq!(
            accept_chunk(st(2_878_381, 131_071), 0, 2_878_381),
            Ok(ChunkOutcome::Append {
                skip: 131_071,
                new_offset: 2_878_381
            })
        );
        assert_eq!(
            accept_chunk(st(10, 4), 2, 4),
            Ok(ChunkOutcome::Append {
                skip: 2,
                new_offset: 6
            })
        );
    }

    #[test]
    fn gaps_and_overflow_are_refused() {
        assert_eq!(
            accept_chunk(st(10, 4), 6, 2),
            Err(ChunkError::OffsetMismatch {
                offset: 6,
                expected: 4
            })
        );
        assert_eq!(accept_chunk(st(10, 4), 4, 7), Err(ChunkError::TooLarge));
        assert_eq!(
            accept_chunk(st(10, 4), u64::MAX, 2),
            Err(ChunkError::TooLarge)
        );
        let done = UploadState {
            declared: 10,
            received: 10,
            finished: true,
        };
        assert_eq!(accept_chunk(done, 10, 0), Err(ChunkError::Finished));
    }

    #[test]
    fn finish_needs_all_bytes_and_matching_hash() {
        assert_eq!(
            check_finish(st(10, 9), "ab", None),
            Err(FinishError::Incomplete { missing: 1 })
        );
        assert_eq!(
            check_finish(st(10, 10), "ab", Some("cd")),
            Err(FinishError::HashMismatch)
        );
        assert_eq!(check_finish(st(10, 10), "ab", Some("AB")), Ok(()));
        assert_eq!(check_finish(st(0, 0), "ab", None), Ok(()));
    }

    #[test]
    fn ingest_dedupes_and_versions_edits() {
        let none = Known {
            by_hash: None,
            by_asset: None,
        };
        assert_eq!(ingest_decision(&none, "h1"), IngestDecision::New);
        let dup = Known {
            by_hash: Some("i1".into()),
            by_asset: None,
        };
        assert_eq!(
            ingest_decision(&dup, "h1"),
            IngestDecision::Duplicate {
                item_id: "i1".into()
            }
        );
        let same = Known {
            by_hash: Some("i1".into()),
            by_asset: Some(("i1".into(), "h1".into())),
        };
        assert_eq!(
            ingest_decision(&same, "h1"),
            IngestDecision::Duplicate {
                item_id: "i1".into()
            }
        );
        let edited = Known {
            by_hash: None,
            by_asset: Some(("i1".into(), "h0".into())),
        };
        assert_eq!(
            ingest_decision(&edited, "h1"),
            IngestDecision::NewVersion {
                item_id: "i1".into()
            }
        );
        let edited_to_other = Known {
            by_hash: Some("i2".into()),
            by_asset: Some(("i1".into(), "h0".into())),
        };
        assert_eq!(
            ingest_decision(&edited_to_other, "h1"),
            IngestDecision::NewVersion {
                item_id: "i1".into()
            }
        );
    }
}
