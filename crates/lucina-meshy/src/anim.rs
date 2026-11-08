//! Meshy's animation library (`action_id`s for `POST /v1/animations`).
//!
//! A snapshot of the public library (`data/animations.tsv`, 678 actions,
//! 2026-10-22) resolves names offline and knows the **in-place** variants
//! (`<key>_inplace`): walks and runs that stay on the spot, which is what a
//! game state machine or Lucina's stage wants. `GET /v1/animations/library`
//! is free and can refresh a name the snapshot does not know.

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

const SNAPSHOT: &str = include_str!("../data/animations.tsv");

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Clip {
    pub id: i64,
    pub key: String,
    pub name: String,
    pub category: String,
    pub in_place: bool,
}

impl Clip {
    /// "Casual Walk (in place, #613)".
    pub fn label(&self) -> String {
        if self.in_place {
            format!("{} (in place, #{})", self.name, self.id)
        } else {
            format!("{} (#{})", self.name, self.id)
        }
    }
}

pub fn library() -> &'static [Clip] {
    static LIB: OnceLock<Vec<Clip>> = OnceLock::new();
    LIB.get_or_init(|| {
        SNAPSHOT
            .lines()
            .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
            .filter_map(|l| {
                let mut f = l.split('\t');
                let id = f.next()?.trim().parse().ok()?;
                let key = f.next()?.trim().to_owned();
                let name = f.next()?.trim().to_owned();
                let category = f.next().unwrap_or("").trim().to_owned();
                Some(Clip {
                    id,
                    in_place: key.ends_with("_inplace"),
                    key,
                    name,
                    category,
                })
            })
            .collect()
    })
}

pub fn by_id(id: i64) -> Option<&'static Clip> {
    library().iter().find(|c| c.id == id)
}

fn norm(s: &str) -> String {
    s.trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// The in-place variant of a clip, if Meshy has one.
pub fn in_place_variant(c: &Clip) -> Option<&'static Clip> {
    if c.in_place {
        return None;
    }
    let want = format!("{}_inplace", c.key).to_lowercase();
    library().iter().find(|x| x.key.to_lowercase() == want)
}

/// Everyday words (it/en/da) → a library key.
const ALIASES: &[(&str, &str)] = &[
    ("idle", "Idle"),
    ("stand", "Idle"),
    ("riposo", "Idle"),
    ("fermo", "Idle"),
    ("walk", "Casual_Walk"),
    ("walking", "Casual_Walk"),
    ("cammina", "Casual_Walk"),
    ("camminata", "Casual_Walk"),
    ("camminare", "Casual_Walk"),
    ("gå", "Casual_Walk"),
    ("run", "run_fast_3"),
    ("running", "run_fast_3"),
    ("corsa", "run_fast_3"),
    ("corri", "run_fast_3"),
    ("correre", "run_fast_3"),
    ("løb", "run_fast_3"),
    ("wave", "Wave_One_Hand"),
    ("hello", "Wave_One_Hand"),
    ("saluto", "Wave_One_Hand"),
    ("saluta", "Wave_One_Hand"),
    ("ciao", "Wave_One_Hand"),
    ("dance", "Boom_Dance"),
    ("ballo", "Boom_Dance"),
    ("balla", "Boom_Dance"),
    ("ballare", "Boom_Dance"),
    ("bow", "Formal_Bow"),
    ("inchino", "Formal_Bow"),
    ("cheer", "Victory_Cheer"),
    ("esulta", "Victory_Cheer"),
    ("vittoria", "Victory_Cheer"),
    ("talk", "Talk_with_Hands_Open"),
    ("parla", "Talk_with_Hands_Open"),
    ("nod", "Agree_Gesture"),
    ("agree", "Agree_Gesture"),
    ("annuisci", "Agree_Gesture"),
    ("sì con la testa", "Agree_Gesture"),
    ("jump", "Back_Jump"),
    ("salto", "Back_Jump"),
    ("salta", "Back_Jump"),
    ("strut", "Confident_Strut"),
    ("sfilata", "Red_Carpet_Walk"),
];

/// A clip from an id ("613", "#613") or a name ("walk", "Casual Walk",
/// "wave_one_hand"). With `prefer_in_place`, a clip that has an in-place
/// variant is replaced by it.
pub fn resolve(input: &str, prefer_in_place: bool) -> Result<Clip, String> {
    let raw = input.trim().trim_start_matches('#');
    let found: Option<&Clip> = if let Ok(id) = raw.parse::<i64>() {
        Some(by_id(id).ok_or_else(|| format!("no Meshy animation with id {id}"))?)
    } else {
        let n = norm(raw);
        if n.is_empty() {
            return Err("empty animation name".into());
        }
        let lib = library();
        let alias = ALIASES
            .iter()
            .find(|(a, _)| norm(a) == n)
            .and_then(|(_, k)| lib.iter().find(|c| c.key == *k));
        alias
            .or_else(|| {
                lib.iter()
                    .find(|c| norm(&c.key) == n || (norm(&c.name) == n && !c.in_place))
            })
            .or_else(|| lib.iter().find(|c| norm(&c.name) == n))
            .or_else(|| {
                // Substring: the shortest matching name, then the lowest id.
                lib.iter()
                    .filter(|c| {
                        !c.in_place && (norm(&c.name).contains(&n) || norm(&c.key).contains(&n))
                    })
                    .min_by_key(|c| (c.name.len(), c.id))
            })
    };
    let clip = found.ok_or_else(|| format!("no Meshy animation matches \"{}\"", input.trim()))?;
    if prefer_in_place {
        if let Some(v) = in_place_variant(clip) {
            return Ok(v.clone());
        }
    }
    Ok(clip.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_and_resolution() {
        assert!(library().len() > 600);
        assert_eq!(by_id(0).unwrap().key, "Idle");
        let walk = resolve("walk", true).unwrap();
        assert_eq!((walk.id, walk.in_place), (613, true));
        assert_eq!(resolve("camminata", false).unwrap().id, 30);
        assert_eq!(resolve("#290", true).unwrap().key, "Wave_One_Hand");
        assert_eq!(resolve("Formal Bow", true).unwrap().id, 41);
        // no in-place variant for a bow: the clip stays as is
        assert!(!resolve("bow", true).unwrap().in_place);
        assert_eq!(resolve("confident strut", true).unwrap().id, 621);
        assert!(resolve("99999", true).is_err());
        assert!(resolve("zzzz-no-such-move", true).is_err());
        assert_eq!(walk.label(), "Casual Walk (in place, #613)");
    }
}
