import SwiftUI

/// The Gym tab: training, history and progress, ported from LucinaGym. Everything shown
/// comes from the engine view.
struct GymScreen: View {
    @Environment(GymStore.self) private var store
    @Environment(\.scenePhase) private var scenePhase

    enum Section: String, CaseIterable, Identifiable {
        case train = "Train", history = "History", progress = "Progress"
        var id: String { rawValue }
    }

    @State private var section: Section = .train
    @State private var showSettings = false

    var body: some View {
        VStack(spacing: 0) {
            Picker("Section", selection: $section) {
                ForEach(Section.allCases) { Text($0.rawValue).tag($0) }
            }
            .pickerStyle(.segmented)
            .padding(.horizontal, 16)
            .padding(.bottom, 8)
            .accessibilityIdentifier("gymSection")

            Group {
                if let view = store.view {
                    switch section {
                    case .train: TrainingView(view: view)
                    case .history: HistoryView(view: view)
                    case .progress: ProgressScreen(view: view)
                    }
                } else if let error = store.loadError {
                    ContentUnavailableView {
                        Label("Couldn't load your training", systemImage: "dumbbell")
                    } description: {
                        Text(error)
                    } actions: {
                        Button("Try again") { Task { await store.refresh() } }
                            .buttonStyle(.borderedProminent)
                    }
                    .frame(maxHeight: .infinity)
                } else {
                    ProgressView("Loading your training…")
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                }
            }
        }
        .background(Theme.page)
        .navigationTitle("Gym")
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button { showSettings = true } label: { Image(systemName: "slider.horizontal.3") }
                    .accessibilityLabel("Gym settings")
                    .disabled(store.view == nil)
            }
        }
        .sheet(isPresented: $showSettings) { GymSettingsScreen() }
        .task { if store.view == nil { await store.refresh() } }
        .onChange(of: scenePhase) { _, phase in
            if phase == .active { Task { await store.refresh() } }
        }
        .alert("Couldn't save that change", isPresented: Binding(
            get: { store.actionError != nil && !store.isPresentingWorkout },
            set: { if !$0 { store.actionError = nil } }
        )) {
            Button("OK") { store.actionError = nil }
        } message: {
            Text(store.actionError ?? "")
        }
    }
}

struct TrainingView: View {
    @Environment(GymStore.self) private var store
    let view: GymView
    @State private var showWorkout = false
    @State private var showRoutine = false
    @State private var starting = false

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                weekCard
                if let active = view.state.active {
                    activeCard(active)
                } else {
                    PrimaryButton(title: "Start empty workout", icon: "play.fill", isBusy: starting) {
                        Task { await start(nil) }
                    }
                    .accessibilityIdentifier("startWorkout")
                }

                HStack {
                    Text("Your routines").font(.title2.bold())
                    Spacer()
                    Button { showRoutine = true } label: { Image(systemName: "plus.circle.fill").font(.title2) }
                        .accessibilityLabel("Create routine")
                }
                if view.state.routines.isEmpty {
                    Text("Save a workout as a routine to start it again in one tap.")
                        .font(.subheadline).foregroundStyle(.secondary)
                }
                ForEach(view.state.routines) { routine in
                    routineCard(routine)
                }

                Card {
                    HStack(alignment: .top) {
                        Metric(value: "\(view.summary.workouts)", label: "Sessions")
                        Metric(value: "\(view.summary.completedSets)", label: "Sets")
                        Metric(value: view.summary.volume, label: "Volume")
                    }
                }
            }
            .padding(16)
        }
        .refreshable { await store.refresh() }
        .sheet(isPresented: $showWorkout) { WorkoutScreen() }
        .sheet(isPresented: $showRoutine) { RoutineBuilder() }
        .onChange(of: showWorkout) { _, shown in store.isPresentingWorkout = shown }
    }

    private var weekCard: some View {
        Card {
            HStack(alignment: .firstTextBaseline) {
                VStack(alignment: .leading, spacing: 6) {
                    Eyebrow(text: "This week")
                    Text("\(view.summary.weeklyWorkouts) of \(view.summary.weeklyGoal) workouts")
                        .font(.headline)
                }
                Spacer()
                Image(systemName: "calendar").font(.title2).foregroundStyle(Theme.accent).accessibilityHidden(true)
            }
            ProgressView(value: Double(min(view.summary.weeklyWorkouts, max(view.summary.weeklyGoal, 1))),
                         total: Double(max(view.summary.weeklyGoal, 1)))
                .accessibilityLabel("Weekly goal")
                .accessibilityValue("\(view.summary.weeklyWorkouts) of \(view.summary.weeklyGoal) workouts")
        }
    }

    private func activeCard(_ active: Workout) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("IN PROGRESS").font(.caption2.bold()).tracking(1.5).foregroundStyle(.white.opacity(0.8))
            Text(active.title).font(.title.bold()).foregroundStyle(.white)
            TimelineView(.periodic(from: .now, by: 1)) { context in
                Text("\(store.activeDuration(at: context.date)) · \(view.summary.activeCompletedSets) sets · \(view.summary.activeVolume)")
                    .font(.subheadline.monospacedDigit())
                    .foregroundStyle(.white.opacity(0.85))
            }
            Button { showWorkout = true } label: {
                HStack {
                    Text("Continue workout")
                    Spacer()
                    Image(systemName: "arrow.up.right")
                }
                .font(.headline)
                .padding(14)
                .foregroundStyle(Theme.accentFill)
                .background(.white, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
            }
            .accessibilityIdentifier("continueWorkout")
        }
        .padding(20)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.accentFill.gradient, in: RoundedRectangle(cornerRadius: 22, style: .continuous))
    }

    private func routineCard(_ routine: Routine) -> some View {
        Card {
            HStack(alignment: .top) {
                VStack(alignment: .leading, spacing: 6) {
                    Text(routine.name).font(.title3.bold())
                    Text(routine.exercises.compactMap { view.exercise($0.exerciseId)?.name }.joined(separator: " · "))
                        .font(.subheadline).foregroundStyle(.secondary).lineLimit(2)
                }
                Spacer()
                Button { Task { await start(routine) } } label: {
                    Image(systemName: "play.fill")
                        .padding(12)
                        .background(Theme.field, in: Circle())
                }
                .disabled(view.state.active != nil || starting)
                .accessibilityLabel("Start \(routine.name)")
            }
            Eyebrow(text: "\(routine.exercises.count) exercises")
        }
        .contextMenu {
            Button("Delete routine", systemImage: "trash", role: .destructive) {
                Task { await store.run(.deleteRoutine(routineId: routine.id)) }
            }
        }
    }

    private func start(_ routine: Routine?) async {
        starting = true
        defer { starting = false }
        if await store.run(.startWorkout(title: routine?.name ?? "Workout", routineId: routine?.id)) != nil {
            showWorkout = true
        }
    }
}
