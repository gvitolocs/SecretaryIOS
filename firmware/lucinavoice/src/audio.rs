//! I2S audio. Proposed wiring inside the Echo shell:
//!
//! | part | signal | GPIO |
//! |---|---|---|
//! | MAX98357A amp (I2S0, out) | BCLK / LRC / DIN | 26 / 25 / 22 |
//! | INMP441 mic (I2S1, in) | SCK / WS / SD | 32 / 33 / 34 |
//!
//! The mic's L/R pin may go either way: the louder slot is picked per
//! recording. Speaker frames are stereo with the same sample twice (the
//! MAX98357A mixes L+R with SD_MODE floating).
//!
//! Both directions stream through DMA ring descriptors. When the network
//! falls behind the speaker, the DMA runs dry and stops; the next
//! [`Playback::push`] restarts it (a short gap, not a crash). When the
//! uploader falls behind the mic, the DMA fills up and stops;
//! [`Recording::read`] reports [`Overrun`] and the turn ends there.

use esp_hal::Async;
use esp_hal::dma::{DmaRxStreamBuf, DmaTxStreamBuf};
use esp_hal::i2s::master::{I2sRx, I2sRxDmaTransfer, I2sTx, I2sTxDmaTransfer};
use lucinavoice_core::pcm;

/// Speaker sample rate: the rate `/v1/voice` answers with.
pub const SPEAKER_RATE: u32 = 24_000;

/// The microphone DMA ring filled up before it was read.
#[derive(Debug)]
pub struct Overrun;

pub struct Speaker {
    tx: Option<I2sTx<'static, Async>>,
    buf: Option<DmaTxStreamBuf>,
}

type TxTransfer = I2sTxDmaTransfer<'static, Async, DmaTxStreamBuf>;

impl Speaker {
    pub fn new(tx: I2sTx<'static, Async>, buf: DmaTxStreamBuf) -> Self {
        Self {
            tx: Some(tx),
            buf: Some(buf),
        }
    }

    pub fn start(&mut self) -> Playback<'_> {
        Playback {
            speaker: self,
            transfer: None,
            restarts: 0,
        }
    }

    /// A square-wave beep.
    pub async fn beep(&mut self, freq: u32, ms: u32) {
        let mut playback = self.start();
        let mut phase = 0;
        let mut frames = [0u8; 960];
        let mut left = SPEAKER_RATE * ms / 1000 * 4;
        while left > 0 {
            let n = pcm::tone(freq, SPEAKER_RATE, 6000, &mut phase, &mut frames).min(left as usize);
            playback.push_stereo(&frames[..n]).await;
            left -= n as u32;
        }
        playback.finish().await;
    }
}

pub struct Playback<'a> {
    speaker: &'a mut Speaker,
    transfer: Option<TxTransfer>,
    /// How often the DMA ran dry and was restarted.
    pub restarts: u32,
}

impl Playback<'_> {
    /// Plays s16le mono samples (an odd trailing byte is the caller's to keep).
    pub async fn push(&mut self, mono: &[u8]) {
        let mut stereo = [0u8; 1024];
        for part in mono.chunks(stereo.len() / 2) {
            let n = pcm::mono_to_stereo(part, &mut stereo);
            self.push_stereo(&stereo[..n]).await;
        }
    }

    async fn push_stereo(&mut self, mut frames: &[u8]) {
        while !frames.is_empty() {
            let transfer = match self.transfer.as_mut() {
                Some(t) if !t.is_done() => t,
                Some(_) => {
                    // Ran dry: hand the buffer back and start again.
                    self.restarts += 1;
                    self.stop();
                    self.begin(&mut frames);
                    continue;
                }
                None => {
                    self.begin(&mut frames);
                    continue;
                }
            };
            let n = transfer.push(frames);
            frames = &frames[n..];
            if n == 0 && transfer.wait_for_available_async().await.is_err() {
                // DMA error: the next round restarts.
                self.stop();
            }
        }
    }

    /// Fills the idle buffer from `frames` (silence for the rest) and starts the DMA.
    fn begin(&mut self, frames: &mut &[u8]) {
        let (Some(tx), Some(mut buf)) = (self.speaker.tx.take(), self.speaker.buf.take()) else {
            return;
        };
        let n = buf.push(frames);
        *frames = &frames[n..];
        while buf.push(&[0u8; 64]) > 0 {}
        match tx.write(buf) {
            Ok(t) => self.transfer = Some(t),
            Err((e, tx, buf)) => {
                log::warn!("speaker: DMA start failed: {e:?}");
                self.speaker.tx = Some(tx);
                self.speaker.buf = Some(buf);
                *frames = &[];
            }
        }
    }

    fn stop(&mut self) {
        if let Some(t) = self.transfer.take() {
            let (tx, buf) = t.stop();
            self.speaker.tx = Some(tx);
            self.speaker.buf = Some(buf);
        }
    }

    /// Lets the queued audio play out (plus a little silence), then stops.
    pub async fn finish(mut self) {
        if self.transfer.is_some() {
            let silence = [0u8; 512];
            // One buffer of silence pushes the last real samples through.
            for _ in 0..24 {
                self.push_stereo(&silence).await;
            }
        }
        self.stop();
    }
}

impl Drop for Playback<'_> {
    fn drop(&mut self) {
        self.stop();
    }
}

pub struct Mic {
    rx: Option<I2sRx<'static, Async>>,
    buf: Option<DmaRxStreamBuf>,
}

type RxTransfer = I2sRxDmaTransfer<'static, Async, DmaRxStreamBuf>;

impl Mic {
    pub fn new(rx: I2sRx<'static, Async>, buf: DmaRxStreamBuf) -> Self {
        Self {
            rx: Some(rx),
            buf: Some(buf),
        }
    }

    pub fn start(&mut self) -> Option<Recording<'_>> {
        let (rx, buf) = (self.rx.take()?, self.buf.take()?);
        match rx.read(buf) {
            Ok(transfer) => Some(Recording {
                mic: self,
                transfer: Some(transfer),
                slot: None,
            }),
            Err((e, rx, buf)) => {
                log::warn!("mic: DMA start failed: {e:?}");
                self.rx = Some(rx);
                self.buf = Some(buf);
                None
            }
        }
    }
}

pub struct Recording<'a> {
    mic: &'a mut Mic,
    transfer: Option<RxTransfer>,
    slot: Option<usize>,
}

impl Recording<'_> {
    /// Waits for samples and writes them to `out` as s16le mono (16 kHz).
    /// `out` should hold at least 256 bytes.
    pub async fn read(&mut self, out: &mut [u8]) -> Result<usize, Overrun> {
        let transfer = self.transfer.as_mut().ok_or(Overrun)?;
        if transfer.is_done() {
            return Err(Overrun);
        }
        transfer
            .wait_for_available_async()
            .await
            .map_err(|_| Overrun)?;
        // 8 bytes of stereo 32-bit words per mono output sample (2 bytes).
        let mut words = [0u8; 2048];
        let want = (out.len() * 4).min(words.len());
        let available = transfer.available_bytes().min(want) & !7;
        let n = transfer.pop(&mut words[..available]);
        let words = &words[..n & !7];
        let slot = *self.slot.get_or_insert_with(|| pcm::louder_slot(words));
        Ok(pcm::mic_to_s16le(words, slot, out))
    }
}

impl Drop for Recording<'_> {
    fn drop(&mut self) {
        if let Some(t) = self.transfer.take() {
            let (rx, buf) = t.stop();
            self.mic.rx = Some(rx);
            self.mic.buf = Some(buf);
        }
    }
}
