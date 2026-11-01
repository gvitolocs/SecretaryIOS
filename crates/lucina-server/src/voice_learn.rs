//! Voice Learn in lucina-rs (docs/lucinavoice-protocol.md section 8): the owner's vocabulary, one file shared
//! with every speech-to-text path (`lucina_vocab`, `lucina_memo`).
//!
//! - **API** (`/api/secretary/voice-learn`, the same bearer as the rest of `/api/secretary`):
//!   `GET` lists, `POST` adds, `PATCH|DELETE /{id}` change one, `POST /suggest {transcript, corrected}` offers
//!   candidate entries from a transcript the owner edited.
//! - **Meaning as context:** a turn (voice or not) whose text holds an entry's word gets one line per such entry,
//!   `Vocabulary: Skibigårdsvej = the owner's home street` ([`context_block`], joined into the turn's context
//!   by `context::prepare_keyed`).
//! - **Meaning as memory:** an entry with a `meaning` is written once to Honcho as a conclusion about the owner
//!   (observer: Lucina's peer, target: the owner) when it is created or its meaning changes, replaced when the
//!   meaning is edited, removed when the entry or its meaning goes. A side file (`<vocab file>.memory.json`) maps
//!   entry id to conclusion id and text, so repeating a save writes nothing twice. A memory that is off, read-only
//!   or down never fails the API call: the response says `memory: "written" | "removed" | "unchanged" | "skipped"
//!   | "failed"`.
//! - **Transcript correction by Jev** ([`apply_ambiguity`], the `vocab` layer): when a voice request carries
//!   `voiceMeta.ambiguous` (spans the STT's post-correction found close to several entries), one capped Jev call
//!   asks which word the speaker said; a sure pick (`LUCINA_VOICE_VOCAB_MIN`, 0.8) replaces the span.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Method, Uri};
use axum::response::Response;
use chrono::Utc;
use lucina_jev::Question;
use lucina_memory::MemoryMode;
use lucina_store::timeutil::iso_ms;
use lucina_vocab::{validate_entry, Entry, Vocab};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::api::{ok, ok_status, read_json, ApiError};
use crate::voice::{self, Layer};
use crate::AppState;

const MAX_BODY: usize = 16 * 1024;
/// The `vocab` layer's own budget (the protocol's 300 ms), under the voice cap.
const VOCAB_CAP: Duration = Duration::from_millis(300);
/// At most this many entries' meanings join one turn's context.
const MAX_CONTEXT_LINES: usize = 5;

fn conflict(msg: String) -> ApiError {
    ApiError::new(409, "conflict", msg)
}

fn invalid(msg: String) -> ApiError {
    ApiError::bad_request(msg)
}

/// `id`s are `vl_` plus twelve hex digits.
fn new_id() -> String {
    format!("vl_{}", &uuid::Uuid::new_v4().simple().to_string()[..12])
}

fn text_field(v: &Value, name: &str) -> Result<Option<String>, ApiError> {
    match v.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.trim().to_owned())),
        Some(_) => Err(invalid(format!("{name} must be a string"))),
    }
}

fn sounds_field(v: &Value) -> Result<Option<Vec<String>>, ApiError> {
    match v.get("soundsLike") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(a)) => {
            let mut out: Vec<String> = Vec::new();
            for x in a {
                let s = x
                    .as_str()
                    .ok_or_else(|| invalid("soundsLike must be an array of strings".into()))?
                    .trim();
                if !s.is_empty() && !out.iter().any(|o| o.eq_ignore_ascii_case(s)) {
                    out.push(s.to_owned());
                }
            }
            Ok(Some(out))
        }
        Some(_) => Err(invalid("soundsLike must be an array of strings".into())),
    }
}

fn load(st: &AppState) -> Result<Vocab, ApiError> {
    Vocab::load(st.vocab.path()).map_err(|e| {
        ApiError::new(
            500,
            "internal_error",
            format!("voice learn file unreadable: {e}"),
        )
    })
}

fn save(st: &AppState, v: &Vocab) -> Result<(), ApiError> {
    v.save(st.vocab.path()).map_err(|e| {
        ApiError::new(
            500,
            "internal_error",
            format!("voice learn save failed: {e}"),
        )
    })
}

/// Maps a `lucina_vocab` rule error to the API: a duplicate word is a conflict, the rest are bad requests.
fn rule_error(e: String) -> ApiError {
    if e.starts_with("duplicate") {
        conflict(e)
    } else {
        invalid(e)
    }
}

