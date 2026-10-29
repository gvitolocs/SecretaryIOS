//! The optional pixiv.net session (`PIXIV_SESSION_FILE`) and what pixiv says about its account.
//! Logged out, pixiv shows all-ages works only, refuses the R-18 rankings and ignores the
//! Premium-only `popular_d` order; so the lists need to know whether a session is there, whether
//! it is still logged in, whether it is Premium and whether its R-18 display is on.

use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionState {
    /// No session configured: anonymous.
    None,
    /// A session that pixiv still accepts (or could not be checked just now).
    Ok,
    /// A session pixiv answers as logged out.
    Expired,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Account {
    pub session: SessionState,
    pub premium: bool,
    /// The account's R-18 display: `None` when pixiv did not say.
    pub r18: Option<bool>,
}

impl Account {
    pub fn anonymous() -> Account {
        Account {
            session: SessionState::None,
            premium: false,
            r18: Some(false),
        }
    }

    /// A session whose status could not be fetched: lists keep working, flags unknown.
    pub fn unknown() -> Account {
        Account {
            session: SessionState::Ok,
            premium: false,
            r18: None,
        }
    }

    /// A logged-in session (pixiv answers it with the account's works, R-18 included).
    pub fn usable(&self) -> bool {
        self.session == SessionState::Ok
    }

    /// Part of the JSON cache keys: answers for one account are never served to another.
    pub fn fingerprint(&self) -> String {
        if !self.usable() {
            return "anon".into();
        }
        let r = match self.r18 {
            Some(true) => "1",
            Some(false) => "0",
            None => "u",
        };
        format!("user-p{}-r{r}", u8::from(self.premium))
    }
}

/// A flag pixiv may send as a bool, 0/1 or "0"/"1".
fn flag(v: Option<&Value>) -> Option<bool> {
    match v? {
        Value::Bool(b) => Some(*b),
        Value::Number(n) => n.as_i64().map(|n| n != 0),
        Value::String(s) => match s.trim() {
            "1" | "true" => Some(true),
            "0" | "false" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

/// `x_restrict` as a number or a numeric string (0 R-18 hidden, 1 R-18, 2 R-18G too).
fn level(v: Option<&Value>) -> Option<i64> {
    match v? {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// The core's `self_status` answer (`body.user_status`) for a configured session.
pub fn parse_account(v: &Value) -> Account {
    if flag(v.get("is_logged_in")) == Some(false) {
        return Account {
            session: SessionState::Expired,
            premium: false,
            r18: Some(false),
        };
    }
    Account {
        session: SessionState::Ok,
        premium: flag(v.get("is_premium")).unwrap_or(false),
        r18: level(v.get("x_restrict")).map(|x| x >= 1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn flags_come_in_every_shape() {
        let a = parse_account(&json!({"is_logged_in": true, "is_premium": "1", "x_restrict": 2}));
        assert_eq!((a.session, a.premium, a.r18), (SessionState::Ok, true, Some(true)));
        let a = parse_account(&json!({"is_logged_in": 1, "is_premium": 0, "x_restrict": "0"}));
        assert_eq!((a.premium, a.r18), (false, Some(false)));
        let a = parse_account(&json!({"is_logged_in": true}));
        assert_eq!((a.premium, a.r18), (false, None));
        assert_eq!(parse_account(&json!({"is_logged_in": false})).session, SessionState::Expired);
    }

    #[test]
    fn fingerprints_separate_accounts() {
        assert_eq!(Account::anonymous().fingerprint(), "anon");
        assert_eq!(Account::unknown().fingerprint(), "user-p0-ru");
        let a = parse_account(&json!({"is_premium": true, "x_restrict": 1}));
        assert_eq!(a.fingerprint(), "user-p1-r1");
        assert_eq!(parse_account(&json!({"is_logged_in": false})).fingerprint(), "anon");
    }
}
