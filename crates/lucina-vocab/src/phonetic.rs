//! A normalized phonetic key and a weighted edit distance over keys.
//!
//! The key is not a real phonetic transcription. It folds the spellings that
//! Whisper (and Italian, Danish and English speakers) confuse into one
//! representation, so that "Skibigårdsvej", "skibi gaardsvej" and "skibigordsvai"
//! land next to each other.

/// Lowercase one char and fold diacritics to ASCII. Non-Latin letters give `""`.
pub(crate) fn fold_char(c: char) -> &'static str {
    match c {
        'a' => "a",
        'b' => "b",
        'c' => "c",
        'd' => "d",
        'e' => "e",
        'f' => "f",
        'g' => "g",
        'h' => "h",
        'i' => "i",
        'j' => "j",
        'k' => "k",
        'l' => "l",
        'm' => "m",
        'n' => "n",
        'o' => "o",
        'p' => "p",
        'q' => "q",
        'r' => "r",
        's' => "s",
        't' => "t",
        'u' => "u",
        'v' => "v",
        'w' => "w",
        'x' => "x",
        'y' => "y",
        'z' => "z",
        '0' => "0",
        '1' => "1",
        '2' => "2",
        '3' => "3",
        '4' => "4",
        '5' => "5",
        '6' => "6",
        '7' => "7",
        '8' => "8",
        '9' => "9",
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => "a",
        'æ' => "ae",
        'œ' => "oe",
        'ç' | 'ć' | 'č' => "c",
        'ď' | 'đ' | 'ð' => "d",
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ę' | 'ě' => "e",
        'ğ' => "g",
        'ì' | 'í' | 'î' | 'ï' | 'ī' | 'ı' => "i",
        'ł' => "l",
        'ñ' | 'ń' | 'ň' => "n",
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ő' => "o",
        'ř' => "r",
        'ś' | 'š' | 'ş' => "s",
        'ß' => "ss",
        'ť' | 'ţ' => "t",
        'ù' | 'ú' | 'û' | 'ü' | 'ū' | 'ů' | 'ű' => "u",
        'ý' | 'ÿ' => "y",
        'ź' | 'ž' | 'ż' => "z",
        'þ' => "th",
        _ => "",
    }
}

/// Lowercase and fold diacritics; keeps ASCII letters and digits only.
pub(crate) fn fold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        for l in c.to_lowercase() {
            out.push_str(fold_char(l));
        }
    }
    out
}

