import SwiftUI

struct WorkoutScreen: View {
    @Environment(GymStore.self) private var store
    @Environment(\.dismiss) private var dismiss
    @State private var showExercises = false
    @State private var discard = false
    @State private var rename = false
    @State private var saveRoutine = false
    @State private var name = ""
    @State private var notes = ""
    @State private var finishing = false

    var body: some View {
        NavigationStack {
            ScrollView {
                if let view = store.view, let workout = view.state.active {
                    VStack(alignment: .leading, spacing: 20) {
                        TimelineView(.periodic(from: .now, by: 1)) { context in
                            VStack(alignment: .leading, spacing: 20) {
                                HStack(alignment: .top) {
                                    GymMetric(value: store.activeDuration(at: context.date), label: "Duration")
                                    GymMetric(value: view.summary.activeVolume, label: "Volume")
                                    GymMetric(value: "\(view.summary.activeCompletedSets)", label: "Sets")
                                }
                                let remaining = store.restRemaining(at: context.date)
                                if remaining > 0 {
                                    HStack {
                                        Image(systemName: "timer").accessibilityHidden(true)
                                        Text("Rest · \(remaining)s").font(.headline.monospacedDigit())
                                        Spacer()
                                        Button("Skip") { Task { await store.run(.stopRest) } }
                                    }
                                    .padding(16)
                                    .background(Theme.accentSoft, in: RoundedRectangle(cornerRadius: LucinaTokens.Radius.tile, style: .continuous))
                                    .accessibilityElement(children: .contain)
                                    .accessibilityIdentifier("restTimer")
                                }
                            }
                        }
                        TextField("Workout notes", text: $notes, axis: .vertical)
                            .font(.subheadline)
                            .padding(14)
                            .background(Theme.card, in: RoundedRectangle(cornerRadius: LucinaTokens.Radius.tile, style: .continuous))
                            .onSubmit { saveNotes() }
                        ForEach(workout.exercises) { exercise in
                            ExerciseCard(exercise: exercise, view: view)
                        }
                        GymPrimaryButton(title: "Add exercise") { showExercises = true }
                            .accessibilityIdentifier("addExercise")
                        Button("Start rest timer") {
                            Task { await store.run(.startRest(seconds: view.state.settings.restSeconds)) }
                        }
                        .frame(maxWidth: .infinity)
                    }
                    .padding(20)
                }
            }
            .background(GymPalette.background)
            .scrollDismissesKeyboard(.interactively)
            .navigationTitle(store.view?.state.active?.title ?? "Workout")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Button("Close") { saveNotes(); dismiss() }
                }
                ToolbarItem(placement: .topBarTrailing) {
                    HStack {
                        Menu {
                            Button("Rename workout") { name = store.view?.state.active?.title ?? "Workout"; rename = true }
                            Button("Save as routine") { name = store.view?.state.active?.title ?? "Routine"; saveRoutine = true }
                            Button("Discard workout", role: .destructive) { discard = true }
                        } label: {
                            Image(systemName: "ellipsis.circle")
                        }
                        .accessibilityLabel("Workout options")
                        Button {
                            Task { await finish() }
                        } label: {
                            Text("Finish").fontWeight(.bold)
                        }
                        .disabled(store.view?.state.active == nil || finishing)
                        .accessibilityIdentifier("finishWorkout")
                    }
                }
                ToolbarItemGroup(placement: .keyboard) {
                    Spacer()
                    Button("Done") { endEditing() }
                }
            }
            .onAppear { notes = store.view?.state.active?.notes ?? "" }
            // Swiping the sheet away must not lose typed notes (set rows save on their own).
            .onDisappear { saveNotes() }
            .task {
                // The rest timer and duration tick locally each second; this re-reads the
                // engine clock (every second on the device engine, like the original).
                while !Task.isCancelled {
                    try? await Task.sleep(for: store.clockInterval)
                    if Task.isCancelled { break }
                    await store.tickClock()
                }
            }
            .sheet(isPresented: $showExercises) {
                ExercisePicker { exercise in
                    Task {
                        if await store.run(.addExercise(exerciseId: exercise.id)) != nil { showExercises = false }
                    }
                }
                .gymStyle()
            }
            .confirmationDialog("Discard this workout?", isPresented: $discard, titleVisibility: .visible) {
                Button("Discard workout", role: .destructive) {
                    Task { if await store.run(.cancelWorkout) != nil { dismiss() } }
                }
            }
            .alert("Workout name", isPresented: $rename) {
                TextField("Name", text: $name)
                Button("Save") { Task { await store.run(.renameWorkout(title: name)) } }
                Button("Cancel", role: .cancel) {}
            }
            .alert("Save routine", isPresented: $saveRoutine) {
                TextField("Name", text: $name)
                Button("Save") { Task { await store.run(.saveRoutine(name: name, exerciseIds: nil, fromActive: true)) } }
                Button("Cancel", role: .cancel) {}
            }
            .alert("Couldn\u{2019}t save that change", isPresented: Binding(
                get: { store.actionError != nil && !showExercises },
                set: { if !$0 { store.actionError = nil } }
            )) {
                Button("OK") { store.actionError = nil }
            } message: {
                Text(store.actionError ?? "")
            }
        }
    }

    private func saveNotes() {
        guard let active = store.view?.state.active, active.notes != notes else { return }
        let text = notes
        Task { await store.run(.setNotes(notes: text, exerciseId: nil), quiet: true) }
    }

    private func finish() async {
        finishing = true
        defer { finishing = false }
        endEditing()
        saveNotes()
        // Pending set edits (saved on blur) go first: the store queue keeps the order.
        if await store.run(.finishWorkout) != nil { dismiss() }
    }
}

