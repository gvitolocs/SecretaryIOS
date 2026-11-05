import SwiftUI

// The spreadsheet editor (docs/xlsx.md): every sheet as a grid with the input cells marked, and for
// the AU worktime template a form per month (code, start, end, actual hours, remarks) with the
// totals Secretary computes. Edits stay on the phone until Save; the server writes only those cells
// into the file and keeps the previous bytes as a version.

struct SheetScreen: View {
    @Environment(GalleryStore.self) private var store
    @Environment(\.dismiss) private var dismiss
    let item: GalleryItem

    enum Mode: Hashable { case form, grid }

    @State private var workbook: SheetWorkbook?
    @State private var loadError: String?
    @State private var draft = SheetDraft()
    @State private var mode: Mode = .form
    @State private var saving = false
    @State private var conflict = false
    /// A refused edit: the cell it names and the server's words.
    @State private var refused: (key: String, message: String)?
    @State private var saveError: String?
    @State private var savedNote: String?
    @State private var showVersions = false
    @State private var confirmDiscard = false

    var body: some View {
        content
            .navigationTitle(item.driveName)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { toolbar }
            .task { if workbook == nil { await load() } }
            .interactiveDismissDisabled(!draft.isEmpty)
            .sheet(isPresented: $showVersions) {
                if let workbook {
                    NavigationStack { SheetVersionsView(workbook: workbook) { restored in apply(restored) } }
                        .environment(store)
                        .presentationDetents([.medium, .large])
                }
            }
            .alert("The file changed", isPresented: $conflict) {
                Button("Reload and keep my edits") { Task { await load(keepDraft: true) } }
                Button("Cancel", role: .cancel) {}
            } message: {
                Text("Someone saved this file since you opened it. Reload it, check your edits and save again.")
            }
            .alert("Couldn't save", isPresented: Binding(get: { saveError != nil }, set: { if !$0 { saveError = nil } })) {
                Button("OK", role: .cancel) {}
            } message: {
                Text(verbatim: saveError ?? "")
            }
            .confirmationDialog("Discard your changes?", isPresented: $confirmDiscard, titleVisibility: .visible) {
                Button("Discard changes", role: .destructive) {
                    draft.clear()
                    refused = nil
                }
            }
    }

    @ToolbarContentBuilder
    private var toolbar: some ToolbarContent {
        ToolbarItem(placement: .cancellationAction) {
            Button("Close") {
                if draft.isEmpty { dismiss() } else { confirmDiscard = true }
            }
        }
        ToolbarItemGroup(placement: .primaryAction) {
            if workbook != nil {
                if saving {
                    ProgressView()
                } else {
                    Button("Save") { Task { await save() } }
                        .bold()
                        .disabled(draft.isEmpty || workbook?.writable == false)
                        .accessibilityIdentifier("sheet-save")
                }
                Menu {
                    Button { showVersions = true } label: { Label("Versions", systemImage: "clock.arrow.circlepath") }
                    if !draft.isEmpty {
                        Button(role: .destructive) { confirmDiscard = true } label: {
                            Label("Discard changes", systemImage: "arrow.uturn.backward")
                        }
                    }
                } label: {
                    Image(systemName: "ellipsis.circle")
                }
                .accessibilityLabel("More")
                DriveFileActions(item: item)
            }
        }
    }

