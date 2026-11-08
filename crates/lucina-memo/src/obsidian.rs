//! The memo's note in the owner's Obsidian vault (canonical copy on nezopt's
//! MyBook disk, `/home/nez/mnt/mybook/Obsidian`, a local folder here; moved from the Pi on 2026-11-08).
//!
//! Layout under `<folder>` (default `Voice Memos/Lucina`, next to the older
//! Lectern imports in `Voice Memos/`):
//! - `<YYYY-MM-DD HHmm> <title>.md`: frontmatter, embedded audio, Lucina's
//!   summary, links to the PDF and app, then the transcript with one block id
//!   per line (`^s012`) that the key points link to;
//! - `Attachments/<same name>.m4a|.pdf`;
//! - `00 Lucina memos.md`: the index, newest first (rewritten each time).
//!
//! Files are staged locally and copied with `rsync` (no `--delete`: nothing
//! in the vault is ever removed from here). A note keeps its file name after
//! the first publish, so links to it stay valid when the title changes.
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use chrono_tz::Tz;
use tokio::process::Command;

use crate::model::{clock, Item, Memo, Summary, Transcript};
use crate::render::{labels, language_name};

#[derive(Debug, Clone)]
pub struct VaultTarget {
    /// `host:/path` (rsync over ssh) or a local directory.
    pub vault: String,
    /// Vault-relative folder for the memo notes.
    pub folder: String,
    pub ssh_opts: String,
}

impl VaultTarget {
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Option<Self> {
        let vault = get("LUCINA_MEMO_OBSIDIAN_VAULT")
            .unwrap_or_else(|| "/home/nez/mnt/mybook/Obsidian".into());
        let vault = vault.trim().trim_end_matches('/').to_owned();
        if vault.is_empty() || vault == "off" {
            return None;
        }
        Some(Self {
            vault,
            folder: get("LUCINA_MEMO_OBSIDIAN_FOLDER")
                .map(|f| f.trim().trim_matches('/').to_owned())
                .filter(|f| !f.is_empty() && !f.split('/').any(|p| p == ".." || p.is_empty()))
                .unwrap_or_else(|| "Voice Memos/Lucina".into()),
            ssh_opts: "ssh -o BatchMode=yes -o ConnectTimeout=10 -o ServerAliveInterval=15".into(),
        })
    }
    fn remote(&self) -> bool {
        self.vault.contains(':')
    }
}

/// A file name Obsidian links can carry: no `/\:*?"<>|#^[]`, at most 90 chars.
pub fn safe_name(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| {
            if "/\\:*?\"<>|#^[]{}".contains(c) || c.is_control() {
                ' '
            } else {
                c
            }
        })
        .collect();
    let squashed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let cut: String = squashed.chars().take(90).collect();
    let cut = cut.trim().trim_matches('.').trim().to_owned();
    if cut.is_empty() {
        "Voice memo".into()
    } else {
        cut
    }
}

pub fn base_name(m: &Memo, s: &Summary, tz: Tz) -> String {
    let tz = m
        .timezone
        .as_deref()
        .and_then(|z| z.parse::<Tz>().ok())
        .unwrap_or(tz);
    let when = m.recorded_at.with_timezone(&tz).format("%Y-%m-%d %H%M");
    safe_name(&format!("{when} {}", s.title))
}

fn yaml(s: &str) -> String {
    format!(
        "\"{}\"",
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', " ")
    )
}

/// Index of the transcript line an item points to: the one starting nearest
/// to it (the model's seconds are approximate), at most 2 s after it.
fn segment_for(t: &Transcript, at: f64) -> Option<usize> {
    t.segments
        .iter()
        .enumerate()
        .filter(|(_, s)| s.start <= at + 2.0)
        .min_by(|a, b| (a.1.start - at).abs().total_cmp(&(b.1.start - at).abs()))
        .map(|(i, _)| i)
}

