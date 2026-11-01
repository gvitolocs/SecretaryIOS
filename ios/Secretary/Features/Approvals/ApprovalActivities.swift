import ActivityKit
import AppIntents
import Foundation
import UIKit
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
    fileprivate let log = Logger(subsystem: "tech.getsecretary.app", category: "ApprovalActivity")
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

    /// Starts a local activity; nil: ActivityKit (tests record the attributes instead).
    private let requestActivity: ((ApprovalActivityAttributes, Date) -> Void)?
    /// The activities on this iPhone (tests: none).
    private let liveActivities: () -> [Activity<ApprovalActivityAttributes>]

    /// Whether this iPhone allows Live Activities (tests: yes).
    private let activitiesEnabled: () -> Bool
    private let now: () -> Date
    /// The notifier's previews setting: off, a local activity says only "Open Secretary to see what it is."
    var previews: () -> Bool
    /// The approval notifications: a request's own notification goes once its card is up (one alert per request).
    var notifier: ApprovalNotifying?
    /// Approvals whose card is up on this iPhone (activities this app started or watched while active).
    private var cardsUp: Set<String> = []

    init(defaults: UserDefaults = .standard, requestActivity: ((ApprovalActivityAttributes, Date) -> Void)? = nil,
         liveActivities: @escaping () -> [Activity<ApprovalActivityAttributes>] = { Activity<ApprovalActivityAttributes>.activities },
         activitiesEnabled: @escaping () -> Bool = { ActivityAuthorizationInfo().areActivitiesEnabled },
         now: @escaping () -> Date = Date.init, previews: @escaping () -> Bool = { true }) {
        self.defaults = defaults
        self.requestActivity = requestActivity
        self.liveActivities = liveActivities
        self.activitiesEnabled = activitiesEnabled
        self.now = now
        self.previews = previews
    }

    /// Installs the buttons' handlers. Called from `SecretaryApp.init`, before any scene: a tap may be all this launch
    /// is for (iOS runs LiveActivityIntents in the app's process). An older card's Approve link goes through
    /// `AppModel.openWidgetRoute` (`ApprovalLink`).
    static func install() {
        ApprovalActivityCommands.deny = { id, sensitive in
            await AppModel.shared.approvals.handleNotification(actionIdentifier: ApprovalNotifier.denyAction,
                                                               approvalId: id, sensitive: sensitive)
        }
        // Approve: in the background with the unlock key when one-tap is ready; else the card, in front, Face ID there.
        ApprovalActivityCommands.approve = { id, payloadHash, trigger, intent in
            let outcome = await AppModel.shared.approvals.approveFromActivity(id: id, shownHash: payloadHash, trigger: trigger)
            guard case let .openCard(startApprove) = outcome else { return }
            await continueOnCard(id, trigger: startApprove ? trigger : nil, intent: intent)
        }
    }

    /// How the card opens after the activity's Approve.
    enum CardOpening: Equatable {
        /// With its approve step started (Face ID or the PIN over the commands).
        case withApproveStep
        /// Only shown: the owner reads it and taps Approve there.
        case withoutApproveStep
    }

    /// The approve step starts only when it was asked for and the app is in front: already (`appActive`), or because
    /// iOS brought it there (`continued`). Declined or impossible: the card only.
    nonisolated static func cardOpening(startRequested: Bool, appActive: Bool, continued: Bool) -> CardOpening {
        startRequested && (appActive || continued) ? .withApproveStep : .withoutApproveStep
    }

    /// Where the card ends up after the activity's Approve.
    enum CardHandOff: Equatable {
        /// The app is in front: the card shows (with its approve step when asked).
        case openInFront
        /// The app stayed in the background: the card waits, and "Open Secretary to approve" tells the owner.
        case openAndNotify
    }

    /// In front already, or brought there (`canContinue` and `continueSucceeded`); otherwise the follow-up
    /// notification, so a tap is never left with nothing to see.
    nonisolated static func handOff(appActive: Bool, canContinue: Bool, continueSucceeded: Bool) -> CardHandOff {
        appActive || (canContinue && continueSucceeded) ? .openInFront : .openAndNotify
    }

    /// Brings the app to the front on `id`'s card, with its approve step started when `trigger` is set (Face ID over
    /// the exact commands). iOS 26+: `continueInForeground` (the intent's `.foreground(.dynamic)` mode); earlier:
    /// `requestToContinueInForeground`. When the app can't come to the front, the card is there the next time it
    /// opens, without its approve step, and "Open Secretary to approve" says so.
    private static func continueOnCard(_ id: String, trigger: ApprovalTrigger?, intent: any AppIntent) async {
        let store = AppModel.shared.approvals
        let log = shared.log
        let startRequested = trigger != nil
        func open(_ opening: CardOpening) async {
            log.info("tap outcome id=\(id, privacy: .public): opened card approveStep=\(opening == .withApproveStep, privacy: .public)")
            await store.open(id: id, startApprove: opening == .withApproveStep ? trigger : nil, now: Date())
        }
        func finish(appActive: Bool, canContinue: Bool, continued: Bool) async {
            switch handOff(appActive: appActive, canContinue: canContinue, continueSucceeded: continued) {
            case .openInFront:
                await open(cardOpening(startRequested: startRequested, appActive: appActive, continued: continued))
            case .openAndNotify:
                await open(.withoutApproveStep)
                log.info("card left in the background id=\(id, privacy: .public): open-to-approve posted")
                await store.postOpenToApprove(id: id)
            }
        }
        if UIApplication.shared.applicationState == .active {
            await finish(appActive: true, canContinue: false, continued: false)
            return
        }
        if #available(iOS 26.0, *) {
            log.info("continue in foreground requested id=\(id, privacy: .public)")
            do {
                try await intent.continueInForeground(nil, alwaysConfirm: false)
                // Coming to the front (active, or still becoming active: the approve step waits for it).
                let inFront = UIApplication.shared.applicationState != .background
                log.info("continue in foreground succeeded id=\(id, privacy: .public) inFront=\(inFront, privacy: .public)")
                await finish(appActive: false, canContinue: true, continued: inFront)
            } catch {
                let e = error as NSError
                log.error("continue in foreground failed id=\(id, privacy: .public): \(e.domain, privacy: .public) \(e.code, privacy: .public)")
                await finish(appActive: false, canContinue: true, continued: false)
            }
            return
        }
        if let foreground = intent as? any ForegroundContinuableIntent {
            log.info("continue in foreground requested id=\(id, privacy: .public) (iOS < 26)")
            do {
                try await foreground.requestToContinueInForeground {
                    // Now in front: the window starts here.
                    log.info("continue in foreground succeeded id=\(id, privacy: .public)")
                    await finish(appActive: false, canContinue: true, continued: true)
                }
                return
            } catch {
                let e = error as NSError
                log.error("continue in foreground failed id=\(id, privacy: .public): \(e.domain, privacy: .public) \(e.code, privacy: .public)")
                await finish(appActive: false, canContinue: true, continued: false)
                return
            }
        }
        log.error("continue in foreground unavailable id=\(id, privacy: .public)")
        await finish(appActive: false, canContinue: false, continued: false)
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
        cardsUp.removeAll()
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
        if activity.activityState == .active { cardUp(approvalId) }
        Task {
            // Ended, dismissed or stale: the card no longer carries the request.
            for await state in activity.activityStateUpdates where state != .active {
                self.cardsUp.remove(approvalId)
            }
        }
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

    /// Every pending list the store learns: activities whose request is gone end; a request whose card is up loses its
    /// own notification (the server's push may land after the card); in the foreground, a pending request without a
    /// card gets one.
    func sync(pending items: [ApprovalItem], foreground: Bool) {
        flush()
        let live = liveActivities().filter { $0.activityState == .active || $0.activityState == .stale }
        let withCard = items.map(\.id).filter { hasCard($0) }
        if !withCard.isEmpty { Task { await self.replaceNotifications(withCard) } }
        let showing = live.filter { $0.content.state.isPending }.map(\.attributes.approvalId)
        for id in ApprovalActivityAttributes.toEnd(activityIds: showing, pending: Set(items.map(\.id))) {
            Task { await endVanished(id) }
        }
        guard foreground, activitiesEnabled() else { return }
        let covered = shown.union(live.map(\.attributes.approvalId))
        let date = now()
        for item in items where !covered.contains(item.id) && !item.isExpired(at: date) && (item.status ?? .pending) == .pending {
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

    /// What a locally started activity shows: with previews off (the notifier's setting) only the generic line and no
    /// commands. (A push-started one is redacted while the iPhone is locked instead: the server can't know the setting.)
    nonisolated static func localAttributes(for item: ApprovalItem, previews: Bool, now: Date) -> ApprovalActivityAttributes {
        let (commands, count) = previews ? ApprovalActivityAttributes.commandLines(item.details) : ("", 0)
        let summary = previews ? (item.summary.isEmpty ? item.action : item.summary)
                               : String(localized: "Open Secretary to see what it is.")
        let expires = item.expiresAt ?? now.addingTimeInterval(300)
        return ApprovalActivityAttributes(
            approvalId: item.id, requester: item.requester.displayName, summary: summary,
            commands: commands, commandCount: count, sensitive: item.sensitive, payloadHash: item.payloadHash,
            expiresAtUnix: expires.timeIntervalSince1970)
    }

    private func startLocal(_ item: ApprovalItem) {
        shown.insert(item.id)
        let attributes = Self.localAttributes(for: item, previews: previews(), now: now())
        let expires = attributes.expiresAt
        if let requestActivity {
            requestActivity(attributes, expires)
            cardUp(item.id)
            return
        }
        do {
            let activity = try Activity.request(attributes: attributes,
                                                content: ActivityContent(state: .pending, staleDate: expires, relevanceScore: 100),
                                                pushType: .token)
            watch(activity)
        } catch {
            log.info("live activity not started: \(error.localizedDescription, privacy: .public)")
        }
    }

    // MARK: - One alert per request

    /// The card for `approvalId` is up on this iPhone.
    func hasCard(_ approvalId: String) -> Bool {
        cardsUp.contains(approvalId)
            || liveActivities().contains { $0.attributes.approvalId == approvalId && $0.activityState == .active }
    }

    private func cardUp(_ approvalId: String) {
        cardsUp.insert(approvalId)
        Task { await replaceNotifications([approvalId]) }
    }

    /// Takes the requests' own notifications off the lock screen (their cards carry them); the follow-ups stay.
    func replaceNotifications(_ ids: [String]) async {
        guard let notifier, await notifier.removeRequest(ids: ids) > 0 else { return }
        for id in ids { log.info("notification replaced by card id=\(id, privacy: .public)") }
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
        liveActivities().filter {
            $0.attributes.approvalId == id && ($0.activityState == .active || $0.activityState == .stale)
        }
    }

    private func end(_ id: String, status: String, dismissal: ActivityUIDismissalPolicy) async {
        for activity in activities(for: id) {
            await activity.end(ActivityContent(state: .init(status: status), staleDate: nil), dismissalPolicy: dismissal)
        }
        cardsUp.remove(id)
        updateTokens[id] = nil
    }
}

/// The Live Activity's Approve may have to bring the app to the front (the card). Declared here, in the app only:
/// app extensions cannot continue in the foreground.
extension ApproveApprovalActivityIntent: ForegroundContinuableIntent {}
