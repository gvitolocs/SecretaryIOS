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
    @ObservationIgnored private var lastAttempt: Date?

    init(api: @escaping () -> CallersAPI?, manager: CallDirectoryManaging,
         defaults: UserDefaults = .standard,
         readSaved: @escaping () -> CallerDirectorySnapshot? = CallerDirectoryStore.read,
         writeSaved: @escaping (CallerDirectorySnapshot) throws -> Void = CallerDirectoryStore.write,
         removeSaved: @escaping () -> Void = CallerDirectoryStore.remove) {
        self.api = api
        self.manager = manager
        self.defaults = defaults
        self.readSaved = readSaved
        self.writeSaved = writeSaved
        self.removeSaved = removeSaved
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

    /// On launch and whenever the app comes to the front.
    func syncIfDue(now: Date = Date()) async {
        guard Self.isDue(lastSync: lastSync, lastAttempt: lastAttempt, now: now) else {
            await refreshStatus()
            return
        }
        await sync(now: now)
    }

    /// Fetches the labels, saves them for the extension and has iOS reload it (only when the list changed,
    /// or never loaded, and the extension is on: iOS loads it by itself when the owner turns it on).
    func sync(now: Date = Date()) async {
        guard !isSyncing else { return }
        guard let api = api() else { return }
        isSyncing = true
        lastAttempt = now
        defer { isSyncing = false }
        let response: CallerDirectoryResponse
        do {
            response = try await api.directory()
        } catch is CancellationError {
            return
        } catch {
            let wrapped = APIError.wrap(error)
            syncError = wrapped.userMessage
            await refreshStatus()
            onTransportError?(wrapped)
            return
        }
        let snapshot = CallerDirectorySnapshot(generatedAt: response.generatedAt, entries: CallerDirectoryList.normalize(response.entries))
        let changed = readSaved()?.entries != snapshot.entries
        do {
            try writeSaved(snapshot)
            let status = await manager.enabledStatus()
            extensionStatus = status
            if status == .enabled, changed || !defaults.bool(forKey: Self.reloadedKey) {
                try await manager.reload()
                defaults.set(true, forKey: Self.reloadedKey)
            } else if status != .enabled {
                defaults.set(false, forKey: Self.reloadedKey)
            }
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
