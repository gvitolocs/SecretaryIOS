import SwiftUI

struct RootView: View {
    @Environment(AppModel.self) private var app
    @Environment(\.scenePhase) private var scenePhase

    var body: some View {
        Group {
            switch app.phase {
            case .checking:
                ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity).background(Theme.page)
            case .signedOut:
                LoginView()
            case .signedIn:
                MainTabs()
                    .onAppear { PerfLog.count("RootView.signedIn") }
            }
        }
        .animation(.default, value: app.phase)
        .onOpenURL { url in
            if let route = WidgetRoute.route(from: url) { app.openWidgetRoute(route) }
        }
        #if DEBUG || SCREENSHOTS
        .overlay {
            if let page = UserDefaults.standard.string(forKey: "SecretaryWidgetGallery").flatMap(Int.init) {
                WidgetGallery(page: page)
            }
        }
        #endif
    }
}

/// The tabs, built once per sign-in: switching tabs only changes the selection, so every tab's
/// screen and store stay alive (no `.id`, no conditional wrapper around the TabView, and the
/// stores live in AppModel, not in the tab bodies).
struct MainTabs: View {
    @Environment(AppModel.self) private var app
    @Environment(\.scenePhase) private var scenePhase
    /// The launch's foreground work waits this long after the first frame (cold start only).
    @State private var launchDelay: Duration? = .seconds(2)

    var body: some View {
        tabs
        .onChange(of: scenePhase, initial: true) { _, phase in
            if phase != .active { app.lucina.stopEverything() } else { Task { await app.device.resume() } }
        }
        .onAppear { app.openPendingWidgetRoute() }
        // A place opened by a widget, a notification or a route (`/map/place?...`): the in-app map.
        .modifier(OpenPlaceHost())
        #if DEBUG || SCREENSHOTS
        .task { await WidgetSelfTest.runIfRequested(app) }
        #endif
        // Lucina in the room (AR), over everything.
        .modifier(LucinaARHost(controller: app.lucina))
        #if SCREENSHOTS
        // `-SecretaryMapSelfTest place|card|deals|route` (Features/Places/MapSelfTest.swift).
        .modifier(MapSelfTestHost())
        #endif
        // Lucina asks before deleting something or changing the calendar.
        .sheet(item: Binding(get: { app.device.bridge.pendingConfirmations.first }, set: { _ in })) { command in
            CommandConfirmation(command: command)
        }
        // Her outfit follows the tab she was opened from (gym, shopping, elsewhere) unless pinned;
        // the stage only applies it once she is on screen (LucinaStageController.dress).
        .onChange(of: app.selectedTab, initial: true) { previous, tab in
            if previous != tab { PerfLog.tabSelected(from: previous, to: tab) }
            if tab != .lucina { app.wardrobe.section = AvatarSettings.Section(tab: tab) }
        }
        .onChange(of: LookKey(app.wardrobe), initial: true) { _, look in
            app.lucina.dress(look.wardrobe, outfit: look.outfit, hair: look.hair, hairColor: look.hairColor)
        }
        // The Wardrobe, from the Lucina tab: she stays visible above the sheet as the live preview.
        // (On its own host view: two sheets chained on one view do not both present.)
        .background {
            Color.clear
                .sheet(isPresented: Binding(get: { app.wardrobe.presented }, set: { app.wardrobe.presented = $0 })) {
                    WardrobeView()
                        .environment(app.wardrobe)
                        .presentationDetents([.medium, .large])
                        .presentationBackgroundInteraction(.enabled(upThrough: .medium))
                }
        }
        // `-SecretaryPerfSelfTest` (App/PerfLog.swift); the count shows the tabs were built once.
        .onAppear { PerfLog.count("MainTabs.appear") }
        .task { await PerfSelfTest.runIfRequested(app) }
    }

