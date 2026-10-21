import SwiftUI

/// The Gym tab: the LucinaGym app (Training, Journal, Progress, Settings) with its
/// original look. Everything shown comes from the engine view.
struct GymScreen: View {
    @Environment(GymStore.self) private var store
    @Environment(\.scenePhase) private var scenePhase

    enum Section: String, CaseIterable, Identifiable {
        case training = "Training", journal = "Journal", progress = "Progress", settings = "Settings"
        var id: String { rawValue }
        var icon: String {
            switch self {
            case .training: return "dumbbell"
            case .journal: return "book.closed"
            case .progress: return "chart.xyaxis.line"
            case .settings: return "slider.horizontal.3"
            }
        }
    }

    @State private var section: Section = .training

    var body: some View {
        @Bindable var store = store
        VStack(spacing: 0) {
            sectionBar
            Group {
                if let view = store.view {
                    switch section {
                    case .training: TrainingView(view: view)
                    case .journal: HistoryView(view: view)
                    case .progress: ProgressScreen(view: view)
                    case .settings: GymSettingsScreen()
                    }
                } else if let error = store.loadError {
                    ContentUnavailableView {
                        Label("Couldn\u{2019}t open your gym data", systemImage: "externaldrive.badge.exclamationmark")
                    } description: {
                        Text(error)
                    } actions: {
                        Button("Try again") { Task { await store.refresh() } }
                            .buttonStyle(.borderedProminent)
                    }
                    .frame(maxHeight: .infinity)
                } else {
                    ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
                }
            }
        }
        .background(GymPalette.background)
        .toolbar(.hidden, for: .navigationBar)
        .gymStyle()
        .sheet(isPresented: $store.isPresentingWorkout) { WorkoutScreen().gymStyle() }
        .task { if store.view == nil { await store.refresh() } }
        .onChange(of: scenePhase) { _, phase in
            if phase == .active { Task { await store.refresh() } }
        }
        .alert("Couldn\u{2019}t save that change", isPresented: Binding(
            get: { store.actionError != nil && !store.isPresentingWorkout },
            set: { if !$0 { store.actionError = nil } }
        )) {
            Button("OK") { store.actionError = nil }
        } message: {
            Text(store.actionError ?? "")
        }
    }

    /// The original app's four tabs, as a bar at the top of the Gym tab.
    private var sectionBar: some View {
        HStack(spacing: 6) {
            ForEach(Section.allCases) { item in
                Button {
                    section = item
                } label: {
                    VStack(spacing: 3) {
                        Image(systemName: item.icon).font(.system(size: 15, weight: .semibold))
                        Text(item.rawValue).font(.system(size: 11, weight: .semibold, design: .rounded))
                    }
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 7)
                    .foregroundStyle(section == item ? Color.white : GymPalette.muted)
                    .background(section == item ? GymPalette.forest : Color.clear, in: RoundedRectangle(cornerRadius: 12))
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel(item.rawValue)
                .accessibilityAddTraits(section == item ? .isSelected : [])
                .accessibilityIdentifier("gymSection-\(item.rawValue)")
            }
        }
        .padding(5)
        .background(.white, in: RoundedRectangle(cornerRadius: 16))
        .overlay(RoundedRectangle(cornerRadius: 16).stroke(GymPalette.ink.opacity(0.04)))
        .padding(.horizontal, 22)
        .padding(.top, 6)
        .padding(.bottom, 4)
    }
}

