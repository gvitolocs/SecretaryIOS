//! Step 4 outside the meeting folder: the Obsidian note, Lucina's memory, the media library,
//! the owner's notification, and the "who is this voice?" question.
use chrono_tz::Tz;
use serde_json::json;

use super::brief;
use super::model::{role_label, Brief, Meeting, Speaker, Utterance};
use super::store::MeetingStore;
use super::worker::StepError;
use crate::model::clock;
use crate::notify::{send_message, Outgoing};
use crate::obsidian::{self, VaultTarget};
use crate::App;

fn yaml(s: &str) -> String {
    format!(
        "\"{}\"",
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', " ")
    )
}

/// The note: frontmatter in the memo notes' style, the brief, then the transcript with one
/// block id per line.
pub fn note_md(
    m: &Meeting,
    b: &Brief,
    speakers: &[Speaker],
    utts: &[Utterance],
    tz: Tz,
    url: &str,
) -> String {
    let mut o = String::from("---\n");
    o.push_str("type: meeting\n");
    o.push_str(&format!(
        "date: {}\n",
        m.started_at.with_timezone(&tz).format("%Y-%m-%dT%H:%M")
    ));
    o.push_str(&format!("duration: {}\n", yaml(&clock(m.duration_sec))));
    o.push_str(&format!("title: {}\n", yaml(&m.title)));
    if let Some(app) = &m.source_app.name {
        o.push_str(&format!("app: {}\n", yaml(app)));
    }
    if let Some(e) = &m.event {
        o.push_str(&format!("event: {}\n", yaml(&e.title)));
        if !e.attendees.is_empty() {
            o.push_str("attendees:\n");
            for a in &e.attendees {
                o.push_str(&format!("  - {}\n", yaml(a)));
            }
        }
    }
    if let Some(c) = &m.company {
        o.push_str(&format!("company: {}\n", yaml(c)));
    }
    o.push_str("speakers:\n");
    for s in speakers {
        o.push_str(&format!(
            "  - {}\n",
            yaml(&format!(
                "{}{}",
                s.display(),
                s.role
                    .as_deref()
                    .map(|r| format!(" ({})", role_label(r)))
                    .unwrap_or_default()
            ))
        ));
    }
    o.push_str(&format!("meeting_id: {}\n", m.id));
    o.push_str(&format!("archive: {}\n", yaml(&m.folder)));
    o.push_str("tags:\n  - meeting\n  - lucina\n");
    for t in &b.tags {
        o.push_str(&format!("  - {t}\n"));
    }
    o.push_str("---\n\n");
    let body = brief::markdown(m, b, speakers, tz, url);
    // The brief's own H1 becomes the note's title line.
    o.push_str(&body);
    o.push_str("\n## Transcript\n\n");
    for (i, u) in utts.iter().enumerate() {
        o.push_str(&format!(
            "**[{}] {}**: {} ^t{i:04}\n\n",
            clock(u.start),
            u.speaker_name,
            u.text
        ));
    }
    o
}

/// Index `00 Lucina meetings.md`, newest first.
pub fn index(entries: &[(Meeting, String)], tz: Tz) -> String {
    let mut o = String::from("# Lucina meetings\n\n");
    for (m, base) in entries {
        o.push_str(&format!(
            "- [[{base}|{}]] · {} · {}\n",
            m.title,
            m.started_at.with_timezone(&tz).format("%Y-%m-%d %H:%M"),
            clock(m.duration_sec)
        ));
    }
    o
}

