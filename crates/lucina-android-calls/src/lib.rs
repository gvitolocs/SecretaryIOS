//! Rust core of the Secretary · Lucina Android app (`tech.getsecretary.secretary`,
//! `docs/lucina-android.md`).
//!
//! The app is a thin Kotlin shell over `liblucina_calls.so`. Kotlin keeps only
//! what Android forces into the JVM (activities, the `InCallService`, the
//! `CallProvider`, permission prompts, `SpeechRecognizer` and text-to-speech);
//! everything else lives here:
//!
//! - [`number`]: which strings are voice-call numbers (never USSD or URIs).
//! - [`journal`]: the private on-device call history and transcripts.
//! - [`net`]: Lucina voice turns and recording transcription over HTTPS.
//! - [`caller`]: screening incoming callers (instant rules verdict, Jev's
//!   verdict from the server, the owner's silence/reject switches).
//! - [`dialer`]: the phone app's call log, missed-call and voicemail
//!   notifications and the remembered Jev label of a number.
//! - [`share`]: validated manifests and streaming multipart share uploads.
//! - [`ui`]: what the in-call screen, notification, keypad, transcript list
//!   and WebView router show or do.
//! - [`phone`]: placing a call through Android Telecom, over JNI.
//! - [`ffi`]: the JNI surface on `tech.getsecretary.secretary.Native`.
//!
//! All but [`phone`] and [`ffi`] are plain Rust and unit-tested on the host.
//! Verification codes never enter the journal, and a voice message that asks
//! for one never reaches the network.

pub mod caller;
pub mod dialer;
pub mod ffi;
pub mod journal;
pub mod net;
pub mod number;
pub mod phone;
pub mod reject;
pub mod share;
pub mod ui;

/// Every fallible operation returns a short, owner-facing error message.
pub type Result<T> = std::result::Result<T, String>;

/// Whether `text` is or asks for a verification code. Such text is never
/// stored in the journal and never sent as a voice message.
pub fn sensitive(text: &str) -> bool {
    lucina_phone_core::otp_intent(text).is_some()
        || lucina_phone_core::parse_otp("Voice", text).is_some()
}

/// Milliseconds since the Unix epoch.
pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_and_code_requests_are_sensitive() {
        assert!(sensitive("show my OTP"));
        assert!(sensitive("Your verification code is 482913"));
        assert!(!sensitive("Call the dentist tomorrow at 9"));
    }
}
