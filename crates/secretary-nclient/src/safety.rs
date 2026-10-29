//! The hard child-safety rule (owner, 2026-10-26; not configurable, no switch, no env var).
//!
//! The plugin never fetches, shows, caches or returns anything that sexualises minors. Every
//! gallery goes through [`vet`] on the server, after the upstream answer is parsed and before
//! anything is written to the cache or sent to a client. The rule fails closed:
//!
//! - a gallery with no tag data, or with a tag id the tag cache cannot name, is dropped
//!   ([`Verdict::Unknown`]), never shown;
//! - any tag matching [`HARD_STEMS`] (substring) or [`HARD_WORDS`] (whole word) blocks it;
//! - a school-uniform tag ([`UNIFORM_TAGS`]) together with a child-context tag
//!   ([`CONTEXT_TAGS`]) blocks it;
//! - a title matching [`TITLE_STEMS`] blocks it, whatever the tags say.
//!
//! Over-blocking adult content is acceptable; letting one minor-sexualising gallery through is
//! not. Extend the lists, never shorten them without the owner. `tests` below pin every entry.

/// Substrings: a tag whose normalised name contains one of these is blocked. Covers
/// lolicon / low lolicon, shotacon / low shotacon, toddlercon and spelling variants.
pub const HARD_STEMS: &[&str] = &[
    "loli",
    "shota",
    "toddler",
    "infant",
    "pedo",
    "paedo",
    "underage",
    "under age",
    "preteen",
    "pre teen",
    "jailbait",
    "kindergarten",
    "randoseru",
    "elementary school",
    "grade school",
    "primary school",
    "middle school",
    "junior high",
    "age regression",
    "aged down",
    "age down",
    "chibi",
];

/// Whole words (after normalisation): the age-related tags.
pub const HARD_WORDS: &[&str] = &[
    "child",
    "children",
    "childs",
    "kid",
    "kids",
    "kiddie",
    "kiddy",
    "young",
    "younger",
    "youngest",
    "youth",
    "minor",
    "minors",
    "baby",
    "babies",
    "little girl",
    "little boy",
    "little sister",
    "little brother",
    "imouto",
    "otouto",
    "infantilism",
    "cub",
];

/// School uniforms: blocked together with any [`CONTEXT_TAGS`] tag.
pub const UNIFORM_TAGS: &[&str] = &[
    "school uniform",
    "schoolgirl uniform",
    "schoolboy uniform",
    "school swimsuit",
    "school gym uniform",
    "gym uniform",
    "bloomers",
    "sailor uniform",
    "school bag",
];

/// Child context next to a school uniform.
pub const CONTEXT_TAGS: &[&str] = &[
    "flat chest",
    "petite",
    "small body",
    "short stature",
    "tiny",
    "small",
    "smaller girl",
    "smaller boy",
];

/// Title substrings (normalised; Japanese and Chinese terms as written).
pub const TITLE_STEMS: &[&str] = &[
    "loli",
    "shota",
    "toddler",
    "jailbait",
    "pedo",
    "underage",
    "preteen",
    "kindergarten",
    "elementary school",
    "randoseru",
    "ロリ",
    "ショタ",
    "幼女",
    "幼児",
    "幼稚園",
    "園児",
    "小学生",
    "中学生",
    "子供",
    "こども",
    "児童",
    "萝莉",
    "正太",
    "幼儿",
    "儿童",
    "小学",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    HardTag,
    UniformInChildContext,
    Title,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Allowed,
    Blocked(Reason),
    /// Missing or unresolved tag data: dropped like a block.
    Unknown,
}

impl Verdict {
    pub fn allowed(self) -> bool {
        self == Verdict::Allowed
    }
}

