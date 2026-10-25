//! Lucina voice turns and recording transcription, over HTTPS to the fixed
//! Secretary origin with the owner's WebView session cookie.
//!
//! | Flow | Request |
//! |---|---|
//! | [`voice_turn`] | `POST /v1/chat` `{message, sessionId: "android_voice", clientMessageId, channel: "web"}` |
//! | [`transcribe`] | `POST /v1/calls/transcribe` `{audioBase64, mimeType?, consent: true}` |
//!
//! The transport ([`Https`]) is rustls with the Graviola provider and the
//! webpki roots, no redirects, a 5 s connect and 120 s total timeout and a
//! 512 KiB response limit; one agent is reused for the process. The flows
//! take any [`Backend`], so they are tested without a network.

use std::{
    sync::{Arc, OnceLock},
    time::Duration,
};

use base64::Engine;
use serde_json::{json, Value};

use crate::{journal::Journal, Result};

/// The only origin requests go to.
pub const ORIGIN: &str = "https://getsecretary.tech";
/// Longest voice message, in bytes.
pub const MAX_VOICE_BYTES: usize = 12_000;
/// Largest recording sent for transcription.
pub const MAX_AUDIO_BYTES: usize = 8 * 1024 * 1024;
/// Largest response read.
pub const MAX_RESPONSE_BYTES: u64 = 512 * 1024;

/// Said instead of sending a voice request for a verification code.
pub const ASK_IN_PRIVATE_CHAT: &str = "Ask for verification codes in your private text chat.";
/// Said instead of a reply Lucina marked sensitive.
pub const SENSITIVE_REPLY: &str = "A sensitive reply is available in your private chat.";
/// Said when Lucina's response has no reply text.
pub const NO_REPLY: &str = "No reply was returned.";

/// Something that can POST JSON to Secretary and return its JSON answer.
pub trait Backend {
    /// POST `body` to `ORIGIN + path`.
    fn post(&self, path: &str, body: &Value) -> Result<Value>;
}

/// The real transport, authenticated by the WebView's session cookie.
pub struct Https<'a> {
    cookie: &'a str,
}

impl<'a> Https<'a> {
    /// A transport for the `Cookie` header value `cookie`.
    pub fn new(cookie: &'a str) -> Self {
        Self { cookie }
    }
}

fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        let tls = ureq::tls::TlsConfig::builder()
            .unversioned_rustls_crypto_provider(Arc::new(rustls_graviola::default_provider()))
            .build();
        ureq::Agent::config_builder()
            .tls_config(tls)
            .https_only(true)
            .timeout_connect(Some(Duration::from_secs(5)))
            .timeout_global(Some(Duration::from_secs(120)))
            .max_redirects(0)
            .build()
            .new_agent()
    })
}

impl Backend for Https<'_> {
    fn post(&self, path: &str, body: &Value) -> Result<Value> {
        if self.cookie.trim().is_empty() {
            return Err("Sign in to Secretary first.".into());
        }
        if !path.starts_with("/v1/") || self.cookie.contains(['\r', '\n']) {
            return Err("Invalid request.".into());
        }
        let mut response = agent()
            .post(format!("{ORIGIN}{path}"))
            .header("Cookie", self.cookie)
            .header("Origin", ORIGIN)
            .header("X-Secretary-CSRF", "1")
            .header("X-Secretary-Client", "web")
            .send_json(body)
            .map_err(|_| {
                "The request failed. Check your sign-in, connection or provider availability."
            })?;
        let bytes = response
            .body_mut()
            .with_config()
            .limit(MAX_RESPONSE_BYTES)
            .read_to_vec()
            .map_err(|_| "Response exceeded limit.")?;
        serde_json::from_slice(&bytes).map_err(|_| "Invalid response.".into())
    }
}

/// Ask Lucina for a spoken reply to `text`. A request for a verification
/// code never leaves the phone, and a reply Lucina marks sensitive is
/// replaced by [`SENSITIVE_REPLY`].
pub fn voice_reply(backend: &dyn Backend, text: &str) -> Result<Value> {
    if text.trim().is_empty() || text.len() > MAX_VOICE_BYTES {
        return Err("Invalid voice message.".into());
    }
    if crate::sensitive(text) {
        return Ok(json!({ "reply": ASK_IN_PRIVATE_CHAT, "sensitive": true }));
    }
    let body = json!({
        "message": text,
        "sessionId": "android_voice",
        "clientMessageId": format!("voice-{}-{}", crate::now_ms(), std::process::id()),
        "channel": "web",
    });
    let value = backend.post("/v1/chat", &body)?;
    if value["sensitive"] == true {
        return Ok(json!({ "reply": SENSITIVE_REPLY, "sensitive": true }));
    }
    Ok(value)
}