struct TrainingView: View {
    @Environment(GymStore.self) private var store
    let view: GymView
    @State private var showRoutine = false

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 24) {
                HStack {
                    GymEyebrow(text: "Lucina / Gym")
                    Spacer()
                    Image(systemName: "sparkle").foregroundStyle(GymPalette.forest).accessibilityHidden(true)
                }
                Text("Make time\nfor strength.")
                    .font(.system(size: 40, weight: .semibold, design: .rounded))
                    .tracking(-1.5)
                    .fixedSize(horizontal: false, vertical: true)
                    .accessibilityAddTraits(.isHeader)
                HStack(alignment: .bottom) {
                    VStack(alignment: .leading, spacing: 8) {
                        GymEyebrow(text: "This week")
                        Text("\(view.summary.weeklyWorkouts) / \(view.summary.weeklyGoal) workouts").font(.headline)
                    }
                    Spacer()
                    Image(systemName: "calendar").font(.title2).foregroundStyle(GymPalette.forest).accessibilityHidden(true)
                }
                .accessibilityElement(children: .combine)
                ProgressView(value: Double(min(view.summary.weeklyWorkouts, max(view.summary.weeklyGoal, 1))),
                             total: Double(max(view.summary.weeklyGoal, 1)))
                    .tint(GymPalette.forest)
                    .accessibilityLabel("Weekly goal")
                if let active = view.state.active {
                    activeCard(active)
                } else {
                    GymPrimaryButton(title: "Start empty workout", icon: "play.fill") {
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
                ForEach(view.state.routines) { routine in
                    GymCard {
                        HStack(alignment: .top) {
                            VStack(alignment: .leading, spacing: 8) {
                                Text(routine.name).font(.title3.bold())
                                Text(routine.exercises.compactMap { view.exercise($0.exerciseId)?.name }.joined(separator: " · "))
                                    .font(.subheadline).foregroundStyle(GymPalette.muted).lineLimit(2)
                            }
                            Spacer()
                            Button { Task { await start(routine) } } label: {
                                Image(systemName: "play.fill").padding(13).background(GymPalette.background, in: Circle())
                            }
                            .disabled(view.state.active != nil)
                            .accessibilityLabel("Start \(routine.name)")
                        }
                        GymEyebrow(text: "\(routine.exercises.count) exercises")
                    }
                    .contextMenu {
                        Button("Delete routine", role: .destructive) {
                            Task { await store.run(.deleteRoutine(routineId: routine.id)) }
                        }
                    }
                }
                HStack(alignment: .top) {
                    GymMetric(value: "\(view.summary.workouts)", label: "Sessions")
                    GymMetric(value: "\(view.summary.completedSets)", label: "Sets logged")
                    GymMetric(value: view.summary.volume, label: "Total volume")
                }
                .padding(.vertical, 12)
                Text("A little stronger, one session at a time.")
                    .font(.footnote)
                    .foregroundStyle(GymPalette.muted)
                    .padding(.bottom, 16)
            }
            .padding(22)
        }
        .refreshable { await store.refresh() }
        .sheet(isPresented: $showRoutine) { RoutineBuilder().gymStyle() }
    }

    private func activeCard(_ active: Workout) -> some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("IN PROGRESS").font(.caption2.bold()).tracking(2).foregroundStyle(GymPalette.lime)
            Text(active.title).font(.title.bold())
            TimelineView(.periodic(from: .now, by: 1)) { context in
                Text("\(store.activeDuration(at: context.date)) · \(view.summary.activeCompletedSets) sets · \(view.summary.activeVolume)")
                    .font(.subheadline.monospacedDigit())
                    .foregroundStyle(.white.opacity(0.75))
            }
            Button { store.isPresentingWorkout = true } label: {
                HStack {
                    Text("Continue workout")
                    Spacer()
                    Image(systemName: "arrow.up.right")
                }
                .font(.headline)
                .padding(14)
                .foregroundStyle(GymPalette.forest)
                .background(GymPalette.lime, in: RoundedRectangle(cornerRadius: 12))
            }
            .accessibilityIdentifier("continueWorkout")
        }
        .padding(22)
        .frame(maxWidth: .infinity, alignment: .leading)
        .foregroundStyle(.white)
        .background(GymPalette.forest, in: RoundedRectangle(cornerRadius: 24))
    }

    private func start(_ routine: Routine?) async {
        if await store.run(.startWorkout(title: routine?.name ?? "Workout", routineId: routine?.id)) != nil {
            store.isPresentingWorkout = true
        }
    }
}
