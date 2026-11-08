//! Voice Learn: the shared custom vocabulary of every speech-to-text path
//! (`docs/lucinavoice-protocol.md` section 8). Lucina owns the entries; this
//! module is the gateway for the web app and the speaker, plus the one place in
//! secretary-server that uses them: the "Vocabulary:" line of the Gemini
//! fallback.
//!
//! **Routes** (behind device auth; see [`routes`]):
//!
//! | route | who |
//! |---|---|
//! | `GET /v1/voice/learn` | the owner's browser session, any device token (the speaker too) |
//! | `POST /v1/voice/learn`, `PATCH\|DELETE /v1/voice/learn/{id}`, `POST /v1/voice/learn/suggest`, `POST /v1/voice/learn/sample` | the owner's browser session (cookie) only; any device token gets `403 owner_only` |
//!
//! The same rule as `/v1/voice/devices*`: a device token only ever reads (here: the list, so a
//! speaker can have it), and only the owner's user session does the owner things. So a speaker
//! that has just been paired and never said hello can never write either, and the iOS app (a
//! device token) reads the vocabulary but edits it from the web app.
//!
//! The four Lucina routes are forwarded as is (`/api/secretary/voice-learn`,
//! [`Call::voice_learn_list`] and friends); Lucina validates the fields and
//! answers `400` with its own message, which reaches the client unchanged.
//!
//! **`POST /v1/voice/learn/sample`** turns a short phone recording into what
//! the speech-to-text engine heard, so the app can offer it as `soundsLike`.
//! The body is `audio/wav` (PCM) or `audio/x-s16le;rate=16000` of at most
//! [`MAX_SAMPLE_SECONDS`]; the web app decodes the MediaRecorder clip in the
//! browser and sends 16 kHz PCM WAV. It goes through [`crate::voice::transcribe`]
//! (the local engine with the caller's credentials, Gemini as the fallback),
//! and the answer is `{heard, corrections}`. Nothing is stored. `heard` is the
//! engine's text after its own Voice Learn post-correction, so a word it
//! already fixed shows up in `corrections` (`from` is what it first heard).
//! `422 no_speech` for silence.
//!
//! **Gemini vocabulary.** [`VocabCache::line`] fetches the entries with the
//! server's Lucina token and builds `Vocabulary: word (sounds like a, b); …`
//! (at most [`LINE_CHARS`] characters), cached for [`TTL`]. It runs only on
//! the Gemini fallback path and fails open: no Lucina, an error, or a slow
//! answer ([`FETCH_TIMEOUT`]) means no line.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, FromRequest, Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, patch, post};
use axum::{Extension, Json, Router};
use secretary_core::Call;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::{Identity, Via};
use crate::error::{ApiError, JsonBody};
use crate::routes::forward;
use crate::voice::{self, Heard};
use crate::AppState;

/// Longest sample accepted, in seconds of audio.
pub const MAX_SAMPLE_SECONDS: f64 = 10.0;
/// Largest sample body: 10 s of 16 kHz s16le plus a WAV header, with some slack.
pub const MAX_SAMPLE_BYTES: usize = 330_000;
/// The vocabulary is fetched again after this long.
pub const TTL: Duration = Duration::from_secs(60);
/// After a failed fetch the next try is this far off.
const FAIL_TTL: Duration = Duration::from_secs(15);
/// The Gemini fallback never waits longer than this for the vocabulary.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(3);
/// The "Vocabulary:" line stops here (about 200 tokens).
pub const LINE_CHARS: usize = 700;

fn owner_only() -> ApiError {
    ApiError::new(
        StatusCode::FORBIDDEN,
        "owner_only",
        "only the owner's session can change the vocabulary; a device token can only read it",
    )
}

/// Writes need the owner's browser session (cookie), like `/v1/voice/devices*`: a device
/// token only ever reads, whatever device it is (a speaker, one paired but not connected yet,
/// the phone app).
fn require_owner(ident: &Identity) -> Result<(), ApiError> {
    if matches!(ident.via, Via::Cookie) {
        Ok(())
    } else {
        Err(owner_only())
    }
}

