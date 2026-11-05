//! Reading a workbook: sheets, cells (value, formula, style), styles, merges, column widths,
//! data validations, protection and defined names. Nothing here writes; the patcher works on the
//! original text with the spans it finds itself.

use std::collections::{BTreeMap, HashMap};

use crate::cellref;
use crate::numfmt::{self, FmtKind};
use crate::package::{rels_of, resolve_target, Package};
use crate::xml::{self, attr, attrs, local, text_until_close, Scanner, Tok};
use crate::Error;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Empty,
    Number(f64),
    Text(String),
    Bool(bool),
    Error(String),
}

#[derive(Debug, Clone)]
pub struct Cell {
    pub row: u32,
    pub col: u32,
    /// Index into [`Workbook::styles`].
    pub style: u32,
    /// The value as stored (for a formula, its cached result).
    pub value: Value,
    /// The formula text; empty for a shared formula's dependent cells.
    pub formula: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Style {
    pub num_fmt: String,
    pub kind: Option<FmtKind>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub size: Option<f32>,
    pub color: Option<String>,
    pub fill: Option<String>,
    pub h_align: Option<String>,
    pub v_align: Option<String>,
    pub wrap: bool,
    /// Top, right, bottom, left.
    pub border: [bool; 4],
    pub locked: bool,
}

#[derive(Debug, Clone)]
pub struct ColInfo {
    pub min: u32,
    pub max: u32,
    pub width: Option<f64>,
    pub hidden: bool,
    pub style: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct RowInfo {
    pub row: u32,
    pub height: Option<f64>,
    pub hidden: bool,
    /// The row's own style when `customFormat` is set.
    pub style: Option<u32>,
}

#[derive(Debug, Clone, Default)]
pub struct Validation {
    pub sqref: String,
    /// `list`, `whole`, `decimal`, `date`, `time`, `textLength`, `custom` (`none` when absent).
    pub kind: String,
    pub operator: String,
    pub allow_blank: bool,
    /// `stop` (refuse), `warning`, `information`.
    pub error_style: String,
    pub show_error: bool,
    pub formula1: Option<String>,
    pub formula2: Option<String>,
    pub prompt_title: Option<String>,
    pub prompt: Option<String>,
    pub error_title: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Sheet {
    pub name: String,
    pub part: String,
    /// `visible`, `hidden`, `veryHidden`.
    pub state: String,
    pub protected: bool,
    pub cols: Vec<ColInfo>,
    pub rows: Vec<RowInfo>,
    pub merges: Vec<String>,
    pub cells: BTreeMap<(u32, u32), Cell>,
    pub validations: Vec<Validation>,
    /// Frozen rows and columns (top-left pane split).
    pub frozen: Option<(u32, u32)>,
    pub default_col_width: Option<f64>,
    pub default_row_height: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct DefinedName {
    pub name: String,
    pub local_sheet: Option<u32>,
    pub hidden: bool,
    pub formula: String,
}

#[derive(Debug, Clone)]
pub struct Workbook {
    pub sheets: Vec<Sheet>,
    pub styles: Vec<Style>,
    pub defined_names: Vec<DefinedName>,
    pub date1904: bool,
    pub full_calc_on_load: bool,
    pub structure_protected: bool,
    pub shared_strings: Vec<String>,
    pub workbook_part: String,
    pub shared_strings_part: Option<String>,
}

impl Workbook {
    pub fn sheet(&self, name: &str) -> Option<&Sheet> {
        self.sheets.iter().find(|s| s.name == name)
    }

    pub fn style(&self, idx: u32) -> &Style {
        static DEFAULT: std::sync::OnceLock<Style> = std::sync::OnceLock::new();
        self.styles.get(idx as usize).unwrap_or_else(|| {
            DEFAULT.get_or_init(|| Style {
                num_fmt: "General".into(),
                locked: true,
                ..Default::default()
            })
        })
    }

    /// The style a new cell at (row, col) gets: the row's, else the column's, else 0.
    pub fn default_style(&self, sheet: &Sheet, row: u32, col: u32) -> u32 {
        sheet
            .rows
            .iter()
            .find(|r| r.row == row)
            .and_then(|r| r.style)
            .or_else(|| {
                sheet
                    .cols
                    .iter()
                    .find(|c| (c.min..=c.max).contains(&col))
                    .and_then(|c| c.style)
            })
            .unwrap_or(0)
    }

    /// Whether this cell may be edited: no formula, and on a protected sheet only unlocked cells.
    pub fn editable(&self, sheet: &Sheet, row: u32, col: u32) -> bool {
        match sheet.cells.get(&(row, col)) {
            Some(c) if c.formula.is_some() => false,
            Some(c) => !sheet.protected || !self.style(c.style).locked,
            None => !sheet.protected || !self.style(self.default_style(sheet, row, col)).locked,
        }
    }

    /// How a cell shows, as Excel would with its format.
    pub fn display(&self, cell: &Cell) -> String {
        match &cell.value {
            Value::Empty => String::new(),
            Value::Text(s) => s.clone(),
            Value::Bool(b) => if *b { "TRUE" } else { "FALSE" }.into(),
            Value::Error(e) => e.clone(),
            Value::Number(v) => {
                numfmt::format_number(&self.style(cell.style).num_fmt, *v, self.date1904)
            }
        }
    }
}

fn parse_f64(s: &str) -> Option<f64> {
    s.trim().parse().ok()
}

fn truthy(s: &str) -> bool {
    matches!(s, "1" | "true" | "on")
}

fn rels(pkg: &Package, part: &str) -> Result<HashMap<String, String>, Error> {
    let mut out = HashMap::new();
    let Some(text) = pkg.read_str(&rels_of(part))? else {
        return Ok(out);
    };
    for t in Scanner::new(&text) {
        if let Tok::Open { name, attrs: a, .. } = t.map_err(Error::xml(&rels_of(part)))? {
            if local(name) == "Relationship" {
                if let (Some(id), Some(target)) = (attr(a, "Id"), attr(a, "Target")) {
                    if attr(a, "TargetMode").as_deref() != Some("External") {
                        out.insert(id, resolve_target(part, &target));
                    }
                }
            }
        }
    }
    Ok(out)
}

fn office_document(pkg: &Package) -> Result<String, Error> {
    let text = pkg
        .read_str("_rels/.rels")?
        .ok_or_else(|| Error::NotXlsx("no _rels/.rels".into()))?;
    for t in Scanner::new(&text) {
        if let Tok::Open { name, attrs: a, .. } = t.map_err(Error::xml("_rels/.rels"))? {
            if local(name) == "Relationship"
                && attr(a, "Type").is_some_and(|t| t.ends_with("/officeDocument"))
            {
                if let Some(target) = attr(a, "Target") {
                    return Ok(resolve_target("", &target));
                }
            }
        }
    }
    Err(Error::NotXlsx("no workbook part".into()))
}

pub fn read(bytes: &[u8]) -> Result<Workbook, Error> {
    let pkg = Package::open(bytes)?;
    read_package(&pkg)
}

pub(crate) fn read_package(pkg: &Package) -> Result<Workbook, Error> {
    let wb_part = office_document(pkg)?;
    if wb_part.ends_with(".bin") {
        return Err(Error::Unsupported(
            "binary workbooks (.xlsb) are not supported".into(),
        ));
    }
    let wb_text = pkg
        .read_str(&wb_part)?
        .ok_or_else(|| Error::NotXlsx(format!("missing {wb_part}")))?;
    let wb_rels = rels(pkg, &wb_part)?;

    let mut sheets_meta = Vec::new();
    let mut defined_names = Vec::new();
    let (mut date1904, mut full_calc, mut structure) = (false, false, false);
    let mut sc = Scanner::new(&wb_text);
    while let Some(t) = sc.next() {
        if let Tok::Open {
            name,
            attrs: a,
            empty,
            end,
            ..
        } = t.map_err(Error::xml(&wb_part))?
        {
            match local(name) {
                "sheet" => {
                    let name = attr(a, "name").unwrap_or_default();
                    let rid = attrs(a)
                        .into_iter()
                        .find(|(n, _)| local(n) == "id" && n.contains(':'))
                        .map(|(_, v)| v)
                        .unwrap_or_default();
                    let state = attr(a, "state").unwrap_or_else(|| "visible".into());
                    sheets_meta.push((name, rid, state));
                }
                "workbookPr" => date1904 = attr(a, "date1904").is_some_and(|v| truthy(&v)),
                "calcPr" => full_calc = attr(a, "fullCalcOnLoad").is_some_and(|v| truthy(&v)),
                "workbookProtection" => {
                    structure = attr(a, "lockStructure").is_some_and(|v| truthy(&v))
                }
                "definedName" if !empty => {
                    let (text, next) =
                        text_until_close(&wb_text, end, &[]).map_err(Error::xml(&wb_part))?;
                    defined_names.push(DefinedName {
                        name: attr(a, "name").unwrap_or_default(),
                        local_sheet: attr(a, "localSheetId").and_then(|v| v.parse().ok()),
                        hidden: attr(a, "hidden").is_some_and(|v| truthy(&v)),
                        formula: text,
                    });
                    skip_to(&mut sc, next);
                }
                _ => {}
            }
        }
    }

    // Shared strings and styles: through the workbook's relationships, with the usual names as
    // the fallback.
    let by_type = |suffix: &str, fallback: &str| -> Option<String> {
        let text = pkg.read_str(&rels_of(&wb_part)).ok().flatten()?;
        for t in Scanner::new(&text).flatten() {
            if let Tok::Open { name, attrs: a, .. } = t {
                if local(name) == "Relationship"
                    && attr(a, "Type").is_some_and(|t| t.ends_with(suffix))
                {
                    return attr(a, "Target").map(|t| resolve_target(&wb_part, &t));
                }
            }
        }
        pkg.has(fallback).then(|| fallback.to_string())
    };
    let sst_part = by_type("/sharedStrings", "xl/sharedStrings.xml");
    let styles_part = by_type("/styles", "xl/styles.xml");
    let theme_part = by_type("/theme", "xl/theme/theme1.xml");

    let shared_strings = match &sst_part {
        Some(p) => match pkg.read_str(p)? {
            Some(t) => parse_sst(&t).map_err(Error::xml(p))?,
            None => Vec::new(),
        },
        None => Vec::new(),
    };
    let theme = match &theme_part {
        Some(p) => pkg
            .read_str(p)?
            .map(|t| parse_theme(&t))
            .unwrap_or_default(),
        None => Vec::new(),
    };
    let styles = match &styles_part {
        Some(p) => match pkg.read_str(p)? {
            Some(t) => parse_styles(&t, &theme).map_err(Error::xml(p))?,
            None => Vec::new(),
        },
        None => Vec::new(),
    };

    let mut sheets = Vec::new();
    for (name, rid, state) in sheets_meta {
        let Some(part) = wb_rels.get(&rid) else {
            continue;
        };
        // Chart sheets and dialog sheets have no cells to show or edit.
        if !part.contains("worksheets/") {
            continue;
        }
        let Some(text) = pkg.read_str(part)? else {
            continue;
        };
        let mut s = parse_sheet(&text, &shared_strings).map_err(Error::xml(part))?;
        s.name = name;
        s.part = part.clone();
        s.state = state;
        sheets.push(s);
    }
    Ok(Workbook {
        sheets,
        styles,
        defined_names,
        date1904,
        full_calc_on_load: full_calc,
        structure_protected: structure,
        shared_strings,
        workbook_part: wb_part,
        shared_strings_part: sst_part,
    })
}

fn skip_to(sc: &mut Scanner, pos: usize) {
    sc.seek(pos);
}

fn parse_sst(text: &str) -> Result<Vec<String>, xml::XmlError> {
    let mut out = Vec::new();
    let mut sc = Scanner::new(text);
    while let Some(t) = sc.next() {
        if let Tok::Open {
            name, empty, end, ..
        } = t?
        {
            if local(name) == "si" {
                if empty {
                    out.push(String::new());
                    continue;
                }
                let (s, next) = text_until_close(text, end, &["rPh", "phoneticPr"])?;
                out.push(s);
                skip_to(&mut sc, next);
            }
        }
    }
    Ok(out)
}

/// The theme's colors in Excel's index order (lt1, dk1, lt2, dk2, accent1..6, hlink, folHlink).
fn parse_theme(text: &str) -> Vec<String> {
    let mut found: Vec<(String, String)> = Vec::new();
    let mut in_scheme = false;
    let mut current: Option<String> = None;
    for t in Scanner::new(text).flatten() {
        match t {
            Tok::Open { name, attrs: a, .. } => {
                let l = local(name);
                if l == "clrScheme" {
                    in_scheme = true;
                } else if in_scheme && current.is_none() && l != "srgbClr" && l != "sysClr" {
                    current = Some(l.to_string());
                } else if in_scheme {
                    if let Some(slot) = current.take() {
                        let v = if l == "srgbClr" {
                            attr(a, "val")
                        } else {
                            attr(a, "lastClr")
                        };
                        found.push((slot, v.unwrap_or_else(|| "000000".into())));
                    }
                }
            }
            Tok::Close { name, .. } if local(name) == "clrScheme" => break,
            _ => {}
        }
    }
    let get = |k: &str| {
        found
            .iter()
            .find(|(s, _)| s == k)
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| "000000".into())
    };
    [
        "lt1", "dk1", "lt2", "dk2", "accent1", "accent2", "accent3", "accent4", "accent5",
        "accent6", "hlink", "folHlink",
    ]
    .iter()
    .map(|k| get(k))
    .collect()
}

const INDEXED: [&str; 64] = [
    "000000", "FFFFFF", "FF0000", "00FF00", "0000FF", "FFFF00", "FF00FF", "00FFFF", "000000",
    "FFFFFF", "FF0000", "00FF00", "0000FF", "FFFF00", "FF00FF", "00FFFF", "800000", "008000",
    "000080", "808000", "800080", "008080", "C0C0C0", "808080", "9999FF", "993366", "FFFFCC",
    "CCFFFF", "660066", "FF8080", "0066CC", "CCCCFF", "000080", "FF00FF", "FFFF00", "00FFFF",
    "800080", "800000", "008080", "0000FF", "00CCFF", "CCFFFF", "CCFFCC", "FFFF99", "99CCFF",
    "FF99CC", "CC99FF", "FFCC99", "3366FF", "33CCCC", "99CC00", "FFCC00", "FF9900", "FF6600",
    "666699", "969696", "003366", "339966", "003300", "333300", "993300", "993366", "333399",
    "333333",
];

fn apply_tint(hex: &str, tint: f64) -> String {
    let n = u32::from_str_radix(hex, 16).unwrap_or(0);
    let ch = |shift: u32| {
        let c = ((n >> shift) & 0xFF) as f64;
        let v = if tint < 0.0 {
            c * (1.0 + tint)
        } else {
            c + (255.0 - c) * tint
        };
        v.round().clamp(0.0, 255.0) as u32
    };
    format!("{:02X}{:02X}{:02X}", ch(16), ch(8), ch(0))
}

/// `#RRGGBB` from a `<color>`-like element's attributes.
fn color_of(a: &str, theme: &[String]) -> Option<String> {
    let hex = if let Some(rgb) = attr(a, "rgb") {
        let rgb = rgb.trim().to_ascii_uppercase();
        let rgb = if rgb.len() == 8 {
            rgb[2..].to_string()
        } else {
            rgb
        };
        (rgb.len() == 6 && rgb.bytes().all(|b| b.is_ascii_hexdigit())).then_some(rgb)?
    } else if let Some(t) = attr(a, "theme").and_then(|v| v.parse::<usize>().ok()) {
        theme.get(t).cloned()?
    } else {
        // 64 is the system foreground, 65 the system background: no colour.
        let i = attr(a, "indexed").and_then(|v| v.parse::<usize>().ok())?;
        (*INDEXED.get(i)?).to_string()
    };
    let tint = attr(a, "tint")
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(0.0);
    Some(format!(
        "#{}",
        if tint != 0.0 {
            apply_tint(&hex, tint)
        } else {
            hex
        }
    ))
}

#[derive(Default, Clone)]
struct Font {
    bold: bool,
    italic: bool,
    underline: bool,
    size: Option<f32>,
    color: Option<String>,
}

fn parse_styles(text: &str, theme: &[String]) -> Result<Vec<Style>, xml::XmlError> {
    let mut num_fmts: HashMap<u32, String> = HashMap::new();
    let mut fonts: Vec<Font> = Vec::new();
    let mut fills: Vec<Option<String>> = Vec::new();
    let mut borders: Vec<[bool; 4]> = Vec::new();
    let mut xfs: Vec<Style> = Vec::new();

    #[derive(PartialEq)]
    enum Sec {
        None,
        Fonts,
        Fills,
        Borders,
        CellXfs,
        Other,
    }
    let mut sec = Sec::None;
    let mut font = Font::default();
    let mut fill: Option<String> = None;
    let mut pattern_solid = false;
    let mut border = [false; 4];
    let mut xf: Option<Style> = None;
    let mut border_side: Option<usize> = None;

    for t in Scanner::new(text) {
        match t? {
            Tok::Open {
                name,
                attrs: a,
                empty,
                ..
            } => match (local(name), &sec) {
                ("numFmt", _) => {
                    if let (Some(id), Some(code)) = (
                        attr(a, "numFmtId").and_then(|v| v.parse().ok()),
                        attr(a, "formatCode"),
                    ) {
                        num_fmts.insert(id, code);
                    }
                }
                ("fonts", _) => sec = Sec::Fonts,
                ("fills", _) => sec = Sec::Fills,
                ("borders", _) => sec = Sec::Borders,
                ("cellXfs", _) => sec = Sec::CellXfs,
                // Not `extLst`: an `<xf>` may carry one, and the sheet-wide one comes last.
                ("cellStyleXfs" | "dxfs" | "cellStyles" | "colors" | "tableStyles", _) => {
                    sec = Sec::Other
                }
                ("font", Sec::Fonts) => {
                    font = Font::default();
                    if empty {
                        fonts.push(font.clone());
                    }
                }
                ("b", Sec::Fonts) => font.bold = attr(a, "val").is_none_or(|v| truthy(&v)),
                ("i", Sec::Fonts) => font.italic = attr(a, "val").is_none_or(|v| truthy(&v)),
                ("u", Sec::Fonts) => font.underline = attr(a, "val").as_deref() != Some("none"),
                ("sz", Sec::Fonts) => font.size = attr(a, "val").and_then(|v| v.parse().ok()),
                ("color", Sec::Fonts) => font.color = color_of(a, theme),
                ("fill", Sec::Fills) => {
                    fill = None;
                    pattern_solid = false;
                    if empty {
                        fills.push(None);
                    }
                }
                ("patternFill", Sec::Fills) => {
                    pattern_solid = attr(a, "patternType").is_some_and(|p| p != "none");
                }
                ("fgColor", Sec::Fills) if pattern_solid => fill = color_of(a, theme),
                ("stop", Sec::Fills) => {}
                ("color", Sec::Fills) if fill.is_none() => fill = color_of(a, theme),
                ("border", Sec::Borders) => {
                    border = [false; 4];
                    if empty {
                        borders.push(border);
                    }
                }
                (side @ ("top" | "right" | "bottom" | "left" | "start" | "end"), Sec::Borders) => {
                    let i = match side {
                        "top" => 0,
                        "right" | "end" => 1,
                        "bottom" => 2,
                        _ => 3,
                    };
                    border[i] = attr(a, "style").is_some_and(|s| s != "none");
                    border_side = (!empty).then_some(i);
                }
                ("xf", Sec::CellXfs) => {
                    let id = attr(a, "numFmtId")
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(0u32);
                    let fid = attr(a, "fontId")
                        .and_then(|v| v.parse::<usize>().ok())
                        .unwrap_or(0);
                    let fill_id = attr(a, "fillId")
                        .and_then(|v| v.parse::<usize>().ok())
                        .unwrap_or(0);
                    let bid = attr(a, "borderId")
                        .and_then(|v| v.parse::<usize>().ok())
                        .unwrap_or(0);
                    let code = num_fmts
                        .get(&id)
                        .cloned()
                        .or_else(|| numfmt::builtin(id).map(str::to_string))
                        .unwrap_or_else(|| "General".into());
                    let f = fonts.get(fid).cloned().unwrap_or_default();
                    let s = Style {
                        kind: Some(numfmt::classify(&code)),
                        num_fmt: code,
                        bold: f.bold,
                        italic: f.italic,
                        underline: f.underline,
                        size: f.size,
                        color: f.color,
                        fill: fills.get(fill_id).cloned().flatten(),
                        border: borders.get(bid).copied().unwrap_or_default(),
                        locked: true,
                        ..Default::default()
                    };
                    if empty {
                        xfs.push(s);
                    } else {
                        xf = Some(s);
                    }
                }
                ("alignment", Sec::CellXfs) => {
                    if let Some(x) = xf.as_mut() {
                        x.h_align = attr(a, "horizontal").filter(|h| h != "general");
                        x.v_align = attr(a, "vertical");
                        x.wrap = attr(a, "wrapText").is_some_and(|v| truthy(&v));
                    }
                }
                ("protection", Sec::CellXfs) => {
                    if let Some(x) = xf.as_mut() {
                        x.locked = attr(a, "locked").is_none_or(|v| truthy(&v));
                    }
                }
                _ => {}
            },
            Tok::Close { name, .. } => match (local(name), &sec) {
                ("font", Sec::Fonts) => fonts.push(std::mem::take(&mut font)),
                ("fill", Sec::Fills) => fills.push(fill.take()),
                ("border", Sec::Borders) => {
                    border_side = None;
                    borders.push(border);
                }
                ("top" | "right" | "bottom" | "left" | "start" | "end", Sec::Borders) => {
                    border_side = None
                }
                ("xf", Sec::CellXfs) => {
                    if let Some(x) = xf.take() {
                        xfs.push(x);
                    }
                }
                (
                    "fonts" | "fills" | "borders" | "cellXfs" | "cellStyleXfs" | "dxfs"
                    | "cellStyles" | "colors" | "tableStyles",
                    _,
                ) => sec = Sec::None,
                _ => {}
            },
            _ => {}
        }
    }
    let _ = border_side;
    Ok(xfs)
}

fn parse_sheet(text: &str, sst: &[String]) -> Result<Sheet, xml::XmlError> {
    let mut sheet = Sheet {
        name: String::new(),
        part: String::new(),
        state: String::new(),
        protected: false,
        cols: Vec::new(),
        rows: Vec::new(),
        merges: Vec::new(),
        cells: BTreeMap::new(),
        validations: Vec::new(),
        frozen: None,
        default_col_width: None,
        default_row_height: None,
    };
    let mut sc = Scanner::new(text);
    let mut row_no = 0u32;
    let mut next_col = 1u32;
    let mut validation: Option<Validation> = None;
    while let Some(t) = sc.next() {
        match t? {
            Tok::Open {
                name,
                attrs: a,
                empty,
                end,
                ..
            } => match local(name) {
                "sheetFormatPr" => {
                    sheet.default_col_width =
                        attr(a, "defaultColWidth").and_then(|v| parse_f64(&v));
                    sheet.default_row_height =
                        attr(a, "defaultRowHeight").and_then(|v| parse_f64(&v));
                }
                "pane" => {
                    if attr(a, "state").is_some_and(|s| s == "frozen" || s == "frozenSplit") {
                        let x = attr(a, "xSplit").and_then(|v| parse_f64(&v)).unwrap_or(0.0) as u32;
                        let y = attr(a, "ySplit").and_then(|v| parse_f64(&v)).unwrap_or(0.0) as u32;
                        sheet.frozen = Some((y, x));
                    }
                }
                "col" => {
                    let min = attr(a, "min").and_then(|v| v.parse().ok()).unwrap_or(1);
                    let max = attr(a, "max")
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(min)
                        .min(cellref::MAX_COL);
                    sheet.cols.push(ColInfo {
                        min,
                        max,
                        width: attr(a, "width").and_then(|v| parse_f64(&v)),
                        hidden: attr(a, "hidden").is_some_and(|v| truthy(&v)),
                        style: attr(a, "style").and_then(|v| v.parse().ok()),
                    });
                }
                "row" => {
                    row_no = attr(a, "r")
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(row_no + 1);
                    next_col = 1;
                    let custom = attr(a, "customFormat").is_some_and(|v| truthy(&v));
                    let info = RowInfo {
                        row: row_no,
                        height: attr(a, "ht").and_then(|v| parse_f64(&v)),
                        hidden: attr(a, "hidden").is_some_and(|v| truthy(&v)),
                        style: if custom {
                            attr(a, "s").and_then(|v| v.parse().ok())
                        } else {
                            None
                        },
                    };
                    sheet.rows.push(info);
                }
                "c" => {
                    let (row, col) = match attr(a, "r").and_then(|r| cellref::parse(&r)) {
                        Some(rc) => rc,
                        None => (row_no.max(1), next_col),
                    };
                    next_col = col + 1;
                    let style = attr(a, "s").and_then(|v| v.parse().ok()).unwrap_or(0);
                    let ty = attr(a, "t").unwrap_or_else(|| "n".into());
                    let mut cell = Cell {
                        row,
                        col,
                        style,
                        value: Value::Empty,
                        formula: None,
                    };
                    if !empty {
                        let (v, f, is) = cell_children(text, end, &mut sc)?;
                        cell.formula = f;
                        cell.value = match ty.as_str() {
                            "s" => v
                                .and_then(|v| v.trim().parse::<usize>().ok())
                                .and_then(|i| sst.get(i).cloned())
                                .map_or(Value::Empty, Value::Text),
                            "str" => v.map_or(Value::Empty, Value::Text),
                            "inlineStr" => is.or(v).map_or(Value::Empty, Value::Text),
                            "b" => v.map_or(Value::Empty, |v| {
                                Value::Bool(v.trim() == "1" || v.trim() == "true")
                            }),
                            "e" => v.map_or(Value::Empty, Value::Error),
                            // ISO 8601 dates (strict OOXML); shown as their text.
                            "d" => v.map_or(Value::Empty, Value::Text),
                            _ => v
                                .and_then(|v| parse_f64(&v))
                                .map_or(Value::Empty, Value::Number),
                        };
                    }
                    sheet.cells.insert((row, col), cell);
                }
                "sheetProtection" => {
                    sheet.protected = attr(a, "sheet").is_some_and(|v| truthy(&v));
                }
                "mergeCell" => {
                    if let Some(r) = attr(a, "ref") {
                        sheet.merges.push(r);
                    }
                }
                "dataValidation" => {
                    let v = Validation {
                        sqref: attr(a, "sqref").unwrap_or_default(),
                        kind: attr(a, "type").unwrap_or_else(|| "none".into()),
                        operator: attr(a, "operator").unwrap_or_else(|| "between".into()),
                        allow_blank: attr(a, "allowBlank").is_some_and(|v| truthy(&v)),
                        error_style: attr(a, "errorStyle").unwrap_or_else(|| "stop".into()),
                        show_error: attr(a, "showErrorMessage").is_some_and(|v| truthy(&v)),
                        prompt_title: attr(a, "promptTitle"),
                        prompt: attr(a, "prompt"),
                        error_title: attr(a, "errorTitle"),
                        error: attr(a, "error"),
                        ..Default::default()
                    };
                    if empty {
                        sheet.validations.push(v);
                    } else {
                        validation = Some(v);
                    }
                }
                l @ ("formula1" | "formula2" | "sqref") if validation.is_some() && !empty => {
                    let (s, next) = text_until_close(text, end, &[])?;
                    if let Some(v) = validation.as_mut() {
                        match l {
                            "formula1" => v.formula1 = Some(s),
                            "formula2" => v.formula2 = Some(s),
                            _ => v.sqref = s,
                        }
                    }
                    skip_to(&mut sc, next);
                }
                _ => {}
            },
            Tok::Close { name, .. } if local(name) == "dataValidation" => {
                if let Some(v) = validation.take() {
                    sheet.validations.push(v);
                }
            }
            _ => {}
        }
    }
    Ok(sheet)
}

/// A `<c>`'s `<v>`, `<f>` and `<is>` texts.
type CellTexts = (Option<String>, Option<String>, Option<String>);

/// A `<c>`'s `<v>`, `<f>` and `<is>` texts; leaves the scanner after `</c>`.
fn cell_children(text: &str, from: usize, sc: &mut Scanner) -> Result<CellTexts, xml::XmlError> {
    let (mut v, mut f, mut is) = (None, None, None);
    let mut inner = Scanner::at(text, from);
    while let Some(t) = inner.next() {
        match t? {
            Tok::Open {
                name, empty, end, ..
            } => {
                let l = local(name);
                if empty {
                    if l == "f" {
                        f = Some(String::new());
                    }
                    continue;
                }
                let (s, next) = text_until_close(text, end, &["rPh", "phoneticPr"])?;
                match l {
                    "v" => v = Some(s),
                    "f" => f = Some(s),
                    "is" => is = Some(s),
                    _ => {}
                }
                skip_to(&mut inner, next);
            }
            Tok::Close { name, end, .. } if local(name) == "c" => {
                skip_to(sc, end);
                return Ok((v, f, is));
            }
            _ => {}
        }
    }
    Err(xml::XmlError("unterminated cell".into(), from))
}
