import Foundation
import Observation
import os

/// An answer to a request. An approve always names the owner's tap it comes from (`ApprovalTrigger`); a deny needs
/// none.
enum ApprovalAnswer: Equatable, Sendable {
    case deny
    case approve(ApprovalTrigger)

    var decision: ApprovalDecision {
        switch self {
        case .deny: return .deny
        case .approve: return .approve
        }
    }
}

/// The Live Activity's Approve link (or the notification's Approve) asked the card to start its approve step: for this request only, and only for
/// `window` seconds after the tap.
struct StartApprove: Equatable, Sendable {
    static let window: TimeInterval = 10

    let id: String
    let trigger: ApprovalTrigger
    let at: Date

    var deadline: Date { at.addingTimeInterval(Self.window) }

    func isValid(for id: String, at now: Date) -> Bool {
        self.id == id && now >= at && now < deadline
    }
}

/// The owner's approvals on this iPhone: polls the pending requests, shows the global prompt, posts a local
/// notification once per request, and answers (a sensitive "yes" is signed with the Face ID key).
///
/// While the app is active it long-polls: the server answers as soon as the list changes (a new request, or one
/// decided, withdrawn or expired anywhere) or after `longPollWait`; a server without long polls is asked every 3 s.
/// A push, the widget or a background refresh asks once at once. A server without the approvals routes (404) is
/// left alone: no banner, no error, one request every 30 s.
@Observable
@MainActor
final class ApprovalsStore {
    /// What the last poll found out about the server.
    enum Availability: Equatable {
        case unknown, available
        /// 404/405: this server has no approvals yet.
        case unsupported
        /// Offline or a server error: try again soon, say nothing.
        case failing
    }

    enum KeyStatus: Equatable {
        case unknown
        case notSetUp
        case ready
        /// This iPhone has a key the server does not know (registration failed, or the server forgot it).
        case notRegistered
        /// The server has a key for this device that this iPhone lost (reinstall, new face, restore).
        case lostLocally
        /// No Secure Enclave.
        case unavailable
    }

    static let pollInterval: Duration = .seconds(3)
    /// Seconds the server may hold a long poll (it allows 30).
    static let longPollWait = 25
    /// Between two long polls: keeps a server that answers at once from turning into a busy loop.
    static let longPollPause: Duration = .seconds(1)
    static let backoffInterval: Duration = .seconds(30)
    static let errorInterval: Duration = .seconds(10)
    static let notifiedKey = "secretary.approvals.notified"
    /// Settings → Approvals → "One-tap Approve from notifications" (absent: on).
    nonisolated static let oneTapKey = "secretary.approvals.oneTap"
    static let notifiedLimit = 100

    private(set) var pending: [ApprovalItem] = []
    private(set) var availability: Availability = .unknown
    /// The request on the prompt (also after it was answered, until the owner closes it).
    private(set) var sheetItem: ApprovalItem?
    /// How the prompt ended; nil while the owner decides.
    private(set) var outcome: ApprovalOutcome?
    /// The answer being sent (Face ID, then the request).
    private(set) var working: ApprovalDecision?
    /// The PIN pad is showing instead of the Approve / Deny buttons.
    private(set) var pinEntry = false
    /// The card opened from the Live Activity's Approve link or the notification's Approve: it starts the approve step as it appears, within
    /// `StartApprove.window`. Cleared when the sheet closes, when the request leaves the list, when the app goes to the
    /// background and on sign-out. Only `open(id:startApprove:)` sets it.
    private(set) var startApprove: StartApprove?

    // Settings → Approvals
    private(set) var settings: ApprovalSettings?
    private(set) var settingsLoaded = false
    private(set) var settingsBusy = false
    var settingsMessage: String?
    private(set) var keyStatus: KeyStatus = .unknown
    /// "One-tap Approve from notifications" (default on).
    private(set) var oneTapOn = true
    /// The server's id of this iPhone's unlock key, when it has one.
    private(set) var unlockKeyId: String? = nil
    /// One-tap Approve works now: the switch is on and the unlock key enrolled.
    var oneTapReady: Bool { oneTapOn && unlockKeyId != nil }

    @ObservationIgnored private let api: () -> ApprovalsAPI?
    @ObservationIgnored let signer: ApprovalSigning
    /// The one-tap Approve's key: the only signer the in-place approve uses.
    @ObservationIgnored let unlockSigner: UnlockApprovalSigning
    /// `oneTapReady` changed (the app registers the notification categories again).
    @ObservationIgnored var onOneTapChanged: ((Bool) -> Void)?
    /// Why the last enrollment didn't happen, for Settings (nil: it did, or the owner cancelled Face ID).
    private(set) var unlockEnrollFailure: String?
    /// The enrollment on its way, in the store's own task: a view's task that ends (the Face ID sheet makes the scene
    /// inactive) must not cancel it. One at a time.
    @ObservationIgnored private var enrollTask: Task<UnlockEnrollResult, Never>?
    /// The owner cancelled the automatic enrollment's Face ID: no more automatic prompts in this launch.
    @ObservationIgnored private var unlockEnrollDeclined = false
    /// `backgroundGeneration` of the automatic enrollment that failed: it is tried again only after a return from the
    /// background (not on the scene's own return from the Face ID sheet, which would prompt again and again).
    @ObservationIgnored private var unlockEnrollFailedGeneration: Int?
    /// The scene is active (the app tells): a register cancelled under the Face ID sheet waits for it to retry.
    @ObservationIgnored private(set) var sceneActive = true
    /// Longest wait for the scene to be active again before a cancelled register is retried.
    @ObservationIgnored var unlockRetryWait: Duration = .seconds(1)
    /// One-tap approves on their way, by request (iOS may run the action twice).
    @ObservationIgnored private var approvingInPlace: Set<String> = []
    @ObservationIgnored let notifier: ApprovalNotifying
    @ObservationIgnored private let previews: () -> Bool
    @ObservationIgnored private let deviceLabel: () -> String
    @ObservationIgnored private let defaults: UserDefaults
    @ObservationIgnored private let log = Logger(subsystem: "tech.getsecretary.app", category: "Approvals")
    /// Bumped each time the app goes to the background: a card's approve step that waited across it stops.
    @ObservationIgnored private(set) var backgroundGeneration = 0
    @ObservationIgnored var onTransportError: ((APIError) -> Void)?
    /// The pending list changed (the app hands it to the approvals widget).
    @ObservationIgnored var onPendingChanged: (([ApprovalItem]) -> Void)?
    /// Every pending list the store learns, changed or not (the app ends stale Live Activities, starts missing ones).
    @ObservationIgnored var onPendingLearned: (([ApprovalItem]) -> Void)?
    /// This iPhone answered a request, or learned how it ended (the app ends its Live Activity).
    @ObservationIgnored var onSettled: ((_ id: String, _ outcome: ApprovalOutcome) -> Void)?
    /// Requests the owner closed without answering; they come back when the app becomes active again.
    @ObservationIgnored private var dismissed: Set<String> = []
    @ObservationIgnored private var polling = false
    /// The last list's cursor (nil: none yet, or the server does not long-poll).
    @ObservationIgnored private var cursor: String?
    /// Fixture data for the screenshots: nothing is polled.
    @ObservationIgnored private(set) var isFixture = false

