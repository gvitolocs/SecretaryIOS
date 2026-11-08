//! Spreadsheets and delimited text: xlsx/xlsm/xls/ods through calamine,
//! csv/tsv through a small quote-aware parser. Each sheet becomes a table
//! with a header row, the first rows as JSON and sums for numeric columns.

use std::collections::HashSet;
use std::io::Read;
use std::path::Path;

use anyhow::{bail, Context};
use calamine::{open_workbook_auto, Data, ExcelDateTime, ExcelDateTimeType, Reader};
use serde_json::{json, Map, Value};

use super::Extracted;

const MAX_SHEETS: usize = 20;
const MAX_COLUMNS: usize = 100;
const JSON_ROWS: usize = 200;
const TEXT_ROWS: usize = 50;
/// Delimited files larger than this are read only up to here.
const MAX_CSV_BYTES: u64 = 64 * 1024 * 1024;

/// Reads a workbook (`xlsx`, `xlsm`, `xls`, `ods`, …) or a delimited file
/// (`csv`, `tsv`) into one table per sheet.
pub fn read_sheet(path: &Path, ext: &str) -> anyhow::Result<Extracted> {
    let ext = ext.trim().trim_start_matches('.').to_ascii_lowercase();
    let mut warnings = Vec::new();
    let mut tables = Vec::new();
    let title = match ext.as_str() {
        "csv" | "tsv" | "tab" | "txt" => {
            let text = read_text(path, &mut warnings)?;
            let delim = if ext == "csv" || ext == "txt" {
                sniff_delimiter(&text)
            } else {
                '\t'
            };
            let rows: Vec<Vec<Cell>> = parse_delimited(&text, delim)
                .into_iter()
                .map(|r| r.into_iter().map(Cell::from_text).collect())
                .collect();
            let name = file_stem(path);
            let default_dec = if delim == ';' { ',' } else { '.' };
            if let Some(t) = build_table(&name, rows, default_dec, &mut warnings) {
                tables.push(t);
            }
            name
        }
        _ => {
            let mut book = open_workbook_auto(path)
                .with_context(|| format!("cannot open the {ext} workbook"))?;
            let names = book.sheet_names();
            if names.len() > MAX_SHEETS {
                warnings.push(format!(
                    "The workbook has {} sheets; only the first {MAX_SHEETS} were read.",
                    names.len()
                ));
            }
            for name in names.iter().take(MAX_SHEETS) {
                match book.worksheet_range(name) {
                    Ok(range) => {
                        let rows: Vec<Vec<Cell>> = range
                            .rows()
                            .map(|r| r.iter().map(Cell::from_data).collect())
                            .collect();
                        if let Some(t) = build_table(name, rows, '.', &mut warnings) {
                            tables.push(t);
                        }
                    }
                    Err(e) => warnings.push(format!("Sheet {name} could not be read: {e}")),
                }
            }
            names.first().cloned().unwrap_or_else(|| file_stem(path))
        }
    };
    if tables.is_empty() {
        warnings.push("No data rows were found.".to_string());
    }

    let mut out = Extracted::new("sheet");
    out.subtype = Some(ext);
    out.title = Some(title);
    out.text = tables
        .iter()
        .map(Table::to_text)
        .collect::<Vec<_>>()
        .join("\n\n");
    out.structured = json!({ "tables": tables.iter().map(Table::to_json).collect::<Vec<_>>() });
    out.warnings = warnings;
    Ok(out)
}

fn file_stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Sheet".to_string())
}

/// Reads a delimited file as UTF-8, falling back to Windows-1252 (Excel's
/// usual CSV export) when the bytes are not UTF-8.
fn read_text(path: &Path, warnings: &mut Vec<String>) -> anyhow::Result<String> {
    let file = std::fs::File::open(path).context("cannot open the file")?;
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let mut bytes = Vec::new();
    file.take(MAX_CSV_BYTES)
        .read_to_end(&mut bytes)
        .context("cannot read the file")?;
    if len > MAX_CSV_BYTES {
        warnings.push(format!(
            "The file is {} MB; only the first {} MB were read.",
            len / (1024 * 1024),
            MAX_CSV_BYTES / (1024 * 1024)
        ));
    }
    if bytes.contains(&0) {
        bail!("the file looks binary, not delimited text");
    }
    let text = match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) => {
            let bytes = e.into_bytes();
            // A truncated read can cut a UTF-8 sequence: accept that tail.
            match std::str::from_utf8(&bytes) {
                Err(err) if err.valid_up_to() + 4 >= bytes.len() && len > MAX_CSV_BYTES => {
                    String::from_utf8_lossy(&bytes[..err.valid_up_to()]).into_owned()
                }
                _ => {
                    warnings.push("The file is not UTF-8; it was read as Windows-1252.".into());
                    bytes.iter().map(|&b| cp1252(b)).collect()
                }
            }
        }
    };
    Ok(text
        .strip_prefix('\u{feff}')
        .map(str::to_string)
        .unwrap_or(text))
}

