//! Free text to shopping-list items (docs/design/shopping-labels.md).
//!
//! The owner's rule: an item is a label. A text links to a catalog item only on an exact
//! match (decided by the caller's `is_known`), and a sentence never becomes one item:
//! [`split_items`] drops command words ("add", "to the shopping list", "aggiungi alla lista
//! della spesa", "tilføj … til indkøbslisten"), splits on commas, semicolons, line breaks
//! and conjunctions, and reads a leading quantity ("4 baguettes", "500 g burro").
//!
//! Pure and dependency-free. Verus model: `verification/verus/shopping_text.rs`. The web
//! (`web/src/lib/shoppingText.ts`) and iOS (`ShoppingText.swift`) twins pass the same
//! cases (`tests/fixtures/shopping-split-cases.json`).

/// At most this many items come out of one text.
pub const MAX_PARTS: usize = 50;
/// The largest quantity read from text.
pub const MAX_QUANTITY: u32 = 9999;

/// One item of a text: the owner's words as its label, and a quantity/unit when given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemPart {
    pub name: String,
    pub quantity: Option<u32>,
    pub unit: Option<String>,
}

impl ItemPart {
    fn plain(name: String) -> Self {
        ItemPart {
            name,
            quantity: None,
            unit: None,
        }
    }
}

const VERBS: &[&str] = &[
    "add", "please", "put", "buy", "aggiungi", "aggiungere", "metti", "compra", "comprare", "prendi",
    "tilfoej", "koeb", "saet", "husk",
];
const COURTESY: &[&str] = &["please", "grazie", "tak"];
const PREPS: &[&str] = &[
    "to", "on", "onto", "in", "into", "for", "alla", "alle", "nella", "sulla", "sulle", "per", "a",
    "til", "paa", "i",
];
const DETS: &[&str] = &["the", "my", "la", "mia", "min"];
/// List names: (words, may stand without a preposition).
const LISTS: &[(&[&str], bool)] = &[
    (&["shopping", "list"], true),
    (&["grocery", "list"], true),
    (&["list"], false),
    (&["shopping"], false),
    (&["lista", "della", "spesa"], true),
    (&["lista", "spesa"], true),
    (&["lista"], false),
    (&["spesa"], false),
    (&["indkoebslisten"], true),
    (&["indkoebsliste"], true),
    (&["listen"], false),
    (&["indkoebssedlen"], false),
];
const CONJUNCTIONS: &[&str] = &["e", "ed", "and", "og", "&", "+"];
const UNITS: &[&str] = &["g", "gr", "hg", "kg", "l", "lt", "dl", "cl", "ml", "pz", "pcs", "pc", "stk"];
const UNIT_LINKS: &[&str] = &["di", "d", "of"];

/// Punctuation trimmed from a word before comparing it.
fn word_trim(c: char) -> bool {
    matches!(c, ':' | ',' | '.' | ';' | '!' | '?' | '"' | '\'' | '“' | '”' | '«' | '»')
}

/// Punctuation and spaces trimmed from each item.
fn part_trim(c: char) -> bool {
    c.is_whitespace() || matches!(c, '.' | ';' | ':' | '!' | '?' | '"' | '\'' | '“' | '”' | '«' | '»')
}

/// One character folded the catalog's way (lowercase, æ→ae ø→oe å→aa œ→oe ß→ss ð→d þ→th
/// ł→l, Latin accents dropped).
fn fold_char(c: char, out: &mut String) {
    for l in c.to_lowercase() {
        match l {
            'æ' => out.push_str("ae"),
            'ø' => out.push_str("oe"),
            'å' => out.push_str("aa"),
            'œ' => out.push_str("oe"),
            'ß' => out.push_str("ss"),
            'ð' => out.push('d'),
            'þ' => out.push_str("th"),
            'ł' => out.push('l'),
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'ā' => out.push('a'),
            'ç' | 'ć' | 'č' => out.push('c'),
            'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ę' | 'ě' => out.push('e'),
            'ì' | 'í' | 'î' | 'ï' | 'ī' => out.push('i'),
            'ñ' | 'ń' | 'ň' => out.push('n'),
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ō' => out.push('o'),
            'ù' | 'ú' | 'û' | 'ü' | 'ū' | 'ů' => out.push('u'),
            'ý' | 'ÿ' => out.push('y'),
            'š' | 'ś' => out.push('s'),
            'ž' | 'ź' | 'ż' => out.push('z'),
            'ř' => out.push('r'),
            _ => out.push(l),
        }
    }
}

/// A word as the command tables compare it: surrounding punctuation trimmed, then folded.
pub fn fold_word(word: &str) -> String {
    let mut out = String::new();
    for c in word.trim_matches(word_trim).chars() {
        fold_char(c, &mut out);
    }
    out
}

