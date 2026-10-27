//! Meetings end to end over the HTTP API with fakes for Whisper, the diarizer and the LLM:
//! the Mac's resumable upload (308 + Range, 409, a dropped connection, a restart), complete,
//! the pipeline (crash and resume, GPU busy), the folder on "the MyBook", the brief and
//! transcript, naming a speaker, and a recurring unknown voice asked about once.
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::Utc;
use http_body_util::BodyExt;
use lucina_memo::diarize::{wav, DiarizeError, Diarization, Diarizer, Turn};
use lucina_memo::meetings::model::{Kind, MeetingTranscript, Speaker, Status};
use lucina_memo::model::{Segment, Transcript, Word};
use lucina_memo::transcribe::{Stt, SttError};
use lucina_memo::{api, App, Config};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tower::ServiceExt;

const SVC: &str = "svc-test-token-0123456789abcdef";

fn hex(b: &[u8]) -> String {
    Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect()
}

/// Whisper: a fixed transcript per track (by the WAV's name); `busy` first answers "no lease".
struct FakeStt {
    calls: AtomicUsize,
    busy: AtomicUsize,
}

fn words(start: f64, text: &str) -> Vec<Word> {
    text.split_whitespace()
        .enumerate()
        .map(|(i, w)| Word {
            start: start + i as f64 * 0.4,
            end: start + i as f64 * 0.4 + 0.35,
            text: w.into(),
        })
        .collect()
}

fn seg(start: f64, end: f64, text: &str) -> Segment {
    Segment {
        start,
        end,
        text: text.into(),
        speaker: None,
        words: words(start, text),
    }
}

#[async_trait::async_trait]
impl Stt for FakeStt {
    async fn decode(&self, _input: &Path, wav_path: &Path) -> Result<f64, SttError> {
        std::fs::write(wav_path, wav::encode(&vec![0.0; 16_000 * 20])).map_err(|e| SttError::Decode(e.to_string()))?;
        Ok(20.0)
    }
    async fn transcribe_words(&self, wav_path: &Path, _work: &Path, dur: f64, _wait: Duration) -> Result<Transcript, SttError> {
        if self.busy.load(Ordering::SeqCst) > 0 {
            self.busy.fetch_sub(1, Ordering::SeqCst);
            return Err(SttError::GpuBusy(Duration::ZERO));
        }
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mic = wav_path.file_name().unwrap().to_str().unwrap().starts_with("mic");
        let segments = if mic {
            vec![seg(1.0, 3.0, "Good morning, is the exam open book?"), seg(12.0, 14.0, "Thanks, I will read chapter six.")]
        } else {
            // One Whisper segment spans the lecturer and a student (split by the diarizer).
            vec![
                seg(0.0, 1.0, "Welcome everyone."),
                seg(5.0, 9.6, "Yes you may bring notes. Can I ask about the project?"),
                seg(10.0, 11.5, "Next week, Anna."),
            ]
        };
        Ok(Transcript {
            language: "en".into(),
            engine: "fake-whisper".into(),
            duration_sec: dur,
            segments,
            speakers: false,
            created_at: Utc::now(),
        })
    }
    async fn gpu_free(&self) -> bool {
        true
    }
}

/// Two voices in the system track: the lecturer (0–7 s, 9.9–12 s) and a student (7–9.9 s).
struct FakeDiarizer {
    lecturer: Vec<f32>,
    student: Vec<f32>,
}

impl Diarizer for FakeDiarizer {
    fn diarize(&self, samples: &[f32]) -> Result<Diarization, DiarizeError> {
        assert_eq!(samples.len(), 16_000 * 20);
        Ok(Diarization {
            turns: vec![
                Turn { start: 0.0, end: 7.0, speaker: 0 },
                Turn { start: 7.0, end: 9.9, speaker: 1 },
                Turn { start: 9.9, end: 12.0, speaker: 0 },
            ],
            centroids: vec![self.lecturer.clone(), self.student.clone()],
            engine: "fake-diarizer".into(),
        })
    }
    fn embed(&self, _samples: &[f32]) -> Result<Vec<f32>, DiarizeError> {
        Ok(vec![0.0, 0.0, 1.0])
    }
    fn label(&self) -> String {
        "fake".into()
    }
}

