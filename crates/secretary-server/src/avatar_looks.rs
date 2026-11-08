//! Lucina's saved looks (the character editor's presets, Custom Cast's
//! `SceneEditCharacterPresetWindow`): up to [`MAX_LOOKS`] named looks in
//! `GET /v1/avatar` (`looks`) and the one she wears now (`current`).
//!
//! A look is the stage's `Look` JSON (`packages/lucina-stage/src/editor/look.ts`,
//! version 1): body and face sliders 0..=100, colours `#rrggbb`, ids for the
//! hairstyle, accessories and outfit. The server checks its shape and size
//! only; ids that are not (or no longer) in the wardrobe are ignored by the
//! clients, so a look survives wardrobe changes.
//!
//! Patch fields (`PUT /v1/avatar`):
//! - `saveLook: {id?, name, look}` adds a look or replaces the one with that id
//!   (without an id, one is made from the name); the reply's `current` is
//!   unchanged unless `current` is also given;
//! - `deleteLook: "<id>"` removes one (and clears `current` if it pointed to it);
//! - `looks: [...]` replaces the whole list (reorder, bulk import);
//! - `current: "<id or name>" | null` picks the look she wears; a name matches
//!   case- and accent-insensitively, so Lucina can say "serata".

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::avatar::{valid_id, MAX_ID_LEN};

/// At most this many saved looks (Custom Cast: `PRESET_MAX = 20`).
pub const MAX_LOOKS: usize = 20;
/// A look's JSON may be at most this long.
pub const MAX_LOOK_BYTES: usize = 8 * 1024;
/// A look's name: 1..=40 characters after trimming.
pub const MAX_NAME_CHARS: usize = 40;

/// One saved look.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SavedLook {
    pub id: String,
    pub name: String,
    pub look: Value,
    #[serde(default)]
    pub updated_ms: u64,
}

/// `saveLook` / an entry of `looks` in a patch.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LookInput {
    pub id: Option<String>,
    pub name: String,
    pub look: Value,
}

const SLIDERS_BODY: &[&str] = &[
    "height",
    "head",
    "neckLength",
    "neckWidth",
    "shoulders",
    "bust",
    "waist",
    "hips",
    "armLength",
    "armThickness",
    "legLength",
    "legThickness",
    "handSize",
];
const SLIDERS_FACE: &[&str] = &[
    "eyeSize",
    "eyeWidth",
    "eyeHeight",
    "eyeSpacing",
    "eyeY",
    "eyeTilt",
    "browY",
    "browAngle",
    "mouthWidth",
    "mouthY",
    "faceWidth",
    "jaw",
    "cheeks",
    "noseY",
];
const MAKEUP: &[&str] = &["lipstick", "eyeliner", "eyeshadow", "blush"];
const ACC_SLOTS: &[&str] = &[
    "head", "hair", "ears", "eyes", "neck", "back", "wristL", "wristR",
];
const PART_SLOTS: &[&str] = &["top", "bottom", "onepiece", "legwear", "shoes", "gloves"];

fn hex(v: &Value) -> bool {
    v.as_str().is_some_and(|h| {
        h.len() == 7 && h.starts_with('#') && h[1..].bytes().all(|b| b.is_ascii_hexdigit())
    })
}

fn hex_or_null(v: &Value) -> bool {
    v.is_null() || hex(v)
}

fn id_or_null(v: &Value) -> bool {
    v.is_null() || v.as_str().is_some_and(valid_id)
}

fn slider(v: &Value) -> bool {
    v.as_f64().is_some_and(|n| (0.0..=100.0).contains(&n))
}

/// An object whose keys are all in `allowed`, each value passing `ok`.
fn map_of(v: &Value, allowed: &[&str], ok: impl Fn(&Value) -> bool) -> Result<(), String> {
    let Some(o) = v.as_object() else {
        return Err("must be an object".into());
    };
    for (k, v) in o {
        if !allowed.contains(&k.as_str()) {
            return Err(format!("unknown key {k:?}"));
        }
        if !ok(v) {
            return Err(format!("{k}: bad value"));
        }
    }
    Ok(())
}

