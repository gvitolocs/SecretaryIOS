import Charts
import SwiftUI

struct WeightView: View {
    @Environment(WeightStore.self) private var store
    @State private var quickValue = ""
    @State private var editing: BodyEntry?
    @State private var showSettings = false
    @State private var logging = false
    @FocusState private var quickFocused: Bool

    var body: some View {
        List {
            Section {
                quickLog
            }
            if let error = store.actionError {
                Section {
                    ErrorBanner(message: error) { store.actionError = nil }
                        .listRowInsets(EdgeInsets())
                        .listRowBackground(Color.clear)
                }
            }
            if let view = store.view {
                Section { statsGrid(view) }
                    .listRowInsets(EdgeInsets())
                    .listRowBackground(Color.clear)
                if let series = view.series, series.count >= 2 {
                    Section("Trend") { trendChart(view, series: series) }
                }
                Section("Entries") {
                    if view.entries.isEmpty {
                        Text("No entries yet. Log your first weigh-in above.").foregroundStyle(.secondary)
                    }
                    ForEach(view.entries) { entry in
                        Button { editing = entry } label: { entryRow(entry, view: view) }
                            .buttonStyle(.plain)
                            .swipeActions(edge: .trailing, allowsFullSwipe: true) {
                                Button("Delete", systemImage: "trash", role: .destructive) {
                                    Task { await store.run(.deleteEntry(entryId: entry.id)) }
                                }
                            }
                    }
                }
            } else if let error = store.loadError {
                Section {
                    ContentUnavailableView {
                        Label("Couldn't load your weight", systemImage: "scalemass")
                    } description: { Text(error) } actions: {
                        Button("Try again") { Task { await store.refresh() } }
                    }
                }
            } else {
                Section { ProgressView().frame(maxWidth: .infinity) }
            }
        }
        .listStyle(.insetGrouped)
        .refreshable { await store.refresh() }
        .navigationTitle("Weight")
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button { showSettings = true } label: { Image(systemName: "target") }
                    .accessibilityLabel("Goal and unit")
                    .disabled(store.view == nil)
            }
        }
        .sheet(item: $editing) { entry in EntryEditor(entry: entry) }
        .sheet(isPresented: $showSettings) { WeightSettings() }
        .task { if store.view == nil { await store.refresh() } }
    }

    private var quickLog: some View {
        HStack(spacing: 10) {
            Image(systemName: "scalemass.fill").foregroundStyle(Theme.accent).accessibilityHidden(true)
            TextField("Today's weight", text: $quickValue)
                .keyboardType(.decimalPad)
                .focused($quickFocused)
                .font(.title3.monospacedDigit())
                .accessibilityIdentifier("weightQuickField")
            Text(store.unit.rawValue).foregroundStyle(.secondary)
            Button {
                Task { await log() }
            } label: {
                if logging { ProgressView() } else { Text("Log").bold() }
            }
            .buttonStyle(.borderedProminent)
            .tint(Theme.accentFill)
            .disabled(quickValue.trimmingCharacters(in: .whitespaces).isEmpty || logging)
            .accessibilityIdentifier("weightLogButton")
        }
        .toolbar {
            ToolbarItemGroup(placement: .keyboard) {
                Spacer()
                Button("Done") { quickFocused = false }
            }
        }
    }

    private func log() async {
        logging = true
        defer { logging = false }
        if await store.log(quickValue) {
            quickValue = ""
            quickFocused = false
        }
    }

    private func statsGrid(_ view: BodyView) -> some View {
        let tiles: [(String, String, String)] = [
            ("Current", "current", "scalemass"),
            ("7-day average", "avg7", "waveform.path.ecg"),
            ("This week", "weekly_change", "arrow.up.arrow.down"),
            ("To goal", "goal_distance", "flag.checkered"),
        ]
        return LazyVGrid(columns: [GridItem(.flexible(), spacing: 12), GridItem(.flexible(), spacing: 12)], spacing: 12) {
            ForEach(tiles, id: \.1) { title, key, icon in
                Card(spacing: 6) {
                    Label(title, systemImage: icon)
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .minimumScaleFactor(0.8)
                    Text(view.statLabel(key) ?? "—")
                        .font(.title2.weight(.semibold).monospacedDigit())
                        .lineLimit(1)
                        .minimumScaleFactor(0.6)
                    if key == "goal_distance", let goal = view.statLabel("goal") {
                        Text(view.stats.goalReached == true ? "Goal \(goal) reached" : "Goal \(goal)")
                            .font(.caption2).foregroundStyle(.secondary)
                    }
                }
                .accessibilityElement(children: .combine)
            }
        }
    }

    private func trendChart(_ view: BodyView, series: [BodySeriesPoint]) -> some View {
        let unit = view.unit
        let values = series.map { unit.value(grams: $0.weightGrams) } + series.compactMap { $0.avg7Grams.map { unit.value(grams: $0) } }
        let low = (values.min() ?? 0).rounded(.down) - 1
        let high = (values.max() ?? 1).rounded(.up) + 1
        return Chart {
            ForEach(series) { point in
                PointMark(x: .value("Date", point.date), y: .value("Weight", unit.value(grams: point.weightGrams)))
                    .foregroundStyle(Color.secondary.opacity(0.5))
                    .symbolSize(18)
            }
            ForEach(series.filter { $0.avg7Grams != nil }) { point in
                LineMark(x: .value("Date", point.date), y: .value("7-day average", unit.value(grams: point.avg7Grams ?? 0)))
                    .foregroundStyle(Theme.accent)
                    .lineStyle(StrokeStyle(lineWidth: 2.5, lineCap: .round))
                    .interpolationMethod(.monotone)
            }
            if let goal = view.settings.goalGrams {
                RuleMark(y: .value("Goal", unit.value(grams: goal)))
                    .foregroundStyle(.orange.opacity(0.7))
                    .lineStyle(StrokeStyle(lineWidth: 1, dash: [4, 4]))
            }
        }
        .chartYScale(domain: low...high)
        .chartYAxis { AxisMarks(position: .leading) }
        .chartXAxis {
            AxisMarks(values: .automatic(desiredCount: 4)) { _ in
                AxisGridLine()
                AxisValueLabel(format: .dateTime.day().month(.abbreviated), collisionResolution: .greedy)
            }
        }
        .frame(height: 200)
        .padding(.vertical, 8)
        .accessibilityLabel("Weight trend with the 7-day average")
        .accessibilityValue(view.statLabel("avg7").map { "Latest 7-day average \($0)" } ?? "")
    }

    private func entryRow(_ entry: BodyEntry, view: BodyView) -> some View {
        HStack {
            VStack(alignment: .leading, spacing: 2) {
                Text(entry.date.formatted(date: .abbreviated, time: .shortened)).font(.subheadline)
                if !entry.note.isEmpty {
                    Text(entry.note).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                }
            }
            Spacer()
            Text(view.label(for: entry)).font(.body.weight(.semibold).monospacedDigit())
        }
        .contentShape(Rectangle())
        .accessibilityElement(children: .combine)
        .accessibilityHint("Double-tap to edit, swipe to delete")
        .accessibilityIdentifier("weightEntry")
    }
}

