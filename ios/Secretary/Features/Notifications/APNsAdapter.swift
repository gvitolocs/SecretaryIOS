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

    /// What iOS lets the app show, sent with the token: background wakes work without any of it.
    static func settingsReport(_ s: UNNotificationSettings) -> [String: String] {
        func on(_ v: UNNotificationSetting) -> String {
            switch v {
            case .enabled: return "on"
            case .disabled: return "off"
            case .notSupported: return "unsupported"
            @unknown default: return "unknown"
            }
        }
        let authorization: String
        switch s.authorizationStatus {
        case .notDetermined: authorization = "notDetermined"
        case .denied: authorization = "denied"
        case .authorized: authorization = "authorized"
        case .provisional: authorization = "provisional"
        case .ephemeral: authorization = "ephemeral"
        @unknown default: authorization = "unknown"
        }
        let style: String
        switch s.alertStyle {
        case .none: style = "none"
        case .banner: style = "banner"
        case .alert: style = "alert"
        @unknown default: style = "unknown"
        }
        return [
            "authorization": authorization,
            "alert": on(s.alertSetting),
            "alertStyle": style,
            "lockScreen": on(s.lockScreenSetting),
            "notificationCenter": on(s.notificationCenterSetting),
            "sound": on(s.soundSetting),
            "timeSensitive": on(s.timeSensitiveSetting),
            "scheduledDelivery": on(s.scheduledDeliverySetting),
        ]
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
        DebugEventsUploader.install(launchOptions: launchOptions)
        UNUserNotificationCenter.current().delegate = self
        // One-tap Approve as this iPhone last left it (the store registers them again when it changes).
        ApprovalNotifier.registerCategories(oneTap: ApprovalsStore.oneTapReady(unlockKeyId: UnlockApprovalSigner().keyId))
        Task { @MainActor in APNsAdapter.registerIfEnabled() }
        // Photo backup: longer passes when iOS grants processing time (Wi-Fi, often charging).
        BGTaskScheduler.shared.register(forTaskWithIdentifier: PhotoBackup.processingIdentifier, using: nil) { task in
            DebugEvents.record("bg.task", ["task": "photobackup", "phase": "start"])
            let work = Task { @MainActor in
                let backup = AppModel.shared.backup
                await backup.runInBackground(budget: nil)
                PhotoBackup.scheduleProcessing(chargingOnly: backup.settings.chargingOnly)
                DebugEvents.record("bg.task", ["task": "photobackup", "phase": "end"])
                task.setTaskCompleted(success: true)
            }
            task.expirationHandler = { DebugEvents.record("bg.task", ["task": "photobackup", "phase": "expired"]); work.cancel() }
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
            let report = APNsAdapter.settingsReport(await UNUserNotificationCenter.current().notificationSettings())
            try? await NotificationsClient(client: app.client).registerAPNs(token: token, lang: AppLanguage.effectiveCode, notifications: report)
        }
    }

    func application(_ application: UIApplication, didFailToRegisterForRemoteNotificationsWithError error: Error) {
        DebugEvents.record("apns.register", ["ok": false, "error": error])
        // Local notifications keep working.
    }

    /// An approval push woke the app (`content-available`): refresh the approvals and the widget. The push's own
    /// banner is already showing, so the request is not posted again.
    func application(_ application: UIApplication, didReceiveRemoteNotification userInfo: [AnyHashable: Any]) async -> UIBackgroundFetchResult {
        DebugEvents.record("notification.delivered", Self.debugFields(userInfo, via: "push"))
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
            // The push may land after the request's Live Activity card: the card carries it.
            if ApprovalActivities.shared.hasCard(approvalId) {
                await ApprovalActivities.shared.replaceNotifications([approvalId])
            }
            return .newData
        }.value
    }

    /// Show banners while the app is open (the store only posts what the owner isn't looking at).
    func userNotificationCenter(_ center: UNUserNotificationCenter, willPresent notification: UNNotification) async -> UNNotificationPresentationOptions {
        DebugEvents.record("notification.presented", Self.debugFields(notification.request.content.userInfo,
                                                                     category: notification.request.content.categoryIdentifier))
        // An approval is already on screen as the prompt while the app is open: no banner on top of it; and none at all
        // while its Live Activity card is up (one alert per request).
        if let id = notification.request.content.userInfo[ApprovalNotifier.idKey] as? String {
            let card = await MainActor.run { ApprovalActivities.shared.hasCard(id) }
            return card && !notification.request.identifier.hasPrefix("approval.failed.") ? [] : [.list]
        }
        return [.banner, .list, .sound]
    }

    /// What the debug log keeps of a notification: its kind, id and category, never its text.
    static func debugFields(_ info: [AnyHashable: Any], category: String? = nil, via: String? = nil) -> [String: Any] {
        var fields: [String: Any] = [:]
        if let id = info[ApprovalNotifier.idKey] as? String {
            fields["kind"] = "approval"
            fields["id"] = id
            if let sensitive = info[ApprovalNotifier.sensitiveKey] as? Bool ?? info["sensitive"] as? Bool { fields["sensitive"] = sensitive }
        } else if let id = info[DecisionNotifier.idKey] as? String {
            fields["kind"] = "decision"
            fields["id"] = id
        } else {
            fields["kind"] = (info[LocalNotifier.routeKey] as? String).map { _ in "route" } ?? "other"
        }
        if let category, !category.isEmpty { fields["category"] = category }
        if let via { fields["via"] = via }
        return fields
    }

    /// The trigger of a notification answer: only the notification's own Approve actions approve (the card one and
    /// the one-tap one); a tap, Deny or any other action carries none.
    static func approvalTrigger(forAction actionIdentifier: String) -> ApprovalTrigger? {
        actionIdentifier == ApprovalNotifier.approveAction || actionIdentifier == ApprovalNotifier.approveInPlaceAction
            ? .notificationApproveAction : nil
    }

    func userNotificationCenter(_ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse) async {
        let info = response.notification.request.content.userInfo
        var debug = Self.debugFields(info, category: response.notification.request.content.categoryIdentifier)
        debug["action"] = response.actionIdentifier
        DebugEvents.record("notification.action", debug)
        defer { Task { @MainActor in DebugEventsUploader.flushInBackground("notification_action") } }
        // An approval: Approve / Deny from the notification, or its tap opens the prompt.
        if let approvalId = info[ApprovalNotifier.idKey] as? String {
            // Local notifications say `approvalSensitive`, older server pushes only `sensitive`; unknown = sensitive.
            let sensitive = info[ApprovalNotifier.sensitiveKey] as? Bool ?? info["sensitive"] as? Bool ?? true
            let action = response.actionIdentifier
            ApprovalActivityCommands.log.info("notification action=\(action, privacy: .public) id=\(approvalId, privacy: .public)")
            // The owner tapped the notification's Approve: the only place this trigger is made.
            let trigger = Self.approvalTrigger(forAction: action)
            // A root plan whose notification showed every command, and the hash of what it showed.
            let inline = response.notification.request.content.categoryIdentifier == ApprovalNotifier.sensitiveInlineCategory
            let shownHash = ApprovalNotifier.payloadHash(in: info)
            await AppModel.shared.approvals.handleNotification(actionIdentifier: action, approvalId: approvalId,
                                                               sensitive: sensitive, inline: inline, shownHash: shownHash,
                                                               trigger: trigger)
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
        DebugEvents.record("bg.task", ["task": "refresh", "phase": "start"])
        await app.notifications.backgroundRefresh()
        await app.approvals.poll()
        app.memos.queue.kick()
        DebugEvents.record("bg.task", ["task": "refresh", "phase": "end"])
        await DebugEvents.flush()
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