/// Publishes the note (vault folder `cfg.meetings_folder`). The path is recorded before the
/// push and kept for good (one note per meeting, like memo K7).
pub(crate) async fn note(
    app: &App,
    store: &MeetingStore,
    m: &Meeting,
    vault: &VaultTarget,
    b: &Brief,
    speakers: &[Speaker],
    utts: &[Utterance],
) -> Result<Meeting, StepError> {
    let tz = app.cfg.timezone;
    let target = VaultTarget {
        folder: app.cfg.meetings_folder.clone(),
        ..vault.clone()
    };
    let existing = m
        .obsidian_note
        .as_deref()
        .and_then(|p| p.rsplit('/').next())
        .and_then(|f| f.strip_suffix(".md"))
        .map(str::to_owned);
    let base = crate::kernel::note_base(existing, || {
        obsidian::safe_name(m.folder.rsplit('/').next().unwrap_or(&m.title))
    });
    let path = format!("{}/{base}.md", target.folder);
    let mut m = m.clone();
    if m.obsidian_note.as_deref() != Some(path.as_str()) {
        m = store.update(&m.id, |x| x.obsidian_note = Some(path.clone()))?;
    }
    let url = app.cfg.meeting_transcript_url(&m.id);
    let stage = store.work(&m).join("obsidian");
    let _ = tokio::fs::remove_dir_all(&stage).await;
    tokio::fs::create_dir_all(&stage).await?;
    tokio::fs::write(
        stage.join(format!("{base}.md")),
        note_md(&m, b, speakers, utts, tz, &url),
    )
    .await?;
    let mut entries: Vec<(Meeting, String)> = store
        .list()
        .into_iter()
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
    entries.sort_by_key(|e| std::cmp::Reverse(e.0.started_at));
    tokio::fs::write(stage.join("00 Lucina meetings.md"), index(&entries, tz)).await?;
    obsidian::push(
        &target,
        &obsidian::Bundle {
            stage: stage.clone(),
        },
    )
    .await?;
    let _ = tokio::fs::remove_dir_all(&stage).await;
    Ok(store.update(&m.id, |x| x.steps.noted_at = Some(chrono::Utc::now()))?)
}

/// Lucina's memory: one exchange in the owner's `secretary_meetings` session, written as the
/// owner's own (the recording came from his authenticated device).
pub async fn remember(
    mem: &lucina_memory::Memory,
    m: &Meeting,
    b: &Brief,
    speakers: &[Speaker],
    tz: Tz,
) -> Result<(), lucina_memory::MemoryError> {
    use lucina_memory::surface::OwnerIds;
    use lucina_memory::{TurnFacts, TurnSurface};
    let surface = TurnSurface::classify(
        &TurnFacts::secretary("web", true, None),
        &OwnerIds::default(),
    );
    let peer = mem
        .client()
        .map(|c| c.config().owner_peer_id.clone())
        .unwrap_or_else(|| "giuseppe".into());
    let who: Vec<String> = speakers
        .iter()
        .filter(|s| s.id != "me")
        .map(|s| {
            format!(
                "{}{}",
                s.display(),
                s.role
                    .as_deref()
                    .map(|r| format!(" ({})", role_label(r)))
                    .unwrap_or_default()
            )
        })
        .collect();
    let user = format!(
        "I recorded a meeting: \"{}\" on {} ({}), with {}.",
        m.title,
        m.started_at.with_timezone(&tz).format("%A %Y-%m-%d %H:%M"),
        clock(m.duration_sec),
        if who.is_empty() {
            "nobody else".into()
        } else {
            who.join(", ")
        }
    );
    let mut assistant = format!("Brief of \"{}\": {}", b.title, b.about);
    for d in &b.decisions {
        assistant.push_str(&format!("\nDecision: {}", d.text));
    }
    for i in &b.action_items {
        assistant.push_str(&format!(
            "\nTo do: {}{}{}",
            i.text,
            i.owner
                .as_deref()
                .map(|o| format!(" ({o})"))
                .unwrap_or_default(),
            i.due
                .as_deref()
                .map(|d| format!(", due {d}"))
                .unwrap_or_default()
        ));
    }
    let assistant: String = assistant.chars().take(6000).collect();
    mem.remember(&surface, &peer, "secretary_meetings", &user, &assistant)
        .await
}

