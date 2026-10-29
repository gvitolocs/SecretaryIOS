//! Putting a [`SecretValue`] where it belongs, and nowhere else.
//!
//! - HTTP: [`bearer`] / [`header`] build a `HeaderValue` marked sensitive
//!   (hidden from reqwest/hyper debug output).
//! - Child process: [`child_env`] sets one variable on a `Command` (not on
//!   Lucina's own environment); [`stdin_secret`] writes the value to the
//!   child's stdin (a file descriptor) and closes it.
//!
//! A tool never copies the value into its result: the result goes to the
//! model.

use reqwest::header::HeaderValue;

use crate::{SecretError, SecretValue};

fn sensitive(text: &str) -> Result<HeaderValue, SecretError> {
    let mut v = HeaderValue::from_str(text)
        .map_err(|_| SecretError::Config("secret is not a valid header value".into()))?;
    v.set_sensitive(true);
    Ok(v)
}

/// `Authorization: Bearer <value>`.
pub fn bearer(value: &SecretValue) -> Result<HeaderValue, SecretError> {
    sensitive(&format!("Bearer {}", value.expose_secret()))
}

/// Any header carrying the raw value (`X-Api-Key`, ...).
pub fn header(value: &SecretValue) -> Result<HeaderValue, SecretError> {
    sensitive(value.expose_secret())
}

/// The value as one variable of a child's environment.
pub fn child_env(cmd: &mut tokio::process::Command, var: &str, value: &SecretValue) {
    cmd.env(var, value.expose_secret());
}

/// Write the value to the child's stdin (spawned with `Stdio::piped()`),
/// then close it.
pub async fn stdin_secret(
    child: &mut tokio::process::Child,
    value: &SecretValue,
) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(value.expose_secret().as_bytes()).await?;
        stdin.shutdown().await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headers_are_sensitive() {
        let v = SecretValue::new("tok-1234567890".into());
        let h = bearer(&v).unwrap();
        assert!(h.is_sensitive());
        assert_eq!(format!("{h:?}"), "Sensitive");
        assert!(header(&v).unwrap().is_sensitive());
        assert!(header(&SecretValue::new("bad\nvalue".into())).is_err());
    }

    #[tokio::test]
    async fn child_gets_the_value_by_stdin_and_env() {
        let v = SecretValue::new("child-secret-123".into());
        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c")
            .arg("read x; [ \"$x\" = \"$EXPECT\" ] && echo same")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped());
        child_env(&mut cmd, "EXPECT", &v);
        let mut child = cmd.spawn().unwrap();
        stdin_secret(&mut child, &v).await.unwrap();
        let out = child.wait_with_output().await.unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "same");
        // Lucina's own environment is untouched.
        assert!(std::env::var("EXPECT").is_err());
    }
}