async fn list(State(st): State<AppState>) -> Result<(StatusCode, Json<Value>), ApiError> {
    forward(&st, Call::voice_learn_list()).await
}

async fn create(
    State(st): State<AppState>,
    Extension(ident): Extension<Identity>,
    JsonBody(body): JsonBody<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    require_owner(&ident)?;
    forward(&st, Call::voice_learn_create(body)?).await
}

async fn update(
    State(st): State<AppState>,
    Extension(ident): Extension<Identity>,
    Path(id): Path<String>,
    JsonBody(body): JsonBody<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    require_owner(&ident)?;
    forward(&st, Call::voice_learn_update(&id, body)?).await
}

async fn remove(
    State(st): State<AppState>,
    Extension(ident): Extension<Identity>,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    require_owner(&ident)?;
    forward(&st, Call::voice_learn_delete(&id)?).await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SuggestBody {
    transcript: String,
    corrected: String,
}

async fn suggest(
    State(st): State<AppState>,
    Extension(ident): Extension<Identity>,
    JsonBody(b): JsonBody<SuggestBody>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    require_owner(&ident)?;
    forward(&st, Call::voice_learn_suggest(&b.transcript, &b.corrected)?).await
}

#[derive(Deserialize)]
struct SampleQuery {
    lang: Option<String>,
}

async fn sample(
    State(st): State<AppState>,
    Extension(ident): Extension<Identity>,
    req: Request,
) -> Result<Json<Value>, ApiError> {
    require_owner(&ident)?;
    let kind = voice::input_kind(req.headers())?;
    let Query(SampleQuery { lang }) = Query::<SampleQuery>::try_from_uri(req.uri())
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, "bad_request", e.body_text()))?;
    // The language hint of the engine: the three Voice Learn languages (it, en, da; it-IT works too).
    if let Some(l) = lang.as_deref() {
        if !matches!(l.split(['-', '_']).next().map(str::to_ascii_lowercase).as_deref(), Some("it" | "en" | "da")) {
            return Err(ApiError::new(StatusCode::BAD_REQUEST, "bad_request", "lang: it, en or da"));
        }
    }
    if let Err(wait) = st.voice.limiter.check(&ident.device_id) {
        return Err(ApiError::rate_limited(wait));
    }
    let caller: HeaderMap = req.headers().clone();
    let body = Bytes::from_request(req, &st).await?;
    if body.is_empty() {
        return Err(voice::no_speech());
    }
    let (audio, seconds) = voice::audio_of(kind, body)?;
    if seconds > MAX_SAMPLE_SECONDS + 0.25 {
        return Err(voice::invalid_audio("a sample is at most 10 seconds"));
    }
    if seconds < voice::MIN_SECONDS {
        return Err(voice::no_speech());
    }
    let Heard { text, facts } = voice::transcribe(&st, &caller, &audio, lang.as_deref()).await?;
    if text.is_empty() {
        return Err(voice::no_speech());
    }
    Ok(Json(json!({"heard": text, "corrections": facts.corrections_json()})))
}

/// Behind `require_device`.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/voice/learn", get(list).post(create))
        .route("/v1/voice/learn/suggest", post(suggest))
        .route(
            "/v1/voice/learn/sample",
            post(sample).layer(DefaultBodyLimit::max(MAX_SAMPLE_BYTES)),
        )
        .route("/v1/voice/learn/{id}", patch(update).delete(remove))
}

// -- the Gemini vocabulary line ---------------------------------------------------------

/// The entry list inside Lucina's answer: a bare array, or an object holding
/// it under `entries`, `items` or `words`.
pub fn entries(v: &Value) -> &[Value] {
    if let Some(a) = v.as_array() {
        return a;
    }
    for key in ["entries", "items", "words"] {
        if let Some(a) = v[key].as_array() {
            return a;
        }
    }
    &[]
}

fn plain(v: &Value) -> String {
    v.as_str()
        .unwrap_or("")
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '<' | '>' | ';' | '(' | ')'))
        .collect::<String>()
        .trim()
        .to_owned()
}

