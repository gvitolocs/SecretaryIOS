//! One judgement per transcript window: which of the nearby talking points has
//! the speaker made?
//!
//! Every candidate first gets the [`matcher`](crate::matcher) score. Points the
//! matcher did not settle (score below [`OBVIOUS`]) become one yes/no Jev
//! question each, current section first, then missed points, then the next
//! section, at most [`MAX_JEV_QUESTIONS`]. Jev's probability replaces the
//! matcher's for the points it answered. When Jev is off, cooling down or
//! fails, the matcher's probabilities stand (fail open).
//!
//! The judge keeps no state: the client owns the points (waiting, started,
//! done, missed) and sends only the ones still open.
use async_trait::async_trait;
use lucina_jev::{Decision, JevError, Question};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::matcher::score;

/// A point counts as covered from this probability.
pub const DONE_P: f64 = 0.75;
/// A point he seems to be on (shown as started) from this probability.
pub const STARTED_P: f64 = 0.4;
/// Matcher score that needs no Jev question (almost the point's own words).
pub const OBVIOUS: f64 = 0.85;
/// Matcher score that maps to [`DONE_P`] when the matcher judges alone.
const LOCAL_DONE: f64 = 0.7;
pub const MAX_CANDIDATES: usize = 32;
pub const MAX_JEV_QUESTIONS: usize = 8;
const MAX_WINDOW_CHARS: usize = 1200;
const MAX_EARLIER_CHARS: usize = 1200;
const MAX_POINT_CHARS: usize = 300;

/// Where a candidate sits relative to the speaker.
#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// An open point of the current section.
    #[default]
    Current,
    /// A point he passed over earlier (the Missed row).
    Missed,
    /// A point of the next section (he may move on without saying so).
    Next,
}

impl Kind {
    fn priority(self) -> u8 {
        match self {
            Kind::Current => 0,
            Kind::Missed => 1,
            Kind::Next => 2,
        }
    }
}

#[derive(Deserialize, Serialize, Clone, Debug, PartialEq)]
pub struct Candidate {
    pub id: String,
    pub text: String,
    #[serde(default)]
    pub kind: Kind,
}

#[derive(Deserialize, Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct JudgeRequest {
    /// The latest speech (the last few seconds).
    pub window: String,
    /// Speech before the window, for context.
    #[serde(default)]
    pub earlier: String,
    /// The current section's title.
    #[serde(default)]
    pub section: String,
    /// Open points, in script order.
    pub candidates: Vec<Candidate>,
    /// false = matcher only.
    #[serde(default = "yes")]
    pub use_jev: bool,
}

fn yes() -> bool {
    true
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Local,
    Jev,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Verdict {
    pub id: String,
    /// Probability that the point was made (what the client thresholds).
    pub p: f64,
    /// The matcher's score.
    pub local: f64,
    /// Jev's probability, when it answered for this point.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jev: Option<f64>,
    pub source: Source,
}

/// What the call cost.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct JevUse {
    /// Questions sent (0 = no call).
    pub asked: usize,
    pub ms: Option<u64>,
    pub input_tokens: u64,
    /// Why Jev did not answer (`unavailable`, `quota`, `timeout`, ...): the matcher decided.
    pub error: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct JudgeResponse {
    pub verdicts: Vec<Verdict>,
    pub jev: JevUse,
}

/// Jev, or a fake in tests.
#[async_trait]
pub trait Decider: Send + Sync {
    /// Configured, keyed and not cooling down.
    fn available(&self) -> bool;
    async fn decide(
        &self,
        state: Value,
        questions: &[(&str, Question)],
    ) -> Result<Decision, JevError>;
}

/// The matcher's score as a probability on the same scale as Jev's.
pub fn local_p(score: f64) -> f64 {
    (score * DONE_P / LOCAL_DONE).clamp(0.0, 1.0)
}

/// The last `max` characters of `s`, starting at a word.
fn tail(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.trim().to_owned();
    }
    let cut: String = s.chars().skip(n - max).collect();
    match cut.find(char::is_whitespace) {
        Some(i) => cut[i..].trim().to_owned(),
        None => cut,
    }
}

