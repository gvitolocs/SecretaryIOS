//! The one error type of the port: a category and a message, never a panic.
//!
//! The categories are exactly the nine names `ExpectError` admits in
//! `conformance/conformance_schema.json` of the original, and the relations between them are the
//! ones the guide's table "Mapping error categories" states: an integrity or a recursion failure
//! also satisfies a `ValidationError` expectation, and a template that does not parse answers both
//! a `ParseError` and an `ExpressionError` expectation. See [`ErrorCategory::satisfies`].

use std::fmt;

/// The category of an [`Error`], named as `ExpectError` names it.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ErrorCategory {
    /// Malformed JSON, invalid syntax of a model notation, or an unterminated block.
    ParseError,
    /// Valid syntax naming an unknown component, an invalid property, a duplicate id, a missing
    /// root or a cycle.
    ValidationError,
    /// A surface lifecycle fault: a duplicate `createSurface`, an update to an unknown surface,
    /// a dangling reference.
    IntegrityError,
    /// A component graph cycle, or nesting past a depth limit.
    RecursionError,
    /// A missing catalogue, a conflicting catalogue id or protocol version, a failed capability
    /// negotiation.
    CatalogError,
    /// An invalid JSON Pointer, a forbidden path segment, a write the data model refuses.
    DataError,
    /// A template that does not parse, or one past the length and part limits.
    ExpressionError,
    /// An operation on a disposed or otherwise unusable object.
    StateError,
    /// A general compilation failure, not narrowed to syntax or schema checks.
    CompilationError,
}

impl ErrorCategory {
    /// The name the conformance suites write.
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCategory::ParseError => "ParseError",
            ErrorCategory::ValidationError => "ValidationError",
            ErrorCategory::IntegrityError => "IntegrityError",
            ErrorCategory::RecursionError => "RecursionError",
            ErrorCategory::CatalogError => "CatalogError",
            ErrorCategory::DataError => "DataError",
            ErrorCategory::ExpressionError => "ExpressionError",
            ErrorCategory::StateError => "StateError",
            ErrorCategory::CompilationError => "CompilationError",
        }
    }

    /// The category a suite names, or `None` when the suite names no category we have.
    pub fn from_name(name: &str) -> Option<ErrorCategory> {
        [
            ErrorCategory::ParseError,
            ErrorCategory::ValidationError,
            ErrorCategory::IntegrityError,
            ErrorCategory::RecursionError,
            ErrorCategory::CatalogError,
            ErrorCategory::DataError,
            ErrorCategory::ExpressionError,
            ErrorCategory::StateError,
            ErrorCategory::CompilationError,
        ]
        .into_iter()
        .find(|category| category.as_str() == name)
    }

    /// Whether an error of this category satisfies an expectation of `expected`.
    ///
    /// The two widenings are the original's, not a licence to pass a wrong error:
    ///
    /// - `IntegrityError` and `RecursionError` are subclasses of `ValidationError` in the original
    ///   hierarchy (`a2ui/core/exceptions.py`, `A2uiIntegrityError` and `A2uiRecursionError` both
    ///   extend `A2uiValidationError`), so a case expecting `ValidationError` accepts either.
    /// - A template that does not parse raises `A2uiExpressionError`, and the guide's
    ///   `parse_expression_template` rule states "`ParseError` maps to `A2uiExpressionError`",
    ///   which the Python harness spells out as `CATEGORY_TO_EXCEPTION["ParseError"] =
    ///   (A2uiParseError, A2uiExpressionError)`. So `ExpressionError` satisfies `ParseError`.
    ///   The way round is not true: `A2uiParseError` never satisfies an `ExpressionError`
    ///   expectation.
    ///
    /// Everything else matches its own name only. Accepting the base error for every category
    /// would make the expectation vacuous, which the guide forbids.
    pub fn satisfies(self, expected: ErrorCategory) -> bool {
        use ErrorCategory::{
            ExpressionError, IntegrityError, ParseError, RecursionError, ValidationError,
        };
        match (self, expected) {
            (found, wanted) if found == wanted => true,
            (IntegrityError | RecursionError, ValidationError) => true,
            (ExpressionError, ParseError) => true,
            _ => false,
        }
    }
}

impl fmt::Display for ErrorCategory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A failure of the A2UI core: what category it is, and what happened in the original's words.
///
/// The message is what a case's `expectError.message` regular expression is matched against, so
/// the wording follows the original where a case depends on it.
#[derive(Clone, Debug, PartialEq)]
pub struct Error {
    category: ErrorCategory,
    message: String,
}

impl Error {
    /// A failure of `category` whose message is `message`.
    pub fn new(category: ErrorCategory, message: impl Into<String>) -> Error {
        Error {
            category,
            message: message.into(),
        }
    }

    /// A [`ErrorCategory::DataError`]: a pointer that does not parse, a forbidden segment, or a
    /// write the data model refuses.
    pub fn data(message: impl Into<String>) -> Error {
        Error::new(ErrorCategory::DataError, message)
    }

    /// A [`ErrorCategory::ExpressionError`]: a template that does not parse or that passes a limit.
    pub fn expression(message: impl Into<String>) -> Error {
        Error::new(ErrorCategory::ExpressionError, message)
    }

    /// A [`ErrorCategory::StateError`]: an operation on a disposed or unusable object.
    pub fn state(message: impl Into<String>) -> Error {
        Error::new(ErrorCategory::StateError, message)
    }

    /// The category of the failure.
    pub fn category(&self) -> ErrorCategory {
        self.category
    }

    /// The message of the failure, as the original words it.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

/// The category relations of the original's error hierarchy are not reachable from the three
/// suites of this step: no case of them expects a category a raised error only satisfies through a
/// widening. So they are pinned here rather than by the oracle.
#[cfg(test)]
mod tests {
    use super::ErrorCategory::*;
    use super::*;

    #[test]
    fn every_category_name_round_trips() {
        for category in [
            ParseError,
            ValidationError,
            IntegrityError,
            RecursionError,
            CatalogError,
            DataError,
            ExpressionError,
            StateError,
            CompilationError,
        ] {
            assert_eq!(ErrorCategory::from_name(category.as_str()), Some(category));
        }
        assert_eq!(ErrorCategory::from_name("Parse"), None);
        assert_eq!(ErrorCategory::from_name(""), None);
    }

    #[test]
    fn an_integrity_or_recursion_failure_satisfies_validation() {
        assert!(IntegrityError.satisfies(ValidationError));
        assert!(RecursionError.satisfies(ValidationError));
        assert!(!IntegrityError.satisfies(RecursionError));
        assert!(!ValidationError.satisfies(IntegrityError));
    }

    #[test]
    fn a_template_that_does_not_parse_satisfies_a_parse_expectation_only_away() {
        assert!(ExpressionError.satisfies(ParseError));
        assert!(!ParseError.satisfies(ExpressionError));
        assert!(!DataError.satisfies(ParseError));
    }

    #[test]
    fn the_message_is_what_display_prints() {
        let error = Error::data("Forbidden path segment '__proto__' in path '/__proto__/x'.");
        assert_eq!(
            error.to_string(),
            "Forbidden path segment '__proto__' in path '/__proto__/x'."
        );
        assert_eq!(error.category(), DataError);
    }
}
