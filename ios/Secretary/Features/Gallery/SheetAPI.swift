import Foundation

// The spreadsheet editor's wire types and calls (lucina-media `sheet.rs`, docs/xlsx.md): an .xlsx
// from the Drive or the library opens as sheets (`GET /v1/media/items/{id}/sheet`), cell edits go
// back as typed input (`POST …/sheet`), and the server writes only the edited cells into the file.
// The AU worktime template also comes as a form (`timesheet`) with its totals.

/// A cell's raw value: a number, text, a boolean or nothing.
enum SheetScalar: Decodable, Hashable, Sendable {
    case number(Double)
    case text(String)
    case bool(Bool)
    case none

    init(from decoder: Decoder) throws {
        let c = try decoder.singleValueContainer()
        if c.decodeNil() {
            self = .none
        } else if let b = try? c.decode(Bool.self) {
            self = .bool(b)
        } else if let d = try? c.decode(Double.self) {
            self = .number(d)
        } else if let s = try? c.decode(String.self) {
            self = .text(s)
        } else {
            self = .none
        }
    }
}

struct SheetCell: Decodable, Hashable, Sendable {
    let ref: String
    let r: Int
    let c: Int
    /// Index into `SheetWorkbook.styles`.
    let s: Int
    /// n (number), s (text), b, e (error), z (empty).
    let t: String
    let v: SheetScalar
    /// As Excel shows it with the cell's format.
    let display: String
    /// The formula (cut after 240 characters); formula cells are never editable.
    let f: String?
    let editable: Bool?
    /// An unlocked cell of a protected sheet: where the owner is meant to type.
    let input: Bool?

    var isEditable: Bool { editable ?? false }
    var isInput: Bool { input ?? false }
}

struct SheetColumn: Decodable, Hashable, Sendable {
    let min: Int
    let max: Int
    /// In Excel's character units.
    let width: Double?
    let hidden: Bool?
}

struct SheetRowInfo: Decodable, Hashable, Sendable {
    let r: Int
    /// In points.
    let height: Double?
    let hidden: Bool?
}

struct SheetValidation: Decodable, Hashable, Sendable {
    let sqref: String
    let type: String
    /// A list validation's choices, as the server resolved them.
    let list: [String]?
    let strict: Bool?
}

struct SheetFrozen: Decodable, Hashable, Sendable {
    let rows: Int
    let cols: Int
}

struct SheetData: Decodable, Hashable, Identifiable, Sendable {
    let index: Int
    let name: String
    /// visible | hidden | veryHidden
    let state: String
    let protected: Bool
    let maxRow: Int
    let maxCol: Int
    let frozen: SheetFrozen?
    let cols: [SheetColumn]
    let rows: [SheetRowInfo]
    let merges: [String]
    let validations: [SheetValidation]
    let cells: [SheetCell]

    var id: Int { index }
    var isVisible: Bool { state == "visible" }

    private enum CodingKeys: String, CodingKey {
        case index, name, state, protected, frozen, cols, rows, merges, validations, cells
        case maxRow = "max_row"
        case maxCol = "max_col"
    }
}

struct SheetStyle: Decodable, Hashable, Sendable {
    let fmt: String
    /// general | number | percent | date | time | duration | date_time | text
    let kind: String
    let bold: Bool?
    let italic: Bool?
    /// `#RRGGBB`.
    let color: String?
    let fill: String?
    let align: String?
    let wrap: Bool?
    /// Top, right, bottom, left.
    let border: [Bool]?
}

/// One input of the timesheet form: where it lives and how to ask for it.
struct SheetField: Decodable, Hashable, Sendable {
    let key: String
    let label: String
    let sheet: String
    let cell: String
    /// date | time | duration | number | text | bool
    let kind: String
    /// Dates as yyyy-mm-dd, times as hh:mm, booleans as yes/no.
    let value: String
    let editable: Bool
}

