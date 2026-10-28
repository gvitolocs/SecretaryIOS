//! `calculator`: a safe arithmetic parser (recursive descent, no eval).
//!
//! `+ - * / ^ %` (postfix percent = /100), parentheses, unary minus, `×`,
//! `÷`, `x` between numbers, `of`/`di` as multiplication ("17% of 230"),
//! decimal comma, constants `pi`, `e`, functions `sqrt abs round floor ceil
//! ln log log2 exp sin cos tan`.
//!
//! Arithmetic is exact (arbitrary-precision rationals), so money, per-kg
//! prices and averages come out right. `pi`, `e`, the transcendental
//! functions, irrational square roots and non-integer powers fall back to
//! f64, and from there on the result is marked inexact.

use num_bigint::BigInt;
use num_rational::BigRational;
use num_traits::{One, Signed, ToPrimitive, Zero};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{schema_for, Ctx, SideEffect, Tool, ToolError, ToolResult};

/// Largest numerator or denominator an exact value may have.
const MAX_BITS: u64 = 4096;
/// Largest integer exponent magnitude for exact powers.
const MAX_EXP: u64 = 10_000;

/// A computed value: exact while only exact operations were applied.
#[derive(Debug, Clone, PartialEq)]
pub enum Number {
    Exact(BigRational),
    Approx(f64),
}

impl Number {
    pub fn to_f64(&self) -> f64 {
        match self {
            Number::Exact(r) => r.to_f64().unwrap_or(f64::NAN),
            Number::Approx(v) => *v,
        }
    }
    pub fn is_exact(&self) -> bool {
        matches!(self, Number::Exact(_))
    }
}

fn check_size(r: BigRational) -> Result<BigRational, String> {
    if r.numer().bits() > MAX_BITS || r.denom().bits() > MAX_BITS {
        return Err(format!(
            "number too large for exact arithmetic (over {MAX_BITS} bits)"
        ));
    }
    Ok(r)
}

fn checked(r: BigRational) -> Result<Number, String> {
    check_size(r).map(Number::Exact)
}

/// `b^n` for an integer `n`, refusing results beyond the size limits.
fn pow_exact(b: &BigRational, n: i64) -> Result<BigRational, String> {
    let e = n.unsigned_abs();
    if e > MAX_EXP {
        return Err(format!("exponent too large (max {MAX_EXP})"));
    }
    if b.is_zero() {
        return match n {
            0 => Ok(BigRational::one()),
            n if n < 0 => Err("division by zero".into()),
            _ => Ok(BigRational::zero()),
        };
    }
    for part in [b.numer(), b.denom()] {
        let bits = part.bits();
        if bits > 1 && (bits - 1) * e + 1 > MAX_BITS {
            return Err(format!(
                "number too large for exact arithmetic (over {MAX_BITS} bits)"
            ));
        }
    }
    let (num, den) = (b.numer().pow(e as u32), b.denom().pow(e as u32));
    check_size(if n < 0 {
        BigRational::new(den, num)
    } else {
        BigRational::new(num, den)
    })
}

fn binop(a: Number, b: Number, op: char) -> Result<Number, String> {
    match (a, b) {
        (Number::Exact(x), Number::Exact(y)) => checked(match op {
            '+' => x + y,
            '-' => x - y,
            '*' => x * y,
            _ => {
                if y.is_zero() {
                    return Err("division by zero".into());
                }
                x / y
            }
        }),
        (a, b) => {
            let (x, y) = (a.to_f64(), b.to_f64());
            Ok(Number::Approx(match op {
                '+' => x + y,
                '-' => x - y,
                '*' => x * y,
                _ => {
                    if y == 0.0 {
                        return Err("division by zero".into());
                    }
                    x / y
                }
            }))
        }
    }
}

