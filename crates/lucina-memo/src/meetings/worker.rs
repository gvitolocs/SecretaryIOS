//! The meetings pipeline: one meeting at a time, oldest first, beside the memo worker.
//! Every step writes its output (in the meeting's `.work/`) before `meeting.json` records it,
//! and a step whose output is recorded is skipped, so a crash or restart continues.
//!
//! 1. `transcribing`: per audio track (`mic.m4a` = the owner; `system.m4a`, else the video's
//!    audio = everyone else) ffmpeg → 16 kHz WAV → Whisper large-v3 with word timings under
//!    `gpu-lease` (busy: retry in a minute, no attempt counted).
//! 2. `diarizing`: the system track into clusters ([`crate::diarize`]); the owner's voice
//!    profile from the mic; each cluster matched to the voice profiles (or the owner); the two
//!    timelines merged ([`super::timeline`]).
//! 3. `analyzing`: roles and utterance kinds ([`super::analyze`]), then the brief
//!    ([`super::brief`]); the model unreachable on the last attempt → deterministic versions.
//! 4. `publishing`: the files in the meeting folder, the Obsidian note (path recorded before the
//!    push), the memory entry and the Telegram message (MK8 markers first), the media library,
//!    the notification; unknown recurring voices are asked about once (MK6).
//! 5. `done` only with every output (MK4).
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use serde::{Deserialize, Serialize};

use super::analyze;
use super::brief;
use super::kernel;
use super::model::{Brief, Meeting, MeetingTranscript, Speaker, Status, Utterance, Voices};
use super::store::MeetingStore;
use super::timeline;
use crate::diarize::profiles::{Identity, Sighting};
use crate::diarize::{cosine, wav, DiarizeError, Diarization};
use crate::model::Transcript;
use crate::store::{read_json, write_atomic, write_json};
use crate::transcribe::SttError;
use crate::App;

/// `.work/timeline.json` and `.work/analysis.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Labelled {
    pub speakers: Vec<Speaker>,
    pub utterances: Vec<Utterance>,
}

pub async fn run(app: Arc<App>) {
    loop {
        match next_due(&app) {
            Some(id) => {
                if let Err(e) = process(&app, &id).await {
                    tracing::warn!(%id, error = %e, "meeting pipeline step failed");
                }
            }
            None => {
                tokio::select! {
                    _ = app.meetings_wake.notified() => {}
                    _ = tokio::time::sleep(Duration::from_secs(30)) => {}
                }
            }
        }
    }
}

fn next_due(app: &App) -> Option<String> {
    let store = app.meetings().ok()?;
    let now = Utc::now();
    let mut due: Vec<Meeting> = store
        .list()
        .into_iter()
        .filter(|m| m.status.is_pipeline() && m.next_attempt_at.is_none_or(|t| t <= now))
        .collect();
    due.sort_by_key(|m| m.created_at);
    due.into_iter().next().map(|m| m.id)
}

pub(crate) enum StepError {
    /// Retry later without counting an attempt (GPU busy).
    Later(Duration, String),
    Failed(String),
}

impl<E: std::fmt::Display> From<E> for StepError {
    fn from(e: E) -> Self {
        StepError::Failed(e.to_string())
    }
}

/// Runs the remaining steps of one meeting and records the outcome.
pub async fn process(app: &App, id: &str) -> Result<(), String> {
    let store = app.meetings().map_err(|e| e.to_string())?;
    let result = steps(app, &store, id).await;
    let max = app.cfg.max_attempts;
    match result {
        Ok(()) => Ok(()),
        Err(StepError::Later(wait, why)) => {
            let _ = store.update(id, |m| {
                m.status = Status::Queued;
                m.error = Some(why.clone());
                m.next_attempt_at = Some(Utc::now() + chrono::Duration::from_std(wait).unwrap_or_default());
            });
            Err(why)
        }
        Err(StepError::Failed(why)) => {
            let _ = store.update(id, |m| {
                m.attempts += 1;
                m.error = Some(why.chars().take(1000).collect());
                if m.attempts >= max {
                    m.status = Status::Failed;
                    m.next_attempt_at = None;
                } else {
                    m.status = Status::Queued;
                    m.next_attempt_at = Some(Utc::now() + chrono::Duration::minutes(2i64.pow(m.attempts.min(6))));
                }
            });
            Err(why)
        }
    }
}

