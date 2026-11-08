//! Round trips on a small workbook built here with the features that break naive writers:
//! shared strings, a protected sheet with unlocked inputs, a list validation whose choices depend
//! on a cell, formulas and a calculation chain, merges, a self-closing row, missing cells, a
//! custom part. Every part but the edited ones must come back with the same compressed bytes.
//!
//! `AU_TIMESHEET=<path to the AU worktime .xlsx>` adds the real file (not in git: it is AU's
//! template and the owner's copy holds his AU ID).

use std::io::{Cursor, Read, Write};

use calamine::{Data, Reader, Xlsx};
use secretary_xlsx::{apply, read, Edit, EditValue, Error, Options};
use zip::write::SimpleFileOptions;

const CT: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/worksheets/sheet2.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/><Override PartName="/xl/sharedStrings.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml"/><Override PartName="/xl/calcChain.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.calcChain+xml"/></Types>"#;
const RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#;
const WB: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><workbookPr/><sheets><sheet name="Month" sheetId="1" r:id="rId1"/><sheet name="Notes" sheetId="2" r:id="rId2"/></sheets><definedNames><definedName name="Codes">Month!$M$3:$M$5</definedName></definedNames><calcPr calcId="191028"/></workbook>"#;
const WB_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet2.xml"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/><Relationship Id="rId4" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings" Target="sharedStrings.xml"/><Relationship Id="rId5" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/calcChain" Target="calcChain.xml"/></Relationships>"#;
// 0 default (locked), 1 unlocked text, 2 unlocked h:mm, 3 locked [h]:mm, 4 unlocked date
const STYLES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><numFmts count="2"><numFmt numFmtId="170" formatCode="[h]:mm;@"/><numFmt numFmtId="171" formatCode="dd-mm-yyyy"/></numFmts><fonts count="1"><font><sz val="11"/></font></fonts><fills count="2"><fill><patternFill patternType="none"/></fill><fill><patternFill patternType="solid"><fgColor rgb="FFFFFF00"/></patternFill></fill></fills><borders count="1"><border><left/><right/><top/><bottom/></border></borders><cellXfs count="5"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/><xf numFmtId="0" fontId="0" fillId="1" borderId="0" applyProtection="1"><protection locked="0"/><extLst><ext uri="{X}"><y/></ext></extLst></xf><xf numFmtId="20" fontId="0" fillId="0" borderId="0" applyProtection="1"><protection locked="0"/></xf><xf numFmtId="170" fontId="0" fillId="0" borderId="0"/><xf numFmtId="171" fontId="0" fillId="0" borderId="0" applyProtection="1"><protection locked="0"/></xf></cellXfs></styleSheet>"#;
const SST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="5" uniqueCount="5"><si><t>Code</t></si><si><t>N</t></si><si><t>R</t></si><si><t>F</t></si><si><r><t>Rich</t></r><r><t xml:space="preserve"> text</t></r></si></sst>"#;
const SHEET1: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><dimension ref="A1:M8"/><sheetData><row r="1" spans="1:13"><c r="A1" t="s"><v>0</v></c><c r="B1" s="1" t="b"><v>0</v></c></row><row r="2" spans="1:13"><c r="A2" s="1" t="s"><v>1</v></c><c r="B2" s="2"><v>0</v></c><c r="C2" s="2"><v>0</v></c><c r="D2" s="3"><f>C2-B2</f><v>0</v></c><c r="E2" s="1"/><c r="G2" s="4"><v>46327</v></c></row><row r="3"><c r="M3" t="s"><v>3</v></c></row><row r="4"><c r="M4" t="s"><v>1</v></c></row><row r="5" s="1" customFormat="1"><c r="M5" t="s"><v>2</v></c></row><row r="6" s="1" customFormat="1"/><row r="8"><c r="A8" t="s"><v>4</v></c></row></sheetData><sheetProtection sheet="1" objects="1"/><mergeCells count="1"><mergeCell ref="E2:F2"/></mergeCells><dataValidations count="2"><dataValidation type="list" allowBlank="1" showErrorMessage="1" sqref="A2:A6"><formula1>IF($B$1,$M$3:$M$5,$M$4:$M$5)</formula1></dataValidation><dataValidation type="time" allowBlank="1" showErrorMessage="1" sqref="B2:C6"><formula1>0</formula1><formula2>0.999305555555556</formula2></dataValidation></dataValidations></worksheet>"#;
const SHEET2: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData/></worksheet>"#;
const CHAIN: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<calcChain xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><c r="D2" i="1"/></calcChain>"#;