    init(api: @escaping () -> ApprovalsAPI?, signer: ApprovalSigning, unlockSigner: UnlockApprovalSigning = NoUnlockApprovalSigner(),
         notifier: ApprovalNotifying, previews: @escaping () -> Bool = { true }, deviceLabel: @escaping () -> String = { "iPhone" },
         defaults: UserDefaults = .standard) {
        self.api = api
        self.signer = signer
        self.unlockSigner = unlockSigner
        oneTapOn = defaults.object(forKey: Self.oneTapKey) as? Bool ?? true
        unlockKeyId = unlockSigner.keyId
        self.notifier = notifier
        self.previews = previews
        self.deviceLabel = deviceLabel
        self.defaults = defaults
    }

    // MARK: - Polling

    /// While the app is active, until cancelled: a long poll the server answers when the list changes (or after
    /// `longPollWait`), then the next one. A server without long polls is asked every 3 s.
    func foregroundLoop() async {
        dismissed.removeAll()
        cursor = nil
        while !Task.isCancelled {
            await poll(wait: cursor == nil ? 0 : Self.longPollWait)
            try? await Task.sleep(for: interval)
        }
    }

    var interval: Duration {
        switch availability {
        case .unsupported: return Self.backoffInterval
        case .failing: return Self.errorInterval
        case .unknown, .available: return cursor == nil ? Self.pollInterval : Self.longPollPause
        }
    }

    /// One look at the pending requests (also the background refresh's). Never throws, never shows an error.
    func poll() async {
        guard !polling else { return }
        polling = true
        defer { polling = false }
        await poll(wait: 0)
    }

    /// `wait` > 0: a long poll from the last `cursor`. It never blocks `poll()` (a push or the widget asks at once).
    private func poll(wait: Int) async {
        guard !isFixture, let api = api() else { return }
        do {
            let list = try await api.pending(since: cursor, wait: wait)
            cursor = list.cursor
            availability = .available
            await apply(list.items)
        } catch {
            let e = APIError.wrap(error)
            if e.status == 404 || e.status == 405 {
                availability = .unsupported
                await apply([])
            } else {
                availability = .failing
                if e.endsSession { onTransportError?(e) }
            }
        }
    }

    /// Show `items`; notify the new ones once each; take answered ones off the lock screen.
    func apply(_ items: [ApprovalItem]) async {
        let before = Set(pending.map(\.id))
        let now = Set(items.map(\.id))
        if items != pending { onPendingChanged?(items) }
        pending = items
        // A start-approve for a request that is no longer waiting must never fire.
        if let start = startApprove, !now.contains(start.id) { startApprove = nil }
        onPendingLearned?(items)
        let gone = before.subtracting(now)
        if !gone.isEmpty { await notifier.remove(ids: Array(gone)) }

        var notified = notifiedIds
        let fresh = items.filter { !notified.contains($0.id) }
        for item in fresh {
            notified.append(item.id)
            await notifier.post(item, previews: previews())
        }
        if !fresh.isEmpty {
            // Keep the pending ids and the most recent others.
            notifiedIds = notified.filter { now.contains($0) } + notified.filter { !now.contains($0) }.suffix(Self.notifiedLimit)
        }
        await refreshSheet()
    }

    private var notifiedIds: [String] {
        get { defaults.stringArray(forKey: Self.notifiedKey) ?? [] }
        set { defaults.set(newValue, forKey: Self.notifiedKey) }
    }

    // MARK: - The prompt

    /// Picks what the prompt shows: the request being answered stays; otherwise the newest pending one the owner
    /// has not closed.
    private func refreshSheet() async {
        if let current = sheetItem {
            if outcome != nil || working != nil { return }
            if let fresh = pending.first(where: { $0.id == current.id }) {
                sheetItem = fresh
                return
            }
            // Answered elsewhere, or expired: find out which.
            await settleVanished(current)
            return
        }
        if let next = pending.first(where: { !dismissed.contains($0.id) }) {
            sheetItem = next
            outcome = nil
        }
    }

    private func settleVanished(_ item: ApprovalItem) async {
        guard let api = api() else { return }
        do {
            let latest = try await api.get(item.id)
            sheetItem = latest
            switch latest.status {
            case .approved: outcome = .alreadyDecided(.approved, by: latest.decidedBy)
            case .denied: outcome = .alreadyDecided(.denied, by: latest.decidedBy)
            case .expired: outcome = .expired
            case .pending, nil:
                // Still pending, but not listed (it moved to another device's turn): keep it.
                break
            }
        } catch {
            // Only an answer from the server ends the prompt; a dropped connection leaves it as it is.
            let result = ApprovalOutcome.from(error)
            if result.isFinal { outcome = result }
        }
    }

    /// The prompt's X: not answered, so it returns the next time the app becomes active.
    func dismissSheet() async {
        pinEntry = false
        startApprove = nil
        if let item = sheetItem, outcome?.isFinal != true { dismissed.insert(item.id) }
        sheetItem = nil
        outcome = nil
        working = nil
        await refreshSheet()
    }

    /// Opens the prompt on one request (a tapped notification), fetching it when it is not listed yet. `startApprove`
    /// (the Live Activity's Approve link, the notification's Approve; the app in front): the card starts its approve step as it appears (Face ID
    /// over the card), within `StartApprove.window` of `now`.
    func open(id: String, startApprove trigger: ApprovalTrigger? = nil, now: Date = Date()) async {
        dismissed.remove(id)
        pinEntry = false
        working = nil
        outcome = nil
        // Set together with the card it is for (never while another card shows), and only for a pending request.
        startApprove = nil
        let start = trigger.map { StartApprove(id: id, trigger: $0, at: now) }
        if let item = pending.first(where: { $0.id == id }) {
            sheetItem = item
            startApprove = start
            return
        }
        guard let api = api() else { return }
        do {
            let item = try await api.get(id)
            sheetItem = item
            switch item.status {
            case .approved: outcome = .alreadyDecided(.approved, by: item.decidedBy)
            case .denied: outcome = .alreadyDecided(.denied, by: item.decidedBy)
            case .expired: outcome = .expired
            case .pending, nil: startApprove = start
            }
            if let outcome, outcome.isFinal { onSettled?(id, outcome) }
        } catch {
            sheetItem = ApprovalItem.placeholder(id: id)
            outcome = ApprovalOutcome.from(error)
        }
    }

