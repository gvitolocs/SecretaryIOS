//! Differential test: `avatar::validate_changes` against a plain-Rust copy of
//! the Verus kernel in `verification/verus/avatar_validation.rs` (same loops,
//! ghost code dropped). Generated settings, previous settings and wardrobes;
//! the verdicts (accept, which field is malformed, which id is unknown) must
//! agree.

use proptest::prelude::*;
use secretary_server::avatar::{validate_changes, AvatarSettings, Rule, Rules, Wardrobe};
use serde_json::json;

/// Plain-Rust copy of the Verus kernel.
mod kernel {
    pub const OUTFIT: u8 = 0;
    pub const HAIR: u8 = 1;
    pub const MAX_ID_LEN: usize = 40;

    pub struct Entry {
        pub slot: u8,
        pub kind: u8,
        pub id: Vec<u8>,
    }

    pub struct Kept {
        pub slot: u8,
        pub id: Vec<u8>,
    }

    #[derive(Debug, PartialEq, Eq)]
    pub enum Verdict {
        Accept,
        BadShape(usize),
        Unknown(usize),
    }

    pub fn valid_id(id: &[u8]) -> bool {
        if id.is_empty() || id.len() > MAX_ID_LEN {
            return false;
        }
        let c0 = id[0];
        if !((97..=122).contains(&c0) || (48..=57).contains(&c0)) {
            return false;
        }
        let mut i = 0;
        while i < id.len() {
            let c = id[i];
            if !((97..=122).contains(&c) || (48..=57).contains(&c) || c == 95 || c == 45) {
                return false;
            }
            i += 1;
        }
        true
    }

    fn known_contains(known: &[Vec<u8>], id: &[u8]) -> bool {
        known.iter().any(|k| k == id)
    }

    fn kept_contains(kept: &[Kept], slot: u8, id: &[u8]) -> bool {
        kept.iter().any(|k| k.slot == slot && k.id == id)
    }

    fn accepted(e: &Entry, ko: &[Vec<u8>], kh: &[Vec<u8>], kc: &[Vec<u8>]) -> bool {
        if e.kind == OUTFIT {
            ko.is_empty() || known_contains(ko, &e.id)
        } else if e.kind == HAIR {
            kh.is_empty() || e.id == b"natural" || known_contains(kh, &e.id)
        } else {
            kc.is_empty() || known_contains(kc, &e.id)
        }
    }

    pub fn validate_changes(
        es: &[Entry],
        kept: &[Kept],
        ko: &[Vec<u8>],
        kh: &[Vec<u8>],
        kc: &[Vec<u8>],
    ) -> Verdict {
        for (i, e) in es.iter().enumerate() {
            if !valid_id(&e.id) {
                return Verdict::BadShape(i);
            }
        }
        for (i, e) in es.iter().enumerate() {
            if !kept_contains(kept, e.slot, &e.id) && !accepted(e, ko, kh, kc) {
                return Verdict::Unknown(i);
            }
        }
        Verdict::Accept
    }
}

const FIELDS: [&str; 9] = [
    "outfit",
    "hair",
    "hairColor",
    "rules.gym.outfit",
    "rules.gym.hair",
    "rules.shopping.outfit",
    "rules.shopping.hair",
    "rules.default.outfit",
    "rules.default.hair",
];

/// The (slot, kind, id) entries in validate_changes' order.
fn entries(s: &AvatarSettings) -> Vec<(u8, u8, String)> {
    let r = &s.rules;
    let mut v = Vec::new();
    if let Some(o) = &s.outfit {
        v.push((0, 0, o.clone()));
    }
    v.push((3, 0, r.gym.outfit.clone()));
    v.push((5, 0, r.shopping.outfit.clone()));
    v.push((7, 0, r.default.outfit.clone()));
    if let Some(h) = &s.hair {
        v.push((1, 1, h.clone()));
    }
    v.push((4, 1, r.gym.hair.clone()));
    v.push((6, 1, r.shopping.hair.clone()));
    v.push((8, 1, r.default.hair.clone()));
    if let Some(c) = &s.hair_color {
        v.push((2, 2, c.clone()));
    }
    v
}

fn run_kernel(
    s: &AvatarSettings,
    w: &Wardrobe,
    prev: Option<&AvatarSettings>,
) -> (kernel::Verdict, Vec<(u8, u8, String)>) {
    let es = entries(s);
    let kes: Vec<kernel::Entry> = es
        .iter()
        .map(|(slot, kind, id)| kernel::Entry {
            slot: *slot,
            kind: *kind,
            id: id.as_bytes().to_vec(),
        })
        .collect();
    let kept: Vec<kernel::Kept> = prev
        .map(entries)
        .unwrap_or_default()
        .into_iter()
        .map(|(slot, _, id)| kernel::Kept {
            slot,
            id: id.into_bytes(),
        })
        .collect();
    let ids = |l: &[serde_json::Value]| -> Vec<Vec<u8>> {
        l.iter()
            .filter_map(|e| e.get("id").and_then(|v| v.as_str()))
            .map(|s| s.as_bytes().to_vec())
            .collect()
    };
    let v = kernel::validate_changes(
        &kes,
        &kept,
        &ids(&w.outfits),
        &ids(&w.hairstyles),
        &ids(&w.colors),
    );
    (v, es)
}