fn power(base: Number, exp: Number) -> Result<Number, String> {
    let exponent = exp.to_f64();
    if exponent.fract() == 0.0 && exponent.abs() > MAX_EXP as f64 {
        return Err(format!("exponent too large (max {MAX_EXP})"));
    }
    let zero_base = match &base {
        Number::Exact(r) => r.is_zero(),
        Number::Approx(v) => *v == 0.0,
    };
    let negative_exponent = match &exp {
        Number::Exact(r) => r.is_negative(),
        Number::Approx(v) => *v < 0.0,
    };
    if zero_base && negative_exponent {
        return Err("division by zero".into());
    }
    match (&base, &exp) {
        (Number::Exact(b), Number::Exact(e)) if e.is_integer() => {
            let n = e
                .numer()
                .to_i64()
                .ok_or_else(|| format!("exponent too large (max {MAX_EXP})"))?;
            checked(pow_exact(b, n)?)
        }
        _ => Ok(Number::Approx(base.to_f64().powf(exp.to_f64()))),
    }
}

fn sqrt(arg: Number) -> Result<Number, String> {
    if match &arg {
        Number::Exact(r) => r.is_negative(),
        Number::Approx(v) => *v < 0.0,
    } {
        return Err("sqrt of a negative number".into());
    }
    if let Number::Exact(r) = &arg {
        let (n, d) = (r.numer().sqrt(), r.denom().sqrt());
        if &(&n * &n) == r.numer() && &(&d * &d) == r.denom() {
            return Ok(Number::Exact(BigRational::new(n, d)));
        }
    }
    Ok(Number::Approx(arg.to_f64().sqrt()))
}

/// Splits a normalized literal ("1000.5", ".5", "1.2e-3") into
/// `digits * 10^scale`.
fn parse_decimal(s: &str) -> Option<(BigInt, i64)> {
    let (mant, exp) = match s.find(['e', 'E']) {
        Some(i) => (&s[..i], s[i + 1..].parse::<i64>().ok()?),
        None => (s, 0),
    };
    let (int, frac) = mant.split_once('.').unwrap_or((mant, ""));
    if !int.chars().chain(frac.chars()).all(|c| c.is_ascii_digit()) {
        return None;
    }
    let digits: BigInt = format!("{int}{frac}").parse().ok()?;
    Some((digits, exp.checked_sub(frac.len() as i64)?))
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(BigRational),
    Ident(String),
    Op(char),
    LParen,
    RParen,
}