/// A whole text folded like the catalog's `fold()` for the letters this crate knows:
/// folded characters, runs of anything but letters and digits as one space, trimmed.
pub fn fold_text(text: &str) -> String {
    let mut folded = String::new();
    for c in text.chars() {
        fold_char(c, &mut folded);
    }
    let mut out = String::new();
    for w in folded.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()) {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(w);
    }
    out
}

fn is_in(set: &[&str], w: &str) -> bool {
    set.contains(&w)
}

fn starts_with(words: &[String], seq: &[&str]) -> bool {
    words.len() >= seq.len() && words.iter().zip(seq).all(|(a, b)| a == b)
}

/// Lengths of every list phrase (`[PREP] [DET] LIST`) at the start of `f`.
fn list_phrases(f: &[String]) -> Vec<usize> {
    let mut out = Vec::new();
    let has_prep = f.first().is_some_and(|w| is_in(PREPS, w));
    for prep in [false, true] {
        if prep && !has_prep {
            continue;
        }
        let p = usize::from(prep);
        for det in [false, true] {
            let k = p + usize::from(det);
            if det && !f.get(p).is_some_and(|w| is_in(DETS, w)) {
                continue;
            }
            for (seq, alone) in LISTS {
                if (prep || *alone) && f.len() >= k && starts_with(&f[k..], seq) {
                    out.push(k + seq.len());
                }
            }
        }
    }
    out
}

/// Words a leading command takes (longest), or 0.
fn lead_len(f: &[String]) -> usize {
    let verb = usize::from(f.first().is_some_and(|w| is_in(VERBS, w)));
    list_phrases(f).into_iter().fold(verb, usize::max)
}

/// Words a trailing command takes (longest), or 0.
fn trail_len(f: &[String]) -> usize {
    let mut best = usize::from(f.last().is_some_and(|w| is_in(COURTESY, w)));
    for i in 0..f.len() {
        if list_phrases(&f[i..]).contains(&(f.len() - i)) {
            best = best.max(f.len() - i);
            break;
        }
    }
    best
}

/// The words left once leading and trailing command words are gone: `lo..hi`, never empty
/// for a non-empty input (a strip that would take every word is not made).
pub fn strip_commands(folded: &[String]) -> (usize, usize) {
    let (mut lo, mut hi) = (0, folded.len());
    loop {
        let mut changed = false;
        let n = lead_len(&folded[lo..hi]);
        if n > 0 && lo + n < hi {
            lo += n;
            changed = true;
        }
        let n = trail_len(&folded[lo..hi]);
        if n > 0 && lo + n < hi {
            hi -= n;
            changed = true;
        }
        if !changed {
            return (lo, hi);
        }
    }
}

/// A segment's words split at conjunctions that have a word on both sides.
pub fn split_conjunctions<'a>(words: &[&'a str]) -> Vec<Vec<&'a str>> {
    let mut groups = Vec::new();
    let mut cur: Vec<&'a str> = Vec::new();
    for (i, w) in words.iter().enumerate() {
        if is_in(CONJUNCTIONS, &fold_word(w)) && !cur.is_empty() && i + 1 < words.len() {
            groups.push(std::mem::take(&mut cur));
        } else {
            cur.push(w);
        }
    }
    if !cur.is_empty() {
        groups.push(cur);
    }
    groups
}

/// `N` with 1-4 ASCII digits and a value of 1..=MAX_QUANTITY.
fn number(s: &str) -> Option<u32> {
    if s.is_empty() || s.len() > 4 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse::<u32>().ok().filter(|n| (1..=MAX_QUANTITY).contains(n))
}

fn times_prefix(s: &str) -> Option<&str> {
    s.strip_prefix('x').or_else(|| s.strip_prefix('×'))
}

fn times_suffix(s: &str) -> Option<&str> {
    s.strip_suffix('x').or_else(|| s.strip_suffix('×'))
}

/// `(quantity, unit, name words)` of an item's words, or `None` when it names no quantity
/// or nothing would be left as its name.
pub fn parse_quantity<'a>(words: &[&'a str]) -> Option<(u32, Option<String>, Vec<&'a str>)> {
    let first = *words.first()?;
    let lead = number(first)
        .map(|n| {
            let times = words.get(1).is_some_and(|w| *w == "x" || *w == "×");
            (n, 1 + usize::from(times))
        })
        .or_else(|| times_suffix(first).and_then(number).map(|n| (n, 1)))
        .or_else(|| times_prefix(first).and_then(number).map(|n| (n, 1)));
    if let Some((n, mut next)) = lead {
        let mut unit = None;
        if let Some(u) = words.get(next).map(|w| fold_word(w)).filter(|u| is_in(UNITS, u)) {
            unit = Some(u);
            next += 1;
            if words.get(next).is_some_and(|w| is_in(UNIT_LINKS, &fold_word(w))) {
                next += 1;
            }
        }
        return (next < words.len()).then(|| (n, unit, words[next..].to_vec()));
    }
    if words.len() >= 2 {
        let last = words[words.len() - 1];
        if let Some(n) = times_prefix(last).and_then(number) {
            return Some((n, None, words[..words.len() - 1].to_vec()));
        }
    }
    None
}