    private var tabs: some View {
        @Bindable var app = app
        return TabView(selection: $app.selectedTab) {
            NavigationStack { ChatView().lucinaStatusToolbar() }
                .onAppear { PerfLog.tabAppeared(.chat) }
                .tabItem { Label("Chat", systemImage: "bubble.left.and.bubble.right") }
                .tag(AppModel.Tab.chat)
            NavigationStack { LucinaTab().lucinaStatusToolbar() }
                .onAppear { PerfLog.tabAppeared(.lucina) }
                .tabItem { Label("Lucina", systemImage: "person.crop.circle") }
                .tag(AppModel.Tab.lucina)
            NavigationStack { ShoppingTab().lucinaStatusToolbar() }
                .onAppear { PerfLog.tabAppeared(.shopping) }
                .tabItem { Label("Shopping", systemImage: "cart") }
                .tag(AppModel.Tab.shopping)
            NavigationStack { FitnessView().lucinaStatusToolbar() }
                .onAppear { PerfLog.tabAppeared(.fitness) }
                .tabItem { Label("Training", systemImage: "figure.strengthtraining.traditional") }
                .tag(AppModel.Tab.fitness)
            NavigationStack { PluginsView().lucinaStatusToolbar() }
                .onAppear { PerfLog.tabAppeared(.plugins) }
                .tabItem { Label("Plugins", systemImage: "square.grid.2x2") }
                .tag(AppModel.Tab.plugins)
        }
        // design/tokens.json `tabBar`: the active tab in the accent colour (LucinaTabBarAppearance
        // sets the others' secondary colour at launch).
        .tint(LucinaTokens.TabBar.active)
        // Settings: a sheet from the gear button in every tab's toolbar.
        .sheet(isPresented: $app.showSettings) {
            NavigationStack {
                SettingsView()
                    .toolbar {
                        ToolbarItem(placement: .confirmationAction) {
                            Button("Done") { app.showSettings = false }.accessibilityIdentifier("settingsDone")
                        }
                    }
            }
            .lucinaSheet()
        }
        .onReceive(NotificationCenter.default.publisher(for: ShareUploads.completed)) { _ in
            if app.isForeground { Task { await app.chat.refresh() } }
        }
        // The timeline stream runs only in the foreground; it pauses in the background.
        .onChange(of: scenePhase, initial: true) { _, phase in
            app.chat.setForeground(phase == .active)
            app.isForeground = phase == .active
            if phase == .background {
                app.publishWidgets()
                BackgroundRefresh.schedule()
                if app.backup.settings.enabled { PhotoBackup.scheduleProcessing(chargingOnly: app.backup.settings.chargingOnly) }
                Task { await app.notifications.reschedule() }
            }
        }
        .task(id: scenePhase) {
            guard scenePhase == .active else { return }
            // Cold start: the first chat frame and the chat's own refresh go first; uploads, the
            // inbox, the photo backup and the memo queue start a moment later. (The wardrobe loads
            // with the Lucina tab.)
            if let delay = launchDelay {
                launchDelay = nil
                try? await Task.sleep(for: delay)
                guard !Task.isCancelled else { return }
            }
            await withTaskGroup(of: Void.self) { group in
                group.addTask { @MainActor in
                    while !Task.isCancelled {
                        await ShareUploads.shared.retryPending()
                        try? await Task.sleep(for: .seconds(120))
                    }
                }
                group.addTask { await app.health.poll() }
                group.addTask { await app.watchPrimary() }
                group.addTask { await app.notifications.foregroundLoop() }
                group.addTask { @MainActor in if app.backup.settings.enabled { await app.backup.requestAccessAndRun() } }
                group.addTask { await MainActor.run { app.memos.queue.kick() } }
            }
        }
    }
}

/// What Lucina should wear, as one value for `onChange`.
private struct LookKey: Equatable {
    let wardrobe: WardrobeFile
    let outfit: String?
    let hair: String
    let hairColor: String?

    @MainActor init(_ store: WardrobeStore) {
        wardrobe = store.wardrobe
        outfit = store.current?.id
        hair = store.currentHair
        hairColor = store.currentHairColor
    }
}
