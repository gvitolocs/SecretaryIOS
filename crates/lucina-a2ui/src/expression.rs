//! The `formatString` template parser: a template string into the parts it means.
//!
//! A template is literal text with `${...}` interpolations in it. Each interpolation holds one
//! expression: a data binding (`/user/name` or `name`), a literal (`42`, `'two words'`, `true`),
//! or a call of a catalogue function with named arguments (`formatCurrency(value: /price,
//! currency: 'USD')`), which may nest. `\${` is the two characters `${` and nothing more.
//!
//! The behaviour is the original's `a2ui/core/expressions/expression_parser.py` measured by the
//! 92 `parse_expression_template` cases of `conformance/core/expressions.yaml`. The interfaces are
//! ours: the parts and expressions are named types instead of a list of strings and dictionaries,
//! and the scanner walks Unicode scalar values, which is what the original's Python strings and
//! the TypeScript `codePointAt` scanner both do.
//!
//! Only the parse is here. What a call evaluates to is the function work of a later step, and
//! evaluation is where the `@` system functions of v1.0 — `${@index(offset: 1)}` — are looked up;
//! the parser only reads such a name, and reads it as ASCII, as every engine of the original does.

use serde_json::{Number, Value};

use crate::error::Error;

/// The longest template the parser accepts, so that one string in a payload cannot make the parser
/// do unbounded work. The original's `MAX_EXPRESSION_TEMPLATE_LENGTH`, counted in characters as
/// Python counts them and not in UTF-16 units as TypeScript does.
pub const MAX_TEMPLATE_LENGTH: usize = 10_000;

/// The most parts one template may produce. The original's `MAX_EXPRESSION_PARTS`.
pub const MAX_PARTS: usize = 1_000;

/// How deeply interpolations and call arguments may nest. The original's
/// `ExpressionParser.MAX_DEPTH`, the same number in every engine, because an expression one engine
/// accepts and another rejects is two languages' different protocol.
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
    /// A literal: a string, a number or a boolean. The `null` keyword is the original's empty
    /// string, which renders nothing.
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
                let mut binding = serde_json::Map::new();
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
        let mut arguments = serde_json::Map::new();
        for (name, value) in &self.args {
            arguments.insert(name.clone(), value.to_json());
        }
        let mut call = serde_json::Map::new();
        call.insert("call".to_string(), Value::String(self.name.clone()));
        call.insert("args".into(), Value::Object(arguments));
        call.insert("returnType".to_string(), Value::String("any".to_string()));
        Value::Object(call)
    }
}

/// Parses `template` into its parts, literal text and interpolations in the order they are written.
///
/// Adjacent literal parts are left as they were read: what a template means is decided when its
/// parts are compared, where runs of text are joined, as the guide states, so that an
/// implementation that split a template differently still conforms.
///
/// # Errors
/// An `ErrorCategory::ExpressionError` for a template that does not parse, that passes
/// [`MAX_TEMPLATE_LENGTH`] or [`MAX_PARTS`], or that nests past [`MAX_DEPTH`].
pub fn parse_template(template: &str) -> Result<Vec<TemplatePart>, Error> {
    parse(template, 0)
}

/// The parts of a template. The original's `ExpressionParser.parse`.
fn parse(template: &str, depth: usize) -> Result<Vec<TemplatePart>, Error> {
    if depth > MAX_DEPTH {
        return Err(max_depth());
    }
    let length = template.chars().count();
    if length > MAX_TEMPLATE_LENGTH {
        return Err(Error::expression(format!(
            "Expression template length ({length}) exceeds maximum limit ({MAX_TEMPLATE_LENGTH})"
        )));
    }
    if length == 0 {
        return Ok(Vec::new());
    }
    if !template.contains("${") {
        return Ok(vec![TemplatePart::Literal(template.to_string())]);
    }

    let mut parts: Vec<TemplatePart> = Vec::new();
    let mut scanner = Scanner::new(template);
    while !scanner.is_at_end() {
        if parts.len() >= MAX_PARTS {
            return Err(Error::expression(format!(
                "Expression parts count exceeds maximum limit ({MAX_PARTS})"
            )));
        }
        if scanner.matches("${") {
            scanner.advance(2);
            let content = extract_interpolation_content(&mut scanner)?;
            let expression = parse_expression(&content, depth + 1)?;
            if !is_empty(&expression) {
                parts.push(TemplatePart::Expression(expression));
            }
        } else if scanner.peek(0) == '\\' && scanner.peek(1) == '$' && scanner.peek(2) == '{' {
            scanner.advance(1);
            parts.push(TemplatePart::Literal("${".to_string()));
            scanner.advance(2);
        } else {
            let start = scanner.pos;
            while !scanner.is_at_end() {
                if scanner.matches("${")
                    || (scanner.peek(0) == '\\' && scanner.peek(1) == '$' && scanner.peek(2) == '{')
                {
                    break;
                }
                scanner.advance(1);
            }
            let text = scanner.text(start, scanner.pos);
            if !text.is_empty() {
                parts.push(TemplatePart::Literal(text));
            }
        }
    }
    Ok(parts)
}

