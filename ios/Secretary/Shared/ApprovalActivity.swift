import ActivityKit
import AppIntents
import Foundation
import os

// An approval request on the Lock Screen and in the Dynamic Island, with Deny and Approve visible at once (no
// long-press). Compiled into the app and the SecretaryWidgets extension. The server starts it with a push-to-start
// push when the request is created and ends it when it is answered anywhere, withdrawn or expires
// (crates/secretary-server/src/notify/live.rs: the attribute names below are its wire format); the app also starts
// one in the foreground and ends stale ones (ApprovalActivities).
//
// The card has two intent buttons, Deny then Approve, LiveActivityIntents that iOS runs in the app's process through
// the handlers the app installs at launch (`ApprovalActivityCommands`). Deny needs no unlock (like the widget).
// Approve needs this iPhone unlocked (iOS asks Face ID or the passcode on the Lock Screen before it runs); with one-tap
// Approve ready (the unlock key enrolled, the switch on) the app signs it with the unlock key and answers in the
// background, so the card stays on the Lock Screen and ends "Approved". Without it, the app comes to the front on the
// request's card with its approve step (Face ID over the exact commands). Each tap is logged first thing
// (`card tap intent=<Deny|Approve> id=<approvalId>`), so a tap that reached the wrong intent shows in the log.
// Cards from before carry an Approve link (`ApprovalLink`); the app still opens those on the card.

struct ApprovalActivityAttributes: ActivityAttributes {
    struct ContentState: Codable, Hashable {
        /// `pending`, `approved`, `denied`, `expired` or `withdrawn`.
        var status: String
    }

    var approvalId: String
    var requester: String
    var summary: String
    /// The first 3 command lines, joined by "\n".
    var commands: String
    var commandCount: Int
    var sensitive: Bool
    var payloadHash: String
    /// Seconds since 1970. Not a `Date`: ActivityKit decodes a push's dates as seconds since 2001.
    var expiresAtUnix: Double

    var expiresAt: Date { Date(timeIntervalSince1970: expiresAtUnix) }

    /// Command lines the activity shows.
    static let shownLines = 3

    private static func lines(_ details: String) -> [String] {
        details.split(separator: "\n", omittingEmptySubsequences: false)
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
    }

    /// `details` (one command per line) as the activity's `commands` and `commandCount`, like the server does.
    static func commandLines(_ details: String) -> (commands: String, count: Int) {
        let lines = lines(details)
        let head = lines.prefix(shownLines).map { $0.count > 120 ? String($0.prefix(119)) + "…" : $0 }
        return (head.joined(separator: "\n"), lines.count)
    }

    /// The approval ids among `activityIds` (the approvals the activities show) that are no longer pending.
    static func toEnd(activityIds: [String], pending: Set<String>) -> [String] {
        var seen = Set<String>()
        return activityIds.filter { !pending.contains($0) && seen.insert($0).inserted }
    }
}

extension ApprovalActivityAttributes.ContentState {
    static let pending = Self(status: "pending")
    var isPending: Bool { status == "pending" }
}

/// The owner's explicit tap that an approve comes from. Every approve carries one; only these call sites create it:
/// `ApproveApprovalActivityIntent.perform` (`.activityApproveButton`), an older card's Approve link in
/// `AppModel.openWidgetRoute` (`.activityApproveLink`: only ever starts the card's approve step), the notification's
/// Approve action in `AppDelegate.userNotificationCenter(_:didReceive:)` (`.notificationApproveAction`), and the
/// card's Approve button or PIN submit (`.cardApproveButton`). Polls, pushes, background refreshes and activity
/// observers have none.
enum ApprovalTrigger: String, Equatable, Sendable {
    case activityApproveButton
    case activityApproveLink
    case notificationApproveAction
    case cardApproveButton
}

/// The activity's links into the app: `approvals/<id>` (Details) and `approvals/<id>?approve=1&hash=<payloadHash>`
/// (an older card's Approve: the card with its approve step, for the request the activity showed).
enum ApprovalLink: Equatable {
    case details(id: String)
    case approve(id: String, payloadHash: String)

    static func route(id: String, approve payloadHash: String? = nil) -> String {
        let path = "approvals/\(WidgetRoute.segment(id))"
        guard let payloadHash else { return path }
        return "\(path)?approve=1&hash=\(WidgetRoute.segment(payloadHash))"
    }

    static func url(id: String, approve payloadHash: String? = nil) -> URL {
        WidgetRoute.url(route(id: id, approve: payloadHash))
    }

