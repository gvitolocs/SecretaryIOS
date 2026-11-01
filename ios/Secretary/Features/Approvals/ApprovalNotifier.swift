import Foundation
import UserNotifications

/// What the store needs to tell the owner outside the app (a fake in tests).
@MainActor
protocol ApprovalNotifying: AnyObject {
    func post(_ item: ApprovalItem, previews: Bool) async
    func postFailure(for item: ApprovalItem) async
    func remove(ids: [String]) async
}

/// What a notification button or tap asks for.
enum ApprovalNotificationAction: Equatable {
    case approve, deny, open

    /// `UNNotificationResponse.actionIdentifier`; nil for the swipe-away.
    init?(identifier: String) {
        switch identifier {
        case ApprovalNotifier.approveAction: self = .approve
        case ApprovalNotifier.denyAction: self = .deny
        case UNNotificationDefaultActionIdentifier: self = .open
        default: return nil
        }
    }

    /// What the app does with it.
    enum Route: Equatable {
        case decide(ApprovalDecision)
        case openSheet
    }

    /// Deny, and a non-sensitive approve, are answered from the notification. A sensitive approve (or a tap) opens
    /// the card: the owner reads it there, and the passcode is asked only when he taps Approve on it.
    func route(sensitive: Bool) -> Route {
        switch self {
        case .deny: return .decide(.deny)
        case .approve: return sensitive ? .openSheet : .decide(.approve)
        case .open: return .openSheet
        }
    }
}

/// Local notifications for approvals, identifier `approval.<id>`; the server's APNs pushes use the same categories
/// and `userInfo` keys, and a request already on the lock screen from APNs is not posted again.
///
/// Category `APPROVAL` has Approve (the iPhone must be unlocked) and Deny (no unlock), both answered without
/// opening the app. `APPROVAL_SENSITIVE`'s Approve opens the card instead: a sensitive approve needs the passcode,
/// asked on the card. Older servers send `APPROVAL` for every request; the store opens the card for a sensitive one.
@MainActor
final class ApprovalNotifier: ApprovalNotifying {
    nonisolated static let category = "APPROVAL"
    nonisolated static let sensitiveCategory = "APPROVAL_SENSITIVE"
    nonisolated static let approveAction = "APPROVAL_APPROVE"
    nonisolated static let denyAction = "APPROVAL_DENY"
    /// Keys in `userInfo`.
    nonisolated static let idKey = "approvalId"
    nonisolated static let sensitiveKey = "approvalSensitive"

    let center: NotificationScheduling

    nonisolated init(center: NotificationScheduling = SystemNotificationCenter()) {
        self.center = center
    }

    static func identifier(_ id: String) -> String { "approval.\(id)" }

    /// Registers the two categories (the app sets no others).
    nonisolated static func registerCategories(on center: UNUserNotificationCenter = .current()) {
        let approve = UNNotificationAction(identifier: approveAction, title: String(localized: "Approve"), options: [.authenticationRequired],
                                           icon: UNNotificationActionIcon(systemImageName: "checkmark"))
        let approveInApp = UNNotificationAction(identifier: approveAction, title: String(localized: "Review and approve"),
                                                options: [.authenticationRequired, .foreground],
                                                icon: UNNotificationActionIcon(systemImageName: "lock"))
        let deny = UNNotificationAction(identifier: denyAction, title: String(localized: "Deny"), options: [.destructive],
                                        icon: UNNotificationActionIcon(systemImageName: "xmark"))
        let mine: Set<UNNotificationCategory> = [
            UNNotificationCategory(identifier: category, actions: [approve, deny], intentIdentifiers: [], options: []),
            UNNotificationCategory(identifier: sensitiveCategory, actions: [approveInApp, deny], intentIdentifiers: [], options: []),
            DecisionNotifier.pushCategoryDefinition(),
        ]
        // Decision categories (DecisionNotifier) registered earlier keep their buttons.
        center.getNotificationCategories { existing in
            center.setNotificationCategories(mine.union(existing.filter { $0.identifier.hasPrefix(DecisionNotifier.prefix) }))
        }
    }

    static func content(for item: ApprovalItem, previews: Bool) -> UNMutableNotificationContent {
        let c = UNMutableNotificationContent()
        c.title = String(localized: "\(item.requester.displayName) needs your OK")
        c.body = previews ? item.summary : String(localized: "Open Secretary to see what it is.")
        c.sound = .default
        c.categoryIdentifier = item.sensitive ? sensitiveCategory : category
        c.interruptionLevel = .timeSensitive
        c.threadIdentifier = category
        c.userInfo = [idKey: item.id, sensitiveKey: item.sensitive]
        return c
    }

    func post(_ item: ApprovalItem, previews: Bool) async {
        // Already on the lock screen from the server's push.
        if await !center.deliveredApprovals([item.id]).isEmpty { return }
        let request = UNNotificationRequest(identifier: Self.identifier(item.id), content: Self.content(for: item, previews: previews), trigger: nil)
        try? await center.add(request)
    }

    /// A button press that could not reach the server: the owner must know it was not recorded.
    func postFailure(for item: ApprovalItem) async {
        let c = UNMutableNotificationContent()
        c.title = String(localized: "Your answer didn't go through")
        c.body = String(localized: "Open Secretary and answer again: \(item.summary)")
        c.sound = .default
        c.userInfo = [Self.idKey: item.id, Self.sensitiveKey: item.sensitive]
        try? await center.add(UNNotificationRequest(identifier: "approval.failed.\(item.id)", content: c, trigger: nil))
    }

    func remove(ids: [String]) async {
        guard !ids.isEmpty else { return }
        // The server's APNs pushes have identifiers of their own: find them by their approval id.
        let pushed = await center.deliveredApprovals(Set(ids))
        await center.removeDelivered(ids.flatMap { [Self.identifier($0), "approval.failed.\($0)"] } + pushed)
    }
}
