//! Shared core of Lucina's phone bridge (`docs/lucina-android.md`).
//!
//! No I/O: the Android app (`lucina-android`, over JNI), the OTP relay
//! (`lucina-otp-relay`) and Lucina (`lucina-server`) link this crate so they
//! parse, redact and speak the relay protocol the same way.
//!
//! - [`otp`]: OTP detection in SMS and notification text, and redaction of
//!   OTPs in any text that might reach a model or a log.
//! - [`intent`]: the owner asking for a code (`/otp`, "what's my OTP").
//! - [`proto`]: relay request/response bodies and their validation.
//! - [`reply`]: the owner-facing replies, so every surface says the same.
//! - [`caller`]: screening unknown callers with Jev (number parsing, the
//!   state and questions, labels, fallback rules, the table).
//!
//! Codes are secrets with a two-minute life: never log them, never put them in
//! model context, deliver them once to the owner's private chat.

pub mod caller;
pub mod intent;
pub mod otp;
pub mod proto;
pub mod reply;

pub use intent::{otp_intent, OtpAction};
pub use otp::{otp_spans, parse_otp, redact_otps};
