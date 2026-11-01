//! Hybrid search over passages: FTS5 (bm25) and vector cosine, merged by reciprocal-rank
//! fusion, so exact terms ("DHCP option 82") and meaning ("how do switches stop fake
//! servers") both find their passage. Without an embedder (or before the vectors are in)
//! it is full-text only.

use std::collections::HashMap;

use crate::embed::{dot, Embedder, Kind};
use crate::store::{Chunk, Store};
use crate::Error;

const RRF_K: f64 = 60.0;
const POOL: i64 = 40;

#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub chunk: Chunk,
    pub score: f64,
}

/// An FTS5 query: every word (letters/digits, 2+ characters) quoted, ORed.
pub fn fts_query(q: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    for w in q.split(|c: char| !c.is_alphanumeric()) {
        let w = w.to_lowercase();
        if w.chars().count() >= 2 && !STOP.contains(&w.as_str()) && !words.contains(&w) {
            words.push(w);
        }
        if words.len() == 16 {
            break;
        }
    }
    words.iter().map(|w| format!("\"{w}\"")).collect::<Vec<_>>().join(" OR ")
}

const STOP: [&str; 40] = [
    "the", "a", "an", "and", "or", "of", "to", "in", "on", "for", "is", "are", "was", "be", "what", "which", "who",
    "how", "why", "when", "does", "do", "did", "it", "this", "that", "with", "as", "by", "at", "from", "about",
    "can", "you", "me", "my", "il", "la", "di", "che",
];

pub async fn search(
    store: &Store,
    embedder: Option<&dyn Embedder>,
    owners: &[String],
    query: &str,
    limit: usize,
) -> Result<Vec<Hit>, Error> {
    if owners.is_empty() || query.trim().is_empty() {
        return Ok(Vec::new());
    }
    let mut fused: HashMap<String, (Chunk, f64)> = HashMap::new();
    let q = fts_query(query);
    if !q.is_empty() {
        for (rank, (c, _)) in store.fts(&q, owners, POOL)?.into_iter().enumerate() {
            let s = 1.0 / (RRF_K + rank as f64 + 1.0);
            fused.entry(c.id.clone()).or_insert((c, 0.0)).1 += s;
        }
    }
    if let Some(e) = embedder {
        match e.embed(Kind::Query, &[query.to_owned()], "").await {
            Ok(v) if !v.is_empty() => {
                let qv = &v[0];
                let mut scored: Vec<(Chunk, f32)> = store
                    .vectors(owners)?
                    .into_iter()
                    .filter(|(_, v)| v.len() == qv.len())
                    .map(|(c, v)| {
                        let s = dot(qv, &v);
                        (c, s)
                    })
                    .collect();
                scored.sort_by(|a, b| b.1.total_cmp(&a.1));
                for (rank, (c, cos)) in scored.into_iter().take(POOL as usize).enumerate() {
                    // Clearly unrelated passages don't get in on rank alone.
                    if cos < 0.2 {
                        break;
                    }
                    let s = 1.0 / (RRF_K + rank as f64 + 1.0);
                    fused.entry(c.id.clone()).or_insert((c, 0.0)).1 += s;
                }
            }
            Ok(_) => {}
            Err(err) => tracing::warn!(error = %err, "notebooks: query embedding failed, full-text only"),
        }
    }
    let mut hits: Vec<Hit> = fused.into_values().map(|(chunk, score)| Hit { chunk, score }).collect();
    hits.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.chunk.id.cmp(&b.chunk.id)));
    hits.truncate(limit);
    Ok(hits)
}

/// A short excerpt of a passage around the query's words.
pub fn excerpt(text: &str, query: &str, max: usize) -> String {
    let lower = text.to_lowercase();
    let pos = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 3)
        .filter_map(|w| lower.find(&w.to_lowercase()))
        .min()
        .filter(|&p| lower.len() == text.len() && text.is_char_boundary(p))
        .unwrap_or(0);
    let start = text[..pos].char_indices().rev().nth(max / 3).map(|(i, _)| i).unwrap_or(0);
    let s: String = text[start..].chars().take(max).collect();
    let mut s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if start > 0 {
        s = format!("…{s}");
    }
    if text[start..].chars().count() > max {
        s.push('…');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::fake::Letters;
    use crate::store::NewChunk;

    #[test]
    fn fts_query_quotes_words() {
        assert_eq!(fts_query("What is DHCP snooping?"), "\"dhcp\" OR \"snooping\"");
        assert_eq!(fts_query("\" OR 1=1 -- NEAR(x"), "\"near\"");
        assert_eq!(fts_query("??"), "");
    }

    #[tokio::test]
    async fn hybrid_finds_by_word_and_by_vector() {
        let s = Store::in_memory().unwrap();
        s.create_source("s1", "text", "T", "text", "ready", "", None, &[]).unwrap();
        s.replace_chunks(
            "s1",
            &[
                NewChunk { text: "DHCP snooping drops offers from untrusted ports".into(), ..Default::default() },
                NewChunk { text: "Questionnaires measure attitudes with Likert scales".into(), ..Default::default() },
            ],
        )
        .unwrap();
        let e = Letters;
        let all = s.unembedded(None, 10).unwrap();
        let v = e.embed(Kind::Document, &all.iter().map(|c| c.text.clone()).collect::<Vec<_>>(), "").await.unwrap();
        s.set_vectors(&all.iter().zip(v).map(|(c, v)| (c.id.clone(), v)).collect::<Vec<_>>()).unwrap();
        let hits = search(&s, Some(&e), &["s1".into()], "snooping", 5).await.unwrap();
        assert_eq!(hits[0].chunk.id, "s1-0");
        let hits = search(&s, None, &["s1".into()], "Likert", 5).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].chunk.id, "s1-1");
        assert!(search(&s, Some(&e), &[], "x", 5).await.unwrap().is_empty());
    }

    #[test]
    fn excerpts_center_on_the_words() {
        let t = format!("{} DHCP snooping here {}", "a ".repeat(200), "b ".repeat(200));
        let x = excerpt(&t, "snooping", 60);
        assert!(x.contains("snooping"), "{x}");
        assert!(x.starts_with('…') && x.ends_with('…'));
    }
}