/// One turn of a voice conversation: journal what the owner said, ask Lucina,
/// journal her answer. Returns `{"reply", "sensitive", "speak"}`; `speak` is
/// false for sensitive replies, which are shown, not read aloud.
///
/// Journal failures do not stop the conversation.
pub fn voice_turn(
    journal: &Journal,
    backend: &dyn Backend,
    id: &str,
    heard: &str,
) -> Result<Value> {
    let _ = journal.append(id, "user", heard);
    let reply = voice_reply(backend, heard)?;
    let answer = reply["reply"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(NO_REPLY);
    let sensitive = reply["sensitive"] == true;
    let _ = journal.append(id, "lucina", answer);
    Ok(json!({ "reply": answer, "sensitive": sensitive, "speak": !sensitive }))
}

/// The MIME type sent for a recording: `audio/mp4` is sent as `audio/m4a`,
/// which the transcription model accepts; anything that is not a plain
/// `type/subtype` is left out.
pub fn recording_mime(mime: Option<&str>) -> Option<String> {
    let mime = mime?.trim().to_ascii_lowercase();
    let valid = mime.len() <= 100
        && mime.split_once('/').is_some_and(|(t, s)| {
            !t.is_empty()
                && !s.is_empty()
                && mime.bytes().filter(|b| *b == b'/').count() == 1
                && mime
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"/.+-".contains(&b))
        });
    match mime.as_str() {
        _ if !valid => None,
        "audio/mp4" => Some("audio/m4a".into()),
        _ => Some(mime),
    }
}