    /// The start-approve for the card of `id`, once, while it is valid. Any other card, or a late one, clears it.
    func takeStartApprove(_ id: String, now: Date = Date()) -> StartApprove? {
        guard let start = startApprove else { return nil }
        startApprove = nil
        return start.isValid(for: id, at: now) ? start : nil
    }

    /// The app went to the background: nothing started from a tap carries on.
    func sceneBackgrounded() {
        startApprove = nil
        backgroundGeneration += 1
    }

    /// The scene became active or stopped being active (the Face ID sheet, the app switcher, the background).
    func sceneActiveChanged(_ active: Bool) {
        sceneActive = active
    }

    /// How a request stands on the server now (nil: unknown, offline).
    func status(of id: String) async -> ApprovalStatus? {
        guard let api = api() else { return nil }
        return try? await api.get(id).status
    }

    // MARK: - Answering

    /// The prompt's Approve / Deny. `pin` is the owner's 6 digits from the PIN pad (a sensitive approve only).
    /// `shownHash`: the payload hash of the request on screen when the owner tapped; a request that changed since is
    /// not answered (the card shows the new one).
    func decide(_ choice: ApprovalAnswer, pin: String? = nil, shownHash: String? = nil) async {
        guard let item = sheetItem, item.canAnswer, working == nil, outcome?.isFinal != true else { return }
        if let shownHash, shownHash != item.payloadHash {
            log.error("answer refused id=\(item.id, privacy: .public): changed since shown")
            outcome = .failed(String(localized: "This request changed. Read it again before you answer."))
            return
        }
        working = choice.decision
        outcome = nil
        let result = await answer(item, choice, pin: pin)
        working = nil
        guard let result else { return } // Face ID cancelled: the owner is back at the prompt
        switch result {
        case .pinRequired:
            pinEntry = true
            return
        case .pinWrong, .pinLocked:
            pinEntry = true
        default:
            break
        }
        outcome = result
        await settle(item, result)
    }

    /// This iPhone knows the owner set a PIN (from Settings → Approvals, loaded when a sensitive prompt opens).
    var hasPin: Bool { settings?.hasPin ?? false }

    /// Sends the answer. A sensitive approve carries the owner's `pin`, or is signed first: Face ID (or the passcode)
    /// asked now, on the card in front (the only caller that approves a sensitive request); a deny never needs either.
    /// Nil: Face ID was cancelled. `.pinRequired`: the PIN pad takes over.
    func answer(_ item: ApprovalItem, _ choice: ApprovalAnswer, pin: String? = nil) async -> ApprovalOutcome? {
        let decision = choice.decision
        if case let .approve(trigger) = choice {
            log.info("approve id=\(item.id, privacy: .public) trigger=\(trigger.rawValue, privacy: .public)")
        }
        guard let api = api() else { return .failed(APIError.network(message: "").userMessage) }
        var proof: ApprovalProof?
        var sentPin: String?
        if decision == .approve, item.sensitive {
            if let pin {
                sentPin = pin
            } else if signer.keyId == nil {
                return hasPin ? .pinRequired : .needsSetup
            } else {
                do {
                    proof = try await signer.sign(item.signingString(for: .approve), reason: item.summary.isEmpty ? item.action : item.summary)
                    log.info("sign ok id=\(item.id, privacy: .public)")
                } catch ApprovalSignError.cancelled {
                    log.info("sign cancelled id=\(item.id, privacy: .public)")
                    return nil
                } catch ApprovalSignError.noKey {
                    log.info("sign no key id=\(item.id, privacy: .public)")
                    return hasPin ? .pinRequired : .needsSetup
                } catch let error as ApprovalSignError {
                    log.error("sign failed id=\(item.id, privacy: .public): \(String(describing: error), privacy: .public)")
                    // Face ID is locked out or not set up, or there is no Secure Enclave: the PIN takes over.
                    if hasPin, error == .needsDeviceSecurity || error == .unavailable { return .pinRequired }
                    return .failed(error.userMessage)
                } catch {
                    let e = error as NSError
                    log.error("sign failed id=\(item.id, privacy: .public): \(e.domain, privacy: .public) \(e.code, privacy: .public)")
                    return .failed(error.localizedDescription)
                }
            }
        }
        do {
            try await api.decide(item.id, decision: decision, payloadHash: item.payloadHash, proof: proof, pin: sentPin)
            return decision == .approve ? .approved : .denied
        } catch {
            let e = APIError.wrap(error)
            if e.endsSession { onTransportError?(e) }
            let result = ApprovalOutcome.from(error)
            return result == .needsSetup && hasPin ? .pinRequired : result
        }
    }

    /// The PIN pad's "Use PIN instead" / its back button.
    func showPinPad(_ on: Bool) {
        pinEntry = on
        if on, case .failed? = outcome { outcome = nil }
    }

    /// After an answer the server accepted or refused: tidy the list and the notification.
    private func settle(_ item: ApprovalItem, _ result: ApprovalOutcome) async {
        if result.isFinal {
            pending.removeAll { $0.id == item.id }
            onPendingChanged?(pending)
            await notifier.remove(ids: [item.id])
        }
        defer {
            // After the lookup below: "Already approved on iPad" ends the Live Activity as approved.
            let ended = sheetItem?.id == item.id ? (outcome ?? result) : result
            if ended.isFinal { onSettled?(item.id, ended) }
        }
        guard let api = api() else { return }
        switch result {
        case .alreadyDecided(nil, _):
            // Learn who answered, for the line "Already approved on iPad".
            if let latest = try? await api.get(item.id), sheetItem?.id == item.id {
                switch latest.status {
                case .approved: outcome = .alreadyDecided(.approved, by: latest.decidedBy)
                case .denied: outcome = .alreadyDecided(.denied, by: latest.decidedBy)
                case .expired: outcome = .expired
                case .pending, nil: break
                }
            }
        case .failed:
            // The request may have changed (payload mismatch): show the current one.
            if let latest = try? await api.get(item.id), latest.status == .pending || latest.status == nil, sheetItem?.id == item.id {
                sheetItem = latest
                if let i = pending.firstIndex(where: { $0.id == latest.id }) { pending[i] = latest }
            }
        default:
            break
        }
    }