fn id_pool() -> BoxedStrategy<String> {
    prop_oneof![
        Just("uniform".to_owned()),
        Just("gym".to_owned()),
        Just("chef".to_owned()),
        Just("maid".to_owned()),
        Just("ponytail".to_owned()),
        Just("bun".to_owned()),
        Just("natural".to_owned()),
        Just("pink".to_owned()),
        Just("nope".to_owned()),
        Just("x_1".to_owned()),
        Just("Bad".to_owned()),
        Just(String::new()),
        Just("-dash".to_owned()),
        Just("a".repeat(40)),
        Just("a".repeat(41)),
        "[a-z0-9_-]{1,6}",
        "[ -~]{0,4}",
    ]
    .boxed()
}

fn valid_pool() -> BoxedStrategy<String> {
    id_pool()
        .prop_filter("shape-valid", |s| kernel::valid_id(s.as_bytes()))
        .boxed()
}

fn rule(ids: BoxedStrategy<String>) -> impl Strategy<Value = Rule> {
    (ids.clone(), ids).prop_map(|(outfit, hair)| Rule { outfit, hair })
}

fn settings(ids: BoxedStrategy<String>) -> impl Strategy<Value = AvatarSettings> {
    (
        proptest::option::of(ids.clone()),
        proptest::option::of(ids.clone()),
        proptest::option::of(ids.clone()),
        any::<bool>(),
        rule(ids.clone()),
        rule(ids.clone()),
        rule(ids),
    )
        .prop_map(
            |(outfit, hair, hair_color, auto, gym, shopping, default)| AvatarSettings {
                outfit,
                hair,
                hair_color,
                auto,
                rules: Rules {
                    gym,
                    shopping,
                    default,
                },
                updated_ms: 0,
                updated_by: String::new(),
                ..AvatarSettings::default()
            },
        )
}

fn list(ids: &[String]) -> Vec<serde_json::Value> {
    ids.iter().map(|id| json!({"id": id})).collect()
}

fn wardrobe() -> impl Strategy<Value = Wardrobe> {
    let l = || proptest::collection::vec(valid_pool(), 0..4);
    (l(), l(), l()).prop_map(|(o, h, c)| Wardrobe {
        outfits: list(&o),
        hairstyles: list(&h),
        colors: list(&c),
        ..Wardrobe::default()
    })
}

/// A next state that mostly keeps prev's values (so the "unchanged" path is hit).
fn next_from(prev: AvatarSettings) -> impl Strategy<Value = AvatarSettings> {
    let p = prev.clone();
    (
        settings(id_pool()),
        proptest::collection::vec(any::<bool>(), 9),
    )
        .prop_map(move |(fresh, keep)| {
            let mut s = fresh;
            if keep[0] {
                s.outfit = p.outfit.clone();
            }
            if keep[1] {
                s.hair = p.hair.clone();
            }
            if keep[2] {
                s.hair_color = p.hair_color.clone();
            }
            if keep[3] {
                s.rules.gym.outfit = p.rules.gym.outfit.clone();
            }
            if keep[4] {
                s.rules.gym.hair = p.rules.gym.hair.clone();
            }
            if keep[5] {
                s.rules.shopping.outfit = p.rules.shopping.outfit.clone();
            }
            if keep[6] {
                s.rules.shopping.hair = p.rules.shopping.hair.clone();
            }
            if keep[7] {
                s.rules.default.outfit = p.rules.default.outfit.clone();
            }
            if keep[8] {
                s.rules.default.hair = p.rules.default.hair.clone();
            }
            s
        })
}

fn check(
    next: &AvatarSettings,
    w: &Wardrobe,
    prev: Option<&AvatarSettings>,
) -> Result<(), TestCaseError> {
    let (k, es) = run_kernel(next, w, prev);
    let p = validate_changes(next, w, prev);
    match (&k, &p) {
        (kernel::Verdict::Accept, Ok(())) => {}
        (kernel::Verdict::BadShape(i), Err(e)) => {
            prop_assert_eq!(e.code.as_str(), "bad_request");
            let field = FIELDS[es[*i].0 as usize];
            prop_assert!(
                e.message.starts_with(&format!("{field}: ")),
                "{} vs {field}",
                e.message
            );
        }
        (kernel::Verdict::Unknown(i), Err(e)) => {
            let (_, kind, id) = &es[*i];
            let code =
                ["unknown_outfit", "unknown_hairstyle", "unknown_hair_color"][*kind as usize];
            prop_assert_eq!(e.code.as_str(), code);
            prop_assert!(
                e.message.contains(&format!("{id:?}")),
                "{} vs {id:?}",
                e.message
            );
        }
        _ => prop_assert!(
            false,
            "kernel {:?} vs production {:?}",
            k,
            p.map_err(|e| e.code)
        ),
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(4000))]

    #[test]
    fn validate_changes_matches_kernel(
        (prev, next) in settings(valid_pool()).prop_flat_map(|p| (Just(p.clone()), next_from(p))),
        w in wardrobe(),
        with_prev in any::<bool>(),
    ) {
        check(&next, &w, with_prev.then_some(&prev))?;
    }

    /// The theorem A1 on production: unchanged settings are always accepted.
    #[test]
    fn unchanged_settings_are_accepted(prev in settings(valid_pool()), w in wardrobe()) {
        prop_assert!(validate_changes(&prev, &w, Some(&prev)).is_ok());
    }
}

#[test]
fn defaults_with_an_empty_wardrobe() {
    let s = AvatarSettings::default();
    check(&s, &Wardrobe::default(), None).unwrap();
    check(&s, &Wardrobe::default(), Some(&s)).unwrap();
}
