# Spreadsheets (.xlsx): open, view, edit, save (2026-11-01)

The owner fills in his Aarhus University worktime timesheet (`AU_worktime_registration_2026__v1.4.xlsx`, 13 sheets:
Masterdata plus one per month) and has no spreadsheet app on the Mac. Secretary opens `.xlsx` and `.xlsm` files from the
Drive and from shares, shows every sheet, marks the input cells, and saves edits back without losing anything. The
file goes to AU afterwards, so the round trip has to open cleanly in Excel.

Nothing is uploaded to Google or Microsoft: everything runs on nezopt (lucina-media).

## Pieces

| Piece | Where | What |
| --- | --- | --- |
| `secretary-xlsx` | `crates/secretary-xlsx` | Reader, view JSON, patcher, AU timesheet form and totals, `xlsx-tool` CLI |
| sheet API | `crates/lucina-media/src/sheet.rs` | `GET/POST /v1/media/items/{id}/sheet`, versions, restore |
| web | `web/src/views/Sheet*.tsx`, `web/src/api/sheet.ts` | Grid with sheet tabs, input cells highlighted, timesheet form (Mac app bundles it) |
| iOS | `ios/Secretary/Features/Gallery/Sheet*.swift` | The same grid and form; shared `.xlsx` files arrive through the share extension |

The owner's timesheet is reached through the Drive folder `CraftIT` (`LUCINA_MEDIA_FOLDERS`,
`/home/nez/Projects/Work/CraftIT/admin`; Lucina's go-ahead `dc-87ceb0ebba36`). A file shared from the iPhone lands in the
library and opens the same way from the Drive's shares.

## How a save keeps the workbook intact

The patcher rewrites only what changed:

- **Cells.** Only the edited `<c>` elements in the edited `sheetN.xml` change. Each keeps its `r` and `s` (style) attributes, and its
  position in the row. Missing cells and rows are inserted in order. `<dimension>` and the row `spans` hint grow when needed.
- **Strings.** A string already in `sharedStrings.xml` reuses its index, so a typed `R` is stored exactly as Excel stores it. Any
  other string is written inline (`t="inlineStr"`), so `sharedStrings.xml` never changes.
- **Formulas.** Formula cells cannot be edited, so `calcChain.xml` stays valid. `xl/workbook.xml` gets
  `<calcPr … fullCalcOnLoad="1"/>`, and Excel recomputes every cached result when it opens the file.
- **Everything else** is copied from the original zip as the same compressed bytes, CRC and timestamp, in the same order. That
  covers styles, data validations, conditional formats, defined names, protection hashes, tables, drawings, the external link,
  `metadata.xml` (dynamic arrays), customXml and printer settings.
- **Excel's rules apply when typing.**
  - On a protected sheet only unlocked cells take edits (`locked_cell`).
  - A list validation with a stop alert refuses other values (`invalid_value`). The list is read the way Excel reads it,
    including `IF($B$2,$M$3:$M$5,$M$4:$M$5)`: `F` is a choice only while "weekly day off" is yes. A choice typed in another case
    is stored in the list's spelling.
  - Time, date and number validations check their ranges.
- **Typed input** follows the cell's format.
  - Time cells take `9:30`, `09.30` or `9`; `[h]:mm` cells may pass 24 hours and be negative.
  - Date cells take `2026-11-03`, `03-11-2026`, `3/11/26` or `03.11.2026`, day first.
  - Boolean cells take yes/no, ja/nej and true/false.
  - General cells take numbers (`12,5` works), else text, so the template's balance cell stays the `hh:mm` text its formulas
    parse.
  - A leading `=` is text: the app never enters formulas.

### Crates considered (porting guidelines §3, licences §4)

| Crate | Licence | Verdict | Why |
| --- | --- | --- | --- |
| umya-spreadsheet 2.x | MIT | **reject** | Read and write on the owner's file dropped `customXml/*`, `calcChain.xml`, `externalLinks/externalLink1.xml` (the defined names `[1]Status!…` then point nowhere), `metadata.xml` (all 30 `cm="1"` dynamic-array cells lose it) and `featurePropertyBag.xml`. It also rewrote `styles.xml` and `workbook.xml` and reset `calcId`. |
| rust_xlsxwriter | MIT/Apache-2.0 | **reject** | Writes new files only. |
| calamine 0.30 | MIT | **adopt (tests)** | Read-only. It is the second reader of every round trip in `tests/roundtrip.rs`. |
| IronCalc 0.9.1 | MIT/Apache-2.0 | **reject (for now)** | On the template, 1102 of 5379 formula cells came out different from Excel's cached results. It has no Danish `TEXT(…,"[tt]:mm")`, and the sheet-scoped name `År` fails. Revisit when it gets locale-aware TEXT. |
| zip 2, chrono, serde | MIT/Apache-2.0 | **adopt** | Already in the workspace. |

The XML tokenizer is our own (`xml.rs`): the patcher needs exact byte spans, and quick-xml 0.38 splits entity references into
separate events.