/// Lowercase, fold diacritics and join the words with single spaces (anything
/// that is not a letter or digit separates words). For whole-word containment.
pub(crate) fn fold_spaced(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut gap = true;
    for c in s.chars() {
        if c.is_alphanumeric() {
            for l in c.to_lowercase() {
                out.push_str(fold_char(l));
            }
            gap = false;
        } else if !gap {
            out.push(' ');
            gap = true;
        }
    }
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

fn is_vowel(c: char) -> bool {
    matches!(c, 'a' | 'e' | 'i' | 'o' | 'u')
}

/// The normalized phonetic key of `s` (spaces, hyphens and punctuation are ignored).
///
/// Steps: fold diacritics (å→a, ø→o, æ→ae, ...); fold graphemes (ph→f, w→v,
/// y→i, z→s, ch/ck/q/c→k, c before e/i→s, sch→sk, th→t, sh→s, gj/dj→j,
/// x→ks); a `j` after a vowel becomes `i` (Danish "vej" is "vai"); silent `h`
/// is dropped; doubled letters collapse (so Danish "aa" = å = "a").
pub fn key(s: &str) -> Vec<char> {
    let f: Vec<char> = fold(s).chars().collect();
    let at = |i: usize| f.get(i).copied();
    let mut out: Vec<char> = Vec::with_capacity(f.len());
    let mut i = 0;
    while i < f.len() {
        let c = f[i];
        let n1 = at(i + 1);
        let n2 = at(i + 2);
        let (rep, adv): (&str, usize) = match (c, n1, n2) {
            ('s', Some('c'), Some('h')) => ("sk", 3),
            ('c', Some('h' | 'k'), _) => ("k", 2),
            ('p', Some('h'), _) => ("f", 2),
            ('t', Some('h'), _) => ("t", 2),
            ('s', Some('h'), _) => ("s", 2),
            ('k', Some('h'), _) => ("k", 2),
            ('g', Some('h'), _) => ("g", 2),
            ('g' | 'd', Some('j'), _) => ("j", 2),
            ('c', Some('e' | 'i' | 'y'), _) => ("s", 1),
            ('c' | 'q', _, _) => ("k", 1),
            ('x', _, _) => ("ks", 1),
            ('w', _, _) => ("v", 1),
            ('y', _, _) => ("i", 1),
            ('z', _, _) => ("s", 1),
            ('h', _, _) => ("", 1),
            ('j', _, _) if out.last().is_some_and(|&p| is_vowel(p)) => ("i", 1),
            _ => {
                out.push(c);
                i += 1;
                continue;
            }
        };
        out.extend(rep.chars());
        i += adv;
    }
    out.dedup();
    out
}

fn similar(a: char, b: char) -> bool {
    const PAIRS: [(char, char); 5] = [('b', 'p'), ('d', 't'), ('g', 'k'), ('m', 'n'), ('v', 'f')];
    PAIRS
        .iter()
        .any(|&(x, y)| (a == x && b == y) || (a == y && b == x))
}

fn sub_cost(a: char, b: char) -> f32 {
    if a == b {
        0.0
    } else if is_vowel(a) && is_vowel(b) {
        0.5
    } else if similar(a, b) {
        0.6
    } else {
        1.0
    }
}

fn indel_cost(c: char) -> f32 {
    if is_vowel(c) {
        0.8
    } else {
        1.0
    }
}

/// Weighted Damerau (optimal string alignment) distance between two keys.
/// Vowel for vowel and voiced/unvoiced pairs (b/p, d/t, g/k, m/n, v/f) are cheap
/// substitutions, a dropped or added vowel is cheaper than a consonant, a
/// transposition costs 1.
pub fn distance(a: &[char], b: &[char]) -> f32 {
    let (n, m) = (a.len(), b.len());
    let mut d = vec![vec![0f32; m + 1]; n + 1];
    for i in 1..=n {
        d[i][0] = d[i - 1][0] + indel_cost(a[i - 1]);
    }
    for j in 1..=m {
        d[0][j] = d[0][j - 1] + indel_cost(b[j - 1]);
    }
    for i in 1..=n {
        for j in 1..=m {
            let mut best = d[i - 1][j - 1] + sub_cost(a[i - 1], b[j - 1]);
            best = best.min(d[i - 1][j] + indel_cost(a[i - 1]));
            best = best.min(d[i][j - 1] + indel_cost(b[j - 1]));
            if i > 1
                && j > 1
                && a[i - 1] == b[j - 2]
                && a[i - 2] == b[j - 1]
                && a[i - 1] != b[j - 1]
            {
                best = best.min(d[i - 2][j - 2] + 1.0);
            }
            d[i][j] = best;
        }
    }
    d[n][m]
}

/// `distance / max(len)`, 0 for two empty keys.
pub fn ratio(a: &[char], b: &[char]) -> f32 {
    let n = a.len().max(b.len());
    if n == 0 {
        0.0
    } else {
        distance(a, b) / n as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(s: &str) -> String {
        key(s).into_iter().collect()
    }

    #[test]
    fn spellings_fold_to_one_key() {
        assert_eq!(k("Skibigårdsvej"), "skibigardsvei");
        assert_eq!(k("skibi gaardsvej"), "skibigardsvei");
        assert_eq!(k("Skibigaardsvej"), "skibigardsvei");
        assert_eq!(k("Horus"), "orus");
        assert_eq!(k("horus"), k("Orus"));
        assert_eq!(k("Phone"), k("fone"));
        assert_eq!(k("Wolt"), k("volt"));
        assert_eq!(k("Schermo"), "skermo");
        assert_eq!(k("Chiara"), "kiara");
        assert_eq!(k("Ærø"), "aero");
    }

    #[test]
    fn distance_is_cheap_for_vowel_swaps_and_dear_for_consonants() {
        let a = key("skibigardsvej");
        assert!(ratio(&a, &key("skibigordsvej")) < 0.05);
        assert!(ratio(&a, &key("skibigardsvai")) < 0.05);
        assert!(ratio(&a, &key("skibsvaerft")) > 0.4);
        assert_eq!(distance(&key("abcd"), &key("abdc")), 1.0);
    }
}
