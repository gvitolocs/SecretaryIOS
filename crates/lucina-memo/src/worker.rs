//! The pipeline worker: one memo at a time (the GPU and the host are
//! shared), oldest first. Every step writes its output before the memo's
//! `steps` record it, and a step whose output exists is skipped, so a crash
//! or restart continues where it stopped.
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;

use crate::model::{Memo, Status, Summary, Transcript};
use crate::store::{read_json, write_atomic, write_json};
use crate::{obsidian, render, summarize, transcribe, App};

/// `resummarize.json`: the owner's instructions for the next summary.
#[derive(serde::Serialize, serde::Deserialize, Default)]
pub struct Resummarize {
    #[serde(default)]
    pub instructions: Option<String>,
}

pub async fn run(app: Arc<App>) {
    loop {
        match next_due(&app) {
            Some(id) => {
                if let Err(e) = process(&app, &id).await {
                    tracing::warn!(%id, error = %e, "memo pipeline step failed");
                }
            }
            None => {
                // Idle: give one memo that runs on the iPhone's transcript Whisper's, if the GPU is free.
                if upgrade_one(&app).await {
                    continue;
                }
                tokio::select! {
                    _ = app.wake.notified() => {}
                    _ = tokio::time::sleep(Duration::from_secs(30)) => {}
                }
            }
        }
    }
}

fn next_due(app: &App) -> Option<String> {
    let now = Utc::now();
    let mut due: Vec<Memo> = app
        .store
        .list()
        .into_iter()
        .filter(|m| m.status.is_pipeline() && m.next_attempt_at.is_none_or(|t| t <= now))
        .collect();
    due.sort_by_key(|m| m.created_at);
    due.into_iter().next().map(|m| m.id)
}

enum StepError {
    /// Try again later without counting an attempt (GPU busy).
    Later(Duration, String),
    Failed(String),
}

impl<E: std::fmt::Display> From<E> for StepError {
    fn from(e: E) -> Self {
        StepError::Failed(e.to_string())
    }
}

fn set(app: &App, id: &str, status: Status) -> Result<Memo, StepError> {
    Ok(app.store.update(id, |m| m.status = status)?)
}

/// Runs the remaining steps of one memo and records the outcome.
pub async fn process(app: &App, id: &str) -> Result<(), String> {
    let result = steps(app, id).await;
    let max = app.cfg.max_attempts;
    match result {
        Ok(()) => Ok(()),
        Err(StepError::Later(wait, why)) => {
            let _ = app.store.update(id, |m| {
                m.status = Status::Queued;
                m.error = Some(why.clone());
                m.next_attempt_at =
                    Some(Utc::now() + chrono::Duration::from_std(wait).unwrap_or_default());
            });
            Err(why)
        }
        Err(StepError::Failed(why)) => {
            let _ = app.store.update(id, |m| {
                m.attempts += 1;
                m.error = Some(why.chars().take(1000).collect());
                if m.attempts >= max {
                    m.status = Status::Failed;
                    m.next_attempt_at = None;
                } else {
                    m.status = Status::Queued;
                    let mins = 2i64.pow(m.attempts.min(6));
                    m.next_attempt_at = Some(Utc::now() + chrono::Duration::minutes(mins));
                }
            });
            Err(why)
        }
    }
}