    @ViewBuilder
    private var content: some View {
        if let workbook {
            VStack(spacing: 0) {
                if workbook.timesheet != nil {
                    Picker("View", selection: $mode) {
                        Text("Form").tag(Mode.form)
                        Text("Sheets").tag(Mode.grid)
                    }
                    .pickerStyle(.segmented)
                    .padding(.horizontal, LucinaTokens.Space.gutter)
                    .padding(.vertical, LucinaTokens.Space.sm)
                }
                status(workbook)
                if let timesheet = workbook.timesheet, mode == .form {
                    TimesheetForm(timesheet: timesheet, draft: $draft, refusedKey: refused?.key)
                } else {
                    SheetGridScreen(workbook: workbook, draft: $draft, refusedKey: refused?.key)
                }
            }
            .background(LucinaTokens.Colors.bg)
        } else if let loadError {
            ContentUnavailableView {
                Label("Couldn't open the sheet", systemImage: "tablecells.badge.ellipsis")
            } description: {
                Text(verbatim: loadError)
            } actions: {
                Button("Retry") { Task { await load() } }
                DriveFileActions(item: item, labeled: true)
            }
        } else {
            ProgressView("Opening the sheet…").frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }

    @ViewBuilder
    private func status(_ workbook: SheetWorkbook) -> some View {
        if let refused {
            SheetBanner(text: refused.message, systemImage: "exclamationmark.triangle.fill", tint: LucinaTokens.Colors.negative)
        } else if !workbook.writable {
            SheetBanner(text: String(localized: "This file is read-only on the server."), systemImage: "lock.fill",
                        tint: LucinaTokens.Colors.warning)
        } else if !draft.isEmpty {
            SheetBanner(text: String(localized: "\(draft.count) unsaved change(s)"), systemImage: "pencil",
                        tint: LucinaTokens.Colors.accent)
        } else if let savedNote {
            SheetBanner(text: savedNote, systemImage: "checkmark.circle.fill", tint: LucinaTokens.Colors.positive)
        }
    }

    private func load(keepDraft: Bool = false) async {
        loadError = nil
        do {
            let w = try await store.api().sheet(id: item.id)
            if !keepDraft { draft.clear() }
            refused = nil
            apply(w, note: nil)
        } catch is CancellationError {
        } catch {
            if workbook == nil { loadError = error.localizedDescription } else { saveError = error.localizedDescription }
        }
    }

    private func apply(_ w: SheetWorkbook, note: String? = nil) {
        if workbook?.timesheet == nil, w.timesheet == nil { mode = .grid }
        workbook = w
        savedNote = note
    }

    private func save() async {
        guard let workbook, !draft.isEmpty, !saving else { return }
        saving = true
        defer { saving = false }
        refused = nil
        do {
            let w = try await store.api().saveSheet(id: workbook.itemId, baseHash: workbook.hash, edits: draft.edits)
            draft.clear()
            let n = w.changed?.count ?? 0
            apply(w, note: String(localized: "Saved \(n) cell(s) · version \(w.version)"))
        } catch SheetSaveError.conflict {
            conflict = true
        } catch let SheetSaveError.refused(_, message, sheet, cell) {
            if let sheet, let cell {
                refused = (SheetDraft.key(sheet, cell), "\(sheet)!\(cell): \(message)")
            } else {
                saveError = message
            }
        } catch is CancellationError {
        } catch {
            saveError = error.localizedDescription
        }
    }
}

private struct SheetBanner: View {
    let text: String
    let systemImage: String
    let tint: Color

    var body: some View {
        Label { Text(verbatim: text) } icon: { Image(systemName: systemImage).foregroundStyle(tint) }
            .font(LucinaTokens.Fonts.footnote)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, LucinaTokens.Space.gutter)
            .padding(.vertical, LucinaTokens.Space.xs)
            .background(tint.opacity(0.08))
    }
}

// MARK: - Timesheet form

struct TimesheetForm: View {
    let timesheet: Timesheet
    @Binding var draft: SheetDraft
    let refusedKey: String?
    @State private var monthSheet: String?
    @State private var showMaster = false

    private var month: SheetMonth? {
        timesheet.months.first { $0.sheet == monthSheet } ?? timesheet.startMonth()
    }

