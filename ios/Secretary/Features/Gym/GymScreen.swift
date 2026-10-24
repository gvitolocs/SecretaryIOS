import SwiftUI

/// The Gym tab: the LucinaGym app (Training, Journal, Progress, Settings) with its
/// original look. Everything shown comes from the engine view.
struct GymScreen: View {
    @Environment(GymStore.self) private var store
    @Environment(\.scenePhase) private var scenePhase

    enum Section: String, CaseIterable, Identifiable {
        case training = "Training", journal = "Journal", progress = "Progress", settings = "Settings"
        var id: String { rawValue }
        var title: LocalizedStringKey {
            switch self {
            case .training: return "Training"
            case .journal: return "Journal"
            case .progress: return "Progress"
            case .settings: return "Settings"
            }
        }
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
        .safeAreaInset(edge: .top, spacing: 0) { sectionBar }
        .lucinaScreen(section == .training ? "Gym" : section.title)
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
        HStack(spacing: 4) {
            ForEach(Section.allCases) { item in
                Button {
                    section = item
                } label: {
                    VStack(spacing: 3) {
                        Image(systemName: item.icon).font(.system(size: 15, weight: .semibold))
                        Text(item.title).font(LucinaTokens.Fonts.caption2.weight(.semibold))
                    }
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 7)
                    .foregroundStyle(section == item ? Theme.accent : GymPalette.muted)
                    .background(section == item ? Theme.accentSoft : Color.clear, in: Capsule())
                    .contentShape(Capsule())
                }
                .buttonStyle(.plain)
                .accessibilityLabel(item.title)
                .accessibilityAddTraits(section == item ? .isSelected : [])
                .accessibilityIdentifier("gymSection-\(item.rawValue)")
            }
        }
        .padding(4)
        .background(Theme.card, in: Capsule())
        .padding(.horizontal, LucinaTokens.Space.gutter)
        .padding(.bottom, LucinaTokens.Space.sm)
        .background(Theme.page)
    }
}

struct TrainingView: View {
    @Environment(GymStore.self) private var store
    let view: GymView
    @State private var showRoutine = false

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: LucinaTokens.Space.lg) {
                Text("Make time\nfor strength.")
                    .font(LucinaTokens.Fonts.title3)
                    .foregroundStyle(GymPalette.muted)
                    .fixedSize(horizontal: false, vertical: true)
                Card(spacing: 10) {
                    HStack(alignment: .bottom) {
                        VStack(alignment: .leading, spacing: 6) {
                            Label("This week", systemImage: "calendar")
                                .font(LucinaTokens.Fonts.caption)
                                .foregroundStyle(GymPalette.muted)
                            Text("\(view.summary.weeklyWorkouts) / \(view.summary.weeklyGoal) workouts")
                                .font(LucinaTokens.Fonts.title2.monospacedDigit())
                        }
                        Spacer()
                    }
                    .accessibilityElement(children: .combine)
                    ProgressView(value: Double(min(view.summary.weeklyWorkouts, max(view.summary.weeklyGoal, 1))),
                                 total: Double(max(view.summary.weeklyGoal, 1)))
                        .tint(Theme.accent)
                        .accessibilityLabel("Weekly goal")
                }
                if let active = view.state.active {
                    activeCard(active)
                } else {
                    GymPrimaryButton(title: "Start empty workout", icon: "play.fill") {
                        Task { await start(nil) }
                    }
                    .accessibilityIdentifier("startWorkout")
                }
                SectionHeader("Your routines") {
                    Button { showRoutine = true } label: { Image(systemName: "plus.circle.fill").font(.title2) }
                        .accessibilityLabel("Create routine")
                }
                ForEach(view.state.routines) { routine in
                    GymCard {
                        HStack(alignment: .top) {
                            VStack(alignment: .leading, spacing: 8) {
                                Text(routine.name).font(LucinaTokens.Fonts.headline)
                                Text(routine.exercises.compactMap { view.exercise($0.exerciseId)?.name }.joined(separator: " · "))
                                    .font(.subheadline).foregroundStyle(GymPalette.muted).lineLimit(2)
                            }
                            Spacer()
                            Button { Task { await start(routine) } } label: {
                                Image(systemName: "play.fill").padding(13).foregroundStyle(Theme.onAccent).background(GymPalette.forestFill, in: Circle())
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
                StatGrid {
                    StatCard(title: "Sessions", icon: "figure.strengthtraining.traditional", value: "\(view.summary.workouts)")
                    StatCard(title: "Sets logged", icon: "checkmark.circle", value: "\(view.summary.completedSets)")
                    StatCard(title: "Total volume", icon: "scalemass", value: view.summary.volume)
                }
                Text("A little stronger, one session at a time.")
                    .font(.footnote)
                    .foregroundStyle(GymPalette.muted)
                    .padding(.bottom, 16)
            }
            .padding(.horizontal, LucinaTokens.Space.gutter)
            .padding(.top, LucinaTokens.Space.sm)
        }
        .refreshable { await store.refresh() }
        .sheet(isPresented: $showRoutine) { RoutineBuilder().gymStyle() }
    }

    private func activeCard(_ active: Workout) -> some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("IN PROGRESS").font(LucinaTokens.Fonts.caption2.weight(.bold)).tracking(1.2).foregroundStyle(GymPalette.lime.opacity(0.8))
            Text(active.title).font(LucinaTokens.Fonts.title)
            TimelineView(.periodic(from: .now, by: 1)) { context in
                Text("\(store.activeDuration(at: context.date)) · \(view.summary.activeCompletedSets) sets · \(view.summary.activeVolume)")
                    .font(.subheadline.monospacedDigit())
                    .foregroundStyle(Theme.onAccent.opacity(0.75))
            }
            Button { store.isPresentingWorkout = true } label: {
                HStack {
                    Text("Continue workout")
                    Spacer()
                    Image(systemName: "arrow.up.right")
                }
                .font(.headline)
                .padding(14)
                .foregroundStyle(GymPalette.forestFill)
                .background(GymPalette.lime, in: Capsule())
            }
            .accessibilityIdentifier("continueWorkout")
        }
        .padding(LucinaTokens.Space.xl)
        .frame(maxWidth: .infinity, alignment: .leading)
        .foregroundStyle(Theme.onAccent)
        .background(GymPalette.forestFill, in: RoundedRectangle(cornerRadius: Theme.cornerRadius, style: .continuous))
    }

    private func start(_ routine: Routine?) async {
        if await store.run(.startWorkout(title: routine?.name ?? "Workout", routineId: routine?.id)) != nil {
            store.isPresentingWorkout = true
        }
    }
}