fn fixture() -> Vec<u8> {
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let o = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (name, body) in [
        ("[Content_Types].xml", CT),
        ("_rels/.rels", RELS),
        ("xl/workbook.xml", WB),
        ("xl/_rels/workbook.xml.rels", WB_RELS),
        ("xl/styles.xml", STYLES),
        ("xl/sharedStrings.xml", SST),
        ("xl/worksheets/sheet1.xml", SHEET1),
        ("xl/worksheets/sheet2.xml", SHEET2),
        ("xl/calcChain.xml", CHAIN),
        ("customXml/item1.xml", "<custom>kept</custom>"),
    ] {
        w.start_file(name, o).unwrap();
        w.write_all(body.as_bytes()).unwrap();
    }
    w.finish().unwrap().into_inner()
}

fn input(sheet: &str, cell: &str, v: &str) -> Edit {
    Edit {
        sheet: sheet.into(),
        cell: cell.into(),
        value: EditValue::Input(v.into()),
    }
}

/// (name, compressed bytes, crc) of every entry, in order.
fn raw_parts(bytes: &[u8]) -> Vec<(String, Vec<u8>, u32)> {
    let mut z = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    (0..z.len())
        .map(|i| {
            let mut f = z.by_index_raw(i).unwrap();
            let mut b = Vec::new();
            f.read_to_end(&mut b).unwrap();
            (f.name().to_string(), b, f.crc32())
        })
        .collect()
}

fn part(bytes: &[u8], name: &str) -> String {
    let mut z = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    let mut s = String::new();
    z.by_name(name).unwrap().read_to_string(&mut s).unwrap();
    s
}

fn calamine_cell(bytes: &[u8], sheet: &str, row: u32, col: u32) -> Data {
    let mut x: Xlsx<_> = Xlsx::new(Cursor::new(bytes.to_vec())).unwrap();
    let r = x.worksheet_range(sheet).unwrap();
    r.get_value((row - 1, col - 1))
        .cloned()
        .unwrap_or(Data::Empty)
}