struct SheetDay: Decodable, Hashable, Identifiable, Sendable {
    let row: Int
    /// yyyy-mm-dd
    let date: String
    let weekday: String
    let holiday: String?
    let workday: Bool
    let employed: Bool
    let code: String
    /// hh:mm, empty when zero.
    let actual: String
    let start: String
    let end: String
    let remarks: String
    let norm: String
    let worked: String
    let diff: String

    var id: Int { row }
}

struct SheetColumns: Decodable, Hashable, Sendable {
    let code: String
    let actual: String
    let start: String
    let end: String
    let remarks: String
}

struct SheetMonth: Decodable, Hashable, Identifiable, Sendable {
    let sheet: String
    let month: Int
    let year: Int
    let weeklyHours: SheetField
    let dayOff: SheetField?
    let columns: SheetColumns
    let codes: [String]
    let days: [SheetDay]
    let norm: String
    let worked: String
    let diff: String
    let registeredDays: Int

    var id: String { sheet }

    private enum CodingKeys: String, CodingKey {
        case sheet, month, year, columns, codes, days, norm, worked, diff
        case weeklyHours = "weekly_hours"
        case dayOff = "day_off"
        case registeredDays = "registered_days"
    }
}

/// The AU worktime registration form and the totals Secretary computes with the template's rules.
struct Timesheet: Decodable, Hashable, Sendable {
    let kind: String
    let masterSheet: String
    let master: [SheetField]
    let months: [SheetMonth]
    let balanceCarried: String
    let diff: String
    let total: String

    private enum CodingKeys: String, CodingKey {
        case kind, master, months, diff, total
        case masterSheet = "master_sheet"
        case balanceCarried = "balance_carried"
    }
}

struct SheetChange: Decodable, Hashable, Sendable {
    let sheet: String
    let cell: String
    let before: String
    let after: String
}

/// `GET /v1/media/items/{id}/sheet`, and the answer to a save.
struct SheetWorkbook: Decodable, Hashable, Sendable {
    let itemId: String
    let name: String
    /// blake3 of the bytes the server read: a save names it so nobody's edits are lost.
    let hash: String
    let version: Int
    let writable: Bool
    let sheets: [SheetData]
    let styles: [SheetStyle]
    let timesheet: Timesheet?
    let fullCalcOnLoad: Bool
    let downloadUrl: String?
    let changed: [SheetChange]?

    private enum CodingKeys: String, CodingKey {
        case name, hash, version, writable, sheets, styles, timesheet, changed
        case itemId = "item_id"
        case fullCalcOnLoad = "full_calc_on_load"
        case downloadUrl = "download_url"
    }

    func style(_ index: Int) -> SheetStyle? {
        styles.indices.contains(index) ? styles[index] : nil
    }
}

struct SheetVersion: Decodable, Hashable, Identifiable, Sendable {
    let version: Int
    let hash: String
    let replacedAt: String
    let bytes: Int?
    let downloadUrl: String

    var id: Int { version }

    private enum CodingKeys: String, CodingKey {
        case version, hash, bytes
        case replacedAt = "replaced_at"
        case downloadUrl = "download_url"
    }
}

struct SheetVersions: Decodable, Hashable, Sendable {
    let versions: [SheetVersion]
}

/// One edit as the server takes it: what the owner typed, read like typing into Excel.
struct SheetEdit: Encodable, Hashable, Sendable {
    struct Value: Encodable, Hashable, Sendable {
        let t: String
        let v: String?

        func encode(to encoder: Encoder) throws {
            var c = encoder.container(keyedBy: CodingKeys.self)
            try c.encode(t, forKey: .t)
            try c.encodeIfPresent(v, forKey: .v)
        }

        private enum CodingKeys: String, CodingKey { case t, v }
    }

    let sheet: String
    let cell: String
    let value: Value

    /// Empty text clears the cell (it keeps its style).
    init(sheet: String, cell: String, text: String) {
        self.sheet = sheet
        self.cell = cell
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        value = trimmed.isEmpty ? Value(t: "empty", v: nil) : Value(t: "input", v: text)
    }
}