fn tokenize(src: &str) -> Result<Vec<Tok>, String> {
    let chars: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c.is_ascii_digit() || (c == '.' && chars.get(i + 1).is_some_and(|d| d.is_ascii_digit()))
        {
            let start = i;
            while i < chars.len()
                && (chars[i].is_ascii_digit()
                    || chars[i] == '.'
                    || (chars[i] == ',' && chars.get(i + 1).is_some_and(|d| d.is_ascii_digit()))
                    || chars[i] == '_')
            {
                i += 1;
            }
            let mut text: String = chars[start..i].iter().filter(|c| **c != '_').collect();
            // exponent
            if i < chars.len() && (chars[i] == 'e' || chars[i] == 'E') {
                let mut j = i + 1;
                if j < chars.len() && (chars[j] == '+' || chars[j] == '-') {
                    j += 1;
                }
                if j < chars.len() && chars[j].is_ascii_digit() {
                    while j < chars.len() && chars[j].is_ascii_digit() {
                        j += 1;
                    }
                    text.push_str(&chars[i..j].iter().collect::<String>());
                    i = j;
                }
            }
            let normalized = if text.contains(',') && !text.contains('.') {
                text.replace(',', ".")
            } else {
                text.replace(',', "")
            };
            let (digits, scale) =
                parse_decimal(&normalized).ok_or_else(|| format!("bad number {text:?}"))?;
            let ten = BigRational::from_integer(BigInt::from(10));
            let n = pow_exact(&ten, scale)? * BigRational::from_integer(digits);
            out.push(Tok::Num(check_size(n)?));
            continue;
        }
        if c.is_alphabetic() {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect::<String>().to_lowercase();
            match word.as_str() {
                "x" => out.push(Tok::Op('*')),
                "of" | "di" | "del" | "della" | "times" | "per" => out.push(Tok::Op('*')),
                "plus" | "più" => out.push(Tok::Op('+')),
                "minus" | "meno" => out.push(Tok::Op('-')),
                _ => out.push(Tok::Ident(word)),
            }
            continue;
        }
        match c {
            '+' | '-' | '*' | '/' | '^' | '%' => out.push(Tok::Op(c)),
            '×' | '·' => out.push(Tok::Op('*')),
            '÷' | ':' => out.push(Tok::Op('/')),
            '−' => out.push(Tok::Op('-')),
            '(' | '[' => out.push(Tok::LParen),
            ')' | ']' => out.push(Tok::RParen),
            '=' | '?' => {}
            _ => return Err(format!("unexpected character {c:?}")),
        }
        i += 1;
    }
    Ok(out)
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
    depth: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }
    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        self.pos += 1;
        t
    }
    fn nested(&mut self, parse: fn(&mut Self) -> Result<Number, String>) -> Result<Number, String> {
        self.depth += 1;
        if self.depth > 64 {
            return Err("expression too deep".into());
        }
        let result = parse(self);
        self.depth -= 1;
        result
    }
    // expr := term (('+'|'-') term)*
    fn expr(&mut self) -> Result<Number, String> {
        let mut v = self.term()?;
        while let Some(Tok::Op(op @ ('+' | '-'))) = self.peek().cloned() {
            self.pos += 1;
            let rhs = self.term()?;
            v = binop(v, rhs, op)?;
        }
        Ok(v)
    }
    // term := power (('*'|'/') power | implicit power)*
    fn term(&mut self) -> Result<Number, String> {
        let mut v = self.unary()?;
        loop {
            match self.peek().cloned() {
                Some(Tok::Op(op @ ('*' | '/'))) => {
                    self.pos += 1;
                    let rhs = self.unary()?;
                    v = binop(v, rhs, op)?;
                }
                // implicit multiplication: 2(3+4), 2pi
                Some(Tok::LParen) | Some(Tok::Ident(_)) => {
                    let rhs = self.unary()?;
                    v = binop(v, rhs, '*')?;
                }
                _ => return Ok(v),
            }
        }
    }
    // unary := ('-'|'+') unary | power
    fn unary(&mut self) -> Result<Number, String> {
        match self.peek() {
            Some(Tok::Op('-')) => {
                self.pos += 1;
                Ok(match self.nested(Self::unary)? {
                    Number::Exact(r) => Number::Exact(-r),
                    Number::Approx(v) => Number::Approx(-v),
                })
            }
            Some(Tok::Op('+')) => {
                self.pos += 1;
                self.nested(Self::unary)
            }
            _ => self.power(),
        }
    }
    // power := postfix ('^' unary)?
    fn power(&mut self) -> Result<Number, String> {
        let base = self.postfix()?;
        if let Some(Tok::Op('^')) = self.peek() {
            self.pos += 1;
            let exp = self.nested(Self::unary)?;
            return power(base, exp);
        }
        Ok(base)
    }
    // postfix := atom '%'*
    fn postfix(&mut self) -> Result<Number, String> {
        let mut v = self.atom()?;
        while let Some(Tok::Op('%')) = self.peek() {
            self.pos += 1;
            v = binop(v, Number::Exact(BigRational::from_integer(100.into())), '/')?;
        }
        Ok(v)
    }
    fn atom(&mut self) -> Result<Number, String> {
        match self.next() {
            Some(Tok::Num(n)) => Ok(Number::Exact(n)),
            Some(Tok::LParen) => {
                let v = self.nested(Self::expr)?;
                match self.next() {
                    Some(Tok::RParen) => Ok(v),
                    _ => Err("missing ')'".into()),
                }
            }
            Some(Tok::Ident(name)) => match name.as_str() {
                "pi" | "π" => Ok(Number::Approx(std::f64::consts::PI)),
                "e" => Ok(Number::Approx(std::f64::consts::E)),
                f => {
                    let arg = match self.peek() {
                        Some(Tok::LParen) => self.atom()?,
                        _ => self.nested(Self::postfix)?,
                    };
                    let exact = match (&arg, f) {
                        (_, "sqrt" | "radice") => return sqrt(arg),
                        (Number::Exact(r), "abs") => Some(r.abs()),
                        (Number::Exact(r), "round") => Some(r.round()),
                        (Number::Exact(r), "floor") => Some(r.floor()),
                        (Number::Exact(r), "ceil") => Some(r.ceil()),
                        _ => None,
                    };
                    if let Some(r) = exact {
                        return checked(r);
                    }
                    let arg = arg.to_f64();
                    Ok(Number::Approx(match f {
                        "abs" => arg.abs(),
                        "round" => arg.round(),
                        "floor" => arg.floor(),
                        "ceil" => arg.ceil(),
                        "ln" => arg.ln(),
                        "log" | "log10" => arg.log10(),
                        "log2" => arg.log2(),
                        "exp" => arg.exp(),
                        "sin" => arg.sin(),
                        "cos" => arg.cos(),
                        "tan" => arg.tan(),
                        _ => return Err(format!("unknown function or name {f:?}")),
                    }))
                }
            },
            Some(t) => Err(format!("unexpected {t:?}")),
            None => Err("unexpected end of expression".into()),
        }
    }
}