/// Lower case, `-`/`_` → space, runs of whitespace collapsed, trimmed.
pub fn normalize(s: &str) -> String {
    let lowered: String = s
        .chars()
        .flat_map(char::to_lowercase)
        .map(|c| if c == '-' || c == '_' { ' ' } else { c })
        .collect();
    lowered.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn has_word(name: &str, word: &str) -> bool {
    // `name` and `word` are normalised: words are separated by single spaces.
    let padded = format!(" {name} ");
    padded.contains(&format!(" {word} "))
}

/// Is one tag name blocked on its own?
pub fn tag_is_hard(name: &str) -> bool {
    let n = normalize(name);
    HARD_STEMS.iter().any(|s| n.contains(s)) || HARD_WORDS.iter().any(|w| has_word(&n, w))
}

fn tag_in(name: &str, set: &[&str]) -> bool {
    let n = normalize(name);
    set.iter().any(|s| n == *s)
}

/// A tag name the filter will never offer in the tag picker (blocked alone or as half of the
/// uniform rule's pair).
pub fn tag_hidden(name: &str) -> bool {
    tag_is_hard(name) || tag_in(name, UNIFORM_TAGS)
}

pub fn title_blocked(title: &str) -> bool {
    let n = normalize(title);
    TITLE_STEMS.iter().any(|s| n.contains(s))
}

/// The decision. `tags` are the gallery's tag names, `None` when any tag is missing or
/// unresolved. `titles` are every title the gallery carries.
pub fn vet<'a>(tags: Option<&[String]>, titles: impl IntoIterator<Item = &'a str>) -> Verdict {
    let Some(tags) = tags else {
        return Verdict::Unknown;
    };
    if tags.is_empty() {
        return Verdict::Unknown;
    }
    if tags.iter().any(|t| tag_is_hard(t)) {
        return Verdict::Blocked(Reason::HardTag);
    }
    let uniform = tags.iter().any(|t| tag_in(t, UNIFORM_TAGS));
    let context = tags.iter().any(|t| tag_in(t, CONTEXT_TAGS));
    if uniform && context {
        return Verdict::Blocked(Reason::UniformInChildContext);
    }
    if titles.into_iter().any(title_blocked) {
        return Verdict::Blocked(Reason::Title);
    }
    Verdict::Allowed
}

/// The tags sent upstream as exclusions on every search (defence in depth: the server-side
/// [`vet`] is what decides).
pub const UPSTREAM_EXCLUDE: &[&str] = &["lolicon", "shotacon", "toddlercon", "loli", "shota"];

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn every_named_tag_is_blocked() {
        for t in [
            "lolicon",
            "Lolicon",
            "low lolicon",
            "shotacon",
            "low shotacon",
            "toddlercon",
            "loli",
            "shota",
            "LOLI-CON",
            "low_shotacon",
            "young",
            "child",
            "children",
            "kid",
            "age regression",
            "randoseru",
            "kindergarten uniform",
            "elementary school",
            "little girl",
            "minor",
            "underage",
            "infantilism",
            "middle school",
        ] {
            assert!(tag_is_hard(t), "{t}");
            assert_eq!(
                vet(Some(&tags(&["big breasts", t])), ["A title"]),
                Verdict::Blocked(Reason::HardTag),
                "{t}"
            );
        }
    }

    #[test]
    fn every_list_entry_blocks() {
        for s in HARD_STEMS.iter().chain(HARD_WORDS) {
            assert!(tag_is_hard(s), "{s}");
        }
        for s in TITLE_STEMS {
            assert!(title_blocked(s), "{s}");
        }
        for u in UNIFORM_TAGS {
            for c in CONTEXT_TAGS {
                assert_eq!(
                    vet(Some(&tags(&[u, c])), ["x"]),
                    Verdict::Blocked(Reason::UniformInChildContext)
                );
            }
        }
    }

    #[test]
    fn uniform_with_child_context_is_blocked() {
        assert_eq!(
            vet(Some(&tags(&["schoolgirl uniform", "flat chest"])), ["x"]),
            Verdict::Blocked(Reason::UniformInChildContext)
        );
        assert_eq!(
            vet(Some(&tags(&["schoolboy uniform", "Petite"])), ["x"]),
            Verdict::Blocked(Reason::UniformInChildContext)
        );
    }

    #[test]
    fn missing_tags_are_dropped() {
        assert_eq!(vet(None, ["fine"]), Verdict::Unknown);
        assert_eq!(vet(Some(&[]), ["fine"]), Verdict::Unknown);
    }

    #[test]
    fn titles_block() {
        assert_eq!(
            vet(Some(&tags(&["big breasts"])), ["My Loli Story"]),
            Verdict::Blocked(Reason::Title)
        );
        assert_eq!(
            vet(Some(&tags(&["big breasts"])), ["ok", "ロリ本"]),
            Verdict::Blocked(Reason::Title)
        );
    }

    #[test]
    fn adult_content_passes() {
        assert_eq!(
            vet(
                Some(&tags(&["big breasts", "milf", "english"])),
                ["Office Story", ""]
            ),
            Verdict::Allowed
        );
        // A uniform alone, or child context alone, is not the pair.
        assert!(vet(Some(&tags(&["sailor uniform", "big breasts"])), ["x"]).allowed());
        assert!(vet(Some(&tags(&["petite", "milf"])), ["x"]).allowed());
        // Whole-word rule: "kidnapping" is not "kid".
        assert!(!tag_is_hard("kidnapping"));
    }

    #[test]
    fn hidden_from_the_tag_picker() {
        assert!(tag_hidden("lolicon"));
        assert!(tag_hidden("school swimsuit"));
        assert!(!tag_hidden("milf"));
    }
}