fn cp1252(b: u8) -> char {
    match b {
        0x80 => '€',
        0x8A => 'Š',
        0x8C => 'Œ',
        0x8E => 'Ž',
        0x91 | 0x92 => '\'',
        0x93 | 0x94 => '"',
        0x96 => '–',
        0x97 => '—',
        0x9A => 'š',
        0x9C => 'œ',
        0x9E => 'ž',
        0x9F => 'Ÿ',
        _ => b as char,
    }
}

/// Picks `;`, `,` or tab by counting each outside quotes on the first line.
fn sniff_delimiter(text: &str) -> char {
    let mut counts = [(',', 0usize), (';', 0), ('\t', 0)];
    let mut in_quotes = false;
    for c in text.chars() {
        match c {
            '"' => in_quotes = !in_quotes,
            '\n' if !in_quotes => break,
            _ if !in_quotes => {
                for (d, n) in counts.iter_mut() {
                    if *d == c {
                        *n += 1;
                    }
                }
            }
            _ => {}
        }
    }
    let mut best = counts[0];
    for &(d, n) in &counts[1..] {
        if n > best.1 {
            best = (d, n);
        }
    }
    best.0
}

/// Splits delimited text into rows of fields. Quoted fields may hold the
/// delimiter, newlines and doubled quotes.
fn parse_delimited(text: &str, delim: char) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut started = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    field.push('"');
                    chars.next();
                } else {
                    in_quotes = false;
                }
            } else {
                field.push(c);
            }
            continue;
        }
        match c {
            '"' if field.trim().is_empty() => {
                field.clear();
                in_quotes = true;
                started = true;
            }
            '\r' => {}
            '\n' => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
                started = false;
            }
            _ if c == delim => {
                row.push(std::mem::take(&mut field));
                started = true;
            }
            _ => {
                field.push(c);
                started = true;
            }
        }
    }
    if started || !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows
}

#[derive(Clone, Debug, PartialEq)]
enum Cell {
    Empty,
    Int(i64),
    Num(f64),
    Bool(bool),
    Text(String),
}

impl Cell {
    fn from_text(s: String) -> Self {
        if s.trim().is_empty() {
            Cell::Empty
        } else {
            Cell::Text(s)
        }
    }

    fn from_data(d: &Data) -> Self {
        match d {
            Data::Empty => Cell::Empty,
            Data::Int(i) => Cell::Int(*i),
            Data::Float(f) => Cell::Num(*f),
            Data::Bool(b) => Cell::Bool(*b),
            Data::String(s) => Cell::from_text(s.clone()),
            Data::DateTime(dt) => datetime_cell(dt),
            Data::DateTimeIso(s) | Data::DurationIso(s) => Cell::Text(s.clone()),
            Data::Error(e) => Cell::Text(e.to_string()),
        }
    }

    fn is_empty(&self) -> bool {
        matches!(self, Cell::Empty)
    }

    fn display(&self) -> String {
        match self {
            Cell::Empty => String::new(),
            Cell::Int(i) => i.to_string(),
            Cell::Num(f) => fmt_num(*f),
            Cell::Bool(b) => b.to_string(),
            Cell::Text(s) => s.clone(),
        }
    }

    fn to_json(&self) -> Value {
        match self {
            Cell::Empty => Value::Null,
            Cell::Int(i) => json!(i),
            Cell::Num(f) => num_json(*f),
            Cell::Bool(b) => json!(b),
            Cell::Text(s) => json!(s),
        }
    }

    fn number(&self, dec: char) -> Option<f64> {
        match self {
            Cell::Int(i) => Some(*i as f64),
            Cell::Num(f) if f.is_finite() => Some(*f),
            Cell::Text(s) => parse_number(s, dec),
            _ => None,
        }
    }
}