/// Evaluate an expression.
pub fn evaluate(src: &str) -> Result<Number, String> {
    if src.chars().count() > 500 {
        return Err("expression too long".into());
    }
    let toks = tokenize(src)?;
    if toks.is_empty() {
        return Err("empty expression".into());
    }
    let mut p = Parser {
        toks,
        pos: 0,
        depth: 0,
    };
    let v = p.expr()?;
    if p.pos < p.toks.len() {
        return Err(format!("unexpected {:?}", p.toks[p.pos]));
    }
    if !v.to_f64().is_finite() {
        return Err("result is not a finite number".into());
    }
    Ok(v)
}

/// Up to 10 decimals, no trailing zeros.
pub fn format_number(v: f64) -> String {
    let s = format!("{:.10}", v);
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" {
        "0".into()
    } else {
        s.to_owned()
    }
}

/// `r` rounded half away from zero to exactly `places` decimals.
fn round_decimal(r: &BigRational, places: u32) -> String {
    let scale = BigInt::from(10).pow(places);
    let n = (r * BigRational::from_integer(scale.clone()))
        .round()
        .to_integer();
    let sign = if n.is_negative() { "-" } else { "" };
    let (int, frac) = (n.abs() / &scale, n.abs() % &scale);
    if places == 0 {
        format!("{sign}{int}")
    } else {
        format!("{sign}{int}.{frac:0>width$}", width = places as usize)
    }
}

/// Decimal places of `r` if its decimal expansion terminates.
fn terminating_places(r: &BigRational) -> Option<u32> {
    let mut d = r.denom().clone();
    let (two, five) = (BigInt::from(2), BigInt::from(5));
    let (mut twos, mut fives) = (0u32, 0u32);
    while (&d % &two).is_zero() {
        d /= &two;
        twos += 1;
    }
    while (&d % &five).is_zero() {
        d /= &five;
        fives += 1;
    }
    d.is_one().then_some(twos.max(fives))
}

/// The exact decimal when it terminates, else 10 decimals with no trailing
/// zeros (like [`format_number`]).
fn format_exact(r: &BigRational) -> String {
    let s = round_decimal(r, terminating_places(r).unwrap_or(10));
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_owned()
    } else {
        s
    }
}

fn output(expression: &str, v: &Number) -> Value {
    let mut out = json!({"expression": expression, "result": v.to_f64(), "exact": v.is_exact()});
    let exact = match v {
        Number::Exact(r) => {
            if terminating_places(r).is_none() {
                out["fraction"] = json!(format!("{}/{}", r.numer(), r.denom()));
            }
            out["formatted"] = json!(format_exact(r));
            r.clone()
        }
        Number::Approx(f) => {
            out["formatted"] = json!(format_number(*f));
            BigRational::from_float(*f).unwrap_or_default()
        }
    };
    out["rounded_2"] = json!(round_decimal(&exact, 2));
    out
}

#[derive(Deserialize, JsonSchema)]
struct Args {
    /// Arithmetic expression, e.g. "17% * 230" or "(1200 - 350) / 3".
    expression: String,
}

pub struct CalculatorTool;

