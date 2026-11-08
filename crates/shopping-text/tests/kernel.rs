//! The rules verification/verus/shopping_text.rs proves on its model, checked on the real code
//! over many generated texts (deterministic, no dependencies).

use shopping_text::{fold_word, split_items, strip_commands, MAX_PARTS, MAX_QUANTITY};

/// Words the strip may remove (VERBS, PREPS, DETS, list names, COURTESY), folded.
const COMMAND: &[&str] = &[
    "add", "please", "put", "buy", "aggiungi", "aggiungere", "metti", "compra", "comprare", "prendi", "tilfoej",
    "koeb", "saet", "husk", "to", "on", "onto", "in", "into", "for", "alla", "alle", "nella", "sulla", "sulle",
    "per", "a", "til", "paa", "i", "the", "my", "la", "mia", "min", "shopping", "list", "grocery", "lista",
    "della", "spesa", "indkoebslisten", "indkoebsliste", "listen", "indkoebssedlen", "grazie", "tak",
];

const VOCAB: &[&str] = &[
    "add", "to", "the", "shopping", "list", "aggiungi", "alla", "lista", "della", "spesa", "tilføj", "til",
    "indkøbslisten", "please", "latte", "uova", "pane", "Red", "Bull", "baguettes", "mælk", "e", "and", "og",
    "&", "+", ",", ";", "\n", "4", "4x", "x4", "x", "0", "10000", "9999", "kg", "g", "di", "of", "“", "!", ".",
];

struct Lcg(u64);

impl Lcg {
    fn next(&mut self, n: usize) -> usize {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 33) as usize) % n
    }
}

fn texts() -> Vec<String> {
    let mut rng = Lcg(0x5eed_1abe1);
    (0..20_000)
        .map(|_| {
            let n = 1 + rng.next(14);
            (0..n).map(|_| VOCAB[rng.next(VOCAB.len())]).collect::<Vec<_>>().join(" ")
        })
        .collect()
}

#[test]
fn strip_keeps_a_word_and_takes_only_command_words() {
    for t in texts() {
        let folded: Vec<String> = t.split_whitespace().map(fold_word).collect();
        if folded.is_empty() {
            continue;
        }
        let (lo, hi) = strip_commands(&folded);
        assert!(lo < hi && hi <= folded.len(), "S6 {t:?}");
        for w in folded[..lo].iter().chain(&folded[hi..]) {
            assert!(COMMAND.contains(&w.as_str()), "S7 {w:?} in {t:?}");
        }
    }
}

#[test]
fn parts_are_non_empty_labels_without_separators_and_bounded_quantities() {
    let known = |s: &str| s.eq_ignore_ascii_case("red bull");
    for t in texts() {
        let parts = split_items(&t, known);
        assert!(parts.len() <= MAX_PARTS, "S4 {t:?}");
        for p in &parts {
            assert!(!p.name.trim().is_empty(), "S1 {t:?}");
            assert!(!p.name.contains([',', ';', '\n', '\r']), "S2 {p:?} in {t:?}");
            if let Some(q) = p.quantity {
                assert!((1..=MAX_QUANTITY).contains(&q), "Q1 {p:?}");
            }
        }
        // A text with any non-command word never comes out empty.
        let has_item = t.split_whitespace().any(|w| {
            let f = fold_word(w);
            !f.is_empty() && !COMMAND.contains(&f.as_str()) && !["e", "and", "og", "&", "+"].contains(&f.as_str())
        });
        if has_item {
            assert!(!parts.is_empty(), "{t:?}");
        }
    }
}