private struct EntryEditor: View {
    @Environment(WeightStore.self) private var store
    @Environment(\.dismiss) private var dismiss
    let entry: BodyEntry
    @State private var value = ""
    @State private var date = Date()
    @State private var note = ""
    @State private var saving = false

    var body: some View {
        NavigationStack {
            Form {
                HStack {
                    TextField("Weight", text: $value).keyboardType(.decimalPad)
                    Text(store.unit.rawValue).foregroundStyle(.secondary)
                }
                DatePicker("Measured", selection: $date, in: ...Date().addingTimeInterval(86_400))
                TextField("Note", text: $note, axis: .vertical)
                if let error = store.actionError {
                    Text(error).font(.footnote).foregroundStyle(.red)
                }
                Section {
                    Button("Delete entry", role: .destructive) {
                        Task { if await store.run(.deleteEntry(entryId: entry.id)) { dismiss() } }
                    }
                }
            }
            .navigationTitle("Edit entry")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Save") {
                        guard let grams = store.grams(from: value) else { return }
                        saving = true
                        Task {
                            defer { saving = false }
                            if await store.run(.editEntry(entryId: entry.id, weightGrams: grams,
                                                          measuredMs: Int(date.timeIntervalSince1970 * 1000), note: note)) {
                                dismiss()
                            }
                        }
                    }
                    .disabled(saving)
                }
            }
            .onAppear {
                value = store.unit.number(grams: entry.weightGrams)
                date = entry.date
                note = entry.note
            }
        }
        .presentationDetents([.medium, .large])
    }
}

private struct WeightSettings: View {
    @Environment(WeightStore.self) private var store
    @Environment(\.dismiss) private var dismiss
    @State private var unit: BodyUnit = .kg
    @State private var goal = ""

    var body: some View {
        NavigationStack {
            Form {
                Picker("Unit", selection: $unit) {
                    ForEach(BodyUnit.allCases, id: \.self) { Text($0.title).tag($0) }
                }
                HStack {
                    TextField("Goal (optional)", text: $goal).keyboardType(.decimalPad)
                    Text(unit.rawValue).foregroundStyle(.secondary)
                }
                if let error = store.actionError {
                    Text(error).font(.footnote).foregroundStyle(.red)
                }
            }
            .navigationTitle("Goal and unit")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Save") {
                        let trimmed = goal.trimmingCharacters(in: .whitespaces)
                        var grams: Int?
                        if !trimmed.isEmpty {
                            guard let parsed = unit.parse(trimmed) else {
                                store.actionError = "Enter the goal like 78 or 78.5."
                                return
                            }
                            grams = parsed
                        }
                        Task { if await store.run(.updateSettings(unit: unit, goalGrams: grams)) { dismiss() } }
                    }
                }
            }
            .onAppear {
                unit = store.unit
                goal = store.view?.settings.goalGrams.map { store.unit.number(grams: $0) } ?? ""
            }
        }
        .presentationDetents([.medium])
    }
}