/// What an interpolation holds, up to and excluding its closing brace. The original's
/// `extract_interpolation_content`: braces inside a quoted string are not counted, and a quote
/// escaped with a backslash does not end the string.
fn extract_interpolation_content(scanner: &mut Scanner) -> Result<String, Error> {
    let start = scanner.pos;
    let mut balance = 1usize;
    while !scanner.is_at_end() && balance > 0 {
        match scanner.step() {
            '{' => balance += 1,
            '}' => balance -= 1,
            quote @ ('\'' | '"') => {
                while !scanner.is_at_end() {
                    let inside = scanner.step();
                    if inside == '\\' {
                        scanner.advance(1);
                    } else if inside == quote {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    if balance > 0 {
        return Err(Error::expression("Unclosed interpolation: missing '}'"));
    }
    Ok(scanner.text(start, scanner.pos.saturating_sub(1)))
}

/// One expression, with nothing left after it. The original's `parse_expression`.
fn parse_expression(expression: &str, depth: usize) -> Result<Expression, Error> {
    if depth > MAX_DEPTH {
        return Err(max_depth());
    }
    let trimmed = expression.trim();
    if trimmed.is_empty() {
        return Ok(empty());
    }
    let mut scanner = Scanner::new(trimmed);
    let parsed = parse_internal(&mut scanner, depth)?;
    if !scanner.is_at_end() {
        return Err(Error::expression(format!(
            "Unexpected characters at end of expression: '{}'",
            scanner.rest()
        )));
    }
    Ok(parsed)
}

/// The expression at the scanner, which is a literal, a binding, a call, or another interpolation.
/// The original's `_parse_expression_internal`.
fn parse_internal(scanner: &mut Scanner, depth: usize) -> Result<Expression, Error> {
    // Both recursive paths pass through here — an interpolation inside an interpolation and a call
    // argument that is itself an expression — so this one check counts both, as it does in the
    // original.
    if depth > MAX_DEPTH {
        return Err(max_depth());
    }
    scanner.skip_whitespace();
    if scanner.is_at_end() {
        return Ok(empty());
    }

    if scanner.matches("${") {
        scanner.advance(2);
        let content = extract_interpolation_content(scanner)?;
        return parse_expression(&content, depth + 1);
    }

    match scanner.peek(0) {
        '\'' | '"' => {
            return Ok(Expression::Literal(Value::String(parse_string_literal(
                scanner,
            )?)));
        }
        _ => {}
    }
    if is_number_start(scanner) {
        return parse_number_literal(scanner);
    }
    if scanner.matches_keyword("true") {
        return Ok(Expression::Literal(Value::Bool(true)));
    }
    if scanner.matches_keyword("false") {
        return Ok(Expression::Literal(Value::Bool(false)));
    }
    if scanner.matches_keyword("null") {
        return Ok(empty());
    }

    let token = scan_path_or_identifier(scanner)?;
    scanner.skip_whitespace();
    if scanner.peek(0) == '(' {
        return parse_function_call(token, scanner, depth);
    }
    if token.starts_with('@') {
        return Err(Error::expression(format!(
            "Expected '(' after function name '{token}'"
        )));
    }
    if token.is_empty() {
        return Ok(empty());
    }
    Ok(Expression::Path(token))
}

/// The token at the scanner: a path, or the name of a function or of a v1.0 system function.
/// The original's `scan_path_or_identifier`.
///
/// A path continues over `XID_Continue` characters and over `/`, `.` and `-`; an identifier
/// starting with `@` is ASCII-only, which is what makes `${@index()}` one name and `${@}` an error.
fn scan_path_or_identifier(scanner: &mut Scanner) -> Result<String, Error> {
    let start = scanner.pos;
    if scanner.peek(0) == '@' {
        if !is_start_of_system_function(scanner.peek(1)) {
            return Err(Error::expression(
                "Invalid identifier starting with '@' in expression",
            ));
        }
        scanner.advance(1);
        while !scanner.is_at_end() && is_part_of_system_function(scanner.peek(0)) {
            scanner.advance(1);
        }
        return Ok(scanner.text(start, scanner.pos));
    }
    while !scanner.is_at_end() {
        let c = scanner.peek(0);
        if is_identifier_continue(c) || c == '/' || c == '.' || c == '-' {
            scanner.advance(1);
        } else {
            break;
        }
    }
    Ok(scanner.text(start, scanner.pos))
}

/// A call and its named arguments. The original's `parse_function_call`.
fn parse_function_call(
    name: String,
    scanner: &mut Scanner,
    depth: usize,
) -> Result<Expression, Error> {
    scanner.match_char('(');
    scanner.skip_whitespace();

    let mut args: Vec<(String, Expression)> = Vec::new();
    while !scanner.is_at_end() && scanner.peek(0) != ')' {
        let arg_name = scan_identifier(scanner);
        scanner.skip_whitespace();
        if !scanner.match_char(':') {
            return Err(Error::expression(format!(
                "Expected ':' after argument name '{arg_name}' in function '{name}'"
            )));
        }
        scanner.skip_whitespace();
        let value = parse_internal(scanner, depth + 1)?;
        put_argument(&mut args, arg_name, value);
        scanner.skip_whitespace();
        if scanner.peek(0) == ',' {
            scanner.advance(1);
            scanner.skip_whitespace();
        }
    }

    if !scanner.match_char(')') {
        return Err(Error::expression(format!(
            "Expected ')' after function arguments for '{name}'"
        )));
    }
    Ok(Expression::Call(Call { name, args }))
}

/// A named argument, replacing an earlier one of the same name where it stands.
fn put_argument(args: &mut Vec<(String, Expression)>, name: String, value: Expression) {
    match args.iter().position(|(named, _)| *named == name) {
        Some(index) => args[index].1 = value,
        None => args.push((name, value)),
    }
}

/// An argument name: `XID_Continue` characters only, so `½` is not one of them and a positional
/// argument has no name to answer to.
fn scan_identifier(scanner: &mut Scanner) -> String {
    let start = scanner.pos;
    while !scanner.is_at_end() && is_identifier_continue(scanner.peek(0)) {
        scanner.advance(1);
    }
    scanner.text(start, scanner.pos)
}

/// A quoted string, with its backslash escapes resolved. The original's `parse_string_literal`:
/// `\n`, `\t` and `\r` are the control characters they name, and any other escaped character is
/// itself, so `\q` is `q`.
///
/// An unterminated quote is not an error here: the interpolation's brace counting already refused
/// the template, which is why the original returns what it read and the suite states no message.
fn parse_string_literal(scanner: &mut Scanner) -> Result<String, Error> {
    let quote = scanner.step();
    let mut result = String::new();
    while !scanner.is_at_end() {
        let c = scanner.step();
        if c == '\\' {
            if scanner.is_at_end() {
                break;
            }
            result.push(match scanner.step() {
                'n' => '\n',
                't' => '\t',
                'r' => '\r',
                other => other,
            });
        } else if c == quote {
            break;
        } else {
            result.push(c);
        }
    }
    Ok(result)
}

/// Whether the scanner is at a number literal: a digit, a `.` followed by a digit, or a sign
/// followed by either. The original's `is_number_start`.
///
/// The grammar has no arithmetic, so a sign here can only belong to a literal. A `-` or `.` inside
/// a path never reaches this check, because the path scanner has already consumed it: `a-1`, `a.5`
/// and `/items/-1` are paths, while `-a` and `.e5` are paths too because no digit follows.
fn is_number_start(scanner: &Scanner) -> bool {
    let offset = match scanner.peek(0) {
        '-' | '+' => 1,
        _ => 0,
    };
    is_digit(scanner.peek(offset))
        || (scanner.peek(offset) == '.' && is_digit(scanner.peek(offset + 1)))
}

/// A number literal, and what it is worth. The original's `parse_number_literal`.
///
/// An integer spelling becomes an integer; anything with a point or an exponent becomes a float,
/// and a float that is zero is written as positive zero, which is how the original's `-0` case
/// answers `0`. A value outside what a double holds is refused rather than quietly rounded.
fn parse_number_literal(scanner: &mut Scanner) -> Result<Expression, Error> {
    let start = scanner.pos;
    if matches!(scanner.peek(0), '-' | '+') {
        scanner.advance(1);
    }
    while !scanner.is_at_end() && (is_digit(scanner.peek(0)) || scanner.peek(0) == '.') {
        scanner.advance(1);
    }
    skip_exponent(scanner);
    let text = scanner.text(start, scanner.pos);
    if !is_number_literal(&text) {
        return Err(Error::expression(format!(
            "Invalid number literal: '{text}'"
        )));
    }
    let Ok(as_float) = text.parse::<f64>() else {
        return Err(out_of_range(&text));
    };
    if !as_float.is_finite() {
        return Err(out_of_range(&text));
    }
    let as_number = Number::from_f64(as_float).ok_or_else(|| out_of_range(&text))?;
    if text.contains('.') || text.contains('e') || text.contains('E') {
        // A literal worth zero is written as zero, sign and all: the original's TypeScript
        // answers `0` for `-0` and its Python never carries the sign, so a port that kept `-0.0`
        // would print `-0` where the protocol says `0`.
        if as_float == 0.0 {
            return Ok(Expression::Literal(Value::Number(
                Number::from_f64(0.0).ok_or_else(|| out_of_range(&text))?,
            )));
        }
        return Ok(Expression::Literal(Value::Number(as_number)));
    }
    // Within the integers a JSON number carries, exactly; past them, the double the original's
    // TypeScript engine would have held. Python keeps such an integer at any size and Swift to
    // 2^63-1, which is the port's one number type talking.
    let integer = text.parse::<i64>().ok().map(Number::from).or_else(|| {
        if text.starts_with('-') {
            None
        } else {
            text.parse::<u64>().ok().map(Number::from)
        }
    });
    Ok(Expression::Literal(Value::Number(
        integer.unwrap_or(as_number),
    )))
}

fn out_of_range(text: &str) -> Error {
    Error::expression(format!("Number literal is out of range: '{text}'"))
}

fn max_depth() -> Error {
    Error::expression("Max recursion depth reached in parse")
}

/// A literal that renders as nothing: the original's answer for `null` and for an interpolation
/// that held no expression at all.
fn empty() -> Expression {
    Expression::Literal(Value::String(String::new()))
}

fn is_empty(expression: &Expression) -> bool {
    matches!(expression, Expression::Literal(Value::String(text)) if text.is_empty())
}

/// Consumes an exponent suffix (`e` or `E`, an optional sign, then digits) if one is there. The
/// original's `skip_exponent`: a malformed exponent such as `1e` or `1e+` is consumed all the same,
/// so the literal is reported as invalid instead of leaving characters behind.
fn skip_exponent(scanner: &mut Scanner) {
    if scanner.is_at_end() || !matches!(scanner.peek(0), 'e' | 'E') {
        return;
    }
    scanner.advance(1);
    if !scanner.is_at_end() && matches!(scanner.peek(0), '+' | '-') {
        scanner.advance(1);
    }
    while !scanner.is_at_end() && is_digit(scanner.peek(0)) {
        scanner.advance(1);
    }
}

/// Whether `text` is a number literal of the grammar: an optional sign, a mantissa of `5`, `5.`,
/// `5.25` or `.5`, and an optional exponent.
///
/// The pattern is `_NUMBER_LITERAL` of the original's
/// `python/a2ui_core/src/a2ui/core/expressions/expression_parser.py:31-33`, spelled the same in
/// `NUMBER_LITERAL` of `typescript/core/web_core/src/expressions/expression_parser.ts:43`; it is
/// written out here as a scan rather than as a regular expression, because `src/` holds no
/// dependency on a regex engine. Every engine checks this same pattern, and they must agree: the
/// suite's `${1.}`, `${.5e2}` and `${1e}` are decided by it.
fn is_number_literal(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut at = usize::from(matches!(bytes.first(), Some(b'+') | Some(b'-')));

    if at < bytes.len() && bytes[at].is_ascii_digit() {
        while at < bytes.len() && bytes[at].is_ascii_digit() {
            at += 1;
        }
        if at < bytes.len() && bytes[at] == b'.' {
            at += 1;
            while at < bytes.len() && bytes[at].is_ascii_digit() {
                at += 1;
            }
        }
    } else if at < bytes.len() && bytes[at] == b'.' {
        at += 1;
        let digits = at;
        while at < bytes.len() && bytes[at].is_ascii_digit() {
            at += 1;
        }
        if at == digits {
            return false;
        }
    } else {
        return false;
    }

    if at < bytes.len() && matches!(bytes[at], b'e' | b'E') {
        at += 1;
        if at < bytes.len() && matches!(bytes[at], b'+' | b'-') {
            at += 1;
        }
        let digits = at;
        while at < bytes.len() && bytes[at].is_ascii_digit() {
            at += 1;
        }
        if at == digits {
            return false;
        }
    }
    at == bytes.len()
}

/// Whether `c` continues an identifier: Unicode UAX #31 `XID_Continue`, which is what the original
/// tests with `\p{XID_Continue}` in every engine, and which admits the combining marks and the
/// astral characters the suite names.
fn is_identifier_continue(c: char) -> bool {
    unicode_ident::is_xid_continue(c)
}

/// The first character after `@` of a system function name: ASCII, as `scanPathOrIdentifier` in
/// the original's TypeScript and `_SYSTEM_FUNCTION_START` in its Python both state.
fn is_start_of_system_function(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

/// A character of the rest of a system function name: `_SYSTEM_FUNCTION_CONTINUE` of the original.
fn is_part_of_system_function(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn is_digit(c: char) -> bool {
    c.is_ascii_digit()
}

/// The characters of a template, one at a time, with the original's `Scanner` positions: `pos` is a
/// count of characters, not of bytes, so a path of astral characters scans as the original scans
/// it.
struct Scanner {
    input: Vec<char>,
    pos: usize,
}

impl Scanner {
    fn new(text: &str) -> Scanner {
        Scanner {
            input: text.chars().collect(),
            pos: 0,
        }
    }

    fn is_at_end(&self) -> bool {
        self.pos >= self.input.len()
    }

    /// The character `offset` ahead, or `'\0'` past the end — the original's `peek` answers the
    /// empty string there, and no character of the grammar is one.
    fn peek(&self, offset: usize) -> char {
        self.input.get(self.pos + offset).copied().unwrap_or('\0')
    }

    /// Consumes `count` characters and answers what they were.
    fn advance(&mut self, count: usize) -> String {
        let start = self.pos;
        self.pos = (self.pos + count).min(self.input.len());
        self.text(start, self.pos)
    }

    /// Consumes one character and answers it.
    fn step(&mut self) -> char {
        let c = self.peek(0);
        self.pos = (self.pos + 1).min(self.input.len());
        c
    }

    /// Whether `expected` is what stands at the scanner.
    fn matches(&self, expected: &str) -> bool {
        expected
            .chars()
            .enumerate()
            .all(|(offset, c)| self.peek(offset) == c)
    }

    /// Consumes `keyword` when it stands at the scanner and the character after it is not one of
    /// an identifier, so that `trueish` is a path and `true` is the boolean.
    fn matches_keyword(&mut self, keyword: &str) -> bool {
        if !self.matches(keyword) {
            return false;
        }
        let length = keyword.chars().count();
        if is_identifier_continue(self.peek(length)) {
            return false;
        }
        self.advance(length);
        true
    }

    /// Consumes `expected` when it stands at the scanner, and answers whether it did.
    fn match_char(&mut self, expected: char) -> bool {
        if self.peek(0) == expected {
            self.advance(1);
            return true;
        }
        false
    }

    fn skip_whitespace(&mut self) {
        while !self.is_at_end() && self.peek(0).is_whitespace() {
            self.advance(1);
        }
    }

    /// The characters from `from` to `to`, as the original's slicing of its own string.
    fn text(&self, from: usize, to: usize) -> String {
        self.input[from..to].iter().collect()
    }

    /// What is left unconsumed, for the message that refuses it.
    fn rest(&self) -> String {
        self.text(self.pos, self.input.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts(template: &str) -> Vec<TemplatePart> {
        parse_template(template).unwrap_or_else(|error| panic!("refused {template}: {error}"))
    }

    /// `MAX_TEMPLATE_LENGTH` counts characters, which for a template of astral characters is not
    /// the byte count of the UTF-8 the payload arrives in, nor the UTF-16 unit count of the
    /// original's TypeScript.
    #[test]
    fn the_length_limit_counts_characters() {
        let exactly = format!("{}${{/a}}", "x".repeat(MAX_TEMPLATE_LENGTH - 5));
        assert_eq!(exactly.chars().count(), MAX_TEMPLATE_LENGTH);
        assert!(parse_template(&exactly).is_ok());
        let too_long = format!("{}${{/a}}", "x".repeat(MAX_TEMPLATE_LENGTH - 4));
        let error = parse_template(&too_long).expect_err("one character past the limit is refused");
        assert_eq!(error.category(), crate::ErrorCategory::ExpressionError);
        assert!(
            error.message().contains("exceeds maximum limit"),
            "{}",
            error.message()
        );
    }

    /// `-0` is zero: the original's Python answers the integer `0` and its TypeScript rewrites
    /// `-0` to `0`, and a port that kept the sign would print `-0` where the protocol says `0`.
    #[test]
    fn a_negative_zero_literal_is_zero() {
        assert_eq!(
            parts("${-0}"),
            vec![TemplatePart::Expression(Expression::Literal(json_int(0)))]
        );
        assert_eq!(
            parts("${-0.}"),
            vec![TemplatePart::Expression(Expression::Literal(json_float(
                0.0
            )))]
        );
    }

    /// The two spellings of one number: an integer literal is held as an integer and a literal
    /// with a point or an exponent as a float, which is what the original's one number type lets a
    /// port forget. The conformance cases cannot say it, because they compare `25000.0` with
    /// `25000` and both answers are the same value.
    #[test]
    fn an_integer_literal_is_not_a_float() {
        assert_eq!(
            parts("${42}"),
            vec![TemplatePart::Expression(Expression::Literal(json_int(42)))]
        );
        assert_eq!(
            parts("${42.0}"),
            vec![TemplatePart::Expression(Expression::Literal(json_float(
                42.0
            )))]
        );
        assert_eq!(
            parts("${1e2}"),
            vec![TemplatePart::Expression(Expression::Literal(json_float(
                100.0
            )))]
        );
    }

    fn json_int(value: i64) -> Value {
        Value::Number(Number::from(value))
    }

    fn json_float(value: f64) -> Value {
        Number::from_f64(value).map(Value::Number).expect("finite")
    }

    /// A system function name is ASCII, so `@` followed by anything else is refused rather than
    /// read as a path that starts with `@`.
    #[test]
    fn an_at_that_does_not_start_a_system_function_is_refused() {
        for template in ["${@}", "${@1x}", "${@ß}"] {
            let error = parse_template(template).expect_err("refused");
            assert!(
                error
                    .message()
                    .contains("Invalid identifier starting with '@'"),
                "{}: {}",
                template,
                error.message()
            );
        }
        assert_eq!(
            parts("${@index(offset: 1)}"),
            vec![TemplatePart::Expression(Expression::Call(Call {
                name: "@index".to_string(),
                args: vec![("offset".to_string(), Expression::Literal(json_int(1)))],
            }))]
        );
    }

    /// A call name written without its parentheses is the one shape of `@` the parser refuses
    /// after the name, and a bare `@` in a path position never reaches it.
    #[test]
    fn an_at_name_needs_its_parentheses() {
        let error =
            parse_template("${@index}").expect_err("a system function is called, not bound");
        assert!(
            error
                .message()
                .contains("Expected '(' after function name '@index'"),
            "{}",
            error.message()
        );
    }
}
