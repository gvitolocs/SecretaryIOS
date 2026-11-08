//! One handler per file type. The router (`super::router`) picks the
//! handler; each returns an [`Extracted`] that is stored in the attachment
//! memory and shown to Lucina with that type's guidance (`super::guidance`).
//! Handlers here are pure or blocking file readers; network and model calls
//! live in the orchestration (`super::extract`).

pub mod archive;
pub mod code;
pub mod ics;
pub mod sheet;
pub mod vcf;
pub mod youtube;

use serde_json::Value;

/// What a handler read from one shared part.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Extracted {
    /// Router kind (`image`, `pdf`, `sheet`, `calendar`, …).
    pub kind: &'static str,
    /// Finer class when known (`receipt`, `invoice`, `screenshot_error`, `rust`, …).
    pub subtype: Option<String>,
    /// A human title (page title, first heading, event summary, contact name).
    pub title: Option<String>,
    /// Everything readable, uncapped here; callers cap what the model sees.
    pub text: String,
    /// A short summary when the text is long (filled by the orchestration).
    pub summary: Option<String>,
    /// Typed data: tables, events, contacts, amounts, archive entries…
    pub structured: Value,
    /// What could not be read, said to the owner honestly.
    pub warnings: Vec<String>,
}

impl Extracted {
    pub fn new(kind: &'static str) -> Self {
        Self {
            kind,
            structured: Value::Null,
            ..Self::default()
        }
    }
}