struct Env {
    _dir: tempfile::TempDir,
    app: Arc<App>,
    stt: Arc<FakeStt>,
    llm: Arc<lucina_llm::ScriptedModel>,
    base: std::path::PathBuf,
    vault: std::path::PathBuf,
}

fn env() -> Env {
    // Never reach the real media library from a test.
    std::env::set_var("LUCINA_MEMO_LIBRARY_URL", "off");
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("mybook/lucina-meetings");
    let vault = dir.path().join("vault");
    std::fs::create_dir_all(&base).unwrap();
    std::fs::create_dir_all(&vault).unwrap();
    let (d, b, v, voices) = (
        dir.path().join("data").display().to_string(),
        base.display().to_string(),
        vault.display().to_string(),
        dir.path().join("voices.json").display().to_string(),
    );
    let cfg = Config::from_lookup(|k| match k {
        "LUCINA_MEMO_DIR" => Some(d.clone()),
        "LUCINA_MEETINGS_DIR" => Some(b.clone()),
        "LUCINA_MEMO_OBSIDIAN_VAULT" => Some(v.clone()),
        "LUCINA_VOICES_FILE" => Some(voices.clone()),
        "LUCINA_MEMO_TIMEZONE" => Some("Europe/Copenhagen".into()),
        "LUCINA_MEMO_NOTIFY_TELEGRAM" => Some("off".into()),
        _ => None,
    });
    let llm = Arc::new(lucina_llm::ScriptedModel::new());
    let stt = Arc::new(FakeStt {
        calls: AtomicUsize::new(0),
        busy: AtomicUsize::new(0),
    });
    let app = App::new(cfg, llm.clone(), "scripted".into())
        .unwrap()
        .with_tokens(None, Some(SVC))
        .with_engines(
            Some(stt.clone()),
            Some(Arc::new(FakeDiarizer {
                lecturer: vec![1.0, 0.0, 0.0],
                student: vec![0.0, 1.0, 0.0],
            })),
        );
    Env {
        _dir: dir,
        app,
        stt,
        llm,
        base,
        vault,
    }
}

async fn call(app: &Arc<App>, method: &str, path: &str, headers: &[(&str, &str)], body: Body) -> (StatusCode, axum::http::HeaderMap, Value) {
    let mut req = Request::builder().method(method).uri(path).header("authorization", format!("Bearer {SVC}"));
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let res = api::router(app.clone()).oneshot(req.body(body).unwrap()).await.unwrap();
    let status = res.status();
    let h = res.headers().clone();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, h, serde_json::from_slice(&bytes).unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into())))
}

async fn json_call(app: &Arc<App>, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let (s, _, v) = call(app, method, path, &[("content-type", "application/json")], Body::from(body.to_string())).await;
    (s, v)
}

const MIC: &[u8] = b"mic-bytes-0123456789-the-owner";
const SYSTEM: &[u8] = b"system-bytes-abcdefghijklmnopqrstuvwxyz";

fn create_body(title: &str) -> Value {
    json!({
        "title": title,
        "started_at": "2026-10-26T13:30:00.000Z",
        "ended_at": "2026-10-26T14:31:02.000Z",
        "source_app": {"bundle_id": "com.microsoft.teams2", "name": "Microsoft Teams"},
        "calendar_event_id": null,
        "tracks": [
            {"name": "system.m4a", "bytes": SYSTEM.len(), "sha256": hex(SYSTEM), "mime": "audio/mp4", "start_offset_ms": 0},
            {"name": "mic.m4a", "bytes": MIC.len(), "sha256": hex(MIC), "mime": "audio/mp4", "start_offset_ms": 0}
        ]
    })
}