pub async fn route(
    st: &AppState,
    method: &Method,
    uri: &Uri,
    parts: &[String],
    body: Body,
) -> Result<Response, ApiError> {
    let _ = uri;
    match parts.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        [] => match *method {
            Method::GET => {
                let v = load(st)?;
                Ok(ok(
                    json!({"entries": v.entries, "max": lucina_vocab::MAX_ENTRIES}),
                ))
            }
            Method::POST => {
                let b = read_json(body, MAX_BODY).await?;
                create(st, &b).await
            }
            _ => Err(ApiError::method_not_allowed(&["GET", "POST"])),
        },
        ["suggest"] => {
            if *method != Method::POST {
                return Err(ApiError::method_not_allowed(&["POST"]));
            }
            let b = read_json(body, MAX_BODY).await?;
            suggest_route(st, &b)
        }
        [id] => match *method {
            Method::GET => {
                let v = load(st)?;
                let e = v.get(id).ok_or_else(ApiError::not_found)?;
                Ok(ok(json!({"entry": e})))
            }
            Method::PATCH => {
                let b = read_json(body, MAX_BODY).await?;
                patch(st, id, &b).await
            }
            Method::DELETE => remove(st, id).await,
            _ => Err(ApiError::method_not_allowed(&["GET", "PATCH", "DELETE"])),
        },
        _ => Err(ApiError::not_found()),
    }
}

async fn create(st: &AppState, b: &Value) -> Result<Response, ApiError> {
    let word = text_field(b, "word")?.ok_or_else(|| invalid("word is required".into()))?;
    let now = iso_ms(Utc::now());
    let mut e = Entry::new(word);
    e.id = new_id();
    e.sounds_like = sounds_field(b)?.unwrap_or_default();
    e.meaning = text_field(b, "meaning")?.filter(|m| !m.is_empty());
    e.lang = text_field(b, "lang")?.filter(|l| !l.is_empty());
    if let Some(src) = text_field(b, "source")? {
        e.source = src;
    }
    e.created_at = now.clone();
    e.updated_at = now;
    validate_entry(&e).map_err(invalid)?;
    {
        let _w = st.vocab_write.lock().unwrap_or_else(|p| p.into_inner());
        let mut v = load(st)?;
        v.add(e.clone()).map_err(rule_error)?;
        save(st, &v)?;
    }
    let memory = sync_memory(st, None, Some(&e)).await;
    Ok(ok_status(201, json!({"entry": e, "memory": memory})))
}

async fn patch(st: &AppState, id: &str, b: &Value) -> Result<Response, ApiError> {
    let (before, after) = {
        let _w = st.vocab_write.lock().unwrap_or_else(|p| p.into_inner());
        let mut v = load(st)?;
        let before = v.get(id).cloned().ok_or_else(ApiError::not_found)?;
        let mut e = before.clone();
        if let Some(w) = text_field(b, "word")? {
            e.word = w;
        }
        if let Some(s) = sounds_field(b)? {
            e.sounds_like = s;
        }
        // `null` clears, absent keeps.
        if b.get("meaning").is_some() {
            e.meaning = text_field(b, "meaning")?.filter(|m| !m.is_empty());
        }
        if b.get("lang").is_some() {
            e.lang = text_field(b, "lang")?.filter(|l| !l.is_empty());
        }
        e.updated_at = iso_ms(Utc::now());
        validate_entry(&e).map_err(invalid)?;
        v.replace(id, e.clone()).map_err(rule_error)?;
        save(st, &v)?;
        (before, e)
    };
    let memory = sync_memory(st, Some(&before), Some(&after)).await;
    Ok(ok(json!({"entry": after, "memory": memory})))
}

async fn remove(st: &AppState, id: &str) -> Result<Response, ApiError> {
    let before = {
        let _w = st.vocab_write.lock().unwrap_or_else(|p| p.into_inner());
        let mut v = load(st)?;
        let before = v.get(id).cloned().ok_or_else(ApiError::not_found)?;
        v.remove(id);
        save(st, &v)?;
        before
    };
    let memory = sync_memory(st, Some(&before), None).await;
    Ok(ok(json!({"id": id, "deleted": true, "memory": memory})))
}

// ---------------------------------------------------------------------------------------------- suggest