#[async_trait::async_trait]
impl Tool for CalculatorTool {
    fn name(&self) -> &'static str {
        "calculator"
    }
    fn description(&self) -> &'static str {
        "Exact arithmetic (no floating-point error) for money, prices per kg, averages: + - * / ^, percent (17% * 230), parentheses, sqrt/abs/round/ln/log/sin/cos/tan, pi, e. Returns `formatted`, `rounded_2` (2 decimals, for money), `exact` (false once pi/e/ln/sin/irrational roots are involved) and `fraction` for repeating decimals (1/3). Use it for any calculation instead of mental math."
    }
    fn parameters(&self) -> Value {
        schema_for::<Args>()
    }
    fn side_effect(&self) -> SideEffect {
        SideEffect::None
    }
    async fn call(&self, args: Value, _ctx: &Ctx) -> ToolResult {
        let a: Args = super::args(args)?;
        let v = evaluate(&a.expression).map_err(ToolError::new)?;
        Ok(output(&a.expression, &v))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn out(src: &str) -> Value {
        output(src, &evaluate(src).unwrap_or_else(|e| panic!("{src}: {e}")))
    }

    #[test]
    fn evaluates() {
        let cases = [
            ("17% * 230", 39.1),
            ("17% of 230", 39.1),
            ("17 % di 230", 39.1),
            ("2 + 3 * 4", 14.0),
            ("(2 + 3) * 4", 20.0),
            ("2^3^2", 512.0),
            ("-2^2", -4.0),
            ("10 / 4", 2.5),
            ("3,5 x 2", 7.0),
            ("1,000.5 + 1", 1001.5),
            ("sqrt(16) + abs(-2)", 6.0),
            ("2pi", 2.0 * std::f64::consts::PI),
            ("1e3 / 8", 125.0),
            ("50%", 0.5),
            ("12 × 3 ÷ 4", 9.0),
        ];
        for (src, want) in cases {
            let got = evaluate(src)
                .unwrap_or_else(|e| panic!("{src}: {e}"))
                .to_f64();
            assert!((got - want).abs() < 1e-9, "{src}: {got} != {want}");
        }
        assert_eq!(out("17% * 230")["formatted"], "39.1");
        assert_eq!(out("1/3")["formatted"], "0.3333333333");
        for bad in [
            "1/0", "2 +", "(1", "rm -rf", "sqrt(-1)", "", "foo(2)", "2 ** ", "1e999*10", "0^-1",
            "2^100000", "1e99999",
        ] {
            assert!(evaluate(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn exact_results() {
        let v = out("0.1 + 0.2");
        assert_eq!(v["formatted"], "0.3");
        assert_eq!(v["exact"], true);
        assert!(v.get("fraction").is_none());

        assert_eq!(out("19.95 / 0.75")["formatted"], "26.6");
        assert_eq!(out("19.95 / 0.75")["rounded_2"], "26.60");

        let avg = out("(72.5 + 80 + 77.25) / 3");
        assert_eq!(avg["formatted"], "76.5833333333");
        assert_eq!(avg["fraction"], "919/12");
        assert_eq!(out("1/3")["fraction"], "1/3");
        assert_eq!(out("-1/3")["formatted"], "-0.3333333333");

        let pct = out("10% of 249.95");
        assert_eq!(pct["formatted"], "24.995");
        assert_eq!(pct["rounded_2"], "25.00");
        assert_eq!(out("2.675")["rounded_2"], "2.68");
        assert_eq!(out("-2.675")["rounded_2"], "-2.68");
        assert_eq!(out("round(2.5) + round(-2.5)")["formatted"], "0");

        assert_eq!(out("2^-2")["formatted"], "0.25");
        assert_eq!(out("2^-2")["exact"], true);
        assert_eq!(out("2^0.5")["exact"], false);
        let root = out("sqrt(2.25)");
        assert_eq!(root["formatted"], "1.5");
        assert_eq!(root["exact"], true);
        assert_eq!(out("sqrt(2)")["exact"], false);
        assert_eq!(out("2pi")["exact"], false);
        assert!(out("2pi").get("fraction").is_none());
    }

    #[test]
    fn exact_literals_and_functions() {
        for (src, want) in [
            ("3,5", "3.5"),
            ("1,000.5", "1000.5"),
            ("1_000.5", "1000.5"),
            ("1.25e-2", "0.0125"),
            (".5 + 1.", "1.5"),
            ("2(3 + 4)", "14"),
            ("12 : 4", "3"),
            ("sqrt(16)", "4"),
            ("sqrt(1/16)", "0.25"),
            ("abs(-1/3)", "0.3333333333"),
            ("round(2.5)", "3"),
            ("round(-2.5)", "-3"),
            ("floor(-1.2)", "-2"),
            ("ceil(-1.2)", "-1"),
            ("1/100000000000", "0.00000000001"),
            ("-1/300000000000", "0"),
        ] {
            let v = out(src);
            assert_eq!(v["formatted"], want, "{src}");
            assert_eq!(v["exact"], true, "{src}");
            assert!(v["result"].is_number(), "{src}");
        }
        for src in [
            "pi",
            "e",
            "ln(1)",
            "log(10)",
            "log2(2)",
            "exp(0)",
            "sin(0)",
            "cos(0)",
            "tan(0)",
            "abs(pi)",
            "round(pi)",
            "floor(pi)",
            "ceil(pi)",
            "sqrt(pi-pi)",
            "0 * pi",
            "pi^0",
        ] {
            let v = out(src);
            assert_eq!(v["exact"], false, "{src}");
            assert!(v.get("fraction").is_none(), "{src}");
        }
        assert_eq!(format_number(-0.0), "0");
        assert_eq!(format_number(1.0 / 3.0), "0.3333333333");
    }

    #[test]
    fn guards() {
        for src in ["1/0", "0^-1", "0^-0.5", "0^(-1e-1000)"] {
            assert_eq!(evaluate(src).unwrap_err(), "division by zero", "{src}");
        }
        for src in ["2^100000", "2^-100000", "pi^100000", "2^round(100000+pi)"] {
            assert!(
                evaluate(src).unwrap_err().contains("exponent too large"),
                "{src}"
            );
        }
        for src in [
            "2^4096",
            "2^-4096",
            "(3^2585)/(3^2585)",
            "(2^3000)*(2^3000)",
        ] {
            assert!(evaluate(src).unwrap_err().contains("4096 bits"), "{src}");
        }
        assert_eq!(
            evaluate("sqrt(-1e-1000)").unwrap_err(),
            "sqrt of a negative number"
        );
        assert!(evaluate(&format!("{}1{}", "(".repeat(64), ")".repeat(64))).is_ok());
        for src in [
            format!("{}1{}", "(".repeat(65), ")".repeat(65)),
            format!("{}1", "-".repeat(65)),
            format!("{}1", "abs ".repeat(65)),
            format!("{}1", "1^".repeat(65)),
        ] {
            assert_eq!(evaluate(&src).unwrap_err(), "expression too deep");
        }
        assert_eq!(
            evaluate(&"1".repeat(501)).unwrap_err(),
            "expression too long"
        );
    }

    /// `n / 10^places` written as a decimal literal, e.g. (-1234, 2) -> "(-12.34)".
    fn lit(n: i64, places: u32) -> String {
        let scale = 10i64.pow(places);
        let sign = if n < 0 { "-" } else { "" };
        let (int, frac) = (n.abs() / scale, n.abs() % scale);
        if places == 0 {
            format!("({sign}{int})")
        } else {
            format!("({sign}{int}.{frac:0>width$})", width = places as usize)
        }
    }

    proptest! {
        #[test]
        fn exact_round_trips(
            an in -1_000_000i64..1_000_000, ap in 0u32..5,
            bn in -1_000_000i64..1_000_000, bp in 0u32..5,
        ) {
            let (a, b) = (lit(an, ap), lit(bn, bp));
            let want = evaluate(&a).unwrap();
            prop_assert_eq!(evaluate(&format!("{a} + {b} - {b}")).unwrap(), want.clone());
            if bn != 0 {
                prop_assert_eq!(evaluate(&format!("({a} * {b}) / {b}")).unwrap(), want);
            }
        }
    }
}
