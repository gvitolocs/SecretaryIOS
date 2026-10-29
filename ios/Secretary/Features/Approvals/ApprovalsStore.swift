import Foundation
import Observation

/// The owner's approvals on this iPhone: polls the pending requests, shows the global prompt, posts a local
/// notification once per request, and answers (a sensitive "yes" is signed with the Face ID key).
///
/// The app has no remote push (free Personal Team), so it asks: every 3 s while it is active, and once when
/// iOS wakes it for a background refresh. A server without the approvals routes (404) is left alone: no banner,
/// no error, one request every 30 s.
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

    // Settings → Approvals
    private(set) var settings: ApprovalSettings?
    private(set) var settingsLoaded = false
    private(set) var settingsBusy = false
    var settingsMessage: String?
    private(set) var keyStatus: KeyStatus = .unknown

    @ObservationIgnored private let api: () -> ApprovalsAPI?
    @ObservationIgnored let signer: ApprovalSigning
    @ObservationIgnored let notifier: ApprovalNotifying
    @ObservationIgnored private let previews: () -> Bool
    @ObservationIgnored private let deviceLabel: () -> String
    @ObservationIgnored private let defaults: UserDefaults
    @ObservationIgnored var onTransportError: ((APIError) -> Void)?
    /// The pending list changed (the app hands it to the approvals widget).
    @ObservationIgnored var onPendingChanged: (([ApprovalItem]) -> Void)?
    /// Requests the owner closed without answering; they come back when the app becomes active again.
    @ObservationIgnored private var dismissed: Set<String> = []
    @ObservationIgnored private var polling = false
    /// The last list's cursor (nil: none yet, or the server does not long-poll).
    @ObservationIgnored private var cursor: String?
    /// Fixture data for the screenshots: nothing is polled.
    @ObservationIgnored private(set) var isFixture = false

    init(api: @escaping () -> ApprovalsAPI?, signer: ApprovalSigning, notifier: ApprovalNotifying,
         previews: @escaping () -> Bool = { true }, deviceLabel: @escaping () -> String = { "iPhone" },
         defaults: UserDefaults = .standard) {
        self.api = api
        self.signer = signer
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
        if let item = sheetItem, outcome?.isFinal != true { dismissed.insert(item.id) }
        sheetItem = nil
        outcome = nil
        working = nil
        await refreshSheet()
    }

    /// Opens the prompt on one request (a tapped notification), fetching it when it is not listed yet.
    func open(id: String) async {
        dismissed.remove(id)
        pinEntry = false
        working = nil
        outcome = nil
        if let item = pending.first(where: { $0.id == id }) {
            sheetItem = item
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
            case .pending, nil: break
            }
        } catch {
            sheetItem = ApprovalItem(id: id, summary: "")
            outcome = ApprovalOutcome.from(error)
        }
    }

    // MARK: - Answering

    /// The prompt's Approve / Deny. `pin` is the owner's 6 digits from the PIN pad (a sensitive approve only).
    func decide(_ decision: ApprovalDecision, pin: String? = nil) async {
        guard let item = sheetItem, working == nil, outcome?.isFinal != true else { return }
        working = decision
        outcome = nil
        let result = await answer(item, decision, pin: pin)
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

    /// Sends the answer. A sensitive approve carries the owner's `pin`, or is signed first (Face ID or the passcode);
    /// a deny never needs either. Nil: Face ID was cancelled. `.pinRequired`: the PIN pad takes over.
    func answer(_ item: ApprovalItem, _ decision: ApprovalDecision, pin: String? = nil) async -> ApprovalOutcome? {
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
                } catch ApprovalSignError.cancelled {
                    return nil
                } catch ApprovalSignError.noKey {
                    return hasPin ? .pinRequired : .needsSetup
                } catch let error as ApprovalSignError {
                    // Face ID is locked out or not set up, or there is no Secure Enclave: the PIN takes over.
                    if hasPin, error == .needsDeviceSecurity || error == .unavailable { return .pinRequired }
                    return .failed(error.userMessage)
                } catch {
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

    /// A notification button or tap (AppDelegate). A button answers without opening the app, except a sensitive
    /// approve, which opens the card (the passcode is asked there, on Approve).
    func handleNotification(actionIdentifier: String, approvalId: String, sensitive: Bool) async {
        guard let action = ApprovalNotificationAction(identifier: actionIdentifier) else { return }
        switch action.route(sensitive: sensitive) {
        case .openSheet:
            await open(id: approvalId)
        case let .decide(decision):
            var known = pending.first { $0.id == approvalId }
            if known == nil, let api = api() { known = try? await api.get(approvalId) }
            guard let item = known else {
                await open(id: approvalId)
                return
            }
            guard item.status == nil || item.status == .pending else {
                await notifier.remove(ids: [item.id])
                return
            }
            // The request became sensitive since the notification was made: ask on the card.
            if decision == .approve, item.sensitive {
                await open(id: approvalId)
                return
            }
            guard let result = await answer(item, decision) else { return }
            if result.isFinal {
                pending.removeAll { $0.id == item.id }
                onPendingChanged?(pending)
                if sheetItem?.id == item.id { sheetItem = nil; outcome = nil }
                await notifier.remove(ids: [item.id])
            } else {
                await notifier.postFailure(for: item)
            }
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
        settingsMessage = nil
        await loadSettings()
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
        defaults.removeObject(forKey: Self.notifiedKey)
        try? signer.removeKey()
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