/// calamine keeps the date kind private without its `dates` feature, so it
/// is recovered by comparing against rebuilt values.
fn datetime_cell(dt: &ExcelDateTime) -> Cell {
    let v = dt.as_f64();
    let is = |kind, is_1904| *dt == ExcelDateTime::new(v, kind, is_1904);
    if is(ExcelDateTimeType::TimeDelta, false) || is(ExcelDateTimeType::TimeDelta, true) {
        Cell::Num(v)
    } else if is(ExcelDateTimeType::DateTime, true) {
        Cell::Text(excel_date(v + 1462.0))
    } else {
        Cell::Text(excel_date(v))
    }
}

/// An Excel serial date (1900 system) as `YYYY-MM-DD[ HH:MM:SS]`.
fn excel_date(serial: f64) -> String {
    if !serial.is_finite() || !(0.0..3_000_000.0).contains(&serial) {
        return fmt_num(serial);
    }
    let days = serial.floor() as i64 - 25_569; // 1899-12-30 → 1970-01-01
    let secs = ((serial - serial.floor()) * 86_400.0).round() as i64;
    // Days-to-civil (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    if secs == 0 {
        format!("{y:04}-{m:02}-{d:02}")
    } else {
        let secs = secs.min(86_399);
        format!(
            "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
            secs / 3600,
            secs / 60 % 60,
            secs % 60
        )
    }
}

/// The decimal separator a value unambiguously uses, if any.
fn decimal_style(s: &str) -> Option<char> {
    let core = numeric_core(s)?.1;
    let dots = core.matches('.').count();
    let commas = core.matches(',').count();
    match (dots, commas) {
        (0, 0) => None,
        (_, 0) if dots > 1 => Some(','),
        (0, _) if commas > 1 => Some('.'),
        (1, 0) | (0, 1) => {
            let sep = if dots == 1 { '.' } else { ',' };
            let after = core.rsplit(sep).next().map(str::len).unwrap_or(0);
            (after != 3).then_some(sep)
        }
        _ => core
            .rfind(['.', ','])
            .and_then(|i| core[i..].chars().next()),
    }
}

/// Strips sign, currency and spacing; returns (negative, digits-and-separators).
fn numeric_core(s: &str) -> Option<(bool, String)> {
    let mut t = s.trim();
    let mut neg = false;
    if t.starts_with('(') && t.ends_with(')') && t.len() > 2 {
        neg = true;
        t = &t[1..t.len() - 1];
    }
    let mut t = t.trim().trim_end_matches('%').trim().to_string();
    for sym in [
        "€", "$", "£", "¥", "₹", "₩", "₽", "CHF", "EUR", "USD", "GBP", "DKK", "SEK", "NOK", "kr.",
        "kr", "Fr.",
    ] {
        if let Some(rest) = t.strip_prefix(sym) {
            t = rest.trim().to_string();
        }
        if let Some(rest) = t.strip_suffix(sym) {
            t = rest.trim().to_string();
        }
    }
    if let Some(rest) = t.strip_prefix('-').or_else(|| t.strip_prefix('−')) {
        neg = !neg;
        t = rest.to_string();
    } else if let Some(rest) = t.strip_prefix('+') {
        t = rest.to_string();
    }
    // A currency symbol after the sign (`-€ 5`).
    for sym in ["€", "$", "£"] {
        if let Some(rest) = t.strip_prefix(sym) {
            t = rest.to_string();
        }
    }
    let core: String = t
        .chars()
        .filter(|c| !matches!(c, ' ' | '\u{a0}' | '\u{202f}' | '\u{2009}' | '\''))
        .collect();
    if core.is_empty()
        || !core.chars().any(|c| c.is_ascii_digit())
        || !core
            .chars()
            .all(|c| c.is_ascii_digit() || c == '.' || c == ',')
        || !core.starts_with(|c: char| c.is_ascii_digit() || c == '.' || c == ',')
    {
        return None;
    }
    Some((neg, core))
}