/// `Vocabulary: word (sounds like a, b); word2` from Lucina's entries, newest
/// first when they carry `updatedAt`, cut at [`LINE_CHARS`]. `None` for no entries.
pub fn vocabulary_line(list: &Value) -> Option<String> {
    let mut items: Vec<&Value> = entries(list).iter().collect();
    // ISO timestamps sort as text; entries without one keep Lucina's order, last.
    items.sort_by(|a, b| {
        b["updatedAt"]
            .as_str()
            .unwrap_or("")
            .cmp(a["updatedAt"].as_str().unwrap_or(""))
    });
    let mut line = String::from("Vocabulary: ");
    let base = line.chars().count();
    let mut first = true;
    for e in items {
        let word = plain(&e["word"]);
        if word.is_empty() {
            continue;
        }
        let sounds: Vec<String> = e["soundsLike"]
            .as_array()
            .into_iter()
            .flatten()
            .map(plain)
            .filter(|s| !s.is_empty())
            .take(3)
            .collect();
        let item = if sounds.is_empty() {
            word
        } else {
            format!("{word} (sounds like {})", sounds.join(", "))
        };
        let sep = if first { "" } else { "; " };
        if line.chars().count() + sep.len() + item.chars().count() > LINE_CHARS {
            break;
        }
        line.push_str(sep);
        line.push_str(&item);
        first = false;
    }
    (line.chars().count() > base).then_some(line)
}

/// The vocabulary as the Gemini fallback uses it: the prompt line and the entries for
/// post-correction ([`lucina_vocab::Vocab::correct`]).
#[derive(Debug, Clone, Default)]
pub struct Loaded {
    /// `Vocabulary: …`, `None` for no entries.
    pub line: Option<String>,
    pub vocab: lucina_vocab::Vocab,
}

impl Loaded {
    /// From Lucina's list answer. Entries that do not parse are skipped.
    pub fn from_list(list: &Value) -> Self {
        let entries = entries(list)
            .iter()
            .filter_map(|e| serde_json::from_value::<lucina_vocab::Entry>(e.clone()).ok())
            .filter(|e| !e.word.trim().is_empty())
            .collect();
        Self {
            line: vocabulary_line(list),
            vocab: lucina_vocab::Vocab { entries },
        }
    }
}

/// Lucina's vocabulary for the Gemini fallback, fetched at most once per [`TTL`].
#[derive(Default)]
pub struct VocabCache {
    /// (when it was fetched, what came; `None` after a failure)
    slot: Mutex<Option<(Instant, Option<Arc<Loaded>>)>>,
}

