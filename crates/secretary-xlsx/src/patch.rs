//! Editing a workbook in place: only the edited `<c>` elements of the edited sheets change (and
//! `calcPr` in the workbook part); every other byte of those parts and every other part stays as
//! it was. Strings already in the shared-string table reuse their index, new ones are written
//! inline, so `sharedStrings.xml` never changes. Formula cells cannot be edited, so the
//! calculation chain stays valid; `fullCalcOnLoad` makes Excel recompute every cached result
//! when it opens the file.

use std::collections::{BTreeMap, HashMap};

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::cellref;
use crate::numfmt::{self, FmtKind};
use crate::package::Package;
use crate::read::{read_package, Sheet, Value, Workbook};
use crate::xml::{attrs, escape_attr, escape_text, local, Scanner, Tok};
use crate::Error;

/// Excel's limit for one cell's text.
pub const MAX_TEXT: usize = 32_767;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", content = "v", rename_all = "snake_case")]
pub enum EditValue {
    /// Clear the value (the cell keeps its style).
    Empty,
    Number(f64),
    Text(String),
    Bool(bool),
    /// What the owner typed, read the way Excel reads typing into this cell: `9:30` in a time
    /// cell, `2026-11-03` or `03-11-2026` in a date cell, a number, `TRUE`, or text.
    Input(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edit {
    pub sheet: String,
    pub cell: String,
    pub value: EditValue,
}

#[derive(Debug, Clone)]
pub struct Options {
    /// Set `<calcPr fullCalcOnLoad="1"/>` so Excel recomputes the cached formula results.
    pub full_calc_on_load: bool,
    /// Refuse locked cells on protected sheets, as Excel does.
    pub enforce_protection: bool,
    /// Refuse values a data validation with a stop alert refuses, as Excel does.
    pub enforce_validation: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            full_calc_on_load: true,
            enforce_protection: true,
            enforce_validation: true,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Changed {
    pub sheet: String,
    pub cell: String,
    pub before: String,
    pub after: String,
}

#[derive(Debug)]
pub struct Outcome {
    pub bytes: Vec<u8>,
    pub changed: Vec<Changed>,
    /// The parts whose bytes differ from the original.
    pub parts: Vec<String>,
}

fn invalid(sheet: &str, cell: &str, reason: impl Into<String>) -> Error {
    Error::Invalid {
        sheet: sheet.into(),
        cell: cell.into(),
        reason: reason.into(),
    }
}

pub fn apply(bytes: &[u8], edits: &[Edit], opts: &Options) -> Result<Outcome, Error> {
    let pkg = Package::open(bytes)?;
    let wb = read_package(&pkg)?;

    // Resolve every edit to (sheet index, row, col) → new value; the last edit of a cell wins.
    let mut pending: BTreeMap<(usize, u32, u32), Value> = BTreeMap::new();
    for e in edits {
        let si = wb
            .sheets
            .iter()
            .position(|s| s.name == e.sheet)
            .ok_or_else(|| Error::UnknownSheet(e.sheet.clone()))?;
        let sheet = &wb.sheets[si];
        let (row, col) =
            cellref::parse(e.cell.trim()).ok_or_else(|| Error::BadCell(e.cell.clone()))?;
        let name = cellref::name(row, col);
        let existing = sheet.cells.get(&(row, col));
        if existing.is_some_and(|c| c.formula.is_some()) {
            return Err(Error::FormulaCell {
                sheet: sheet.name.clone(),
                cell: name,
            });
        }
        if opts.enforce_protection && !wb.editable(sheet, row, col) {
            return Err(Error::LockedCell {
                sheet: sheet.name.clone(),
                cell: name,
            });
        }
        let style = existing.map_or_else(|| wb.default_style(sheet, row, col), |c| c.style);
        let was_bool = existing.is_some_and(|c| matches!(c.value, Value::Bool(_)));
        let v = match &e.value {
            EditValue::Empty => Value::Empty,
            EditValue::Number(n) if n.is_finite() => Value::Number(*n),
            EditValue::Number(_) => return Err(invalid(&sheet.name, &name, "not a finite number")),
            EditValue::Text(s) => Value::Text(s.clone()),
            EditValue::Bool(b) => Value::Bool(*b),
            EditValue::Input(s) => {
                parse_input(&wb, style, was_bool, s).map_err(|r| invalid(&sheet.name, &name, r))?
            }
        };
        if let Value::Text(s) = &v {
            if s.chars().count() > MAX_TEXT {
                return Err(invalid(&sheet.name, &name, "more than 32767 characters"));
            }
        }
        pending.insert((si, row, col), v);
    }

    if opts.enforce_validation {
        let keys: Vec<_> = pending.keys().copied().collect();
        for (si, row, col) in keys {
            let v = pending[&(si, row, col)].clone();
            let nv = validate(&wb, &pending, si, row, col, v)?;
            pending.insert((si, row, col), nv);
        }
    }

    // Drop edits that change nothing.
    let mut changed = Vec::new();
    pending.retain(|&(si, row, col), v| {
        let sheet = &wb.sheets[si];
        let before = sheet
            .cells
            .get(&(row, col))
            .map_or(Value::Empty, |c| c.value.clone());
        if before == *v {
            return false;
        }
        let style = sheet.cells.get(&(row, col)).map_or(0, |c| c.style);
        let show = |x: &Value| match x {
            Value::Number(n) => numfmt::format_number(&wb.style(style).num_fmt, *n, wb.date1904),
            Value::Text(s) => s.clone(),
            Value::Bool(b) => {
                if *b {
                    "TRUE".into()
                } else {
                    "FALSE".into()
                }
            }
            Value::Error(e) => e.clone(),
            Value::Empty => String::new(),
        };
        changed.push(Changed {
            sheet: sheet.name.clone(),
            cell: cellref::name(row, col),
            before: show(&before),
            after: show(v),
        });
        true
    });

    let mut replaced: HashMap<String, Vec<u8>> = HashMap::new();
    let mut by_sheet: BTreeMap<usize, Vec<(u32, u32, Value)>> = BTreeMap::new();
    for ((si, row, col), v) in pending {
        by_sheet.entry(si).or_default().push((row, col, v));
    }
    for (si, cells) in by_sheet {
        let sheet = &wb.sheets[si];
        let text = pkg
            .read_str(&sheet.part)?
            .ok_or_else(|| Error::NotXlsx(format!("missing {}", sheet.part)))?;
        let out = splice_sheet(&wb, sheet, &text, &cells).map_err(Error::xml(&sheet.part))?;
        replaced.insert(sheet.part.clone(), out.into_bytes());
    }
    if !replaced.is_empty() && opts.full_calc_on_load && !wb.full_calc_on_load {
        let text = pkg
            .read_str(&wb.workbook_part)?
            .ok_or_else(|| Error::NotXlsx("missing workbook part".into()))?;
        let out = set_full_calc(&text).map_err(Error::xml(&wb.workbook_part))?;
        replaced.insert(wb.workbook_part.clone(), out.into_bytes());
    }
    if replaced.is_empty() {
        return Ok(Outcome {
            bytes: bytes.to_vec(),
            changed,
            parts: Vec::new(),
        });
    }
    let mut parts: Vec<String> = replaced.keys().cloned().collect();
    parts.sort();
    Ok(Outcome {
        bytes: pkg.rewrite(&replaced)?,
        changed,
        parts,
    })
}

// ---- reading what the owner typed ----------------------------------------------------------

fn parse_bool(s: &str) -> Option<bool> {
    match s.trim().to_lowercase().as_str() {
        "true" | "yes" | "1" | "ja" | "sand" | "sì" | "si" | "vero" | "x" => Some(true),
        "false" | "no" | "0" | "nej" | "falsk" | "falso" => Some(false),
        _ => None,
    }
}

pub fn parse_number(s: &str) -> Option<f64> {
    let t: String = s
        .trim()
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '\u{a0}')
        .collect();
    if t.is_empty() {
        return None;
    }
    let t = if !t.contains('.') && t.matches(',').count() == 1 {
        t.replace(',', ".")
    } else {
        t.replace(',', "")
    };
    if !t
        .bytes()
        .all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'-' | b'+' | b'e' | b'E'))
    {
        return None;
    }
    t.parse::<f64>().ok().filter(|v| v.is_finite())
}

