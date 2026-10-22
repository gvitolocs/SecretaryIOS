//! Monotonic ULIDs, the same scheme as `newTimelineId` in the Node timeline:
//! 48-bit millisecond time + 80-bit random (79 bits drawn, headroom for
//! increments), strictly increasing within the process.

use std::sync::Mutex;

const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
const RANDOM_MASK: u128 = (1u128 << 80) - 1;

struct State {
    last_ms: i64,
    last_random: u128,
}

static STATE: Mutex<State> = Mutex::new(State {
    last_ms: -1,
    last_random: 0,
});

fn encode_time(ms: i64) -> String {
    let mut value = ms.max(0) as u64;
    let mut out = [b'0'; 10];
    for slot in out.iter_mut().rev() {
        *slot = CROCKFORD[(value % 32) as usize];
        value /= 32;
    }
    String::from_utf8(out.to_vec()).expect("ascii")
}

fn encode_random(value: u128) -> String {
    let mut rest = value & RANDOM_MASK;
    let mut out = [b'0'; 16];
    for slot in out.iter_mut().rev() {
        *slot = CROCKFORD[(rest & 31) as usize];
        rest >>= 5;
    }
    String::from_utf8(out.to_vec()).expect("ascii")
}

fn random80() -> u128 {
    let mut buf = [0u8; 10];
    getrandom::fill(&mut buf).expect("os randomness");
    buf.iter().fold(0u128, |acc, b| (acc << 8) | u128::from(*b))
}

/// A new timeline id for `now_ms`.
pub fn new_id(now_ms: i64) -> String {
    let mut st = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if now_ms <= st.last_ms {
        st.last_random = (st.last_random + 1) & RANDOM_MASK;
        if st.last_random == 0 {
            st.last_ms += 1;
        }
    } else {
        st.last_ms = now_ms;
        st.last_random = random80() >> 1;
    }
    format!(
        "{}{}",
        encode_time(st.last_ms),
        encode_random(st.last_random)
    )
}

/// The smallest id after `id` (Crockford base 32 plus one), so an append
/// never sorts before a line another process already wrote. `None` for the
/// largest id (`ZZZ…Z`): nothing sorts after it (it used to wrap around to
/// `000…0`, which sorts first; Verus kernel verification/verus/timeline_ids.rs).
pub fn successor(id: &str) -> Option<String> {
    let mut digits: Vec<u8> = id
        .bytes()
        .map(|b| CROCKFORD.iter().position(|c| *c == b).unwrap_or(0) as u8)
        .collect();
    if digits.iter().all(|d| *d == 31) {
        return None;
    }
    for d in digits.iter_mut().rev() {
        if *d == 31 {
            *d = 0;
        } else {
            *d += 1;
            break;
        }
    }
    Some(
        digits
            .iter()
            .map(|d| CROCKFORD[*d as usize] as char)
            .collect(),
    )
}

/// `^[0-9A-HJKMNP-TV-Z]{26}$`
pub fn is_timeline_id(s: &str) -> bool {
    s.len() == 26
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b.is_ascii_uppercase() && !b"ILOU".contains(&b)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monotonic_and_valid() {
        let mut prev = String::new();
        for _ in 0..1000 {
            let id = new_id(super::super::timeutil::now_ms());
            assert!(is_timeline_id(&id), "{id}");
            assert!(id > prev);
            prev = id;
        }
        assert_eq!(encode_time(1_760_000_000_000), "01K742SG00");
        assert!(!is_timeline_id("01K7BV6Q00ILOU000000000000"));
        assert_eq!(
            successor("01K742SG000000000000000000").unwrap(),
            "01K742SG000000000000000001"
        );
        assert_eq!(
            successor("01K742SG00000000000000000Z").unwrap(),
            "01K742SG000000000000000010"
        );
        assert!(successor(&prev).unwrap() > prev);
        assert_eq!(successor(&"Z".repeat(26)), None);
    }
}
