import SwiftUI

struct HistoryView: View {
    let view: GymView

    var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 14) {
                if view.state.history.isEmpty {
                    EmptyCard(icon: "book.closed", title: "Your story starts here",
                              detail: "Finish a workout and it will be saved in your journal.")
                }
                ForEach(view.state.history) { workout in
                    NavigationLink {
                        WorkoutDetail(workoutId: workout.id)
                    } label: {
                        Card(spacing: 8) {
                            HStack {
                                Eyebrow(text: dateLabel(workout.finishedMs ?? workout.startedMs))
                                Spacer()
                                Image(systemName: "chevron.right").font(.footnote).foregroundStyle(.tertiary)
                            }
                            Text(workout.title).font(.title3.bold())
                            if let summary = view.workoutLabels[workout.id] {
                                Text("\(summary.duration) · \(summary.sets) sets · \(summary.volume)")
                                    .font(.subheadline).foregroundStyle(.secondary)
                            }
                            Text(workout.exercises.compactMap { view.exercise($0.exerciseId)?.name }.joined(separator: " · "))
                                .font(.caption).foregroundStyle(.secondary).lineLimit(2)
                        }
                        .accessibilityElement(children: .combine)
                    }
                    .buttonStyle(.plain)
                    .accessibilityIdentifier("history-\(workout.title)")
                }
            }
            .padding(16)
        }
    }
}

struct WorkoutDetail: View {
    @Environment(GymStore.self) private var store
    @Environment(\.dismiss) private var dismiss
    let workoutId: String
    @State private var confirmDelete = false

    private var workout: Workout? { store.view?.state.history.first { $0.id == workoutId } }

    var body: some View {
        ScrollView {
            if let workout, let view = store.view {
                VStack(alignment: .leading, spacing: 14) {
                    Text(dateTimeLabel(workout.finishedMs ?? workout.startedMs)).foregroundStyle(.secondary)
                    if let summary = view.workoutLabels[workout.id] {
                        Card {
                            HStack(alignment: .top) {
                                Metric(value: summary.duration, label: "Duration")
                                Metric(value: "\(summary.sets)", label: "Sets")
                                Metric(value: summary.volume, label: "Volume")
                            }
                        }
                    }
                    if !workout.notes.isEmpty { Text(workout.notes) }
                    ForEach(workout.exercises) { exercise in
                        Card(spacing: 8) {
                            Text(view.exercise(exercise.exerciseId)?.name ?? "Exercise").font(.headline)
                            ForEach(exercise.sets) { set in
                                HStack {
                                    Image(systemName: set.completed ? "checkmark.circle.fill" : "circle")
                                        .foregroundStyle(set.completed ? Theme.accent : .secondary)
                                        .accessibilityHidden(true)
                                    Text(SetKind(rawValue: set.kind)?.title ?? set.kind).font(.caption).foregroundStyle(.secondary)
                                    Spacer()
                                    Text(describe(set, exercise: exercise, view: view)).font(.subheadline.monospacedDigit())
                                }
                                .accessibilityElement(children: .combine)
                            }
                            if !exercise.notes.isEmpty {
                                Text(exercise.notes).font(.footnote).foregroundStyle(.secondary)
                            }
                        }
                    }
                    Button {
                        Task { await store.run(.repeatWorkout(workoutId: workout.id)) }
                    } label: {
                        Label("Repeat this workout", systemImage: "arrow.clockwise")
                            .frame(maxWidth: .infinity)
                    }
                    .buttonStyle(.bordered)
                    .disabled(view.state.active != nil)
                }
                .padding(16)
            }
        }
        .background(Theme.page)
        .navigationTitle(workout?.title ?? "Workout")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            Button { confirmDelete = true } label: { Image(systemName: "trash") }
                .accessibilityLabel("Delete workout")
        }
        .confirmationDialog("Delete this workout from your journal?", isPresented: $confirmDelete, titleVisibility: .visible) {
            Button("Delete workout", role: .destructive) {
                Task { if await store.run(.deleteWorkout(workoutId: workoutId)) != nil { dismiss() } }
            }
        }
    }

    private func describe(_ set: WorkoutSet, exercise: WorkoutExercise, view: GymView) -> String {
        switch view.exercise(exercise.exerciseId)?.exerciseKind ?? .weightReps {
        case .repsOnly: return "\(set.reps) reps"
        case .timed: return "\(set.seconds) s"
        case .distanceTime: return "\(set.distanceMeters) m · \(set.seconds) s"
        case .weightReps: return "\(view.weightLabels[set.id] ?? "0") \(view.unit) × \(set.reps)"
        }
    }
}