enum SheetSaveError: Error, Equatable, LocalizedError {
    /// The file changed since it was opened (another device, or someone in the folder).
    case conflict
    /// The server refused an edit; nothing was written.
    case refused(code: String, message: String, sheet: String?, cell: String?)

    var errorDescription: String? {
        switch self {
        case .conflict:
            String(localized: "The file changed since you opened it. Reload it and save again.")
        case let .refused(_, message, _, _):
            message
        }
    }
}

extension GalleryAPI {
    private static func sheetPath(_ id: String) -> String { "/v1/media/items/\(MediaAPI.encode(id))/sheet" }

    func sheet(id: String) async throws -> SheetWorkbook {
        try await client.get(Self.sheetPath(id), timeout: 60)
    }

    private struct SaveBody: Encodable {
        let baseHash: String
        let edits: [SheetEdit]
        private enum CodingKeys: String, CodingKey {
            case edits
            case baseHash = "base_hash"
        }
    }

    private struct RestoreBody: Encodable {
        let version: Int
        let baseHash: String
        private enum CodingKeys: String, CodingKey {
            case version
            case baseHash = "base_hash"
        }
    }

    private struct ErrorBody: Decodable {
        let error: String?
        let message: String?
        let sheet: String?
        let cell: String?
    }

    /// Saves the edits. 409 → `.conflict`, 400/403/422 → `.refused` with the cell the server named.
    func saveSheet(id: String, baseHash: String, edits: [SheetEdit]) async throws -> SheetWorkbook {
        try await post(Self.sheetPath(id), body: SaveBody(baseHash: baseHash, edits: edits))
    }

    func sheetVersions(id: String) async throws -> SheetVersions {
        try await client.get(Self.sheetPath(id) + "/versions")
    }

    func restoreSheet(id: String, version: Int, baseHash: String) async throws -> SheetWorkbook {
        try await post(Self.sheetPath(id) + "/restore", body: RestoreBody(version: version, baseHash: baseHash))
    }

    private func post<B: Encodable>(_ path: String, body: B) async throws -> SheetWorkbook {
        let data = try APIClient.encoder.encode(body)
        let response = try await client.send("POST", path, body: data, timeout: 60, passThrough: [400, 403, 409, 422],
                                             idempotent: false)
        switch response.status {
        case 200..<300:
            return try client.decode(SheetWorkbook.self, from: response)
        case 409:
            throw SheetSaveError.conflict
        default:
            let e = try? APIClient.decoder.decode(ErrorBody.self, from: response.data)
            throw SheetSaveError.refused(code: e?.error ?? "http_\(response.status)", message: e?.message ?? "",
                                         sheet: e?.sheet, cell: e?.cell)
        }
    }
}

// MARK: - Logic (no UI)

enum SheetRefs {
    /// 1 → A, 27 → AA.
    static func column(_ n: Int) -> String {
        var n = n
        var out = ""
        while n > 0 {
            let r = (n - 1) % 26
            out = String(UnicodeScalar(UInt8(65 + r))) + out
            n = (n - 1) / 26
        }
        return out
    }

    /// `B12` → (12, 2); `$B$12` too.
    static func parse(_ ref: String) -> (row: Int, col: Int)? {
        var col = 0
        var digits = ""
        for ch in ref where ch != "$" {
            if let a = ch.asciiValue, ch.isLetter, digits.isEmpty {
                col = col * 26 + Int(a & 0xDF) - 64
            } else if ch.isNumber {
                digits.append(ch)
            } else {
                return nil
            }
        }
        guard col > 0, let row = Int(digits), row > 0 else { return nil }
        return (row, col)
    }