/// Parses `1.234,56`, `1,234.56`, `€ 12,30`, `(5.00)`, `12%`… `dec` breaks
/// ties for values like `1,234` whose separator is ambiguous.
fn parse_number(s: &str, dec: char) -> Option<f64> {
    let (neg, core) = numeric_core(s)?;
    let dec = decimal_style(s).unwrap_or(dec);
    let thousands = if dec == ',' { '.' } else { ',' };
    let (int_part, frac_part) = match core.rfind(dec) {
        Some(i) => (&core[..i], Some(&core[i + 1..])),
        None => (core.as_str(), None),
    };
    if let Some(f) = frac_part {
        if f.contains(['.', ',']) || (f.is_empty() && int_part.is_empty()) {
            return None;
        }
    }
    if int_part.contains(dec) {
        return None;
    }
    let groups: Vec<&str> = int_part.split(thousands).collect();
    if groups.len() > 1 {
        let first_ok = (1..=3).contains(&groups[0].len());
        if !first_ok || groups[1..].iter().any(|g| g.len() != 3) {
            return None;
        }
    }
    let mut plain: String = groups.concat();
    if plain.is_empty() {
        plain.push('0');
    }
    if let Some(f) = frac_part {
        plain.push('.');
        plain.push_str(f);
    }
    let v: f64 = plain.parse().ok()?;
    Some(if neg { -v } else { v })
}

fn num_json(f: f64) -> Value {
    if !f.is_finite() {
        Value::Null
    } else if f.fract() == 0.0 && f.abs() < 9.0e15 {
        json!(f as i64)
    } else {
        json!(f)
    }
}

/// Rounds away float noise (`0.1 + 0.2`) for stats.
fn tidy(f: f64) -> f64 {
    if f.abs() < 1.0e12 {
        (f * 1.0e6).round() / 1.0e6
    } else {
        f
    }
}

fn fmt_num(f: f64) -> String {
    if f.fract() == 0.0 && f.abs() < 9.0e15 {
        format!("{}", f as i64)
    } else {
        format!("{}", tidy(f))
    }
}

#[derive(Clone, Debug)]
struct Stats {
    sum: f64,
    min: f64,
    max: f64,
    count: usize,
}

struct Table {
    name: String,
    columns: Vec<String>,
    rows: Vec<Vec<Cell>>,
    stats: Vec<(String, Stats)>,
}

/// Header = first non-empty row; data = the non-empty rows after it.
fn build_table(
    name: &str,
    rows: Vec<Vec<Cell>>,
    default_dec: char,
    warnings: &mut Vec<String>,
) -> Option<Table> {
    let mut rows = rows
        .into_iter()
        .filter(|r| r.iter().any(|c| !c.is_empty()))
        .map(|mut r| {
            while r.last().is_some_and(Cell::is_empty) {
                r.pop();
            }
            r
        });
    let header = rows.next()?;
    let data: Vec<Vec<Cell>> = rows.collect();
    let full_width = data
        .iter()
        .map(Vec::len)
        .chain([header.len()])
        .max()
        .unwrap_or(0);
    let width = full_width.min(MAX_COLUMNS);
    if full_width > MAX_COLUMNS {
        warnings.push(format!(
            "Sheet {name} has {full_width} columns; only the first {MAX_COLUMNS} were read."
        ));
    }
    if data.len() > JSON_ROWS {
        warnings.push(format!(
            "Sheet {name} has {} rows; the first {JSON_ROWS} are in the table data.",
            data.len()
        ));
    }

    let mut seen = HashSet::new();
    let columns: Vec<String> = (0..width)
        .map(|i| {
            let raw = header.get(i).map(Cell::display).unwrap_or_default();
            let base = match raw.trim() {
                "" => format!("Column {}", i + 1),
                t => t.to_string(),
            };
            let mut col = base.clone();
            let mut n = 2;
            while !seen.insert(col.clone()) {
                col = format!("{base} ({n})");
                n += 1;
            }
            col
        })
        .collect();

    let mut votes = (0usize, 0usize);
    for cell in data.iter().flatten() {
        if let Cell::Text(s) = cell {
            match decimal_style(s) {
                Some('.') => votes.0 += 1,
                Some(',') => votes.1 += 1,
                _ => {}
            }
        }
    }
    let dec = match votes {
        (d, c) if d > c => '.',
        (d, c) if c > d => ',',
        _ => default_dec,
    };

    let mut stats = Vec::new();
    for (i, col) in columns.iter().enumerate() {
        let cells: Vec<&Cell> = data
            .iter()
            .filter_map(|r| r.get(i))
            .filter(|c| !c.is_empty())
            .collect();
        let nums: Vec<f64> = cells.iter().filter_map(|c| c.number(dec)).collect();
        if nums.is_empty() || nums.len() * 5 < cells.len() * 4 {
            continue;
        }
        let s = nums.iter().fold(
            Stats {
                sum: 0.0,
                min: f64::INFINITY,
                max: f64::NEG_INFINITY,
                count: 0,
            },
            |mut s, &v| {
                s.sum += v;
                s.min = s.min.min(v);
                s.max = s.max.max(v);
                s.count += 1;
                s
            },
        );
        stats.push((col.clone(), s));
    }

    let rows = data
        .into_iter()
        .map(|mut r| {
            r.truncate(width);
            r
        })
        .collect();
    Some(Table {
        name: name.to_string(),
        columns,
        rows,
        stats,
    })
}

