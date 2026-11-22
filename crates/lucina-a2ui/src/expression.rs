//! The `formatString` template parser: a template string into the parts it means.
//!
//! A template is literal text with `${...}` interpolations in it. Each interpolation holds one
//! expression: a data binding (`/user/name` or `name`), a literal (`42`, `'two words'`, `true`),
//! or a call of a catalogue function with named arguments (`formatCurrency(value: /price,
//! currency: 'USD')`), which may nest. `\${` is the two characters `${` and nothing more.
//!
//! The behaviour is the original's `a2ui/core/expressions/expression_parser.py` measured by the
//! 92 `parse_expression_template` cases of `conformance/core/expressions.yaml`. Only the parse is
//! here: what a call evaluates to is the function work of a later step.

use serde_json::{Map, Value};

use crate::error::Error;

/// The longest template the parser accepts, so that one string in a payload cannot make the parser
/// do unbounded work. The original's `MAX_EXPRESSION_TEMPLATE_LENGTH`.
pub const MAX_TEMPLATE_LENGTH: usize = 10_000;

/// The most parts one template may produce. The original's `MAX_EXPRESSION_PARTS`.
pub const MAX_PARTS: usize = 1_000;

/// How deeply interpolations and call arguments may nest. The original's
/// `ExpressionParser.MAX_DEPTH`, the same number in every engine of the original.
pub const MAX_DEPTH: usize = 100;

/// One part of a parsed template.
#[derive(Clone, Debug, PartialEq)]
pub enum TemplatePart {
    /// Text the template states literally.
    Literal(String),
    /// An interpolation, with what it holds.
    Expression(Expression),
}

impl TemplatePart {
    /// The part as the oracle writes it: a string for text, a binding or a call object for an
    /// interpolation.
    pub fn to_json(&self) -> Value {
        match self {
            TemplatePart::Literal(text) => Value::String(text.clone()),
            TemplatePart::Expression(expression) => expression.to_json(),
        }
    }
}

/// What one `${...}` interpolation holds.
#[derive(Clone, Debug, PartialEq)]
pub enum Expression {
    /// A literal. The `null` keyword is the original's empty string, which renders nothing.
    Literal(Value),
    /// A data binding, absolute when it starts with `/` and relative to the scope otherwise.
    Path(String),
    /// A call of a named function with named arguments.
    Call(Call),
}

impl Expression {
    /// The expression as the oracle writes it.
    pub fn to_json(&self) -> Value {
        match self {
            Expression::Literal(value) => value.clone(),
            Expression::Path(path) => {
                let mut binding = Map::new();
                binding.insert("path".to_string(), Value::String(path.clone()));
                Value::Object(binding)
            }
            Expression::Call(call) => call.to_json(),
        }
    }
}

/// A function call: the name, the arguments in the order the template wrote them, and the return
/// type the wire states, which the parser only ever fills in as `any`.
#[derive(Clone, Debug, PartialEq)]
pub struct Call {
    /// The function name, with its leading `@` when it is one of the system functions of v1.0.
    pub name: String,
    /// The named arguments. A name written twice keeps its place and takes the later value, as it
    /// does in the original.
    pub args: Vec<(String, Expression)>,
}

impl Call {
    /// The call as the oracle writes it: `{call, args, returnType}`.
    pub fn to_json(&self) -> Value {
        let mut arguments = Map::new();
        for (name, value) in &self.args {
            arguments.insert(name.clone(), value.to_json());
        }
        let mut call = Map::new();
        call.insert("call".to_string(), Value::String(self.name.clone()));
        call.insert("args".into(), Value::Object(arguments));
        call.insert("returnType".to_string(), Value::String("any".to_string()));
        Value::Object(call)
    }
}

/// Parses `template` into its parts, literal text and interpolations in the order they are written.
///
/// Adjacent literal parts are left as they were read: what a template means is decided when the
/// parts are compared, where runs of text are joined, as the guide states.
///
/// # Errors
/// An `ErrorCategory::ExpressionError` for a template that does not parse or that passes a limit.
pub fn parse_template(template: &str) -> Result<Vec<TemplatePart>, Error> {
    let _ = template;
    Err(Error::state("not implemented in this commit"))
}