#[test]
fn edits_touch_only_their_cells() {
    let src = fixture();
    let out = apply(
        &src,
        &[
            input("Month", "B2", "9:00"),
            input("Month", "C2", "12:30"),
            input("Month", "A2", "r"),
            input("Month", "E2", "Hej & <tak>"),
        ],
        &Options::default(),
    )
    .unwrap();
    assert_eq!(
        out.parts,
        vec!["xl/workbook.xml", "xl/worksheets/sheet1.xml"]
    );
    // Every other part: the same compressed bytes and CRC, in the same order.
    let (a, b) = (raw_parts(&src), raw_parts(&out.bytes));
    assert_eq!(
        a.iter().map(|p| &p.0).collect::<Vec<_>>(),
        b.iter().map(|p| &p.0).collect::<Vec<_>>()
    );
    for (x, y) in a.iter().zip(&b) {
        if !out.parts.contains(&x.0) {
            assert_eq!(x, y, "{} changed", x.0);
        }
    }
    let s = part(&out.bytes, "xl/worksheets/sheet1.xml");
    let expected = SHEET1
        .replace(
            r#"<c r="B2" s="2"><v>0</v></c>"#,
            r#"<c r="B2" s="2"><v>0.375</v></c>"#,
        )
        .replace(
            r#"<c r="C2" s="2"><v>0</v></c>"#,
            r#"<c r="C2" s="2"><v>0.5208333333333334</v></c>"#,
        )
        // "r" is the list's "R": shared string 2, as Excel writes it.
        .replace(
            r#"<c r="A2" s="1" t="s"><v>1</v></c>"#,
            r#"<c r="A2" s="1" t="s"><v>2</v></c>"#,
        )
        .replace(
            r#"<c r="E2" s="1"/>"#,
            r#"<c r="E2" s="1" t="inlineStr"><is><t>Hej &amp; &lt;tak&gt;</t></is></c>"#,
        );
    assert_eq!(s, expected);
    assert_eq!(
        part(&out.bytes, "xl/workbook.xml"),
        WB.replace(
            r#"<calcPr calcId="191028"/>"#,
            r#"<calcPr calcId="191028" fullCalcOnLoad="1"/>"#
        )
    );
    // Calamine reads the new values.
    assert_eq!(
        calamine_cell(&out.bytes, "Month", 2, 1),
        Data::String("R".into())
    );
    assert_eq!(
        calamine_cell(&out.bytes, "Month", 2, 5),
        Data::String("Hej & <tak>".into())
    );
    match calamine_cell(&out.bytes, "Month", 2, 2) {
        Data::DateTime(d) => assert!((d.as_f64() - 0.375).abs() < 1e-12),
        Data::Float(f) => assert!((f - 0.375).abs() < 1e-12),
        d => panic!("{d:?}"),
    }
    // And so does this crate, with the display Excel would show.
    let wb = read(&out.bytes).unwrap();
    let m = wb.sheet("Month").unwrap();
    assert_eq!(wb.display(&m.cells[&(2, 3)]), "12:30");
    assert!(wb.full_calc_on_load);
}

#[test]
fn refusals_match_excel() {
    let src = fixture();
    let o = Options::default();
    assert!(matches!(
        apply(&src, &[input("Month", "D2", "1")], &o),
        Err(Error::FormulaCell { .. })
    ));
    // A locked cell on a protected sheet.
    assert!(matches!(
        apply(&src, &[input("Month", "A1", "x")], &o),
        Err(Error::LockedCell { .. })
    ));
    // F is only a choice while B1 (the weekly day off) is TRUE.
    assert!(matches!(
        apply(&src, &[input("Month", "A2", "F")], &o),
        Err(Error::Invalid { .. })
    ));
    let ok = apply(
        &src,
        &[input("Month", "B1", "yes"), input("Month", "A2", "f")],
        &o,
    )
    .unwrap();
    assert_eq!(
        calamine_cell(&ok.bytes, "Month", 2, 1),
        Data::String("F".into())
    );
    assert_eq!(calamine_cell(&ok.bytes, "Month", 1, 2), Data::Bool(true));
    // Times past midnight are not times; nor is text.
    assert!(matches!(
        apply(&src, &[input("Month", "B2", "25:00")], &o),
        Err(Error::Invalid { .. })
    ));
    assert!(matches!(
        apply(&src, &[input("Month", "B2", "soon")], &o),
        Err(Error::Invalid { .. })
    ));
    assert!(matches!(
        apply(&src, &[input("Nope", "A1", "x")], &o),
        Err(Error::UnknownSheet(_))
    ));
    assert!(matches!(
        apply(&src, &[input("Month", "1A", "x")], &o),
        Err(Error::BadCell(_))
    ));
    // Unprotected sheets take anything but formulas.
    let notes = apply(&src, &[input("Notes", "B2", "free")], &o).unwrap();
    assert_eq!(
        calamine_cell(&notes.bytes, "Notes", 2, 2),
        Data::String("free".into())
    );
}