    /// A notification button or tap (AppDelegate). Deny, and a plain Approve, answer without opening the app. A
    /// sensitive Approve opens the app on the card: with its approve step started (Face ID at once, over the commands)
    /// when the notification showed the whole plan (`inline`: `APPROVAL_SENSITIVE_INLINE`) and still shows the request
    /// on the server (`shownHash`, not expired); else the card only. An approve needs `trigger`
    /// `.notificationApproveAction` (the owner tapped the notification's Approve); without it nothing is approved.
    func handleNotification(actionIdentifier: String, approvalId: String, sensitive: Bool, inline: Bool = false,
                            shownHash: String? = nil, trigger: ApprovalTrigger? = nil, now: Date = Date()) async {
        guard let action = ApprovalNotificationAction(identifier: actionIdentifier) else { return }
        switch action.route(sensitive: sensitive, inline: inline) {
        case .openSheet:
            await open(id: approvalId)
        case .openSheetAndApprove:
            await openCardToApprove(approvalId, shownHash: shownHash, trigger: trigger, now: now)
        case .approveInPlace:
            await approveInPlace(approvalId, shownHash: shownHash, trigger: trigger, now: now)
        case let .decide(decision):
            var known = pending.first { $0.id == approvalId }
            if known == nil, let api = api() { known = try? await api.get(approvalId) }
            guard let item = known else {
                log.info("\(decision.rawValue, privacy: .public) id=\(approvalId, privacy: .public): opened (request not found)")
                await open(id: approvalId)
                return
            }
            guard item.status == nil || item.status == .pending else {
                log.info("\(decision.rawValue, privacy: .public) id=\(approvalId, privacy: .public): forgotten (no longer pending)")
                await forgetAnswered(item)
                return
            }
            let choice: ApprovalAnswer
            switch decision {
            case .deny:
                choice = .deny
            case .approve:
                guard let trigger, trigger == .notificationApproveAction else {
                    log.error("approve refused id=\(item.id, privacy: .public): not from the notification's Approve")
                    return
                }
                // The request became sensitive since the notification was made: ask on the card (this action doesn't
                // bring the app to the front, so the follow-up leads there).
                if item.sensitive {
                    log.info("approve id=\(approvalId, privacy: .public): now sensitive, card and open-to-approve")
                    await open(id: approvalId)
                    await notifier.postOpenToApprove(for: item)
                    return
                }
                choice = .approve(trigger)
            }
            guard let result = await answer(item, choice) else {
                log.info("\(decision.rawValue, privacy: .public) id=\(approvalId, privacy: .public): cancelled")
                return
            }
            log.info("""
                \(decision.rawValue, privacy: .public) id=\(approvalId, privacy: .public): \
                \(result.isFinal ? "decided" : "failed", privacy: .public) \(String(describing: result), privacy: .public)
                """)
            if result.isFinal { log.info("tap outcome id=\(approvalId, privacy: .public): decided \(decision.rawValue, privacy: .public)") }
            await finishButtonAnswer(item, result)
        }
    }

    /// The notification's one-tap Approve of a root plan it showed whole (`APPROVAL_SENSITIVE_INLINE` with the unlock key;
    /// iOS asked Face ID or the passcode to run it, the app stays in the background). Fetches the request and signs it
    /// with the unlock key only (never the Face ID key: its prompt can't show from here), for the owner's own tap on
    /// the request the notification showed, while it is pending. A request answered already is left alone (no prompt,
    /// no card); one that changed, or one-tap not ready, gets "Open Secretary to approve"; a failure the failure
    /// notification. Nothing is ever dropped silently.
    private func approveInPlace(_ id: String, shownHash: String?, trigger: ApprovalTrigger?, now: Date) async {
        guard trigger == .notificationApproveAction else {
            log.error("one-tap approve refused id=\(id, privacy: .public): not from the notification's Approve")
            return
        }
        guard !approvingInPlace.contains(id) else { return }
        approvingInPlace.insert(id)
        defer { approvingInPlace.remove(id) }
        guard let api = api() else {
            await notifier.postFailure(for: pending.first { $0.id == id } ?? ApprovalItem(id: id, summary: "", sensitive: true))
            return
        }
        let fetched: ApprovalItem
        do {
            fetched = try await api.get(id)
        } catch {
            let e = APIError.wrap(error)
            if e.endsSession { onTransportError?(e) }
            log.error("one-tap approve id=\(id, privacy: .public): request not fetched (\(e.status, privacy: .public))")
            let result = ApprovalOutcome.from(error)
            let item = pending.first { $0.id == id } ?? ApprovalItem(id: id, summary: "", sensitive: true)
            if result.isFinal { await finishButtonAnswer(item, result) } else { await notifier.postFailure(for: item) }
            return
        }
        let item = fetched
        guard item.status == nil || item.status == .pending else {
            // Answered already (by this iPhone too: a repeated tap): nothing to ask, nothing to open.
            log.info("one-tap approve id=\(id, privacy: .public): no longer pending")
            await forgetAnswered(item)
            return
        }
        guard !item.isExpired(at: now) else {
            log.info("one-tap approve id=\(id, privacy: .public): expired")
            await forgetAnswered(item)
            return
        }
        guard let shownHash, shownHash == item.payloadHash else {
            log.error("one-tap approve refused id=\(id, privacy: .public): changed since shown")
            await notifier.postOpenToApprove(for: item)
            return
        }
        // Not ready (switched off, or the key went since the categories were registered), or an approver change
        // (the server takes Face ID for those): the card instead.
        guard oneTapReady, unlockSigner.keyId != nil, !item.action.hasPrefix("approver.") else {
            log.info("one-tap approve id=\(id, privacy: .public): not ready, open-to-approve")
            await notifier.postOpenToApprove(for: item)
            return
        }
        guard let result = await approveWithUnlockKey(item, api: api, trigger: .notificationApproveAction) else {
            await notifier.postFailure(for: item)
            return
        }
        log.info("one-tap approve id=\(id, privacy: .public): \(String(describing: result), privacy: .public)")
        if result == .approved { log.info("tap outcome id=\(id, privacy: .public): decided approve via unlock key") }
        await finishButtonAnswer(item, result)
    }