fn part(text: &str, is_known: &impl Fn(&str) -> bool) -> Option<ItemPart> {
    let t = text.trim_matches(part_trim);
    if t.is_empty() {
        return None;
    }
    if is_known(t) {
        return Some(ItemPart::plain(t.to_owned()));
    }
    let words: Vec<&str> = t.split_whitespace().collect();
    Some(match parse_quantity(&words) {
        Some((n, unit, name)) => ItemPart {
            name: name.join(" "),
            quantity: Some(n),
            unit,
        },
        None => ItemPart::plain(t.to_owned()),
    })
}

/// The items of a free text (see the module docs). `is_known(s)`: `s` is exactly a
/// catalog name or alias (or one of the owner's own items), so it stays whole.
pub fn split_items(text: &str, is_known: impl Fn(&str) -> bool) -> Vec<ItemPart> {
    let unified: String = text.chars().map(|c| if c == '\n' || c == '\r' { ',' } else { c }).collect();
    let words: Vec<&str> = unified.split_whitespace().collect();
    if words.is_empty() {
        return Vec::new();
    }
    let whole = words.join(" ");
    if is_known(&whole) {
        return part(&whole, &is_known).into_iter().collect();
    }
    let folded: Vec<String> = words.iter().map(|w| fold_word(w)).collect();
    let (lo, hi) = strip_commands(&folded);
    let rest = words[lo..hi].join(" ");
    let mut out = Vec::new();
    for segment in rest.split([',', ';']) {
        let seg = segment.trim();
        if seg.is_empty() {
            continue;
        }
        let groups = if is_known(seg) {
            vec![seg.to_owned()]
        } else {
            let w: Vec<&str> = seg.split_whitespace().collect();
            split_conjunctions(&w).into_iter().map(|g| g.join(" ")).collect()
        };
        for g in groups {
            if out.len() == MAX_PARTS {
                return out;
            }
            if let Some(p) = part(&g, &is_known) {
                out.push(p);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none(_: &str) -> bool {
        false
    }

    fn names(text: &str) -> Vec<String> {
        split_items(text, none).into_iter().map(|p| p.name).collect()
    }

    #[test]
    fn quantity_bounds() {
        assert_eq!(split_items("1 uovo", none)[0].quantity, Some(1));
        assert_eq!(split_items("9999 uova", none)[0].quantity, Some(9999));
        assert_eq!(split_items("0 uova", none)[0], ItemPart::plain("0 uova".into()));
        assert_eq!(split_items("10000 uova", none)[0], ItemPart::plain("10000 uova".into()));
        assert_eq!(split_items("x0 uova", none)[0].quantity, None);
    }

    #[test]
    fn strip_never_empties() {
        assert_eq!(names("add"), ["add"]);
        assert_eq!(names("shopping list"), ["shopping list"]);
        assert_eq!(names("add to the shopping list"), ["to the shopping list"]);
        assert_eq!(names("lista della spesa"), ["lista della spesa"]);
    }

    #[test]
    fn cap() {
        let text: Vec<String> = (0..60).map(|i| format!("a{i}")).collect();
        let parts = split_items(&text.join(", "), none);
        assert_eq!(parts.len(), MAX_PARTS);
        assert_eq!(parts[49].name, "a49");
    }

    #[test]
    fn conjunction_needs_both_sides() {
        assert_eq!(names("e latte"), ["e latte"]);
        assert_eq!(names("latte e"), ["latte e"]);
        assert_eq!(names("latte og"), ["latte og"]);
        assert_eq!(names("latte and eggs"), ["latte", "eggs"]);
    }

    #[test]
    fn unit_and_link_word() {
        let p = &split_items("2 kg di farina", none)[0];
        assert_eq!((p.name.as_str(), p.quantity, p.unit.as_deref()), ("farina", Some(2), Some("kg")));
        let p = &split_items("3 pz", none)[0];
        assert_eq!(p, &ItemPart::plain("3 pz".into()));
        let p = &split_items("200 g of", none)[0];
        assert_eq!(p, &ItemPart::plain("200 g of".into()));
    }

    #[test]
    fn known_text_stays_whole() {
        let known = |s: &str| fold_text(s) == "sale e pepe";
        assert_eq!(split_items("sale e pepe, latte", known).len(), 2);
        assert_eq!(split_items("Sale e Pepe", known)[0].name, "Sale e Pepe");
    }

    #[test]
    fn folds() {
        assert_eq!(fold_word("Tilføj"), "tilfoej");
        assert_eq!(fold_word("indkøbslisten."), "indkoebslisten");
        assert_eq!(fold_word("på"), "paa");
        assert_eq!(fold_text("  Caffè  d'orzo! "), "caffe d orzo");
    }
}
