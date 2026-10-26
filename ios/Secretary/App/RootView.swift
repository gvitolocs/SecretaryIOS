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
            }
        }
        .animation(.default, value: app.phase)
    }
}

struct MainTabs: View {
    @Environment(AppModel.self) private var app
    @Environment(\.scenePhase) private var scenePhase
    @Environment(\.horizontalSizeClass) private var horizontalSize
    @Environment(\.verticalSizeClass) private var verticalSize

    /// iPad, or an iPhone in landscape: Lucina gets a full-body column beside the tabs.
    private var sideColumn: Bool { horizontalSize == .regular || verticalSize == .compact }

    var body: some View {
        Group {
            if app.lucina.enabled && sideColumn {
                HStack(spacing: 0) {
                    tabs
                    Divider()
                    LucinaColumn(controller: app.lucina)
                        .frame(width: horizontalSize == .regular ? 340 : 260)
                }
            } else {
                tabs.overlay {
                    if app.lucina.enabled { LucinaOverlay(controller: app.lucina) }
                }
            }
        }
        .onChange(of: scenePhase, initial: true) { _, phase in
            if phase != .active { app.lucina.stopEverything() } else { Task { await app.device.resume() } }
        }
        // Lucina in the room (AR), over everything.
        .modifier(LucinaARHost(controller: app.lucina))
        #if SCREENSHOTS
        // `-SecretaryMapSelfTest place|card|deals` (Features/Places/MapSelfTest.swift).
        .modifier(MapSelfTestHost())
        #endif
        // Lucina asks before deleting something or changing the calendar.
        .sheet(item: Binding(get: { app.device.bridge.pendingConfirmations.first }, set: { _ in })) { command in
            CommandConfirmation(command: command)
        }
        // Her outfit follows the tab (gym, shopping, elsewhere) unless pinned.
        .onChange(of: app.selectedTab, initial: true) { _, tab in
            app.wardrobe.section = AvatarSettings.Section(tab: tab)
        }
        .onChange(of: LookKey(app.wardrobe), initial: true) { _, look in
            app.lucina.dress(look.wardrobe, outfit: look.outfit, hair: look.hair, hairColor: look.hairColor)
        }
        // The Wardrobe: Lucina comes forward as the live preview above the sheet. (On its own
        // host view: two sheets chained on one view do not both present.)
        .background {
            Color.clear
                .sheet(isPresented: Binding(get: { app.wardrobe.presented }, set: { app.wardrobe.presented = $0 }), onDismiss: {
                    if !sideColumn { app.lucina.expanded = false }
                }) {
                    WardrobeView()
                        .environment(app.wardrobe)
                        .presentationDetents([.medium, .large])
                        .presentationBackgroundInteraction(.enabled(upThrough: .medium))
                        .onAppear { if app.lucina.enabled && !sideColumn { app.lucina.expanded = true } }
                }
        }
    }

    private var tabs: some View {
        @Bindable var app = app
        return TabView(selection: $app.selectedTab) {
            NavigationStack { ChatView().lucinaStatusToolbar() }
                .tabItem { Label("Chat", systemImage: "bubble.left.and.bubble.right") }
                .tag(AppModel.Tab.chat)
            NavigationStack { ShoppingTab().lucinaStatusToolbar() }
                .tabItem { Label("Shopping", systemImage: "cart") }
                .tag(AppModel.Tab.shopping)
            NavigationStack { FitnessView().lucinaStatusToolbar() }
                .tabItem { Label("Training", systemImage: "figure.strengthtraining.traditional") }
                .tag(AppModel.Tab.fitness)
            NavigationStack { PluginsView().lucinaStatusToolbar() }
                .tabItem { Label("Plugins", systemImage: "square.grid.2x2") }
                .tag(AppModel.Tab.plugins)
        }
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
                BackgroundRefresh.schedule()
                if app.backup.settings.enabled { PhotoBackup.scheduleProcessing(chargingOnly: app.backup.settings.chargingOnly) }
                Task { await app.notifications.reschedule() }
            }
        }
        .task(id: scenePhase) {
            guard scenePhase == .active else { return }
            await withTaskGroup(of: Void.self) { group in
                group.addTask { @MainActor in
                    while !Task.isCancelled {
                        await ShareUploads.shared.retryPending()
                        try? await Task.sleep(for: .seconds(120))
                    }
                }
                group.addTask { await app.health.poll() }
                group.addTask { await app.watchPrimary() }
                group.addTask { await app.wardrobe.load() }
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