impl Table {
    fn to_json(&self) -> Value {
        let width = self.columns.len();
        let rows: Vec<Value> = self
            .rows
            .iter()
            .take(JSON_ROWS)
            .map(|r| {
                Value::Array(
                    (0..width)
                        .map(|i| r.get(i).map(Cell::to_json).unwrap_or(Value::Null))
                        .collect(),
                )
            })
            .collect();
        let mut stats = Map::new();
        for (col, s) in &self.stats {
            stats.insert(
                col.clone(),
                json!({
                    "sum": num_json(tidy(s.sum)),
                    "min": num_json(s.min),
                    "max": num_json(s.max),
                    "count": s.count,
                }),
            );
        }
        json!({
            "name": self.name,
            "columns": self.columns,
            "rows": rows,
            "row_count": self.rows.len(),
            "stats": stats,
        })
    }

    fn to_text(&self) -> String {
        let clean = |s: String| s.replace(['\t', '\n', '\r'], " ");
        let mut out = format!("## {} ({} rows)\n", self.name, self.rows.len());
        out.push_str(
            &self
                .columns
                .iter()
                .cloned()
                .map(clean)
                .collect::<Vec<_>>()
                .join("\t"),
        );
        out.push('\n');
        for r in self.rows.iter().take(TEXT_ROWS) {
            let line: Vec<String> = (0..self.columns.len())
                .map(|i| clean(r.get(i).map(Cell::display).unwrap_or_default()))
                .collect();
            out.push_str(line.join("\t").trim_end());
            out.push('\n');
        }
        if self.rows.len() > TEXT_ROWS {
            out.push_str(&format!("… {} more rows\n", self.rows.len() - TEXT_ROWS));
        }
        for (col, s) in &self.stats {
            out.push_str(&format!(
                "{col}: sum {}, min {}, max {}, count {}\n",
                fmt_num(tidy(s.sum)),
                fmt_num(s.min),
                fmt_num(s.max),
                s.count
            ));
        }
        out.trim_end().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write(dir: &tempfile::TempDir, name: &str, body: &[u8]) -> std::path::PathBuf {
        let p = dir.path().join(name);
        std::fs::write(&p, body).unwrap();
        p
    }

    fn table(out: &Extracted, i: usize) -> &Value {
        &out.structured["tables"][i]
    }

    #[test]
    fn numbers_in_local_formats() {
        assert_eq!(parse_number("1.234,56", '.'), Some(1234.56));
        assert_eq!(parse_number("1,234.56", ','), Some(1234.56));
        assert_eq!(parse_number("€ 12,30", '.'), Some(12.3));
        assert_eq!(parse_number("12,30 €", '.'), Some(12.3));
        assert_eq!(parse_number("-€5.00", '.'), Some(-5.0));
        assert_eq!(parse_number("(7.50)", '.'), Some(-7.5));
        assert_eq!(parse_number("1 234,5", '.'), Some(1234.5));
        assert_eq!(parse_number("1,234", '.'), Some(1234.0));
        assert_eq!(parse_number("1,234", ','), Some(1.234));
        assert_eq!(parse_number("1.234.567", '.'), Some(1_234_567.0));
        assert_eq!(parse_number("42", '.'), Some(42.0));
        assert_eq!(parse_number("12%", '.'), Some(12.0));
        assert_eq!(parse_number("12.03.2024", '.'), None);
        assert_eq!(parse_number("2024-01-05", '.'), None);
        assert_eq!(parse_number("1.2.3", '.'), None);
        assert_eq!(parse_number("abc", '.'), None);
        assert_eq!(parse_number("", '.'), None);
    }

    #[test]
    fn semicolon_csv_with_european_decimals() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(
            &dir,
            "spese.csv",
            "Data;Descrizione;Importo\n01/10/2026;Pane;1,50\n02/10/2026;Latte;0,99\n03/10/2026;Spesa grande;1.234,56\n".as_bytes(),
        );
        let out = read_sheet(&p, "csv").unwrap();
        assert_eq!(out.kind, "sheet");
        assert_eq!(out.title.as_deref(), Some("spese"));
        let t = table(&out, 0);
        assert_eq!(t["columns"], json!(["Data", "Descrizione", "Importo"]));
        assert_eq!(t["row_count"], 3);
        assert_eq!(t["rows"][1], json!(["02/10/2026", "Latte", "0,99"]));
        let s = &t["stats"]["Importo"];
        assert_eq!(s["sum"], json!(1237.05));
        assert_eq!(s["min"], json!(0.99));
        assert_eq!(s["max"], json!(1234.56));
        assert_eq!(s["count"], 3);
        assert!(t["stats"].get("Data").is_none());
        assert!(out
            .text
            .starts_with("## spese (3 rows)\nData\tDescrizione\tImporto\n"));
        assert!(out
            .text
            .contains("Importo: sum 1237.05, min 0.99, max 1234.56, count 3"));
        assert!(out.warnings.is_empty(), "{:?}", out.warnings);
    }