    var body: some View {
        VStack(spacing: 0) {
            ScrollViewReader { proxy in
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: LucinaTokens.Space.sm) {
                        ForEach(timesheet.months) { m in
                            Button { monthSheet = m.sheet } label: {
                                Text(verbatim: m.title)
                                    .font(LucinaTokens.Fonts.subheadline)
                                    .padding(.horizontal, LucinaTokens.Space.md)
                                    .padding(.vertical, LucinaTokens.Space.xs)
                                    .background(m.sheet == month?.sheet ? LucinaTokens.Colors.accentSoft : LucinaTokens.Colors.field,
                                                in: Capsule())
                            }
                            .buttonStyle(.plain)
                            .id(m.sheet)
                            .accessibilityAddTraits(m.sheet == month?.sheet ? .isSelected : [])
                        }
                    }
                    .padding(.horizontal, LucinaTokens.Space.gutter)
                    .padding(.vertical, LucinaTokens.Space.sm)
                }
                .onAppear { if let m = month { proxy.scrollTo(m.sheet, anchor: .center) } }
            }
            if let month {
                List {
                    Section {
                        DisclosureGroup(isExpanded: $showMaster) {
                            ForEach(timesheet.master, id: \.cell) { f in
                                SheetFieldRow(field: f, draft: $draft, refused: refusedKey == SheetDraft.key(f.sheet, f.cell))
                            }
                        } label: {
                            Label(timesheet.masterSheet, systemImage: "person.text.rectangle")
                        }
                        SheetFieldRow(field: month.weeklyHours, draft: $draft,
                                      refused: refusedKey == SheetDraft.key(month.sheet, month.weeklyHours.cell))
                        if let off = month.dayOff {
                            SheetFieldRow(field: off, draft: $draft, refused: refusedKey == SheetDraft.key(month.sheet, off.cell))
                        }
                    }
                    Section {
                        ForEach(month.days) { day in
                            TimesheetDayRow(month: month, day: day, draft: $draft, refusedKey: refusedKey)
                        }
                    } header: {
                        Text(verbatim: "\(month.title) \(month.year)")
                    }
                    Section {
                        LabeledContent("Standard hours", value: month.norm)
                        LabeledContent("Worked hours", value: month.worked)
                        LabeledContent("Difference this month", value: month.diff)
                        LabeledContent("Carried from earlier periods", value: timesheet.balanceCarried)
                        LabeledContent("Total difference this year", value: timesheet.total)
                            .bold()
                    } footer: {
                        Text("Totals are computed by Secretary with the template's rules and update after Save. Excel recomputes every formula when the file is opened.")
                    }
                }
                .listStyle(.insetGrouped)
                .scrollDismissesKeyboard(.interactively)
            }
        }
    }
}

/// A Masterdata or month field, edited by its kind.
private struct SheetFieldRow: View {
    let field: SheetField
    @Binding var draft: SheetDraft
    let refused: Bool

    private var text: String { draft.text(field.sheet, field.cell) ?? field.value }

    private func set(_ value: String) {
        draft.set(field.sheet, field.cell, text: value, original: field.value)
    }

    var body: some View {
        Group {
            switch field.kind {
            case "bool":
                Toggle(isOn: Binding(get: { text == "yes" }, set: { set($0 ? "yes" : "no") })) {
                    Text(verbatim: field.label)
                }
            case "date":
                SheetDateField(label: field.label, text: Binding(get: { text }, set: set))
            default:
                LabeledContent {
                    TextField(field.kind == "text" ? "" : "0", text: Binding(get: { text }, set: set))
                        .multilineTextAlignment(.trailing)
                        .keyboardType(field.kind == "number" ? .decimalPad : .default)
                        .textInputAutocapitalization(field.key == "name" ? .words : .never)
                        .autocorrectionDisabled()
                } label: {
                    Text(verbatim: field.label).lineLimit(3)
                }
            }
        }
        .disabled(!field.editable)
        .listRowBackground(refused ? LucinaTokens.Colors.negative.opacity(0.12) : nil)
        .accessibilityIdentifier("sheet-field-\(field.key)")
    }
}