#[test]
fn new_cells_and_rows_land_in_order() {
    let src = fixture();
    let o = Options {
        enforce_protection: false,
        ..Options::default()
    };
    let out = apply(
        &src,
        &[
            input("Month", "F2", "between"), // missing cell inside a row (after E2, before G2)
            input("Month", "N2", "past the end"), // after the last cell, outside spans
            input("Month", "B6", "8:00"),    // a self-closing row
            input("Month", "A7", "new row"), // a missing row in the middle
            input("Month", "C10", "at the end"),
            input("Notes", "A1", "first"), // <sheetData/>
            Edit {
                sheet: "Month".into(),
                cell: "Z9".into(),
                value: EditValue::Empty,
            }, // clearing nothing: no-op
        ],
        &o,
    )
    .unwrap();
    let s = part(&out.bytes, "xl/worksheets/sheet1.xml");
    assert!(s.contains(r#"<c r="E2" s="1"/><c r="F2" t="inlineStr"><is><t>between</t></is></c><c r="G2" s="4">"#), "{s}");
    assert!(s.contains(r#"<row r="2" spans="1:14">"#));
    assert!(s.contains(r#"<c r="N2" t="inlineStr"><is><t>past the end</t></is></c></row>"#));
    // A new cell in a row with its own style takes that style (as Excel does).
    assert!(s.contains(r#"<row r="6" s="1" customFormat="1"><c r="B6" s="1"><v>0.3333333333333333</v></c></row>"#), "{s}");
    assert!(s.contains(
        r#"</row><row r="7"><c r="A7" t="inlineStr"><is><t>new row</t></is></c></row><row r="8">"#
    ));
    assert!(s.contains(
        r#"<row r="10"><c r="C10" t="inlineStr"><is><t>at the end</t></is></c></row></sheetData>"#
    ));
    assert!(s.contains(r#"<dimension ref="A1:N10"/>"#));
    let n = part(&out.bytes, "xl/worksheets/sheet2.xml");
    assert!(n.contains(r#"<sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>first</t></is></c></row></sheetData>"#), "{n}");
    assert_eq!(
        calamine_cell(&out.bytes, "Month", 7, 1),
        Data::String("new row".into())
    );
    assert_eq!(
        calamine_cell(&out.bytes, "Notes", 1, 1),
        Data::String("first".into())
    );
    // Re-reading finds everything where it was put; nothing else moved.
    let wb = read(&out.bytes).unwrap();
    let m = wb.sheet("Month").unwrap();
    assert_eq!(
        m.cells.len(),
        read(&src).unwrap().sheet("Month").unwrap().cells.len() + 5
    );
    assert_eq!(wb.display(&m.cells[&(8, 1)]), "Rich text");
}

#[test]
fn clearing_and_unchanged_edits() {
    let src = fixture();
    let o = Options::default();
    // The same value again changes nothing: the original bytes come back.
    let same = apply(&src, &[input("Month", "B2", "0:00")], &o).unwrap();
    assert!(same.parts.is_empty());
    assert_eq!(same.bytes, src);
    let cleared = apply(
        &src,
        &[Edit {
            sheet: "Month".into(),
            cell: "G2".into(),
            value: EditValue::Empty,
        }],
        &o,
    )
    .unwrap();
    assert!(part(&cleared.bytes, "xl/worksheets/sheet1.xml").contains(r#"<c r="G2" s="4"/>"#));
    let dated = apply(&src, &[input("Month", "G2", "03-11-2026")], &o).unwrap();
    assert!(part(&dated.bytes, "xl/worksheets/sheet1.xml")
        .contains(r#"<c r="G2" s="4"><v>46329</v></c>"#));
}

#[test]
fn view_marks_inputs() {
    let wb = read(&fixture()).unwrap();
    let v = secretary_xlsx::view::workbook_json(&wb);
    let cells = v["sheets"][0]["cells"].as_array().unwrap();
    let get = |r: &str| {
        cells
            .iter()
            .find(|c| c["ref"] == r)
            .cloned()
            .unwrap_or_default()
    };
    assert_eq!(get("B2")["input"], true);
    assert_eq!(get("D2")["f"], "C2-B2");
    assert!(get("D2").get("editable").is_none());
    assert!(get("A1").get("input").is_none());
    assert_eq!(
        v["sheets"][0]["validations"][0]["list"],
        serde_json::json!(["N", "R"])
    );
    assert_eq!(v["styles"][1]["fill"], "#FFFF00");
    assert_eq!(v["styles"][3]["kind"], "duration");
}

/// The owner's real timesheet: a filled November reads back in calamine, and the app's totals
/// equal LibreOffice's recalculation of the same file (docs/xlsx.md "Proof", 2026-11-01).
#[test]
fn au_timesheet() {
    let Ok(path) = std::env::var("AU_TIMESHEET") else {
        eprintln!("AU_TIMESHEET not set: skipped");
        return;
    };
    let src = std::fs::read(path).unwrap();
    let e = |c: &str, v: &str| {
        let (s, c) = c.split_once('!').unwrap();
        input(s, c, v)
    };
    let edits = [
        e("Masterdata!B3", "2026-01-01"),
        e("Masterdata!B4", "31-12-2026"),
        e("Masterdata!B8", "-01:30"),
        e("November!B1", "12"),
        e("November!B2", "yes"),
        e("November!F10", "r"),
        e("November!J10", "9:00"),
        e("November!K10", "12:30"),
        e("November!S10", "CraftIT: WP draft embed test"),
        e("November!F11", "R"),
        e("November!I11", "2:15"),
        e("November!F14", "R"),
        e("November!J14", "13:00"),
        e("November!K14", "16:00"),
        e("November!F15", "R"),
        e("November!J15", "10:00"),
        e("November!K15", "11:00"),
        e("November!F19", "f"),
        e("November!F24", "R"),
        e("November!J24", "10:00"),
        e("November!K24", "09:00"),
        e("November!F28", "R"),
        e("November!I28", "3:45"),
    ];
    let out = apply(&src, &edits, &Options::default()).unwrap();
    assert_eq!(
        out.parts,
        vec![
            "xl/workbook.xml",
            "xl/worksheets/sheet1.xml",
            "xl/worksheets/sheet12.xml"
        ]
    );
    let (a, b) = (raw_parts(&src), raw_parts(&out.bytes));
    assert_eq!(a.len(), b.len());
    for (x, y) in a.iter().zip(&b) {
        if !out.parts.contains(&x.0) {
            assert_eq!(x, y, "{} changed", x.0);
        }
    }
    assert_eq!(
        calamine_cell(&out.bytes, "November", 10, 6),
        Data::String("R".into())
    );
    assert_eq!(
        calamine_cell(&out.bytes, "November", 10, 19),
        Data::String("CraftIT: WP draft embed test".into())
    );
    let mut x: Xlsx<_> = Xlsx::new(Cursor::new(out.bytes.clone())).unwrap();
    assert_eq!(x.sheet_names().len(), 13);
    for name in x.sheet_names().to_owned() {
        x.worksheet_range(&name).unwrap();
        x.worksheet_formula(&name).unwrap();
    }
    let wb = read(&out.bytes).unwrap();
    let t = secretary_xlsx::timesheet::detect(&wb).unwrap();
    let nov = &t.months[10];
    assert_eq!(nov.codes, ["F", "N", "R"]);
    // LibreOffice 25.x (da-DK) on the same file: B4 00:30, Masterdata D27 -01:00.
    assert_eq!(
        (nov.norm.as_str(), nov.worked.as_str(), nov.diff.as_str()),
        ("50:24", "50:54", "00:30")
    );
    assert_eq!(
        (t.balance_carried.as_str(), t.total.as_str()),
        ("-01:30", "-01:00")
    );
    let day = |row: u32| nov.days.iter().find(|d| d.row == row).unwrap();
    for (row, norm, worked, diff) in [
        (10, "02:24", "03:30", "01:06"),
        (11, "02:24", "02:15", "-00:09"),
        (15, "02:24", "01:00", "01:00"),
        (17, "03:00", "03:00", "00:00"),
        (19, "00:00", "00:00", "00:00"),
        (24, "02:24", "-01:00", "-03:24"),
        (28, "02:24", "03:45", "01:21"),
    ] {
        let d = day(row);
        assert_eq!(
            (d.norm.as_str(), d.worked.as_str(), d.diff.as_str()),
            (norm, worked, diff),
            "row {row}"
        );
    }
    // The other months are untouched and balance out.
    assert!(t
        .months
        .iter()
        .filter(|m| m.month != 11)
        .all(|m| m.diff == "00:00"));
}
