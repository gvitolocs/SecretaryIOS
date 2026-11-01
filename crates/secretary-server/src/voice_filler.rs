//! Filler phrases for slow voice turns (protocol section 6).
//!
//! When Lucina says a turn will take a slow path (`ack` event), the speaker
//! should not sit silent: a short phrase ("Un attimo…") goes out at once, from
//! memory, and the real answer queues behind it. The phrases are synthesized
//! with Kokoro (`pcm`, 24 kHz) in the background at startup
//! ([`spawn_prewarm`]) and again, lazily, when an `ack` finds the cache empty
//! (Kokoro was down at startup): that turn then speaks the ack text like an
//! ordinary sentence and a rebuild starts for the next one.
//!
//! The cache is keyed by Kokoro voice (`if_sara` for Italian, `af_heart` for
//! English) and bounded: [`MAX_PHRASE_BYTES`] per phrase, [`MAX_TOTAL_BYTES`]
//! in all (a few hundred KB in practice: about 1 s of 24 kHz s16le is 48 KB).
//! [`Fillers::pick`] rotates through a language's phrases, so the speaker does
//! not say the same thing every time.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

use axum::body::Bytes;
use futures_util::StreamExt;

use crate::tts::{self, Format, Tts};
use crate::AppState;

/// The phrases, per language (`true`: Italian). Kokoro reads the ellipsis as a
/// short falling pause.
pub const PHRASES_IT: &[&str] = &["Un attimo…", "Vediamo…"];
pub const PHRASES_EN: &[&str] = &["One moment…", "Let me see…"];

/// A synthesized phrase longer than this (about 5 s) is not a filler.
pub const MAX_PHRASE_BYTES: usize = 256 * 1024;
/// The whole cache never holds more than this.
pub const MAX_TOTAL_BYTES: usize = 1024 * 1024;

/// The Kokoro voice for a language, as the pipeline picks it for `lang`.
pub fn voice_for(italian: bool) -> &'static str {
    if italian {
        tts::ITALIAN_VOICE
    } else {
        tts::ENGLISH_VOICE
    }
}

fn phrases(italian: bool) -> &'static [&'static str] {
    if italian {
        PHRASES_IT
    } else {
        PHRASES_EN
    }
}

/// The in-memory filler audio.
#[derive(Default)]
pub struct Fillers {
    /// voice → (phrase, PCM) in phrase order.
    cache: Mutex<HashMap<String, Vec<(&'static str, Bytes)>>>,
    /// A build is running: a second one would only repeat its Kokoro calls.
    building: AtomicBool,
    /// Rotation counter for [`Fillers::pick`].
    next: AtomicUsize,
}

/// A filler ready to play.
#[derive(Debug, Clone)]
pub struct Filler {
    pub voice: &'static str,
    pub phrase: &'static str,
    pub pcm: Bytes,
}

impl Fillers {
    pub fn new() -> Self {
        Self::default()
    }