/// `9:30`, `09:30:15`, `9.30`, `9` (hours) → fraction of a day. Durations may pass 24 hours and
/// be negative.
pub fn parse_time(s: &str, duration: bool) -> Option<f64> {
    let t = s.trim();
    let (neg, t) = match t.strip_prefix('-') {
        Some(r) if duration => (true, r.trim()),
        _ => (false, t),
    };
    let parts: Vec<&str> = if t.contains(':') {
        t.split(':').collect()
    } else if let Some((h, m)) = t.split_once('.') {
        if m.len() != 2 {
            return None;
        }
        vec![h, m]
    } else {
        vec![t]
    };
    if parts.is_empty()
        || parts.len() > 3
        || parts
            .iter()
            .any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) || p.len() > 5)
    {
        return None;
    }
    let h: u64 = parts[0].parse().ok()?;
    let m: u64 = parts.get(1).map_or(Some(0), |p| p.parse().ok())?;
    let sec: u64 = parts.get(2).map_or(Some(0), |p| p.parse().ok())?;
    if m >= 60 || sec >= 60 || (!duration && h >= 24) || h > 9999 {
        return None;
    }
    let v = (h * 3600 + m * 60 + sec) as f64 / 86400.0;
    Some(if neg { -v } else { v })
}

/// `2026-11-03`, `03-11-2026`, `3/11/2026`, `03.11.26` → the date (day first, as in Denmark and
/// Italy).
pub fn parse_date(s: &str) -> Option<NaiveDate> {
    let t = s.trim();
    let parts: Vec<&str> = t.split(['-', '/', '.']).collect();
    if parts.len() != 3
        || parts
            .iter()
            .any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    let n: Vec<i32> = parts.iter().map(|p| p.parse().unwrap_or(-1)).collect();
    if parts[0].len() == 4 {
        return NaiveDate::from_ymd_opt(n[0], n[1] as u32, n[2] as u32);
    }
    let y = match parts[2].len() {
        4 => n[2],
        2 => 2000 + n[2],
        _ => return None,
    };
    NaiveDate::from_ymd_opt(y, n[1] as u32, n[0] as u32)
}

