import SwiftUI

struct ExercisePicker: View {
    @Environment(GymStore.self) private var store
    @Environment(\.dismiss) private var dismiss
    let onSelect: (Exercise) -> Void
    @State private var query = ""
    @State private var showCreate = false

    var body: some View {
        NavigationStack {
            List(store.search(query)) { exercise in
                Button { onSelect(exercise) } label: {
                    HStack {
                        VStack(alignment: .leading, spacing: 4) {
                            Text(exercise.name).font(.headline).foregroundStyle(.primary)
                            Text("\(exercise.muscle) · \(exercise.equipment)").font(.caption).foregroundStyle(.secondary)
                        }
                        Spacer()
                        Image(systemName: "plus.circle").foregroundStyle(Theme.accent).accessibilityHidden(true)
                    }
                    .contentShape(Rectangle())
                }
                .accessibilityIdentifier("exercise-\(exercise.id)")
                .accessibilityHint("Adds it to the workout")
            }
            .overlay {
                if store.search(query).isEmpty {
                    ContentUnavailableView.search(text: query)
                }
            }
            .searchable(text: $query, prompt: "Name, muscle, or equipment")
            .navigationTitle("Exercises")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) { Button("Close") { dismiss() } }
                ToolbarItem(placement: .topBarTrailing) {
                    Button { showCreate = true } label: { Image(systemName: "plus") }
                        .accessibilityLabel("Create exercise")
                }
            }
            .sheet(isPresented: $showCreate) { CreateExerciseScreen() }
        }
    }
}

struct CreateExerciseScreen: View {
    @Environment(GymStore.self) private var store
    @Environment(\.dismiss) private var dismiss
    @State private var name = ""
    @State private var muscle = "Chest"
    @State private var equipment = "Dumbbell"
    @State private var kind = ExerciseKind.weightReps.rawValue
    @State private var saving = false

    var body: some View {
        NavigationStack {
            Form {
                TextField("Exercise name", text: $name)
                TextField("Muscle group", text: $muscle)
                TextField("Equipment", text: $equipment)
                Picker("Tracking", selection: $kind) {
                    ForEach(ExerciseKind.allCases, id: \.rawValue) { Text($0.title).tag($0.rawValue) }
                }
                if let error = store.actionError {
                    Text(error).font(.footnote).foregroundStyle(.red)
                }
            }
            .navigationTitle("New exercise")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Save") {
                        saving = true
                        Task {
                            defer { saving = false }
                            if await store.run(.createExercise(name: name, muscle: muscle, equipment: equipment, kind: kind)) != nil {
                                dismiss()
                            }
                        }
                    }
                    .disabled(saving)
                }
            }
        }
    }
}

struct RoutineBuilder: View {
    @Environment(GymStore.self) private var store
    @Environment(\.dismiss) private var dismiss
    @State private var name = ""
    @State private var selected: [String] = []
    @State private var showExercises = false

    var body: some View {
        NavigationStack {
            Form {
                Section("Routine") { TextField("Name", text: $name) }
                Section("Exercises in order") {
                    ForEach(selected, id: \.self) { id in Text(store.exercise(id)?.name ?? id) }
                        .onDelete { selected.remove(atOffsets: $0) }
                        .onMove { selected.move(fromOffsets: $0, toOffset: $1) }
                    Button("Add exercise", systemImage: "plus") { showExercises = true }
                }
                Section {
                    Text("New routines start with three sets per exercise. Save a workout as a routine to keep your own set prescriptions.")
                        .font(.footnote).foregroundStyle(.secondary)
                }
                if let error = store.actionError {
                    Text(error).font(.footnote).foregroundStyle(.red)
                }
            }
            .navigationTitle("New routine")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Save") {
                        Task {
                            if await store.run(.saveRoutine(name: name, exerciseIds: selected, fromActive: false)) != nil { dismiss() }
                        }
                    }
                }
                ToolbarItem(placement: .bottomBar) { EditButton() }
            }
            .sheet(isPresented: $showExercises) {
                ExercisePicker { exercise in
                    if !selected.contains(exercise.id) { selected.append(exercise.id) }
                    showExercises = false
                }
            }
        }
    }
}