struct ExerciseCard: View {
    @Environment(GymStore.self) private var store
    let exercise: WorkoutExercise
    let view: GymView

    private var definition: Exercise? { view.exercise(exercise.exerciseId) }
    private var kind: ExerciseKind { definition?.exerciseKind ?? .weightReps }

    var body: some View {
        GymCard {
            HStack(alignment: .top) {
                VStack(alignment: .leading, spacing: 5) {
                    Text(definition?.name ?? "Exercise").font(.headline)
                    if let previous = view.previousLabels[exercise.id] {
                        Text("Previous · \(previous)").font(.caption).foregroundStyle(GymPalette.muted)
                    }
                }
                Spacer()
                Menu {
                    Button("Remove exercise", role: .destructive) {
                        Task { await store.run(.removeExercise(exerciseId: exercise.id)) }
                    }
                } label: {
                    Image(systemName: "ellipsis").padding(6)
                }
                .accessibilityLabel("Exercise options")
            }
            HStack {
                GymEyebrow(text: "Set").frame(width: 50, alignment: .leading)
                Spacer()
                GymEyebrow(text: fieldsLabel)
                Spacer()
                GymEyebrow(text: "Done")
            }
            ForEach(Array(exercise.sets.enumerated()), id: \.element.id) { index, set in
                SetRow(set: set, number: index + 1, kind: kind, weightLabel: view.weightLabels[set.id] ?? "0")
            }
            Button {
                Task { await store.run(.addSet(exerciseId: exercise.id)) }
            } label: {
                Label("Add set", systemImage: "plus")
                    .font(.subheadline.bold())
                    .frame(maxWidth: .infinity)
                    .padding(10)
                    .background(Theme.field, in: RoundedRectangle(cornerRadius: LucinaTokens.Radius.field, style: .continuous))
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("addSet-\(exercise.exerciseId)")
        }
    }

    private var fieldsLabel: LocalizedStringKey {
        switch kind {
        case .repsOnly: return "Reps"
        case .timed: return "Seconds"
        case .distanceTime: return "Meters / seconds"
        case .weightReps: return "\(view.unit) / reps"
        }
    }
}

struct SetRow: View {
    @Environment(GymStore.self) private var store
    let set: WorkoutSet
    let number: Int
    let kind: ExerciseKind
    let weightLabel: String

    @State private var weight = "0"
    @State private var reps = "8"
    @State private var seconds = "60"
    @State private var distance = "1000"
    @State private var setKind = "normal"
    @FocusState private var focused: Bool