fn parse_input(wb: &Workbook, style: u32, was_bool: bool, raw: &str) -> Result<Value, String> {
    let t = raw.trim();
    if t.is_empty() {
        return Ok(Value::Empty);
    }
    if was_bool {
        return parse_bool(t)
            .map(Value::Bool)
            .ok_or_else(|| "expected yes or no".into());
    }
    let kind = wb.style(style).kind.unwrap_or(FmtKind::General);
    match kind {
        FmtKind::Time | FmtKind::Duration => parse_time(t, kind == FmtKind::Duration)
            .map(Value::Number)
            .ok_or_else(|| "expected a time as hh:mm".into()),
        FmtKind::Date => parse_date(t)
            .map(|d| Value::Number(numfmt::date_to_serial(d, wb.date1904)))
            .ok_or_else(|| "expected a date as yyyy-mm-dd or dd-mm-yyyy".into()),
        FmtKind::DateTime => {
            let (d, tm) = t.split_once(' ').map_or((t, None), |(a, b)| (a, Some(b)));
            let day = parse_date(d).ok_or("expected a date and time as yyyy-mm-dd hh:mm")?;
            let frac = match tm {
                Some(x) => {
                    parse_time(x, false).ok_or("expected a date and time as yyyy-mm-dd hh:mm")?
                }
                None => 0.0,
            };
            Ok(Value::Number(
                numfmt::date_to_serial(day, wb.date1904) + frac,
            ))
        }
        FmtKind::Text => Ok(Value::Text(raw.to_string())),
        FmtKind::Percent => parse_number(t.trim_end_matches('%'))
            .map(|v| Value::Number(v / 100.0))
            .ok_or_else(|| "expected a percentage".into()),
        FmtKind::Number | FmtKind::General => {
            if let Some(v) = parse_number(t) {
                return Ok(Value::Number(v));
            }
            if t.eq_ignore_ascii_case("true") || t.eq_ignore_ascii_case("false") {
                return Ok(Value::Bool(t.eq_ignore_ascii_case("true")));
            }
            if kind == FmtKind::Number {
                return Err("expected a number".into());
            }
            Ok(Value::Text(raw.to_string()))
        }
    }
}

// ---- data validation -----------------------------------------------------------------------