fn suggest_route(st: &AppState, b: &Value) -> Result<Response, ApiError> {
    let transcript =
        text_field(b, "transcript")?.ok_or_else(|| invalid("transcript is required".into()))?;
    let corrected =
        text_field(b, "corrected")?.ok_or_else(|| invalid("corrected is required".into()))?;
    if transcript.chars().count() > 2000 || corrected.chars().count() > 2000 {
        return Err(invalid(
            "transcript and corrected must be at most 2000 characters".into(),
        ));
    }
    let v = load(st)?;
    Ok(ok(
        json!({"candidates": suggest(&v, &transcript, &corrected)}),
    ))
}

fn words_of(s: &str) -> Vec<String> {
    s.split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'' && c != '-')
                .to_owned()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

/// Candidate entries from an edit: every place where the owner replaced a run of words of the transcript by
/// another run is a `word` (what he wrote) that was misheard as `soundsLike` (what the speech-to-text wrote).
/// A word that is already an entry yields that entry's id, so the app can add the new `soundsLike` to it.
pub fn suggest(vocab: &Vocab, transcript: &str, corrected: &str) -> Vec<Value> {
    let a = words_of(transcript);
    let b = words_of(corrected);
    let key = |w: &str| w.to_lowercase();
    // Longest common subsequence over the lowercased words.
    let (n, m) = (a.len(), b.len());
    let mut lcs = vec![vec![0u16; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if key(&a[i]) == key(&b[j]) {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let (mut del, mut ins): (Vec<&str>, Vec<&str>) = (Vec::new(), Vec::new());
    let mut spans: Vec<(String, String)> = Vec::new();
    let mut flush = |del: &mut Vec<&str>, ins: &mut Vec<&str>| {
        if !del.is_empty() && !ins.is_empty() {
            spans.push((del.join(" "), ins.join(" ")));
        }
        del.clear();
        ins.clear();
    };
    while i < n || j < m {
        if i < n && j < m && key(&a[i]) == key(&b[j]) {
            flush(&mut del, &mut ins);
            i += 1;
            j += 1;
        } else if j < m && (i == n || lcs[i][j + 1] >= lcs[i + 1][j]) {
            ins.push(&b[j]);
            j += 1;
        } else {
            del.push(&a[i]);
            i += 1;
        }
    }
    flush(&mut del, &mut ins);
    let mut out = Vec::new();
    for (heard, word) in spans {
        if heard.to_lowercase() == word.to_lowercase()
            || heard.split(' ').count() > 3
            || word.split(' ').count() > 3
            || word.chars().count() < 3
        {
            continue;
        }
        let mut e = Entry::new(word.clone());
        e.sounds_like = vec![heard.clone()];
        e.source = "correction".into();
        if validate_entry(&e).is_err() {
            continue;
        }
        let existing = vocab
            .entries
            .iter()
            .find(|x| x.word.to_lowercase() == word.to_lowercase());
        let mut c = json!({"word": word, "soundsLike": [heard], "source": "correction"});
        if let Some(x) = existing {
            c["existingId"] = json!(x.id);
        }
        out.push(c);
    }
    out
}

// ------------------------------------------------------------------------------------------ the context

/// One `Vocabulary:` line per entry with a meaning whose word the text holds.
pub fn context_block(st: &AppState, text: &str) -> String {
    let v = st.vocab.get();
    if v.entries.is_empty() {
        return String::new();
    }
    lines_for(&v, text)
}

pub fn lines_for(v: &Vocab, text: &str) -> String {
    v.meanings_for(text)
        .into_iter()
        .take(MAX_CONTEXT_LINES)
        .map(|(w, m)| format!("Vocabulary: {w} = {m}"))
        .collect::<Vec<_>>()
        .join("\n")
}

// ------------------------------------------------------------------------------------------ the memory

#[derive(Debug, Default, Serialize, Deserialize)]
struct MemoryMap {
    /// Entry id → what was written for it.
    #[serde(default)]
    entries: BTreeMap<String, Written>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Written {
    conclusion: String,
    content: String,
}

fn map_path(st: &AppState) -> PathBuf {
    let mut p = st.vocab.path().as_os_str().to_owned();
    p.push(".memory.json");
    PathBuf::from(p)
}

fn load_map(path: &PathBuf) -> MemoryMap {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// What Honcho is told about an entry with a meaning.
pub fn memory_text(e: &Entry) -> Option<String> {
    let m = e.meaning.as_deref().filter(|m| !m.is_empty())?;
    Some(format!(
        "In the owner's Voice Learn vocabulary, \"{}\" means: {m}.",
        e.word
    ))
}

/// Writes, replaces or removes the entry's conclusion in Honcho. Idempotent: the same text is not written twice.
/// Returns what happened, for the API response; never an error.
async fn sync_memory(st: &AppState, before: Option<&Entry>, after: Option<&Entry>) -> &'static str {
    let id = match (before, after) {
        (_, Some(e)) | (Some(e), None) => e.id.clone(),
        (None, None) => return "unchanged",
    };
    let want = after.and_then(memory_text);
    let path = map_path(st);
    let map = load_map(&path);
    let have = map.entries.get(&id).cloned();
    if want.is_none() && have.is_none() {
        return "unchanged";
    }
    if want.as_deref() == have.as_ref().map(|h| h.content.as_str()) {
        return "unchanged";
    }
    if st.memory.mode() != MemoryMode::ReadWrite {
        return "skipped";
    }
    let Some(client) = st.memory.client() else {
        return "skipped";
    };
    let cfg = client.config().clone();
    let http = match reqwest::Client::builder().timeout(cfg.timeout).build() {
        Ok(h) => h,
        Err(_) => return "failed",
    };
    let base = format!(
        "{}/v3/workspaces/{}/conclusions",
        cfg.base_url, cfg.workspace_id
    );
    let mut result = "unchanged";
    // Write the new text first, then drop the old one: a failure in between leaves a stale line, not a gap.
    let mut written: Option<Written> = None;
    if let Some(content) = &want {
        let created = async {
            client.ensure_peer(&cfg.owner_peer_id).await.ok()?;
            client.ensure_peer(&cfg.agent_peer_id).await.ok()?;
            let mut body = json!({"conclusions": [{
                "content": content,
                "observer_id": cfg.agent_peer_id,
                "observed_id": cfg.owner_peer_id,
            }]});
            lucina_secrets::redactor().redact_json(&mut body);
            let resp = http
                .post(&base)
                .bearer_auth(&cfg.api_key)
                .json(&body)
                .send()
                .await
                .ok()?;
            if !resp.status().is_success() {
                return None;
            }
            let v: Value = resp.json().await.ok()?;
            let first = v
                .as_array()
                .or_else(|| v["items"].as_array())
                .or_else(|| v["conclusions"].as_array())
                .and_then(|a| a.first())?;
            first["id"].as_str().map(str::to_owned)
        }
        .await;
        match created {
            Some(conclusion) => {
                written = Some(Written {
                    conclusion,
                    content: content.clone(),
                });
                result = "written";
            }
            None => return "failed",
        }
    }
    if let Some(old) = &have {
        let gone = match http
            .delete(format!("{base}/{}", old.conclusion))
            .bearer_auth(&cfg.api_key)
            .send()
            .await
        {
            // A conclusion that is already gone is gone.
            Ok(r) => r.status().is_success() || r.status().as_u16() == 404,
            Err(_) => false,
        };
        if !gone && written.is_none() {
            return "failed";
        }
        if written.is_none() {
            result = "removed";
        }
    }
    let _w = st.vocab_write.lock().unwrap_or_else(|p| p.into_inner());
    let mut map = load_map(&path);
    match written {
        Some(w) => {
            map.entries.insert(id, w);
        }
        None => {
            map.entries.remove(&id);
        }
    }
    if let Ok(text) = serde_json::to_vec_pretty(&map) {
        if let Err(e) = lucina_store::write_atomic(&path, &text) {
            tracing::warn!(error = %e, "voice learn memory map write failed");
        }
    }
    result
}

// -------------------------------------------------------------------------------- the vocab Jev layer

/// Replaces the first whole-word, case-insensitive `heard` in `text` by `word`.
fn replace_span(text: &str, heard: &str, word: &str) -> Option<String> {
    let low = text.to_lowercase();
    let needle = heard.to_lowercase();
    if needle.is_empty() || low.len() != text.len() {
        return None;
    }
    let mut from = 0;
    while let Some(at) = low[from..].find(&needle) {
        let start = from + at;
        let end = start + needle.len();
        let word_char = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric());
        let before = text[..start].chars().next_back();
        let after = text[end..].chars().next();
        if !word_char(before) && !word_char(after) {
            return Some(format!("{}{}{}", &text[..start], word, &text[end..]));
        }
        from = end;
    }
    None
}

/// The `vocab` layer: the spoken sentence came with spans the STT could not settle (`voiceMeta.ambiguous`,
/// `[{heard, candidates}]`). One Jev call (a question per span, at most three, 300 ms, one of the turn's three
/// calls) asks which word the speaker said; a sure pick replaces the span. Anything else (no Jev, too slow, not
/// sure, "none") leaves the sentence as the STT wrote it.
pub async fn apply_ambiguity(st: &AppState, message: String, body: &Value) -> String {
    let Some(spans) = body["voiceMeta"]["ambiguous"]
        .as_array()
        .filter(|a| !a.is_empty())
    else {
        return message;
    };
    // Clean the sender's spans: short, at most three, each with two to five candidates.
    let spans: Vec<(String, Vec<String>)> = spans
        .iter()
        .filter_map(|s| {
            let heard = s["heard"].as_str()?.trim().to_owned();
            let cands: Vec<String> = s["candidates"]
                .as_array()?
                .iter()
                .filter_map(|c| c.as_str().map(|c| c.trim().to_owned()))
                .filter(|c| !c.is_empty() && c.chars().count() <= 100)
                .take(5)
                .collect();
            (!heard.is_empty() && heard.chars().count() <= 100 && !cands.is_empty())
                .then_some((heard, cands))
        })
        .take(3)
        .collect();
    apply_spans(st, message, spans).await
}

/// A transcript that did not come through a spoken turn (a voice note's on-device transcript): the vocabulary
/// corrects it, and doubtful spans go to the `vocab` layer like a spoken turn's. Without a turn trace there is
/// no call count to share; the layer is still one capped call that fails open.
pub async fn correct_transcript(st: &AppState, text: &str) -> String {
    let vocab = st.vocab.get();
    if vocab.entries.is_empty() {
        return text.to_owned();
    }
    let c = vocab.correct(text);
    if !c.corrections.is_empty() {
        tracing::info!(
            corrections = c.corrections.len(),
            "voice learn corrected a transcript"
        );
    }
    let spans: Vec<(String, Vec<String>)> = c
        .ambiguous
        .iter()
        .take(3)
        .map(|a| {
            (
                a.heard.clone(),
                a.candidates.iter().take(5).cloned().collect(),
            )
        })
        .filter(|(h, c): &(String, Vec<String>)| !h.is_empty() && !c.is_empty())
        .collect();
    apply_spans(st, c.text, spans).await
}

/// The `vocab` layer on cleaned spans.
async fn apply_spans(st: &AppState, message: String, spans: Vec<(String, Vec<String>)>) -> String {
    if !st.cfg.voice_layers || spans.is_empty() {
        return message;
    }
    let jev = match &st.voice_jev {
        Some(j) => j.as_ref(),
        None => lucina_jev::shared(),
    };
    let trace = voice::trace();
    let fail = |reason: &'static str, ms: u64| {
        if reason != "unavailable" {
            if let Some(t) = &trace {
                t.push(Layer {
                    layer: "vocab",
                    verdict: reason.to_owned(),
                    confidence: 0.0,
                    ms,
                    used: false,
                });
            }
        }
    };
    if !jev.available() {
        fail("unavailable", 0);
        return message;
    }
    if trace.as_ref().is_some_and(|t| !t.take_call()) {
        fail("budget", 0);
        return message;
    }
    // The facts: the sentence and the spans with their candidates. Nothing else.
    let state = json!({
        "channel": "voice",
        "sentence": message,
        "spans": spans.iter().map(|(h, c)| json!({"heard": h, "candidates": c})).collect::<Vec<_>>(),
    });
    let names: Vec<String> = (0..spans.len()).map(|i| format!("span{i}")).collect();
    let questions: Vec<(&str, Question)> = spans
        .iter()
        .enumerate()
        .map(|(i, (heard, cands))| {
            // "none" first: Jev leans to the first option, and doubt must keep what the STT wrote.
            let mut options: Vec<(String, String)> = vec![(
                "none".into(),
                format!("None of them: the speaker said \"{heard}\" as written."),
            )];
            options.extend(
                cands
                    .iter()
                    .enumerate()
                    .map(|(k, c)| (format!("c{}", k + 1), format!("The speaker said \"{c}\"."))),
            );
            (
                names[i].as_str(),
                Question::choice(
                    format!("Which word did the speaker say where the sentence has \"{heard}\"?"),
                    options,
                ),
            )
        })
        .collect();
    let started = Instant::now();
    let cap = VOCAB_CAP.min(st.cfg.voice_jev_cap);
    let answer = tokio::time::timeout(cap, jev.decide("voice_vocab", state, &questions)).await;
    let ms = started.elapsed().as_millis() as u64;
    let d = match answer {
        Err(_) => {
            fail("timeout", ms);
            return message;
        }
        Ok(Err(_)) => {
            fail("error", ms);
            return message;
        }
        Ok(Ok(d)) => d,
    };
    let mut text = message;
    let mut used = false;
    let (mut verdict, mut confidence) = ("none".to_owned(), 0.0);
    for (i, (heard, cands)) in spans.iter().enumerate() {
        let Some((pick, p)) = d.choice(&names[i]) else {
            continue;
        };
        if i == 0 {
            (verdict, confidence) = (pick.to_owned(), p);
        }
        let Some(k) = pick.strip_prefix('c').and_then(|k| k.parse::<usize>().ok()) else {
            continue;
        };
        if p < st.cfg.voice_vocab_min {
            continue;
        }
        if let Some(word) = cands.get(k.wrapping_sub(1)) {
            if let Some(t) = replace_span(&text, heard, word) {
                text = t;
                used = true;
            }
        }
    }
    if let Some(t) = &trace {
        t.push(Layer {
            layer: "vocab",
            verdict,
            confidence,
            ms,
            used,
        });
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggestions_come_from_replaced_runs_of_words() {
        let v = Vocab::default();
        let c = suggest(
            &v,
            "porta il pacco a schiavi garde vai",
            "porta il pacco a Skibigårdsvej",
        );
        // The run "schiavi garde vai" became "Skibigårdsvej".
        assert_eq!(c.len(), 1, "{c:?}");
        assert_eq!(c[0]["word"], "Skibigårdsvej");
        assert_eq!(c[0]["soundsLike"], json!(["schiavi garde vai"]));
        assert_eq!(c[0]["source"], "correction");
        // Same words, different case or punctuation: nothing to learn.
        assert!(suggest(&v, "Ciao Marco.", "ciao marco").is_empty());
        // A pure insertion or deletion has no misheard counterpart.
        assert!(suggest(&v, "porta il pacco", "porta il pacco subito").is_empty());
        // Too short a word, or a run too long, is not a vocabulary word.
        assert!(suggest(&v, "a b c d", "x y z w").is_empty());
    }

    #[test]
    fn a_known_word_points_at_its_entry() {
        let mut v = Vocab::default();
        let mut e = Entry::new("Skibigårdsvej");
        e.id = "vl_1".into();
        v.entries.push(e);
        let c = suggest(&v, "vado a schiavi garde", "vado a skibigårdsvej");
        assert_eq!(c[0]["existingId"], "vl_1");
    }

    #[test]
    fn a_span_is_replaced_as_a_whole_word_only() {
        assert_eq!(
            replace_span("vai a schiavi garde ora", "Schiavi Garde", "Skibigårdsvej").unwrap(),
            "vai a Skibigårdsvej ora"
        );
        assert!(replace_span("la scuola", "scuo", "X").is_none());
        assert_eq!(
            replace_span("ana e anna", "anna", "Hanna").unwrap(),
            "ana e Hanna"
        );
    }

    #[test]
    fn the_context_lines_hold_only_entries_with_a_meaning_the_text_mentions() {
        let mut v = Vocab::default();
        let mut home = Entry::new("Skibigårdsvej");
        home.meaning = Some("the owner's home street".into());
        let bare = Entry::new("Marco");
        v.entries = vec![home, bare];
        assert_eq!(
            lines_for(&v, "porta il pacco a Skibigårdsvej, da Marco"),
            "Vocabulary: Skibigårdsvej = the owner's home street"
        );
        assert_eq!(lines_for(&v, "che ore sono?"), "");
    }

    #[test]
    fn the_memory_text_needs_a_meaning() {
        let mut e = Entry::new("Skibigårdsvej");
        assert!(memory_text(&e).is_none());
        e.meaning = Some("the owner's home street".into());
        assert!(memory_text(&e).unwrap().contains("Skibigårdsvej"));
    }
}
