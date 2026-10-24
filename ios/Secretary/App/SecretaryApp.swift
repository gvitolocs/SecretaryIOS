import SwiftUI

@main
struct SecretaryApp: App {
    @UIApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    @State private var app = AppModel.shared

    var body: some Scene {
        WindowGroup {
            RootView()
                .environment(app)
                .environment(app.chat)
                .environment(app.shopping)
                .environment(app.shoppingV2)
                .environment(app.gym)
                .environment(app.weight)
                .environment(app.notes)
                .environment(app.tasks)
                .environment(app.memos)
                .environment(app.health)
                .environment(app.wardrobe)
                .environment(app.notifications)
                .environment(app.media)
                .environment(app.gallery)
                .environment(app.backup)
                .task {
                    // Hosted unit tests load the app; keep it off the network.
                    guard ProcessInfo.processInfo.environment["XCTestConfigurationFilePath"] == nil else { return }
                    await app.start()
                }
        }
        // Inbox sync and local schedules while the app is closed (no APNs on a free team).
        .backgroundTask(.appRefresh(BackgroundRefresh.identifier)) {
            await BackgroundRefresh.run()
            // Start the next photo uploads; the background URLSession carries them on.
            await AppModel.shared.backup.runInBackground(budget: .seconds(20))
        }
    }
}
