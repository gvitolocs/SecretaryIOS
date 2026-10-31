//! Speaker labels for voice memos (additive: the memo API is unchanged; a memo with two or
//! more voices gets `speaker` on its transcript lines and `speakers: true`).
//!
//! The memo's audio is diarized; each cluster is the owner (his voice profile), a named person
//! (their profile) or `Speaker N`; each line gets the speaker whose turns overlap it most
//! (kernel MT7). Best effort: a failure leaves the transcript as it was. Kernel K11 decides
//! when to run (once per transcript; a new transcript is labelled again).
use std::path::Path;

use chrono::Utc;

use super::profiles::{Identity, Sighting};
use super::{cosine, wav, Diarization, DiarizeError};
use crate::kernel::overlap_ms;
use crate::meetings::kernel::speaker_for;
use crate::model::{Memo, Status, Transcript};
use crate::App;

/// Labels by cluster: the owner's name, a profile's name, or `Speaker N`.
fn labels(app: &App, m: &Memo, d: &Diarization) -> std::io::Result<Vec<String>> {
    let threshold = app.cfg.diarize.as_ref().map_or(0.6, |c| c.match_threshold);
    let owner = app.voices.owner_voice();
    let mut out = Vec::new();
    let mut n = 0;
    for c in &d.centroids {
        if owner.as_ref().is_some_and(|o| cosine(o, c) >= threshold) {
            out.push(app.cfg.owner_name.clone());
            continue;
        }
        n += 1;
        let label = format!("Speaker {n}");
        let id = app.voices.identify(
            c,
            threshold,
            Sighting {
                source: m.id.clone(),
                speaker: label.clone(),
                title: m.title.clone(),
                at: m.recorded_at,
            },
        )?;
        out.push(match id {
            Identity::Known { owner: true, .. } => app.cfg.owner_name.clone(),
            Identity::Known { name, .. } => name,
            Identity::Unknown { .. } => label,
        });
    }
    Ok(out)
}

/// Applies the turns to the transcript's lines. None when fewer than two voices spoke.
pub fn apply(t: &Transcript, d: &Diarization, labels: &[String]) -> Option<Transcript> {
    if d.centroids.len() < 2 || labels.len() != d.centroids.len() {
        return None;
    }
    let ms = |s: f64| (s * 1000.0).round() as i64;
    let mut out = t.clone();
    let mut last: Option<usize> = None;
    for s in out.segments.iter_mut() {
        let mut overlaps = vec![0i64; labels.len()];
        for turn in &d.turns {
            if let Some(o) = overlaps.get_mut(turn.speaker) {
                *o += overlap_ms(ms(s.start), ms(s.end), ms(turn.start), ms(turn.end));
            }
        }
        let who = speaker_for(&overlaps).or(last);
        last = who;
        s.speaker = who.map(|i| labels[i].clone());
    }
    let distinct: std::collections::HashSet<_> = out
        .segments
        .iter()
        .filter_map(|s| s.speaker.as_deref())
        .collect();
    if distinct.len() < 2 {
        return None;
    }
    out.speakers = true;
    Some(out)
}

/// Diarizes the memo's audio (`work` holds the WAV while it runs). `Ok(None)`: one voice.
pub async fn label(
    app: &App,
    m: &Memo,
    t: &Transcript,
    work: &Path,
) -> Result<Option<Transcript>, DiarizeError> {
    let Some(dz) = app.diarizer()? else {
        return Ok(None);
    };
    let wav_path = work.join("diarize.wav");
    crate::transcribe::decode(&app.cfg.whisper, &app.store.audio_path(m), &wav_path)
        .await
        .map_err(|e| DiarizeError::Failed(e.to_string()))?;
    let p = wav_path.clone();
    let d = tokio::task::spawn_blocking(move || dz.diarize(&wav::read(&p)?))
        .await
        .map_err(|e| DiarizeError::Failed(e.to_string()))??;
    let _ = tokio::fs::remove_file(&wav_path).await;
    if d.centroids.len() < 2 {
        return Ok(None);
    }
    let l = labels(app, m, &d).map_err(|e| DiarizeError::Failed(e.to_string()))?;
    Ok(apply(t, &d, &l))
}

/// The owner named a voice heard in this memo: its lines get the name, and the PDF and note
/// are made again (no new summary, no notification).
pub fn rename_speaker(app: &App, id: &str, label: &str, name: &str) {
    let path = app.store.transcript_path(id);
    let Ok(Some(mut t)) = crate::store::read_json::<Transcript>(&path) else {
        return;
    };
    let mut changed = false;
    for s in t
        .segments
        .iter_mut()
        .filter(|s| s.speaker.as_deref() == Some(label))
    {
        s.speaker = Some(name.to_owned());
        changed = true;
    }
    if !changed || crate::store::write_json(&path, &t).is_err() {
        return;
    }
    let _ = app.store.update(id, |m| {
        if m.steps.rendered_at.is_some() {
            m.steps.rendered_at = None;
            m.steps.published_at = None;
            m.steps.notified_at = m.steps.notified_at.or(Some(Utc::now()));
            if m.status == Status::Done {
                m.status = Status::Queued;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diarize::Turn;
    use crate::model::Segment;

    #[test]
    fn lines_get_the_speaker_who_spoke_most() {
        let seg = |start: f64, end: f64| Segment {
            start,
            end,
            text: "x".into(),
            speaker: None,
            words: vec![],
        };
        let t = Transcript {
            language: "it".into(),
            engine: "e".into(),
            duration_sec: 10.0,
            segments: vec![seg(0.0, 3.0), seg(3.0, 6.0), seg(6.5, 7.0), seg(7.0, 9.0)],
            speakers: false,
            created_at: Utc::now(),
        };
        let d = Diarization {
            turns: vec![
                Turn {
                    start: 0.0,
                    end: 3.2,
                    speaker: 0,
                },
                Turn {
                    start: 3.2,
                    end: 6.2,
                    speaker: 1,
                },
                Turn {
                    start: 7.0,
                    end: 9.0,
                    speaker: 0,
                },
            ],
            centroids: vec![vec![1.0, 0.0], vec![0.0, 1.0]],
            engine: "e".into(),
        };
        let labels = vec!["Giuseppe".to_owned(), "Speaker 1".to_owned()];
        let out = apply(&t, &d, &labels).unwrap();
        let who: Vec<_> = out
            .segments
            .iter()
            .map(|s| s.speaker.clone().unwrap())
            .collect();
        // The third line has no turn: it stays with the previous speaker.
        assert_eq!(who, ["Giuseppe", "Speaker 1", "Speaker 1", "Giuseppe"]);
        assert!(out.speakers);
        let one = Diarization {
            centroids: vec![vec![1.0]],
            ..d.clone()
        };
        assert!(apply(&t, &one, &labels[..1]).is_none());
    }
}
