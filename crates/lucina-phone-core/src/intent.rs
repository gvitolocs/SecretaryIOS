//! The owner asking Lucina about OTPs.

use std::sync::LazyLock;

use regex::Regex;

/// What the owner wants from the OTP bridge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtpAction {
    /// Deliver the newest fresh code (`/otp`, "what's my OTP").
    Take,
    /// Resume forwarding (`/otp arm`).
    Arm,
    /// Pause forwarding and drop pending codes (`/otp stop`).
    Disarm,
    /// Is the bridge listening? (`/otp status`).
    Status,
}

impl OtpAction {
    /// Wire name, as the Node bridge and the old `lucina-device otp-intent` use it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Take => "take",
            Self::Arm => "arm",
            Self::Disarm => "disarm",
            Self::Status => "status",
        }
    }
}

static SUBJECT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?iu)\b(?:otp|verification codes?|security codes?|sms codes?|codic[ei] (?:di (?:verifica|sicurezza|accesso)|sms)|codice otp|verifikationskode|engangskode)\b",
    )
    .expect("OTP subject pattern")
});
static ASK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?iu)\b(?:read|show|tell|send|get|give|check|what|latest|leggi|leggimi|mostra|mostrami|dimmi|mandami|invia|ultimo|qual|læs|vis)\b",
    )
    .expect("OTP ask pattern")
});

/// The OTP action a chat message asks for, if any.
///
/// Commands are exact (`/otp`, `/otp arm`, `/otp stop`, `/otp status`); free
/// text only ever means [`OtpAction::Take`] and needs both an OTP subject and
/// a request verb, so "implement an OTP service" is not a request.
pub fn otp_intent(text: &str) -> Option<OtpAction> {
    let s = text.trim().to_lowercase();
    match s.as_str() {
        "/otp" => return Some(OtpAction::Take),
        "/otp arm" => return Some(OtpAction::Arm),
        "/otp stop" => return Some(OtpAction::Disarm),
        "/otp status" => return Some(OtpAction::Status),
        _ => {}
    }
    (SUBJECT.is_match(&s) && ASK.is_match(&s)).then_some(OtpAction::Take)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_only() {
        assert_eq!(
            otp_intent("dimmi il codice di verifica"),
            Some(OtpAction::Take)
        );
        assert_eq!(otp_intent("  /OTP stop "), Some(OtpAction::Disarm));
        assert_eq!(otp_intent("/otp status"), Some(OtpAction::Status));
        assert_eq!(otp_intent("what is my otp"), Some(OtpAction::Take));
        assert_eq!(otp_intent("implement an OTP service"), None);
        assert_eq!(otp_intent("/otpx"), None);
    }
}