async fn steps(app: &App, id: &str) -> Result<(), StepError> {
    let store = &app.store;
    let mut m = store
        .get(id)
        .ok_or_else(|| StepError::Failed("memo vanished".into()))?;
    let work = store.dir(id).join("work");
    tokio::fs::create_dir_all(&work).await?;
    let tz = app.cfg.timezone;

    // 1. Transcript.
    let transcript: Transcript = match read_json::<Transcript>(&store.transcript_path(id))? {
        Some(t) if m.steps.transcribed_at.is_some() => t,
        _ => {
            m = set(app, id, Status::Transcribing)?;
            let wav = work.join("audio.wav");
            let audio = store.audio_path(&m);
            let dur = transcribe::decode(&app.cfg.whisper, &audio, &wav).await?;
            // Whisper on the GPU when it is free. Otherwise the iPhone's own transcript (short
            // memos) carries the memo now and the upgrade pass replaces it later; without one
            // the memo waits for the GPU without blocking the others.
            let device: Option<Transcript> = read_json(&store.device_transcript_path(id))?;
            let wait = if device.is_some() || !transcribe::gpu_free(&app.cfg.whisper).await {
                Duration::ZERO
            } else {
                Duration::from_secs(120)
            };
            let (t, source) =
                match transcribe::transcribe(&app.cfg.whisper, &wav, &work, dur, wait).await {
                    Ok(t) => (t, "whisper"),
                    Err(transcribe::SttError::GpuBusy(_)) => match device {
                        Some(d) => {
                            tracing::info!(%id, "GPU busy: using the iPhone's transcript for now");
                            (d, "device")
                        }
                        None => {
                            let _ = tokio::fs::remove_file(&wav).await;
                            return Err(StepError::Later(
                                Duration::from_secs(60),
                                "waiting for the GPU".into(),
                            ));
                        }
                    },
                    Err(e) => return Err(e.into()),
                };
            write_json(&store.transcript_path(id), &t)?;
            let _ = tokio::fs::remove_file(&wav).await;
            m = store.update(id, |m| {
                m.duration_sec = if t.duration_sec > 0.0 {
                    t.duration_sec
                } else {
                    dur
                };
                m.language = Some(t.language.clone());
                m.engine = Some(t.engine.clone());
                m.transcript_source = Some(source.into());
                m.steps.transcribed_at = Some(Utc::now());
            })?;
            t
        }
    };

    // 2. Summary.
    let summary: Summary = match read_json::<Summary>(&store.summary_path(id))? {
        Some(s) if m.steps.summarized_at.is_some() => s,
        _ => {
            m = set(app, id, Status::Summarizing)?;
            let extra_path = store.dir(id).join("resummarize.json");
            let extra: Resummarize = read_json(&extra_path)?.unwrap_or_default();
            let s = summarize::summarize(
                app.llm.as_ref(),
                &app.llm_label,
                &m,
                &transcript,
                tz,
                extra.instructions.as_deref(),
            )
            .await?;
            write_json(&store.summary_path(id), &s)?;
            let _ = std::fs::remove_file(&extra_path);
            m = store.update(id, |m| {
                if m.title_source != "owner" {
                    m.title = s.title.clone();
                    m.title_source = "lucina".into();
                }
                m.preview = Some(s.overview.chars().take(280).collect());
                m.action_items = s.action_items.len() as u32;
                m.tags = s.tags.clone();
                m.language = Some(s.language.clone());
                m.steps.summarized_at = Some(Utc::now());
                m.steps.rendered_at = None;
                m.steps.published_at = None;
                // notified_at stays: new memos start without one, and the owner's re-summary
                // clears it in request_resummary; a Whisper upgrade must not notify twice.
            })?;
            s
        }
    };
    // The owner's title wins in every output.
    let summary = Summary {
        title: if m.title_source == "owner" {
            m.title.clone()
        } else {
            summary.title
        },
        ..summary
    };
    let url = app.cfg.memo_url(id);

    // 3. PDF.
    if m.steps.rendered_at.is_none() || !store.pdf_path(id).exists() {
        m = set(app, id, Status::Rendering)?;
        let html = render::html(&m, &summary, &transcript, tz, &url);
        write_atomic(&store.html_path(id), html.as_bytes())?;
        render::print_pdf(
            &app.cfg.chrome,
            &store.html_path(id),
            &store.pdf_path(id),
            &work,
        )
        .await?;
        m = store.update(id, |m| m.steps.rendered_at = Some(Utc::now()))?;
    }

    // 4. Obsidian.
    if let Some(vault) = &app.cfg.vault {
        if m.steps.published_at.is_none() {
            m = set(app, id, Status::Publishing)?;
            let base = m
                .obsidian_note
                .as_deref()
                .and_then(|p| p.rsplit('/').next())
                .and_then(|f| f.strip_suffix(".md"))
                .map(str::to_owned)
                .unwrap_or_else(|| obsidian::base_name(&m, &summary, tz));
            let note_md = obsidian::note(&m, &summary, &transcript, tz, &base, &url);
            let mut entries: Vec<(Memo, String)> = store
                .list()
                .into_iter()
                .filter(|x| x.id != m.id)
                .filter_map(|x| {
                    let b = x
                        .obsidian_note
                        .as_deref()?
                        .rsplit('/')
                        .next()?
                        .strip_suffix(".md")?
                        .to_owned();
                    Some((x, b))
                })
                .collect();
            entries.push((m.clone(), base.clone()));
            entries.sort_by_key(|e| std::cmp::Reverse(e.0.recorded_at));
            let index_md = obsidian::index(&entries, tz);
            let bundle = obsidian::stage(
                &work.join("obsidian"),
                &base,
                &note_md,
                &store.audio_path(&m),
                &m.ext,
                &store.pdf_path(id),
                &index_md,
            )
            .await?;
            obsidian::push(vault, &bundle).await?;
            let _ = tokio::fs::remove_dir_all(&bundle.stage).await;
            let note_path = format!("{}/{base}.md", vault.folder);
            m = store.update(id, |m| {
                m.obsidian_note = Some(note_path.clone());
                m.steps.published_at = Some(Utc::now());
            })?;
        }
    }

    // 5. Notification.
    if m.steps.notified_at.is_none() {
        let sent = crate::notify::send(&app.cfg.notify, &m, &summary, &url).await?;
        tracing::info!(%id, channels = ?sent, "memo ready");
        store.update(id, |m| m.steps.notified_at = Some(Utc::now()))?;
    }

    let m = store.update(id, |m| {
        m.status = Status::Done;
        m.error = None;
        m.attempts = 0;
        m.next_attempt_at = None;
    })?;
    crate::library::register(app, &m).await;
    let _ = tokio::fs::remove_dir_all(&work).await;
    Ok(())
}

