import ActivityKit
import Foundation
import os

/// The app's side of the approval Live Activity (Secretary/Shared/ApprovalActivity.swift): it sends the push-to-start
/// token and each activity's update token to the server (`POST /v1/push/liveactivity`), runs the activity's buttons
/// through `ApprovalsStore`, ends activities whose request is no longer pending, and, with the app in front, starts
/// one for a pending request that has none (the push-to-start token may be missing). Never two for one request.
/// Tokens and request details are never logged.
@MainActor
final class ApprovalActivities {
    static let shared = ApprovalActivities()
    /// The push-to-start token the server has (sent again only when it changes).
    static let startTokenKey = "secretary.approvals.liveActivityStartToken"
    /// An activity the app ends after its own answer stays this long, showing the answer.
    static let ownAnswerLinger: TimeInterval = 4

    private let defaults: UserDefaults
    private let log = Logger(subsystem: "tech.getsecretary.app", category: "ApprovalActivity")
    private var client: APIClient?
    private var status: ((String) async -> ApprovalStatus?)?
    private var observing = false
    /// Activity id → the approval it shows, for the activities whose tokens are watched.
    private var watched: [String: String] = [:]
    /// Approvals that had an activity during this launch: none is started for them again (the owner may have swiped
    /// it away).
    private var shown: Set<String> = []
    /// The latest tokens iOS gave, and what the server already has.
    private var startToken: String?
    private var updateTokens: [String: String] = [:]
    private var sentUpdates: Set<String> = []
    private var sending: Set<String> = []
    /// Approvals whose activity is being ended (a status lookup is in flight).
    private var ending: Set<String> = []

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    /// Installs the buttons' handlers. Called from `SecretaryApp.init`, before any scene: a button may be all this
    /// launch is for (iOS runs LiveActivityIntents in the app's process).
    static func install() {
        ApprovalActivityCommands.deny = { id, sensitive in
            await AppModel.shared.approvals.handleNotification(actionIdentifier: ApprovalNotifier.denyAction,
                                                               approvalId: id, sensitive: sensitive)
        }
        // The plain Approve answers like the notification's; a request that became sensitive opens its card.
        ApprovalActivityCommands.approve = { id in
            await AppModel.shared.approvals.handleNotification(actionIdentifier: ApprovalNotifier.approveAction,
                                                               approvalId: id, sensitive: false)
        }
        // The app is coming to the front: the card opens and starts the approve step (Face ID or the PIN).
        ApprovalActivityCommands.approveOnCard = { id in
            await AppModel.shared.approvals.open(id: id, startApprove: true)
        }
    }