    /// A route from `WidgetRoute.route(from:)` (still percent-encoded); nil for any other route. Approve needs
    /// `approve=1` and a hash; anything less is Details.
    static func parse(_ route: String) -> ApprovalLink? {
        guard route.hasPrefix("approvals/") else { return nil }
        let rest = route.dropFirst("approvals/".count)
        let parts = rest.split(separator: "?", maxSplits: 1, omittingEmptySubsequences: false)
        // One segment (an id's own `/` comes encoded).
        guard let segment = parts.first, !segment.contains("/"),
              let id = String(segment).removingPercentEncoding, !id.isEmpty else { return nil }
        guard parts.count == 2, let items = URLComponents(string: "x:?\(parts[1])")?.queryItems else { return .details(id: id) }
        let value = { (name: String) in items.first { $0.name == name }?.value }
        guard value("approve") == "1", let hash = value("hash"), !hash.isEmpty else { return .details(id: id) }
        return .approve(id: id, payloadHash: hash)
    }
}

/// What the app does for the activity's buttons. Set by the app at launch; nil in the widget extension (it never runs
/// these intents: LiveActivityIntents run in the app).
enum ApprovalActivityCommands {
    /// Deny `approvalId` (`sensitive` as the activity knows it).
    @MainActor static var deny: ((_ approvalId: String, _ sensitive: Bool) async -> Void)?
    /// Approve: in the background with the unlock key when it can be, else on the card in front (`intent` asks iOS
    /// for that). `payloadHash`: the hash of the request the activity showed (the tap is bound to it).
    @MainActor static var approve: ((_ approvalId: String, _ payloadHash: String, _ trigger: ApprovalTrigger,
                                     _ intent: any AppIntent) async -> Void)?

    /// The card's taps and what came of them (public: ids only, never a request's details).
    static let log = Logger(subsystem: "tech.getsecretary.app", category: "ApprovalActivity")
}

enum ApprovalActivityError: Error, CustomLocalizedStringResourceConvertible {
    case notInApp

    var localizedStringResource: LocalizedStringResource { "Open Secretary once, then try again." }
}

/// Deny from the Lock Screen: no unlock needed (a deny never needs Face ID).
struct DenyApprovalActivityIntent: LiveActivityIntent {
    static let title: LocalizedStringResource = "Deny an approval"
    static let isDiscoverable = false
    static let authenticationPolicy: IntentAuthenticationPolicy = .alwaysAllowed

    @Parameter(title: "Approval") var approvalId: String
    @Parameter(title: "Sensitive") var sensitive: Bool

    init() {}

    init(approvalId: String, sensitive: Bool) {
        self.approvalId = approvalId
        self.sensitive = sensitive
    }

    @MainActor
    func perform() async throws -> some IntentResult {
        ApprovalActivityCommands.log.info("card tap intent=Deny id=\(approvalId, privacy: .public)")
        let trace = DebugEvents.intent("deny_approval_activity", ["id": approvalId, "sensitive": sensitive]); defer { trace.end() }
        guard let deny = ApprovalActivityCommands.deny else { throw ApprovalActivityError.notInApp }
        await deny(approvalId, sensitive)
        return .result()
    }
}

/// Approve from the Lock Screen: iOS asks to unlock this iPhone first (Face ID or the passcode; an unlocked Watch or
/// Mac does not count). With one-tap Approve ready it is answered in the background and the card ends "Approved";
/// otherwise the card in the app, in front, with its approve step (Face ID there).
struct ApproveApprovalActivityIntent: LiveActivityIntent {
    static let title: LocalizedStringResource = "Approve an approval"
    static let isDiscoverable = false
    static let authenticationPolicy: IntentAuthenticationPolicy = .requiresLocalDeviceAuthentication
    /// Runs in the background, and brings the app to the front when the approve step has to happen on the card
    /// (`continueInForeground`). iOS 26 replaced `ForegroundContinuableIntent` with this.
    @available(iOS 26.0, *)
    static var supportedModes: IntentModes { [.background, .foreground(.dynamic)] }

    @Parameter(title: "Approval") var approvalId: String
    @Parameter(title: "Payload hash") var payloadHash: String

    init() {}

    init(approvalId: String, payloadHash: String) {
        self.approvalId = approvalId
        self.payloadHash = payloadHash
    }

    @MainActor
    func perform() async throws -> some IntentResult {
        ApprovalActivityCommands.log.info("card tap intent=Approve id=\(approvalId, privacy: .public)")
        let trace = DebugEvents.intent("approve_approval_activity", ["id": approvalId]); defer { trace.end() }
        guard let approve = ApprovalActivityCommands.approve else { throw ApprovalActivityError.notInApp }
        // The owner tapped Approve on the activity: the only place this trigger is made.
        await approve(approvalId, payloadHash, .activityApproveButton, self)
        return .result()
    }
}