async fn create(app: &Arc<App>, key: &str, title: &str) -> (StatusCode, String) {
    let (s, _, v) = call(
        app,
        "POST",
        "/v1/meetings",
        &[("content-type", "application/json"), ("idempotency-key", key)],
        Body::from(create_body(title).to_string()),
    )
    .await;
    (s, v["id"].as_str().unwrap_or_default().to_owned())
}

/// The Mac's loop: ask for the offset, then send the rest in chunks.
async fn upload(app: &Arc<App>, id: &str, name: &str, data: &'static [u8], chunk: usize) -> Value {
    let url = format!("/v1/meetings/{id}/tracks/{name}");
    let total = data.len();
    let (mut s, mut h, mut v) = call(app, "PUT", &url, &[("content-range", &format!("bytes */{total}"))], Body::empty()).await;
    loop {
        match s {
            StatusCode::PERMANENT_REDIRECT => {
                let offset = h
                    .get("range")
                    .map(|r| r.to_str().unwrap().trim_start_matches("bytes=0-").parse::<usize>().unwrap() + 1)
                    .unwrap_or(0);
                let end = (offset + chunk).min(total);
                (s, h, v) = call(
                    app,
                    "PUT",
                    &url,
                    &[("content-range", &format!("bytes {offset}-{}/{total}", end - 1))],
                    Body::from(&data[offset..end]),
                )
                .await;
            }
            StatusCode::OK => return v,
            other => panic!("{other} {v}"),
        }
    }
}

fn analysis_reply() -> String {
    // Indices follow the merged timeline (see the asserts below).
    json!({
        "speakers": [
            {"id": "me", "role": "student", "name": null},
            {"id": "S1", "role": "professor", "name": null},
            {"id": "S2", "role": "student", "name": "Anna"}
        ],
        "utterances": [{"i": 1, "kind": "question"}, {"i": 2, "kind": "answer"}, {"i": 3, "kind": "question"}, {"i": 5, "kind": "action_item"}]
    })
    .to_string()
}

fn brief_reply() -> String {
    json!({
        "title": "Exam rules and the project",
        "about": "The professor explained the exam rules and when the project is discussed.",
        "key_points": [{"speaker": "S1", "points": ["Notes are allowed in the exam."]}],
        "qa": [{"question": "Is the exam open book?", "asked_by": "me", "answer": "Yes, notes are allowed.", "answered_by": "S1", "at": 1.0}],
        "decisions": [],
        "action_items": [{"text": "Read chapter six", "owner": "me", "due": "2026-11-02", "at": 12.0}],
        "follow_ups": ["Ask about the project next week"],
        "tags": ["exam"]
    })
    .to_string()
}