fn set(store: &MeetingStore, id: &str, status: Status) -> Result<Meeting, StepError> {
    Ok(store.update(id, |m| m.status = status)?)
}

/// (`mic` | `system`, track file) for every track with speech: the video's audio stands in
/// for a missing system track.
pub fn sources(m: &Meeting) -> Vec<(&'static str, String)> {
    let mut v = Vec::new();
    if m.track("mic.m4a").is_some() {
        v.push(("mic", "mic.m4a".to_owned()));
    }
    if m.track("system.m4a").is_some() {
        v.push(("system", "system.m4a".to_owned()));
    } else if m.track("video.mp4").is_some() {
        v.push(("system", "video.mp4".to_owned()));
    }
    v
}

fn offset(m: &Meeting, track: &str) -> f64 {
    m.track(track).and_then(|t| t.start_offset_ms).unwrap_or(0) as f64 / 1000.0
}

/// The sample range of the owner's speech in the mic track, at most `max_sec` of it.
fn speech_samples(samples: &[f32], t: &Transcript, max_sec: f64) -> Vec<f32> {
    let mut out = Vec::new();
    for s in &t.segments {
        let a = ((s.start * 16_000.0) as usize).min(samples.len());
        let b = ((s.end * 16_000.0) as usize).min(samples.len());
        out.extend_from_slice(&samples[a..b]);
        if out.len() as f64 >= max_sec * 16_000.0 {
            break;
        }
    }
    out
}