/// A date the server reads as yyyy-mm-dd; empty is "not set".
private struct SheetDateField: View {
    let label: String
    @Binding var text: String

    private static let format: DateFormatter = {
        let f = DateFormatter()
        f.calendar = Calendar(identifier: .gregorian)
        f.locale = Locale(identifier: "en_US_POSIX")
        f.dateFormat = "yyyy-MM-dd"
        return f
    }()

    var body: some View {
        if let date = Self.format.date(from: text) {
            HStack {
                DatePicker(selection: Binding(get: { date }, set: { text = Self.format.string(from: $0) }),
                           displayedComponents: .date) {
                    Text(verbatim: label).lineLimit(3)
                }
                Button { text = "" } label: { Image(systemName: "xmark.circle.fill") }
                    .buttonStyle(.borderless)
                    .foregroundStyle(LucinaTokens.Colors.textTertiary)
                    .accessibilityLabel("Clear")
            }
        } else {
            LabeledContent {
                Button("Set date") { text = Self.format.string(from: Date()) }
            } label: {
                Text(verbatim: label).lineLimit(3)
            }
        }
    }
}

struct TimesheetDayRow: View {
    let month: SheetMonth
    let day: SheetDay
    @Binding var draft: SheetDraft
    let refusedKey: String?

    private func value(_ column: KeyPath<SheetColumns, String>, _ original: String) -> String {
        draft.text(month.sheet, month.cell(column, day)) ?? original
    }

    private func binding(_ column: KeyPath<SheetColumns, String>, _ original: String) -> Binding<String> {
        Binding(get: { value(column, original) },
                set: { draft.set(month.sheet, month.cell(column, day), text: $0, original: original) })
    }

    private var code: String { value(\.code, day.code) }
    private var edited: Bool {
        [(\SheetColumns.code, day.code), (\.start, day.start), (\.end, day.end), (\.actual, day.actual), (\.remarks, day.remarks)]
            .contains { draft.text(month.sheet, month.cell($0.0, day)) != nil }
    }
    private func isRefused(_ column: KeyPath<SheetColumns, String>) -> Bool {
        refusedKey == SheetDraft.key(month.sheet, month.cell(column, day))
    }

    private var heading: String {
        guard let d = day.dateValue else { return day.date }
        return d.formatted(.dateTime.weekday(.abbreviated).day().month(.abbreviated))
    }

    var body: some View {
        VStack(alignment: .leading, spacing: LucinaTokens.Space.sm) {
            HStack(alignment: .firstTextBaseline) {
                Text(verbatim: heading)
                    .font(LucinaTokens.Fonts.headline)
                    .foregroundStyle(day.workday ? LucinaTokens.Colors.text : LucinaTokens.Colors.textSecondary)
                if let holiday = day.holiday {
                    Text(verbatim: holiday)
                        .font(LucinaTokens.Fonts.caption)
                        .foregroundStyle(LucinaTokens.Colors.warning)
                }
                Spacer()
                if edited {
                    Image(systemName: "pencil.circle.fill").foregroundStyle(LucinaTokens.Colors.accent)
                        .accessibilityLabel("Edited")
                }
                Picker("Code", selection: binding(\.code, day.code)) {
                    ForEach(choices, id: \.self) { Text(verbatim: $0).tag($0) }
                }
                .pickerStyle(.segmented)
                .fixedSize()
                .background(isRefused(\.code) ? LucinaTokens.Colors.negative.opacity(0.2) : .clear)
                .accessibilityIdentifier("sheet-code-\(day.row)")
            }
            if code.uppercased() == "R" {
                HStack(spacing: LucinaTokens.Space.sm) {
                    timeField("Start", \.start, day.start)
                    timeField("End", \.end, day.end)
                    timeField("or hours", \.actual, day.actual)
                }
            }
            if code.uppercased() == "R" || !value(\.remarks, day.remarks).isEmpty {
                TextField("Remarks", text: binding(\.remarks, day.remarks), axis: .vertical)
                    .font(LucinaTokens.Fonts.subheadline)
                    .padding(LucinaTokens.Space.xs)
                    .background(isRefused(\.remarks) ? LucinaTokens.Colors.negative.opacity(0.12) : LucinaTokens.Colors.field,
                                in: RoundedRectangle(cornerRadius: LucinaTokens.Radius.field / 2))
                    .accessibilityIdentifier("sheet-remarks-\(day.row)")
            }
            if day.code.uppercased() != "N" || !edited {
                HStack(spacing: LucinaTokens.Space.md) {
                    Text("Worked \(day.worked)")
                    if !day.isZeroDiff {
                        Text(verbatim: (day.isNegative ? "" : "+") + day.diff)
                            .foregroundStyle(day.isNegative ? LucinaTokens.Colors.negative : LucinaTokens.Colors.positive)
                    }
                }
                .font(LucinaTokens.Fonts.caption2)
                .foregroundStyle(LucinaTokens.Colors.textSecondary)
                .opacity(edited ? 0.4 : 1)
            }
        }
        .padding(.vertical, LucinaTokens.Space.xxs)
        .listRowBackground(day.workday ? nil : LucinaTokens.Colors.cardRaised)
    }

