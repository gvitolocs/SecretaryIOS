//! The workbook as the apps show it (`GET /v1/media/items/{id}/sheet`, docs/xlsx.md): sheets
//! with their visible cells, a style table, merges, sizes, validations and which cells are inputs.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{json, Value as Json};

use crate::cellref;
use crate::numfmt::FmtKind;
use crate::read::{Value, Workbook};

#[derive(Serialize)]
pub struct StyleOut {
    pub fmt: String,
    pub kind: FmtKind,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub bold: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub italic: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub underline: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fill: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub align: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub valign: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub wrap: bool,
    /// Top, right, bottom, left.
    pub border: [bool; 4],
}

/// Cells per sheet the view sends at most (the rest is counted in `truncated`).
pub const MAX_CELLS: usize = 60_000;
/// Formula text in the view is cut after this many characters.
pub const MAX_FORMULA: usize = 240;

pub fn workbook_json(wb: &Workbook) -> Json {
    let styles: Vec<StyleOut> = wb
        .styles
        .iter()
        .map(|s| StyleOut {
            fmt: s.num_fmt.clone(),
            kind: s.kind.unwrap_or(FmtKind::General),
            bold: s.bold,
            italic: s.italic,
            underline: s.underline,
            size: s.size,
            color: s.color.clone().filter(|c| c != "#000000"),
            fill: s.fill.clone(),
            align: s.h_align.clone(),
            valign: s.v_align.clone(),
            wrap: s.wrap,
            border: s.border,
        })
        .collect();
    let sheets: Vec<Json> = wb
        .sheets
        .iter()
        .enumerate()
        .map(|(si, sheet)| {
            let mut cells = Vec::new();
            let (mut max_r, mut max_c) = (0u32, 0u32);
            let mut truncated = 0usize;
            for c in sheet.cells.values() {
                let st = wb.style(c.style);
                let editable = wb.editable(sheet, c.row, c.col);
                let input = sheet.protected && editable;
                let has_value = c.value != Value::Empty;
                let decorated = st.fill.is_some() || st.border.iter().any(|b| *b);
                if !(has_value || c.formula.is_some() || input || decorated) {
                    continue;
                }
                max_r = max_r.max(c.row);
                max_c = max_c.max(c.col);
                if cells.len() >= MAX_CELLS {
                    truncated += 1;
                    continue;
                }
                let (t, v) = match &c.value {
                    Value::Empty => ("z", Json::Null),
                    Value::Number(n) => ("n", json!(n)),
                    Value::Text(s) => ("s", json!(s)),
                    Value::Bool(b) => ("b", json!(b)),
                    Value::Error(e) => ("e", json!(e)),
                };
                let mut o = json!({
                    "ref": cellref::name(c.row, c.col),
                    "r": c.row,
                    "c": c.col,
                    "s": c.style,
                    "t": t,
                    "v": v,
                    "display": wb.display(c),
                });
                if let Some(f) = &c.formula {
                    // Shown as an info line; the template's longest are 700 characters.
                    let f = if f.chars().count() > MAX_FORMULA {
                        format!("{}…", f.chars().take(MAX_FORMULA).collect::<String>())
                    } else {
                        f.clone()
                    };
                    o["f"] = json!(f);
                }
                if editable {
                    o["editable"] = json!(true);
                }
                if input {
                    o["input"] = json!(true);
                }
                cells.push(o);
            }
            // Editable cells a protected sheet has no element for still count as inputs; only
            // their extent matters to the grid, they render empty.
            let validations: Vec<Json> = sheet
                .validations
                .iter()
                .map(|v| {
                    let mut o = json!({
                        "sqref": v.sqref,
                        "type": v.kind,
                        "operator": v.operator,
                        "strict": v.show_error && v.error_style == "stop",
                    });
                    if v.kind == "list" {
                        if let Some(list) = v
                            .formula1
                            .as_deref()
                            .and_then(|f| crate::patch::list_choices(wb, &BTreeMap::new(), si, f, 0))
                        {
                            o["list"] = json!(list);
                        }
                    }
                    for (k, val) in [
                        ("formula1", &v.formula1),
                        ("formula2", &v.formula2),
                        ("prompt_title", &v.prompt_title),
                        ("prompt", &v.prompt),
                        ("error_title", &v.error_title),
                        ("error", &v.error),
                    ] {
                        if let Some(x) = val {
                            o[k] = json!(x);
                        }
                    }
                    o
                })
                .collect();
            for m in &sheet.merges {
                if let Some((_, _, r2, c2)) = cellref::parse_range(m) {
                    max_r = max_r.max(r2);
                    max_c = max_c.max(c2);
                }
            }
            json!({
                "index": si,
                "name": sheet.name,
                "state": sheet.state,
                "protected": sheet.protected,
                "max_row": max_r,
                "max_col": max_c,
                "frozen": sheet.frozen.map(|(r, c)| json!({"rows": r, "cols": c})),
                "default_col_width": sheet.default_col_width,
                "default_row_height": sheet.default_row_height,
                "cols": sheet.cols.iter().filter(|c| c.min <= max_c.max(1)).map(|c| json!({
                    "min": c.min, "max": c.max.min(max_c.max(c.min)), "width": c.width, "hidden": c.hidden,
                })).collect::<Vec<_>>(),
                "rows": sheet.rows.iter().filter(|r| r.row <= max_r && (r.height.is_some() || r.hidden)).map(|r| json!({
                    "r": r.row, "height": r.height, "hidden": r.hidden,
                })).collect::<Vec<_>>(),
                "merges": sheet.merges,
                "validations": validations,
                "cells": cells,
                "truncated": truncated,
            })
        })
        .collect();
    json!({
        "sheets": sheets,
        "styles": styles,
        "defined_names": wb.defined_names.iter().map(|d| json!({
            "name": d.name, "sheet": d.local_sheet, "hidden": d.hidden, "formula": d.formula,
        })).collect::<Vec<_>>(),
        "date1904": wb.date1904,
        "full_calc_on_load": wb.full_calc_on_load,
        "structure_protected": wb.structure_protected,
        "timesheet": crate::timesheet::detect(wb),
    })
}
