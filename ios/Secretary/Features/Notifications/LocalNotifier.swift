import Foundation
import UserNotifications

/// What `LocalNotifier` needs from `UNUserNotificationCenter` (a fake in tests).
protocol NotificationScheduling: AnyObject, Sendable {
    func add(_ request: UNNotificationRequest) async throws
    func pendingIdentifiers() async -> [String]
    func removePending(_ identifiers: [String])
    func authorizationStatus() async -> UNAuthorizationStatus
    func requestAuthorization() async throws -> Bool
    func setBadge(_ count: Int) async
    /// Takes notifications off the lock screen and Notification Center (an approval answered elsewhere).
    func removeDelivered(_ identifiers: [String]) async
    /// The identifiers of delivered notifications (local or APNs) about these approval ids.
    func deliveredApprovals(_ ids: Set<String>) async -> [String]
}

extension NotificationScheduling {
    func removeDelivered(_ identifiers: [String]) async {}
    func deliveredApprovals(_ ids: Set<String>) async -> [String] { [] }
}

final class SystemNotificationCenter: NotificationScheduling, @unchecked Sendable {
    private var center: UNUserNotificationCenter { .current() }

    func add(_ request: UNNotificationRequest) async throws { try await center.add(request) }

    func pendingIdentifiers() async -> [String] { await center.pendingNotificationRequests().map(\.identifier) }

    func removePending(_ identifiers: [String]) { center.removePendingNotificationRequests(withIdentifiers: identifiers) }

    func authorizationStatus() async -> UNAuthorizationStatus { await center.notificationSettings().authorizationStatus }

    func requestAuthorization() async throws -> Bool {
        try await center.requestAuthorization(options: [.alert, .sound, .badge])
    }

    func setBadge(_ count: Int) async { try? await center.setBadgeCount(count) }

    func removeDelivered(_ identifiers: [String]) async {
        center.removeDeliveredNotifications(withIdentifiers: identifiers)
        center.removePendingNotificationRequests(withIdentifiers: identifiers)
    }

    /// The identifiers of delivered notifications (local or APNs) about these approval ids.
    func deliveredApprovals(_ ids: Set<String>) async -> [String] {
        await center.deliveredNotifications().compactMap { n in
            guard let id = n.request.content.userInfo[ApprovalNotifier.idKey] as? String, ids.contains(id) else { return nil }
            return n.request.identifier
        }
    }
}

/// Local notifications: immediate ones for what arrives while the app runs or
/// refreshes in the background, and scheduled ones for known future items
/// (Lucina's reminders, calendar events) that fire even when the app is closed.
///
/// Identifiers: `inbox.<notification id>`, `tl.<timeline id>`,
/// `reminder.<reminder id>`, `calendar.<event id>.<start ms>`.
/// iOS keeps at most 64 pending requests: at most [`maxReminders`] reminders
/// and [`maxEvents`] events are scheduled, soonest first.
@MainActor
final class LocalNotifier {
    static let maxReminders = 40
    static let maxEvents = 20
    /// Keys in `userInfo`.
    static let routeKey = "route"
    static let idKey = "notificationId"

    let center: NotificationScheduling

    nonisolated init(center: NotificationScheduling = SystemNotificationCenter()) {
        self.center = center
    }

    private func content(title: String, body: String, category: NotificationCategory, route: String, id: String?) -> UNMutableNotificationContent {
        let c = UNMutableNotificationContent()
        c.title = title
        c.body = body
        c.sound = .default
        c.threadIdentifier = category.rawValue
        c.categoryIdentifier = category.rawValue
        var info: [String: String] = [Self.routeKey: NotificationRoute.isSafe(route) ? route : "/"]
        if let id { info[Self.idKey] = id }
        c.userInfo = info
        return c
    }