    private var choices: [String] {
        let typed = month.dayOff.flatMap { draft.text($0.sheet, $0.cell) }
        return month.codes(dayOff: typed, current: code)
    }

    private func timeField(_ title: LocalizedStringKey, _ column: KeyPath<SheetColumns, String>, _ original: String) -> some View {
        VStack(alignment: .leading, spacing: LucinaTokens.Space.xxs) {
            Text(title).font(LucinaTokens.Fonts.caption2).foregroundStyle(LucinaTokens.Colors.textSecondary)
            TextField("hh:mm", text: binding(column, original))
                .keyboardType(.numbersAndPunctuation)
                .font(LucinaTokens.Fonts.body.monospacedDigit())
                .padding(LucinaTokens.Space.xs)
                .background(isRefused(column) ? LucinaTokens.Colors.negative.opacity(0.15) : LucinaTokens.Colors.field,
                            in: RoundedRectangle(cornerRadius: LucinaTokens.Radius.field / 2))
                .accessibilityIdentifier("sheet-\(month.columns[keyPath: column])\(day.row)")
        }
    }
}

// MARK: - Grid

struct SheetGridScreen: View {
    let workbook: SheetWorkbook
    @Binding var draft: SheetDraft
    let refusedKey: String?
    @State private var sheetIndex: Int?
    @State private var selected: SheetCell?
    @State private var editing: SheetCellEdit?

    private var visible: [SheetData] { workbook.sheets.filter(\.isVisible) }
    private var sheet: SheetData? { visible.first { $0.index == sheetIndex } ?? visible.first }

    var body: some View {
        VStack(spacing: 0) {
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: LucinaTokens.Space.xs) {
                    ForEach(visible) { s in
                        Button { sheetIndex = s.index; selected = nil } label: {
                            Text(verbatim: s.name)
                                .font(LucinaTokens.Fonts.subheadline)
                                .padding(.horizontal, LucinaTokens.Space.md)
                                .padding(.vertical, LucinaTokens.Space.xs)
                                .background(s.index == sheet?.index ? LucinaTokens.Colors.accentSoft : LucinaTokens.Colors.field,
                                            in: Capsule())
                        }
                        .buttonStyle(.plain)
                    }
                }
                .padding(.horizontal, LucinaTokens.Space.gutter)
                .padding(.vertical, LucinaTokens.Space.sm)
            }
            if let selected, let sheet {
                SheetFormulaBar(sheet: sheet.name, cell: selected, pending: draft.text(sheet.name, selected.ref))
            }
            if let sheet {
                SheetGridView(grid: SheetGrid(sheet), workbook: workbook, draft: draft, refusedKey: refusedKey) { cell, row, col in
                    let c = cell ?? SheetCell(ref: SheetRefs.column(col) + "\(row)", r: row, c: col, s: 0, t: "z", v: .none,
                                              display: "", f: nil, editable: !sheet.protected, input: nil)
                    selected = c
                    if c.isEditable && workbook.writable {
                        editing = SheetCellEdit(sheet: sheet.name, cell: c, choices: SheetGrid(sheet).choices(row, col),
                                                text: draft.text(sheet.name, c.ref) ?? c.display)
                    }
                }
                .id(sheet.index)
            }
        }
        .sheet(item: $editing) { e in
            SheetCellEditor(edit: e) { text in
                draft.set(e.sheet, e.cell.ref, text: text, original: e.cell.display)
                editing = nil
            }
            .presentationDetents([.height(e.choices == nil ? 220 : 320)])
        }
    }
}

