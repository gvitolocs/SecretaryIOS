//! The catalog as a `secretary_media_stream::MediaSource`, so the player's routes (play tokens,
//! Range, HLS, subtitles, resume positions) stream the library's originals.

use std::path::{Path, PathBuf};

use secretary_media_stream::{MediaSource, SourceItem, SourceSub};

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
}
