import Foundation
import Observation

/// Whether the Call Directory extension may run (Settings > Phone > Call Blocking & Identification).
enum CallerExtensionStatus: Equatable, Sendable {
    /// Not asked yet, or iOS could not tell.
    case unknown
    case enabled
    case disabled
    /// The extension is not part of this build, or iOS refused to look.
    case unavailable
}

/// The two things the app asks of CallKit, so the sync can be tested without a phone.
protocol CallDirectoryManaging: Sendable {
    func enabledStatus() async -> CallerExtensionStatus
    func reload() async throws
    @MainActor func openSettings() async
    /// What to tell the owner when `reload()` failed.
    func reloadMessage(for error: Error) -> String
}

extension CallDirectoryManaging {
    func reloadMessage(for error: Error) -> String {
        String(localized: "Couldn't update the caller labels: \(error.localizedDescription)")
    }
}

/// A number iOS blocks, with the label the server gave it (when it has one).
struct CallerBlockedNumber: Equatable, Identifiable, Sendable {
    let number: Int64
    let label: String?

    var id: Int64 { number }
    /// `+4570405205`: what `POST /v1/callers/allow` takes.
    var e164: String { "+\(number)" }
    var display: String { e164 }
}

/// "Who called?": the label list on this iPhone (Call Directory extension) and the lookup screen.
@Observable
@MainActor
final class CallersStore {
    /// The list is refreshed at most this often, unless asked.
    nonisolated static let syncInterval: TimeInterval = 15 * 60
    /// After a failed attempt, the next one waits this long.
    nonisolated static let retryInterval: TimeInterval = 60
    static let lastSyncKey = "secretary.callers.lastSync"
    static let countKey = "secretary.callers.count"
    static let reloadedKey = "secretary.callers.reloaded"

    private(set) var extensionStatus: CallerExtensionStatus = .unknown
    /// Labels in the saved list; nil before the first sync.
    private(set) var entryCount: Int?
    private(set) var lastSync: Date?
    private(set) var syncError: String?
    private(set) var isSyncing = false

    /// "Block likely spam": on by default; changing it saves it and refreshes the lists (see `setBlockSettings`).
    private(set) var blockSettings = CallerBlockSettings.standard
    /// The numbers iOS blocks now, from the saved list.
    private(set) var blocked: [CallerBlockedNumber] = []
    /// Numbers an Allow is in flight for (digits only).
    private(set) var allowing: Set<String> = []
    /// Numbers the owner allowed since the app started (digits only): no "Would be blocked" for them.
    private(set) var allowed: Set<String> = []
    private(set) var blockError: String?

    private(set) var results: [CallerResult] = []
    private(set) var isLookingUp = false
    private(set) var lookupError: String?
    /// The lookup field's text, kept while the screen is away.
    var input = ""

    @ObservationIgnored var onTransportError: ((APIError) -> Void)?
    @ObservationIgnored private let api: () -> CallersAPI?
    @ObservationIgnored private let manager: CallDirectoryManaging
    @ObservationIgnored private let defaults: UserDefaults
    @ObservationIgnored private let readSaved: () -> CallerDirectorySnapshot?
    @ObservationIgnored private let writeSaved: (CallerDirectorySnapshot) throws -> Void
    @ObservationIgnored private let removeSaved: () -> Void
    @ObservationIgnored private let readSettings: () -> CallerBlockSettings?
    @ObservationIgnored private let writeSettings: (CallerBlockSettings) throws -> Void
    @ObservationIgnored private let removeSettings: () -> Void
    @ObservationIgnored private var lastAttempt: Date?
    @ObservationIgnored private var loadedSaved = false
    @ObservationIgnored private var resyncRequested = false
    @ObservationIgnored private var settingsTask: Task<Void, Never>?

    init(api: @escaping () -> CallersAPI?, manager: CallDirectoryManaging,
         defaults: UserDefaults = .standard,
         readSaved: @escaping () -> CallerDirectorySnapshot? = CallerDirectoryStore.read,
         writeSaved: @escaping (CallerDirectorySnapshot) throws -> Void = CallerDirectoryStore.write,
         removeSaved: @escaping () -> Void = CallerDirectoryStore.remove,
         readSettings: @escaping () -> CallerBlockSettings? = CallerBlockSettingsStore.read,
         writeSettings: @escaping (CallerBlockSettings) throws -> Void = CallerBlockSettingsStore.write,
         removeSettings: @escaping () -> Void = CallerBlockSettingsStore.remove) {
        self.api = api
        self.manager = manager
        self.defaults = defaults
        self.readSaved = readSaved
        self.writeSaved = writeSaved
        self.removeSaved = removeSaved
        self.readSettings = readSettings
        self.writeSettings = writeSettings
        self.removeSettings = removeSettings
        let last = defaults.double(forKey: Self.lastSyncKey)
        lastSync = last > 0 ? Date(timeIntervalSince1970: last) : nil
        entryCount = defaults.object(forKey: Self.countKey) as? Int
    }

    // MARK: Status

