//! The catalog as a `secretary_media_stream::MediaSource`, so the player's routes (play tokens,
//! Range, HLS, subtitles, resume positions) stream the library's originals.

use std::path::{Path, PathBuf};

use chrono::DateTime;
use secretary_media_stream::{MediaSource, SourceItem, SourceSub, Video};
use tokio::sync::broadcast::error::RecvError;

use crate::db;
use crate::ingest::{self, Library};
use crate::thumbs;

pub struct CatalogSource(pub Library);

impl MediaSource for CatalogSource {
    async fn item(&self, id: &str) -> Option<SourceItem> {
        let lib = self.0.clone();
        let id = id.to_string();
        tokio::task::spawn_blocking(move || source_item(&lib, &id))
            .await
            .ok()
            .flatten()
    }

    async fn videos(&self) -> Vec<Video> {
        let lib = self.0.clone();
        tokio::task::spawn_blocking(move || videos(&lib))
            .await
            .unwrap_or_default()
    }

    /// A new item, or new bytes for one (a folder file replaced), may need preparing.
    async fn changed(&self) {
        let mut events = self.0.events.subscribe();
        loop {
            match events.recv().await {
                Ok(e) if matches!(e.kind, "item.added" | "item.updated") => return,
                Ok(_) => {}
                // Missed some: one of them may have been an add.
                Err(RecvError::Lagged(_)) => return,
                Err(RecvError::Closed) => std::future::pending().await,
            }
        }
    }
}

/// The catalog's films (not hidden, not trashed), newest added first. Only the
/// film folders are prepared: phone videos would be a lot of 4K tone-mapping.
pub fn videos(lib: &Library) -> Vec<Video> {
    let films = lib.cfg.film_folders.clone();
    let ids = lib
        .db
        .with(move |c| db::film_video_ids(c, &films))
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "listing films failed");
            Vec::new()
        });
    ids.into_iter()
        .filter_map(|(id, added_at)| {
            Some(Video {
                item: source_item(lib, &id)?,
                added_at: DateTime::parse_from_rfc3339(&added_at)
                    .map_or(0, |t| t.timestamp_millis().max(0) as u64),
            })
        })
        .collect()
}

pub fn source_item(lib: &Library, id: &str) -> Option<SourceItem> {
    let (path, item) = ingest::original_path(lib, id).ok()??;
    if item.trashed_at.is_some() {
        return None;
    }
    let dir = thumbs::dir_for(&lib.cfg, id);
    let existing = |f: &str| Some(dir.join(f)).filter(|p| p.is_file());
    let subtitles = if item.kind == "video" {
        sibling_subtitles(&path)
    } else {
        Vec::new()
    };
    Some(SourceItem {
        id: item.id,
        kind: item.kind,
        title: item.title,
        mime: item.mime,
        poster: existing("poster.jpg").or_else(|| existing("l.jpg")),
        thumb: existing("s.jpg"),
        path,
        subtitles,
    })
}

/// `Movie.srt`, `Movie.da.srt`, `Movie.English.vtt` next to `Movie.mkv`.
pub fn sibling_subtitles(video: &Path) -> Vec<SourceSub> {
    let (Some(dir), Some(stem)) = (video.parent(), video.file_stem().and_then(|s| s.to_str()))
    else {
        return Vec::new();
    };
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut subs: Vec<(String, PathBuf)> = rd
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter_map(|p| {
            let name = p.file_name()?.to_str()?.to_string();
            let ext = p.extension()?.to_str()?.to_ascii_lowercase();
            (matches!(ext.as_str(), "srt" | "vtt" | "ass" | "ssa") && name.starts_with(stem))
                .then_some((name, p))
        })
        .collect();
    subs.sort();
    subs.into_iter()
        .map(|(name, path)| {
            let middle = name[stem.len()..].trim_start_matches('.');
            let middle = middle.rsplit_once('.').map(|(m, _)| m).unwrap_or("");
            let lang =
                (!middle.is_empty() && middle.len() <= 3).then(|| middle.to_ascii_lowercase());
            let label = if middle.is_empty() {
                "Subtitles".to_string()
            } else {
                middle.to_string()
            };
            SourceSub { path, lang, label }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_sibling_subtitles() {
        let t = tempfile::tempdir().unwrap();
        for f in [
            "Film.mkv",
            "Film.srt",
            "Film.da.srt",
            "Film.English.vtt",
            "Other.srt",
        ] {
            std::fs::write(t.path().join(f), b"x").unwrap();
        }
        let subs = sibling_subtitles(&t.path().join("Film.mkv"));
        let labels: Vec<_> = subs
            .iter()
            .map(|s| (s.label.as_str(), s.lang.as_deref()))
            .collect();
        assert_eq!(
            labels,
            vec![("English", None), ("da", Some("da")), ("Subtitles", None)]
        );
    }

    #[test]
    fn videos_are_the_visible_films_newest_first() {
        let t = tempfile::tempdir().unwrap();
        let cfg = crate::config::Config::for_dirs(t.path().join("data"), t.path().join("store"));
        let lib = Library::open(cfg).unwrap();
        let film = |id: &str, kind: &str, hidden: i64, trashed: Option<&str>, added: &str| {
            let path = t.path().join(format!("{id}.mkv"));
            std::fs::write(&path, b"x").unwrap();
            lib.db
                .with(|c| {
                    c.execute(
                        "INSERT INTO blobs (hash, bytes, ext, location, path, created_at) VALUES (?1, 1, 'mkv', 'external', ?2, ?3)",
                        rusqlite::params![id, path.to_str(), added],
                    )?;
                    c.execute(
                        "INSERT INTO items (id, kind, title, hash, mime, bytes, sort_at, source, hidden, trashed_at, added_at, updated_at)
                         VALUES (?1, ?2, ?1, ?1, 'video/x-matroska', 1, ?3, 'folder', ?4, ?5, ?3, ?3)",
                        rusqlite::params![id, kind, added, hidden, trashed],
                    )
                })
                .unwrap();
        };
        film("old", "video", 0, None, "2026-01-01T00:00:00.000Z");
        film("new", "video", 0, None, "2026-10-01T00:00:00.000Z");
        film("live-motion", "video", 1, None, "2026-10-02T00:00:00.000Z");
        film(
            "binned",
            "video",
            0,
            Some("2026-10-03T00:00:00.000Z"),
            "2026-10-03T00:00:00.000Z",
        );
        film("memo", "audio", 0, None, "2026-10-04T00:00:00.000Z");
        // A phone video is a video a viewer can open, but not a film: it is not
        // in the film folder and must not be prepared.
        film("phone", "video", 0, None, "2026-10-05T00:00:00.000Z");
        lib.db
            .with(|c| {
                c.execute(
                    "INSERT INTO albums (id, title, system_key, created_at, updated_at)
                     VALUES ('film-album', 'Film', NULL, '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z')",
                    [],
                )?;
                for id in ["old", "new"] {
                    c.execute(
                        "INSERT INTO album_items (album_id, item_id, added_at)
                         VALUES ('film-album', ?1, '2026-01-01T00:00:00.000Z')",
                        rusqlite::params![id],
                    )?;
                }
                Ok(())
            })
            .unwrap();
        let got: Vec<(String, u64)> = videos(&lib)
            .into_iter()
            .map(|v| (v.item.id, v.added_at))
            .collect();
        assert_eq!(
            got,
            vec![
                ("new".to_string(), 1_790_812_800_000),
                ("old".to_string(), 1_767_225_600_000)
            ]
        );
    }
}