fn item_line(i: &Item, t: &Transcript, me: &str, due: &str, todo: bool) -> String {
    let mut line = if todo {
        format!("- [ ] {}", i.text)
    } else {
        format!("- {}", i.text)
    };
    let mut meta = Vec::new();
    if let Some(o) = &i.owner {
        meta.push(if o.eq_ignore_ascii_case("me") {
            me.to_owned()
        } else {
            o.clone()
        });
    }
    if let Some(d) = &i.due {
        meta.push(format!("{due} {d}"));
    }
    if let Some(n) = i.at.and_then(|a| segment_for(t, a)) {
        meta.push(format!("[[#^s{n:03}|{}]]", clock(t.segments[n].start)));
    }
    if !meta.is_empty() {
        line.push_str(&format!(" ({})", meta.join(", ")));
    }
    line
}

/// The note's markdown. `base` is the file name without extension.
pub fn note(m: &Memo, s: &Summary, t: &Transcript, tz: Tz, base: &str, app_url: &str) -> String {
    let l = labels(&s.language);
    let ztz = m
        .timezone
        .as_deref()
        .and_then(|z| z.parse::<Tz>().ok())
        .unwrap_or(tz);
    let local = m.recorded_at.with_timezone(&ztz);
    let mut tags = vec!["voice-memos".to_owned(), "lucina".to_owned()];
    for t in &s.tags {
        if !tags.contains(t) {
            tags.push(t.clone());
        }
    }
    let mut o = String::new();
    o.push_str("---\n");
    o.push_str("type: voice-memo\n");
    o.push_str(&format!("created: {}\n", local.format("%Y-%m-%d")));
    o.push_str(&format!("recorded: {}\n", local.to_rfc3339()));
    o.push_str(&format!("duration: {}\n", yaml(&clock(t.duration_sec))));
    o.push_str(&format!("language: {}\n", t.language));
    o.push_str(&format!("title: {}\n", yaml(&s.title)));
    o.push_str(&format!("memo_id: {}\n", m.id));
    o.push_str("source: lucina\n");
    if let Some(e) = &m.event {
        o.push_str(&format!(
            "event: {}\nevent_start: {}\nevent_end: {}\nlocation: {}\nattendees: {}\nseries: {}\n",
            yaml(&e.title),
            yaml(&e.start),
            yaml(&e.end),
            yaml(e.location.as_deref().unwrap_or_default()),
            serde_json::to_string(&e.attendees).unwrap_or_else(|_| "[]".into()),
            yaml(&e.series)
        ));
        let label = match e.kind {
            crate::model::EventKind::Lecture => Some("course"),
            crate::model::EventKind::Meeting => Some("meeting"),
            _ => None,
        };
        if let Some(label) = label {
            o.push_str(&format!(
                "{label}: {}\n",
                yaml(&crate::events::series_title(&e.title))
            ));
        }
    }
    o.push_str(&format!(
        "audio: {}\n",
        yaml(&format!("[[Attachments/{base}.{}]]", m.ext))
    ));
    o.push_str(&format!(
        "pdf: {}\n",
        yaml(&format!("[[Attachments/{base}.pdf]]"))
    ));
    o.push_str(&format!("app: {}\n", yaml(app_url)));
    o.push_str(&format!("action_items: {}\n", s.action_items.len()));
    o.push_str("tags:\n");
    for t in &tags {
        o.push_str(&format!("  - {t}\n"));
    }
    o.push_str("---\n\n");
    o.push_str(&format!("# {}\n\n", s.title));
    if let Some(e) = &m.event {
        let time = |ms| {
            chrono::DateTime::from_timestamp_millis(ms)
                .map(|t| t.with_timezone(&ztz).format("%H:%M").to_string())
                .unwrap_or_default()
        };
        o.push_str(&format!(
            "Event: {} · {}–{} · {}\n\n",
            e.title,
            time(e.start_ms),
            time(e.end_ms),
            e.location.as_deref().unwrap_or_default()
        ));
    }
    if m.event
        .as_ref()
        .is_some_and(|e| e.kind == crate::model::EventKind::Meeting)
    {
        o.push_str("## Minutes\n\n");
    }
    o.push_str(&format!(
        "{} {} · {} {} · {} {}\n\n",
        l.recorded,
        local.format("%d/%m/%Y %H:%M"),
        l.duration,
        clock(t.duration_sec),
        l.language,
        language_name(&t.language)
    ));
    o.push_str(&format!("![[Attachments/{base}.{}]]\n\n", m.ext));
    o.push_str(&format!("> [!summary] {}\n", l.summary));
    for line in s.overview.lines() {
        o.push_str(&format!("> {line}\n"));
    }
    o.push('\n');
    let lecture = m
        .event
        .as_ref()
        .is_some_and(|e| e.kind == crate::model::EventKind::Lecture);
    let meeting = m
        .event
        .as_ref()
        .is_some_and(|e| e.kind == crate::model::EventKind::Meeting);
    let lists: [(&str, &[Item], bool); 5] = [
        (
            if lecture {
                "Key concepts"
            } else {
                l.key_points
            },
            &s.key_points,
            false,
        ),
        ("Definitions", &s.definitions, false),
        ("Questions to review", &s.questions, false),
        (
            if meeting { "Decisions" } else { l.decisions },
            &s.decisions,
            false,
        ),
        (
            if meeting {
                "Action items"
            } else {
                l.action_items
            },
            &s.action_items,
            true,
        ),
    ];
    for (title, items, todo) in lists {
        if items.is_empty() {
            continue;
        }
        o.push_str(&format!("## {title}\n\n"));
        for i in items {
            o.push_str(&item_line(i, t, l.owner_me, l.due, todo));
            o.push('\n');
        }
        o.push('\n');
    }
    o.push_str(&format!(
        "PDF: [[Attachments/{base}.pdf]] · [{}]({app_url})\n\n",
        l.listen
    ));
    o.push_str(&format!("## {}\n\n", l.transcript));
    o.push_str(&format!("*{} {}.*\n\n", l.machine_note, t.engine));
    for (n, seg) in t.segments.iter().enumerate() {
        let who = seg
            .speaker
            .as_ref()
            .map(|sp| format!("{sp}: "))
            .unwrap_or_default();
        o.push_str(&format!(
            "**{}** {who}{} ^s{n:03}\n\n",
            clock(seg.start),
            seg.text.trim()
        ));
    }
    o
}