async fn steps(app: &App, store: &MeetingStore, id: &str) -> Result<(), StepError> {
    let mut m = store.get(id).ok_or_else(|| StepError::Failed("meeting vanished".into()))?;
    let work = store.work(&m);
    tokio::fs::create_dir_all(&work).await?;
    let tz = app.cfg.timezone;
    let max = app.cfg.max_attempts;

    // 0. The calendar event (by the Mac's id, else the one the recording falls in). Best effort.
    if m.steps.linked_at.is_none() && crate::events::configured(app) {
        match link_event(app, &m).await {
            Ok(event) => {
                m = store.update(id, |m| {
                    if m.event.is_none() {
                        m.event = event;
                    }
                    if m.company.is_none() {
                        m.company = m.event.as_ref().and_then(|e| super::company(&e.attendees));
                    }
                    m.steps.linked_at = Some(Utc::now());
                })?;
            }
            Err(e) => tracing::warn!(%id, error = %e, "meeting calendar link deferred"),
        }
    }

    // 1. Transcripts.
    if m.steps.transcribed_at.is_none() {
        m = set(store, id, Status::Transcribing)?;
        let mut language = (0usize, String::from("und"));
        let mut duration = m.duration_sec;
        for (kind, track) in sources(&m) {
            let out = work.join(format!("{kind}.json"));
            let t: Transcript = match read_json(&out)? {
                Some(t) => t,
                None => {
                    let wav = work.join(format!("{kind}.wav"));
                    let dur = app.stt.decode(&store.track_path(&m, &track), &wav).await?;
                    let wait = if app.stt.gpu_free().await { Duration::from_secs(120) } else { Duration::ZERO };
                    let sub = work.join(format!("whisper-{kind}"));
                    tokio::fs::create_dir_all(&sub).await?;
                    match app.stt.transcribe_words(&wav, &sub, dur, wait).await {
                        Ok(t) => {
                            write_json(&out, &t)?;
                            t
                        }
                        Err(e) if e.is_transient() => {
                            let why = match e {
                                SttError::ModelMissing(_) => "waiting for the Whisper model (MyBook locked?)",
                                _ => "waiting for the GPU",
                            };
                            return Err(StepError::Later(Duration::from_secs(60), why.into()));
                        }
                        Err(e) => return Err(e.into()),
                    }
                }
            };
            let chars = t.text().chars().count();
            if chars > language.0 {
                language = (chars, t.language.clone());
            }
            duration = duration.max(t.duration_sec + offset(&m, &track));
        }
        m = store.update(id, |m| {
            m.language = Some(language.1.clone());
            m.duration_sec = duration;
            m.steps.transcribed_at = Some(Utc::now());
        })?;
    }

    // 2. Speakers and the merged timeline.
    let timeline_path = work.join("timeline.json");
    if m.steps.diarized_at.is_none() || !timeline_path.exists() {
        m = set(store, id, Status::Diarizing)?;
        let labelled = diarize(app, &m, &work).await?;
        write_json(&timeline_path, &labelled)?;
        m = store.update(id, |m| m.steps.diarized_at = Some(Utc::now()))?;
    }
    let language = m.language.clone().unwrap_or_else(|| "und".into());

    // 3. Roles and kinds, then the brief.
    let analysis_path = work.join("analysis.json");
    if m.steps.analyzed_at.is_none() || !analysis_path.exists() {
        m = set(store, id, Status::Analyzing)?;
        let Labelled { mut speakers, mut utterances } = read_json(&timeline_path)?.unwrap_or_default();
        let a = match analyze::analyze(app.llm.as_ref(), &m, &utterances, &speakers).await {
            Ok(a) => a,
            Err(e) if m.attempts + 1 >= max => {
                analyze::fallback(&utterances, &speakers, m.event.as_ref().map(|e| e.kind), Some(e.to_string()))
            }
            Err(e) => return Err(StepError::Failed(format!("analysis: {e}"))),
        };
        for s in speakers.iter_mut() {
            s.role = a.roles.get(&s.id).cloned();
            if s.name.is_none() {
                s.suggested_name = a.suggested.get(&s.id).cloned();
            }
        }
        for (u, k) in utterances.iter_mut().zip(&a.kinds) {
            u.kind = *k;
        }
        relabel(&mut utterances, &speakers);
        write_json(&analysis_path, &Labelled { speakers, utterances })?;
        let source = a.source.clone();
        m = store.update(id, |m| {
            m.analysis_source = Some(source);
            m.steps.analyzed_at = Some(Utc::now());
            m.steps.briefed_at = None;
        })?;
    }
    let Labelled { speakers, utterances } = read_json(&analysis_path)?.unwrap_or_default();
    let brief_path = work.join("brief.json");
    if m.steps.briefed_at.is_none() || !brief_path.exists() {
        let b = match brief::write(app.llm.as_ref(), &app.llm_label, &m, &utterances, &speakers, &language, tz).await {
            Ok(Ok(b)) => b,
            Ok(Err(why)) => {
                tracing::warn!(%id, %why, "brief reply rejected; deterministic brief");
                brief::fallback(&m, &utterances, &speakers, &language)
            }
            Err(_) if m.attempts + 1 >= max => brief::fallback(&m, &utterances, &speakers, &language),
            Err(e) => return Err(StepError::Failed(format!("brief: {e}"))),
        };
        write_json(&brief_path, &b)?;
        let source = b.source.clone();
        m = store.update(id, |m| {
            m.brief_source = Some(source);
            m.steps.briefed_at = Some(Utc::now());
            m.steps.written_at = None;
            m.steps.noted_at = None;
        })?;
    }
    let b: Brief = read_json(&brief_path)?.ok_or_else(|| StepError::Failed("brief.json missing".into()))?;

    // 4. Publishing.
    let url = app.cfg.meeting_transcript_url(id);
    if m.steps.written_at.is_none() {
        m = set(store, id, Status::Publishing)?;
        write_outputs(store, &m, &b, &speakers, &utterances, tz, &url)?;
        m = store.update(id, |m| m.steps.written_at = Some(Utc::now()))?;
    }
    if let Some(vault) = &app.cfg.vault {
        if m.steps.noted_at.is_none() {
            m = set(store, id, Status::Publishing)?;
            m = super::publish::note(app, store, &m, vault, &b, &speakers, &utterances).await?;
        }
    }
    if let Some(mem) = &app.memory {
        if m.steps.remembered_at.is_none() {
            m = set(store, id, Status::Publishing)?;
            if kernel::send_once(m.memory_rev, m.revision) {
                let rev = m.revision;
                m = store.update(id, |m| m.memory_rev = Some(rev))?;
                if let Err(e) = super::publish::remember(mem, &m, &b, &speakers, tz).await {
                    tracing::warn!(%id, error = %e, "meeting memory entry failed (not retried: at most once)");
                }
            }
            m = store.update(id, |m| m.steps.remembered_at = Some(Utc::now()))?;
        }
    }
    if m.steps.notified_at.is_none() {
        m = set(store, id, Status::Publishing)?;
        super::publish::register_media(store, &m, &url).await;
        let telegram = kernel::send_once(m.notify_telegram_rev, m.revision);
        if telegram {
            let rev = m.revision;
            m = store.update(id, |m| m.notify_telegram_rev = Some(rev))?;
        }
        let sent = super::publish::notify(app, &m, &b, &url, telegram).await?;
        tracing::info!(%id, channels = ?sent, "meeting ready");
        m = store.update(id, |m| m.steps.notified_at = Some(Utc::now()))?;
    }
    super::publish::ask_unknown_voices(app).await;

    if !kernel::ready_to_finish(
        m.steps.transcribed_at.is_some(),
        m.steps.diarized_at.is_some(),
        m.steps.analyzed_at.is_some(),
        m.steps.briefed_at.is_some(),
        m.steps.written_at.is_some(),
        m.steps.noted_at.is_some(),
        app.cfg.vault.is_some(),
        m.steps.remembered_at.is_some(),
        app.memory.is_some(),
        m.steps.notified_at.is_some(),
    ) {
        return Err(StepError::Failed("a step is missing; not marking done".into()));
    }
    store.update(id, |m| {
        m.status = Status::Done;
        m.error = None;
        m.attempts = 0;
        m.next_attempt_at = None;
    })?;
    // The WAVs and Whisper's raw output go; the JSON in .work stays (renaming a speaker
    // re-renders from it, and its voice centroid feeds the profile).
    for (kind, _) in sources(&m) {
        let _ = tokio::fs::remove_file(work.join(format!("{kind}.wav"))).await;
        let _ = tokio::fs::remove_dir_all(work.join(format!("whisper-{kind}"))).await;
    }
    let _ = tokio::fs::remove_dir_all(work.join("obsidian")).await;
    Ok(())
}

