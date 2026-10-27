//! Google Takeout → Lucina Keep importer.
//!
//! [`takeout`] reads one or more Takeout zips from disk (entries are
//! streamed one at a time, the zips are never loaded whole) and parses the
//! Keep JSON files into [`ParsedNote`]s; [`html`] turns `textContentHtml`
//! into markdown basics; [`report`] is the [`ImportReport`] the CLI and the
//! endpoint answer with. Nothing here touches the note store.

pub mod html;
pub mod report;
pub mod takeout;

pub use report::{
    AttachmentCounts, FileCounts, ImportReport, LabelCounts, NoteCounts, ReportError, SampleNote,
};
pub use takeout::{
    decide_mime, normalize_label, parse_note_json, sniff_mime, AttachmentRef, KeepAnnotation,
    ParsedNote, Scan, Takeout, ALLOWED_MIMES, MAX_ATTACHMENTS_PER_NOTE, MAX_ATTACHMENT_BYTES,
};
