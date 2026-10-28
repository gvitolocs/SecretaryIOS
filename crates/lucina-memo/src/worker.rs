//! The pipeline worker: one memo at a time (the GPU and the host are
//! shared), oldest first. Every step writes its output before the memo's
//! `steps` record it, and a step whose output exists is skipped, so a crash
//! or restart continues where it stopped.
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;

use crate::model::{Memo, Status, Summary, Transcript};
use crate::store::{read_json, write_atomic, write_json};
use crate::{kernel, obsidian, render, summarize, transcribe, App};

/// `resummarize.json`: the owner's instructions for the next summary.
#[derive(serde::Serialize, serde::Deserialize, Default)]
pub struct Resummarize {
    #[serde(default)]
    pub instructions: Option<String>,
}

pub async fn run(app: Arc<App>) {
    if let Err(e) = crate::calendar::refresh(&app).await {
        tracing::warn!(error = %e, "calendar refresh deferred");
    }
    let mut refreshed = std::time::Instant::now();
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
                if refreshed.elapsed() >= Duration::from_secs(900) {
                    if let Err(e) = crate::calendar::refresh(&app).await {
                        tracing::warn!(error = %e, "calendar refresh deferred");
                    }
                    refreshed = std::time::Instant::now();
                }
                if relink_one(&app).await {
                    continue;
                }
                if crate::archive::archive_one(&app) {
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
                m.archived_rev = None;
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

    // 0. An ADTS upload (.aac, crash-safe on the iPhone) becomes .m4a for playback,
    //    Whisper and the vault. The original stays until the .m4a is in place.
    if m.ext == "aac" {
        let input = store.audio_path(&m);
        let output = store.dir(id).join("audio.m4a");
        transcribe::remux_adts(&app.cfg.whisper, &input, &output).await?;
        let size = tokio::fs::metadata(&output).await?.len();
        m = store.update(id, |m| {
            m.ext = "m4a".into();
            m.mime = "audio/mp4".into();
            m.size_bytes = size;
        })?;
        let _ = tokio::fs::remove_file(&input).await;
    }

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
            // A reprocess may run step 1 again over an existing transcript.
            let existing = kernel::Source::parse(m.transcript_source.as_deref())
                .filter(|_| store.transcript_path(id).exists());
            let result = transcribe::transcribe(&app.cfg.whisper, &wav, &work, dur, wait).await;
            let gpu = match &result {
                Ok(_) => kernel::Gpu::Done,
                Err(e) if e.is_transient() => kernel::Gpu::Busy,
                Err(_) => kernel::Gpu::Failed,
            };
            let (t, source) = match (
                kernel::transcript_choice(existing, gpu, device.is_some()),
                result,
                device,
            ) {
                (kernel::Choice::UseWhisper, Ok(t), _) => (t, "whisper"),
                (kernel::Choice::UseDevice, Err(e), Some(d)) => {
                    tracing::info!(%id, reason = %e, "using the iPhone's transcript for now");
                    (d, "device")
                }
                (kernel::Choice::Wait, Err(e), _) => {
                    let _ = tokio::fs::remove_file(&wav).await;
                    let why = match e {
                        transcribe::SttError::ModelMissing(_) => {
                            "waiting for the Whisper model (MyBook locked?)"
                        }
                        _ => "waiting for the GPU",
                    };
                    return Err(StepError::Later(Duration::from_secs(60), why.into()));
                }
                (_, Err(e), _) => return Err(e.into()),
                (c, Ok(_), _) => {
                    return Err(StepError::Failed(format!(
                        "transcript choice {c:?} without a transcript"
                    )))
                }
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
                m.steps.diarized_at = None;
            })?;
            t
        }
    };

    // 1a. Speaker labels (best effort, K11): once per transcript; a missing diarizer (models
    //     not installed, MyBook locked) is retried on the next run, a failure is not.
    let mut transcript = transcript;
    let available = match app.diarizer() {
        Ok(d) => d.is_some(),
        Err(e) => {
            tracing::debug!(%id, error = %e, "diarizer unavailable");
            false
        }
    };
    if kernel::diarize_memo(
        available,
        transcript.speakers,
        m.steps.diarized_at.is_some(),
    ) {
        match crate::diarize::memo::label(app, &m, &transcript, &work).await {
            Ok(Some(t)) => {
                write_json(&store.transcript_path(id), &t)?;
                tracing::info!(%id, "memo speakers labelled");
                transcript = t;
            }
            Ok(None) => {}
            Err(e) => tracing::warn!(%id, error = %e, "memo diarization failed; transcript kept"),
        }
        m = store.update(id, |m| m.steps.diarized_at = Some(Utc::now()))?;
    }

    // 1b. Calendar linking is best effort and never delays transcription.
    if m.steps.linked_at.is_none()
        && kernel::may_auto_link(kernel::LinkSource::parse(m.event_source.as_deref()))
        && crate::events::configured(app)
    {
        match crate::events::candidates(app, &m, 30).await {
            Ok(candidates) => {
                let picked = kernel::pick_event(
                    m.recorded_at.timestamp_millis(),
                    m.recording_end().timestamp_millis(),
                    &candidates.iter().map(|c| c.cand).collect::<Vec<_>>(),
                );
                m = store.update(id, |m| {
                    if kernel::may_auto_link(kernel::LinkSource::parse(m.event_source.as_deref())) {
                        m.event = picked.map(|i| candidates[i].event.clone());
                        m.event_source = Some("auto".into());
                        m.steps.linked_at = Some(Utc::now());
                        crate::events::apply_title(m, tz);
                    }
                })?;
            }
            Err(e) => tracing::warn!(%id, error = %e, "calendar link deferred"),
        }
    }

    // 2. Summary.
    let summary: Summary = match read_json::<Summary>(&store.summary_path(id))? {
        Some(s) if m.steps.summarized_at.is_some() => s,
        _ => {
            m = set(app, id, Status::Summarizing)?;
            let previous = crate::events::previous(app, &m)
                .into_iter()
                .take(3)
                .filter_map(|p| {
                    read_json::<Summary>(&store.summary_path(&p.id))
                        .ok()
                        .flatten()
                        .map(|s| {
                            format!(
                                "{} ({}): {}",
                                p.title,
                                p.recorded_at.format("%-d %b"),
                                s.overview
                            )
                        })
                })
                .collect::<Vec<_>>();
            let extra_path = store.dir(id).join("resummarize.json");
            let extra: Resummarize = read_json(&extra_path)?.unwrap_or_default();
            let s = summarize::summarize(
                app.llm.as_ref(),
                &app.llm_label,
                &m,
                &transcript,
                tz,
                extra.instructions.as_deref(),
                &previous,
            )
            .await?;
            write_json(&store.summary_path(id), &s)?;
            let _ = std::fs::remove_file(&extra_path);
            m = store.update(id, |m| {
                if m.title_source != "owner" && m.title_source != "event" {
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
        title: if m.title_source == "owner" || m.title_source == "event" {
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
            let existing = m
                .obsidian_note
                .as_deref()
                .and_then(|p| p.rsplit('/').next())
                .and_then(|f| f.strip_suffix(".md"))
                .map(str::to_owned);
            let base = kernel::note_base(existing, || obsidian::base_name(&m, &summary, tz));
            // The path is recorded before the copy: a crash after rsync must not let a later
            // title (rename, new summary) create a second note for this memo.
            let subfolder = obsidian::event_folder(&m);
            let relative = if subfolder.is_empty() {
                format!("{base}.md")
            } else {
                format!("{subfolder}/{base}.md")
            };
            let note_path = format!("{}/{relative}", vault.folder);
            if m.obsidian_note.as_deref() != Some(note_path.as_str()) {
                m = store.update(id, |m| obsidian::move_note(m, &note_path))?;
            }
            let mut note_md = obsidian::note(&m, &summary, &transcript, tz, &base, &url);
            let previous = crate::events::previous(app, &m);
            let links: Vec<_> = previous
                .iter()
                .filter_map(|p| p.obsidian_note.as_deref())
                .filter_map(|p| p.rsplit('/').next())
                .filter_map(|p| p.strip_suffix(".md"))
                .map(|p| format!("- [[{p}]]"))
                .collect();
            if !links.is_empty() {
                note_md.push_str(&format!("\n## Previous\n\n{}\n", links.join("\n")));
            }
            let mut entries: Vec<(Memo, String)> = store
                .list()
                .into_iter()
                .filter(|x| x.id != m.id)
                .filter_map(|x| {
                    let b = x.obsidian_note.as_deref()?.strip_suffix(".md")?.to_owned();
                    Some((x, b))
                })
                .collect();
            entries.push((m.clone(), note_path.trim_end_matches(".md").to_owned()));
            entries.sort_by_key(|e| std::cmp::Reverse(e.0.recorded_at));
            let index_md = obsidian::index(&entries, tz);
            write_atomic(&store.dir(id).join("note.md"), note_md.as_bytes())?;
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
            if !subfolder.is_empty() {
                tokio::fs::create_dir_all(bundle.stage.join(&subfolder)).await?;
                tokio::fs::rename(
                    bundle.stage.join(format!("{base}.md")),
                    bundle.stage.join(&relative),
                )
                .await?;
                if let Some(event) = &m.event {
                    let series_entries: Vec<_> = entries
                        .iter()
                        .filter(|(p, _)| p.event.as_ref().is_some_and(|e| e.series == event.series))
                        .cloned()
                        .collect();
                    let index_path = bundle.stage.join(&subfolder).join(format!(
                        "{}.md",
                        obsidian::safe_name(&crate::events::series_title(&event.title))
                    ));
                    let series_md = obsidian::index(&series_entries, tz);
                    write_atomic(&index_path, series_md.as_bytes())?;
                    write_atomic(&store.dir(id).join("series-index.md"), series_md.as_bytes())?;
                }
            }
            obsidian::push(vault, &bundle).await?;
            // The note moved (its event changed): delete every earlier copy, then forget them.
            if !m.obsidian_old_notes.is_empty() {
                for old in &m.obsidian_old_notes {
                    obsidian::delete_note(vault, old).await?;
                }
                let done = m.obsidian_old_notes.clone();
                store.update(id, |m| m.obsidian_old_notes.retain(|o| !done.contains(o)))?;
            }
            let _ = tokio::fs::remove_dir_all(&bundle.stage).await;
            m = store.update(id, |m| m.steps.published_at = Some(Utc::now()))?;
        }
    }

    // 5. Notification.
    if m.steps.notified_at.is_none() {
        let telegram = kernel::send_telegram(m.notify_telegram_rev, m.revision);
        if telegram {
            let rev = m.revision;
            m = store.update(id, |m| m.notify_telegram_rev = Some(rev))?;
        }
        let sent = crate::notify::send(&app.cfg.notify, &m, &summary, &url, telegram).await?;
        tracing::info!(%id, channels = ?sent, "memo ready");
        m = store.update(id, |m| m.steps.notified_at = Some(Utc::now()))?;
    }
    if !kernel::ready_to_finish(
        m.steps.transcribed_at.is_some(),
        m.steps.summarized_at.is_some(),
        m.steps.rendered_at.is_some(),
        m.steps.published_at.is_some(),
        app.cfg.vault.is_some(),
        m.steps.notified_at.is_some(),
    ) {
        return Err(StepError::Failed(
            "a step is missing; not marking done".into(),
        ));
    }

    let m = store.update(id, |m| {
        m.status = Status::Done;
        // --all may have archived this revision while it was still processing.
        m.archived_rev = None;
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
            if !kernel::upgrade_replaces(ours as u64, theirs as u64) {
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
                m.steps.diarized_at = None;
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
                .is_some_and(transcribe::SttError::is_transient);
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
            m.steps.diarized_at = None;
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

/// Check one pending recording against the persistent cache, without network access.
pub async fn relink_one(app: &App) -> bool {
    let generation = app.calendar.generation();
    let mut memos: Vec<_> = app
        .store
        .list()
        .into_iter()
        .filter(|m| {
            // Memos still in the pipeline get their link in step 1b; this job takes the
            // finished ones (Stateright `WorkerRelink`: done or failed).
            m.event.is_none()
                && kernel::may_auto_link(kernel::LinkSource::parse(m.event_source.as_deref()))
                && matches!(m.status, Status::Done | Status::Failed)
                && m.recorded_at >= Utc::now() - chrono::Duration::days(90)
                && m.link_checked_gen.is_none_or(|g| g < generation)
        })
        .collect();
    memos.sort_by_key(|m| m.recorded_at);
    let Some(m) = memos.first() else {
        return false;
    };
    let (generation, candidates) = crate::events::cached_candidates(app, m).await;
    let picked = kernel::pick_event(
        m.recorded_at.timestamp_millis(),
        m.recording_end().timestamp_millis(),
        &candidates.iter().map(|c| c.cand).collect::<Vec<_>>(),
    );
    let mut linked = false;
    let result = app.store.update(&m.id, |m| {
        if m.event.is_some()
            || !kernel::may_auto_link(kernel::LinkSource::parse(m.event_source.as_deref()))
            || !matches!(m.status, Status::Done | Status::Failed)
        {
            return;
        }
        m.link_checked_gen = Some(generation);
        if let Some(i) = picked {
            m.event = Some(candidates[i].event.clone());
            m.event_source = Some("auto".into());
            m.steps.linked_at = Some(Utc::now());
            crate::events::apply_title(m, app.cfg.timezone);
            m.steps.summarized_at = None;
            m.steps.rendered_at = None;
            m.steps.published_at = None;
            m.revision += 1;
            m.status = Status::Queued;
            m.attempts = 0;
            m.next_attempt_at = None;
            m.error = None;
            linked = true;
        }
    });
    match result {
        Ok(_) => {
            tracing::info!(id = %m.id, generation, linked, "memo calendar re-link checked");
            linked
        }
        Err(e) => {
            tracing::warn!(id = %m.id, error = %e, "memo re-link failed");
            false
        }
    }
}
