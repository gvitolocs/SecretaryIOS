import XCTest
@testable import Secretary

/// The spreadsheet editor's logic and wire format (docs/xlsx.md).
final class SheetTests: XCTestCase {
    private func decode<T: Decodable>(_ type: T.Type, _ json: String) throws -> T {
        try APIClient.decoder.decode(T.self, from: Data(json.utf8))
    }

    private let sheetJSON = #"""
    {"index":0,"name":"November","state":"visible","protected":true,"max_row":6,"max_col":6,"frozen":null,
     "cols":[{"min":2,"max":3,"width":10,"hidden":true},{"min":4,"max":4,"width":20,"hidden":false}],
     "rows":[{"r":3,"height":30,"hidden":false},{"r":4,"height":null,"hidden":true}],
     "merges":["E2:F3"],
     "validations":[{"sqref":"A1:A3 D5","type":"list","operator":"between","strict":true,"list":["N","R"]},
                    {"sqref":"D1","type":"time","operator":"between","strict":true}],
     "cells":[{"ref":"A1","r":1,"c":1,"s":0,"t":"s","v":"x","display":"x"},
              {"ref":"D5","r":5,"c":4,"s":1,"t":"n","v":0.375,"display":"9:00","editable":true,"input":true},
              {"ref":"F5","r":5,"c":6,"s":0,"t":"b","v":true,"display":"TRUE","f":"A1=1"},
              {"ref":"E6","r":6,"c":5,"s":0,"t":"z","v":null,"display":""}],
     "truncated":0}
    """#

    func testWorkbookDecodes() throws {
        let sheet = try decode(SheetData.self, sheetJSON)
        let json = #"{"item_id":"itm_1","name":"t.xlsx","hash":"h1","version":2,"writable":true,"sheets":[\#(sheetJSON)],"#
            + ##""styles":[{"fmt":"General","kind":"general","border":[false,false,false,false]},{"fmt":"h:mm","kind":"time","bold":true,"fill":"#FFFF00","border":[true,true,true,true]}],"##
            + #""timesheet":null,"full_calc_on_load":true,"download_url":"/x","defined_names":[],"location":{"kind":"library"}}"#
        let book = try decode(SheetWorkbook.self, json)
        XCTAssertEqual(book.itemId, "itm_1")
        XCTAssertTrue(book.fullCalcOnLoad)
        XCTAssertEqual(book.style(1)?.fill, "#FFFF00")
        XCTAssertNil(book.style(9))
        XCTAssertNil(book.changed)
        XCTAssertEqual(sheet.cells[1].v, .number(0.375))
        XCTAssertEqual(sheet.cells[2].v, .bool(true))
        XCTAssertEqual(sheet.cells[3].v, .none)
        XCTAssertTrue(sheet.cells[1].isInput)
        XCTAssertFalse(sheet.cells[2].isEditable)
    }

    func testReferences() {
        XCTAssertEqual(SheetRefs.column(1), "A")
        XCTAssertEqual(SheetRefs.column(28), "AB")
        XCTAssertEqual(SheetRefs.parse("$J$10")?.row, 10)
        XCTAssertEqual(SheetRefs.parse("J10")?.col, 10)
        XCTAssertNil(SheetRefs.parse("10"))
        XCTAssertNil(SheetRefs.parse("J"))
        XCTAssertTrue(SheetRefs.contains("A1 I9:K38", row: 20, col: 10))
        XCTAssertFalse(SheetRefs.contains("I9:K38", row: 20, col: 12))
    }

    func testGridSkipsHiddenAndHandlesMerges() throws {
        let grid = SheetGrid(try decode(SheetData.self, sheetJSON))
        XCTAssertEqual(grid.cols, [1, 4, 5, 6])
        XCTAssertEqual(grid.rows, [1, 2, 3, 5, 6])
        XCTAssertEqual(grid.width(4), 145)
        XCTAssertEqual(grid.height(3), 40)
        XCTAssertEqual(grid.span(2, 5), SheetGrid.Span(rows: 2, cols: 2))
        XCTAssertTrue(grid.isCovered(3, 6))
        XCTAssertFalse(grid.isCovered(2, 5))
        XCTAssertEqual(grid.width(from: 5, span: 2), grid.width(5) + grid.width(6))
        XCTAssertEqual(grid.choices(2, 1), ["N", "R"])
        XCTAssertNil(grid.choices(1, 4))
        XCTAssertEqual(grid.cell(5, 4)?.display, "9:00")
    }

    func testDraftKeepsOnlyRealChanges() throws {
        var d = SheetDraft()
        d.set("Nov", "F10", text: "R", original: "N")
        d.set("Nov", "J10", text: "9:00", original: "")
        XCTAssertEqual(d.count, 2)
        // Back to the original: no edit left.
        d.set("Nov", "J10", text: "", original: "")
        XCTAssertEqual(d.count, 1)
        d.set("Nov", "F10", text: "F", original: "N")
        d.set("Nov", "S10", text: "  ", original: "old")
        let edits = d.edits
        XCTAssertEqual(edits.map(\.cell), ["F10", "S10"])
        XCTAssertEqual(edits[0].value, .init(t: "input", v: "F"))
        XCTAssertEqual(edits[1].value, .init(t: "empty", v: nil), "blank clears the cell")
        let json = String(decoding: try APIClient.encoder.encode(edits[1]), as: UTF8.self)
        XCTAssertEqual(json, #"{"cell":"S10","sheet":"Nov","value":{"t":"empty"}}"#)
        d.clear()
        XCTAssertTrue(d.isEmpty)
    }

    private func month(codes: String) throws -> SheetMonth {
        let field = #"{"key":"k","label":"L","sheet":"Nov","cell":"B1","kind":"number","value":"37","editable":true}"#
        return try decode(SheetMonth.self, #"""
        {"sheet":"Nov","month":11,"year":2026,"weekly_hours":\#(field),"day_off":null,
         "columns":{"code":"F","actual":"I","start":"J","end":"K","remarks":"S"},"codes":\#(codes),
         "days":[{"row":10,"date":"2026-11-02","weekday":"Monday","holiday":null,"workday":true,"employed":true,"code":"N",
                  "actual":"","start":"","end":"","remarks":"","norm":"07:24","worked":"07:24","diff":"-00:09"}],
         "norm":"1","worked":"2","diff":"3","registered_days":0}
        """#)
    }

    func testMonthCellsAndCodes() throws {
        let m = try month(codes: #"["N","R"]"#)
        XCTAssertEqual(m.cell(\.start, m.days[0]), "J10")
        XCTAssertTrue(m.days[0].isNegative)
        XCTAssertEqual(m.codes(dayOff: nil, current: "N"), ["N", "R"])
        XCTAssertEqual(m.codes(dayOff: "yes", current: "N"), ["F", "N", "R"])
        let off = try month(codes: #"["F","N","R"]"#)
        XCTAssertEqual(off.codes(dayOff: "no", current: "N"), ["N", "R"])
        XCTAssertEqual(off.codes(dayOff: "no", current: "F"), ["N", "R", "F"], "the cell's own value stays")
    }

    func testOnlyWorkbooksOpenAsSheets() throws {
        func item(_ name: String, kind: String = "file") throws -> GalleryItem {
            try decode(GalleryItem.self, #"{"id":"i","kind":"\#(kind)","title":"\#(name)","urls":{"original":"/o"}}"#)
        }
        XCTAssertEqual(DriveOpen.decide(try item("a.XLSX")), .sheet)
        XCTAssertEqual(DriveOpen.decide(try item("a.xlsm")), .sheet)
        XCTAssertNotEqual(DriveOpen.decide(try item("a.xls")), .sheet)
        XCTAssertNotEqual(DriveOpen.decide(try item("a.ods")), .sheet)
        XCTAssertNotEqual(DriveOpen.decide(try item("xlsx")), .sheet)
    }
}
