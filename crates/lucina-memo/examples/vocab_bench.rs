//! Voice Learn accuracy bench (docs/lucinavoice-protocol.md section 8, "Accuracy check").
//!
//! Generates clips with Kokoro (`if_sara` for Italian, `af_heart` for English; Kokoro has no
//! Danish voice, so the Danish clip is the English voice reading Danish words), then
//! transcribes each one through the real engines of this crate:
//!
//! - **engine**: the prompter's whisper-server (small), the engine behind `POST /v1/memos/stt`
//!   (voice turns): [`Engine::transcribe`];
//! - **worker**: `whisper-cli` large-v3, the function the memo worker, the iPhone-transcript
//!   upgrade, meetings and chat voice notes use: [`lucina_memo::transcribe`].
//!
//! Per clip and path it runs: no vocabulary; the vocabulary as prompt only; the real pipeline
//! (prompt + post-correction). "Correction only" is the post-correction applied to the
//! no-vocabulary text. The vocabulary is a temporary file, never the real one.
//!
//! ```text
//! cargo run -p lucina-memo --example vocab_bench --release
//! ```
//! Environment: `KOKORO_URL` (default `http://127.0.0.1:8880`), `WHISPER_BIN_DIR`
//! (default `$HOME/Projects/AI/whisper.cpp/build/bin`), `BENCH_PORT` (default 28798),
//! `BENCH_CLIPS_DIR` (keep and reuse the generated clips).
//! The GPU comes from `gpu-lease`, as in the service; nothing is left running.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use lucina_memo::prompter::engine::{Engine, EngineConfig};
use lucina_memo::transcribe::{self, WhisperConfig};
use lucina_vocab::{Entry, Vocab};

struct Clip {
    id: &'static str,
    voice: &'static str,
    /// Language handed to the engine (`auto` for the Danish clip).
    lang: &'static str,
    text: &'static str,
    /// Canonical words the clip says (empty = control clip).
    expect: &'static [&'static str],
    long: bool,
}

const SK: &str = "Skibigårdsvej";
const HO: &str = "Horus";

fn clips() -> Vec<Clip> {
    let c = |id, voice, lang, text, expect: &'static [&'static str], long| Clip {
        id,
        voice,
        lang,
        text,
        expect,
        long,
    };
    vec![
        c("s1-it", "if_sara", "it", "Sto tornando a Skibigårdsvej.", &[SK], false),
        c("s2-it", "if_sara", "it", "Ricordami di comprare il latte quando arrivo a Skibigårdsvej.", &[SK], false),
        c("s3-it", "if_sara", "it", "Horus è il mio progetto.", &[HO], false),
        c("s4-it", "if_sara", "it", "Come sta andando Horus oggi?", &[HO], false),
        c("s5-en", "af_heart", "en", "Can you check Horus status?", &[HO], false),
        c("s6-en", "af_heart", "en", "Remind me to call my mother when I get to Skibigårdsvej.", &[SK], false),
        c("s7-en", "af_heart", "en", "Navigate to Skibigårdsvej and tell me when Horus finishes the build.", &[SK, HO], false),
        c("s8-da*", "af_heart", "auto", "Jeg bor på Skibigårdsvej, og Horus kører på serveren.", &[SK, HO], false),
        c("c1-it", "if_sara", "it", "Che ore sono adesso?", &[], false),
        c("c2-en", "af_heart", "en", "What time is it in Aarhus right now, and how many hours until Friday?", &[], false),
        c("c3-it", "if_sara", "it", "Aggiungi pane e latte alla lista della spesa e ricordami di chiamare Marco.", &[], false),
        c("l1-it", "if_sara", "it", "Allora, appunti per domani. Devo passare a Skibigårdsvej per ritirare il pacco, poi ho una riunione su Horus alle undici. Ricordati di scrivere a Marco per il preventivo e di comprare il latte e il pane prima di tornare a casa. Se Horus non compila, guarda i log del server.", &[SK, HO, HO], true),
        c("l2-en", "af_heart", "en", "Quick note for myself. The Horus build failed again last night, so I need to look at the logs first thing in the morning. After lunch I will drive to Skibigårdsvej to meet the electrician, and then pick up the groceries on the way back.", &[HO, SK], true),
        c("l3-it", "if_sara", "it", "Promemoria vocale. Domani mattina controllo lo stato di Horus, poi vado a Skibigårdsvej. Se trovo traffico ti avviso. Ah, e non dimenticare di chiamare il dentista.", &[HO, SK], true),
        c("l4-en", "af_heart", "en", "Voice note. Tomorrow I need to buy a birthday present, call the bank about the card, and book a table for Friday evening.", &[], true),
    ]
}