    func refreshStatus() async {
        extensionStatus = await manager.enabledStatus()
    }

    func openCallBlockingSettings() async {
        await manager.openSettings()
    }

    // MARK: Label list

    /// Whether a sync is due: the last good one is older than `interval`, and a failed attempt is not too recent.
    nonisolated static func isDue(lastSync: Date?, lastAttempt: Date?, now: Date,
                                  interval: TimeInterval = syncInterval, retry: TimeInterval = retryInterval) -> Bool {
        if let lastAttempt, now.timeIntervalSince(lastAttempt) < retry { return false }
        guard let lastSync else { return true }
        return now.timeIntervalSince(lastSync) >= interval
    }

    /// The saved settings and lists, read once (the keychain is not touched before the first frame).
    func loadSaved() {
        guard !loadedSaved else { return }
        loadedSaved = true
        if let settings = readSettings() { blockSettings = settings }
        if let snapshot = readSaved() { publishBlocked(snapshot) }
    }

    private func publishBlocked(_ snapshot: CallerDirectorySnapshot) {
        let labels = Dictionary(snapshot.entries.map { ($0.number, $0.label) }, uniquingKeysWith: { first, _ in first })
        blocked = snapshot.block.map { CallerBlockedNumber(number: $0, label: labels[$0]) }
    }

    /// On launch and whenever the app comes to the front.
    func syncIfDue(now: Date = Date()) async {
        guard Self.isDue(lastSync: lastSync, lastAttempt: lastAttempt, now: now) else {
            await refreshStatus()
            return
        }
        await sync(now: now)
    }

    /// Fetches the labels (and, with "Block likely spam" on, the numbers above the thresholds), saves them for the
    /// extension and has iOS reload it (only when a list changed, or never loaded, and the extension is on: iOS
    /// loads it by itself when the owner turns it on). A sync asked for while one runs runs again right after it.
    func sync(now: Date = Date()) async {
        guard api() != nil else { return }
        if isSyncing {
            resyncRequested = true
            return
        }
        loadSaved()
        isSyncing = true
        defer { isSyncing = false }
        repeat {
            resyncRequested = false
            await runSync(now: now)
        } while resyncRequested && !Task.isCancelled
    }

    private func runSync(now: Date) async {
        guard let api = api() else { return }
        lastAttempt = now
        let settings = blockSettings
        let response: CallerDirectoryResponse
        do {
            response = try await api.directory(query: settings.directoryQuery)
        } catch is CancellationError {
            return
        } catch {
            let wrapped = APIError.wrap(error)
            syncError = wrapped.userMessage
            await refreshStatus()
            onTransportError?(wrapped)
            return
        }
        // Switched off while the request ran: the lists carry no block numbers.
        let block = blockSettings.enabled && settings.enabled ? CallerDirectoryList.normalizeBlock(response.block) : []
        let snapshot = CallerDirectorySnapshot(generatedAt: response.generatedAt, entries: CallerDirectoryList.normalize(response.entries), block: block)
        let previous = readSaved()
        let changed = previous?.entries != snapshot.entries || previous?.block != snapshot.block
        do {
            try writeSaved(snapshot)
            publishBlocked(snapshot)
            try await reloadIfNeeded(changed: changed)
            syncError = nil
        } catch {
            syncError = manager.reloadMessage(for: error)
        }
        // The list is saved even when the reload failed; the sync counts as done so a broken reload is not retried in a loop.
        entryCount = snapshot.entries.count
        lastSync = now
        defaults.set(now.timeIntervalSince1970, forKey: Self.lastSyncKey)
        defaults.set(snapshot.entries.count, forKey: Self.countKey)
    }

    private func reloadIfNeeded(changed: Bool) async throws {
        let status = await manager.enabledStatus()
        extensionStatus = status
        if status == .enabled, changed || !defaults.bool(forKey: Self.reloadedKey) {
            try await manager.reload()
            defaults.set(true, forKey: Self.reloadedKey)
        } else if status != .enabled {
            defaults.set(false, forKey: Self.reloadedKey)
        }
    }

    // MARK: Block likely spam

    /// Saves the switch and thresholds. Off: the block list is emptied and the extension reloaded at once. On, or
    /// new thresholds: the lists are fetched again a moment later (a stepper tapped five times syncs once).
    func setBlockSettings(_ new: CallerBlockSettings) {
        loadSaved()
        let settings = new.clamped
        guard settings != blockSettings else { return }
        let wasEnabled = blockSettings.enabled
        blockSettings = settings
        do { try writeSettings(settings) } catch { blockError = String(localized: "Couldn't save the setting: \(error.localizedDescription)") }
        settingsTask?.cancel()
        if !settings.enabled {
            settingsTask = Task { await emptyBlockList() }
        } else if api() != nil {
            settingsTask = Task {
                if wasEnabled { try? await Task.sleep(for: .seconds(1)) }
                guard !Task.isCancelled else { return }
                await sync()
            }
        }
    }

