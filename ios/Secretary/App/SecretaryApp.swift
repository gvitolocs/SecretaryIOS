import SwiftUI

@main
struct SecretaryApp: App {
    @UIApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate

    init() {
        PerfLog.appInit()
        // A Lock Screen / Action button recording may be all this launch is for: only the
        // recorder is set up here; AppModel waits for a window (AppWindow) or the stop.
        MemoRecordingCenter.install()
        LucinaTabBarAppearance.apply()
        #if DEBUG || SCREENSHOTS
        WindowSnapshots.schedule()
        #endif
    }

    var body: some Scene {
        WindowGroup {
            #if DEBUG
            if MemoMinimalLaunchTest.requested {
                MemoMinimalLaunchTest.Screen()
            } else {
                AppWindow()
            }
            #else
            AppWindow()
            #endif
        }
        // Inbox sync and local schedules while the app is closed (no APNs on a free team).
        .backgroundTask(.appRefresh(BackgroundRefresh.identifier)) {
            await BackgroundRefresh.run()
            // Start the next photo uploads; the background URLSession carries them on.
            await AppModel.shared.backup.runInBackground(budget: .seconds(20))
        }
    }
}

/// The app's window. AppModel is built when the first window opens, not at launch.
private struct AppWindow: View {
    @State private var app = AppModel.shared

    var body: some View {
        RootView()
            .environment(app)
            .environment(app.chat)
            .environment(app.shopping)
            .environment(app.shoppingV2)
            .environment(app.gym)
            .environment(app.weight)
            .environment(app.notes)
            .environment(app.tasks)
            .environment(app.agents)
            .environment(app.server)
            .environment(app.memos)
            .environment(app.health)
            .environment(app.wardrobe)
            .environment(app.notifications)
            .environment(app.media)
            .environment(app.gallery)
            .environment(app.backup)
            .task {
                // Hosted unit tests load the app; keep it off the network.
                guard !AppModel.isUnitTestHost else { return }
                await app.start()
            }
    }
}
