//! The local matcher: how much of a talking point's wording the speech
//! carries, from 0 to 1.
//!
//! Words are folded (lower case, no accents, `æ` → `ae`), stop words of
//! English, Italian and Danish are dropped, and two words match when they are
//! equal, share a long prefix (`crossed` / `cross`, `punti` / `punto`) or are
//! one edit apart (`whisper` / `whispar`, a typical speech-to-text slip).
//! Longer words weigh more, up to 8 letters. Points of one or two words are
//! capped, so a single shared word never crosses a point out on its own.
//!
//! It is fast (microseconds) and needs no network, so it catches obvious hits
//! before Jev answers and is the whole judge when Jev is out. It does not
//! understand paraphrase or another language; that is Jev's job.
use std::collections::HashSet;
use std::sync::OnceLock;

/// Score cap for a point with a single content word.
const ONE_WORD_CAP: f64 = 0.6;
/// Factor for a point with two content words.
const TWO_WORD_FACTOR: f64 = 0.9;

const STOP_WORDS: &str = "
a an the and or but if so to of in on at by for with from as is are was were be been being it its
this that these those i im me my we our you your he she they them their his her do does did have
has had not no yes can will would should could just very really also then than there here what
which who how when where why all any some about into over up down out more most like get got going
go let lets well now okay ok um uh oh right thing things
il lo la gli le un uno una ed ma se di da con su per tra fra al allo alla ai agli alle del dello
della dei degli delle nel nella nei sul sulla che chi cui non si mi ti ci vi è sono era essere ho
ha hanno questo questa quello quella come anche più molto poi già cosa quando dove perché quindi
allora ecco tipo cioè
og jeg det en den til er som på de med af ikke der var mig sig men et har om vi min havde ham hun nu
da du ud sin dem os op man hans hvor eller hvad skal selv her alle vil blev kunne ind når være dog
noget ville jo deres efter ned skulle denne end dette mit også under dig anden hende mine alt meget
sit sine vor mod disse hvis din nogle hos blive mange ad bliver hendes været jer sådan så ja nej
altså
";

fn stop_words() -> &'static HashSet<String> {
    static S: OnceLock<HashSet<String>> = OnceLock::new();
    S.get_or_init(|| STOP_WORDS.split_whitespace().map(fold).collect())
}

/// Lower case without accents; `æ` → `ae`, `ø` → `o`, `ß` → `ss`.
pub fn fold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars().flat_map(char::to_lowercase) {
        match c {
            'à' | 'á' | 'â' | 'ä' | 'ã' | 'å' => out.push('a'),
            'è' | 'é' | 'ê' | 'ë' => out.push('e'),
            'ì' | 'í' | 'î' | 'ï' => out.push('i'),
            'ò' | 'ó' | 'ô' | 'ö' | 'õ' | 'ø' => out.push('o'),
            'ù' | 'ú' | 'û' | 'ü' => out.push('u'),
            'ñ' => out.push('n'),
            'ç' => out.push('c'),
            'æ' => out.push_str("ae"),
            'ß' => out.push_str("ss"),
            '’' | '\'' => {}
            c => out.push(c),
        }
    }
    out
}

