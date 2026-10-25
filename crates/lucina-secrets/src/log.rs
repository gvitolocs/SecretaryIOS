//! The log sink: a `tracing_subscriber` writer that redacts every formatted
//! event before it reaches stdout (journald).
//!
//! ```ignore
//! tracing_subscriber::fmt()
//!     .with_writer(lucina_secrets::log::RedactingMakeWriter::new(std::io::stdout))
//!     .init();
//! ```
//!
//! The fmt layer makes one writer per event and drops it afterwards; the
//! writer buffers the event and writes it redacted on flush or drop, so a
//! value split across `write` calls is still caught.

use std::io::{self, Write};

use tracing_subscriber::fmt::MakeWriter;

use crate::redactor;

#[derive(Debug, Clone)]
pub struct RedactingMakeWriter<M> {
    inner: M,
}

impl<M> RedactingMakeWriter<M> {
    pub fn new(inner: M) -> Self {
        Self { inner }
    }
}

pub struct RedactingWriter<W: Write> {
    inner: W,
    buf: Vec<u8>,
}

impl<W: Write> RedactingWriter<W> {
    fn emit(&mut self) -> io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let text = String::from_utf8_lossy(&self.buf).into_owned();
        self.buf.clear();
        let out = redactor().redact_outbound(&text);
        self.inner.write_all(out.as_bytes())?;
        self.inner.flush()
    }
}

impl<W: Write> Write for RedactingWriter<W> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.emit()
    }
}

impl<W: Write> Drop for RedactingWriter<W> {
    fn drop(&mut self) {
        let _ = self.emit();
    }
}

impl<'a, M> MakeWriter<'a> for RedactingMakeWriter<M>
where
    M: MakeWriter<'a>,
{
    type Writer = RedactingWriter<M::Writer>;

    fn make_writer(&'a self) -> Self::Writer {
        RedactingWriter {
            inner: self.inner.make_writer(),
            buf: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Buf(Arc<Mutex<Vec<u8>>>);

    impl Write for Buf {
        fn write(&mut self, d: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(d);
            Ok(d.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn log_lines_are_redacted() {
        let buf = Buf::default();
        let sink = buf.clone();
        let sub = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(RedactingMakeWriter::new(move || sink.clone()))
            .finish();
        redactor().register("LOG_TEST_KEY", "log-secret-Value-9876");
        tracing::subscriber::with_default(sub, || {
            tracing::info!(body = "got log-secret-Value-9876 back", "upstream said");
            tracing::warn!("header was Bearer abcdEFGH1234ijklMNOP5678");
            tracing::info!("sms said: your verification code is 482917");
        });
        let text = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        assert!(!text.contains("log-secret-Value-9876"), "{text}");
        assert!(text.contains("[secret:LOG_TEST_KEY]"), "{text}");
        assert!(text.contains("Bearer [secret:token]"), "{text}");
        assert!(
            !text.contains("482917") && text.contains("code is [otp]"),
            "{text}"
        );
    }
}
