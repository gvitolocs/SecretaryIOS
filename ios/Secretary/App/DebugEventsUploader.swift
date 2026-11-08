import UIKit

/// The app's side of the debug event log (docs/debug-events.md, `Core/DebugEvents.swift`): lifecycle events, and
/// uploads on foreground, every `interval` while in front, and on background inside a background-task window. Never
/// on the main thread's critical path: `DebugEvents.flush` runs detached.
@MainActor
enum DebugEventsUploader {
    nonisolated static let interval: Duration = .seconds(60)
    private static var timer: Task<Void, Never>?
    private static var observers: [NSObjectProtocol] = []

    static func install(launchOptions: [UIApplication.LaunchOptionsKey: Any]?) {
        guard observers.isEmpty, !AppModel.isUnitTestHost else { return }
        var launch: [String: Any] = ["state": UIApplication.shared.applicationState == .background ? "background" : "foreground"]
        if launchOptions?[.remoteNotification] != nil { launch["reason"] = "remote_notification" }
        if launchOptions?[.location] != nil { launch["reason"] = "location" }
        if launchOptions?[.url] != nil { launch["reason"] = "url" }
        launch["os"] = UIDevice.current.systemVersion
        launch["version"] = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "?"
        DebugEvents.record("app.launch", launch)
        let center = NotificationCenter.default
        observers = [
            center.addObserver(forName: UIApplication.didBecomeActiveNotification, object: nil, queue: .main) { _ in
                MainActor.assumeIsolated { foreground() }
            },
            center.addObserver(forName: UIApplication.didEnterBackgroundNotification, object: nil, queue: .main) { _ in
                MainActor.assumeIsolated { background() }
            },
            center.addObserver(forName: UIApplication.willTerminateNotification, object: nil, queue: .main) { _ in
                DebugEvents.record("app.terminate")
            },
        ]
    }

    private static func foreground() {
        DebugEvents.record("app.foreground")
        timer?.cancel()
        timer = Task.detached(priority: .utility) {
            while !Task.isCancelled {
                await DebugEvents.flush()
                try? await Task.sleep(for: interval)
            }
        }
    }

    private static func background() {
        DebugEvents.record("app.background")
        timer?.cancel()
        timer = nil
        flushInBackground("background")
    }

    /// One flush inside a background-task window (also after a notification action or an intent finished).
    static func flushInBackground(_ reason: String) {
        let window = BackgroundWindow()
        window.id = UIApplication.shared.beginBackgroundTask(withName: "debug-events") {
            MainActor.assumeIsolated { window.end() }
        }
        Task {
            let outcome = await Task.detached(priority: .utility) { await DebugEvents.flush() }.value
            if case .retryLater = outcome { DebugEvents.log.notice("debug events upload deferred (\(reason, privacy: .public))") }
            window.end()
        }
    }

    @MainActor private final class BackgroundWindow {
        var id = UIBackgroundTaskIdentifier.invalid

        func end() {
            guard id != .invalid else { return }
            UIApplication.shared.endBackgroundTask(id)
            id = .invalid
        }
    }
}