/// Shape and size of a stage `Look` (all fields optional; unknown ones refused).
pub fn validate_look(look: &Value) -> Result<(), String> {
    let size = serde_json::to_vec(look)
        .map(|b| b.len())
        .unwrap_or(usize::MAX);
    if size > MAX_LOOK_BYTES {
        return Err(format!("look: at most {MAX_LOOK_BYTES} bytes"));
    }
    let Some(o) = look.as_object() else {
        return Err("look: must be an object".into());
    };
    let field = |name: &str, r: Result<(), String>| r.map_err(|e| format!("look.{name}: {e}"));
    for (k, v) in o {
        match k.as_str() {
            "v" => {
                if v.as_u64() != Some(1) {
                    return Err("look.v: must be 1".into());
                }
            }
            "body" => field(k, map_of(v, SLIDERS_BODY, slider))?,
            "face" => field(k, map_of(v, SLIDERS_FACE, slider))?,
            "eyes" => field(
                k,
                map_of(v, &["iris", "highlight"], |x| {
                    hex_or_null(x) || x.is_boolean()
                }),
            )?,
            "skin" => field(k, map_of(v, &["tone"], hex_or_null))?,
            "makeup" => field(
                k,
                map_of(v, MAKEUP, |m| {
                    map_of(m, &["color", "amount"], |x| hex(x) || slider(x)).is_ok()
                }),
            )?,
            "hair" => {
                let o = v.as_object().ok_or("look.hair: must be an object")?;
                for (hk, hv) in o {
                    let ok = match hk.as_str() {
                        "style" => id_or_null(hv),
                        "color" | "highlight" => hex_or_null(hv),
                        _ => return Err(format!("look.hair: unknown key {hk:?}")),
                    };
                    if !ok {
                        return Err(format!("look.hair.{hk}: bad value"));
                    }
                }
            }
            "accessories" => field(k, map_of(v, ACC_SLOTS, id_or_null))?,
            "tiara" => {
                if !v.is_boolean() {
                    return Err("look.tiara: must be a boolean".into());
                }
            }
            "outfit" => {
                let o = v.as_object().ok_or("look.outfit: must be an object")?;
                for (ok, ov) in o {
                    match ok.as_str() {
                        "id" if id_or_null(ov) => {}
                        "parts" => field("outfit.parts", map_of(ov, PART_SLOTS, id_or_null))?,
                        _ => return Err(format!("look.outfit.{ok}: bad key or value")),
                    }
                }
            }
            _ => return Err(format!("look: unknown key {k:?}")),
        }
    }
    Ok(())
}

/// A name, trimmed, 1..=[`MAX_NAME_CHARS`] characters, no control characters.
pub fn clean_name(name: &str) -> Result<String, String> {
    let n = name.trim();
    if n.is_empty() || n.chars().count() > MAX_NAME_CHARS || n.chars().any(char::is_control) {
        return Err(format!(
            "name: 1 to {MAX_NAME_CHARS} characters, no control characters"
        ));
    }
    Ok(n.to_owned())
}

/// Lower case, accents folded (common Latin ones), only `[a-z0-9]`, words joined by `-`.
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in name.chars().flat_map(char::to_lowercase) {
        let c = match c {
            'à' | 'á' | 'â' | 'ä' | 'ã' | 'å' => 'a',
            'è' | 'é' | 'ê' | 'ë' => 'e',
            'ì' | 'í' | 'î' | 'ï' => 'i',
            'ò' | 'ó' | 'ô' | 'ö' | 'õ' | 'ø' => 'o',
            'ù' | 'ú' | 'û' | 'ü' => 'u',
            'æ' => 'a',
            'ç' => 'c',
            'ñ' => 'n',
            c => c,
        };
        if c.is_ascii_alphanumeric() {
            if dash && !out.is_empty() {
                out.push('-');
            }
            dash = false;
            out.push(c);
        } else {
            dash = true;
        }
        if out.len() >= MAX_ID_LEN {
            break;
        }
    }
    out.truncate(MAX_ID_LEN);
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// A free id for `name` among `taken` (`serata`, `serata-2`, …; `look` when the name has no letters).
pub fn new_id(name: &str, taken: &[&str]) -> String {
    let base = match slug(name) {
        s if s.is_empty() => "look".to_owned(),
        s => s,
    };
    if !taken.contains(&base.as_str()) {
        return base;
    }
    (2..)
        .map(|n| {
            let suffix = format!("-{n}");
            let mut b = base.clone();
            b.truncate(MAX_ID_LEN - suffix.len());
            b + &suffix
        })
        .find(|c| !taken.contains(&c.as_str()))
        .unwrap_or(base)
}

