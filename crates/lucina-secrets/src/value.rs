//! [`SecretValue`]: a resolved secret, for the outbound call only.

use secrecy::{ExposeSecret, SecretString};
use zeroize::Zeroize;

/// A secret value in memory. No `Display`, no `Serialize`, `Debug` prints
/// `SecretValue([redacted])`, the buffer is zeroized on drop (`secrecy`).
///
/// Use it where it leaves the process for the service it belongs to: an
/// HTTP header ([`crate::outbound`]), a child's environment or stdin. Never
/// put it in a tool result, a log line or a prompt (the redactor would mask
/// it anyway).
pub struct SecretValue(SecretString);

impl SecretValue {
    pub fn new(value: String) -> Self {
        Self(SecretString::from(value))
    }

    /// The value, for the outbound call. Keep the borrow short.
    pub fn expose_secret(&self) -> &str {
        self.0.expose_secret()
    }

    pub fn len(&self) -> usize {
        self.0.expose_secret().len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.expose_secret().is_empty()
    }

    /// A second zeroizing copy (the cache hands out copies, never borrows).
    pub fn duplicate(&self) -> Self {
        Self::new(self.0.expose_secret().to_owned())
    }
}

impl Zeroize for SecretValue {
    /// Replaces the value with an empty one; the old buffer is zeroized as
    /// it drops.
    fn zeroize(&mut self) {
        self.0 = SecretString::from(String::new());
    }
}

impl std::fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretValue([redacted])")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_shows_the_value() {
        let v = SecretValue::new("hunter2-very-secret".into());
        assert_eq!(format!("{v:?}"), "SecretValue([redacted])");
        assert_eq!(v.expose_secret(), "hunter2-very-secret");
        let mut d = v.duplicate();
        d.zeroize();
        assert!(d.is_empty());
        assert_eq!(v.len(), 19);
    }
}