/// Each track in the media library (album "Meetings"); best effort, idempotent per track.
pub async fn register_media(store: &MeetingStore, m: &Meeting, transcript_url: &str) {
    for t in m.tracks.iter().filter(|t| t.whole()) {
        let kind = super::model::track_kind(&t.name);
        let body = json!({
            "kind": if kind == "video" { "video" } else { "audio" },
            "title": format!("{} ({kind})", m.title),
            "created_at": m.started_at,
            "duration_ms": (m.duration_sec * 1000.0).round() as u64,
            "mime": t.mime,
            "path": store.track_path(m, &t.name),
            "album": "Meetings",
            "links": {
                "meeting": format!("/v1/meetings/{}", m.id),
                "transcript": transcript_url,
                "brief": store.dir(m).join("brief.md"),
            },
        });
        crate::library::put_external("meeting", &format!("{}-{kind}", m.id), &body).await;
    }
}

fn ready_title(lang: &str) -> &'static str {
    match lang {
        "it" => "Riunione pronta",
        "da" => "Møde klar",
        _ => "Meeting brief ready",
    }
}

/// The notification text: the brief's title and summary, the action items, the link.
pub fn message(m: &Meeting, b: &Brief, speakers: &[Speaker], url: &str) -> (String, String) {
    let n = brief::names(speakers);
    let mut body = format!("🗓 {}\n\n{}", b.title, b.about);
    if !b.action_items.is_empty() {
        body.push_str("\n\nAction items:");
        for i in b.action_items.iter().take(8) {
            let owner = i
                .owner
                .as_deref()
                .map(|o| n.get(o).cloned().unwrap_or_else(|| o.to_owned()));
            body.push_str(&format!(
                "\n• {}{}",
                i.text,
                owner.map(|o| format!(" ({o})")).unwrap_or_default()
            ));
        }
    }
    let unnamed = speakers
        .iter()
        .filter(|s| s.name.is_none() && s.id != "me")
        .count();
    if unnamed > 0 {
        body.push_str(&format!(
            "\n\n{unnamed} speaker(s) without a name: name them in the app."
        ));
    }
    body.push_str(&format!("\n\nTranscript: {url}"));
    (
        ready_title(m.language.as_deref().unwrap_or("en")).to_owned(),
        body,
    )
}

pub async fn notify(
    app: &App,
    m: &Meeting,
    b: &Brief,
    url: &str,
    telegram: bool,
) -> Result<Vec<&'static str>, String> {
    let speakers: Vec<Speaker> = crate::store::read_json::<super::worker::Labelled>(
        &app.meetings()
            .map_err(|e| e.to_string())?
            .work(m)
            .join("analysis.json"),
    )
    .ok()
    .flatten()
    .map(|l| l.speakers)
    .unwrap_or_default();
    let (title, body) = message(m, b, &speakers, url);
    send_message(
        &app.cfg.notify,
        &Outgoing {
            title: &title,
            body: &body,
            route: &format!("/meetings/{}", m.id),
            channel: "meeting",
            dedup_key: &format!("meeting:{}:{}", m.id, m.revision),
            data: json!({"meetingId": m.id}),
        },
        telegram,
    )
    .await
}

/// Asks once about each unknown voice heard in enough recordings (MT6; marker first).
pub async fn ask_unknown_voices(app: &App) {
    let after = app.cfg.diarize.as_ref().map_or(2, |c| c.ask_after);
    for u in app.voices.to_ask(after) {
        match app.voices.mark_asked(&u.id) {
            Ok(true) => {}
            _ => continue,
        }
        let mut seen: Vec<String> = u
            .sightings
            .iter()
            .map(|s| format!("• {} ({}, {})", s.title, s.at.format("%-d %b"), s.speaker))
            .collect();
        seen.dedup();
        let body = format!(
            "A voice you have not named was in {} recordings:\n{}\n\nWho is it? Name it in the app (Meetings → voices) or with POST /v1/meetings/voices/{}.",
            u.recordings(),
            seen.join("\n"),
            u.id
        );
        if let Err(e) = send_message(
            &app.cfg.notify,
            &Outgoing {
                title: "Who is this voice?",
                body: &body,
                route: &format!("/meetings/voices/{}", u.id),
                channel: "meeting",
                dedup_key: &format!("voice:{}", u.id),
                data: json!({"voiceId": u.id}),
            },
            true,
        )
        .await
        {
            tracing::warn!(voice = %u.id, error = %e, "voice question not delivered (asked at most once)");
        }
    }
}