struct SheetCellEdit: Identifiable, Hashable {
    let sheet: String
    let cell: SheetCell
    let choices: [String]?
    let text: String
    var id: String { "\(sheet)!\(cell.ref)" }
}

private struct SheetFormulaBar: View {
    let sheet: String
    let cell: SheetCell
    let pending: String?

    var body: some View {
        HStack(spacing: LucinaTokens.Space.sm) {
            Text(verbatim: cell.ref).font(LucinaTokens.Fonts.caption.monospaced())
                .foregroundStyle(LucinaTokens.Colors.textSecondary)
            if let f = cell.f {
                Text(verbatim: "ƒ " + f).font(LucinaTokens.Fonts.caption2.monospaced()).lineLimit(2)
            } else {
                Text(verbatim: pending ?? cell.display).font(LucinaTokens.Fonts.caption2).lineLimit(2)
            }
            Spacer(minLength: 0)
            if cell.f != nil {
                Image(systemName: "function").foregroundStyle(LucinaTokens.Colors.textTertiary)
            } else if !cell.isEditable {
                Image(systemName: "lock.fill").foregroundStyle(LucinaTokens.Colors.textTertiary)
            }
        }
        .padding(.horizontal, LucinaTokens.Space.gutter)
        .padding(.vertical, LucinaTokens.Space.xs)
        .background(LucinaTokens.Colors.card)
    }
}

private struct SheetGridView: View {
    let grid: SheetGrid
    let workbook: SheetWorkbook
    let draft: SheetDraft
    let refusedKey: String?
    let onTap: (SheetCell?, Int, Int) -> Void

    private let header: CGFloat = 36

    var body: some View {
        ScrollView([.horizontal, .vertical]) {
            LazyVStack(alignment: .leading, spacing: 0, pinnedViews: []) {
                HStack(spacing: 0) {
                    Color.clear.frame(width: header, height: 22)
                    ForEach(grid.cols, id: \.self) { c in
                        Text(verbatim: SheetRefs.column(c))
                            .font(LucinaTokens.Fonts.caption2)
                            .foregroundStyle(LucinaTokens.Colors.textSecondary)
                            .frame(width: grid.width(c), height: 22)
                            .background(LucinaTokens.Colors.card)
                    }
                }
                ForEach(grid.rows, id: \.self) { r in
                    HStack(alignment: .top, spacing: 0) {
                        Text(verbatim: "\(r)")
                            .font(LucinaTokens.Fonts.caption2)
                            .foregroundStyle(LucinaTokens.Colors.textSecondary)
                            .frame(width: header, height: grid.height(r))
                            .background(LucinaTokens.Colors.card)
                        ForEach(grid.cols, id: \.self) { c in
                            cellView(r, c)
                        }
                    }
                    .zIndex(Double(-r))
                }
                if grid.truncated {
                    Text("Only the first 2000 rows and 200 columns are shown.")
                        .font(LucinaTokens.Fonts.footnote)
                        .padding(LucinaTokens.Space.md)
                }
            }
        }
        .background(LucinaTokens.Colors.card)
    }