## The AU timesheet form and totals

`timesheet::detect` finds the template by its English labels, not by fixed cells, so a later version that moves rows still
works:

- the Masterdata fields: start and end dates, year, name, AU ID, carried balance;
- per month: standard hours per week (`B1`), weekly day off (`B2`), and the day rows (code, actual hours, start, end,
  remarks) under the row that has both "Code" and "Date".

It computes the template's own rules in Rust:

- G, standard hours: the weekly hours spread over the weekdays left after `F` days in the same ISO week, including the
  neighbouring months;
- L, worked: `F` → 0, `N` → standard, `R` → actual hours if set, else end − start;
- M, difference;
- R, which rounds to the minute;
- the month total `B4` and the year total Masterdata `D27`, including the carried balance.

Excel still recomputes the real formulas on open; the app shows these values meanwhile.

## API (lucina-media, under `/v1/media`, same origin through p1)

- `GET items/{id}/sheet`: the workbook view.
  - `sheets[]`, each with `name, state, protected, max_row, max_col, frozen, cols[], rows[], merges[], validations[]` (lists
    resolved) and `cells[]` (`ref, r, c, s, t, v, display, f?, editable?, input?`).
  - `styles[]`: `fmt, kind, bold, fill, color, align, wrap, border[t,r,b,l]`.
  - `timesheet`: the form above, or `null`.
  - `hash`: blake3 of the bytes read. `version`, `writable`, `location` (`drive` folder and path, or `library`),
    `download_url`.
  - `415 unsupported` for anything but `.xlsx`, `.xlsm`, `.xltx` and `.xltm`. `.xls`, `.ods` and `.numbers` keep the PDF
    preview.
- `POST items/{id}/sheet {base_hash, edits: [{sheet, cell, value: {t: "input"|"text"|"number"|"bool"|"empty", v}}]}`.
  - `409 conflict {hash}` when the file is no longer `base_hash`.
  - `400 {error: formula_cell|locked_cell|invalid_value|unknown_sheet|bad_cell, message, sheet, cell}`. Nothing is written
    then.
  - `403 read_only`.
  - Otherwise the new view with `changed[] {sheet, cell, before, after}` and the new `version`.
- `GET items/{id}/sheet/versions`: earlier versions, newest first. Each downloads from
  `items/{id}/original?version=N&download=1`.
- `POST items/{id}/sheet/restore {version, base_hash}`: makes an earlier version current. The current bytes become a version
  too, so a restore can be undone.

### Saving and versions

Saves run one at a time.

1. The bytes the owner saw go into the store first, as the blob of the item's previous version (`item_versions`).
2. A Drive file is then replaced in place: a hidden temp file in the same folder (the Drive scan skips hidden files), fsync,
   rename keeping the file's permissions, fsync the folder.
3. It is indexed again at once: same item, new hash, size and mtime.
4. A library file (shared from the iPhone) gets a new blob instead.

A crash at any step leaves either the old file or the new one, and the previous bytes are always in the store.

## Proof on the owner's real file (2026-11-01)

The checks ran on a copy of `Giuseppe_Vitolo_worktime_2026_CraftIT.xlsx` (nezopt `~/Projects/Work/CraftIT/admin`), with a
test month filled in through `xlsx-tool edit`. It is the same code as the API.

- **Structural diff** (`xlsx-tool diff blank.xlsx nov.xlsx`):
  - 88 of 91 parts are byte-identical.
  - `xl/workbook.xml` differs only in `calcPr`.
  - `sheet1.xml` (1 cell) and `sheet12.xml` (14 cells) differ only in the edited `<c>` elements; the rest of each part is
    identical.
  - `tests/roundtrip.rs::au_timesheet` asserts that the unchanged parts keep their compressed bytes and CRCs
    (`AU_TIMESHEET=<path>` turns it on).
- **calamine** reads all 13 sheets, values and formulas, and the edited cells hold the new values.
- **LibreOffice 25** (Debian trixie, headless, da-DK, recalculation forced on load) opens both copies, recomputes, and the
  edited values flow through the formulas.
  - With the employment dates set, all 30 November days match the app's per-day standard, worked and difference hours.
    So do the month total (00:30) and the year total including the carried −01:30 (−01:00).
  - LibreOffice differs from Excel on the template's check `Masterdata!$B$4<>DATE(1900,1,0)`: it gives `DATE(1900,1,0)` = 1,
    Excel gives 0. Without the dates, LibreOffice zeroes the standard hours even in the untouched template. That is a
    LibreOffice/Excel difference, not one of ours.
- **Open XML SDK 3.1** (Microsoft's validator, `FileFormatVersions.Microsoft365`), run on the original and both round trips
  (`nov.xlsx`, and `nov2.xlsx` with the dates, an `F` day, negative and actual-hours rows): **0 errors** on all three. It ran in a
  throwaway .NET container as a verification harness only.
