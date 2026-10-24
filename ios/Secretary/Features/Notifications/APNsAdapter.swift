import BackgroundTasks
import Foundation
import UIKit
import UserNotifications

/// Remote push (APNs): **ready, and off.**
///
/// The app is signed with a free Personal Team, which cannot use the
/// `aps-environment` entitlement, so `registerForRemoteNotifications()` would
/// fail and nothing can wake the app from the server. With a paid Apple
/// Developer Program team (docs/notifications.md, "APNs with a paid account"):
/// 1. add `aps-environment: development` to `Secretary.entitlements`
///    (`production` for TestFlight/App Store) and the Push Notifications
///    capability for the App ID;
/// 2. set `SecretaryAPNsEnabled` to `true` in `project.yml` (Info.plist);
/// 3. create an APNs auth key (.p8) for that team and set `SECRETARY_APNS_*`
///    on the server.
/// The app then sends its device token to `POST /v1/push/apns`, and the server
/// pushes every notification there too.
enum APNsAdapter {
    static var isEnabled: Bool {
        Bundle.main.object(forInfoDictionaryKey: "SecretaryAPNsEnabled") as? Bool == true
    }

    @MainActor
    static func registerIfEnabled() {
        guard isEnabled else { return }
        UIApplication.shared.registerForRemoteNotifications()
    }

    static func hex(_ token: Data) -> String {
        token.map { String(format: "%02x", $0) }.joined()
    }
}

/// `BGAppRefreshTask`: iOS wakes the app now and then (its own schedule, at
/// most every 15 minutes) to sync the inbox and the local schedules.
enum BackgroundRefresh {
    static let identifier = "tech.getsecretary.app.refresh"

    static func schedule(after interval: TimeInterval = 15 * 60) {
        let request = BGAppRefreshTaskRequest(identifier: identifier)
        request.earliestBeginDate = Date(timeIntervalSinceNow: interval)
        try? BGTaskScheduler.shared.submit(request)
    }
}

/// Notification Center delegate (banners while the app is open, taps) and the
/// APNs callbacks.
final class AppDelegate: NSObject, UIApplicationDelegate, UNUserNotificationCenterDelegate {
    func application(_ application: UIApplication, didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil) -> Bool {
        UNUserNotificationCenter.current().delegate = self
        Task { @MainActor in APNsAdapter.registerIfEnabled() }
        // Photo backup: longer passes when iOS grants processing time (Wi-Fi, often charging).
        BGTaskScheduler.shared.register(forTaskWithIdentifier: PhotoBackup.processingIdentifier, using: nil) { task in
            let work = Task { @MainActor in
                let backup = AppModel.shared.backup
                await backup.runInBackground(budget: nil)
                PhotoBackup.scheduleProcessing(chargingOnly: backup.settings.chargingOnly)
                task.setTaskCompleted(success: true)
            }
            task.expirationHandler = { work.cancel() }
        }
        return true
    }

    /// iOS relaunched the app because background photo uploads finished.
    func application(_ application: UIApplication, handleEventsForBackgroundURLSession identifier: String,
                     completionHandler: @escaping () -> Void) {
        BackgroundUploader.shared.systemCompletion[identifier] = completionHandler
        BackgroundUploader.shared.reattach(identifier)
    }

    /// Portrait on iPhone, except while a video plays full screen (`OrientationLock`).
    func application(_ application: UIApplication, supportedInterfaceOrientationsFor window: UIWindow?) -> UIInterfaceOrientationMask {
        MainActor.assumeIsolated { UIDevice.current.userInterfaceIdiom == .pad ? .all : OrientationLock.mask }
    }

    func application(_ application: UIApplication, didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data) {
        let token = APNsAdapter.hex(deviceToken)
        Task { @MainActor in
            let app = AppModel.shared
            guard app.phase == .signedIn else { return }
            try? await NotificationsClient(client: app.client).registerAPNs(token: token, lang: AppLanguage.effectiveCode)
        }
    }

    func application(_ application: UIApplication, didFailToRegisterForRemoteNotificationsWithError error: Error) {
        // Expected without a paid team; local notifications keep working.
    }

    /// Show banners while the app is open (the store only posts what the owner isn't looking at).
    func userNotificationCenter(_ center: UNUserNotificationCenter, willPresent notification: UNNotification) async -> UNNotificationPresentationOptions {
        [.banner, .list, .sound]
    }

    func userNotificationCenter(_ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse) async {
        let info = response.notification.request.content.userInfo
        let route = info[LocalNotifier.routeKey] as? String
        let id = info[LocalNotifier.idKey] as? String
        await MainActor.run {
            var payload: [AnyHashable: Any] = [:]
            if let route { payload[LocalNotifier.routeKey] = route }
            if let id { payload[LocalNotifier.idKey] = id }
            let app = AppModel.shared
            Task { @MainActor in
                app.selectedTab = await app.notifications.openFromSystem(userInfo: payload)
                if let route, NotificationRoute.opensDealsMap(route) { app.showDealsMap = true }
            }
        }
    }
}

extension BackgroundRefresh {
    /// The `BGAppRefreshTask` work (the app may not have shown any UI yet).
    @MainActor
    static func run() async {
        let app = AppModel.shared
        guard app.client.accessKey?.isEmpty == false else { return }
        await app.notifications.backgroundRefresh()
    }
}

extension DeviceHub {
    /// Upcoming timed events from EventKit, when calendar access is on.
    func calendarAlerts() async -> [CalendarAlert] {
        guard access.isOn(.calendar) else { return [] }
        let state = await executors.currentState()
        return state.events.compactMap { event -> CalendarAlert? in
            guard event["allDay"] != .bool(true),
                  let id = event["id"]?.stringValue, !id.isEmpty,
                  let start = event["start"]?.stringValue.flatMap(UpcomingReminder.parse) else { return nil }
            return CalendarAlert(id: id, title: event["title"]?.stringValue ?? "", start: start)
        }
    }
}