    @ViewBuilder
    private func cellView(_ r: Int, _ c: Int) -> some View {
        let w = grid.width(c)
        let h = grid.height(r)
        if grid.isCovered(r, c) {
            Color.clear.frame(width: w, height: h)
        } else {
            let cell = grid.cell(r, c)
            let span = grid.span(r, c)
            let style = cell.flatMap { workbook.style($0.s) }
            let key = SheetDraft.key(grid.sheet.name, cell?.ref ?? SheetRefs.column(c) + "\(r)")
            let pending = draft.text(grid.sheet.name, cell?.ref ?? SheetRefs.column(c) + "\(r)")
            let full = span.map { grid.width(from: c, span: $0.cols) } ?? w
            SheetCellView(cell: cell, style: style, pending: pending, refused: refusedKey == key)
                .frame(width: full, height: h, alignment: .topLeading)
                .frame(width: w, height: h, alignment: .topLeading)
                .contentShape(Rectangle())
                .onTapGesture { onTap(cell, r, c) }
        }
    }
}

private struct SheetCellView: View {
    let cell: SheetCell?
    let style: SheetStyle?
    let pending: String?
    let refused: Bool

    private var alignment: Alignment {
        switch style?.align {
        case "center", "centerContinuous": .center
        case "right": .trailing
        case "left": .leading
        default:
            // Excel puts numbers on the right and text on the left.
            cell?.t == "n" ? .trailing : .leading
        }
    }

    var body: some View {
        let text = pending ?? cell?.display ?? ""
        let input = cell?.isInput ?? false
        Text(verbatim: text)
            .font(.system(size: 12, weight: style?.bold == true ? .semibold : .regular))
            .italic(style?.italic == true)
            .foregroundStyle(foreground)
            .lineLimit(style?.wrap == true ? 4 : 1)
            .padding(.horizontal, 3)
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: alignment)
            .background(background(input: input))
            .overlay { border }
            .overlay {
                if input {
                    Rectangle().strokeBorder(LucinaTokens.Colors.accent.opacity(0.55), style: StrokeStyle(lineWidth: 1, dash: [3, 2]))
                }
                if refused {
                    Rectangle().strokeBorder(LucinaTokens.Colors.negative, lineWidth: 2)
                }
            }
    }

    /// Excel's default text is black: on the sheet's own (light) fills it stays black in dark mode too.
    private var foreground: Color {
        if pending != nil { return LucinaTokens.Colors.accent }
        if let color = Color(sheetHex: style?.color) { return color }
        return style?.fill != nil ? .black : LucinaTokens.Colors.text
    }

    private func background(input: Bool) -> Color {
        if pending != nil { return LucinaTokens.Colors.accentSoft }
        if let fill = Color(sheetHex: style?.fill) {
            return input ? fill.opacity(0.85) : fill
        }
        return input ? LucinaTokens.Colors.accentSoft.opacity(0.5) : .clear
    }

    private var border: some View {
        let b = style?.border ?? []
        let line = LucinaTokens.Colors.separator
        return ZStack {
            if b.count == 4 {
                VStack(spacing: 0) {
                    (b[0] ? line : .clear).frame(height: 0.5)
                    Spacer(minLength: 0)
                    (b[2] ? line : .clear).frame(height: 0.5)
                }
                HStack(spacing: 0) {
                    (b[3] ? line : .clear).frame(width: 0.5)
                    Spacer(minLength: 0)
                    (b[1] ? line : .clear).frame(width: 0.5)
                }
            }
        }
        .overlay(alignment: .bottomTrailing) {
            // The grid's own faint lines.
            Rectangle().fill(line.opacity(0.25)).frame(height: 0.33).frame(maxHeight: .infinity, alignment: .bottom)
        }
    }
}