async fn link_event(app: &App, m: &Meeting) -> Result<Option<crate::model::EventLink>, String> {
    let events = crate::events::fetch(app, m.started_at - chrono::Duration::hours(1), m.end() + chrono::Duration::hours(1)).await?;
    if let Some(want) = &m.calendar_event_id {
        if let Some(e) = events.iter().find(|e| &e.id == want) {
            return Ok(Some(e.link()));
        }
    }
    let cands: Vec<crate::kernel::Cand> = events
        .iter()
        .map(|e| crate::kernel::Cand {
            start_ms: e.start_ms,
            end_ms: e.end_ms,
            all_day: e.all_day,
            loc_match: false,
        })
        .collect();
    Ok(crate::kernel::pick_event(m.started_at.timestamp_millis(), m.end().timestamp_millis(), &cands).map(|i| events[i].link()))
}

/// Display names from the speakers (name, else role, else label).
pub fn relabel(utts: &mut [Utterance], speakers: &[Speaker]) {
    for u in utts.iter_mut() {
        if let Some(s) = speakers.iter().find(|s| s.id == u.speaker) {
            u.speaker_name = s.display();
            u.role = s.role.clone();
        }
    }
}

/// Step 2: clusters, identities and the merged timeline.
async fn diarize(app: &App, m: &Meeting, work: &Path) -> Result<Labelled, StepError> {
    let mic: Option<Transcript> = read_json(&work.join("mic.json"))?;
    let sys: Option<Transcript> = read_json(&work.join("system.json"))?;
    let sources = sources(m);
    let sys_track = sources.iter().find(|s| s.0 == "system").map(|s| s.1.clone());
    let diarizer = match app.diarizer() {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(id = %m.id, error = %e, "no diarizer; the system track is one speaker");
            None
        }
    };
    let turns_path = work.join("turns.json");
    let mut d: Diarization = read_json(&turns_path)?.unwrap_or_default();
    let fresh = !turns_path.exists();
    if let (true, Some(dz), Some(track), Some(_)) = (fresh, &diarizer, &sys_track, &sys) {
        let wav_path = work.join("system.wav");
        if !wav_path.exists() {
            app.stt.decode(&app.meetings()?.track_path(m, track), &wav_path).await?;
        }
        let dz = dz.clone();
        d = tokio::task::spawn_blocking(move || -> Result<Diarization, DiarizeError> { dz.diarize(&wav::read(&wav_path)?) })
            .await?
            .map_err(|e| StepError::Failed(e.to_string()))?;
    }
    // The owner's voice from his microphone (feeds his profile; recognises him in the system
    // track, e.g. a meeting room's audio).
    if let (true, Some(dz), Some(t)) = (fresh, &diarizer, &mic) {
        let wav_path = work.join("mic.wav");
        if !wav_path.exists() {
            if let Some((_, track)) = sources.iter().find(|s| s.0 == "mic") {
                app.stt.decode(&app.meetings()?.track_path(m, track), &wav_path).await?;
            }
        }
        let (dz, t) = (dz.clone(), t.clone());
        let voice = tokio::task::spawn_blocking(move || -> Result<Vec<f32>, DiarizeError> {
            let speech = speech_samples(&wav::read(&wav_path)?, &t, 60.0);
            if speech.len() < 16_000 * 3 {
                return Ok(Vec::new());
            }
            dz.embed(&speech)
        })
        .await?;
        match voice {
            Ok(v) if !v.is_empty() => {
                app.voices.add_to_profile(&app.cfg.owner_name, true, &v)?;
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(id = %m.id, error = %e, "owner voice embedding failed"),
        }
    }
    if fresh {
        write_json(&turns_path, &d)?;
    }
    let threshold = app.cfg.diarize.as_ref().map_or(0.6, |c| c.match_threshold);
    let owner_voice = app.voices.owner_voice();
    let mut ids: Vec<String> = Vec::new();
    let mut speakers: Vec<Speaker> = Vec::new();
    let mut voices = Voices {
        centroids: Vec::new(),
        engine: (!d.engine.is_empty()).then(|| d.engine.clone()),
    };
    if mic.is_some() {
        speakers.push(owner_speaker(&app.cfg.owner_name));
    }
    let mut n = 0;
    for c in &d.centroids {
        if owner_voice.as_ref().is_some_and(|o| cosine(o, c) >= threshold) {
            ids.push("me".into());
            if !speakers.iter().any(|s| s.id == "me") {
                speakers.push(owner_speaker(&app.cfg.owner_name));
            }
            continue;
        }
        n += 1;
        let sid = format!("S{n}");
        let identity = app.voices.identify(
            c,
            threshold,
            Sighting {
                source: m.id.clone(),
                speaker: sid.clone(),
                title: m.title.clone(),
                at: m.started_at,
            },
        )?;
        let mut s = system_speaker(&sid, n);
        match identity {
            Identity::Known { profile, name, owner, similarity } => {
                s.name = Some(if owner { app.cfg.owner_name.clone() } else { name });
                s.name_source = Some("profile".into());
                s.profile_id = Some(profile);
                s.similarity = Some(similarity);
            }
            Identity::Unknown { voice, similarity } => {
                s.voice_id = Some(voice);
                s.similarity = similarity;
            }
        }
        voices.centroids.push((sid.clone(), c.clone()));
        ids.push(sid);
        speakers.push(s);
    }
    if d.centroids.is_empty() && sys.as_ref().is_some_and(|t| !t.segments.is_empty()) {
        ids.push("S1".into());
        speakers.push(system_speaker("S1", 1));
    }
    write_json(&work.join("voices.json"), &voices)?;
    let mic_parts = mic
        .as_ref()
        .map(|t| timeline::mic_parts(&t.segments, offset(m, "mic.m4a"), "me"))
        .unwrap_or_default();
    let sys_parts = match (&sys, &sys_track) {
        (Some(t), Some(track)) => timeline::system_parts(&t.segments, &d.turns, &ids, offset(m, track)),
        _ => Vec::new(),
    };
    let parts = timeline::merge(mic_parts, sys_parts);
    let mut utterances: Vec<Utterance> = parts
        .into_iter()
        .map(|p| Utterance {
            start: p.start,
            end: p.end,
            speaker: p.speaker,
            speaker_name: String::new(),
            role: None,
            kind: Default::default(),
            text: p.text,
            track: p.track.to_owned(),
            words: p.words,
        })
        .collect();
    for s in speakers.iter_mut() {
        let mine = utterances.iter().filter(|u| u.speaker == s.id);
        s.talk_sec = mine.clone().map(|u| (u.end - u.start).max(0.0)).sum();
        s.utterances = mine.count() as u32;
    }
    speakers.retain(|s| s.utterances > 0);
    relabel(&mut utterances, &speakers);
    Ok(Labelled { speakers, utterances })
}