#[tokio::test]
async fn upload_pipeline_brief_and_naming() {
    let e = env();
    let app = &e.app;

    // Create (idempotent by key), with a folder named after the app (no calendar here).
    let (s, id) = create(app, "rec-1", "Algorithms Q&A").await;
    assert_eq!(s, StatusCode::CREATED);
    assert!(id.starts_with("mtg_"));
    let (s, again) = create(app, "rec-1", "Algorithms Q&A").await;
    assert_eq!((s, again.as_str()), (StatusCode::OK, id.as_str()));
    let folder = e.base.join("2026/2026-10-26 1430 Microsoft Teams – Algorithms Q&A");
    assert!(folder.join("meeting.json").is_file());

    // Completing early is refused; a wrong offset gets 409 with the stored Range.
    let (s, _) = json_call(app, "POST", &format!("/v1/meetings/{id}/complete"), json!({})).await;
    assert_eq!(s, StatusCode::CONFLICT);
    let url = format!("/v1/meetings/{id}/tracks/mic");
    let (s, h, _) = call(app, "PUT", &url, &[("content-range", &format!("bytes 0-4/{}", MIC.len()))], Body::from(&MIC[..5])).await;
    assert_eq!(s, StatusCode::PERMANENT_REDIRECT);
    assert_eq!(h["range"], "bytes=0-4");
    assert!(h.get("location").is_none());
    let (s, h, _) = call(app, "PUT", &url, &[("content-range", &format!("bytes 0-4/{}", MIC.len()))], Body::from(&MIC[..5])).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(h["range"], "bytes=0-4");
    let (s, _, _) = call(app, "PUT", &url, &[("content-range", "bytes */999")], Body::empty()).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    let done = upload(app, &id, "mic.m4a", MIC, 7).await;
    assert_eq!(done["sha256"], hex(MIC));
    let done = upload(app, &id, "system.m4a", SYSTEM, 8).await;
    assert_eq!(done["sha256"], hex(SYSTEM));
    assert_eq!(std::fs::read(folder.join("mic.m4a")).unwrap(), MIC);
    let (s, v) = json_call(app, "POST", &format!("/v1/meetings/{id}/complete"), json!({})).await;
    assert_eq!((s, v["status"].as_str()), (StatusCode::ACCEPTED, Some("processing")));

    // The GPU is busy once: the meeting waits without an attempt counted.
    e.stt.busy.store(1, Ordering::SeqCst);
    assert!(lucina_memo::meetings::worker::process(app, &id).await.is_err());
    let m = app.meetings().unwrap().get(&id).unwrap();
    assert_eq!((m.status, m.attempts), (Status::Queued, 0));
    assert!(m.next_attempt_at.is_some());

    // The model is down for the analysis: one failed attempt; Whisper is not run again.
    lucina_memo::meetings::worker::process(app, &id).await.unwrap_err();
    assert_eq!(e.stt.calls.load(Ordering::SeqCst), 2);
    let m = app.meetings().unwrap().get(&id).unwrap();
    assert_eq!((m.status, m.attempts), (Status::Queued, 1));
    assert!(m.steps.transcribed_at.is_some() && m.steps.diarized_at.is_some() && m.steps.analyzed_at.is_none());

    // The next run continues from the recorded steps (store reopen: meetings::store tests).
    e.llm.push_text(&analysis_reply()).push_text(&brief_reply());
    lucina_memo::meetings::worker::process(app, &id).await.unwrap();
    assert_eq!(e.stt.calls.load(Ordering::SeqCst), 2, "Whisper ran once per track");
    let m = app.meetings().unwrap().get(&id).unwrap();
    assert_eq!(m.status, Status::Done);
    assert_eq!(m.analysis_source.as_deref(), Some("llm"));

    // The folder: every output, no WAVs left.
    for f in ["transcript.md", "transcript.json", "speakers.json", "brief.md", "meeting.json", "mic.m4a", "system.m4a"] {
        assert!(folder.join(f).is_file(), "{f}");
    }
    assert!(!folder.join(".work/system.wav").exists());
    let t: MeetingTranscript = serde_json::from_slice(&std::fs::read(folder.join("transcript.json")).unwrap()).unwrap();
    let lines: Vec<(String, String, Kind)> = t.segments.iter().map(|u| (u.speaker.clone(), u.text.clone(), u.kind)).collect();
    assert_eq!(
        lines,
        vec![
            ("S1".into(), "Welcome everyone.".into(), Kind::Statement),
            ("me".into(), "Good morning, is the exam open book?".into(), Kind::Question),
            ("S1".into(), "Yes you may bring notes.".into(), Kind::Answer),
            ("S2".into(), "Can I ask about the project?".into(), Kind::Question),
            ("S1".into(), "Next week, Anna.".into(), Kind::Statement),
            ("me".into(), "Thanks, I will read chapter six.".into(), Kind::ActionItem),
        ]
    );
    assert_eq!(t.segments[2].role.as_deref(), Some("professor"));
    assert_eq!(t.segments[1].speaker_name, "Giuseppe");
    assert_eq!(t.segments[2].speaker_name, "Professor");
    let speakers: Vec<Speaker> = serde_json::from_slice(&std::fs::read(folder.join("speakers.json")).unwrap()).unwrap();
    let s2 = speakers.iter().find(|s| s.id == "S2").unwrap();
    assert_eq!((s2.name.as_deref(), s2.suggested_name.as_deref()), (None, Some("Anna")));
    assert!(s2.voice_id.as_deref().unwrap().starts_with("vu_"));

    // The Obsidian note in Meetings/, and the index.
    let note = e.vault.join("Meetings/2026-10-26 1430 Microsoft Teams – Algorithms Q&A.md");
    let md = std::fs::read_to_string(&note).unwrap();
    assert!(md.starts_with("---\ntype: meeting\n"));
    assert!(md.contains("- [ ] Read chapter six — Giuseppe, due 2026-11-02, 0:12"));
    assert!(e.vault.join("Meetings/00 Lucina meetings.md").is_file());

    // GET: the Mac's fields.
    let (s, _, v) = call(app, "GET", &format!("/v1/meetings/{id}"), &[], Body::empty()).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["status"], "ready");
    assert!(v["brief"].as_str().unwrap().contains("## Questions and answers"));
    assert!(v["transcript"].as_str().unwrap().contains("[0:05] Professor: Yes you may bring notes."));
    assert_eq!(v["transcript_url"], format!("https://getsecretary.tech/v1/meetings/{id}/transcript"));
    assert!(v["error"].is_null());
    let (s, _, txt) = call(app, "GET", &format!("/v1/meetings/{id}/transcript"), &[], Body::empty()).await;
    assert_eq!(s, StatusCode::OK);
    assert!(txt.as_str().unwrap().contains("**[0:01] Giuseppe** (Student) *question*"));
    let (_, _, list) = call(app, "GET", "/v1/meetings", &[], Body::empty()).await;
    assert_eq!(list["meetings"][0]["id"], id.as_str());

    // Naming S2: the brief and note are made again with the name (no LLM call, no new
    // notification), and the voice joins Anna's profile.
    let requests = e.llm.request_count();
    let (s, v) = json_call(app, "POST", &format!("/v1/meetings/{id}/speakers"), json!({"speaker": "S2", "name": "Anna"})).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["status"], "processing");
    let (s, _) = json_call(app, "POST", &format!("/v1/meetings/{id}/speakers"), json!({"speaker": "S9", "name": "X"})).await;
    assert_eq!(s, StatusCode::CONFLICT, "busy until re-rendered");
    lucina_memo::meetings::worker::process(app, &id).await.unwrap();
    assert_eq!(e.llm.request_count(), requests);
    let t = std::fs::read_to_string(folder.join("transcript.md")).unwrap();
    assert!(t.contains("**[0:07] Anna** (Student) *question*: Can I ask about the project?"));
    let m = app.meetings().unwrap().get(&id).unwrap();
    assert_eq!((m.status, m.revision), (Status::Done, 1));
    let (_, _, voices) = call(app, "GET", "/v1/meetings/voices", &[], Body::empty()).await;
    let names: Vec<&str> = voices["profiles"].as_array().unwrap().iter().map(|p| p["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"Anna") && names.contains(&"Giuseppe"), "{voices}");
}

#[tokio::test]
async fn a_recurring_unknown_voice_is_known_after_naming() {
    let e = env();
    let app = &e.app;
    let mut ids = Vec::new();
    for (key, title) in [("a", "Lecture one"), ("b", "Lecture two")] {
        let (_, id) = create(app, key, title).await;
        upload(app, &id, "mic.m4a", MIC, 64).await;
        upload(app, &id, "system.m4a", SYSTEM, 64).await;
        json_call(app, "POST", &format!("/v1/meetings/{id}/complete"), json!({})).await;
        // The model answers nonsense: the deterministic analysis and brief are used.
        e.llm.push_text("{\"speakers\":[{\"id\":\"S7\",\"role\":\"wizard\"}],\"utterances\":[]}").push_text("not json");
        lucina_memo::meetings::worker::process(app, &id).await.unwrap();
        let m = app.meetings().unwrap().get(&id).unwrap();
        assert_eq!(m.status, Status::Done);
        assert_eq!((m.analysis_source.as_deref(), m.brief_source.as_deref()), (Some("fallback"), Some("fallback")));
        ids.push(id);
    }
    // Both voices were heard twice: each was asked about exactly once (marker set).
    let (_, _, v) = call(app, "GET", "/v1/meetings/voices", &[], Body::empty()).await;
    let unknown = v["unknown"].as_array().unwrap();
    assert_eq!(unknown.len(), 2);
    assert!(unknown.iter().all(|u| u["recordings"] == 2 && !u["asked_at"].is_null()));
    let voice = unknown[0]["id"].as_str().unwrap().to_owned();
    let asked = unknown[0]["asked_at"].clone();
    // A third pass changes nothing about the question.
    lucina_memo::meetings::worker::process(app, &ids[1]).await.ok();
    let (_, _, v) = call(app, "GET", "/v1/meetings/voices", &[], Body::empty()).await;
    assert_eq!(v["unknown"][0]["asked_at"], asked);

    // Naming the voice names it in both meetings.
    let (s, r) = json_call(app, "POST", &format!("/v1/meetings/voices/{voice}"), json!({"name": "Prof. Hansen"})).await;
    assert_eq!(s, StatusCode::OK, "{r}");
    assert_eq!(r["recordings"], 2);
    for id in &ids {
        lucina_memo::meetings::worker::process(app, id).await.unwrap();
        let m = app.meetings().unwrap().get(id).unwrap();
        let t = std::fs::read_to_string(app.meetings().unwrap().dir(&m).join("transcript.md")).unwrap();
        assert!(t.contains("Prof. Hansen"), "{t}");
    }
    // A third meeting recognises the voice by its profile.
    let (_, id) = create(app, "c", "Lecture three").await;
    upload(app, &id, "mic.m4a", MIC, 64).await;
    upload(app, &id, "system.m4a", SYSTEM, 64).await;
    json_call(app, "POST", &format!("/v1/meetings/{id}/complete"), json!({})).await;
    e.llm.push_text(&analysis_reply()).push_text(&brief_reply());
    lucina_memo::meetings::worker::process(app, &id).await.unwrap();
    let m = app.meetings().unwrap().get(&id).unwrap();
    let speakers: Vec<Speaker> = serde_json::from_slice(&std::fs::read(app.meetings().unwrap().dir(&m).join("speakers.json")).unwrap()).unwrap();
    let known: Vec<_> = speakers.iter().filter(|s| s.name_source.as_deref() == Some("profile")).collect();
    assert_eq!(known.len(), 1);
    assert_eq!(known[0].name.as_deref(), Some("Prof. Hansen"));
    assert!(known[0].similarity.unwrap() > 0.99);
}

#[tokio::test]
async fn the_archive_must_be_mounted() {
    std::env::set_var("LUCINA_MEMO_LIBRARY_URL", "off");
    let dir = tempfile::tempdir().unwrap();
    let (d, missing) = (
        dir.path().join("data").display().to_string(),
        dir.path().join("not-mounted/lucina-meetings").display().to_string(),
    );
    let cfg = Config::from_lookup(|k| match k {
        "LUCINA_MEMO_DIR" => Some(d.clone()),
        "LUCINA_MEETINGS_DIR" => Some(missing.clone()),
        _ => None,
    });
    let app = App::new(cfg, Arc::new(lucina_llm::ScriptedModel::new()), "s".into()).unwrap().with_tokens(None, Some(SVC));
    let (s, _) = create(&app, "k", "x").await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
    assert!(!dir.path().join("not-mounted").exists(), "never created on the system disk");
    let (s, _, _) = call(&app, "GET", "/health", &[], Body::empty()).await;
    assert_eq!(s, StatusCode::OK);
}