fn vocab() -> Vocab {
    let mut v = Vocab::default();
    for (i, (w, sounds, meaning)) in [
        (SK, Vec::<&str>::new(), "the owner's home street"),
        (HO, vec![], "the owner's project"),
    ]
    .into_iter()
    .enumerate()
    {
        let mut e = Entry::new(w);
        e.id = format!("bench{i}");
        e.sounds_like = sounds.into_iter().map(String::from).collect();
        e.meaning = Some(meaning.into());
        e.created_at = "2026-11-01T10:00:00Z".into();
        e.updated_at = format!("2026-11-01T10:0{i}:00Z");
        v.add(e).expect("bench entry");
    }
    v
}

/// Whole-word, case-insensitive containment (diacritics must match).
fn has_word(text: &str, word: &str) -> bool {
    let w = word.to_lowercase();
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .any(|t| t == w)
}

async fn kokoro(url: &str, clip: &Clip, out: &Path) -> anyhow::Result<()> {
    let body = serde_json::json!({
        "model": "kokoro", "input": clip.text, "voice": clip.voice,
        "response_format": "wav", "speed": 1.0
    });
    let r = reqwest::Client::new()
        .post(format!("{url}/v1/audio/speech"))
        .json(&body)
        .timeout(Duration::from_secs(120))
        .send()
        .await?;
    anyhow::ensure!(r.status().is_success(), "kokoro answered {}", r.status());
    std::fs::write(out, r.bytes().await?)?;
    Ok(())
}

/// The samples of a 16 kHz mono 16-bit WAV (the `data` chunk).
fn samples(wav: &[u8]) -> Vec<i16> {
    let mut i = 12;
    while i + 8 <= wav.len() {
        let len = u32::from_le_bytes(wav[i + 4..i + 8].try_into().unwrap()) as usize;
        if &wav[i..i + 4] == b"data" {
            let end = (i + 8 + len).min(wav.len());
            return wav[i + 8..end]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| i16::from_le_bytes(*c))
                .collect();
        }
        i += 8 + len + (len & 1);
    }
    Vec::new()
}