fn owner_speaker(name: &str) -> Speaker {
    Speaker {
        id: "me".into(),
        label: name.into(),
        name: Some(name.into()),
        name_source: Some("config".into()),
        suggested_name: None,
        role: None,
        track: "mic".into(),
        profile_id: None,
        voice_id: None,
        similarity: None,
        talk_sec: 0.0,
        utterances: 0,
    }
}

fn system_speaker(id: &str, n: usize) -> Speaker {
    Speaker {
        id: id.into(),
        label: format!("Speaker {n}"),
        name: None,
        name_source: None,
        suggested_name: None,
        role: None,
        track: "system".into(),
        profile_id: None,
        voice_id: None,
        similarity: None,
        talk_sec: 0.0,
        utterances: 0,
    }
}

/// `transcript.json`, `transcript.md`, `speakers.json`, `brief.md` in the meeting folder.
pub fn write_outputs(
    store: &MeetingStore,
    m: &Meeting,
    b: &Brief,
    speakers: &[Speaker],
    utts: &[Utterance],
    tz: chrono_tz::Tz,
    url: &str,
) -> std::io::Result<()> {
    let dir = store.dir(m);
    let voices: Voices = read_json(&store.work(m).join("voices.json"))?.unwrap_or_default();
    let mic: Option<Transcript> = read_json(&store.work(m).join("mic.json"))?;
    let sys: Option<Transcript> = read_json(&store.work(m).join("system.json"))?;
    let engine = sys.as_ref().or(mic.as_ref()).map(|t| t.engine.clone()).unwrap_or_default();
    write_json(
        &dir.join("transcript.json"),
        &MeetingTranscript {
            meeting_id: m.id.clone(),
            language: m.language.clone().unwrap_or_else(|| "und".into()),
            engine,
            diarization: voices.engine,
            duration_sec: m.duration_sec,
            segments: utts.to_vec(),
            created_at: Utc::now(),
        },
    )?;
    write_json(&dir.join("speakers.json"), &speakers)?;
    write_atomic(&dir.join("transcript.md"), brief::transcript_md(m, utts, tz).as_bytes())?;
    write_atomic(&dir.join("brief.md"), brief::markdown(m, b, speakers, tz, url).as_bytes())?;
    Ok(())
}