    /// An inbox notification, now.
    func post(_ n: AppNotification, previews: Bool = true) async {
        if let id = n.decisionId {
            // Already on the lock screen from the server's push: the push's wake-up replaces it with the labelled one.
            if await !DecisionNotifier.delivered(id: id).isEmpty { return }
            // Its own category: long-press shows one button per option; a tap opens the decision.
            await DecisionNotifier.post(id: id, title: n.displayTitle, body: n.body, options: n.decisionOptions, previews: previews)
            return
        }
        let body = previews ? n.body : String(localized: "Open Secretary to read it.")
        let request = UNNotificationRequest(identifier: "inbox.\(n.id)",
                                            content: content(title: n.displayTitle, body: body, category: n.category, route: n.route, id: n.id),
                                            trigger: nil)
        try? await center.add(request)
    }

    /// A Lucina timeline item that arrived while the chat was not in view.
    func post(timelineId: String, category: NotificationCategory, text: String, previews: Bool = true) async {
        let body = previews ? text : String(localized: "Open Secretary to read it.")
        let request = UNNotificationRequest(identifier: "tl.\(timelineId)",
                                            content: content(title: category.title, body: body, category: category, route: "/", id: nil),
                                            trigger: nil)
        try? await center.add(request)
    }

    private func trigger(at date: Date) -> UNCalendarNotificationTrigger {
        let parts = Calendar.current.dateComponents([.year, .month, .day, .hour, .minute, .second], from: date)
        return UNCalendarNotificationTrigger(dateMatching: parts, repeats: false)
    }

    /// Replace the scheduled reminder notifications with `reminders`. Returns the
    /// ids now scheduled.
    @discardableResult
    func schedule(reminders: [UpcomingReminder], rules: NotificationRules, now: Date = Date()) async -> [String] {
        let wanted = rules.allows(.reminder)
            ? reminders
                .filter { $0.dueAt > now && rules.shouldAlert(.reminder, at: $0.dueAt) }
                .sorted { $0.dueAt < $1.dueAt }
                .prefix(Self.maxReminders)
            : []
        let ids = wanted.map { "reminder.\($0.id)" }
        await replace(prefix: "reminder.", keeping: Set(ids))
        for r in wanted {
            let request = UNNotificationRequest(identifier: "reminder.\(r.id)",
                                                content: content(title: NotificationCategory.reminder.title, body: r.text, category: .reminder, route: "/", id: nil),
                                                trigger: trigger(at: r.dueAt))
            try? await center.add(request)
        }
        return ids
    }

    /// Replace the scheduled calendar notifications: each fires `calendarLeadMinutes` before its event.
    @discardableResult
    func schedule(events: [CalendarAlert], rules: NotificationRules, now: Date = Date()) async -> [String] {
        let lead = TimeInterval(max(0, rules.settings.calendarLeadMinutes) * 60)
        let formatter = DateFormatter()
        formatter.timeStyle = .short
        formatter.dateStyle = .none
        let wanted = rules.allows(.calendar)
            ? events
                .map { (event: $0, fire: $0.start.addingTimeInterval(-lead)) }
                .filter { $0.fire > now && rules.shouldAlert(.calendar, at: $0.fire) }
                .sorted { $0.fire < $1.fire }
                .prefix(Self.maxEvents)
            : []
        let ids = wanted.map { Self.calendarId($0.event) }
        await replace(prefix: "calendar.", keeping: Set(ids))
        for (event, fire) in wanted {
            let time = formatter.string(from: event.start)
            let body = event.title.isEmpty ? time : "\(event.title) · \(time)"
            let request = UNNotificationRequest(identifier: Self.calendarId(event),
                                                content: content(title: NotificationCategory.calendar.title, body: body, category: .calendar, route: "/", id: nil),
                                                trigger: trigger(at: fire))
            try? await center.add(request)
        }
        return ids
    }

    static func calendarId(_ event: CalendarAlert) -> String {
        "calendar.\(event.id).\(Int64(event.start.timeIntervalSince1970 * 1000))"
    }

    private func replace(prefix: String, keeping: Set<String>) async {
        let stale = await center.pendingIdentifiers().filter { $0.hasPrefix(prefix) && !keeping.contains($0) }
        if !stale.isEmpty { center.removePending(stale) }
    }

    /// Everything scheduled by this app (sign-out).
    func removeAll() async {
        let mine = await center.pendingIdentifiers().filter { $0.hasPrefix("reminder.") || $0.hasPrefix("calendar.") }
        center.removePending(mine)
        await center.setBadge(0)
    }
}