    /// Watches the tokens and the activities (from AppModel, once it is built). `status` looks a request up on the
    /// server.
    func start(client: APIClient, status: @escaping (String) async -> ApprovalStatus?) {
        self.client = client
        self.status = status
        guard !observing else { return }
        observing = true
        if #available(iOS 17.2, *) {
            Task {
                for await data in Activity<ApprovalActivityAttributes>.pushToStartTokenUpdates {
                    self.startToken = APNsAdapter.hex(data)
                    self.flush()
                }
            }
        }
        Task {
            for await activity in Activity<ApprovalActivityAttributes>.activityUpdates { self.watch(activity) }
        }
        for activity in Activity<ApprovalActivityAttributes>.activities { watch(activity) }
    }

    /// Sign-out: the server forgets this device's tokens with it; the activities go.
    func reset() {
        defaults.removeObject(forKey: Self.startTokenKey)
        sentUpdates.removeAll()
        updateTokens.removeAll()
        shown.removeAll()
        Task {
            for activity in Activity<ApprovalActivityAttributes>.activities {
                await activity.end(nil, dismissalPolicy: .immediate)
            }
        }
    }

    // MARK: - Tokens

    private func watch(_ activity: Activity<ApprovalActivityAttributes>) {
        guard watched[activity.id] == nil, activity.activityState == .active || activity.activityState == .stale else { return }
        let approvalId = activity.attributes.approvalId
        // Never two activities for one request (a push-to-start racing a local start): the later one ends at once.
        if watched.values.contains(approvalId) {
            Task { await activity.end(nil, dismissalPolicy: .immediate) }
            return
        }
        watched[activity.id] = approvalId
        shown.insert(approvalId)
        Task {
            for await data in activity.pushTokenUpdates {
                self.updateTokens[approvalId] = APNsAdapter.hex(data)
                self.flush()
            }
        }
    }

    /// Sends the tokens the server does not have yet (once signed in).
    private func flush() {
        guard let client, client.accessKey?.isEmpty == false else { return }
        let api = NotificationsClient(client: client)
        if let token = startToken, defaults.string(forKey: Self.startTokenKey) != token, sending.insert(token).inserted {
            Task {
                defer { self.sending.remove(token) }
                do {
                    try await api.registerLiveActivity(kind: "start", token: token, approvalId: nil)
                    self.defaults.set(token, forKey: Self.startTokenKey)
                } catch {
                    self.log.info("live activity start token not registered: \(APIError.wrap(error).status ?? 0, privacy: .public)")
                }
            }
        }
        for (approvalId, token) in updateTokens where !sentUpdates.contains(token) && sending.insert(token).inserted {
            Task {
                defer { self.sending.remove(token) }
                do {
                    try await api.registerLiveActivity(kind: "update", token: token, approvalId: approvalId)
                    self.sentUpdates.insert(token)
                } catch {
                    self.log.info("live activity update token not registered: \(APIError.wrap(error).status ?? 0, privacy: .public)")
                }
            }
        }
    }

    // MARK: - The pending list

    /// Every pending list the store learns: activities whose request is gone end; in the foreground, a pending
    /// request without one gets one.
    func sync(pending items: [ApprovalItem], foreground: Bool) {
        flush()
        let live = Activity<ApprovalActivityAttributes>.activities.filter { $0.activityState == .active || $0.activityState == .stale }
        let showing = live.filter { $0.content.state.isPending }.map(\.attributes.approvalId)
        for id in ApprovalActivityAttributes.toEnd(activityIds: showing, pending: Set(items.map(\.id))) {
            Task { await endVanished(id) }
        }
        guard foreground, ActivityAuthorizationInfo().areActivitiesEnabled else { return }
        let covered = shown.union(live.map(\.attributes.approvalId))
        let now = Date()
        for item in items where !covered.contains(item.id) && !item.isExpired(at: now) && (item.status ?? .pending) == .pending {
            startLocal(item)
        }
    }

    /// A request left the list: end its activity with what the server says (still pending — e.g. not yet this
    /// device's turn, or offline — keeps it, unless its time is up).
    private func endVanished(_ id: String) async {
        guard ending.insert(id).inserted else { return }
        defer { ending.remove(id) }
        let expired = activities(for: id).contains { Date() >= $0.attributes.expiresAt }
        let result: String
        switch await status?(id) {
        case .approved?: result = "approved"
        case .denied?: result = "denied"
        case .expired?: result = "expired"
        case .pending?, nil:
            guard expired else { return }
            result = "expired"
        }
        await end(id, status: result, dismissal: .immediate)
    }

    private func startLocal(_ item: ApprovalItem) {
        shown.insert(item.id)
        let (commands, count) = ApprovalActivityAttributes.commandLines(item.details)
        let expires = item.expiresAt ?? Date().addingTimeInterval(300)
        let attributes = ApprovalActivityAttributes(
            approvalId: item.id, requester: item.requester.displayName,
            summary: item.summary.isEmpty ? item.action : item.summary,
            commands: commands, commandCount: count, sensitive: item.sensitive, payloadHash: item.payloadHash,
            expiresAtUnix: expires.timeIntervalSince1970)
        do {
            let activity = try Activity.request(attributes: attributes,
                                                content: ActivityContent(state: .pending, staleDate: expires, relevanceScore: 100),
                                                pushType: .token)
            watch(activity)
        } catch {
            log.info("live activity not started: \(error.localizedDescription, privacy: .public)")
        }
    }

    // MARK: - Ending

    /// This iPhone answered `id`, or learned how it ended. Its own answer stays on the Lock Screen for a moment.
    func settled(_ id: String, _ outcome: ApprovalOutcome) {
        let status: String
        var own = false
        switch outcome {
        case .approved: status = "approved"; own = true
        case .denied: status = "denied"; own = true
        case .alreadyDecided(.approved?, _): status = "approved"
        case .alreadyDecided(.denied?, _): status = "denied"
        case .alreadyDecided, .expired, .gone: status = "expired"
        case .needsSetup, .pinRequired, .pinWrong, .pinLocked, .failed: return
        }
        let dismissal: ActivityUIDismissalPolicy = own ? .after(Date().addingTimeInterval(Self.ownAnswerLinger)) : .immediate
        Task { await end(id, status: status, dismissal: dismissal) }
    }

    private func activities(for id: String) -> [Activity<ApprovalActivityAttributes>] {
        Activity<ApprovalActivityAttributes>.activities.filter {
            $0.attributes.approvalId == id && ($0.activityState == .active || $0.activityState == .stale)
        }
    }

    private func end(_ id: String, status: String, dismissal: ActivityUIDismissalPolicy) async {
        for activity in activities(for: id) {
            await activity.end(ActivityContent(state: .init(status: status), staleDate: nil), dismissalPolicy: dismissal)
        }
        updateTokens[id] = nil
    }
}