/// Replaces an iPhone transcript with Whisper large-v3 once the GPU is free, then queues a
/// new summary, PDF and note (no second notification). True when it did something.
async fn upgrade_one(app: &App) -> bool {
    let Some(m) = app
        .store
        .list()
        .into_iter()
        .filter(|m| {
            m.status == Status::Done
                && m.transcript_source.as_deref() == Some("device")
                && m.upgrade_attempts < 3
        })
        .min_by_key(|m| m.created_at)
    else {
        return false;
    };
    if !transcribe::gpu_free(&app.cfg.whisper).await {
        return false;
    }
    let id = m.id.clone();
    let work = app.store.dir(&id).join("work");
    let result = async {
        tokio::fs::create_dir_all(&work).await?;
        let wav = work.join("audio.wav");
        let dur = transcribe::decode(&app.cfg.whisper, &app.store.audio_path(&m), &wav).await?;
        let t =
            transcribe::transcribe(&app.cfg.whisper, &wav, &work, dur, Duration::from_secs(120))
                .await?;
        Ok::<_, anyhow::Error>(t)
    }
    .await;
    let _ = tokio::fs::remove_dir_all(&work).await;
    let device: Option<Transcript> = read_json(&app.store.transcript_path(&id)).ok().flatten();
    match result {
        Ok(t) => {
            // Keep the iPhone's text when Whisper heard much less (silence filtering, VAD).
            let ours = t.text().chars().count();
            let theirs = device.as_ref().map_or(0, |d| d.text().chars().count());
            if ours * 2 < theirs {
                let _ = app
                    .store
                    .update(&id, |m| m.transcript_source = Some("device-kept".into()));
                tracing::info!(%id, ours, theirs, "Whisper heard less than the iPhone; kept the iPhone transcript");
                return true;
            }
            if write_json(&app.store.transcript_path(&id), &t).is_err() {
                return false;
            }
            let _ = app.store.update(&id, |m| {
                m.transcript_source = Some("whisper".into());
                m.engine = Some(t.engine.clone());
                m.language = Some(t.language.clone());
                m.steps.transcribed_at = Some(Utc::now());
                // New summary, PDF and note; the owner was already notified.
                m.steps.summarized_at = None;
                m.steps.rendered_at = None;
                m.steps.published_at = None;
                m.revision += 1;
                m.status = Status::Queued;
            });
            tracing::info!(%id, "upgraded the iPhone transcript to Whisper");
            true
        }
        Err(e) => {
            let busy = e
                .downcast_ref::<transcribe::SttError>()
                .is_some_and(|e| matches!(e, transcribe::SttError::GpuBusy(_)));
            if !busy {
                tracing::warn!(%id, error = %e, "Whisper upgrade failed");
                let _ = app.store.update(&id, |m| m.upgrade_attempts += 1);
            }
            !busy
        }
    }
}

/// Re-queues a memo for a new summary (and new PDF, note and notification).
pub fn request_resummary(
    app: &App,
    id: &str,
    instructions: Option<String>,
) -> Result<Memo, crate::store::StoreError> {
    let m = app
        .store
        .get(id)
        .ok_or(crate::store::StoreError::NotFound)?;
    if m.status == Status::Uploading {
        return Err(crate::store::StoreError::Conflict(
            "the upload is not complete".into(),
        ));
    }
    write_json(
        &app.store.dir(id).join("resummarize.json"),
        &Resummarize { instructions },
    )?;
    let m = app.store.update(id, |m| {
        m.steps.summarized_at = None;
        m.steps.rendered_at = None;
        m.steps.published_at = None;
        m.steps.notified_at = None;
        m.revision += 1;
        m.status = Status::Queued;
        m.attempts = 0;
        m.error = None;
        m.next_attempt_at = None;
    })?;
    app.wake.notify_one();
    Ok(m)
}

/// Re-runs every step (e.g. after a failure, or with a better model).
pub fn request_reprocess(
    app: &App,
    id: &str,
    from_transcript: bool,
) -> Result<Memo, crate::store::StoreError> {
    let m = app.store.update(id, |m| {
        if from_transcript {
            m.steps.transcribed_at = None;
        }
        m.steps.summarized_at = None;
        m.steps.rendered_at = None;
        m.steps.published_at = None;
        m.steps.notified_at = None;
        m.revision += 1;
        m.status = Status::Queued;
        m.attempts = 0;
        m.error = None;
        m.next_attempt_at = None;
    })?;
    app.wake.notify_one();
    Ok(m)
}
