import Foundation
import UserNotifications

/// What the store needs to tell the owner outside the app (a fake in tests).
@MainActor
protocol ApprovalNotifying: AnyObject {
    func post(_ item: ApprovalItem, previews: Bool) async
    func postFailure(for item: ApprovalItem) async
    /// An Approve the notification could not answer in place: "Open Secretary to approve", its tap opens the card.
    func postOpenToApprove(for item: ApprovalItem) async
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
        /// The card, with its approve step started: Face ID at once, over the commands.
        case openSheetAndApprove
        case openSheet
    }

    /// Deny, and a plain approve, are answered from the notification. A sensitive approve opens the card (the app comes
    /// to the front): with its approve step started when the notification showed the whole plan (`inline`: category
    /// `APPROVAL_SENSITIVE_INLINE`), else for the owner to read it and tap Approve there. A tap opens the card.
    func route(sensitive: Bool, inline: Bool) -> Route {
        switch self {
        case .deny: return .decide(.deny)
        case .approve:
            guard sensitive else { return .decide(.approve) }
            return inline ? .openSheetAndApprove : .openSheet
        case .open: return .openSheet
        }
    }
}

/// Local notifications for approvals, identifier `approval.<id>`; the server's APNs pushes use the same categories
/// and `userInfo` keys, and a request already on the lock screen from APNs is not posted again.
///
/// Category `APPROVAL` has Approve (the iPhone must be unlocked) and Deny (no unlock), both answered without
/// opening the app. `APPROVAL_SENSITIVE`'s Approve opens the card instead: a sensitive approve needs Face ID or the
/// passcode, asked on the card. `APPROVAL_SENSITIVE_INLINE` is a sensitive request whose body carries the whole plan:
/// its Approve opens the card with Face ID asked at once. Older servers send `APPROVAL` for every request; the store
/// opens the card for a sensitive one.
@MainActor
final class ApprovalNotifier: ApprovalNotifying {
    nonisolated static let category = "APPROVAL"
    nonisolated static let sensitiveCategory = "APPROVAL_SENSITIVE"
    nonisolated static let sensitiveInlineCategory = "APPROVAL_SENSITIVE_INLINE"
    nonisolated static let approveAction = "APPROVAL_APPROVE"
    nonisolated static let denyAction = "APPROVAL_DENY"
    /// Keys in `userInfo`.
    nonisolated static let idKey = "approvalId"
    nonisolated static let sensitiveKey = "approvalSensitive"
    /// The request's payload hash: `approvalPayloadHash` in the app's own notifications, `payloadHash` in the server's
    /// pushes.
    nonisolated static let payloadHashKey = "approvalPayloadHash"
    nonisolated static let pushPayloadHashKey = "payloadHash"
    /// Bytes of the commands a notification body carries (the server's `APNS_DETAIL_BYTES`).
    nonisolated static let detailBytes = 2400

    let center: NotificationScheduling

    nonisolated init(center: NotificationScheduling = SystemNotificationCenter()) {
        self.center = center
    }

    static func identifier(_ id: String) -> String { "approval.\(id)" }

    /// The payload hash a notification carries (the app's own key, or the server push's).
    nonisolated static func payloadHash(in userInfo: [AnyHashable: Any]) -> String? {
        (userInfo[payloadHashKey] as? String) ?? (userInfo[pushPayloadHashKey] as? String)
    }

    /// The first `max` UTF-8 bytes of `text`, cut at a line (or else a character) boundary, with "\n…" when cut; and
    /// whether it was cut. Like the server's `clip_bytes`.
    nonisolated static func clipBytes(_ text: String, max: Int = detailBytes) -> (text: String, cut: Bool) {
        let s = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard s.utf8.count > max else { return (s, false) }
        var head = ""
        var used = 0
        for ch in s {
            let n = String(ch).utf8.count
            if used + n > max { break }
            head.append(ch)
            used += n
        }
        if let nl = head.lastIndex(of: "\n") { head = String(head[..<nl]) }
        while head.last?.isWhitespace == true { head.removeLast() }
        return (head + "\n…", true)
    }

    /// Registers the categories (the app sets no others).
    nonisolated static func registerCategories(on center: UNUserNotificationCenter = .current()) {
        let approve = UNNotificationAction(identifier: approveAction, title: String(localized: "Approve"), options: [.authenticationRequired],
                                           icon: UNNotificationActionIcon(systemImageName: "checkmark"))
        let approveInApp = UNNotificationAction(identifier: approveAction, title: String(localized: "Review and approve"),
                                                options: [.authenticationRequired, .foreground],
                                                icon: UNNotificationActionIcon(systemImageName: "lock"))
        // The whole plan was in the body: Approve brings the card with Face ID at once.
        let approveWithFaceID = UNNotificationAction(identifier: approveAction, title: String(localized: "Approve"),
                                                     options: [.authenticationRequired, .foreground],
                                                     icon: UNNotificationActionIcon(systemImageName: "faceid"))
        let deny = UNNotificationAction(identifier: denyAction, title: String(localized: "Deny"), options: [.destructive],
                                        icon: UNNotificationActionIcon(systemImageName: "xmark"))
        let mine: Set<UNNotificationCategory> = [
            UNNotificationCategory(identifier: category, actions: [approve, deny], intentIdentifiers: [], options: []),
            UNNotificationCategory(identifier: sensitiveCategory, actions: [approveInApp, deny], intentIdentifiers: [], options: []),
            UNNotificationCategory(identifier: sensitiveInlineCategory, actions: [approveWithFaceID, deny],
                                   intentIdentifiers: [], options: []),
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
        c.sound = .default
        c.interruptionLevel = .timeSensitive
        c.threadIdentifier = category
        c.userInfo = [idKey: item.id, sensitiveKey: item.sensitive, payloadHashKey: item.payloadHash]
        guard previews else {
            c.body = String(localized: "Open Secretary to see what it is.")
            c.categoryIdentifier = item.sensitive ? sensitiveCategory : category
            return c
        }
        guard item.sensitive else {
            c.body = item.summary
            c.categoryIdentifier = category
            return c
        }
        // A sensitive request: the summary, then its commands (the expanded notification, where the buttons are, shows
        // them all). Approve starts Face ID on the card at once only when every command is in the body.
        let details = item.details.trimmingCharacters(in: .whitespacesAndNewlines)
        let (shown, cut) = clipBytes(details)
        c.body = details.isEmpty ? item.summary : item.summary + "\n" + shown
        c.categoryIdentifier = !details.isEmpty && !cut ? sensitiveInlineCategory : sensitiveCategory
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

    /// Uses the failure's slot: either way the owner has to answer in the app, and it goes with the request.
    func postOpenToApprove(for item: ApprovalItem) async {
        let c = UNMutableNotificationContent()
        c.title = String(localized: "Open Secretary to approve")
        c.body = item.summary
        c.sound = .default
        c.interruptionLevel = .timeSensitive
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