/// The index note: one line per published memo, newest first.
pub fn index(entries: &[(Memo, String)], tz: Tz) -> String {
    let mut o = String::from("---\ntype: moc\nsource: lucina\ntags:\n  - voice-memos\n  - lucina\n---\n\n# Lucina memos\n\nVoice memos recorded in the Secretary app; Lucina writes the summary, the PDF and these notes.\n\n");
    for (m, base) in entries {
        let ztz = m
            .timezone
            .as_deref()
            .and_then(|z| z.parse::<Tz>().ok())
            .unwrap_or(tz);
        let when = m.recorded_at.with_timezone(&ztz).format("%Y-%m-%d");
        let preview = m
            .preview
            .as_deref()
            .map(|p| {
                let p: String = p.chars().take(140).collect();
                format!(" — {}", p.replace('\n', " "))
            })
            .unwrap_or_default();
        o.push_str(&format!(
            "- {when} [[{base}|{}]] ({}){preview}\n",
            m.title,
            clock(m.duration_sec)
        ));
    }
    o
}

#[derive(Debug, thiserror::Error)]
#[error("Obsidian publish failed: {0}")]
pub struct PublishError(pub String);

/// Files of one publish, relative to `<folder>`.
pub struct Bundle {
    pub stage: PathBuf,
}

/// Writes the note, attachments and index into a fresh stage directory.
pub async fn stage(
    stage: &Path,
    base: &str,
    note_md: &str,
    audio: &Path,
    ext: &str,
    pdf: &Path,
    index_md: &str,
) -> std::io::Result<Bundle> {
    let _ = tokio::fs::remove_dir_all(stage).await;
    tokio::fs::create_dir_all(stage.join("Attachments")).await?;
    tokio::fs::write(stage.join(format!("{base}.md")), note_md).await?;
    let link = |from: &Path, to: PathBuf| {
        let from = from.to_path_buf();
        async move {
            if tokio::fs::hard_link(&from, &to).await.is_err() {
                tokio::fs::copy(&from, &to).await?;
            }
            Ok::<_, std::io::Error>(())
        }
    };
    link(
        audio,
        stage.join("Attachments").join(format!("{base}.{ext}")),
    )
    .await?;
    link(pdf, stage.join("Attachments").join(format!("{base}.pdf"))).await?;
    tokio::fs::write(stage.join("00 Lucina memos.md"), index_md).await?;
    Ok(Bundle {
        stage: stage.to_path_buf(),
    })
}

