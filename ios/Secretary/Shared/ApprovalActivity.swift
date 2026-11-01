import ActivityKit
import AppIntents
import Foundation

// An approval request on the Lock Screen and in the Dynamic Island, with Deny and Approve visible at once (no
// long-press). Compiled into the app and the SecretaryWidgets extension. The server starts it with a push-to-start
// push when the request is created and ends it when it is answered anywhere, withdrawn or expires
// (crates/secretary-server/src/notify/live.rs: the attribute names below are its wire format); the app also starts
// one in the foreground and ends stale ones (ApprovalActivities).
//
// The buttons are LiveActivityIntents, which iOS runs in the app's process through the handlers the app installs at
// launch (`ApprovalActivityCommands`). Deny needs no unlock (like the widget). Approve needs the iPhone unlocked; a
// plain request is then answered at once. A sensitive one (root plans) is signed with the Face ID key in the
// background when the activity showed the whole plan and the iPhone was unlocked with Face ID in the last 5 minutes (unless "Require Face ID or passcode for root
// approvals" is on); otherwise the app comes to the front on the request's card and starts the approve step there
// (Face ID or the PIN over the exact commands).

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
    /// The longest command line the Lock Screen shows whole (two lines of monospaced caption on an iPhone).
    static let fullyShownLineChars = 60

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

    /// The activity shows every command of `details` whole: at most `shownLines`, none clipped, none longer than
    /// `fullyShownLineChars`. Only then may one tap on the Lock Screen approve a sensitive request.
    static func fullyShown(_ details: String) -> Bool {
        let all = lines(details)
        let (commands, count) = commandLines(details)
        return count <= shownLines && commands == all.joined(separator: "\n")
            && all.allSatisfy { $0.count <= fullyShownLineChars }
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
/// `ApproveApprovalActivityIntent.perform` (`.activityApproveButton`), the notification's Approve action in
/// `AppDelegate.userNotificationCenter(_:didReceive:)` (`.notificationApproveAction`), and the card's Approve button
/// or PIN submit (`.cardApproveButton`). Polls, pushes, background refreshes and activity observers have none.
enum ApprovalTrigger: String, Equatable, Sendable {
    case activityApproveButton
    case notificationApproveAction
    case cardApproveButton
}

/// What the app does for the activity's buttons. Set by the app at launch; nil in the widget extension (it never
/// runs these intents: LiveActivityIntents and `openAppWhenRun` intents run in the app).
enum ApprovalActivityCommands {
    /// Deny `approvalId` (`sensitive` as the activity knows it).
    @MainActor static var deny: ((_ approvalId: String, _ sensitive: Bool) async -> Void)?
    /// Approve: answered in the background when it can be, else on the card in front (`intent` asks iOS for that).
    /// `payloadHash`: the hash of the request the activity showed (the tap is bound to it).
    @MainActor static var approve: ((_ approvalId: String, _ payloadHash: String, _ trigger: ApprovalTrigger,
                                     _ intent: any AppIntent) async -> Void)?
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
        guard let deny = ApprovalActivityCommands.deny else { throw ApprovalActivityError.notInApp }
        await deny(approvalId, sensitive)
        return .result()
    }
}

/// Approve from the Lock Screen: iOS asks to unlock this iPhone first (an unlocked Watch or Mac does not count).
/// The app decides how (plain, sensitive, strict mode, whether the activity showed the whole plan).
struct ApproveApprovalActivityIntent: LiveActivityIntent {
    static let title: LocalizedStringResource = "Approve an approval"
    static let isDiscoverable = false
    static let authenticationPolicy: IntentAuthenticationPolicy = .requiresLocalDeviceAuthentication
    /// Runs in the background, and may bring the app to the front when the approve step has to happen on the card
    /// (`continueInForeground`). iOS 26 replaced `ForegroundContinuableIntent` with this; it does nothing there.
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
        guard let approve = ApprovalActivityCommands.approve else { throw ApprovalActivityError.notInApp }
        // The owner tapped Approve on the activity: the only place this trigger is made.
        await approve(approvalId, payloadHash, .activityApproveButton, self)
        return .result()
    }
}