    /// Signs `item`'s approve with the unlock key only (never the Face ID key: no prompt can show from the
    /// background) and sends it. Nil: the key did not sign. A server that no longer takes the key (`.needsSetup`)
    /// gets it removed here: one-tap is off until it is enrolled again.
    private func approveWithUnlockKey(_ item: ApprovalItem, api: ApprovalsAPI, trigger: ApprovalTrigger) async -> ApprovalOutcome? {
        log.info("approve id=\(item.id, privacy: .public) trigger=\(trigger.rawValue, privacy: .public) via=unlock_key")
        let proof: ApprovalProof
        do {
            proof = try unlockSigner.sign(item.signingString(for: .approve))
        } catch {
            log.error("one-tap approve id=\(item.id, privacy: .public): unlock key did not sign")
            return nil
        }
        do {
            try await api.decide(item.id, decision: .approve, payloadHash: item.payloadHash, proof: proof)
            return .approved
        } catch {
            let e = APIError.wrap(error)
            if e.endsSession { onTransportError?(e) }
            let result = ApprovalOutcome.from(error)
            if result == .needsSetup {
                // The server no longer takes this unlock key (the Face ID key changed): back to the card's Approve.
                log.error("one-tap approve id=\(item.id, privacy: .public): unlock key refused, removed")
                forgetUnlockKey()
            }
            return result
        }
    }

    /// What the app does after the Live Activity's Approve.
    enum ActivityApproveOutcome: Equatable {
        /// Answered, refused, or nothing to do: the app stays where it is (the card ends with the answer).
        case done
        /// The card in the app, in front: with its approve step started (Face ID over the commands), or only shown
        /// (the request changed since the activity showed it, or could not be looked up).
        case openCard(startApprove: Bool)
    }

    /// The Live Activity's Approve (iOS ran it after Face ID or the passcode on this iPhone; the app may be in the
    /// background). Only for the activity's own tap, on a request that is pending now (fetched now), not expired and
    /// still the one the activity showed (`shownHash`), and once at a time per request: a repeated tap does nothing,
    /// and a request answered already (by this tap too) is left alone. With one-tap Approve ready it is signed with the
    /// unlock key and answered here; a plain request without it is answered unsigned (as the notification's Approve
    /// does). Otherwise, or when the unlock key's answer fails, the card with its approve step: nothing is signed with
    /// the Face ID key from the background.
    func approveFromActivity(id: String, shownHash: String, trigger: ApprovalTrigger,
                             now: Date = Date()) async -> ActivityApproveOutcome {
        guard trigger == .activityApproveButton else {
            log.error("tap outcome id=\(id, privacy: .public): refused: not from the activity's Approve")
            return .done
        }
        guard !approvingInPlace.contains(id), !(working != nil && sheetItem?.id == id) else {
            log.info("tap outcome id=\(id, privacy: .public): refused: already in flight")
            return .done
        }
        approvingInPlace.insert(id)
        defer { approvingInPlace.remove(id) }
        guard let api = api() else {
            log.info("tap outcome id=\(id, privacy: .public): opened card (signed out)")
            return .openCard(startApprove: false)
        }
        let item: ApprovalItem
        do {
            item = try await api.get(id)
        } catch {
            let e = APIError.wrap(error)
            if e.endsSession { onTransportError?(e) }
            let result = ApprovalOutcome.from(error)
            if result.isFinal {
                log.info("tap outcome id=\(id, privacy: .public): refused: \(String(describing: result), privacy: .public)")
                await finishButtonAnswer(pending.first { $0.id == id } ?? ApprovalItem(id: id, summary: "", sensitive: true), result)
                return .done
            }
            // Offline: the card shows what it can; nothing starts on a request the app can't see.
            log.info("tap outcome id=\(id, privacy: .public): opened card (request not fetched)")
            return .openCard(startApprove: false)
        }
        guard item.status == nil || item.status == .pending else {
            // Answered already (by this iPhone too: a repeated tap): nothing to ask, nothing to open.
            log.info("tap outcome id=\(id, privacy: .public): refused: no longer pending")
            await forgetAnswered(item)
            return .done
        }
        guard !item.isExpired(at: now) else {
            log.info("tap outcome id=\(id, privacy: .public): refused: expired")
            await forgetAnswered(item)
            return .done
        }
        guard item.payloadHash == shownHash else {
            // The owner reads the new request on the card and answers there.
            log.error("tap outcome id=\(id, privacy: .public): refused: changed since shown")
            return .openCard(startApprove: false)
        }
        // Approver changes keep Face ID (the server refuses the unlock key for them).
        if oneTapReady, unlockSigner.keyId != nil, !item.action.hasPrefix("approver.") {
            if let result = await approveWithUnlockKey(item, api: api, trigger: trigger), result.isFinal {
                if result == .approved {
                    log.info("tap outcome id=\(id, privacy: .public): decided approve via unlock key")
                } else {
                    log.info("tap outcome id=\(id, privacy: .public): refused: \(String(describing: result), privacy: .public)")
                }
                await finishButtonAnswer(item, result)
                return .done
            }
            log.error("tap outcome id=\(id, privacy: .public): opened card (unlock key approve failed)")
            return .openCard(startApprove: true)
        }
        if !item.sensitive {
            if let result = await answer(item, .approve(trigger)), result.isFinal {
                let said = result == .approved ? "decided approve" : "refused: \(String(describing: result))"
                log.info("tap outcome id=\(id, privacy: .public): \(said, privacy: .public)")
                await finishButtonAnswer(item, result)
                return .done
            }
            log.error("tap outcome id=\(id, privacy: .public): opened card (approve failed)")
            return .openCard(startApprove: true)
        }
        log.info("tap outcome id=\(id, privacy: .public): opened card (one-tap not ready)")
        return .openCard(startApprove: true)
    }

    /// "Open Secretary to approve" for `id` (the activity's Approve could not bring the card to the front).
    func postOpenToApprove(id: String) async {
        let item = pending.first { $0.id == id } ?? sheetItem.flatMap { $0.id == id ? $0 : nil }
            ?? ApprovalItem(id: id, summary: "", sensitive: true)
        await notifier.postOpenToApprove(for: item)
    }

    /// The Live Activity's Approve link (`approvals/<id>?approve=1&hash=…`; the app is coming to the front): the card,
    /// with its approve step (Face ID over the commands, on the card) only when the request is pending, not expired
    /// and still the one the activity showed (`shownHash`); otherwise the card only. Nothing is answered here: a link
    /// can come from anywhere, the owner's Face ID on the card is the approve.
    func openFromActivityLink(id: String, shownHash: String, now: Date = Date()) async {
        await openCardToApprove(id, shownHash: shownHash, trigger: .activityApproveLink, now: now)
    }

