import BackgroundTasks
import Foundation
import UIKit
import UserNotifications

/// Remote push (APNs): **on** (paid team 2N4DA9L8VH, `aps-environment: development` in
/// `Secretary.entitlements`, `SecretaryAPNsEnabled` in `project.yml`). The app sends its device token to
/// `POST /v1/push/apns`; the server pushes there once it has an APNs auth key (`SECRETARY_APNS_*`,
/// docs/notifications.md). Approval pushes carry `approvalId`, so their Approve / Deny buttons work from the
/// lock screen, and `content-available`, so the app refreshes its approvals and the widget.
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
        ApprovalNotifier.registerCategories()
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
        if identifier.hasPrefix(ShareRequestBuilder.prefix) {
            ShareUploads.shared.handleEvents(identifier, completion: completionHandler)
            return
        }
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
        // Local notifications keep working.
    }

    /// An approval push woke the app (`content-available`): refresh the approvals and the widget. The push's own
    /// banner is already showing, so the request is not posted again.
    func application(_ application: UIApplication, didReceiveRemoteNotification userInfo: [AnyHashable: Any]) async -> UIBackgroundFetchResult {
        // A decision push: swap in the notification with the options' own buttons.
        if let decisionId = userInfo[DecisionNotifier.idKey] as? String {
            let options = userInfo[DecisionNotifier.optionsKey] as? [String] ?? []
            let alert = (userInfo["aps"] as? [String: Any])?["alert"] as? [String: Any]
            let title = alert?["title"] as? String ?? String(localized: "Lucina needs your decision")
            // The push's text is the question plus the numbered options; the labelled buttons make the list redundant.
            let body = (alert?["body"] as? String ?? "").components(separatedBy: "\n1. ").first ?? ""
            guard !options.isEmpty else { return .noData }
            await DecisionNotifier.replacePush(id: decisionId, title: title, body: body, options: options, previews: true)
            return .newData
        }
        guard let approvalId = userInfo[ApprovalNotifier.idKey] as? String else { return .noData }
        return await Task { @MainActor in
            let app = AppModel.shared
            guard app.phase == .signedIn else { return UIBackgroundFetchResult.noData }
            app.approvals.markNotified(approvalId)
            await app.approvals.poll()
            return .newData
        }.value
    }

    /// Show banners while the app is open (the store only posts what the owner isn't looking at).
    func userNotificationCenter(_ center: UNUserNotificationCenter, willPresent notification: UNNotification) async -> UNNotificationPresentationOptions {
        // An approval is already on screen as the prompt while the app is open: no banner on top of it.
        if notification.request.content.userInfo[ApprovalNotifier.idKey] != nil { return [.list] }
        return [.banner, .list, .sound]
    }

    func userNotificationCenter(_ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse) async {
        let info = response.notification.request.content.userInfo
        // An approval: Approve / Deny from the notification, or its tap opens the prompt.
        if let approvalId = info[ApprovalNotifier.idKey] as? String {
            // Local notifications say `approvalSensitive`, older server pushes only `sensitive`; unknown = sensitive.
            let sensitive = info[ApprovalNotifier.sensitiveKey] as? Bool ?? info["sensitive"] as? Bool ?? true
            let action = response.actionIdentifier
            await AppModel.shared.approvals.handleNotification(actionIdentifier: action, approvalId: approvalId, sensitive: sensitive)
            return
        }
        // A decision: an option or the owner's words from the notification, or its tap opens it.
        if let decisionId = info[DecisionNotifier.idKey] as? String {
            let options = info[DecisionNotifier.optionsKey] as? [String] ?? []
            let text = (response as? UNTextInputNotificationResponse)?.userText
            guard let r = DecisionNotifier.response(actionIdentifier: response.actionIdentifier, userText: text) else { return }
            await Task { @MainActor in
                let app = AppModel.shared
                switch r {
                case .open: app.openDecision(decisionId)
                case .option(let i) where options.indices.contains(i):
                    await app.decisions.answer(id: decisionId, index: i, option: options[i])
                case .option: app.openDecision(decisionId)
                case .words(let w): await app.decisions.answer(id: decisionId, words: w)
                }
            }.value
            return
        }
        let route = info[LocalNotifier.routeKey] as? String
        let id = info[LocalNotifier.idKey] as? String
        await MainActor.run {
            var payload: [AnyHashable: Any] = [:]
            if let route { payload[LocalNotifier.routeKey] = route }
            if let id { payload[LocalNotifier.idKey] = id }
            let app = AppModel.shared
            Task { @MainActor in
                app.selectedTab = await app.notifications.openFromSystem(userInfo: payload)
                if let route, NotificationRoute.isSafe(route), Place.fromRoute(route) != nil { app.open(route: route) }
                if let route, NotificationRoute.opensDealsMap(route) { app.showDealsMap = true }
                if let route, let memo = NotificationRoute.memo(route) { app.openMemo(memo.isEmpty ? nil : memo) }
                if let route, let decision = NotificationRoute.decision(route) { app.openDecision(decision) }
                if let route, let server = NotificationRoute.server(route) { app.openServer(server) }
                if let route, let calendar = NotificationRoute.calendar(route) { app.openCalendar(calendar) }
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
        await app.approvals.poll()
        app.memos.queue.kick()
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
