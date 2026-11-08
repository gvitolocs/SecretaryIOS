//! Port of `api/local/LocalSortType.java` and `api/comments/*`.

use serde::{Deserialize, Serialize};

/// `LocalSortType.Type`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LocalSortKind {
    Title,
    Date,
    PageCount,
    Artist,
    Group,
    Random,
}

pub const LOCAL_SORT_KIND_LEN: usize = 6;
// Java bytes: MASK_DESCENDING = (byte)0x80 (sign-extends to 0xFFFFFF80 in int
// context), MASK_TYPE = (byte)0x7F (stays +127). Computed separately on purpose.
const MASK_DESCENDING: i32 = -128;
const MASK_TYPE: i32 = 127;

/// `LocalSortType` — packed into a single byte-ish hash by the original.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalSortType {
    pub kind: LocalSortKind,
    pub descending: bool,
}

impl LocalSortType {
    /// `hashCode()` — `ordinal() | (descending ? 0x80 : 0)`.
    pub fn hash(&self) -> i32 {
        let mut h = self.kind as usize as i32;
        if self.descending {
            h |= MASK_DESCENDING;
        }
        h
    }

    /// `LocalSortType(int hash)` — `(hash & 0x7F) % 6` for the type and bit
    /// 0x80 for the direction (Java ints are sign-extended bytes; for hash
    /// values from `hashCode()` the low 7 bits never exceed the enum length,
    /// but out-of-range raw hashes wrap via modulo — mirrored exactly).
    pub fn from_hash(hash: i32) -> LocalSortType {
        let idx = ((hash & MASK_TYPE) % LOCAL_SORT_KIND_LEN as i32) as usize;
        // Java % can be negative for negative operands; (hash & 0x7F) is
        // always in 0..=127 so idx is non-negative here.
        LocalSortType {
            kind: [
                LocalSortKind::Title,
                LocalSortKind::Date,
                LocalSortKind::PageCount,
                LocalSortKind::Artist,
                LocalSortKind::Group,
                LocalSortKind::Random,
            ][idx],
            descending: (hash & MASK_DESCENDING) != 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_all_variants() {
        for k in [
            LocalSortKind::Title,
            LocalSortKind::Date,
            LocalSortKind::PageCount,
            LocalSortKind::Artist,
            LocalSortKind::Group,
            LocalSortKind::Random,
        ] {
            for desc in [false, true] {
                let st = LocalSortType { kind: k, descending: desc };
                let back = LocalSortType::from_hash(st.hash());
                assert_eq!(back, st);
            }
        }
    }
}
