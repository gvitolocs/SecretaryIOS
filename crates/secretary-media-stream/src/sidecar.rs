//! Subtitle files next to a video, found the way VLC finds them (its
//! "subtitle autodetection fuzziness" at the default level): every subtitle
//! file in the video's folder and in the usual subtitle subfolders, best
//! name match first. A file whose name clearly belongs to another video in
//! the same folder is left to that video.
use crate::SourceSub;
use std::path::{Path, PathBuf};

/// Text formats (`sub` is MicroDVD unless an `.idx` sits next to it) and
/// image formats (`idx` = VobSub, `sup` = Blu-ray PGS).
pub const EXTENSIONS: &[&str] = &["srt", "vtt", "ass", "ssa", "sub", "idx", "sup"];
const VIDEO_EXTENSIONS: &[&str] = &["mp4", "m4v", "mov", "mkv", "webm", "avi", "mpg", "mpeg", "vob", "ts", "m2ts", "mts", "wmv"];
const SUBFOLDERS: &[&str] = &["subs", "subtitles", "sub", "subtitle", "undertekster", "sottotitoli"];

pub fn ext_of(path: &Path) -> String {
    path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase()
}

/// True for VobSub (`.idx`) and PGS (`.sup`): pictures that need OCR.
pub fn is_image(path: &Path) -> bool {
    matches!(ext_of(path).as_str(), "idx" | "sup")
}

fn stem(path: &Path) -> String {
    path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string()
}

fn norm(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

fn common_prefix(a: &str, b: &str) -> usize {
    a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count()
}

/// Language codes and names in subtitle file names, to ISO 639-1 where known.
fn language(token: &str) -> Option<&'static str> {
    Some(match token.to_ascii_lowercase().as_str() {
        "da" | "dan" | "dansk" | "danish" => "da",
        "en" | "eng" | "english" | "engelsk" => "en",
        "it" | "ita" | "italian" | "italiano" => "it",
        "de" | "deu" | "ger" | "german" | "deutsch" => "de",
        "fr" | "fra" | "fre" | "french" | "francais" => "fr",
        "es" | "spa" | "spanish" | "espanol" => "es",
        "sv" | "swe" | "swedish" | "svenska" => "sv",
        "no" | "nor" | "nob" | "norwegian" | "norsk" => "no",
        "nl" | "nld" | "dut" | "dutch" => "nl",
        "fi" | "fin" | "finnish" => "fi",
        "pt" | "por" | "portuguese" => "pt",
        "ja" | "jpn" | "japanese" => "ja",
        _ => return None,
    })
}

/// How well a subtitle file name fits a video: 3 = same name, 2 = the video's
/// name plus tags (`Film.da.srt`), 1 = a shared prefix, 0 = unrelated.
fn score(video_stem: &str, sub_stem: &str) -> u8 {
    let (v, s) = (norm(video_stem), norm(sub_stem));
    if v.is_empty() || s.is_empty() {
        0
    } else if sub_stem == video_stem || s == v {
        3
    } else if sub_stem.starts_with(video_stem) || s.starts_with(&v) {
        2
    } else if common_prefix(&s, &v) >= 6.min(v.len()) {
        1
    } else {
        0
    }
}

fn list(dir: &Path) -> Vec<PathBuf> {
    let Ok(read) = std::fs::read_dir(dir) else { return vec![] };
    let mut out: Vec<PathBuf> = read.flatten().map(|e| e.path()).collect();
    out.sort();
    out
}

/// The subtitle files for `video`, best match first.
pub fn discover(video: &Path) -> Vec<SourceSub> {
    let Some(dir) = video.parent() else { return vec![] };
    let video_stem = stem(video);
    let entries = list(dir);
    let others: Vec<String> = entries
        .iter()
        .filter(|p| p.as_path() != video && VIDEO_EXTENSIONS.contains(&ext_of(p).as_str()))
        .map(|p| stem(p))
        .collect();
    // (path, prefix shown in the label) for the folder and its subtitle subfolders.
    let mut candidates: Vec<(PathBuf, String)> = Vec::new();
    for p in &entries {
        if p.is_file() {
            candidates.push((p.clone(), String::new()));
        } else if p.is_dir() {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if SUBFOLDERS.contains(&name.to_ascii_lowercase().as_str()) {
                for q in list(p) {
                    if q.is_file() {
                        candidates.push((q, format!("{name}/")));
                    } else if q.is_dir() && norm(&stem(&q)) == norm(&video_stem) {
                        // Subs/<episode name>/*.srt (TV packs): all of them belong to this video.
                        for r in list(&q).into_iter().filter(|r| r.is_file()) {
                            candidates.push((r, format!("{name}/")));
                        }
                    }
                }
            }
        }
    }
    let mut found: Vec<(u8, SourceSub)> = Vec::new();
    for (path, prefix) in candidates {
        let ext = ext_of(&path);
        if !EXTENSIONS.contains(&ext.as_str()) {
            continue;
        }
        // A VobSub pair is opened through its .idx.
        if ext == "sub" && path.with_extension("idx").exists() {
            continue;
        }
        let sub_stem = stem(&path);
        let mine = score(&video_stem, &sub_stem);
        // Leave a file to another video in the folder when it matches that one better.
        let theirs = others.iter().map(|o| score(o, &sub_stem)).max().unwrap_or(0);
        let in_episode_folder = !prefix.is_empty() && path.parent().is_some_and(|p| norm(&stem(p)) == norm(&video_stem));
        if theirs > mine && !in_episode_folder {
            continue;
        }
        let rest = if mine >= 2 { sub_stem[video_stem.len().min(sub_stem.len())..].to_string() } else { sub_stem.clone() };
        let tokens: Vec<&str> = rest.split(['.', '_', '-', ' ']).filter(|t| !t.is_empty()).collect();
        let lang = tokens.iter().rev().find_map(|t| language(t)).map(str::to_string);
        let name = rest.trim_matches(|c: char| c == '.' || c == ' ' || c == '-' || c == '_');
        let label = format!("{prefix}{}", if name.is_empty() { "Subtitles" } else { name });
        let rank = if in_episode_folder { mine.max(2) } else { mine };
        found.push((rank, SourceSub { path, lang, label }));
    }
    // Best name match first; text before pictures; then by name.
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| is_image(&a.1.path).cmp(&is_image(&b.1.path))).then_with(|| a.1.path.cmp(&b.1.path)));
    found.into_iter().map(|(_, s)| s).collect()
}