    /// The notification's Approve of a root plan it showed whole, or the activity's Approve link (the app is coming to
    /// the front): the card, with its approve step started only for the owner's own tap on the request he saw, while it
    /// is pending.
    private func openCardToApprove(_ id: String, shownHash: String?, trigger: ApprovalTrigger?, now: Date) async {
        guard let trigger, trigger == .notificationApproveAction || trigger == .activityApproveLink else {
            log.error("approve refused id=\(id, privacy: .public): not from the notification's Approve or the activity's link")
            await open(id: id)
            return
        }
        var known = pending.first { $0.id == id }
        if known == nil, let api = api() { known = try? await api.get(id) }
        let shown = known.map { ($0.status == nil || $0.status == .pending) && !$0.isExpired(at: now) && shownHash == $0.payloadHash }
        let step = Self.cardApproveStep(trigger: trigger, shownAndPending: shown ?? false)
        log.info("\(trigger == .activityApproveLink ? "activity link" : "notification", privacy: .public) approve id=\(id, privacy: .public) approveStep=\(step, privacy: .public)")
        await open(id: id, startApprove: step ? trigger : nil, now: now)
    }

    /// The card's approve step starts for the notification's own Approve, or the activity's Approve link, on the request
    /// it showed (same payload hash, pending, not expired); any doubt opens the card without it.
    nonisolated static func cardApproveStep(trigger: ApprovalTrigger?, shownAndPending: Bool) -> Bool {
        (trigger == .notificationApproveAction || trigger == .activityApproveLink) && shownAndPending
    }

    /// A button for a request that is no longer pending: take it off the lock screen, end its activity.
    private func forgetAnswered(_ item: ApprovalItem) async {
        await notifier.remove(ids: [item.id])
        switch item.status {
        case .approved?: onSettled?(item.id, .alreadyDecided(.approved, by: item.decidedBy))
        case .denied?: onSettled?(item.id, .alreadyDecided(.denied, by: item.decidedBy))
        default: onSettled?(item.id, .expired)
        }
    }

    /// A button's answer (notification, Live Activity): tidy up, or tell the owner it failed.
    private func finishButtonAnswer(_ item: ApprovalItem, _ result: ApprovalOutcome) async {
        if result.isFinal {
            pending.removeAll { $0.id == item.id }
            onPendingChanged?(pending)
            if sheetItem?.id == item.id { sheetItem = nil; outcome = nil }
            await notifier.remove(ids: [item.id])
            onSettled?(item.id, result)
        } else {
            await notifier.postFailure(for: item)
        }
    }

    /// A request the server pushed (APNs): it is on the lock screen already, so `apply` doesn't post it again.
    func markNotified(_ id: String) {
        var ids = notifiedIds
        guard !ids.contains(id) else { return }
        ids.append(id)
        notifiedIds = ids
    }

    // MARK: - Settings → Approvals

    func loadSettings(keepMessage: Bool = false) async {
        guard !isFixture, let api = api() else { return }
        do {
            let s = try await api.settings()
            settings = s
            if !keepMessage { settingsMessage = nil }
            availability = .available
            // The server dropped this iPhone's unlock key (its Face ID key was removed or replaced): it signs nothing
            // the server takes, so it goes here too, and the next launch in front enrolls a new one.
            if unlockKeyId != nil, s.thisDevice != nil, !s.serverHasUnlockKeyForThisDevice {
                log.info("unlock key no longer on the server: removed")
                forgetUnlockKey()
            }
        } catch {
            let e = APIError.wrap(error)
            if e.status == 404 || e.status == 405 {
                availability = .unsupported
                settings = nil
            } else {
                settingsMessage = e.userMessage
            }
        }
        settingsLoaded = true
        refreshKeyStatus()
    }

    func refreshKeyStatus() {
        guard let settings else {
            keyStatus = signer.keyId == nil ? .unknown : .ready
            return
        }
        switch (signer.keyId != nil, settings.serverHasKeyForThisDevice) {
        case (true, true): keyStatus = .ready
        case (true, false): keyStatus = .notRegistered
        case (false, true): keyStatus = .lostLocally
        case (false, false): keyStatus = .notSetUp
        }
    }

    /// "Main device for approvals": the server sends the first 30 s of every request to this device only.
    func setThisDeviceMain(_ on: Bool) async {
        guard let api = api(), let id = settings?.thisDeviceId, !settingsBusy else { return }
        settingsBusy = true
        defer { settingsBusy = false }
        do {
            try await api.setMainDevice(on ? id : nil)
            settingsMessage = nil
        } catch {
            settingsMessage = APIError.wrap(error).userMessage
        }
        await loadSettings()
    }

    /// Makes the Secure Enclave key and registers it. No password: this device's token authorizes the call, and the
    /// new key proves it exists by signing `secretary-approval-key-v1\n{thisDeviceId}\n{publicKeyB64}` (Face ID or the
    /// passcode). Returns true when the key is registered and saved.
    @discardableResult
    func setUpFaceID() async -> Bool {
        guard let api = api(), !settingsBusy else { return false }
        settingsBusy = true
        defer { settingsBusy = false }
        var registered = false
        do {
            let latest = try await api.settings()
            settings = latest
            guard let deviceId = latest.thisDeviceId, !deviceId.isEmpty else {
                throw ApprovalSignError.failed(String(localized: "The server doesn't know this iPhone yet. Open Secretary again in a moment."))
            }
            let publicKey = try signer.prepareKey().base64EncodedString()
            let signature = try await signer.signWithPreparedKey(
                ApprovalsAPI.registrationString(deviceId: deviceId, publicKey: publicKey),
                reason: String(localized: "Set up approvals on this iPhone"))
            let keyId = try await api.registerKey(publicKey: publicKey, label: "Face ID on \(deviceLabel())", signature: signature)
            try signer.commitKey(keyId: keyId)
            settingsMessage = nil
            registered = true
        } catch ApprovalSignError.cancelled {
            settingsMessage = nil
        } catch let error as ApprovalSignError {
            settingsMessage = error.userMessage
        } catch {
            let e = APIError.wrap(error)
            switch e.status {
            case 404, 405: settingsMessage = String(localized: "Approvals aren't on the server yet. Try again later."); availability = .unsupported
            default: settingsMessage = e.userMessage
            }
        }
        await loadSettings(keepMessage: true)
        return registered
    }

    /// The result of choosing, changing or removing the PIN.
    enum PinChange: Equatable {
        case done
        /// Face ID was cancelled: nothing was sent.
        case cancelled
        /// The server (or the app) refused; `currentWasWrong` sends the owner back to the current-PIN step.
        case refused(String, currentWasWrong: Bool)
    }

    static func isValidPin(_ pin: String) -> Bool {
        pin.count == 6 && pin.allSatisfy { $0 >= "0" && $0 <= "9" }
    }