impl VocabCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// The cached answer, if still fresh.
    fn fresh(&self) -> Option<Option<Arc<Loaded>>> {
        let slot = self.slot.lock().unwrap_or_else(|p| p.into_inner());
        let (at, got) = slot.as_ref()?;
        let ttl = if got.as_ref().is_some_and(|l| l.line.is_some()) { TTL } else { FAIL_TTL };
        (at.elapsed() < ttl).then(|| got.clone())
    }

    fn store(&self, got: Option<Arc<Loaded>>) {
        *self.slot.lock().unwrap_or_else(|p| p.into_inner()) = Some((Instant::now(), got));
    }

    /// The vocabulary, or `None` (Lucina not configured or down, or too slow). Never an error.
    pub async fn load(&self, st: &AppState) -> Option<Arc<Loaded>> {
        if let Some(got) = self.fresh() {
            return got;
        }
        let got = match st.lucina() {
            Ok(lucina) => {
                let call = Call::voice_learn_list();
                let fetch = lucina.call::<Value>(&call);
                match tokio::time::timeout(FETCH_TIMEOUT, fetch).await {
                    Ok(Ok(list)) => Some(Arc::new(Loaded::from_list(&list))),
                    Ok(Err(e)) => {
                        tracing::debug!(error = %e, "voice learn: vocabulary fetch failed");
                        None
                    }
                    Err(_) => {
                        tracing::debug!("voice learn: vocabulary fetch timed out");
                        None
                    }
                }
            }
            Err(_) => None,
        };
        self.store(got.clone());
        got
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_line_lists_words_and_sounds_like_newest_first() {
        let list = json!({"entries": [
            {"id": "a", "word": "Lucina", "updatedAt": "2026-11-01T10:00:00Z"},
            {"id": "b", "word": "Skibigårdsvej", "soundsLike": ["skibi gård vej", "skibigor"], "updatedAt": "2026-11-01T12:00:00Z"},
            {"id": "c", "word": "  ", "updatedAt": "2026-11-01T13:00:00Z"},
        ]});
        assert_eq!(
            vocabulary_line(&list).unwrap(),
            "Vocabulary: Skibigårdsvej (sounds like skibi gård vej, skibigor); Lucina"
        );
        // a bare array works too, and an empty list gives no line
        assert_eq!(vocabulary_line(&json!([{"word": "Mikkel"}])).unwrap(), "Vocabulary: Mikkel");
        assert!(vocabulary_line(&json!({"entries": []})).is_none());
        assert!(vocabulary_line(&json!({"nope": 1})).is_none());
        assert!(vocabulary_line(&Value::Null).is_none());
    }

    #[test]
    fn the_line_is_clean_and_bounded() {
        let list = json!([{"word": "a;b(c)<d>\n", "soundsLike": ["x\u{0}y"]}]);
        assert_eq!(vocabulary_line(&list).unwrap(), "Vocabulary: abcd (sounds like xy)");
        let many: Vec<Value> = (0..500).map(|i| json!({"word": format!("parola{i}")})).collect();
        let line = vocabulary_line(&Value::Array(many)).unwrap();
        assert!(line.chars().count() <= LINE_CHARS, "{}", line.chars().count());
        assert!(line.ends_with(|c: char| c.is_ascii_digit()));
    }

    #[test]
    fn the_cache_remembers_a_vocabulary_and_a_failure_for_different_times() {
        let c = VocabCache::new();
        assert!(c.fresh().is_none());
        let loaded = Arc::new(Loaded::from_list(&json!([{"word": "Mikkel"}])));
        assert_eq!(loaded.line.as_deref(), Some("Vocabulary: Mikkel"));
        c.store(Some(loaded));
        assert!(c.fresh().unwrap().is_some());
        // an old success is stale after TTL; an old failure after FAIL_TTL
        let old = |by: Duration, got: Option<Arc<Loaded>>| {
            *c.slot.lock().unwrap() = Some((Instant::now() - by, got));
        };
        old(TTL + Duration::from_secs(1), Some(Arc::new(Loaded::from_list(&json!([{"word": "x"}])))));
        assert!(c.fresh().is_none());
        old(FAIL_TTL + Duration::from_secs(1), None);
        assert!(c.fresh().is_none());
        old(Duration::from_secs(5), None);
        assert_eq!(c.fresh().map(|g| g.is_none()), Some(true));
    }

    #[test]
    fn entries_parse_like_the_vocab_crate_reads_them_and_bad_ones_are_skipped() {
        let l = Loaded::from_list(&json!({"entries": [
            {"id": "vl_1", "word": "Skibigårdsvej", "soundsLike": ["skibi gård vej"], "meaning": "home", "lang": "da", "source": "app", "createdAt": "x", "updatedAt": "y"},
            {"id": "vl_2"}, {"word": "  "}, 7
        ]}));
        assert_eq!(l.vocab.entries.len(), 1);
        assert_eq!(l.vocab.entries[0].sounds_like, ["skibi gård vej"]);
        let c = l.vocab.correct("Skibi gaardsvej");
        assert_eq!(c.text, "Skibigårdsvej", "{c:?}");
    }

    #[test]
    fn in_process_corrections_become_the_same_facts_as_the_engines() {
        let l = Loaded::from_list(&json!([{"word": "Mikkel"}, {"word": "Michel"}, {"word": "Skibigårdsvej", "soundsLike": ["skibi gård vej"]}]));
        let c = l.vocab.correct("Ciao Mikel, vai a Skibi gaardsvej");
        let facts = crate::voice::SttFacts::from_corrected(&c);
        assert_eq!(c.text, "Ciao Mikel, vai a Skibigårdsvej");
        assert_eq!(facts.corrections.len(), 1);
        assert_eq!(facts.corrections[0].to, "Skibigårdsvej");
        assert_eq!(
            facts.ambiguous_json(),
            [json!({"heard": "Mikel", "candidates": ["Mikkel", "Michel"]})]
        );
    }
}