/// Transcribe a recording and keep the transcript in the journal.
///
/// `req` is `{"consent": true, "mimeType"?, "id"?}`: with `id` the transcript
/// is attached to that record, without it a new `recording` record is made
/// and closed as `transcribed`. Returns `{"id", "saved", "partial"}`.
pub fn transcribe(
    journal: &Journal,
    backend: &dyn Backend,
    req: &Value,
    audio: &[u8],
) -> Result<Value> {
    if req["consent"] != true {
        return Err("Confirm sending this recording to Gemini.".into());
    }
    if audio.is_empty() {
        return Err("The recording is empty.".into());
    }
    if audio.len() > MAX_AUDIO_BYTES {
        return Err("Recording exceeds 8 MiB.".into());
    }
    let mut body = json!({
        "audioBase64": base64::engine::general_purpose::STANDARD.encode(audio),
        "consent": true,
    });
    if let Some(mime) = recording_mime(req["mimeType"].as_str()) {
        body["mimeType"] = json!(mime);
    }
    let out = backend.post("/v1/calls/transcribe", &body)?;
    let mut text = out["transcript"]
        .as_str()
        .ok_or("No transcript was returned.")?
        .to_owned();
    let partial = out["partial"] == true;
    if partial {
        text = format!("[Partial transcript]\n{text}");
    }
    let given = req["id"].as_str().filter(|s| !s.is_empty());
    let id = match given {
        Some(id) => id.to_owned(),
        None => journal.begin("recording", "Imported recording")?,
    };
    let saved = journal.append(&id, "recording", &text)?;
    if given.is_none() {
        journal.end(&id, "transcribed")?;
    }
    Ok(json!({ "id": id, "saved": saved, "partial": partial }))
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::journal::{tests::app_dir, Op};

    /// Answers every request with `answer` and remembers what was sent.
    struct Fake {
        answer: Value,
        sent: RefCell<Vec<(String, Value)>>,
    }

    impl Fake {
        fn new(answer: Value) -> Self {
            Self {
                answer,
                sent: RefCell::new(Vec::new()),
            }
        }
    }

    impl Backend for Fake {
        fn post(&self, path: &str, body: &Value) -> Result<Value> {
            self.sent.borrow_mut().push((path.into(), body.clone()));
            Ok(self.answer.clone())
        }
    }

    #[test]
    fn no_voice_otp_request_is_sent_to_the_network() {
        let fake = Fake::new(json!({"reply": "x"}));
        assert_eq!(
            voice_reply(&fake, "show my OTP").unwrap()["sensitive"],
            true
        );
        assert!(fake.sent.borrow().is_empty());
        assert!(voice_reply(&fake, "  ").is_err());
        assert!(voice_reply(&fake, &"a".repeat(MAX_VOICE_BYTES + 1)).is_err());
    }

    #[test]
    fn the_real_transport_needs_a_session() {
        assert_eq!(
            Https::new("").post("/v1/chat", &json!({})).unwrap_err(),
            "Sign in to Secretary first."
        );
        assert_eq!(
            Https::new("a=b\r\nX: y")
                .post("/v1/chat", &json!({}))
                .unwrap_err(),
            "Invalid request."
        );
    }

    #[test]
    fn voice_turn_journals_both_sides_and_hides_sensitive_replies() {
        let journal = Journal::open(&app_dir("voice")).unwrap();
        let id = journal.begin("internet", "Lucina").unwrap();
        let fake = Fake::new(json!({"reply": "It is sunny."}));
        let out = voice_turn(&journal, &fake, &id, "What's the weather?").unwrap();
        assert_eq!(
            out,
            json!({"reply": "It is sunny.", "sensitive": false, "speak": true})
        );
        let sent = fake.sent.borrow();
        assert_eq!(sent[0].0, "/v1/chat");
        assert_eq!(sent[0].1["sessionId"], "android_voice");
        assert_eq!(sent[0].1["channel"], "web");
        assert!(sent[0].1["clientMessageId"]
            .as_str()
            .unwrap()
            .starts_with("voice-"));
        let secret = Fake::new(json!({"reply": "Your code is 123456", "sensitive": true}));
        let out = voice_turn(&journal, &secret, &id, "Read my last email").unwrap();
        assert_eq!(
            out,
            json!({"reply": SENSITIVE_REPLY, "sensitive": true, "speak": false})
        );
        let empty = Fake::new(json!({}));
        assert_eq!(
            voice_turn(&journal, &empty, &id, "Hello").unwrap()["reply"],
            NO_REPLY
        );
        let h = journal.run(Op::History, &json!({})).unwrap();
        let turns = h["records"][0]["turns"].as_array().unwrap();
        let roles: Vec<_> = turns.iter().map(|t| t["role"].as_str().unwrap()).collect();
        assert_eq!(
            roles,
            ["user", "lucina", "user", "lucina", "user", "lucina"]
        );
        assert!(turns
            .iter()
            .all(|t| !t["text"].as_str().unwrap().contains("123456")));
    }

    #[test]
    fn transcription_needs_consent_and_lands_in_the_journal() {
        let journal = Journal::open(&app_dir("transcribe")).unwrap();
        let fake = Fake::new(json!({"transcript": "Hello there", "partial": true}));
        assert!(transcribe(&journal, &fake, &json!({}), b"abc")
            .unwrap_err()
            .contains("Confirm"));
        assert!(transcribe(&journal, &fake, &json!({"consent": true}), b"").is_err());
        assert!(transcribe(
            &journal,
            &fake,
            &json!({"consent": true}),
            &vec![0; MAX_AUDIO_BYTES + 1]
        )
        .is_err());
        assert!(fake.sent.borrow().is_empty());
        let out = transcribe(
            &journal,
            &fake,
            &json!({"consent": true, "mimeType": "audio/mp4"}),
            b"abc",
        )
        .unwrap();
        assert_eq!(out["saved"], true);
        assert_eq!(out["partial"], true);
        let sent = fake.sent.borrow();
        assert_eq!(sent[0].0, "/v1/calls/transcribe");
        assert_eq!(
            sent[0].1,
            json!({"audioBase64": "YWJj", "consent": true, "mimeType": "audio/m4a"})
        );
        drop(sent);
        let h = journal.run(Op::History, &json!({})).unwrap();
        let r = &h["records"][0];
        assert_eq!(
            (r["kind"].as_str(), r["peer"].as_str(), r["status"].as_str()),
            (
                Some("recording"),
                Some("Imported recording"),
                Some("transcribed")
            )
        );
        assert_eq!(r["turns"][0]["text"], "[Partial transcript]\nHello there");
        let sim = journal.begin("sim", "+4512345678").unwrap();
        let whole = Fake::new(json!({"transcript": "Second"}));
        let out = transcribe(
            &journal,
            &whole,
            &json!({"consent": true, "id": sim}),
            b"abc",
        )
        .unwrap();
        assert_eq!(out["id"], sim);
        let h = journal.run(Op::History, &json!({})).unwrap();
        assert_eq!(h["records"][0]["status"], "active");
        assert_eq!(h["records"][0]["turns"][0]["text"], "Second");
        assert!(transcribe(
            &journal,
            &Fake::new(json!({})),
            &json!({"consent": true}),
            b"a"
        )
        .is_err());
    }

    #[test]
    fn recording_mime_types() {
        assert_eq!(
            recording_mime(Some("audio/mp4")).as_deref(),
            Some("audio/m4a")
        );
        assert_eq!(
            recording_mime(Some("Audio/MPEG")).as_deref(),
            Some("audio/mpeg")
        );
        assert_eq!(recording_mime(Some("audio/ogg; codecs=opus")), None);
        assert_eq!(recording_mime(Some("audio")), None);
        assert_eq!(recording_mime(None), None);
    }
}