    /// Blocking off: the saved block list goes, the extension drops its blocking entries.
    private func emptyBlockList() async {
        guard var snapshot = readSaved(), !snapshot.block.isEmpty else {
            blocked = []
            return
        }
        snapshot.block = []
        do {
            try writeSaved(snapshot)
            blocked = []
            try await reloadIfNeeded(changed: true)
            syncError = nil
        } catch {
            syncError = manager.reloadMessage(for: error)
        }
    }

    nonisolated static func digits(_ number: String) -> String { number.filter(\.isNumber) }

    /// Whether the thresholds would block this looked-up number: not a contact, not allowed, not invalid.
    func wouldBlock(_ result: CallerResult) -> Bool {
        guard !result.isInvalid, result.source != "contact", result.known?.contact == nil,
              !allowed.contains(Self.digits(result.number)) else { return false }
        return blockSettings.blocks(spam: result.spam, pickUp: result.pickUp)
    }

    func isAllowed(_ result: CallerResult) -> Bool { allowed.contains(Self.digits(result.number)) }

    /// "Allow": the server never rejects or blocks this number again; the lists are fetched again and the
    /// extension reloaded. `number` is `+4570405205` or any spelling the server reads.
    func allow(_ number: String) async {
        let key = Self.digits(number)
        guard !key.isEmpty, !allowing.contains(key), let api = api() else { return }
        allowing.insert(key)
        defer { allowing.remove(key) }
        blockError = nil
        do {
            let response = try await api.allow(number: number)
            guard response.allowed else {
                blockError = String(localized: "Couldn't allow this number.")
                return
            }
            allowed.insert(key)
            blocked.removeAll { String($0.number) == key }
            // Looked-up cards pick up the new label too.
            if let index = results.firstIndex(where: { Self.digits($0.number) == key }) {
                var row = results[index]
                row.label = String(localized: "Allowed")
                results[index] = row
            }
            await sync()
        } catch is CancellationError {
            return
        } catch {
            let wrapped = APIError.wrap(error)
            blockError = wrapped.userMessage
            onTransportError?(wrapped)
        }
    }

    #if DEBUG || SCREENSHOTS
    /// `-SecretaryCallersSeed 5000`: saves that many synthetic labels (numbers 99 900 000 000 and up, no real
    /// subscriber), reads them back and reloads the extension, to try the storage and iOS's reload on the phone.
    /// `-SecretaryCallersReset YES` removes them again.
    func seedForTesting(count: Int) async {
        let entries = (0 ..< count).map { CallerDirectoryEntry(number: 99_900_000_000 + Int64($0), label: "Test label \($0)") }
        let snapshot = CallerDirectorySnapshot(generatedAt: nil, entries: entries)
        do {
            try writeSaved(snapshot)
            let back = readSaved()
            entryCount = back?.entries.count
            lastSync = Date()
            syncError = back?.entries == CallerDirectoryList.normalize(entries) ? nil : "Seed read-back differs: \(back?.entries.count ?? -1) of \(count)"
            extensionStatus = await manager.enabledStatus()
            if extensionStatus == .enabled { try await manager.reload() }
        } catch {
            syncError = "Seed failed: \(error.localizedDescription)"
        }
    }
    #endif

    /// Sign-out: the labels leave this iPhone too.
    func reset() {
        removeSaved()
        removeSettings()
        settingsTask?.cancel()
        blockSettings = .standard
        blocked = []
        allowed = []
        allowing = []
        blockError = nil
        loadedSaved = false
        defaults.removeObject(forKey: Self.lastSyncKey)
        defaults.removeObject(forKey: Self.countKey)
        defaults.removeObject(forKey: Self.reloadedKey)
        lastSync = nil
        lastAttempt = nil
        entryCount = nil
        syncError = nil
        results = []
        lookupError = nil
        input = ""
        let manager = manager
        // An empty list replaces what the extension added.
        Task { try? await manager.reload() }
    }

    // MARK: Lookup

    /// `en`, `it` or `da`: the app's language.
    nonisolated static func requestLanguage(_ localizations: [String] = Bundle.main.preferredLocalizations) -> String {
        for code in localizations {
            let base = String(code.prefix(2)).lowercased()
            if ["en", "it", "da"].contains(base) { return base }
        }
        return "en"
    }

    func lookup(_ text: String) async {
        let numbers = CallerNumbers.parse(text)
        guard !numbers.isEmpty else {
            results = []
            lookupError = nil
            return
        }
        guard let api = api(), !isLookingUp else { return }
        isLookingUp = true
        defer { isLookingUp = false }
        var found: [CallerResult] = []
        do {
            for chunk in CallerNumbers.chunks(numbers) {
                let response = try await api.screen(CallerScreenRequest(numbers: chunk, lang: Self.requestLanguage()))
                found += response.results
            }
            results = found
            lookupError = nil
            // The server may have added these numbers to the list: pick that up for the next call.
            Task { await sync() }
        } catch is CancellationError {
            return
        } catch {
            let wrapped = APIError.wrap(error)
            results = found
            lookupError = wrapped.userMessage
            onTransportError?(wrapped)
        }
    }
}