    /// Chooses a PIN, or replaces the existing one. With a PIN already set, `currentPin` is the old PIN; without it, this
    /// device's key signs `secretary-approval-pin-v1\n{thisDeviceId}` (Face ID or the passcode).
    func setPIN(_ pin: String, currentPin: String? = nil) async -> PinChange {
        await changePIN(currentPin: currentPin) { api, proof in try await api.setPin(pin, currentPin: currentPin, proof: proof) } validate: { Self.isValidPin(pin) && (currentPin.map(Self.isValidPin) ?? true) }
    }

    func removePIN(currentPin: String? = nil) async -> PinChange {
        await changePIN(currentPin: currentPin) { api, proof in try await api.deletePin(currentPin: currentPin, proof: proof) } validate: { currentPin.map(Self.isValidPin) ?? true }
    }

    private func changePIN(currentPin: String?, send: (ApprovalsAPI, ApprovalsAPI.SignatureProof?) async throws -> Void, validate: () -> Bool) async -> PinChange {
        guard validate() else { return .refused(String(localized: "A PIN is 6 digits."), currentWasWrong: false) }
        guard let api = api() else { return .refused(APIError.network(message: "").userMessage, currentWasWrong: false) }
        guard !settingsBusy else { return .cancelled }
        settingsBusy = true
        defer { settingsBusy = false }
        do {
            if settings == nil { settings = try await api.settings() }
            var proof: ApprovalsAPI.SignatureProof?
            if settings?.hasPin == true, currentPin == nil, signer.keyId != nil, let deviceId = settings?.thisDeviceId {
                let signed = try await signer.sign(ApprovalsAPI.pinProofString(deviceId: deviceId), reason: String(localized: "Change your approval PIN"))
                proof = ApprovalsAPI.SignatureProof(signature: signed.signature)
            }
            try await send(api, proof)
            settingsMessage = nil
            await loadSettings()
            return .done
        } catch ApprovalSignError.cancelled {
            return .cancelled
        } catch let error as ApprovalSignError {
            return .refused(error.userMessage, currentWasWrong: false)
        } catch {
            let e = APIError.wrap(error)
            if e.endsSession { onTransportError?(e) }
            let outcome = ApprovalOutcome.from(error)
            if let text = outcome.pinMessage {
                if case .pinWrong = outcome { return .refused(text, currentWasWrong: true) }
                return .refused(text, currentWasWrong: false)
            }
            return .refused(e.userMessage, currentWasWrong: false)
        }
    }

    func removeFaceID() async {
        guard !settingsBusy else { return }
        settingsBusy = true
        defer { settingsBusy = false }
        if let api = api() { try? await api.deleteKey() }
        try? signer.removeKey()
        // The server drops the unlock key with the Face ID key.
        forgetUnlockKey()
        settingsMessage = nil
        await loadSettings()
    }

    // MARK: - One-tap Approve (the unlock key)

    /// Whether the notification's one-tap Approve is registered: the switch (absent: on) and an enrolled unlock key.
    /// For the launch, before the store exists.
    nonisolated static func oneTapReady(defaults: UserDefaults = .standard, unlockKeyId: String?) -> Bool {
        (defaults.object(forKey: oneTapKey) as? Bool ?? true) && unlockKeyId != nil
    }

    /// The Settings switch. On: enroll the unlock key (one Face ID); if that doesn't happen the switch goes back off.
    /// Off: the server forgets the key, then this iPhone deletes it.
    func setOneTap(_ on: Bool) async {
        let wasReady = oneTapReady
        oneTapOn = on
        defaults.set(on, forKey: Self.oneTapKey)
        if on {
            if unlockKeyId == nil, !(await enrollUnlockKey()) {
                oneTapOn = false
                defaults.set(false, forKey: Self.oneTapKey)
                // The switch is off again: its failure goes under the section (the Set up button is gone).
                if let failure = unlockEnrollFailure { settingsMessage = failure }
            }
        } else {
            await removeUnlockKey()
        }
        if oneTapReady != wasReady { onOneTapChanged?(oneTapReady) }
    }

    /// The app is in front: with the switch on (the default) and no unlock key yet, but this iPhone's Face ID key
    /// active on the server, enroll it. Never over a card or an answer on its way. A Face ID the owner cancelled ends it
    /// for this launch; any other failure is tried again the next time the app comes back from the background.
    func ensureUnlockKey() async {
        guard oneTapOn, unlockKeyId == nil, signer.keyId != nil, !unlockEnrollDeclined, enrollTask == nil, !isFixture,
              sheetItem == nil, working == nil else { return }
        if let failed = unlockEnrollFailedGeneration, failed == backgroundGeneration { return }
        let generation = backgroundGeneration
        switch await enroll() {
        case .enrolled: unlockEnrollFailedGeneration = nil
        case .cancelled: unlockEnrollDeclined = true
        case .failed: unlockEnrollFailedGeneration = generation
        }
    }

    /// Settings → Approvals → "Set up one-tap Approve": the enrollment now, whatever the automatic setup did before.
    func setUpOneTap() {
        guard oneTapOn, unlockKeyId == nil, enrollTask == nil else { return }
        Task { await enrollUnlockKey() }
    }

    /// Makes the unlock key, has the Face ID key prove it (`secretary-unlock-key-v1\n{thisDeviceId}\n{publicKeyB64}`, one
    /// Face ID prompt: "Allow one-tap Approve") and enrolls it. Returns true when the key is enrolled and saved.
    @discardableResult
    func enrollUnlockKey() async -> Bool {
        await enroll() == .enrolled
    }

    enum UnlockEnrollResult: Equatable {
        case enrolled
        /// The owner (or the system) cancelled Face ID.
        case cancelled
        case failed
    }

    /// The enrollment in its own task (one at a time; a second caller waits for the same one). Cancelling the caller
    /// doesn't cancel it.
    private func enroll() async -> UnlockEnrollResult {
        if let enrollTask { return await enrollTask.value }
        let task = Task { await self.runEnrollment() }
        enrollTask = task
        let result = await task.value
        if enrollTask == task { enrollTask = nil }
        return result
    }