/// Copies the stage into the vault folder (adds and updates, never deletes).
pub async fn push(target: &VaultTarget, bundle: &Bundle) -> Result<(), PublishError> {
    let src = format!("{}/", bundle.stage.display());
    if !target.remote() {
        let dest = Path::new(&target.vault).join(&target.folder);
        copy_tree(&bundle.stage, &dest)
            .await
            .map_err(|e| PublishError(e.to_string()))?;
        return Ok(());
    }
    let (host, path) = target.vault.split_once(':').unwrap_or((&target.vault, ""));
    let dest_dir = format!("{path}/{}", target.folder);
    // Create the folder first (rsync makes only the last level).
    let mk = Command::new("ssh")
        .args([
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=10",
            host,
            "mkdir",
            "-p",
            "--",
        ])
        .arg(shell_quote(&dest_dir))
        .stdin(Stdio::null())
        .output();
    let mk = tokio::time::timeout(Duration::from_secs(60), mk)
        .await
        .map_err(|_| PublishError("ssh mkdir timed out".into()))?
        .map_err(|e| PublishError(format!("ssh: {e}")))?;
    if !mk.status.success() {
        return Err(PublishError(format!(
            "ssh mkdir: {}",
            String::from_utf8_lossy(&mk.stderr).trim()
        )));
    }
    let out = Command::new("rsync")
        .args([
            "-rt",
            "--protect-args",
            "--chmod=D755,F644",
            "--timeout=60",
            "-e",
            &target.ssh_opts,
        ])
        .arg(&src)
        .arg(format!("{host}:{dest_dir}/"))
        .stdin(Stdio::null())
        .output();
    let out = tokio::time::timeout(Duration::from_secs(900), out)
        .await
        .map_err(|_| PublishError("rsync timed out".into()))?
        .map_err(|e| PublishError(format!("rsync: {e}")))?;
    if !out.status.success() {
        return Err(PublishError(format!(
            "rsync exit {:?}: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(())
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

async fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    tokio::fs::create_dir_all(to).await?;
    let mut rd = tokio::fs::read_dir(from).await?;
    while let Some(e) = rd.next_entry().await? {
        let p = e.path();
        let dest = to.join(e.file_name());
        if e.file_type().await?.is_dir() {
            Box::pin(copy_tree(&p, &dest)).await?;
        } else {
            tokio::fs::copy(&p, &dest).await?;
        }
    }
    Ok(())
}

/// The note's folder under the memo folder: `Courses/<course>` for a lecture,
/// `Meetings/<meeting>` for a meeting, none otherwise. Only the folder follows the event; the
/// file name stays the memo's (K7).
pub fn event_folder(m: &Memo) -> String {
    match &m.event {
        Some(e) => match e.kind {
            crate::model::EventKind::Lecture => format!(
                "Courses/{}",
                safe_name(&crate::events::series_title(&e.title))
            ),
            crate::model::EventKind::Meeting => format!(
                "Meetings/{}",
                safe_name(&crate::events::series_title(&e.title))
            ),
            _ => String::new(),
        },
        None => String::new(),
    }
}

/// Records that the memo's note now lives at `new` (before the push, K7): the path it had joins
/// the copies to delete after the push, and `new` is never among them. Several moves before a
/// delete went through keep every earlier path, so the vault ends with exactly one note.
pub fn move_note(m: &mut Memo, new: &str) {
    if let Some(old) = m.obsidian_note.take() {
        if old != new && !m.obsidian_old_notes.contains(&old) {
            m.obsidian_old_notes.push(old);
        }
    }
    m.obsidian_old_notes.retain(|o| o != new);
    m.obsidian_note = Some(new.to_owned());
}

/// Delete only the previously recorded note after its replacement was pushed.
/// Missing files are success, so replay after a crash converges.
pub async fn delete_note(target: &VaultTarget, note: &str) -> Result<(), PublishError> {
    if !note.starts_with(&format!("{}/", target.folder))
        || !note.ends_with(".md")
        || note.split('/').any(|p| p == "..")
    {
        return Err(PublishError("invalid old note path".into()));
    }
    if let Some((host, path)) = target.vault.split_once(':') {
        let output = tokio::time::timeout(
            Duration::from_secs(60),
            Command::new("ssh")
                .args([
                    "-o",
                    "BatchMode=yes",
                    "-o",
                    "ConnectTimeout=10",
                    host,
                    "rm",
                    "-f",
                    "--",
                ])
                .arg(shell_quote(&format!("{path}/{note}")))
                .stdin(Stdio::null())
                .output(),
        )
        .await
        .map_err(|_| PublishError("old note deletion timed out".into()))?
        .map_err(|e| PublishError(e.to_string()))?;
        if !output.status.success() {
            return Err(PublishError("old note deletion failed".into()));
        }
    } else {
        match tokio::fs::remove_file(Path::new(&target.vault).join(note)).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(PublishError(e.to_string())),
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::model::{Segment, Status, Steps};
    use chrono::{TimeZone, Utc};

    pub fn memo() -> Memo {
        let at = Utc.with_ymd_and_hms(2026, 10, 24, 12, 31, 0).unwrap();
        Memo {
            id: "mm_0123456789abcdef".into(),
            client_id: "c".into(),
            title: "Voice memo".into(),
            title_source: "auto".into(),
            recorded_at: at,
            timezone: Some("Europe/Copenhagen".into()),
            created_at: at,
            updated_at: at,
            duration_sec: 70.0,
            mime: "audio/mp4".into(),
            ext: "m4a".into(),
            size_bytes: 1,
            received_bytes: 1,
            sha256: None,
            source: "ios".into(),
            status: Status::Done,
            error: None,
            attempts: 0,
            next_attempt_at: None,
            language: Some("it".into()),
            engine: None,
            preview: Some("Parli del preventivo".into()),
            action_items: 1,
            tags: vec![],
            obsidian_note: None,
            steps: Steps::default(),
            revision: 0,
            link_checked_gen: None,
            archived_rev: None,
            archived_at: None,
            transcript_source: None,
            upgrade_attempts: 0,
            notify_telegram_rev: None,
            ended_at: None,
            location: None,
            event: None,
            event_source: None,
            obsidian_old_notes: Vec::new(),
        }
    }

    #[test]
    fn note_shape() {
        let m = memo();
        let t = Transcript {
            language: "it".into(),
            engine: "whisper.cpp large-v3 (GPU)".into(),
            duration_sec: 70.0,
            segments: vec![
                Segment {
                    start: 0.0,
                    end: 30.0,
                    text: "Allora, il preventivo.".into(),
                    speaker: None,
                    words: vec![],
                },
                Segment {
                    start: 30.0,
                    end: 70.0,
                    text: "Chiamo Marco lunedì.".into(),
                    speaker: None,
                    words: vec![],
                },
            ],
            speakers: false,
            created_at: Utc::now(),
            corrections: vec![],
        };
        let s = Summary {
            template: None,
            definitions: vec![],
            questions: vec![],
            title: "Preventivo: cucina/Marco".into(),
            language: "it".into(),
            overview: "Parli del preventivo.".into(),
            key_points: vec![],
            decisions: vec![],
            action_items: vec![Item {
                text: "Chiamare Marco".into(),
                at: Some(31.0),
                owner: Some("me".into()),
                due: Some("2026-10-27".into()),
            }],
            tags: vec!["casa".into()],
            model: "deepseek".into(),
            created_at: Utc::now(),
            instructions: None,
        };
        let base = base_name(&m, &s, chrono_tz::Europe::Copenhagen);
        assert_eq!(base, "2026-10-24 1431 Preventivo cucina Marco");
        let n = note(
            &m,
            &s,
            &t,
            chrono_tz::Europe::Copenhagen,
            &base,
            "https://getsecretary.tech/memos/mm_0123456789abcdef",
        );
        assert!(n.starts_with("---\ntype: voice-memo\ncreated: 2026-10-24\n"));
        assert!(n.contains("recorded: 2026-10-24T14:31:00+02:00"));
        assert!(n.contains("![[Attachments/2026-10-24 1431 Preventivo cucina Marco.m4a]]"));
        assert!(n.contains(
            "## Action items\n\n- [ ] Chiamare Marco (you, due 2026-10-27, [[#^s001|0:30]])"
        ));
        assert!(n.contains("**0:30** Chiamo Marco lunedì. ^s001"));
        assert!(n.contains("  - casa\n"));
        let idx = index(&[(m, base.clone())], chrono_tz::Europe::Copenhagen);
        assert!(idx.contains(&format!("[[{base}|Voice memo]] (1:10)")));
    }

    #[test]
    fn names() {
        assert_eq!(safe_name("a/b: c#d [e]"), "a b c d e");
        assert_eq!(safe_name("..."), "Voice memo");
    }

    #[test]
    fn moving_notes_keeps_every_old_copy_to_delete() {
        let mut m = memo();
        move_note(&mut m, "VM/a.md");
        assert!(m.obsidian_old_notes.is_empty());
        // Linked to a lecture: the note moves; the first copy is deleted after the push.
        move_note(&mut m, "VM/Courses/Algorithms/a.md");
        assert_eq!(m.obsidian_old_notes, ["VM/a.md"]);
        // The delete did not happen (crash) and the owner picks a meeting: both old copies stay listed.
        move_note(&mut m, "VM/Meetings/Sync/a.md");
        assert_eq!(
            m.obsidian_old_notes,
            ["VM/a.md", "VM/Courses/Algorithms/a.md"]
        );
        // Back to where it started: that path is the note again, never deleted.
        move_note(&mut m, "VM/a.md");
        assert_eq!(
            m.obsidian_old_notes,
            ["VM/Courses/Algorithms/a.md", "VM/Meetings/Sync/a.md"]
        );
        assert_eq!(m.obsidian_note.as_deref(), Some("VM/a.md"));
    }

    #[tokio::test]
    async fn deletes_only_notes_inside_the_memo_folder() {
        let dir = tempfile::tempdir().unwrap();
        let target = VaultTarget {
            vault: dir.path().display().to_string(),
            folder: "VM".into(),
            ssh_opts: String::new(),
        };
        std::fs::create_dir_all(dir.path().join("VM/Courses/A")).unwrap();
        std::fs::write(dir.path().join("VM/Courses/A/n.md"), "x").unwrap();
        delete_note(&target, "VM/Courses/A/n.md").await.unwrap();
        assert!(!dir.path().join("VM/Courses/A/n.md").exists());
        // Already gone: fine (a retry after a crash).
        delete_note(&target, "VM/Courses/A/n.md").await.unwrap();
        for bad in ["Other/n.md", "VM/../x.md", "VM/n.txt"] {
            assert!(delete_note(&target, bad).await.is_err(), "{bad}");
        }
    }

    #[test]
    fn folders_follow_the_event() {
        let mut m = memo();
        assert_eq!(event_folder(&m), "");
        let link = |kind| crate::model::EventLink {
            id: "e".into(),
            source: "google".into(),
            calendar: None,
            title: "Algorithms – Lecture 5".into(),
            start: String::new(),
            end: String::new(),
            start_ms: 0,
            end_ms: 0,
            location: None,
            attendees: vec![],
            recurring: false,
            series: "algorithms".into(),
            kind,
        };
        m.event = Some(link(crate::model::EventKind::Lecture));
        assert_eq!(event_folder(&m), "Courses/Algorithms");
        m.event = Some(link(crate::model::EventKind::Meeting));
        assert_eq!(event_folder(&m), "Meetings/Algorithms");
        m.event = Some(link(crate::model::EventKind::Other));
        assert_eq!(event_folder(&m), "");
    }
}
