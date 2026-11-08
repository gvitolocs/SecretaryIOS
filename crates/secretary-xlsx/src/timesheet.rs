//! The AU worktime registration template (`AU_worktime_registration_2026__v1.4.xlsx`): a form
//! over its input cells and the month totals, computed in Rust with the template's own rules
//! (columns G, L, M and R of each month sheet, Masterdata C14:D27).
//!
//! Excel recomputes the real formulas when the file opens (`fullCalcOnLoad`); these numbers are
//! what the app shows meanwhile. `tests/timesheet.rs` pins them against the template's cached
//! results and against a LibreOffice recalculation of a filled month (docs/xlsx.md).
//!
//! The layout is found by the template's English labels, not by fixed cells, so a later version
//! that moves rows keeps working; a workbook without them is not a timesheet.

use chrono::{Datelike, NaiveDate, Weekday};
use serde::Serialize;

use crate::numfmt::{hhmm, serial_to_datetime, FmtKind};
use crate::read::{Sheet, Value, Workbook};
use crate::{cellref, numfmt};

#[derive(Debug, Clone, Serialize)]
pub struct Field {
    pub key: &'static str,
    pub label: String,
    pub sheet: String,
    pub cell: String,
    /// `date`, `time`, `duration`, `number`, `text`, `bool`: how the app asks for it.
    pub kind: &'static str,
    /// As the cell shows it (dates as `yyyy-mm-dd`, times as `hh:mm`).
    pub value: String,
    pub editable: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Day {
    pub row: u32,
    pub date: String,
    pub weekday: String,
    pub holiday: Option<String>,
    /// Monday to Friday and not a holiday.
    pub workday: bool,
    /// Inside the employment dates (Masterdata start/end).
    pub employed: bool,
    pub code: String,
    /// `hh:mm`, empty when zero.
    pub actual: String,
    pub start: String,
    pub end: String,
    pub remarks: String,
    /// The day's standard hours (column G).
    pub norm: String,
    /// Worked hours (column L).
    pub worked: String,
    /// Difference (column M).
    pub diff: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Columns {
    pub code: String,
    pub actual: String,
    pub start: String,
    pub end: String,
    pub remarks: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Month {
    pub sheet: String,
    pub month: u32,
    pub year: i32,
    pub weekly_hours: Field,
    pub day_off: Option<Field>,
    pub columns: Columns,
    /// The codes the Code column accepts now (the template's list validation).
    pub codes: Vec<String>,
    pub days: Vec<Day>,
    pub norm: String,
    pub worked: String,
    pub diff: String,
    /// Days with a code other than N.
    pub registered_days: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Timesheet {
    pub kind: &'static str,
    pub master_sheet: String,
    pub master: Vec<Field>,
    pub months: Vec<Month>,
    pub balance_carried: String,
    /// The year's difference without the carried balance.
    pub diff: String,
    /// "Total difference incl. balance from previous period".
    pub total: String,
}

const WEEKDAYS: [&str; 7] = [
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];

fn text_of(v: &Value) -> String {
    match v {
        Value::Text(s) => s.clone(),
        Value::Number(n) => numfmt::general(*n),
        Value::Bool(b) => if *b { "TRUE" } else { "FALSE" }.into(),
        Value::Error(e) => e.clone(),
        Value::Empty => String::new(),
    }
}

fn num_of(v: Option<&Value>) -> f64 {
    match v {
        Some(Value::Number(n)) => *n,
        Some(Value::Bool(b)) => f64::from(u8::from(*b)),
        Some(Value::Text(s)) => s.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

fn cell_value(sheet: &Sheet, row: u32, col: u32) -> Option<&Value> {
    sheet.cells.get(&(row, col)).map(|c| &c.value)
}

/// The first cell whose text starts with `label` (case-insensitive), as (row, col).
fn find_label(sheet: &Sheet, label: &str) -> Option<(u32, u32)> {
    let l = label.to_lowercase();
    sheet
        .cells
        .values()
        .find(|c| matches!(&c.value, Value::Text(t) if t.trim().to_lowercase().starts_with(&l)))
        .map(|c| (c.row, c.col))
}

fn field(wb: &Workbook, sheet: &Sheet, key: &'static str, label_cell: (u32, u32)) -> Field {
    let (r, c) = (label_cell.0, label_cell.1 + 1);
    let label = text_of(cell_value(sheet, label_cell.0, label_cell.1).unwrap_or(&Value::Empty));
    let cell = sheet.cells.get(&(r, c));
    let style = cell.map_or_else(|| wb.default_style(sheet, r, c), |x| x.style);
    let fmt = wb.style(style).kind.unwrap_or(FmtKind::General);
    let v = cell.map_or(Value::Empty, |x| x.value.clone());
    let kind: &'static str = match (&v, fmt) {
        (Value::Bool(_), _) => "bool",
        (_, FmtKind::Date | FmtKind::DateTime) => "date",
        (_, FmtKind::Time) => "time",
        (_, FmtKind::Duration) => "duration",
        (Value::Number(_), _) | (_, FmtKind::Number) => "number",
        _ => "text",
    };
    let value = match (&v, kind) {
        (Value::Number(n), "date") if *n == 0.0 => String::new(),
        (Value::Number(n), "date") => serial_to_datetime(*n, wb.date1904)
            .map(|d| d.date().to_string())
            .unwrap_or_default(),
        (Value::Number(n), "time" | "duration") => hhmm(*n),
        (Value::Bool(b), _) => if *b { "yes" } else { "no" }.into(),
        (v, _) => text_of(v),
    };
    Field {
        key,
        label,
        sheet: sheet.name.clone(),
        cell: cellref::name(r, c),
        kind,
        value,
        editable: wb.editable(sheet, r, c),
    }
}

/// `hh:mm` or `-hh:mm` text (the template's balance cell), or a time number, as hours.
fn hours_of(v: Option<&Value>) -> f64 {
    match v {
        Some(Value::Number(n)) => n * 24.0,
        Some(Value::Text(s)) => {
            let t = s.trim();
            let (neg, t) = t.strip_prefix('-').map_or((false, t), |r| (true, r));
            let h = crate::patch::parse_time(t, true).map_or(0.0, |d| d * 24.0);
            if neg {
                -h
            } else {
                h
            }
        }
        _ => 0.0,
    }
}

fn blank_zero(v: Option<&Value>) -> String {
    match v {
        Some(Value::Number(n)) if *n != 0.0 => hhmm(*n),
        Some(Value::Text(s)) => s.clone(),
        _ => String::new(),
    }
}

/// Excel's TEXT(x, "[h]:mm") read back as hours: rounded to the minute.
fn minute_hours(days: f64) -> f64 {
    (days * 1440.0).round() / 60.0
}

struct MonthLayout<'a> {
    sheet: &'a Sheet,
    header: u32,
    cols: [u32; 6],
}

// Column slots in MonthLayout::cols.
const DATE: usize = 0;
const CODE: usize = 1;
const ACTUAL: usize = 2;
const START: usize = 3;
const END: usize = 4;
const REMARKS: usize = 5;

fn month_layout(sheet: &Sheet) -> Option<MonthLayout<'_>> {
    // The header row is the one with both "Code" and "Date" (the legend above has "Code" too).
    let is = |c: &crate::read::Cell, label: &str| matches!(&c.value, Value::Text(t) if t.trim().eq_ignore_ascii_case(label));
    let (header, code) = sheet
        .cells
        .values()
        .filter(|c| is(c, "Code"))
        .find(|c| {
            sheet
                .cells
                .range((c.row, 1)..=(c.row, cellref::MAX_COL))
                .any(|(_, d)| is(d, "Date"))
        })
        .map(|c| (c.row, c.col))?;
    let col_of = |label: &str| {
        sheet
            .cells
            .range((header, 1)..=(header, cellref::MAX_COL))
            .find(
                |(_, c)| matches!(&c.value, Value::Text(t) if t.trim().eq_ignore_ascii_case(label)),
            )
            .map(|(k, _)| k.1)
    };
    Some(MonthLayout {
        sheet,
        header,
        cols: [
            col_of("Date")?,
            code,
            col_of("Actual hours worked")?,
            col_of("Start")?,
            col_of("End")?,
            col_of("Remarks")?,
        ],
    })
}

struct RawDay {
    row: u32,
    date: NaiveDate,
    code: String,
}

fn raw_days(wb: &Workbook, l: &MonthLayout) -> Vec<RawDay> {
    let mut out = Vec::new();
    let mut month: Option<u32> = None;
    for row in l.header + 1..l.header + 40 {
        let Some(Value::Number(serial)) = cell_value(l.sheet, row, l.cols[DATE]) else {
            break;
        };
        let Some(dt) = serial_to_datetime(*serial, wb.date1904) else {
            break;
        };
        let d = dt.date();
        if month.is_some_and(|m| m != d.month()) {
            break;
        }
        month = Some(d.month());
        let code = text_of(cell_value(l.sheet, row, l.cols[CODE]).unwrap_or(&Value::Empty))
            .trim()
            .to_string();
        out.push(RawDay { row, date: d, code });
    }
    out
}

pub fn detect(wb: &Workbook) -> Option<Timesheet> {
    let master = wb
        .sheets
        .iter()
        .find(|s| find_label(s, "Enter your AU ID").is_some())?;
    let keys: [(&'static str, &str); 6] = [
        ("start_date", "Start date"),
        ("end_date", "End date"),
        ("year", "Year"),
        ("name", "Enter your first and last name"),
        ("au_id", "Enter your AU ID"),
        ("balance", "Any balance carried forward"),
    ];
    let mut fields = Vec::new();
    for (key, label) in keys {
        if let Some(at) = find_label(master, label) {
            fields.push(field(wb, master, key, at));
        }
    }
    let master_num = |key: &str| {
        keys.iter()
            .find(|(k, _)| *k == key)
            .and_then(|(_, l)| find_label(master, l))
            .and_then(|(r, c)| cell_value(master, r, c + 1).cloned())
    };
    let emp_start = num_of(master_num("start_date").as_ref());
    let emp_end = num_of(master_num("end_date").as_ref());
    let balance = hours_of(master_num("balance").as_ref());
    // The holiday table: names in one column, dates next to them, under "Holiday".
    let mut holidays: Vec<(f64, String)> = Vec::new();
    if let Some((hr, hc)) = find_label(master, "Holiday") {
        for row in hr + 1..hr + 40 {
            match (cell_value(master, row, hc), cell_value(master, row, hc + 1)) {
                (Some(Value::Text(name)), Some(Value::Number(d))) => {
                    holidays.push((*d, name.clone()))
                }
                _ => break,
            }
        }
    }

    // Month sheets in workbook order, with their days.
    let layouts: Vec<(MonthLayout, Vec<RawDay>)> = wb
        .sheets
        .iter()
        .filter(|s| s.name != master.name)
        .filter_map(month_layout)
        .map(|l| {
            let d = raw_days(wb, &l);
            (l, d)
        })
        .filter(|(_, d)| !d.is_empty())
        .collect();
    if layouts.is_empty() {
        return None;
    }

    let mut months = Vec::new();
    let mut year_diff = 0.0;
    for (i, (l, days)) in layouts.iter().enumerate() {
        let sheet = l.sheet;
        let first = days[0].date;
        let weekly_at = find_label(sheet, "Standard work hours").filter(|(r, _)| *r < l.header);
        let weekly = weekly_at.map(|at| field(wb, sheet, "weekly_hours", at));
        let weekly_hours = weekly_at.map_or(0.0, |(r, c)| num_of(cell_value(sheet, r, c + 1)));
        let x7 = weekly_hours / 24.0;
        let day_off = find_label(sheet, "Do you have a weekly day off")
            .map(|at| field(wb, sheet, "day_off", at));
        // F days off on weekdays, by ISO week, in this month and the ones next to it (column H).
        let mut neighbours: Vec<&RawDay> = days.iter().collect();
        if i > 0 {
            neighbours.extend(layouts[i - 1].1.iter());
        }
        if i + 1 < layouts.len() {
            neighbours.extend(layouts[i + 1].1.iter());
        }
        let off_in_week = |d: NaiveDate| {
            neighbours
                .iter()
                .filter(|o| {
                    o.date.iso_week() == d.iso_week()
                        && o.date.weekday().number_from_monday() <= 5
                        && o.code.eq_ignore_ascii_case("F")
                })
                .count() as f64
        };
        let codes = sheet
            .validations
            .iter()
            .find(|v| v.kind == "list" && cellref::in_sqref(&v.sqref, days[0].row, l.cols[CODE]))
            .and_then(|v| v.formula1.as_deref())
            .and_then(|f| {
                let si = wb.sheets.iter().position(|s| s.name == sheet.name)?;
                crate::patch::list_choices(wb, &Default::default(), si, f, 0)
            })
            .unwrap_or_else(|| vec!["N".into(), "R".into(), "F".into()]);

        let (mut norm_t, mut worked_t, mut diff_h) = (0.0, 0.0, 0.0);
        let mut out_days = Vec::new();
        for d in days {
            let serial = numfmt::date_to_serial(d.date, wb.date1904);
            let holiday = holidays
                .iter()
                .find(|(h, _)| (*h - serial).abs() < 0.5)
                .map(|(_, n)| n.clone());
            let wd = d.date.weekday();
            let workday = holiday.is_none() && !matches!(wd, Weekday::Sat | Weekday::Sun);
            let employed =
                !(emp_end != 0.0 && emp_end < serial) && !(emp_start != 0.0 && emp_start > serial);
            let code = d.code.to_uppercase();
            let act = num_of(cell_value(sheet, d.row, l.cols[ACTUAL]));
            let (st, en) = (
                num_of(cell_value(sheet, d.row, l.cols[START])),
                num_of(cell_value(sheet, d.row, l.cols[END])),
            );
            let h = off_in_week(d.date);
            // G
            let g = if (code == "N" && !workday) || code == "F" || !employed || h >= 5.0 {
                0.0
            } else {
                x7 / (5.0 - h)
            };
            // L
            let lw = match code.as_str() {
                "F" => 0.0,
                "N" => g,
                _ if act != 0.0 => act,
                _ => en - st,
            };
            // M
            let m = if code == "N" || code == "F" || !employed {
                0.0
            } else if act > 0.0 {
                if workday {
                    act - g
                } else {
                    act
                }
            } else if workday {
                en - st - g
            } else {
                en - st
            };
            if workday {
                norm_t += g;
            }
            worked_t += lw;
            diff_h += minute_hours(m);
            out_days.push(Day {
                row: d.row,
                date: d.date.to_string(),
                weekday: WEEKDAYS[wd.num_days_from_monday() as usize].into(),
                holiday,
                workday,
                employed,
                code: d.code.clone(),
                actual: blank_zero(cell_value(sheet, d.row, l.cols[ACTUAL])),
                start: blank_zero(cell_value(sheet, d.row, l.cols[START])),
                end: blank_zero(cell_value(sheet, d.row, l.cols[END])),
                remarks: text_of(
                    cell_value(sheet, d.row, l.cols[REMARKS]).unwrap_or(&Value::Empty),
                ),
                norm: hhmm(g),
                worked: hhmm(lw),
                diff: hhmm(m),
            });
        }
        year_diff += diff_h;
        months.push(Month {
            sheet: sheet.name.clone(),
            month: first.month(),
            year: first.year(),
            weekly_hours: weekly.unwrap_or(Field {
                key: "weekly_hours",
                label: "Standard work hours".into(),
                sheet: sheet.name.clone(),
                cell: String::new(),
                kind: "number",
                value: String::new(),
                editable: false,
            }),
            day_off,
            columns: Columns {
                code: cellref::col_name(l.cols[CODE]),
                actual: cellref::col_name(l.cols[ACTUAL]),
                start: cellref::col_name(l.cols[START]),
                end: cellref::col_name(l.cols[END]),
                remarks: cellref::col_name(l.cols[REMARKS]),
            },
            codes,
            registered_days: out_days
                .iter()
                .filter(|d| !d.code.eq_ignore_ascii_case("N"))
                .count(),
            days: out_days,
            norm: hhmm(norm_t),
            worked: hhmm(worked_t),
            diff: hhmm(diff_h / 24.0),
        });
    }
    Some(Timesheet {
        kind: "au-worktime",
        master_sheet: master.name.clone(),
        master: fields,
        months,
        balance_carried: hhmm(balance / 24.0),
        diff: hhmm(year_diff / 24.0),
        total: hhmm((balance + year_diff) / 24.0),
    })
}