private struct SheetCellEditor: View {
    let edit: SheetCellEdit
    let done: (String) -> Void
    @State private var text = ""
    @FocusState private var focused: Bool
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            Form {
                if let choices = edit.choices {
                    Picker("Value", selection: $text) {
                        Text("Empty").tag("")
                        ForEach(choices, id: \.self) { Text(verbatim: $0).tag($0) }
                    }
                    .pickerStyle(.inline)
                } else {
                    TextField("Value", text: $text, axis: .vertical)
                        .focused($focused)
                        .autocorrectionDisabled()
                        .textInputAutocapitalization(.never)
                        .onSubmit { done(text) }
                }
            }
            .navigationTitle(Text(verbatim: "\(edit.sheet)!\(edit.cell.ref)"))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) { Button("Done") { done(text) } }
            }
        }
        .onAppear {
            text = edit.text
            focused = true
        }
    }
}

// MARK: - Versions

struct SheetVersionsView: View {
    @Environment(GalleryStore.self) private var store
    @Environment(\.dismiss) private var dismiss
    let workbook: SheetWorkbook
    let restored: (SheetWorkbook) -> Void
    @State private var versions: [SheetVersion]?
    @State private var error: String?
    @State private var confirm: SheetVersion?
    @State private var busy = false

    var body: some View {
        List {
            Section {
                LabeledContent("Current", value: "v\(workbook.version)")
            }
            Section("Earlier versions") {
                if let versions {
                    if versions.isEmpty {
                        Text("No earlier versions yet. Every save keeps the previous file here.")
                            .foregroundStyle(LucinaTokens.Colors.textSecondary)
                    }
                    ForEach(versions) { v in
                        HStack {
                            VStack(alignment: .leading) {
                                Text(verbatim: "v\(v.version)")
                                if let date = GalleryFormat.parse(v.replacedAt) {
                                    Text(date.formatted(date: .abbreviated, time: .shortened))
                                        .font(LucinaTokens.Fonts.caption2)
                                        .foregroundStyle(LucinaTokens.Colors.textSecondary)
                                }
                            }
                            Spacer()
                            Button("Restore") { confirm = v }
                                .buttonStyle(.borderless)
                                .disabled(busy || !workbook.writable)
                        }
                    }
                } else if let error {
                    Text(verbatim: error).foregroundStyle(LucinaTokens.Colors.negative)
                } else {
                    ProgressView()
                }
            }
        }
        .navigationTitle("Versions")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar { ToolbarItem(placement: .cancellationAction) { Button("Close") { dismiss() } } }
        .task { await load() }
        .confirmationDialog("Restore this version?", isPresented: Binding(get: { confirm != nil }, set: { if !$0 { confirm = nil } }),
                            titleVisibility: .visible, presenting: confirm) { v in
            Button("Restore v\(v.version)") { Task { await restore(v) } }
        } message: { _ in
            Text("The current file is kept as a version too, so you can undo this.")
        }
    }

    private func load() async {
        do {
            versions = try await store.api().sheetVersions(id: workbook.itemId).versions
        } catch is CancellationError {
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func restore(_ v: SheetVersion) async {
        busy = true
        defer { busy = false }
        do {
            let w = try await store.api().restoreSheet(id: workbook.itemId, version: v.version, baseHash: workbook.hash)
            restored(w)
            dismiss()
        } catch is CancellationError {
        } catch {
            self.error = error.localizedDescription
        }
    }
}

extension Color {
    /// `#RRGGBB` from the sheet's styles.
    init?(sheetHex: String?) {
        guard let hex = sheetHex?.trimmingCharacters(in: CharacterSet(charactersIn: "#")), hex.count == 6,
              let n = UInt32(hex, radix: 16) else { return nil }
        self.init(red: Double((n >> 16) & 0xFF) / 255, green: Double((n >> 8) & 0xFF) / 255, blue: Double(n & 0xFF) / 255)
    }
}
