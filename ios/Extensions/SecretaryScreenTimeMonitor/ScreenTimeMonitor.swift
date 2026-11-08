import DeviceActivity
import Foundation

/// Tells Lucina when a limit the owner set in Secretary is passed. Only the limit's name
/// ("Social over 60 min") is sent, never the apps or their numbers (iOS hides those).
final class SecretaryScreenTimeMonitor: DeviceActivityMonitor {
    override func eventDidReachThreshold(_ event: DeviceActivityEvent.Name, activity: DeviceActivityName) {
        super.eventDidReachThreshold(event, activity: activity)
        let done = DispatchSemaphore(value: 0)
        Task {
            _ = await ExtensionReporter.send(type: "screen_threshold", source: "ios", requires: "screenTime",
                                             data: ["limit": event.rawValue, "schedule": activity.rawValue])
            done.signal()
        }
        // The extension may be suspended as soon as this returns.
        _ = done.wait(timeout: .now() + 20)
    }
}
