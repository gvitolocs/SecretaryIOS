import Foundation

/// One small item the app leaves for the Call Directory extension.
///
/// Like the widgets (`WidgetStore`): a file in the App Group container `group.tech.getsecretary.shared` once
/// that group is registered on the team and in every entitlement, until then one item in the shared keychain
/// group the extensions already use (readable after the first unlock, never synced or backed up).
struct CallerSharedItem: Sendable {
    static let appGroup = "group.tech.getsecretary.shared"
    static let keychainService = "tech.getsecretary.shared.callers"

    let fileName: String
    let account: String

    private var fileURL: URL? {
        guard let root = FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: Self.appGroup) else { return nil }
        let dir = root.appendingPathComponent("Library/Callers", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir.appendingPathComponent(fileName)
    }

    private var keychain: KeychainStore? {
        BridgeCredentials.accessGroup.map { KeychainStore(service: Self.keychainService, account: account, accessGroup: $0) }
    }

    func read() -> Data? {
        if let url = fileURL, let data = try? Data(contentsOf: url) { return data }
        guard let text = try? keychain?.read() else { return nil }
        return Data(text.utf8)
    }

    /// Throws when neither backend takes it.
    func write(_ data: Data) throws {
        if let url = fileURL {
            try data.write(to: url, options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
            try? keychain?.delete()
            return
        }
        guard let keychain else { throw CocoaError(.fileWriteNoPermission) }
        try keychain.save(String(decoding: data, as: UTF8.self))
    }

    func remove() {
        if let url = fileURL { try? FileManager.default.removeItem(at: url) }
        try? keychain?.delete()
    }
}

/// Where the app leaves the label and block lists for the Call Directory extension.
enum CallerDirectoryStore {
    static let appGroup = CallerSharedItem.appGroup
    private static let item = CallerSharedItem(fileName: "caller-directory.txt", account: "directory")

    /// The saved list; nil when there is none or it is not readable.
    static func read() -> CallerDirectorySnapshot? {
        item.read().flatMap(CallerDirectoryList.decode)
    }

    /// Saves the list (normalized by `encode`).
    static func write(_ snapshot: CallerDirectorySnapshot) throws {
        try item.write(CallerDirectoryList.encode(snapshot))
    }

    /// Sign-out: no labels stay behind.
    static func remove() { item.remove() }
}

/// "Block likely spam": the owner's switch and thresholds. The app sends them to the server with the directory
/// request (the server lists only numbers above them); the extension reads the switch too, so a stale list is
/// never blocked on after the owner turned blocking off.
struct CallerBlockSettings: Codable, Equatable, Sendable {
    /// On by default (owner, 2026-10-29).
    var enabled = true
    /// Block when P(spam) is at least this ...
    var spamMin = 0.60
    /// ... and P(he should pick up) is at most this.
    var pickUpMax = 0.25

    static let standard = CallerBlockSettings()
    static let spamRange = 0.30 ... 0.99
    static let pickUpRange = 0.05 ... 0.60
    static let step = 0.05

    /// Thresholds inside their ranges, on the 0.01 grid the server query uses.
    var clamped: CallerBlockSettings {
        func snap(_ value: Double, _ range: ClosedRange<Double>) -> Double {
            guard value.isFinite else { return range.lowerBound }
            return (min(max(value, range.lowerBound), range.upperBound) * 100).rounded() / 100
        }
        return CallerBlockSettings(enabled: enabled, spamMin: snap(spamMin, Self.spamRange), pickUpMax: snap(pickUpMax, Self.pickUpRange))
    }

    /// Whether a verdict is above the thresholds (the server applies the same rule to its list). Never for a
    /// number the owner turned blocking off for.
    func blocks(spam: Double?, pickUp: Double?) -> Bool {
        guard enabled, let spam, let pickUp, spam.isFinite, pickUp.isFinite else { return false }
        // The server compares at two decimals.
        return (spam * 100).rounded() >= (spamMin * 100).rounded() && (pickUp * 100).rounded() <= (pickUpMax * 100).rounded()
    }

    /// `block=1&spam=0.60&pickup=0.25`, or nothing when blocking is off.
    var directoryQuery: [URLQueryItem] {
        guard enabled else { return [] }
        let c = clamped
        return [URLQueryItem(name: "block", value: "1"),
                URLQueryItem(name: "spam", value: String(format: "%.2f", c.spamMin)),
                URLQueryItem(name: "pickup", value: String(format: "%.2f", c.pickUpMax))]
    }
}

enum CallerBlockSettingsStore {
    private static let item = CallerSharedItem(fileName: "caller-block-settings.json", account: "block-settings")

    static func read() -> CallerBlockSettings? {
        guard let data = item.read() else { return nil }
        return (try? JSONDecoder().decode(CallerBlockSettings.self, from: data))?.clamped
    }

    static func write(_ settings: CallerBlockSettings) throws {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        try item.write(encoder.encode(settings.clamped))
    }

    static func remove() { item.remove() }
}
