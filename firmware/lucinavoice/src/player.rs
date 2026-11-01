//! The speaker task: everything audible goes through [`PLAY`], so earcons
//! never wait for the network and the answer plays while it streams in.
//!
//! - `Pcm` chunks (s16le mono, 24 kHz) start or continue a playback; a gap
//!   longer than [`IDLE`] ends it (the DMA would run dry anyway).
//! - `Stop` lets the queued audio play out; `Flush` cuts it now (barge-in).
//! - `Beep` ends any playback first, then sounds.

use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Timer};

use crate::audio::Speaker;

/// Bytes per PCM chunk (about 21 ms at 24 kHz).
pub const CHUNK: usize = 1024;
/// A playback with no new audio for this long is finished.
const IDLE: Duration = Duration::from_millis(400);

#[allow(
    clippy::large_enum_variant,
    reason = "fixed-size channel slots keep audio off the heap"
)]
pub enum Play {
    Beep { freq: u32, ms: u32 },
    Pcm([u8; CHUNK], usize),
    Stop,
    Flush,
}

pub static PLAY: Channel<CriticalSectionRawMutex, Play, 6> = Channel::new();

/// Earcons: press (listening), release (heard you), error.
pub async fn listening() {
    PLAY.send(Play::Beep { freq: 880, ms: 60 }).await;
}

pub async fn ack() {
    PLAY.send(Play::Beep { freq: 1320, ms: 25 }).await;
}

pub async fn error() {
    PLAY.send(Play::Beep { freq: 330, ms: 120 }).await;
    PLAY.send(Play::Beep { freq: 220, ms: 180 }).await;
}

#[embassy_executor::task]
pub async fn player_task(mut speaker: Speaker) {
    let mut next: Option<Play> = None;
    loop {
        let cmd = match next.take() {
            Some(c) => c,
            None => PLAY.receive().await,
        };
        match cmd {
            Play::Beep { freq, ms } => speaker.beep(freq, ms).await,
            Play::Stop | Play::Flush => {}
            Play::Pcm(buf, n) => {
                let mut playback = speaker.start();
                playback.push(&buf[..n]).await;
                let flush = loop {
                    match select(PLAY.receive(), Timer::after(IDLE)).await {
                        Either::First(Play::Pcm(buf, n)) => playback.push(&buf[..n]).await,
                        Either::First(Play::Stop) | Either::Second(()) => break false,
                        Either::First(Play::Flush) => break true,
                        Either::First(beep @ Play::Beep { .. }) => {
                            next = Some(beep);
                            break false;
                        }
                    }
                };
                if flush {
                    // Dropping the playback stops the DMA at once; queued chunks go too.
                    drop(playback);
                    while let Ok(p) = PLAY.try_receive() {
                        if let Play::Beep { .. } = p {
                            next = Some(p);
                        }
                    }
                } else {
                    let restarts = playback.restarts;
                    playback.finish().await;
                    if restarts > 0 {
                        log::info!("player: {restarts} underruns");
                    }
                }
            }
        }
    }
}