    #[test]
    fn quoted_fields_with_commas_and_newlines() {
        let dir = tempfile::tempdir().unwrap();
        let body = "name,notes,amount\r\n\"Rossi, Mario\",\"line one\nline two\",\"1,200.50\"\r\nBianchi,\"said \"\"hi\"\"\",300\r\n\r\n";
        let p = write(&dir, "people.csv", body.as_bytes());
        let out = read_sheet(&p, "csv").unwrap();
        let t = table(&out, 0);
        assert_eq!(t["row_count"], 2);
        assert_eq!(
            t["rows"][0],
            json!(["Rossi, Mario", "line one\nline two", "1,200.50"])
        );
        assert_eq!(t["rows"][1], json!(["Bianchi", "said \"hi\"", "300"]));
        assert_eq!(t["stats"]["amount"]["sum"], json!(1500.5));
        // The TSV preview keeps one row per line.
        assert!(out
            .text
            .contains("Rossi, Mario\tline one line two\t1,200.50"));
    }

    #[test]
    fn tsv_and_sniffed_tab_csv() {
        let dir = tempfile::tempdir().unwrap();
        let body = "item\tqty\tprice\napple\t3\t0.5\npear\t2\t0.75\nkiwi\t\t\n";
        for (name, ext) in [("a.tsv", "tsv"), ("b.csv", "csv")] {
            let p = write(&dir, name, body.as_bytes());
            let out = read_sheet(&p, ext).unwrap();
            let t = table(&out, 0);
            assert_eq!(t["columns"], json!(["item", "qty", "price"]));
            assert_eq!(t["row_count"], 3);
            assert_eq!(t["rows"][2], json!(["kiwi", null, null]));
            assert_eq!(t["stats"]["qty"]["sum"], json!(5));
            assert_eq!(t["stats"]["price"]["sum"], json!(1.25));
            assert_eq!(t["stats"]["price"]["count"], 2);
        }
    }

