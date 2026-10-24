//! Which subtitle track to show: the owner's choice for that file, else the
//! language they last picked, else a forced track (foreign-language lines).
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// One track as the server lists it.
#[derive(Debug, Clone, Deserialize)]
pub struct TrackInfo {
    pub id: String,
    #[serde(default)]
    pub lang: Option<String>,
    #[serde(default)]
    pub forced: bool,
    /// `text` or `image` (OCR).
    #[serde(default)]
    pub kind: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SubtitlePrefs {
    /// Item id → track id, or `off`.
    #[serde(default)]
    per_item: BTreeMap<String, String>,
    /// ISO 639-1 language of the last track the owner turned on.
    #[serde(default)]
    lang: Option<String>,
}

pub const OFF: &str = "off";

/// ISO 639-2 and 639-1 codes to 639-1, so `dan` (in a file) matches `da` (a file name tag).
pub fn normalise_lang(lang: &str) -> String {
    let l = lang.to_ascii_lowercase();
    match l.as_str() {
        "dan" => "da",
        "eng" => "en",
        "ita" => "it",
        "deu" | "ger" => "de",
        "fra" | "fre" => "fr",
        "spa" => "es",
        "swe" => "sv",
        "nor" | "nob" | "nno" => "no",
        "nld" | "dut" => "nl",
        "fin" => "fi",
        "por" => "pt",
        "jpn" => "ja",
        other => other,
    }
    .to_string()
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Pick {
    pub track: Option<String>,
    /// `item` (chosen for this file), `language`, `forced` or `none`.
    pub reason: &'static str,
}

impl SubtitlePrefs {
    pub fn pick(&self, item: &str, tracks: &[TrackInfo]) -> Pick {
        if let Some(choice) = self.per_item.get(item) {
            if choice == OFF {
                return Pick { track: None, reason: "item" };
            }
            if tracks.iter().any(|t| &t.id == choice) {
                return Pick { track: Some(choice.clone()), reason: "item" };
            }
        }
        if let Some(lang) = &self.lang {
            let matching = |t: &&TrackInfo| t.lang.as_deref().is_some_and(|l| normalise_lang(l) == *lang);
            // Full tracks before forced ones, text before pictures, then server order.
            let best = tracks
                .iter()
                .filter(matching)
                .min_by_key(|t| (t.forced, t.kind.as_deref() == Some("image")));
            if let Some(t) = best {
                return Pick { track: Some(t.id.clone()), reason: "language" };
            }
        }
        if let Some(t) = tracks.iter().find(|t| t.forced) {
            return Pick { track: Some(t.id.clone()), reason: "forced" };
        }
        Pick { track: None, reason: "none" }
    }

    /// Remembers the owner's choice for this file; turning a track on also
    /// makes its language the default for other files.
    pub fn choose(&mut self, item: &str, track: Option<&str>, lang: Option<&str>) {
        self.per_item.insert(item.to_string(), track.unwrap_or(OFF).to_string());
        if track.is_some() {
            if let Some(l) = lang {
                self.lang = Some(normalise_lang(l));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(id: &str, lang: Option<&str>, forced: bool, kind: &str) -> TrackInfo {
        TrackInfo { id: id.into(), lang: lang.map(Into::into), forced, kind: Some(kind.into()) }
    }

    #[test]
    fn per_file_choice_then_language_then_forced() {
        let mut p = SubtitlePrefs::default();
        let film = vec![t("e2", Some("dan"), true, "image"), t("e3", Some("dan"), false, "image"), t("x0", Some("en"), false, "text")];
        assert_eq!(p.pick("a", &film), Pick { track: Some("e2".into()), reason: "forced" });
        p.choose("a", Some("e3"), Some("dan"));
        assert_eq!(p.pick("a", &film).track.as_deref(), Some("e3"));
        // Another film: Danish (the last language turned on), full text track first.
        let other = vec![t("x0", Some("da"), false, "text"), t("e2", Some("dan"), false, "image"), t("e4", Some("da"), true, "text")];
        assert_eq!(p.pick("b", &other), Pick { track: Some("x0".into()), reason: "language" });
        // Off for one file only.
        p.choose("b", None, None);
        assert_eq!(p.pick("b", &other), Pick { track: None, reason: "item" });
        assert_eq!(p.pick("c", &other).track.as_deref(), Some("x0"));
        // A remembered track that disappeared falls back to the language.
        p.choose("d", Some("gone"), Some("da"));
        assert_eq!(p.pick("d", &other).track.as_deref(), Some("x0"));
    }

    #[test]
    fn nothing_matching_is_off() {
        let p = SubtitlePrefs::default();
        assert_eq!(p.pick("a", &[t("x0", Some("en"), false, "text")]), Pick { track: None, reason: "none" });
    }
}