/// Folded words (runs of letters and digits).
pub fn words(s: &str) -> Vec<String> {
    fold(s)
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The words that carry meaning: no stop words, no single letters (digits stay).
pub fn content_words(s: &str) -> Vec<String> {
    let stop = stop_words();
    let mut seen = HashSet::new();
    words(s)
        .into_iter()
        .filter(|w| !stop.contains(w))
        .filter(|w| w.chars().count() >= 2 || w.chars().all(|c| c.is_ascii_digit()))
        .filter(|w| seen.insert(w.clone()))
        .collect()
}

/// Same word, a shared long prefix, or one edit apart.
pub fn same_word(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let (short, long) = if a.len() <= b.len() {
        (&a, &b)
    } else {
        (&b, &a)
    };
    if short.iter().all(|c| c.is_ascii_digit()) || long.iter().all(|c| c.is_ascii_digit()) {
        return false;
    }
    // A shared stem of 5+ letters with up to 3 letters of ending (`listen|ing`), or of 4
    // letters with one-letter endings (`punt|i`, `punt|o`; not `serv|er` / `serv|ice`).
    let prefix = short
        .iter()
        .zip(long.iter())
        .take_while(|(x, y)| x == y)
        .count();
    let (rest_short, rest_long) = (short.len() - prefix, long.len() - prefix);
    if prefix >= 4 && rest_long <= 3 && (prefix >= 5 || rest_short.max(rest_long) <= 1) {
        return true;
    }
    short.len() >= 5 && long.len() - short.len() <= 1 && edits_at_most_one(short, long)
}

fn edits_at_most_one(short: &[char], long: &[char]) -> bool {
    let (mut i, mut j, mut edits) = (0, 0, 0);
    while i < short.len() && j < long.len() {
        if short[i] == long[j] {
            i += 1;
            j += 1;
            continue;
        }
        edits += 1;
        if edits > 1 {
            return false;
        }
        if short.len() == long.len() {
            i += 1;
        }
        j += 1;
    }
    edits + (long.len() - j) + (short.len() - i) <= 1
}

fn weight(w: &str) -> f64 {
    w.chars().count().clamp(2, 8) as f64
}

/// How much of `point`'s wording `speech` carries (0..=1).
pub fn score(point: &str, speech: &str) -> f64 {
    let p = content_words(point);
    if p.is_empty() {
        return 0.0;
    }
    let s = content_words(speech);
    let total: f64 = p.iter().map(|w| weight(w)).sum();
    let hit: f64 = p
        .iter()
        .filter(|w| s.iter().any(|x| same_word(w, x)))
        .map(|w| weight(w))
        .sum();
    let r = hit / total;
    match p.len() {
        1 => r.min(ONE_WORD_CAP),
        2 => r * TWO_WORD_FACTOR,
        _ => r,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_accents_and_ligatures() {
        assert_eq!(fold("Perché Være Ørsted ÆBLE"), "perche vaere orsted aeble");
        assert_eq!(words("it's a TEST, 2026!"), ["its", "a", "test", "2026"]);
    }

    #[test]
    fn stop_words_are_dropped_in_three_languages() {
        assert_eq!(
            content_words("the server and the model"),
            ["server", "model"]
        );
        assert_eq!(
            content_words("il modello è sul server"),
            ["modello", "server"]
        );
        assert_eq!(
            content_words("modellen kører på serveren"),
            ["modellen", "korer", "serveren"]
        );
    }

    #[test]
    fn word_forms_and_asr_slips_match() {
        for (a, b) in [
            ("crossed", "cross"),
            ("listening", "listens"),
            ("points", "point"),
            ("prompter", "promptor"),
            ("punti", "punto"),
            ("punkter", "punkt"),
            ("whisper", "whispar"),
            ("decides", "decide"),
        ] {
            assert!(same_word(a, b), "{a} ~ {b}");
        }
        for (a, b) in [
            ("cost", "costume"),
            ("server", "service"),
            ("2025", "2026"),
            ("jev", "jet"),
            ("model", "modest"),
        ] {
            assert!(!same_word(a, b), "{a} !~ {b}");
        }
    }

    #[test]
    fn paraphrased_hit_scores_high() {
        let s = score(
            "Whisper runs on my own server",
            "so speech recognition runs with whisper on my own server at home",
        );
        assert!(s > 0.95, "{s}");
        let s = score(
            "Jev decides which point was covered",
            "then Jev decides which of my points I just covered",
        );
        assert!(s > 0.95, "{s}");
    }

    #[test]
    fn unrelated_speech_scores_low() {
        let s = score(
            "Whisper runs on my own server",
            "and if I skip something it goes to the missed row",
        );
        assert!(s < 0.2, "{s}");
        assert_eq!(score("", "anything"), 0.0);
        assert_eq!(score("the and of", "the and of"), 0.0);
    }

    #[test]
    fn partial_wording_is_partial() {
        let s = score(
            "Missed points go to a row at the bottom",
            "some points were missed",
        );
        assert!((0.3..0.7).contains(&s), "{s}");
    }

    #[test]
    fn short_points_are_capped() {
        assert!(score("Pricing", "let's talk about pricing") <= ONE_WORD_CAP);
        let two = score("Pricing tiers", "the pricing tiers are simple");
        assert!(two < 1.0 && two > 0.8, "{two}");
    }

    #[test]
    fn italian_and_danish_match_in_their_own_language() {
        assert!(
            score(
                "Il modello gira sul mio server",
                "allora il modello gira proprio sul mio server di casa"
            ) > 0.9
        );
        assert!(
            score(
                "Punkterne bliver streget over mens jeg taler",
                "og så bliver punkterne streget over mens jeg taler"
            ) > 0.9
        );
    }
}
