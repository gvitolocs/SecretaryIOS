//! Frame pacing and poll-rounding math.

use std::time::{Duration, Instant};

/// When frame `k` leaves: `t_start + k * frame_ms` (frame 0 goes out at t_start).
pub fn frame_deadline(t_start: Instant, frame_ms: u64, k: u64) -> Instant {
    t_start + Duration::from_millis(k * frame_ms)
}

/// Byte size of one PCM16 frame of `frame_ms` at 16 kHz s16le mono.
pub fn pcm16_frame_bytes(frame_ms: u64) -> usize {
    (16000 * frame_ms / 1000) as usize * 2
}

/// Byte size of one IMA-ADPCM frame of `frame_ms`: the byte count needed for
/// `rate * frame_ms / 1000` samples at `block` bytes per
/// `2 * (block - 4) + 1` samples, rounded up.
pub fn adpcm_frame_bytes(frame_ms: u64, rate: u32, block: u16) -> usize {
    let samples_per_frame = rate as u64 * frame_ms / 1000;
    let per_block = 2 * (block as u64 - 4) + 1;
    (samples_per_frame * block as u64).div_ceil(per_block) as usize
}

/// Number of frames to send for `total` bytes at `frame_bytes` each.
pub fn frame_count(total: usize, frame_bytes: usize) -> usize {
    if frame_bytes == 0 {
        return 0;
    }
    total.div_ceil(frame_bytes)
}

/// The first multiple of 2000 ms that is >= `reply_ms`: when an old client
/// polling every 2 s would have seen the reply.
pub fn reply_at_2s_poll(reply_ms: u64) -> u64 {
    reply_ms.div_ceil(2000) * 2000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deadlines() {
        let t0 = Instant::now();
        assert_eq!(frame_deadline(t0, 40, 0), t0);
        assert_eq!(frame_deadline(t0, 40, 1), t0 + Duration::from_millis(40));
        assert_eq!(frame_deadline(t0, 40, 3), t0 + Duration::from_millis(120));
        assert_eq!(frame_deadline(t0, 20, 500), t0 + Duration::from_millis(10_000));
    }

    #[test]
    fn frame_sizes() {
        // 40 ms of 16 kHz s16le = 1280 bytes; 20 ms = 640.
        assert_eq!(pcm16_frame_bytes(40), 1280);
        assert_eq!(pcm16_frame_bytes(20), 640);
        // IMA: Fortuna-style 10909 Hz, 256-byte blocks (505 samples each):
        // 40 ms = 436 samples -> ceil(436 * 256 / 505) = 222 bytes.
        assert_eq!(adpcm_frame_bytes(40, 10909, 256), 222);
        // 100 ms = 1090 samples -> ceil(1090 * 256 / 505) = 553.
        assert_eq!(adpcm_frame_bytes(100, 10909, 256), 553);
    }

    #[test]
    fn counts() {
        assert_eq!(frame_count(10_000, 1280), 8); // 7 full + 400
        assert_eq!(frame_count(1280, 1280), 1);
        assert_eq!(frame_count(1279, 1280), 1);
        assert_eq!(frame_count(0, 1280), 0);
    }

    #[test]
    fn poll_rounding() {
        assert_eq!(reply_at_2s_poll(0), 0);
        assert_eq!(reply_at_2s_poll(1999), 2000);
        assert_eq!(reply_at_2s_poll(2000), 2000);
        assert_eq!(reply_at_2s_poll(2001), 4000);
        assert_eq!(reply_at_2s_poll(2500), 4000);
        assert_eq!(reply_at_2s_poll(5999), 6000);
    }
}