#[derive(Default, Clone)]
struct Run {
    text: String,
    /// `to` of each correction made.
    corrected: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Phase {
    None,
    PromptOnly,
    Full,
}

struct Results {
    /// (path, clip id, phase) -> run
    runs: BTreeMap<(String, String, Phase), Run>,
}

fn set_vocab_file(path: &Path, v: &Vocab) {
    v.save(path).expect("write the bench vocabulary");
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let home = std::env::var("HOME")?;
    let kokoro_url = std::env::var("KOKORO_URL").unwrap_or_else(|_| "http://127.0.0.1:8880".into());
    let bin = std::env::var("WHISPER_BIN_DIR")
        .unwrap_or_else(|_| format!("{home}/Projects/AI/whisper.cpp/build/bin"));
    let port = std::env::var("BENCH_PORT").unwrap_or_else(|_| "28798".into());
    let dir: PathBuf = std::env::temp_dir().join(format!("vocab-bench-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let vocab_file = dir.join("voice-learn.json");
    let real = vocab();
    let empty = Vocab::default();

    let env = |k: &str| -> Option<String> {
        match k {
            "HOME" => Some(home.clone()),
            "LUCINA_MEMO_DIR" => Some(dir.join("data").display().to_string()),
            "LUCINA_VOICE_LEARN_FILE" => Some(vocab_file.display().to_string()),
            "LUCINA_MEMO_WHISPER_CLI" => Some(format!("{bin}/whisper-cli")),
            "LUCINA_PROMPTER_WHISPER_SERVER" => Some(format!("{bin}/whisper-server")),
            "LUCINA_PROMPTER_STT_PORT" => Some(port.clone()),
            "LUCINA_MEMO_LANGUAGE" => None,
            _ => None,
        }
    };
    let whisper = WhisperConfig::from_lookup(env);
    let engine = Engine::new(EngineConfig::from_lookup(env, &whisper));

    // 1. Clips: Kokoro -> 16 kHz mono WAV. `BENCH_CLIPS_DIR` keeps them between runs.
    let cache = std::env::var("BENCH_CLIPS_DIR").ok().map(PathBuf::from);
    if let Some(c) = &cache {
        std::fs::create_dir_all(c)?;
    }
    let clips = clips();
    let mut wavs: BTreeMap<&str, (PathBuf, f64)> = BTreeMap::new();
    for c in &clips {
        let wav = cache.as_ref().unwrap_or(&dir).join(format!("{}.wav", c.id));
        let dur = if wav.exists() {
            transcribe::duration(&whisper, &wav).await?
        } else {
            let raw = dir.join(format!("{}.kokoro.wav", c.id));
            kokoro(&kokoro_url, c, &raw).await?;
            transcribe::decode(&whisper, &raw, &wav).await?
        };
        eprintln!("clip {} {dur:.1}s", c.id);
        wavs.insert(c.id, (wav, dur));
    }

    let mut res = Results {
        runs: BTreeMap::new(),
    };
    let prompt_words = real.prompt(500);
    let lease_wait = Duration::from_secs(600);

    for phase in [Phase::None, Phase::PromptOnly, Phase::Full] {
        // The vocabulary file is what switches the pipeline on; the bench rewrites it between
        // phases (the services re-read it when it changes).
        set_vocab_file(
            &vocab_file,
            if phase == Phase::Full { &real } else { &empty },
        );
        // 2. engine (whisper-server small): short clips only.
        let mode = engine.acquire().await.map_err(anyhow::Error::msg)?;
        eprintln!("engine phase {phase:?} ({mode:?})");
        for c in clips.iter().filter(|c| !c.long) {
            let pcm = samples(&std::fs::read(&wavs[c.id].0)?);
            let base = match phase {
                Phase::PromptOnly => format!("Lucina, {prompt_words}"),
                _ => "Lucina".into(),
            };
            let out = engine
                .transcribe(&pcm, c.lang, &base)
                .await
                .map_err(anyhow::Error::msg)?;
            res.runs.insert(
                ("engine".into(), c.id.into(), phase),
                Run {
                    text: out.text,
                    corrected: out.corrections.into_iter().map(|x| x.1).collect(),
                },
            );
        }
        engine.release().await;
        // 3. worker (whisper-cli large-v3): every clip.
        eprintln!("worker phase {phase:?}");
        for c in &clips {
            let (wav, dur) = &wavs[c.id];
            let work = dir.join(format!("work-{}-{phase:?}", c.id));
            std::fs::create_dir_all(&work)?;
            let prompt = (phase == Phase::PromptOnly).then_some(prompt_words.as_str());
            let t = match prompt {
                Some(p) => {
                    transcribe::transcribe_prompted(&whisper, wav, &work, *dur, lease_wait, p)
                        .await?
                }
                None => transcribe::transcribe(&whisper, wav, &work, *dur, lease_wait).await?,
            };
            res.runs.insert(
                ("worker".into(), c.id.into(), phase),
                Run {
                    text: t.text(),
                    corrected: t.corrections.into_iter().map(|x| x.to).collect(),
                },
            );
        }
    }
    engine.shutdown().await;

    // 4. Report.
    println!("\nVoice Learn accuracy bench — vocabulary: {SK}, {HO}");
    println!("Kokoro voices: if_sara (it), af_heart (en). *s8 is Danish words read by the English voice (no Danish Kokoro voice).\n");
    for (path, long) in [("engine", false), ("worker", false), ("worker", true)] {
        let label = match (path, long) {
            ("engine", _) => "engine (whisper-server small; voice turns, /v1/memos/stt)",
            (_, false) => "worker (whisper-cli large-v3), short clips",
            _ => "worker (whisper-cli large-v3), voice-note clips",
        };
        let set: Vec<&Clip> = clips.iter().filter(|c| c.long == long).collect();
        let total: usize = set.iter().map(|c| c.expect.len()).sum();
        let hits = |f: &dyn Fn(&Clip) -> String| -> usize {
            set.iter()
                .map(|c| c.expect.iter().filter(|w| has_word(&f(c), w)).count())
                .sum()
        };
        let none = |c: &Clip| {
            res.runs[&(path.into(), c.id.into(), Phase::None)]
                .text
                .clone()
        };
        let prompt = |c: &Clip| {
            res.runs[&(path.into(), c.id.into(), Phase::PromptOnly)]
                .text
                .clone()
        };
        let full = |c: &Clip| {
            res.runs[&(path.into(), c.id.into(), Phase::Full)]
                .text
                .clone()
        };
        let corr_only = |c: &Clip| real.correct(&none(c)).text;
        // False corrections: a correction to a word the clip does not say. False insertions: a
        // vocabulary word in the output of a clip that does not say it.
        let false_corr: usize = set
            .iter()
            .map(|c| {
                res.runs[&(path.into(), c.id.into(), Phase::Full)]
                    .corrected
                    .iter()
                    .filter(|to| !c.expect.contains(&to.as_str()))
                    .count()
            })
            .sum();
        let false_ins = |f: &dyn Fn(&Clip) -> String| -> usize {
            set.iter()
                .map(|c| {
                    [SK, HO]
                        .iter()
                        .filter(|w| !c.expect.contains(w) && has_word(&f(c), w))
                        .count()
                })
                .sum()
        };
        println!("{label}: {} clips, {total} vocabulary words", set.len());
        println!("| none | correction only | prompt only | prompt+correction | false corrections | false words (none/prompt/full) | reported ambiguous |");
        println!("|---|---|---|---|---|---|---|");
        let pct = |n: usize| {
            format!(
                "{n}/{total} ({:.0}%)",
                100.0 * n as f64 / total.max(1) as f64
            )
        };
        println!(
            "| {} | {} | {} | {} | {} | {}/{}/{} | {} |\n",
            pct(hits(&none)),
            pct(hits(&corr_only)),
            pct(hits(&prompt)),
            pct(hits(&full)),
            false_corr,
            false_ins(&none),
            false_ins(&prompt),
            false_ins(&full),
            // Spans the pipeline left alone and returned for a "did they mean" check.
            set.iter()
                .map(|c| real.correct(&prompt(c)).ambiguous.len())
                .sum::<usize>(),
        );
    }
    println!("Per clip (heard without the vocabulary -> with prompt+correction):");
    for path in ["engine", "worker"] {
        for c in &clips {
            let Some(n) = res
                .runs
                .get(&(path.to_string(), c.id.to_string(), Phase::None))
            else {
                continue;
            };
            let f = &res.runs[&(path.to_string(), c.id.to_string(), Phase::Full)];
            let p = &res.runs[&(path.to_string(), c.id.to_string(), Phase::PromptOnly)];
            println!("[{path} {}] said: {}", c.id, c.text);
            println!("    none:   {}", n.text.trim());
            println!("    prompt: {}", p.text.trim());
            println!(
                "    full:   {}  (corrections to: {:?})",
                f.text.trim(),
                f.corrected
            );
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
