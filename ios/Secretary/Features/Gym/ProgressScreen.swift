import Charts
import SwiftUI

struct ProgressScreen: View {
    @Environment(GymStore.self) private var store
    let view: GymView
    @State private var showMeasurement = false
    @State private var chartExercise = ""

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                Card {
                    Eyebrow(text: "Training volume")
                    Text(view.summary.volume).font(.largeTitle.weight(.semibold)).lineLimit(1).minimumScaleFactor(0.6)
                    let points = chartExercise.isEmpty ? view.chart : (view.exerciseCharts[chartExercise] ?? [])
                    if !view.exerciseCharts.isEmpty {
                        Picker("Exercise", selection: $chartExercise) {
                            Text("All exercises").tag("")
                            ForEach(view.exerciseCharts.keys.sorted { (view.exercise($0)?.name ?? $0) < (view.exercise($1)?.name ?? $1) }, id: \.self) { id in
                                Text(view.exercise(id)?.name ?? id).tag(id)
                            }
                        }
                        .pickerStyle(.menu)
                    }
                    if points.isEmpty {
                        Text("Your completed sessions will appear here.").font(.subheadline).foregroundStyle(.secondary)
                    } else {
                        Chart(points) { point in
                            BarMark(x: .value("Session", point.label), y: .value("Volume", point.volume))
                                .foregroundStyle(Theme.accent.gradient)
                                .cornerRadius(5)
                        }
                        .chartYAxis { AxisMarks(position: .leading) }
                        .frame(height: 170)
                        .accessibilityLabel("Volume over the most recent workouts, in \(view.unit)")
                    }
                    Eyebrow(text: "Warm-up sets excluded")
                }

                Text("Personal records").font(.title2.bold())
                if view.records.isEmpty {
                    EmptyCard(icon: "trophy", title: "Make your first mark", detail: "Complete working sets to set your personal records.")
                }
                ForEach(view.records) { record in
                    Card(spacing: 10) {
                        Label(record.name, systemImage: "trophy.fill").font(.headline).foregroundStyle(.primary)
                        HStack(alignment: .top) {
                            switch view.exercise(record.exerciseId)?.exerciseKind ?? .weightReps {
                            case .timed:
                                Metric(value: "\(record.seconds) s", label: "Longest set")
                            case .distanceTime:
                                Metric(value: "\(record.distanceMeters) m", label: "Longest distance")
                                Metric(value: "\(record.seconds) s", label: "Longest time")
                            case .repsOnly:
                                Metric(value: "\(record.reps)", label: "Most reps")
                            case .weightReps:
                                Metric(value: record.weight, label: "Heaviest")
                                Metric(value: "\(record.reps)", label: "Most reps")
                                if let e1rm = record.estimated1rm { Metric(value: e1rm, label: "Est. 1RM") }
                            }
                        }
                    }
                }

                HStack {
                    Text("Measurements").font(.title2.bold())
                    Spacer()
                    Button { showMeasurement = true } label: { Image(systemName: "plus.circle.fill").font(.title2) }
                        .accessibilityLabel("Add measurement")
                }
                if view.state.measurements.isEmpty {
                    Text("Track waist and body fat here. Body weight lives in the Weight tab.")
                        .font(.subheadline).foregroundStyle(.secondary)
                }
                ForEach(view.state.measurements) { measurement in
                    Card {
                        HStack {
                            VStack(alignment: .leading, spacing: 4) {
                                Text(measurement.kind.replacingOccurrences(of: "_", with: " ").capitalized).font(.headline)
                                Text(dateLabel(measurement.timestampMs)).font(.caption).foregroundStyle(.secondary)
                            }
                            Spacer()
                            Text(view.measurementLabels[measurement.id] ?? "").font(.title3.bold())
                        }
                    }
                    .contextMenu {
                        Button("Delete measurement", systemImage: "trash", role: .destructive) {
                            Task { await store.run(.deleteMeasurement(measurementId: measurement.id)) }
                        }
                    }
                }
            }
            .padding(16)
        }
        .refreshable { await store.refresh() }
        .sheet(isPresented: $showMeasurement) { MeasurementScreen() }
    }
}

struct MeasurementScreen: View {
    @Environment(GymStore.self) private var store
    @Environment(\.dismiss) private var dismiss
    @State private var kind = "waist"
    @State private var value = ""

    var body: some View {
        NavigationStack {
            Form {
                Picker("Measurement", selection: $kind) {
                    Text("Waist").tag("waist")
                    Text("Body fat").tag("body_fat")
                }
                HStack {
                    TextField("Value", text: $value).keyboardType(.decimalPad)
                    Text(kind == "body_fat" ? "%" : "cm").foregroundStyle(.secondary)
                }
                if let error = store.actionError { Text(error).font(.footnote).foregroundStyle(.red) }
            }
            .navigationTitle("New measurement")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Save") {
                        Task { if await store.run(.recordMeasurement(kind: kind, value: value)) != nil { dismiss() } }
                    }
                }
            }
        }
        .presentationDetents([.medium])
    }
}
