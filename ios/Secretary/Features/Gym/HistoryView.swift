import SwiftUI

/// The journal (the original's History tab).
struct HistoryView: View {
    let view: GymView

    var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 18) {
                Text("Journal").font(.largeTitle.bold()).accessibilityAddTraits(.isHeader)
                if view.state.history.isEmpty {
                    GymEmptyCard(icon: "book.closed", title: "Your story starts here",
                                 detail: "Finish a workout and it will be saved in your journal.")
                }
                ForEach(view.state.history) { workout in
                    NavigationLink {
                        WorkoutDetail(workoutId: workout.id).gymStyle()
                    } label: {
                        GymCard {
                            HStack {
                                GymEyebrow(text: dateLabel(workout.finishedMs ?? workout.startedMs))
                                Spacer()
                                Image(systemName: "arrow.up.right").foregroundStyle(GymPalette.forest).accessibilityHidden(true)
                            }
                            Text(workout.title).font(.title2.bold())
                            if let summary = view.workoutLabels[workout.id] {
                                Text("\(summary.duration) · \(summary.sets) sets · \(summary.volume)")
                                    .font(.subheadline).foregroundStyle(GymPalette.muted)
                            }
                            Text(workout.exercises.compactMap { view.exercise($0.exerciseId)?.name }.joined(separator: " · "))
                                .font(.caption).foregroundStyle(GymPalette.muted).lineLimit(2)
                        }
                        .accessibilityElement(children: .combine)
                    }
                    .buttonStyle(.plain)
                    .accessibilityIdentifier("history-\(workout.title)")
                }
            }
            .padding(20)
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
                VStack(alignment: .leading, spacing: 18) {
                    Text(dateLabel(workout.finishedMs ?? workout.startedMs)).foregroundStyle(GymPalette.muted)
                    if let summary = view.workoutLabels[workout.id] {
                        HStack(alignment: .top) {
                            GymMetric(value: summary.duration, label: "Duration")
                            GymMetric(value: "\(summary.sets)", label: "Sets")
                            GymMetric(value: summary.volume, label: "Volume")
                        }
                    }
                    if !workout.notes.isEmpty { Text(workout.notes) }
                    ForEach(workout.exercises) { exercise in
                        GymCard {
                            Text(view.exercise(exercise.exerciseId)?.name ?? "Exercise").font(.headline)
                            ForEach(exercise.sets) { set in
                                HStack {
                                    Image(systemName: set.completed ? "checkmark.circle.fill" : "circle")
                                        .foregroundStyle(set.completed ? GymPalette.forest : GymPalette.muted)
                                        .accessibilityHidden(true)
                                    Text(set.kind.capitalized).font(.caption).foregroundStyle(GymPalette.muted)
                                    Spacer()
                                    Text(describe(set, exercise: exercise, view: view)).font(.subheadline.monospacedDigit())
                                }
                                .accessibilityElement(children: .combine)
                            }
                            if !exercise.notes.isEmpty {
                                Text(exercise.notes).font(.footnote).foregroundStyle(GymPalette.muted)
                            }
                        }
                    }
                    GymPrimaryButton(title: "Repeat this workout", icon: "arrow.clockwise") {
                        Task {
                            if await store.run(.repeatWorkout(workoutId: workout.id)) != nil {
                                dismiss()
                                store.isPresentingWorkout = true
                            }
                        }
                    }
                    .disabled(view.state.active != nil)
                    .opacity(view.state.active != nil ? 0.5 : 1)
                    .accessibilityIdentifier("repeatWorkout")
                }
                .padding(20)
            }
        }
        .background(GymPalette.background)
        .navigationTitle(workout?.title ?? "Workout")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar(.visible, for: .navigationBar)
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