    private func runEnrollment() async -> UnlockEnrollResult {
        guard let api = api(), signer.keyId != nil, !settingsBusy else { return .failed }
        settingsBusy = true
        defer { settingsBusy = false }
        do {
            let latest = try await survivingCancel("settings") { try await api.settings() }
            settings = latest
            guard let deviceId = latest.thisDeviceId, !deviceId.isEmpty, latest.serverHasKeyForThisDevice else {
                // No Face ID key on the server yet (not set up, or waiting for another device's OK): nothing to ask.
                log.info("unlock key not enrolled: no active Face ID key")
                unlockEnrollFailure = nil
                return .failed
            }
            let publicKey = try unlockSigner.prepareKey().base64EncodedString()
            let proof = try await signer.sign(ApprovalsAPI.unlockKeyString(deviceId: deviceId, publicKey: publicKey),
                                              reason: String(localized: "Allow one-tap Approve"))
            let keyId = try await survivingCancel("register") {
                try await api.registerUnlockKey(publicKey: publicKey, signature: proof.signature)
            }
            try unlockSigner.commitKey(keyId: keyId)
            let wasReady = oneTapReady
            unlockKeyId = keyId
            unlockEnrollFailure = nil
            log.info("unlock key enrolled")
            if oneTapReady != wasReady { onOneTapChanged?(oneTapReady) }
            return .enrolled
        } catch ApprovalSignError.cancelled {
            log.info("unlock key not enrolled: Face ID cancelled")
            unlockEnrollFailure = nil
            return .cancelled
        } catch let error as ApprovalSignError {
            log.error("unlock key not enrolled: \(String(describing: error), privacy: .public)")
            unlockEnrollFailure = error.userMessage
            return .failed
        } catch {
            let e = APIError.wrap(error)
            if e.endsSession { onTransportError?(e) }
            log.error("unlock key not enrolled: \(e.status, privacy: .public) \(e.code, privacy: .public)")
            unlockEnrollFailure = e.userMessage
            return .failed
        }
    }

    /// One of the enrollment's requests (the register after a good signature: the same key and proof, no second Face ID).
    /// The Face ID sheet makes the scene inactive and the requests in flight may be cancelled with it (or by the
    /// endpoint race when the app is active again): a cancel that isn't this task's own waits for the scene to be
    /// active (at most `unlockRetryWait`) and sends it again, up to 3 more times.
    private func survivingCancel<T>(_ what: StaticString, _ request: () async throws -> T) async throws -> T {
        var retries = 0
        while true {
            do {
                return try await request()
            } catch let error where Self.isCancel(error) && !Task.isCancelled && retries < 3 {
                retries += 1
                log.info("unlock key not enrolled yet: \(String(describing: what), privacy: .public) cancelled, retrying after cancel (\(retries, privacy: .public))")
                await waitForActiveScene()
            }
        }
    }

    nonisolated static func isCancel(_ error: Error) -> Bool {
        if error is CancellationError { return true }
        if let url = error as? URLError, url.code == .cancelled { return true }
        return false
    }

    private func waitForActiveScene() async {
        let clock = ContinuousClock()
        let deadline = clock.now.advanced(by: unlockRetryWait)
        while !sceneActive, clock.now < deadline, !Task.isCancelled {
            try? await Task.sleep(for: .milliseconds(50))
        }
    }

    /// The server forgets this iPhone's unlock key, then it is deleted here (also when the server can't be reached:
    /// without the private key nothing can sign).
    private func removeUnlockKey() async {
        if let api = api() {
            do {
                try await api.deleteUnlockKey()
            } catch {
                log.error("unlock key not removed on the server: \(APIError.wrap(error).status, privacy: .public)")
            }
        }
        forgetUnlockKey()
    }

    /// Deletes the local unlock key (one-tap falls back to the card's Approve).
    private func forgetUnlockKey() {
        let wasReady = oneTapReady
        try? unlockSigner.removeKey()
        unlockKeyId = nil
        if oneTapReady != wasReady { onOneTapChanged?(oneTapReady) }
    }

    /// Sign-out: forget the requests and the Face ID key (it was registered to the device token that is gone).
    func reset() {
        let ids = pending.map(\.id)
        pending = []
        pinEntry = false
        sheetItem = nil
        outcome = nil
        working = nil
        settings = nil
        settingsLoaded = false
        settingsMessage = nil
        keyStatus = .unknown
        availability = .unknown
        dismissed = []
        startApprove = nil
        defaults.removeObject(forKey: Self.notifiedKey)
        try? signer.removeKey()
        forgetUnlockKey()
        unlockEnrollDeclined = false
        unlockEnrollFailedGeneration = nil
        unlockEnrollFailure = nil
        let notifier = notifier
        Task { await notifier.remove(ids: ids) }
    }

    #if DEBUG || SCREENSHOTS
    /// `-SecretaryInitialTab approval | approval-plain | approval-done`: the prompt with fixture data (device screenshots).
    func loadFixture(_ name: String, now: Date = Date()) {
        isFixture = true
        switch name {
        case "approval-plain":
            sheetItem = Self.plainFixture(now: now)
        case "approval-done":
            sheetItem = Self.sensitiveFixture(now: now)
            outcome = .approved
        case "approval-already":
            sheetItem = Self.sensitiveFixture(now: now)
            outcome = .alreadyDecided(.approved, by: "iPad")
        default:
            sheetItem = Self.sensitiveFixture(now: now)
        }
        pending = sheetItem.map { [$0] } ?? []
    }

    static func sensitiveFixture(now: Date) -> ApprovalItem {
        ApprovalItem(
            id: "apr_fixture_sensitive",
            requester: ApprovalRequester(kind: "lucina", name: "Lucina", detail: "ask_lucina from Claude Code"),
            action: "root.exec",
            summary: "Install the fixed oracle-poko-tunnel unit and restart it",
            details: """
            install -m 644 /home/nez/scratch/oracle-poko-tunnel.service /etc/systemd/system/oracle-poko-tunnel.service
            systemctl daemon-reload
            systemctl enable oracle-poko-tunnel.service
            systemctl restart oracle-poko-tunnel.service
            systemctl is-active oracle-poko-tunnel.service
            """,
            risk: .high, sensitive: true, payloadHash: "sha256:" + String(repeating: "ab", count: 32),
            createdAt: now.addingTimeInterval(-12), expiresAt: now.addingTimeInterval(288), youAreMain: true)
    }

    static func plainFixture(now: Date) -> ApprovalItem {
        ApprovalItem(
            id: "apr_fixture_plain",
            requester: ApprovalRequester(kind: "agentd", name: "Lucina Agents", detail: "Writing the weekly summary"),
            action: "agent.permission",
            summary: "Read the 12 notes labelled Gym this week",
            details: "notes.search label=Gym since=2026-10-21\nnotes.get (12 notes, read only)",
            risk: .low, sensitive: false, payloadHash: "sha256:" + String(repeating: "cd", count: 32),
            createdAt: now.addingTimeInterval(-5), expiresAt: now.addingTimeInterval(540), youAreMain: true)
    }
    #endif
}
