import SwiftUI

@main
struct SecretaryApp: App {
    @State private var app = AppModel()

    var body: some Scene {
        WindowGroup {
            RootView()
                .environment(app)
                .environment(app.chat)
                .environment(app.shopping)
                .environment(app.gym)
                .environment(app.weight)
                .environment(app.health)
                .task { await app.start() }
        }
    }
}