/// The look `query` names: an exact id, else a name or id equal after [`slug`].
pub fn resolve<'a>(looks: &'a [SavedLook], query: &str) -> Option<&'a SavedLook> {
    if let Some(l) = looks.iter().find(|l| l.id == query) {
        return Some(l);
    }
    let q = slug(query);
    if q.is_empty() {
        return None;
    }
    if let Some(l) = looks
        .iter()
        .find(|l| slug(&l.name) == q || slug(&l.id) == q)
    {
        return Some(l);
    }
    // "serata" for "Look serata": every word of the query in the name, and only one such look.
    let words: Vec<&str> = q.split('-').collect();
    let mut hits = looks.iter().filter(|l| {
        let name = slug(&l.name);
        let have: Vec<&str> = name.split('-').collect();
        words.iter().all(|w| have.contains(w))
    });
    match (hits.next(), hits.next()) {
        (Some(l), None) => Some(l),
        _ => None,
    }
}

/// A checked [`SavedLook`] from input (`taken`: ids already used, for a new id).
pub fn saved(input: LookInput, taken: &[&str], now_ms: u64) -> Result<SavedLook, String> {
    let name = clean_name(&input.name)?;
    validate_look(&input.look)?;
    let id = match input.id {
        Some(id) if valid_id(&id) => id,
        Some(_) => {
            return Err(format!(
                "id: must match [a-z0-9][a-z0-9_-]* (at most {MAX_ID_LEN} characters)"
            ))
        }
        None => new_id(&name, taken),
    };
    Ok(SavedLook {
        id,
        name,
        look: input.look,
        updated_ms: now_ms,
    })
}

/// Checks a stored list (on load): ids unique and valid, at most [`MAX_LOOKS`], looks valid.
pub fn validate_list(looks: &[SavedLook], current: Option<&str>) -> Result<(), String> {
    if looks.len() > MAX_LOOKS {
        return Err(format!("looks: at most {MAX_LOOKS}"));
    }
    let mut seen = std::collections::HashSet::new();
    for l in looks {
        if !valid_id(&l.id) || !seen.insert(l.id.as_str()) {
            return Err(format!("looks: bad or duplicate id {:?}", l.id));
        }
        clean_name(&l.name)?;
        validate_look(&l.look)?;
    }
    if let Some(c) = current {
        if !seen.contains(c) {
            return Err(format!("current: no look {c:?}"));
        }
    }
    Ok(())
}

/// Why a looks patch was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LookError {
    /// Malformed input (400).
    Bad(String),
    /// `current` names no saved look (422 `unknown_look`).
    Unknown(String),
}

/// The look fields of a `PUT /v1/avatar` patch.
#[derive(Debug, Default)]
pub struct LooksPatch {
    pub current: Option<Option<String>>,
    pub looks: Option<Vec<LookInput>>,
    pub save: Option<LookInput>,
    pub delete: Option<String>,
}

impl LooksPatch {
    pub fn is_empty(&self) -> bool {
        self.current.is_none()
            && self.looks.is_none()
            && self.save.is_none()
            && self.delete.is_none()
    }

