import Charts
import SwiftUI

struct ProgressScreen: View {
    @Environment(GymStore.self) private var store
    let view: GymView
    @State private var showMeasurement = false
    @State private var chartExercise = ""

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 22) {
                Text("Progress").font(.largeTitle.bold()).accessibilityAddTraits(.isHeader)
                GymCard {
                    GymEyebrow(text: "Training volume")
                    Text(view.summary.volume)
                        .font(.system(size: 34, weight: .semibold, design: .rounded))
                        .lineLimit(1)
                        .minimumScaleFactor(0.6)
                    if !view.exerciseCharts.isEmpty {
                        Picker("Exercise", selection: $chartExercise) {
                            Text("All exercises").tag("")
                            ForEach(chartExerciseIds, id: \.self) { id in
                                Text(view.exercise(id)?.name ?? id).tag(id)
                            }
                        }
                        .pickerStyle(.menu)
                    }
                    let points = chartExercise.isEmpty ? view.chart : (view.exerciseCharts[chartExercise] ?? [])
                    if points.isEmpty {
                        Text("Your completed sessions will appear here.").font(.subheadline).foregroundStyle(GymPalette.muted)
                    } else {
                        // x is the workout id (two workouts on the same day stay two bars).
                        Chart(points) { point in
                            BarMark(x: .value("Session", point.id), y: .value("Volume", point.volume))
                                .foregroundStyle(GymPalette.forest.gradient)
                                .cornerRadius(5)
                        }
                        .chartXAxis(.hidden)
                        .chartYAxis { AxisMarks(position: .leading) }
                        .frame(height: 160)
                        .accessibilityLabel("Volume over the most recent fourteen workouts")
                    }
                    GymEyebrow(text: "Warmup sets excluded")
                }

                Text("Personal records").font(.title2.bold())
                if view.records.isEmpty {
                    GymEmptyCard(icon: "trophy", title: "Make your first mark",
                                 detail: "Complete working sets to establish your personal records.")
                }
                ForEach(view.records) { record in
                    GymCard {
                        HStack {
                            Image(systemName: "trophy.fill").foregroundStyle(GymPalette.forest).accessibilityHidden(true)
                            Text(record.name).font(.headline)
                        }
                        switch view.exercise(record.exerciseId)?.exerciseKind ?? .weightReps {
                        case .timed:
                            GymMetric(value: "\(record.seconds)s", label: "Longest set")
                        case .distanceTime:
                            HStack(alignment: .top) {
                                GymMetric(value: "\(record.distanceMeters)m", label: "Longest distance")
                                GymMetric(value: "\(record.seconds)s", label: "Longest duration")
                            }
                        case .repsOnly:
                            GymMetric(value: "\(record.reps)", label: "Most reps")
                        case .weightReps:
                            HStack(alignment: .top) {
                                GymMetric(value: record.weight, label: "Heaviest weight")
                                GymMetric(value: "\(record.reps)", label: "Most reps")
                                if let e1rm = record.estimated1rm { GymMetric(value: e1rm, label: "Est. 1RM") }
                            }
                        }
                    }
                }

                HStack {
                    Text("Body measurements").font(.title2.bold())
                    Spacer()
                    Button { showMeasurement = true } label: { Image(systemName: "plus.circle.fill").font(.title2) }
                        .accessibilityLabel("Add measurement")
                }
                if view.state.measurements.isEmpty {
                    // Body weight now lives in the Weight tab (secretary-body).
                    Text("Keep a private record of waist and body fat. Body weight lives in the Weight tab.")
                        .font(.subheadline).foregroundStyle(GymPalette.muted)
                }
                ForEach(view.state.measurements) { measurement in
                    GymCard {
                        HStack {
                            VStack(alignment: .leading, spacing: 6) {
                                Text(measurement.kind.replacingOccurrences(of: "_", with: " ").capitalized).font(.headline)
                                Text(dateLabel(measurement.timestampMs)).font(.caption).foregroundStyle(GymPalette.muted)
                            }
                            Spacer()
                            Text(view.measurementLabels[measurement.id] ?? "").font(.title3.bold())
                        }
                    }
                    .contextMenu {
                        Button("Delete measurement", role: .destructive) {
                            Task { await store.run(.deleteMeasurement(measurementId: measurement.id)) }
                        }
                    }
                }
            }
            .padding(20)
        }
        .refreshable { await store.refresh() }
        .sheet(isPresented: $showMeasurement) { MeasurementScreen().gymStyle() }
    }

    private var chartExerciseIds: [String] {
        view.exerciseCharts.keys.sorted { (view.exercise($0)?.name ?? $0) < (view.exercise($1)?.name ?? $1) }
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
                    TextField("Value", text: $value).keyboardType(.decimalPad).gymFieldFont()
                    Text(kind == "body_fat" ? "%" : "cm").foregroundStyle(GymPalette.muted)
                }
                if let error = store.actionError { Text(error).font(.footnote).foregroundStyle(.red) }
            }
            .scrollContentBackground(.hidden)
            .background(GymPalette.background)
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
    }
}