    /// `A1:B3` or `A1` → the rectangle.
    static func range(_ text: String) -> (r1: Int, c1: Int, r2: Int, c2: Int)? {
        let parts = text.split(separator: ":").map(String.init)
        guard let a = parse(parts[0]) else { return nil }
        let b = parts.count > 1 ? parse(parts[1]) : a
        guard let b else { return nil }
        return (min(a.row, b.row), min(a.col, b.col), max(a.row, b.row), max(a.col, b.col))
    }

    static func contains(_ sqref: String, row: Int, col: Int) -> Bool {
        sqref.split(separator: " ").contains { part in
            guard let r = range(String(part)) else { return false }
            return (r.r1...r.r2).contains(row) && (r.c1...r.c2).contains(col)
        }
    }
}

/// One sheet laid out for drawing: visible rows and columns, sizes, merges and cell lookup.
struct SheetGrid: Sendable {
    struct Span: Hashable, Sendable {
        let rows: Int
        let cols: Int
    }

    let sheet: SheetData
    let rows: [Int]
    let cols: [Int]
    private let cells: [Int: SheetCell]
    /// Top-left cells of merges and their size in visible rows/columns.
    private let spans: [Int: Span]
    /// Cells a merge covers (not drawn).
    private let covered: Set<Int>
    private let widths: [Int: CGFloat]
    private let heights: [Int: CGFloat]

    /// The template is about 80 × 45; generic workbooks are cut here.
    static let maxRows = 2000
    static let maxCols = 200

    private static func key(_ r: Int, _ c: Int) -> Int { r * 20_000 + c }

    init(_ sheet: SheetData) {
        self.sheet = sheet
        var hiddenCols = Set<Int>()
        var widths: [Int: CGFloat] = [:]
        let lastCol = min(max(sheet.maxCol, 1), Self.maxCols)
        for col in sheet.cols {
            for c in col.min...min(col.max, lastCol) where c >= 1 {
                if col.hidden == true { hiddenCols.insert(c) }
                // Excel's width is in characters of the default font: about 7 pt each plus padding.
                if let w = col.width { widths[c] = CGFloat(max(w, 1) * 7 + 5) }
            }
        }
        var hiddenRows = Set<Int>()
        var heights: [Int: CGFloat] = [:]
        for row in sheet.rows {
            if row.hidden == true { hiddenRows.insert(row.r) }
            if let h = row.height { heights[row.r] = CGFloat(h * 4 / 3) }
        }
        let lastRow = min(max(sheet.maxRow, 1), Self.maxRows)
        rows = (1...lastRow).filter { !hiddenRows.contains($0) }
        cols = (1...lastCol).filter { !hiddenCols.contains($0) }
        self.widths = widths
        self.heights = heights
        var cells: [Int: SheetCell] = [:]
        for cell in sheet.cells { cells[Self.key(cell.r, cell.c)] = cell }
        self.cells = cells
        var spans: [Int: Span] = [:]
        var covered = Set<Int>()
        let visibleRows = Set(rows)
        let visibleCols = Set(cols)
        for merge in sheet.merges {
            guard let m = SheetRefs.range(merge) else { continue }
            let rs = (m.r1...m.r2).filter { visibleRows.contains($0) }.count
            let cs = (m.c1...m.c2).filter { visibleCols.contains($0) }.count
            spans[Self.key(m.r1, m.c1)] = Span(rows: max(rs, 1), cols: max(cs, 1))
            for r in m.r1...m.r2 {
                for c in m.c1...m.c2 where !(r == m.r1 && c == m.c1) { covered.insert(Self.key(r, c)) }
            }
        }
        self.spans = spans
        self.covered = covered
    }

    func cell(_ row: Int, _ col: Int) -> SheetCell? { cells[Self.key(row, col)] }
    func span(_ row: Int, _ col: Int) -> Span? { spans[Self.key(row, col)] }
    func isCovered(_ row: Int, _ col: Int) -> Bool { covered.contains(Self.key(row, col)) }
    func width(_ col: Int) -> CGFloat { widths[col] ?? 64 }
    func height(_ row: Int) -> CGFloat { max(heights[row] ?? 20, 18) }