    var body: some View {
        HStack(spacing: 8) {
            Menu {
                ForEach(SetKind.allCases, id: \.self) { value in
                    Button(value.title) { setKind = value.rawValue; Task { _ = await save() } }
                }
                Button("Delete set", role: .destructive) { Task { await store.run(.removeSet(setId: set.id)) } }
            } label: {
                Text(setKind == "normal" ? "\(number)" : String(setKind.prefix(1)).uppercased())
                    .font(.caption.bold())
                    .frame(width: 28, height: 36)
                    .background(Theme.field, in: RoundedRectangle(cornerRadius: LucinaTokens.Radius.field, style: .continuous))
            }
            .disabled(set.completed)
            .accessibilityLabel("Set \(number) type")

            if kind == .weightReps { field("Weight", value: $weight, id: "weight-\(set.id)") }
            if kind == .weightReps || kind == .repsOnly { field("Reps", value: $reps, id: "reps-\(set.id)") }
            if kind == .distanceTime { field("Meters", value: $distance, id: "distance-\(set.id)") }
            if kind == .timed || kind == .distanceTime { field("Seconds", value: $seconds, id: "seconds-\(set.id)") }

            Button {
                focused = false
                Task { await toggle() }
            } label: {
                Image(systemName: set.completed ? "checkmark.circle.fill" : "circle")
                    .font(.system(size: 27))
                    .foregroundStyle(set.completed ? GymPalette.forest : GymPalette.muted.opacity(0.45))
                    .frame(minWidth: 44, minHeight: 44)
            }
            .buttonStyle(.plain)
            .accessibilityLabel(set.completed ? "Uncheck set \(number)" : "Complete set \(number)")
            .accessibilityIdentifier("complete-set-\(set.id)")
        }
        .onAppear(perform: load)
        .onChange(of: set) { _, _ in if !focused { load() } }
        .onChange(of: focused) { _, isFocused in
            if !isFocused, store.setIsEditable(set.id), isDirty { Task { _ = await save(quiet: true) } }
        }
        // Closing the sheet or scrolling away keeps what was typed (as the original did).
        .onDisappear {
            if store.setIsEditable(set.id), isDirty { Task { _ = await save(quiet: true) } }
        }
    }

    private func load() {
        weight = weightLabel
        reps = "\(set.reps)"
        seconds = "\(set.seconds)"
        distance = "\(set.distanceMeters)"
        setKind = set.kind
    }

    private var isDirty: Bool {
        weight != weightLabel || reps != "\(set.reps)" || seconds != "\(set.seconds)"
            || distance != "\(set.distanceMeters)" || setKind != set.kind
    }

    private func toggle() async {
        if set.completed {
            await store.run(.completeSet(setId: set.id, completed: false))
        } else {
            if isDirty, !(await save()) { return }
            await store.run(.completeSet(setId: set.id, completed: true))
        }
    }

    private func field(_ title: String, value: Binding<String>, id: String) -> some View {
        TextField(title, text: value)
            .keyboardType(.decimalPad)
            .multilineTextAlignment(.center)
            .gymFieldFont()
            .padding(.vertical, 10)
            .frame(maxWidth: .infinity)
            .background(set.completed ? Theme.positive.opacity(0.18) : Color.clear, in: RoundedRectangle(cornerRadius: LucinaTokens.Radius.field, style: .continuous))
            .disabled(set.completed)
            .focused($focused)
            .accessibilityLabel(title)
            .accessibilityIdentifier(id)
    }

    /// Sends the typed values; the engine parses the weight text and validates everything.
    private func save(quiet: Bool = false) async -> Bool {
        // Unparseable input becomes a value above every engine limit, so the engine rejects
        // it with its own message (as LucinaGym did with UInt64.max).
        func int(_ text: String) -> Int { Int(text.trimmingCharacters(in: .whitespaces)) ?? Int(Int32.max) }
        return await store.run(.editSet(setId: set.id, weight: weight, reps: int(reps), seconds: int(seconds),
                                        distanceMeters: int(distance), kind: setKind), quiet: quiet) != nil
    }
}