fn clip(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

fn question(point: &str) -> Question {
    Question::noul_with(
        format!(
            "Talking point: \"{}\". Has the speaker made this point in the speech (earlier_speech \
             or latest_speech), in any wording or language?",
            clip(point, MAX_POINT_CHARS)
        ),
        "The speech states the idea of this point: a paraphrase, a translation or a shorter form counts.",
        "The speech does not state this point, only hints at it, or only shares a word or two with it.",
    )
}

/// Judge one window. `jev` is None (or unavailable) for the matcher alone.
pub async fn judge(req: &JudgeRequest, jev: Option<&dyn Decider>) -> JudgeResponse {
    let window = tail(&req.window, MAX_WINDOW_CHARS);
    let candidates: Vec<&Candidate> = req
        .candidates
        .iter()
        .filter(|c| !c.id.is_empty() && !c.text.trim().is_empty())
        .take(MAX_CANDIDATES)
        .collect();
    // The matcher reads the window plus the speech just before it, so a point
    // that straddles the window's start still matches.
    let heard = format!("{} {window}", tail(&req.earlier, 300));
    let mut verdicts: Vec<Verdict> = candidates
        .iter()
        .map(|c| {
            let local = score(&c.text, &heard);
            Verdict {
                id: c.id.clone(),
                p: local_p(local),
                local,
                jev: None,
                source: Source::Local,
            }
        })
        .collect();
    let mut usage = JevUse::default();
    if window.is_empty() || candidates.is_empty() {
        return JudgeResponse {
            verdicts,
            jev: usage,
        };
    }
    let jev = match jev {
        Some(j) if req.use_jev && j.available() => j,
        _ => {
            usage.error = Some("unavailable".into());
            return JudgeResponse {
                verdicts,
                jev: usage,
            };
        }
    };
    let mut ask: Vec<usize> = (0..candidates.len())
        .filter(|&i| verdicts[i].local < OBVIOUS)
        .collect();
    ask.sort_by_key(|&i| candidates[i].kind.priority());
    ask.truncate(MAX_JEV_QUESTIONS);
    if ask.is_empty() {
        return JudgeResponse {
            verdicts,
            jev: usage,
        };
    }
    let names: Vec<String> = (0..ask.len()).map(|k| format!("p{k}")).collect();
    let questions: Vec<(&str, Question)> = ask
        .iter()
        .zip(&names)
        .map(|(&i, n)| (n.as_str(), question(&candidates[i].text)))
        .collect();
    let state = json!({
        "task": "A speaker is giving a talk from a list of talking points. Decide which points the speaker has made.",
        "section": clip(&req.section, 200),
        "earlier_speech": tail(&req.earlier, MAX_EARLIER_CHARS),
        "latest_speech": window,
    });
    usage.asked = ask.len();
    match jev.decide(state, &questions).await {
        Ok(d) => {
            usage.ms = Some(d.elapsed_ms);
            usage.input_tokens = d.input_tokens;
            for (k, &i) in ask.iter().enumerate() {
                if let Some(y) = d.yes(&names[k]) {
                    let v = &mut verdicts[i];
                    v.jev = Some(y);
                    v.p = y.clamp(0.0, 1.0);
                    v.source = Source::Jev;
                }
            }
        }
        Err(e) => usage.error = Some(e.code().to_owned()),
    }
    JudgeResponse {
        verdicts,
        jev: usage,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use lucina_jev::Answer;

    use super::*;

    /// Answers each question from `yes` (by point text) or fails with `error`.
    struct Fake {
        available: bool,
        error: Option<JevError>,
        yes: Vec<(&'static str, f64)>,
        seen: Mutex<Vec<(Value, Vec<String>)>>,
    }

    impl Fake {
        fn answering(yes: Vec<(&'static str, f64)>) -> Self {
            Self {
                available: true,
                error: None,
                yes,
                seen: Mutex::default(),
            }
        }
        fn failing(e: JevError) -> Self {
            Self {
                available: true,
                error: Some(e),
                yes: vec![],
                seen: Mutex::default(),
            }
        }
        fn calls(&self) -> usize {
            self.seen.lock().unwrap().len()
        }
    }

    #[async_trait]
    impl Decider for Fake {
        fn available(&self) -> bool {
            self.available
        }
        async fn decide(
            &self,
            state: Value,
            questions: &[(&str, Question)],
        ) -> Result<Decision, JevError> {
            let texts: Vec<String> = questions
                .iter()
                .map(|(_, q)| match q {
                    Question::Noul { instructions, .. } => instructions.clone(),
                    _ => String::new(),
                })
                .collect();
            self.seen.lock().unwrap().push((state, texts.clone()));
            if let Some(e) = &self.error {
                return Err(e.clone());
            }
            let mut answers = BTreeMap::new();
            for ((name, _), text) in questions.iter().zip(&texts) {
                let p = self
                    .yes
                    .iter()
                    .find(|(point, _)| text.contains(point))
                    .map(|(_, p)| *p)
                    .unwrap_or(0.05);
                answers.insert((*name).to_owned(), Answer::Noul(p));
            }
            Ok(Decision {
                model: "jev-test".into(),
                answers,
                input_tokens: 120,
                elapsed_ms: 210,
            })
        }
    }

    fn cand(id: &str, text: &str, kind: Kind) -> Candidate {
        Candidate {
            id: id.into(),
            text: text.into(),
            kind,
        }
    }

    fn req(window: &str, candidates: Vec<Candidate>) -> JudgeRequest {
        JudgeRequest {
            window: window.into(),
            earlier: String::new(),
            section: "How it works".into(),
            candidates,
            use_jev: true,
        }
    }

    fn p(r: &JudgeResponse, id: &str) -> f64 {
        r.verdicts.iter().find(|v| v.id == id).unwrap().p
    }

    const SPEECH: &str =
        "speech recognition runs with whisper on my own server at home, so the audio stays with me";

    #[tokio::test]
    async fn jev_decides_paraphrase_the_matcher_cannot_see() {
        let jev = Fake::answering(vec![("Audio never leaves my machines", 0.93)]);
        let r = judge(
            &req(
                SPEECH,
                vec![cand("a", "Audio never leaves my machines", Kind::Current)],
            ),
            Some(&jev),
        )
        .await;
        assert!(p(&r, "a") >= DONE_P);
        assert_eq!(r.verdicts[0].source, Source::Jev);
        assert_eq!(r.jev.asked, 1);
        assert_eq!(r.jev.ms, Some(210));
    }

    #[tokio::test]
    async fn obvious_hits_need_no_question() {
        let jev = Fake::answering(vec![]);
        let r = judge(
            &req(
                SPEECH,
                vec![cand("w", "Whisper runs on my own server", Kind::Current)],
            ),
            Some(&jev),
        )
        .await;
        assert!(p(&r, "w") >= DONE_P);
        assert_eq!(r.verdicts[0].source, Source::Local);
        assert_eq!(jev.calls(), 0);
        assert_eq!(r.jev.asked, 0);
    }

    #[tokio::test]
    async fn jev_failure_falls_back_to_the_matcher() {
        for e in [
            JevError::Quota(429),
            JevError::Timeout,
            JevError::CoolingDown("rate limit"),
            JevError::Overloaded(503),
        ] {
            let jev = Fake::failing(e.clone());
            let r = judge(
                &req(
                    "and whisper is running on my own server here",
                    vec![
                        cand("w", "Whisper runs on my own server", Kind::Current),
                        cand("j", "Jev decides which point was covered", Kind::Current),
                    ],
                ),
                Some(&jev),
            )
            .await;
            assert!(p(&r, "w") >= DONE_P, "{e:?}: {}", p(&r, "w"));
            assert!(p(&r, "j") < STARTED_P, "{e:?}");
            assert!(r
                .verdicts
                .iter()
                .all(|v| v.source == Source::Local && v.jev.is_none()));
            assert_eq!(r.jev.error.as_deref(), Some(e.code()));
        }
    }

    #[tokio::test]
    async fn unavailable_or_off_jev_is_never_called() {
        let mut jev = Fake::answering(vec![("Audio never leaves my machines", 0.99)]);
        jev.available = false;
        let r = judge(
            &req(
                SPEECH,
                vec![cand("a", "Audio never leaves my machines", Kind::Current)],
            ),
            Some(&jev),
        )
        .await;
        assert_eq!(jev.calls(), 0);
        assert!(p(&r, "a") < DONE_P);
        assert_eq!(r.jev.error.as_deref(), Some("unavailable"));

        let jev = Fake::answering(vec![("Audio never leaves my machines", 0.99)]);
        let mut q = req(
            SPEECH,
            vec![cand("a", "Audio never leaves my machines", Kind::Current)],
        );
        q.use_jev = false;
        judge(&q, Some(&jev)).await;
        assert_eq!(jev.calls(), 0);

        let r = judge(
            &req(
                SPEECH,
                vec![cand("w", "Whisper runs on my own server", Kind::Current)],
            ),
            None,
        )
        .await;
        assert!(p(&r, "w") >= DONE_P);
    }

    #[tokio::test]
    async fn silence_asks_nothing() {
        let jev = Fake::answering(vec![]);
        let r = judge(
            &req("  ", vec![cand("a", "Anything", Kind::Current)]),
            Some(&jev),
        )
        .await;
        assert_eq!(jev.calls(), 0);
        assert_eq!(p(&r, "a"), 0.0);
    }

    #[tokio::test]
    async fn questions_go_current_then_missed_then_next_and_are_capped() {
        let jev = Fake::answering(vec![]);
        let mut cs = vec![cand("n0", "Next section opener about pricing", Kind::Next)];
        cs.push(cand("m0", "A missed point about latency", Kind::Missed));
        for i in 0..10 {
            cs.push(cand(
                &format!("c{i}"),
                &format!("Current point number {i} about topic{i}"),
                Kind::Current,
            ));
        }
        let r = judge(&req("something unrelated entirely", cs), Some(&jev)).await;
        assert_eq!(r.jev.asked, MAX_JEV_QUESTIONS);
        let seen = jev.seen.lock().unwrap();
        let texts = &seen[0].1;
        assert!(texts[0].contains("number 0"));
        assert!(texts
            .iter()
            .all(|t| !t.contains("missed point") && !t.contains("Next section")));
        assert_eq!(r.verdicts.len(), 12, "every candidate still gets a verdict");
    }

    #[tokio::test]
    async fn state_carries_only_text_and_is_clipped() {
        let jev = Fake::answering(vec![]);
        let long = "word ".repeat(2000);
        let mut q = req(&long, vec![cand("a", "Some point here", Kind::Current)]);
        q.earlier = long.clone();
        judge(&q, Some(&jev)).await;
        let seen = jev.seen.lock().unwrap();
        let state = &seen[0].0;
        assert!(state["latest_speech"].as_str().unwrap().chars().count() <= MAX_WINDOW_CHARS);
        assert!(state["earlier_speech"].as_str().unwrap().chars().count() <= MAX_EARLIER_CHARS);
        assert_eq!(state.as_object().unwrap().len(), 4);
    }

    #[test]
    fn local_scale_lines_up_with_the_thresholds() {
        assert!((local_p(0.7) - DONE_P).abs() < 1e-9);
        assert!(local_p(0.4) >= STARTED_P);
        assert!(local_p(0.3) < STARTED_P);
        assert_eq!(local_p(2.0), 1.0);
    }

    #[test]
    fn request_reads_the_web_shape() {
        let r: JudgeRequest = serde_json::from_value(json!({
            "window": "hi", "candidates": [{"id": "s0p1", "text": "Hello", "kind": "missed"}, {"id": "x", "text": "y"}]
        }))
        .unwrap();
        assert!(r.use_jev);
        assert_eq!(r.candidates[0].kind, Kind::Missed);
        assert_eq!(r.candidates[1].kind, Kind::Current);
    }
}