    /// The width of a merge starting at `col` (its visible columns).
    func width(from col: Int, span: Int) -> CGFloat {
        guard let start = cols.firstIndex(of: col) else { return width(col) }
        return cols[start..<min(start + span, cols.count)].reduce(0) { $0 + width($1) }
    }

    /// The choices of a list validation that covers the cell.
    func choices(_ row: Int, _ col: Int) -> [String]? {
        sheet.validations.first { $0.type == "list" && $0.list != nil && SheetRefs.contains($0.sqref, row: row, col: col) }?.list
    }

    var truncated: Bool { sheet.maxRow > Self.maxRows || sheet.maxCol > Self.maxCols }
}

/// The owner's unsaved edits: `Sheet!A1` → what he typed, against what the cell showed when loaded.
struct SheetDraft: Hashable, Sendable {
    private(set) var texts: [String: String] = [:]
    private(set) var originals: [String: String] = [:]
    private var order: [String] = []

    static func key(_ sheet: String, _ cell: String) -> String { "\(sheet)!\(cell)" }

    var isEmpty: Bool { texts.isEmpty }
    var count: Int { texts.count }

    func text(_ sheet: String, _ cell: String) -> String? { texts[Self.key(sheet, cell)] }

    /// Typing the original value back removes the edit.
    mutating func set(_ sheet: String, _ cell: String, text: String, original: String) {
        let k = Self.key(sheet, cell)
        if originals[k] == nil { originals[k] = original }
        if text == originals[k] {
            texts[k] = nil
            originals[k] = nil
            order.removeAll { $0 == k }
        } else {
            if texts[k] == nil { order.append(k) }
            texts[k] = text
        }
    }

    mutating func clear() {
        texts = [:]
        originals = [:]
        order = []
    }

    /// The edits in the order they were first made.
    var edits: [SheetEdit] {
        order.compactMap { k in
            guard let text = texts[k], let bang = k.lastIndex(of: "!") else { return nil }
            return SheetEdit(sheet: String(k[..<bang]), cell: String(k[k.index(after: bang)...]), text: text)
        }
    }
}

extension SheetMonth {
    /// The cell of a day's column (`J` + 10 → `J10`).
    func cell(_ column: KeyPath<SheetColumns, String>, _ day: SheetDay) -> String {
        "\(columns[keyPath: column])\(day.row)"
    }

    /// The codes a day may take. The server's list follows the saved weekly-day-off answer (`F` only
    /// while it is yes); an answer typed but not saved moves `F` too, and the cell's own value stays
    /// selectable.
    func codes(dayOff typed: String?, current: String) -> [String] {
        var list = codes
        if typed == "yes", !list.contains("F") { list.insert("F", at: 0) }
        if typed == "no" { list.removeAll { $0 == "F" } }
        if !current.isEmpty, !list.contains(current) { list.append(current) }
        return list
    }

    /// January…December in the device's language.
    var title: String {
        var c = DateComponents()
        c.year = year
        c.month = month
        c.day = 1
        guard let date = Calendar(identifier: .gregorian).date(from: c) else { return sheet }
        return date.formatted(.dateTime.month(.wide))
    }
}

extension Timesheet {
    /// The month to show first: this month when the sheet has it, else the first.
    func startMonth(now: Date = Date()) -> SheetMonth? {
        let c = Calendar(identifier: .gregorian).dateComponents([.year, .month], from: now)
        return months.first { $0.month == c.month && $0.year == c.year } ?? months.first { $0.month == c.month } ?? months.first
    }
}

extension SheetDay {
    var dateValue: Date? {
        let f = DateFormatter()
        f.calendar = Calendar(identifier: .gregorian)
        f.locale = Locale(identifier: "en_US_POSIX")
        f.dateFormat = "yyyy-MM-dd"
        return f.date(from: date)
    }

    var isNegative: Bool { diff.hasPrefix("-") }
    var isZeroDiff: Bool { diff == "00:00" }
}
