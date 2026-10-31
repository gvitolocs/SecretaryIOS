//! Ranked, typo-tolerant text matching for agent search (Paseo
//! `protocol/search/text-match.js`, used by `agent-history-search.js`).
//!
//! A score is a tier plus the offset it was found at; lower is better on both.
//! Offsets count characters (Paseo: UTF-16 units; the same outside astral planes).

use std::cmp::Ordering;

const TIER_EXACT: u32 = 0;
const TIER_WHOLE_WORD: u32 = 1;
const TIER_PREFIX: u32 = 2;
const TIER_WORD_START: u32 = 3;
const TIER_SUBSTRING: u32 = 4;
const TIER_SUBSEQUENCE: u32 = 5;
const TIER_FUZZY: u32 = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Score {
    pub tier: u32,
    pub offset: usize,
    pub spread: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FuzzyPolicy {
    pub max_edits: usize,
    pub transpositions_only: bool,
}

fn word_char(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit()
}

fn boundary(c: Option<char>) -> bool {
    !c.is_some_and(word_char)
}

fn find(text: &[char], query: &[char], from: usize) -> Option<usize> {
    if query.is_empty() {
        return (from <= text.len()).then_some(from);
    }
    (from..=text.len().checked_sub(query.len())?).find(|&i| text[i..i + query.len()] == *query)
}

fn substring(query: &[char], text: &[char]) -> Option<Score> {
    let mut best: Option<Score> = None;
    let mut pos = 0;
    while text.len() >= query.len() && pos <= text.len() - query.len() {
        let Some(found) = find(text, query, pos) else { break };
        let before = found.checked_sub(1).map(|i| text[i]);
        let after = text.get(found + query.len()).copied();
        let starts = found == 0 || boundary(before);
        let ends = after.is_none() || boundary(after);
        let tier = if starts && ends {
            TIER_WHOLE_WORD
        } else if found == 0 {
            TIER_PREFIX
        } else if starts {
            TIER_WORD_START
        } else {
            TIER_SUBSTRING
        };
        if best.is_none_or(|b| tier < b.tier || (tier == b.tier && found < b.offset)) {
            best = Some(Score { tier, offset: found, spread: None });
        }
        pos = found + 1;
    }
    best
}

fn subsequence(query: &[char], text: &[char]) -> Option<Score> {
    let (mut qi, mut first, mut last) = (0usize, None::<usize>, 0usize);
    for (ti, &c) in text.iter().enumerate() {
        if qi >= query.len() {
            break;
        }
        if c.is_whitespace() {
            qi = 0;
            first = None;
            last = 0;
            continue;
        }
        if c != query[qi] {
            continue;
        }
        first.get_or_insert(ti);
        last = ti;
        qi += 1;
    }
    let first = first?;
    (qi == query.len()).then_some(Score { tier: TIER_SUBSEQUENCE, offset: first, spread: Some(last - first + 1) })
}

/// Damerau-Levenshtein (optimal string alignment), given up once a row is over budget.
fn bounded_edit_distance(query: &[char], word: &[char], budget: usize) -> Option<usize> {
    if query.len().abs_diff(word.len()) > budget {
        return None;
    }
    let mut two_back: Vec<usize> = Vec::new();
    let mut prev: Vec<usize> = (0..=word.len()).collect();
    for qi in 1..=query.len() {
        let mut cur = vec![qi];
        let mut row_best = qi;
        for wi in 1..=word.len() {
            let sub = usize::from(query[qi - 1] != word[wi - 1]);
            let mut cost = (cur[wi - 1] + 1).min(prev[wi] + 1).min(prev[wi - 1] + sub);
            if qi > 1 && wi > 1 && query[qi - 1] == word[wi - 2] && query[qi - 2] == word[wi - 1] {
                cost = cost.min(two_back[wi - 2] + 1);
            }
            cur.push(cost);
            row_best = row_best.min(cost);
        }
        if row_best > budget {
            return None;
        }
        two_back = prev;
        prev = cur;
    }
    let d = prev[word.len()];
    (d <= budget).then_some(d)
}

fn adjacent_transposition(q: &[char], w: &[char]) -> bool {
    if q.len() != w.len() {
        return false;
    }
    let mut i = 0;
    while i < q.len() && q[i] == w[i] {
        i += 1;
    }
    if i + 1 >= q.len() {
        return false;
    }
    q[i] == w[i + 1] && q[i + 1] == w[i] && q[i + 2..] == w[i + 2..]
}

/// Paseo `fuzzyPolicyForToken`.
pub fn fuzzy_policy(token: &str) -> Option<FuzzyPolicy> {
    match token.chars().count() {
        0..=3 => None,
        4 => Some(FuzzyPolicy { max_edits: 1, transpositions_only: true }),
        5..=7 => Some(FuzzyPolicy { max_edits: 1, transpositions_only: false }),
        _ => Some(FuzzyPolicy { max_edits: 2, transpositions_only: false }),
    }
}

pub fn compare(a: &Score, b: &Score) -> Ordering {
    a.tier.cmp(&b.tier).then(a.offset.cmp(&b.offset)).then(a.spread.unwrap_or(0).cmp(&b.spread.unwrap_or(0)))
}

fn fuzzy(query: &[char], text: &[char], policy: FuzzyPolicy) -> Option<Score> {
    if policy.max_edits == 0 || query.len() <= policy.max_edits {
        return None;
    }
    let mut best: Option<Score> = None;
    let mut i = 0;
    while i < text.len() {
        if !word_char(text[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < text.len() && word_char(text[i]) {
            i += 1;
        }
        let word = &text[start..i];
        let mut candidates: Vec<&[char]> = Vec::new();
        for c in [word, &word[..query.len().min(word.len())], &word[..(query.len() + policy.max_edits).min(word.len())]] {
            if !candidates.contains(&c) {
                candidates.push(c);
            }
        }
        for c in candidates {
            let d = if policy.transpositions_only {
                adjacent_transposition(query, c).then_some(1)
            } else {
                bounded_edit_distance(query, c, policy.max_edits)
            };
            let Some(d) = d else { continue };
            let score = Score { tier: TIER_FUZZY, offset: start, spread: Some(d) };
            if best.is_none_or(|b| compare(&score, &b) == Ordering::Less) {
                best = Some(score);
            }
        }
    }
    best
}

/// Paseo `scoreMatch`.
pub fn score_match(query: &str, text: &str, fuzzy_policy: Option<FuzzyPolicy>, allow_subsequence: bool) -> Option<Score> {
    if query.is_empty() {
        return Some(Score { tier: TIER_EXACT, offset: 0, spread: None });
    }
    let q: Vec<char> = query.to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    if q == t {
        return Some(Score { tier: TIER_EXACT, offset: 0, spread: None });
    }
    let exact = substring(&q, &t).or_else(|| if allow_subsequence { subsequence(&q, &t) } else { None });
    if exact.is_some() {
        return exact;
    }
    fuzzy_policy.and_then(|p| fuzzy(&q, &t, p))
}

pub fn tokenize(query: &str) -> Vec<String> {
    query.trim().to_lowercase().split_whitespace().map(str::to_string).collect()
}

/// Paseo `scoreTextFields`: every token must match some field; scores add up.
pub fn score_text_fields(query: &str, fields: &[&str], typo_tolerant: bool) -> Option<Score> {
    let tokens = tokenize(query);
    let mut agg = Score { tier: TIER_EXACT, offset: 0, spread: Some(0) };
    for token in &tokens {
        let policy = if typo_tolerant { fuzzy_policy(token) } else { None };
        let best = fields
            .iter()
            .filter_map(|f| score_match(token, f, policy, true))
            .min_by(compare)?;
        agg.tier += best.tier;
        agg.offset += best.offset;
        agg.spread = Some(agg.spread.unwrap_or(0) + best.spread.unwrap_or(token.chars().count()));
    }
    Some(agg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiers() {
        assert_eq!(score_match("fix", "fix", None, true).unwrap().tier, TIER_EXACT);
        assert_eq!(score_match("fix", "a fix here", None, true).unwrap().tier, TIER_WHOLE_WORD);
        assert_eq!(score_match("fix", "fixing", None, true).unwrap().tier, TIER_PREFIX);
        assert_eq!(score_match("fix", "a fixing", None, true).unwrap().tier, TIER_WORD_START);
        assert_eq!(score_match("fix", "prefix", None, true).unwrap().tier, TIER_SUBSTRING);
        assert_eq!(score_match("fx", "fox", None, true).unwrap().tier, TIER_SUBSEQUENCE);
        assert_eq!(score_match("confug", "configuration", fuzzy_policy("confug"), true).unwrap().tier, TIER_FUZZY);
        assert!(score_text_fields("scrol ancor", &["Fix the scroll anchor"], true).is_some());
        assert!(score_text_fields("zzz", &["Fix the scroll anchor"], true).is_none());
    }
}
