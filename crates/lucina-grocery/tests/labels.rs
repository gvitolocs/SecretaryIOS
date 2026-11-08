//! Items are labels (docs/design/shopping-labels.md), against the real catalog, no database.

use lucina_grocery::items::resolve_catalog_text;
use lucina_grocery::{split_text_items, CatalogIndex};

fn catalog() -> CatalogIndex {
    let raw = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/catalog/catalog.v1.json"))
        .expect("catalog");
    CatalogIndex::build(&serde_json::from_str(&raw).expect("catalog json"))
}

fn linked(idx: &CatalogIndex, text: &str) -> Option<String> {
    resolve_catalog_text(idx, text).and_then(|(o, _)| o.id)
}

#[test]
fn the_owners_sentence_becomes_five_linked_items() {
    let idx = catalog();
    for text in [
        "to shopping list patatine fritte, ricotta, redbull, mortadella e 4 baguettes",
        "Add to shopping list patatine fritte, ricotta, redbull, mortadella e 4 baguettes.",
        "add patatine fritte, ricotta, redbull, mortadella e 4 baguettes to the shopping list",
    ] {
        let parts = split_text_items(&idx, text);
        let got: Vec<(String, Option<u32>, Option<String>)> =
            parts.iter().map(|p| (p.name.clone(), p.quantity, linked(&idx, &p.name))).collect();
        let want = [
            ("patatine fritte", None, "french-fries"),
            ("ricotta", None, "ricotta"),
            ("redbull", None, "energy-drink"),
            ("mortadella", None, "mortadella"),
            ("baguettes", Some(4), "baguette"),
        ];
        let want: Vec<(String, Option<u32>, Option<String>)> =
            want.iter().map(|(n, q, id)| (n.to_string(), *q, Some(id.to_string()))).collect();
        assert_eq!(got, want, "{text}");
    }
}

#[test]
fn red_bull_is_the_energy_drink() {
    let idx = catalog();
    for t in ["Red Bull", "red bull", "REDBULL", "Monster"] {
        assert_eq!(linked(&idx, t).as_deref(), Some("energy-drink"), "{t}");
    }
}

#[test]
fn anything_but_an_exact_term_is_an_unrecognized_label() {
    let idx = catalog();
    for t in [
        "Redbul",
        "Pane di segale tostato",
        "Prosciutto crudo di Parma",
        "organic milk",
        "latte di avena fatto in casa",
        "to shopping list patatine fritte, ricotta, redbull, mortadella e 4 baguettes",
        "the milk",
        "bananas split",
    ] {
        assert_eq!(linked(&idx, t), None, "{t}");
        assert!(idx.find(t).is_none(), "{t}");
    }
}