    #[test]
    fn mostly_text_column_is_not_numeric() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(&dir, "m.csv", b"code,v\n1,1\n2,x\nA,y\nB,z\n");
        let out = read_sheet(&p, "csv").unwrap();
        let t = table(&out, 0);
        assert!(t["stats"].get("code").is_none());
        assert!(t["stats"].get("v").is_none());
    }

    #[test]
    fn header_skips_blank_lines_and_names_columns() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(&dir, "h.csv", b"\n,,\na,,a\n1,2,3\n");
        let out = read_sheet(&p, "csv").unwrap();
        let t = table(&out, 0);
        assert_eq!(t["columns"], json!(["a", "Column 2", "a (2)"]));
        assert_eq!(t["stats"]["a (2)"]["sum"], json!(3));
    }

    #[test]
    fn long_csv_is_truncated_with_warning() {
        let dir = tempfile::tempdir().unwrap();
        let mut body = String::from("n\n");
        for i in 1..=250 {
            body.push_str(&format!("{i}\n"));
        }
        let p = write(&dir, "long.csv", body.as_bytes());
        let out = read_sheet(&p, "csv").unwrap();
        let t = table(&out, 0);
        assert_eq!(t["row_count"], 250);
        assert_eq!(t["rows"].as_array().unwrap().len(), 200);
        assert_eq!(t["stats"]["n"]["sum"], json!(31375));
        assert!(out.warnings.iter().any(|w| w.contains("250 rows")));
        assert!(out.text.contains("… 200 more rows"));
    }

    #[test]
    fn wide_csv_caps_columns() {
        let dir = tempfile::tempdir().unwrap();
        let header: Vec<String> = (0..120).map(|i| format!("c{i}")).collect();
        let body = format!("{}\n{}\n", header.join(","), ["1"; 120].join(","));
        let p = write(&dir, "wide.csv", body.as_bytes());
        let out = read_sheet(&p, "csv").unwrap();
        assert_eq!(table(&out, 0)["columns"].as_array().unwrap().len(), 100);
        assert!(out.warnings.iter().any(|w| w.contains("120 columns")));
    }

    #[test]
    fn latin1_csv_is_decoded() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(&dir, "l.csv", b"Citt\xe0;Prezzo\nRoma;\x80 3,50\n");
        let out = read_sheet(&p, "csv").unwrap();
        let t = table(&out, 0);
        assert_eq!(t["columns"], json!(["Città", "Prezzo"]));
        assert_eq!(t["stats"]["Prezzo"]["sum"], json!(3.5));
        assert!(out.warnings.iter().any(|w| w.contains("Windows-1252")));
    }

    #[test]
    fn empty_csv_warns() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(&dir, "e.csv", b"");
        let out = read_sheet(&p, "csv").unwrap();
        assert_eq!(out.structured["tables"], json!([]));
        assert!(!out.warnings.is_empty());
    }

    #[test]
    fn excel_dates() {
        assert_eq!(excel_date(45_658.0), "2025-01-01");
        assert_eq!(excel_date(45_658.5), "2025-01-01 12:00:00");
    }

    /// Writes a minimal xlsx by hand: inline strings and numbers.
    fn tiny_xlsx(path: &Path) {
        let file = std::fs::File::create(path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        let parts: [(&str, &str); 5] = [
            (
                "[Content_Types].xml",
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#,
            ),
            (
                "_rels/.rels",
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#,
            ),
            (
                "xl/workbook.xml",
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Budget" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>
<row r="1"><c r="A1" t="inlineStr"><is><t>Item</t></is></c><c r="B1" t="inlineStr"><is><t>Cost</t></is></c></row>
<row r="2"><c r="A2" t="inlineStr"><is><t>Rent</t></is></c><c r="B2"><v>950</v></c></row>
<row r="3"><c r="A3" t="inlineStr"><is><t>Food</t></is></c><c r="B3"><v>312.4</v></c></row>
<row r="4"><c r="A4" t="inlineStr"><is><t>Phone</t></is></c><c r="B4"><v>15.6</v></c></row>
</sheetData></worksheet>"#,
            ),
        ];
        for (name, body) in parts {
            zip.start_file(name, opts).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }

    #[test]
    fn hand_built_xlsx_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("budget.xlsx");
        tiny_xlsx(&p);
        let out = read_sheet(&p, "xlsx").unwrap();
        assert_eq!(out.title.as_deref(), Some("Budget"));
        assert_eq!(out.subtype.as_deref(), Some("xlsx"));
        let t = table(&out, 0);
        assert_eq!(t["name"], "Budget");
        assert_eq!(t["columns"], json!(["Item", "Cost"]));
        assert_eq!(t["row_count"], 3);
        assert_eq!(t["rows"][0], json!(["Rent", 950]));
        assert_eq!(t["stats"]["Cost"]["sum"], json!(1278));
        assert_eq!(t["stats"]["Cost"]["min"], json!(15.6));
        assert_eq!(t["stats"]["Cost"]["max"], json!(950));
        assert!(out
            .text
            .starts_with("## Budget (3 rows)\nItem\tCost\nRent\t950\n"));
        assert!(out
            .text
            .contains("Cost: sum 1278, min 15.6, max 950, count 3"));
    }

    #[test]
    fn broken_workbook_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(&dir, "bad.xlsx", b"not a zip");
        assert!(read_sheet(&p, "xlsx").is_err());
    }
}