    /// Applies, in order: replace the list, delete, save, pick `current`.
    pub fn apply(
        self,
        looks: &mut Vec<SavedLook>,
        current: &mut Option<String>,
        now_ms: u64,
    ) -> Result<(), LookError> {
        if let Some(list) = self.looks {
            if list.len() > MAX_LOOKS {
                return Err(LookError::Bad(format!("looks: at most {MAX_LOOKS}")));
            }
            let mut out: Vec<SavedLook> = Vec::new();
            for input in list {
                let taken: Vec<&str> = out.iter().map(|l| l.id.as_str()).collect();
                let l = saved(input, &taken, now_ms).map_err(LookError::Bad)?;
                if out.iter().any(|o| o.id == l.id) {
                    return Err(LookError::Bad(format!("looks: duplicate id {:?}", l.id)));
                }
                out.push(l);
            }
            *looks = out;
        }
        if let Some(id) = self.delete {
            looks.retain(|l| l.id != id);
        }
        if let Some(input) = self.save {
            let taken: Vec<&str> = looks.iter().map(|l| l.id.as_str()).collect();
            let l = saved(input, &taken, now_ms).map_err(LookError::Bad)?;
            let full = looks.len() >= MAX_LOOKS;
            match looks.iter_mut().find(|o| o.id == l.id) {
                Some(o) => *o = l,
                None if full => {
                    return Err(LookError::Bad(format!(
                        "looks: at most {MAX_LOOKS}; delete one first"
                    )))
                }
                None => looks.push(l),
            }
        }
        match self.current {
            Some(None) => *current = None,
            Some(Some(q)) => match resolve(looks, &q) {
                Some(l) => *current = Some(l.id.clone()),
                None => return Err(LookError::Unknown(q)),
            },
            None => {}
        }
        if current
            .as_deref()
            .is_some_and(|c| !looks.iter().any(|l| l.id == c))
        {
            *current = None;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn looks_are_shape_checked() {
        assert!(validate_look(&json!({})).is_ok());
        let full = json!({
            "v": 1,
            "body": {"height": 60, "bust": 0},
            "face": {"eyeSize": 100},
            "eyes": {"iris": "#3d6fd1", "highlight": false},
            "skin": {"tone": null},
            "makeup": {"lipstick": {"color": "#d0445e", "amount": 40}},
            "hair": {"style": "ponytail", "color": null, "highlight": "#ffffff"},
            "accessories": {"head": "beret", "wristL": null},
            "tiara": false,
            "outfit": {"id": null, "parts": {"top": "casual-top"}}
        });
        assert_eq!(validate_look(&full), Ok(()));
        for bad in [
            json!([]),
            json!({"v": 2}),
            json!({"body": {"height": 101}}),
            json!({"body": {"tail": 50}}),
            json!({"face": {"eyeSize": "big"}}),
            json!({"eyes": {"iris": "blue"}}),
            json!({"makeup": {"glitter": {}}}),
            json!({"hair": {"style": "../x"}}),
            json!({"accessories": {"tail": "x"}}),
            json!({"outfit": {"parts": {"hat": "x"}}}),
            json!({"tiara": "yes"}),
            json!({"script": "<x>"}),
        ] {
            assert!(validate_look(&bad).is_err(), "{bad}");
        }
        let huge = json!({"body": {}, "hair": {"style": "x".repeat(MAX_LOOK_BYTES)}});
        assert!(validate_look(&huge).is_err());
    }

    #[test]
    fn ids_and_names() {
        assert_eq!(slug("Look Serata!"), "look-serata");
        assert_eq!(slug("Città di sera"), "citta-di-sera");
        assert_eq!(slug("  "), "");
        assert_eq!(new_id("Serata", &[]), "serata");
        assert_eq!(new_id("Serata", &["serata"]), "serata-2");
        assert_eq!(new_id("Serata", &["serata", "serata-2"]), "serata-3");
        assert_eq!(new_id("!!!", &[]), "look");
        assert_eq!(new_id(&"a".repeat(80), &[]).len(), MAX_ID_LEN);
        assert!(clean_name("").is_err());
        assert!(clean_name(&"x".repeat(41)).is_err());
        assert!(clean_name("a\u{7}b").is_err());
        assert_eq!(clean_name("  Gala  ").unwrap(), "Gala");
    }

    #[test]
    fn resolve_by_id_or_name() {
        let looks = vec![
            SavedLook {
                id: "evening".into(),
                name: "Look serata".into(),
                look: json!({}),
                updated_ms: 0,
            },
            SavedLook {
                id: "gym-day".into(),
                name: "Palestra".into(),
                look: json!({}),
                updated_ms: 0,
            },
        ];
        assert_eq!(resolve(&looks, "evening").unwrap().id, "evening");
        assert_eq!(resolve(&looks, "look serata").unwrap().id, "evening");
        assert_eq!(resolve(&looks, "LOOK SERATA").unwrap().id, "evening");
        assert_eq!(resolve(&looks, "Gym day").unwrap().id, "gym-day");
        assert_eq!(
            resolve(&looks, "serata").unwrap().id,
            "evening",
            "a word of the name"
        );
        assert!(resolve(&looks, "look").is_some(), "unique word");
        assert!(resolve(&looks, "nope").is_none());
        assert!(resolve(&looks, "").is_none());
    }
}
