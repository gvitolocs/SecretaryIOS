import SwiftUI

@main
struct SecretaryApp: App {
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
                .environment(app.health)
                .task {
                    // Hosted unit tests load the app; keep it off the network.
                    guard ProcessInfo.processInfo.environment["XCTestConfigurationFilePath"] == nil else { return }
                    await app.start()
                }
        }
    }
}