/// Split a formula's arguments on top-level commas.
fn split_args(s: &str) -> Vec<&str> {
    let (mut depth, mut quoted, mut start) = (0i32, false, 0);
    let mut out = Vec::new();
    for (i, c) in s.char_indices() {
        match c {
            '"' => quoted = !quoted,
            '(' if !quoted => depth += 1,
            ')' if !quoted => depth -= 1,
            ',' if !quoted && depth == 0 => {
                out.push(s[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(s[start..].trim());
    out
}

/// `Sheet!$A$1:$B$2`, `'My sheet'!A1`, `$M$3:$M$5` → (sheet index, rectangle).
fn parse_ref(wb: &Workbook, here: usize, r: &str) -> Option<(usize, (u32, u32, u32, u32))> {
    let r = r.trim();
    let (si, rng) = match r.rsplit_once('!') {
        Some((sh, rng)) => {
            let sh = sh.trim_matches('\'').replace("''", "'");
            (wb.sheets.iter().position(|s| s.name == sh)?, rng)
        }
        None => (here, r),
    };
    let rect = cellref::parse_range(&rng.replace('$', ""))?;
    Some((si, rect))
}

fn value_at(
    wb: &Workbook,
    pending: &BTreeMap<(usize, u32, u32), Value>,
    si: usize,
    row: u32,
    col: u32,
) -> Value {
    pending
        .get(&(si, row, col))
        .cloned()
        .or_else(|| {
            wb.sheets[si]
                .cells
                .get(&(row, col))
                .map(|c| c.value.clone())
        })
        .unwrap_or(Value::Empty)
}

fn as_text(v: &Value) -> Option<String> {
    match v {
        Value::Empty => None,
        Value::Text(s) => Some(s.clone()),
        Value::Number(n) => Some(numfmt::general(*n)),
        Value::Bool(b) => Some(if *b { "TRUE" } else { "FALSE" }.into()),
        Value::Error(e) => Some(e.clone()),
    }
}

/// The choices of a list validation, with the cells' current (pending) values. `None` when the
/// formula is beyond this reader (then the value is not checked).
pub(crate) fn list_choices(
    wb: &Workbook,
    pending: &BTreeMap<(usize, u32, u32), Value>,
    si: usize,
    formula: &str,
    depth: u8,
) -> Option<Vec<String>> {
    let f = formula.trim().trim_start_matches('=');
    if depth > 3 {
        return None;
    }
    if let Some(inner) = f.strip_prefix('"').and_then(|x| x.strip_suffix('"')) {
        return Some(
            inner
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
        );
    }
    if let Some(args) = f
        .strip_prefix("IF(")
        .or_else(|| f.strip_prefix("if("))
        .and_then(|x| x.strip_suffix(')'))
    {
        let a = split_args(args);
        if a.len() == 3 {
            let (csi, (r, c, _, _)) = parse_ref(wb, si, a[0])?;
            let cond = match value_at(wb, pending, csi, r, c) {
                Value::Bool(b) => b,
                Value::Number(n) => n != 0.0,
                Value::Empty => false,
                _ => return None,
            };
            return list_choices(wb, pending, si, if cond { a[1] } else { a[2] }, depth + 1);
        }
        return None;
    }
    if let Some(dn) = wb
        .defined_names
        .iter()
        .find(|d| d.name.eq_ignore_ascii_case(f))
    {
        return list_choices(wb, pending, si, &dn.formula, depth + 1);
    }
    let (rsi, (r1, c1, r2, c2)) = parse_ref(wb, si, f)?;
    if (r2 - r1 + 1) * (c2 - c1 + 1) > 10_000 {
        return None;
    }
    let mut out: Vec<String> = Vec::new();
    for r in r1..=r2 {
        for c in c1..=c2 {
            if let Some(t) = as_text(&value_at(wb, pending, rsi, r, c)) {
                if !out.contains(&t) {
                    out.push(t);
                }
            }
        }
    }
    Some(out)
}

fn validate(
    wb: &Workbook,
    pending: &BTreeMap<(usize, u32, u32), Value>,
    si: usize,
    row: u32,
    col: u32,
    mut v: Value,
) -> Result<Value, Error> {
    let sheet = &wb.sheets[si];
    let name = cellref::name(row, col);
    if v == Value::Empty {
        return Ok(v);
    }
    for dv in sheet
        .validations
        .iter()
        .filter(|d| cellref::in_sqref(&d.sqref, row, col))
    {
        if !dv.show_error || dv.error_style != "stop" {
            continue;
        }
        let refuse = |why: String| {
            let msg = dv
                .error
                .clone()
                .filter(|m| !m.trim().is_empty())
                .unwrap_or(why);
            Err(invalid(&sheet.name, &name, msg))
        };
        match dv.kind.as_str() {
            "list" => {
                let Some(choices) = dv
                    .formula1
                    .as_deref()
                    .and_then(|f| list_choices(wb, pending, si, f, 0))
                else {
                    continue;
                };
                let typed = as_text(&v).unwrap_or_default();
                match choices
                    .iter()
                    .find(|c| c.eq_ignore_ascii_case(typed.trim()))
                {
                    // Excel stores the list's spelling.
                    Some(c) => {
                        return Ok(match v {
                            Value::Number(_) => v,
                            _ => Value::Text(c.clone()),
                        })
                    }
                    None => return refuse(format!("choose one of {}", choices.join(", "))),
                }
            }
            "whole" | "decimal" | "date" | "time" => {
                // Typed into a General cell, `8:00` or `03-11-2026` becomes a number, as in Excel.
                let typed = match &v {
                    Value::Text(t) => match dv.kind.as_str() {
                        "time" => parse_time(t, false),
                        "date" => parse_date(t).map(|d| numfmt::date_to_serial(d, wb.date1904)),
                        _ => parse_number(t),
                    },
                    _ => None,
                };
                if let Some(n) = typed {
                    v = Value::Number(n);
                }
                let Value::Number(x) = v else {
                    return refuse("expected a number".into());
                };
                if dv.kind == "whole" && x.fract() != 0.0 {
                    return refuse("expected a whole number".into());
                }
                let num =
                    |f: &Option<String>| f.as_deref().and_then(|s| s.trim().parse::<f64>().ok());
                let (Some(a), b) = (num(&dv.formula1), num(&dv.formula2)) else {
                    continue;
                };
                let eps = 1e-9;
                let ok = match dv.operator.as_str() {
                    "between" => b.is_none_or(|b| x >= a - eps && x <= b + eps),
                    "notBetween" => b.is_none_or(|b| x < a - eps || x > b + eps),
                    "equal" => (x - a).abs() <= eps,
                    "notEqual" => (x - a).abs() > eps,
                    "greaterThan" => x > a,
                    "lessThan" => x < a,
                    "greaterThanOrEqual" => x >= a - eps,
                    "lessThanOrEqual" => x <= a + eps,
                    _ => true,
                };
                if !ok {
                    return refuse("value out of the allowed range".into());
                }
            }
            "textLength" => {
                let n = as_text(&v).map_or(0, |t| t.chars().count()) as f64;
                let num =
                    |f: &Option<String>| f.as_deref().and_then(|s| s.trim().parse::<f64>().ok());
                if let (Some(a), b) = (num(&dv.formula1), num(&dv.formula2)) {
                    let ok = match dv.operator.as_str() {
                        "between" => b.is_none_or(|b| n >= a && n <= b),
                        "lessThanOrEqual" => n <= a,
                        "lessThan" => n < a,
                        "greaterThan" => n > a,
                        "greaterThanOrEqual" => n >= a,
                        "equal" => n == a,
                        _ => true,
                    };
                    if !ok {
                        return refuse("text length out of the allowed range".into());
                    }
                }
            }
            _ => {}
        }
    }
    Ok(v)
}

// ---- splicing ------------------------------------------------------------------------------

struct CellSpan<'a> {
    col: u32,
    start: usize,
    end: usize,
    name: &'a str,
    attrs: &'a str,
}

struct RowSpan<'a> {
    row: u32,
    open_start: usize,
    open_end: usize,
    name: &'a str,
    attrs: &'a str,
    /// `<row …/>` with no cells.
    empty: bool,
    close_start: usize,
    cells: Vec<CellSpan<'a>>,
}

struct SheetSpans<'a> {
    /// The `<sheetData>` open tag and where `</sheetData>` starts (`None` for `<sheetData/>`).
    data_open: (usize, usize),
    data_name: &'a str,
    data_close: Option<usize>,
    rows: Vec<RowSpan<'a>>,
    dimension: Option<(usize, usize, &'a str, &'a str)>,
}

fn scan_spans(text: &str) -> Result<SheetSpans<'_>, crate::xml::XmlError> {
    let mut sc = Scanner::new(text);
    let mut spans = SheetSpans {
        data_open: (0, 0),
        data_name: "sheetData",
        data_close: None,
        rows: Vec::new(),
        dimension: None,
    };
    let mut found_data = false;
    let mut in_data = false;
    let mut row_no = 0u32;
    let mut next_col = 1u32;
    while let Some(t) = sc.next() {
        match t? {
            Tok::Open {
                name,
                attrs: a,
                empty,
                start,
                end,
            } => match local(name) {
                "dimension" if !in_data => spans.dimension = Some((start, end, name, a)),
                "sheetData" => {
                    found_data = true;
                    spans.data_open = (start, end);
                    spans.data_name = name;
                    if !empty {
                        in_data = true;
                    }
                }
                "row" if in_data => {
                    row_no = crate::xml::attr(a, "r")
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(row_no + 1);
                    next_col = 1;
                    spans.rows.push(RowSpan {
                        row: row_no,
                        open_start: start,
                        open_end: end,
                        name,
                        attrs: a,
                        empty,
                        close_start: end,
                        cells: Vec::new(),
                    });
                }
                "c" if in_data => {
                    let col = crate::xml::attr(a, "r")
                        .and_then(|r| cellref::parse(&r))
                        .map_or(next_col, |(_, c)| c);
                    next_col = col + 1;
                    let mut cend = end;
                    if !empty {
                        // To the matching </c>.
                        let mut depth = 0;
                        loop {
                            match sc.next() {
                                Some(Ok(Tok::Open { empty: false, .. })) => depth += 1,
                                Some(Ok(Tok::Close {
                                    name: n, end: e, ..
                                })) => {
                                    if depth == 0 && local(n) == "c" {
                                        cend = e;
                                        break;
                                    }
                                    depth -= 1;
                                }
                                Some(Err(e)) => return Err(e),
                                None => {
                                    return Err(crate::xml::XmlError(
                                        "unterminated cell".into(),
                                        start,
                                    ))
                                }
                                _ => {}
                            }
                        }
                    }
                    if let Some(r) = spans.rows.last_mut() {
                        r.cells.push(CellSpan {
                            col,
                            start,
                            end: cend,
                            name,
                            attrs: a,
                        });
                    }
                }
                _ => {}
            },
            Tok::Close { name, start, .. } => match local(name) {
                "row" if in_data => {
                    if let Some(r) = spans.rows.last_mut() {
                        r.close_start = start;
                    }
                }
                "sheetData" => {
                    spans.data_close = Some(start);
                    in_data = false;
                }
                _ => {}
            },
            _ => {}
        }
    }
    if !found_data {
        return Err(crate::xml::XmlError("no sheetData".into(), 0));
    }
    Ok(spans)
}

fn prefix_of(name: &str) -> &str {
    name.rsplit_once(':').map_or("", |(p, _)| p)
}

fn q(prefix: &str, local: &str) -> String {
    if prefix.is_empty() {
        local.to_string()
    } else {
        format!("{prefix}:{local}")
    }
}

/// A `<c>` element: the kept attributes (in their order, without `t`, `cm`, `vm`), the new type
/// and value.
fn cell_xml(prefix: &str, kept: &[(String, String)], wb: &Workbook, v: &Value) -> String {
    let c = q(prefix, "c");
    let mut out = format!("<{c}");
    for (k, val) in kept {
        out.push_str(&format!(" {k}=\"{}\"", escape_attr(val)));
    }
    match v {
        Value::Empty | Value::Error(_) => out.push_str("/>"),
        Value::Number(n) => out.push_str(&format!("><{v}>{n}</{v}></{c}>", v = q(prefix, "v"))),
        Value::Bool(b) => out.push_str(&format!(
            " t=\"b\"><{v}>{}</{v}></{c}>",
            u8::from(*b),
            v = q(prefix, "v")
        )),
        Value::Text(s) => match wb.shared_strings.iter().position(|x| x == s) {
            Some(i) => out.push_str(&format!(
                " t=\"s\"><{v}>{i}</{v}></{c}>",
                v = q(prefix, "v")
            )),
            None => {
                let space = if s.trim() != s || s.contains('\n') {
                    " xml:space=\"preserve\""
                } else {
                    ""
                };
                out.push_str(&format!(
                    " t=\"inlineStr\"><{is}><{t}{space}>{}</{t}></{is}></{c}>",
                    escape_text(s),
                    is = q(prefix, "is"),
                    t = q(prefix, "t"),
                ));
            }
        },
    }
    out
}

fn splice_sheet(
    wb: &Workbook,
    sheet: &Sheet,
    text: &str,
    cells: &[(u32, u32, Value)],
) -> Result<String, crate::xml::XmlError> {
    let spans = scan_spans(text)?;
    let prefix = prefix_of(spans.data_name).to_string();
    // (start, end, seq, text)
    let mut ops: Vec<(usize, usize, usize, String)> = Vec::new();
    let mut seq = 0usize;
    let mut push = |ops: &mut Vec<_>, s: usize, e: usize, t: String| {
        ops.push((s, e, seq, t));
        seq += 1;
    };
    let mut by_row: BTreeMap<u32, Vec<(u32, &Value)>> = BTreeMap::new();
    for (r, c, v) in cells {
        by_row.entry(*r).or_default().push((*c, v));
    }
    let new_cell = |row: u32, col: u32, v: &Value| -> String {
        let mut kept = vec![("r".to_string(), cellref::name(row, col))];
        let s = wb.default_style(sheet, row, col);
        if s != 0 {
            kept.push(("s".to_string(), s.to_string()));
        }
        cell_xml(&prefix, &kept, wb, v)
    };
    let mut new_rows: BTreeMap<usize, String> = BTreeMap::new();
    let (mut max_r, mut max_c) = (0u32, 0u32);
    for (row, mut list) in by_row {
        list.sort_by_key(|(c, _)| *c);
        match spans.rows.iter().find(|r| r.row == row) {
            Some(rs) => {
                let mut inserted_cols: Vec<u32> = Vec::new();
                let mut empty_row_cells = String::new();
                for (col, v) in list {
                    match rs.cells.iter().find(|c| c.col == col) {
                        Some(cs) => {
                            let kept: Vec<(String, String)> = attrs(cs.attrs)
                                .into_iter()
                                .filter(|(k, _)| !matches!(local(k), "t" | "cm" | "vm"))
                                .map(|(k, v)| (k.to_string(), v))
                                .collect();
                            let mut kept = kept;
                            if !kept.iter().any(|(k, _)| k == "r") {
                                kept.insert(0, ("r".to_string(), cellref::name(row, col)));
                            }
                            push(
                                &mut ops,
                                cs.start,
                                cs.end,
                                cell_xml(prefix_of(cs.name), &kept, wb, v),
                            );
                        }
                        None if *v == Value::Empty => {}
                        None => {
                            max_r = max_r.max(row);
                            max_c = max_c.max(col);
                            inserted_cols.push(col);
                            if rs.empty {
                                empty_row_cells.push_str(&new_cell(row, col, v));
                            } else {
                                let at = rs
                                    .cells
                                    .iter()
                                    .find(|c| c.col > col)
                                    .map_or(rs.close_start, |c| c.start);
                                push(&mut ops, at, at, new_cell(row, col, v));
                            }
                        }
                    }
                }
                if !inserted_cols.is_empty() {
                    // Keep `spans` (a hint) covering the row's cells.
                    let mut a: Vec<(String, String)> = attrs(rs.attrs)
                        .into_iter()
                        .map(|(k, v)| (k.to_string(), v))
                        .collect();
                    let mut touched = false;
                    for (k, v) in a.iter_mut() {
                        if k == "spans" {
                            if let Some((lo, hi)) = v.split_once(':').and_then(|(l, h)| {
                                Some((l.parse::<u32>().ok()?, h.parse::<u32>().ok()?))
                            }) {
                                let lo2 = inserted_cols.iter().copied().fold(lo, u32::min);
                                let hi2 = inserted_cols.iter().copied().fold(hi, u32::max);
                                if (lo2, hi2) != (lo, hi) {
                                    *v = format!("{lo2}:{hi2}");
                                    touched = true;
                                }
                            }
                        }
                    }
                    let open = |a: &[(String, String)], close: bool| {
                        let mut s = format!("<{}", rs.name);
                        for (k, v) in a {
                            s.push_str(&format!(" {k}=\"{}\"", escape_attr(v)));
                        }
                        s.push_str(if close { "/>" } else { ">" });
                        s
                    };
                    if rs.empty {
                        let t = format!("{}{}</{}>", open(&a, false), empty_row_cells, rs.name);
                        push(&mut ops, rs.open_start, rs.open_end, t);
                    } else if touched {
                        push(&mut ops, rs.open_start, rs.open_end, open(&a, false));
                    }
                }
            }
            None => {
                let mut body = String::new();
                for (col, v) in list {
                    if *v == Value::Empty {
                        continue;
                    }
                    max_r = max_r.max(row);
                    max_c = max_c.max(col);
                    body.push_str(&new_cell(row, col, v));
                }
                if body.is_empty() {
                    continue;
                }
                let rname = q(&prefix, "row");
                let at = spans
                    .rows
                    .iter()
                    .find(|r| r.row > row)
                    .map_or(spans.data_close.unwrap_or(spans.data_open.1), |r| {
                        r.open_start
                    });
                new_rows
                    .entry(at)
                    .or_default()
                    .push_str(&format!("<{rname} r=\"{row}\">{body}</{rname}>"));
            }
        }
    }
    for (at, rows) in new_rows {
        if spans.data_close.is_none() {
            // <sheetData/> → <sheetData>rows</sheetData>
            let dn = spans.data_name;
            let open = &text[spans.data_open.0..spans.data_open.1];
            let open = format!(
                "{}>",
                open.trim_end_matches('>').trim_end_matches('/').trim_end()
            );
            push(
                &mut ops,
                spans.data_open.0,
                spans.data_open.1,
                format!("{open}{rows}</{dn}>"),
            );
        } else {
            push(&mut ops, at, at, rows);
        }
    }
    // Grow <dimension> when a new cell falls outside it.
    if max_r > 0 {
        if let Some((ds, de, dname, da)) = spans.dimension {
            let cur = crate::xml::attr(da, "ref").and_then(|r| cellref::parse_range(&r));
            let (r1, c1, r2, c2) = cur.unwrap_or((1, 1, 1, 1));
            let (nr2, nc2) = (r2.max(max_r), c2.max(max_c));
            if (nr2, nc2) != (r2, c2) {
                let rf = format!("{}:{}", cellref::name(r1, c1), cellref::name(nr2, nc2));
                push(&mut ops, ds, de, format!("<{dname} ref=\"{rf}\"/>"));
            }
        }
    }
    // Insertions go before a replacement that starts at the same place (a new cell before an
    // edited one, a new row before a row whose tag changes).
    ops.sort_by_key(|(s, e, q, _)| (*s, e > s, *q));
    let mut out = String::with_capacity(text.len() + 256);
    let mut cursor = 0;
    for (s, e, _, t) in ops {
        if s < cursor {
            return Err(crate::xml::XmlError("overlapping edits".into(), s));
        }
        out.push_str(&text[cursor..s]);
        out.push_str(&t);
        cursor = e;
    }
    out.push_str(&text[cursor..]);
    Ok(out)
}

/// `<calcPr … fullCalcOnLoad="1"/>`, adding the element where the schema puts it when missing.
pub(crate) fn set_full_calc(text: &str) -> Result<String, crate::xml::XmlError> {
    let mut after: Option<(usize, String)> = None;
    for t in Scanner::new(text) {
        match t? {
            Tok::Open {
                name,
                attrs: a,
                empty,
                start,
                end,
            } if local(name) == "calcPr" => {
                let mut kept: Vec<(String, String)> = attrs(a)
                    .into_iter()
                    .filter(|(k, _)| *k != "fullCalcOnLoad")
                    .map(|(k, v)| (k.to_string(), v))
                    .collect();
                kept.push(("fullCalcOnLoad".into(), "1".into()));
                let mut tag = format!("<{name}");
                for (k, v) in kept {
                    tag.push_str(&format!(" {k}=\"{}\"", escape_attr(&v)));
                }
                tag.push_str(if empty { "/>" } else { ">" });
                return Ok(format!("{}{}{}", &text[..start], tag, &text[end..]));
            }
            Tok::Close { name, end, .. }
                if matches!(
                    local(name),
                    "sheets" | "functionGroups" | "externalReferences" | "definedNames"
                ) =>
            {
                after = Some((end, prefix_of(name).to_string()));
            }
            Tok::Open {
                name,
                empty: true,
                end,
                ..
            } if matches!(
                local(name),
                "sheets" | "functionGroups" | "externalReferences" | "definedNames"
            ) =>
            {
                after = Some((end, prefix_of(name).to_string()));
            }
            _ => {}
        }
    }
    let (at, prefix) =
        after.ok_or_else(|| crate::xml::XmlError("no <sheets> in the workbook".into(), 0))?;
    Ok(format!(
        "{}<{} fullCalcOnLoad=\"1\"/>{}",
        &text[..at],
        q(&prefix, "calcPr"),
        &text[at..]
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing() {
        assert_eq!(parse_time("9:30", false), Some(9.5 / 24.0));
        assert_eq!(parse_time("09.30", false), Some(9.5 / 24.0));
        assert_eq!(parse_time("17", false), Some(17.0 / 24.0));
        assert_eq!(parse_time("25:00", false), None);
        assert_eq!(parse_time("37:00", true), Some(37.0 / 24.0));
        assert_eq!(parse_time("-1:30", true), Some(-1.5 / 24.0));
        assert_eq!(parse_time("9:75", false), None);
        assert_eq!(parse_time("9.5", false), None);
        assert_eq!(
            parse_date("2026-11-03"),
            NaiveDate::from_ymd_opt(2026, 11, 3)
        );
        assert_eq!(
            parse_date("03-11-2026"),
            NaiveDate::from_ymd_opt(2026, 11, 3)
        );
        assert_eq!(parse_date("3/11/26"), NaiveDate::from_ymd_opt(2026, 11, 3));
        assert_eq!(parse_date("31-02-2026"), None);
        assert_eq!(parse_number("12,5"), Some(12.5));
        assert_eq!(parse_number("1,234.5"), Some(1234.5));
        assert_eq!(parse_number("12 h"), None);
    }

    #[test]
    fn calc_pr() {
        let w =
            r#"<workbook><sheets><sheet name="a"/></sheets><calcPr calcId="191028"/></workbook>"#;
        assert_eq!(
            set_full_calc(w).unwrap(),
            r#"<workbook><sheets><sheet name="a"/></sheets><calcPr calcId="191028" fullCalcOnLoad="1"/></workbook>"#
        );
        let w = r#"<x:workbook><x:sheets><x:sheet name="a"/></x:sheets><x:definedNames><x:definedName name="n">a!A1</x:definedName></x:definedNames><x:extLst/></x:workbook>"#;
        assert!(set_full_calc(w)
            .unwrap()
            .contains("</x:definedNames><x:calcPr fullCalcOnLoad=\"1\"/><x:extLst/>"));
        let w = r#"<workbook><sheets/><calcPr fullCalcOnLoad="0" calcId="1"/></workbook>"#;
        assert_eq!(
            set_full_calc(w).unwrap(),
            r#"<workbook><sheets/><calcPr calcId="1" fullCalcOnLoad="1"/></workbook>"#
        );
    }
}
