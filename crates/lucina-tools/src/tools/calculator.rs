//! `calculator`: a safe arithmetic parser (recursive descent, no eval).
//!
//! `+ - * / ^ %` (postfix percent = /100), parentheses, unary minus, `×`,
//! `÷`, `x` between numbers, `of`/`di` as multiplication ("17% of 230"),
//! decimal comma, constants `pi`, `e`, functions `sqrt abs round floor ceil
//! ln log log2 exp sin cos tan`.

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{schema_for, Ctx, SideEffect, Tool, ToolError, ToolResult};

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(f64),
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
        if c.is_ascii_digit() || (c == '.' && chars.get(i + 1).is_some_and(|d| d.is_ascii_digit())) {
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
            let normalized = if text.contains(',') && !text.contains('.') { text.replace(',', ".") } else { text.replace(',', "") };
            let n: f64 = normalized.parse().map_err(|_| format!("bad number {text:?}"))?;
            out.push(Tok::Num(n));
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
    // expr := term (('+'|'-') term)*
    fn expr(&mut self) -> Result<f64, String> {
        self.depth += 1;
        if self.depth > 64 {
            return Err("expression too deep".into());
        }
        let mut v = self.term()?;
        while let Some(Tok::Op(op @ ('+' | '-'))) = self.peek().cloned() {
            self.pos += 1;
            let rhs = self.term()?;
            v = if op == '+' { v + rhs } else { v - rhs };
        }
        self.depth -= 1;
        Ok(v)
    }
    // term := power (('*'|'/') power | implicit power)*
    fn term(&mut self) -> Result<f64, String> {
        let mut v = self.unary()?;
        loop {
            match self.peek().cloned() {
                Some(Tok::Op(op @ ('*' | '/'))) => {
                    self.pos += 1;
                    let rhs = self.unary()?;
                    if op == '/' {
                        if rhs == 0.0 {
                            return Err("division by zero".into());
                        }
                        v /= rhs;
                    } else {
                        v *= rhs;
                    }
                }
                // implicit multiplication: 2(3+4), 2pi
                Some(Tok::LParen) | Some(Tok::Ident(_)) => v *= self.unary()?,
                _ => return Ok(v),
            }
        }
    }
    // unary := ('-'|'+') unary | power
    fn unary(&mut self) -> Result<f64, String> {
        match self.peek() {
            Some(Tok::Op('-')) => {
                self.pos += 1;
                Ok(-self.unary()?)
            }
            Some(Tok::Op('+')) => {
                self.pos += 1;
                self.unary()
            }
            _ => self.power(),
        }
    }
    // power := postfix ('^' unary)?
    fn power(&mut self) -> Result<f64, String> {
        let base = self.postfix()?;
        if let Some(Tok::Op('^')) = self.peek() {
            self.pos += 1;
            let exp = self.unary()?;
            return Ok(base.powf(exp));
        }
        Ok(base)
    }
    // postfix := atom '%'*
    fn postfix(&mut self) -> Result<f64, String> {
        let mut v = self.atom()?;
        while let Some(Tok::Op('%')) = self.peek() {
            self.pos += 1;
            v /= 100.0;
        }
        Ok(v)
    }
    fn atom(&mut self) -> Result<f64, String> {
        match self.next() {
            Some(Tok::Num(n)) => Ok(n),
            Some(Tok::LParen) => {
                let v = self.expr()?;
                match self.next() {
                    Some(Tok::RParen) => Ok(v),
                    _ => Err("missing ')'".into()),
                }
            }
            Some(Tok::Ident(name)) => match name.as_str() {
                "pi" | "π" => Ok(std::f64::consts::PI),
                "e" => Ok(std::f64::consts::E),
                f => {
                    let arg = match self.peek() {
                        Some(Tok::LParen) => self.atom()?,
                        _ => self.postfix()?,
                    };
                    Ok(match f {
                        "sqrt" | "radice" => {
                            if arg < 0.0 {
                                return Err("sqrt of a negative number".into());
                            }
                            arg.sqrt()
                        }
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
                    })
                }
            },
            Some(t) => Err(format!("unexpected {t:?}")),
            None => Err("unexpected end of expression".into()),
        }
    }
}

/// Evaluate an expression.
pub fn evaluate(src: &str) -> Result<f64, String> {
    if src.chars().count() > 500 {
        return Err("expression too long".into());
    }
    let toks = tokenize(src)?;
    if toks.is_empty() {
        return Err("empty expression".into());
    }
    let mut p = Parser { toks, pos: 0, depth: 0 };
    let v = p.expr()?;
    if p.pos < p.toks.len() {
        return Err(format!("unexpected {:?}", p.toks[p.pos]));
    }
    if !v.is_finite() {
        return Err("result is not a finite number".into());
    }
    Ok(v)
}

/// Up to 10 decimals, no trailing zeros.
pub fn format_number(v: f64) -> String {
    let s = format!("{:.10}", v);
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.to_owned() }
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
        "Exact arithmetic: + - * / ^, percent (17% * 230), parentheses, sqrt/abs/round/ln/log/sin/cos/tan, pi, e. Use it for any calculation instead of mental math."
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
        Ok(json!({"expression": a.expression, "result": v, "formatted": format_number(v)}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            let got = evaluate(src).unwrap_or_else(|e| panic!("{src}: {e}"));
            assert!((got - want).abs() < 1e-9, "{src}: {got} != {want}");
        }
        assert_eq!(format_number(evaluate("17% * 230").unwrap()), "39.1");
        assert_eq!(format_number(evaluate("1/3").unwrap()), "0.3333333333");
        for bad in ["1/0", "2 +", "(1", "rm -rf", "sqrt(-1)", "", "foo(2)", "2 ** ", "1e999*10"] {
            assert!(evaluate(bad).is_err(), "{bad}");
        }
    }
}