/// `Film.eng.forced.srt`: only the lines spoken in another language.
pub fn forced(path: &Path) -> bool {
    stem(path).split(['.', '_', '-', ' ']).any(|t| t.eq_ignore_ascii_case("forced"))
}

/// The library's own subtitle list plus the discovered files it does not
/// already name.
pub fn merge(given: &[SourceSub], video: &Path) -> Vec<SourceSub> {
    let mut all = given.to_vec();
    for s in discover(video) {
        if !all.iter().any(|g| g.path == s.path) {
            all.push(s);
        }
    }
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(dir: &Path, names: &[&str]) {
        for n in names {
            let p = dir.join(n);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, b"x").unwrap();
        }
    }

    fn labels(subs: &[SourceSub]) -> Vec<(String, Option<String>, bool)> {
        subs.iter().map(|s| (s.label.clone(), s.lang.clone(), forced(&s.path))).collect()
    }

    #[test]
    fn folder_subs_like_vlc() {
        // The owner's layout: one Whisper .srt next to two cuts of the same film.
        let d = tempfile::tempdir().unwrap();
        touch(d.path(), &[
            "Pokemon10 - Dansk dub (AC3).mkv",
            "Pokemon10 - Dansk dub + Whisper dansk subs.mkv",
            "whisper-da.srt",
            "whisper-dan.BAD-loop.srt",
            "README.txt",
        ]);
        let subs = discover(&d.path().join("Pokemon10 - Dansk dub (AC3).mkv"));
        assert_eq!(labels(&subs), vec![
            ("whisper-da".into(), Some("da".into()), false),
            ("whisper-dan.BAD-loop".into(), Some("da".into()), false),
        ]);
        assert_eq!(discover(&d.path().join("Pokemon10 - Dansk dub + Whisper dansk subs.mkv")).len(), 2);
    }

    #[test]
    fn named_tags_subfolders_and_vobsub() {
        let d = tempfile::tempdir().unwrap();
        touch(d.path(), &[
            "Film.mkv",
            "Film.srt",
            "Film.eng.forced.srt",
            "Film.ita.ass",
            "Film.idx",
            "Film.sub",
            "Film.sup",
            "Other.mkv",
            "Other.srt",
            "Subs/Danish.srt",
            "Subs/Film/2_English.srt",
            "Subs/Other/1_Italian.srt",
        ]);
        let subs = discover(&d.path().join("Film.mkv"));
        let got: Vec<String> = subs.iter().map(|s| s.path.strip_prefix(d.path()).unwrap().display().to_string()).collect();
        assert_eq!(got, vec![
            "Film.srt",
            "Film.idx",
            "Film.sup",
            "Film.eng.forced.srt",
            "Film.ita.ass",
            "Subs/Film/2_English.srt",
            "Subs/Danish.srt",
        ]);
        assert_eq!(subs[3].lang.as_deref(), Some("en"));
        assert!(forced(&subs[3].path) && !forced(&subs[0].path));
        assert_eq!(subs[4].lang.as_deref(), Some("it"));
        assert_eq!(subs[5].lang.as_deref(), Some("en"));
        assert_eq!(subs[6].label, "Subs/Danish");
        assert_eq!(subs[6].lang.as_deref(), Some("da"));
        assert!(is_image(&subs[1].path) && is_image(&subs[2].path));
        // Other.srt stays with Other.mkv.
        assert!(discover(&d.path().join("Other.mkv")).iter().any(|s| s.path.ends_with("Other.srt")));
    }

    #[test]
    fn microdvd_sub_without_idx_is_text() {
        let d = tempfile::tempdir().unwrap();
        touch(d.path(), &["Film.avi", "Film.sub"]);
        let subs = discover(&d.path().join("Film.avi"));
        assert_eq!(subs.len(), 1);
        assert!(!is_image(&subs[0].path));
    }

    #[test]
    fn merge_keeps_given_first() {
        let d = tempfile::tempdir().unwrap();
        touch(d.path(), &["Film.mkv", "Film.da.srt"]);
        let given = vec![SourceSub { path: d.path().join("Film.da.srt"), lang: Some("da".into()), label: "Dansk".into() }];
        let all = merge(&given, &d.path().join("Film.mkv"));
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].label, "Dansk");
    }
}