    fn total_bytes(map: &HashMap<String, Vec<(&'static str, Bytes)>>) -> usize {
        map.values().flatten().map(|(_, b)| b.len()).sum()
    }

    /// Phrases cached for a language.
    pub fn count(&self, italian: bool) -> usize {
        self.cache
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(voice_for(italian))
            .map_or(0, Vec::len)
    }

    /// Bytes of PCM held.
    pub fn bytes(&self) -> usize {
        Self::total_bytes(&self.cache.lock().unwrap_or_else(|p| p.into_inner()))
    }

    /// The next cached phrase for the language, rotating; `None` when the
    /// cache holds none for it.
    pub fn pick(&self, italian: bool) -> Option<Filler> {
        let voice = voice_for(italian);
        let cache = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        let list = cache.get(voice).filter(|l| !l.is_empty())?;
        let n = self.next.fetch_add(1, Ordering::Relaxed);
        let (phrase, pcm) = &list[n % list.len()];
        Some(Filler {
            voice,
            phrase,
            pcm: pcm.clone(),
        })
    }

    fn store(&self, voice: &str, phrase: &'static str, pcm: Bytes) {
        let mut cache = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        if Self::total_bytes(&cache) + pcm.len() > MAX_TOTAL_BYTES {
            tracing::warn!(voice, phrase, "voice filler cache is full; phrase dropped");
            return;
        }
        let list = cache.entry(voice.to_owned()).or_default();
        if !list.iter().any(|(p, _)| *p == phrase) {
            list.push((phrase, pcm));
        }
    }

    fn has(&self, voice: &str, phrase: &str) -> bool {
        self.cache
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(voice)
            .is_some_and(|l| l.iter().any(|(p, _)| *p == phrase))
    }

    /// Synthesize every phrase the cache lacks. Returns how many are cached
    /// afterwards. A second call while one runs returns at once. For a voice,
    /// the first Kokoro failure stops that voice (Kokoro is down; the next
    /// `ack` retries).
    pub async fn prewarm(&self, tts: &Tts) -> usize {
        if self
            .building
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return self.count(true) + self.count(false);
        }
        for italian in [true, false] {
            let voice = voice_for(italian);
            for &phrase in phrases(italian) {
                if self.has(voice, phrase) {
                    continue;
                }
                match synth(tts, voice, phrase).await {
                    Ok(pcm) => self.store(voice, phrase, pcm),
                    Err(why) => {
                        tracing::warn!(voice, %why, "voice filler synthesis failed");
                        break;
                    }
                }
            }
        }
        self.building.store(false, Ordering::SeqCst);
        let n = self.count(true) + self.count(false);
        tracing::info!(phrases = n, bytes = self.bytes(), "voice fillers ready");
        n
    }
}

/// One phrase as PCM, within [`MAX_PHRASE_BYTES`].
async fn synth(tts: &Tts, voice: &str, phrase: &str) -> Result<Bytes, String> {
    let mut stream = tts
        .synthesize(phrase, voice, 1.0, Format::Pcm)
        .await
        .map_err(|e| e.message)?;
    let mut pcm = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| e.to_string())?;
        if pcm.len() + chunk.len() > MAX_PHRASE_BYTES {
            return Err("longer than a filler may be".into());
        }
        pcm.extend_from_slice(&chunk);
    }
    pcm.truncate(pcm.len() & !1); // whole samples
    if pcm.is_empty() {
        return Err("no audio".into());
    }
    Ok(Bytes::from(pcm))
}

/// Build the cache in the background (startup). Never blocks, never fails.
pub fn spawn_prewarm(st: &AppState) {
    let st = st.clone();
    tokio::spawn(async move {
        st.fillers.prewarm(&st.tts).await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pick_rotates_and_is_per_language() {
        let f = Fillers::new();
        assert!(f.pick(true).is_none() && f.pick(false).is_none());
        f.store(
            tts::ITALIAN_VOICE,
            "Un attimo…",
            Bytes::from_static(&[1, 0]),
        );
        f.store(tts::ITALIAN_VOICE, "Vediamo…", Bytes::from_static(&[2, 0]));
        f.store(
            tts::ENGLISH_VOICE,
            "One moment…",
            Bytes::from_static(&[3, 0]),
        );
        assert!(f.pick(false).is_some_and(|x| x.phrase == "One moment…"));
        let a = f.pick(true).unwrap();
        let b = f.pick(true).unwrap();
        assert_ne!(a.phrase, b.phrase, "rotates");
        assert_eq!(a.voice, "if_sara");
        // storing the same phrase twice keeps one copy
        f.store(
            tts::ITALIAN_VOICE,
            "Un attimo…",
            Bytes::from_static(&[9, 0]),
        );
        assert_eq!(f.count(true), 2);
    }

    #[test]
    fn the_cache_is_bounded() {
        let f = Fillers::new();
        let big = Bytes::from(vec![0u8; MAX_TOTAL_BYTES - 10]);
        f.store(tts::ITALIAN_VOICE, "Un attimo…", big);
        f.store(tts::ITALIAN_VOICE, "Vediamo…", Bytes::from(vec![0u8; 100]));
        assert_eq!(f.count(true), 1);
        assert!(f.bytes() <= MAX_TOTAL_BYTES);
    }
}
