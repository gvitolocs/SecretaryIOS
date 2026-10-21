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
        // Lucina asks before deleting something or changing the calendar.
        .sheet(item: Binding(get: { app.device.bridge.pendingConfirmations.first }, set: { _ in })) { command in
            CommandConfirmation(command: command)
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
            NavigationStack { GymScreen() }
                .tabItem { Label("Gym", systemImage: "dumbbell") }
                .tag(AppModel.Tab.gym)
            NavigationStack { WeightView().lucinaStatusToolbar() }
                .tabItem { Label("Weight", systemImage: "scalemass") }
                .tag(AppModel.Tab.weight)
            NavigationStack { SettingsView().lucinaStatusToolbar() }
                .tabItem { Label("Settings", systemImage: "gearshape") }
                .tag(AppModel.Tab.settings)
        }
        // The timeline stream runs only in the foreground; it pauses in the background.
        .onChange(of: scenePhase, initial: true) { _, phase in
            app.chat.setForeground(phase == .active)
        }
        .task(id: scenePhase) {
            guard scenePhase == .active else { return }
            await withTaskGroup(of: Void.self) { group in
                group.addTask { await app.health.poll() }
                group.addTask { await app.watchPrimary() }
            }
        }
    }
}
