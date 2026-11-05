//! Open, show and edit existing `.xlsx` workbooks without losing anything (docs/xlsx.md).
//!
//! - [`read`]: sheets, cells, formulas, cached results, styles, merges, validations, protection.
//! - [`view::workbook_json`]: what the apps show, with input cells marked and, for the AU
//!   worktime template, a form and its totals ([`timesheet`]).
//! - [`patch::apply`]: write edits back. Only the edited `<c>` elements change; every other part
//!   is copied byte for byte from the original package. Formula cells stay formulas and
//!   `fullCalcOnLoad` makes Excel recompute their results when it opens the file.

pub mod cellref;
pub mod numfmt;
pub mod package;
pub mod patch;
pub mod read;
pub mod timesheet;
pub mod view;
pub mod xml;

pub use patch::{apply, Changed, Edit, EditValue, Options, Outcome};
pub use read::{read, Value, Workbook};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not an xlsx workbook: {0}")]
    NotXlsx(String),
    #[error("unsupported workbook: {0}")]
    Unsupported(String),
    #[error("{part}: {msg}")]
    Xml { part: String, msg: String },
    #[error("no sheet named {0:?}")]
    UnknownSheet(String),
    #[error("{0:?} is not a cell reference")]
    BadCell(String),
    #[error("{sheet}!{cell} holds a formula")]
    FormulaCell { sheet: String, cell: String },
    #[error("{sheet}!{cell} is locked (the sheet is protected)")]
    LockedCell { sheet: String, cell: String },
    #[error("{sheet}!{cell}: {reason}")]
    Invalid {
        sheet: String,
        cell: String,
        reason: String,
    },
}

impl Error {
    /// A stable code for API clients.
    pub fn code(&self) -> &'static str {
        match self {
            Error::NotXlsx(_) => "not_xlsx",
            Error::Unsupported(_) => "unsupported",
            Error::Xml { .. } => "bad_xml",
            Error::UnknownSheet(_) => "unknown_sheet",
            Error::BadCell(_) => "bad_cell",
            Error::FormulaCell { .. } => "formula_cell",
            Error::LockedCell { .. } => "locked_cell",
            Error::Invalid { .. } => "invalid_value",
        }
    }

    /// Whether the edit request was at fault (400), not the file (422).
    pub fn is_request_error(&self) -> bool {
        !matches!(
            self,
            Error::NotXlsx(_) | Error::Unsupported(_) | Error::Xml { .. }
        )
    }

    pub(crate) fn xml(part: &str) -> impl Fn(xml::XmlError) -> Error + '_ {
        move |e| Error::Xml {
            part: part.to_string(),
            msg: e.to_string(),
        }
    }
}
