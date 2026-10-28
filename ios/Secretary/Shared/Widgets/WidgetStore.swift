import Foundation
#if canImport(WidgetKit)
import WidgetKit
#endif

/// Where the app and the widget extension leave widget sections for each other.
///
/// With the App Group `group.tech.getsecretary.shared` (once it is registered on the team and in
/// both entitlements) each section is a JSON file in the group container. Until then, each is an
/// item in the shared keychain group the extensions already use (`BridgeCredentials`): small JSON,
/// readable after the first unlock, never synced or backed up. Writes keep the newer copy, so a
/// slow fetch never replaces a fresher one.
enum WidgetStore {
    static let appGroup = "group.tech.getsecretary.shared"

    static var groupContainer: URL? {
        FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: appGroup)
    }

    static func read<S: WidgetSection>(_ type: S.Type) -> S? {
        guard let data = load(S.key) else { return nil }
        return try? decoder.decode(S.self, from: data)
    }

    /// Saves `section` unless the stored copy is newer. Returns whether it was saved.
    @discardableResult
    static func write<S: WidgetSection>(_ section: S) -> Bool {
        if let old = read(S.self), old.fetchedAt > section.fetchedAt { return false }
        guard let data = try? encoder.encode(section) else { return false }
        return save(S.key, data)
    }

    /// Changes the stored section in place (an optimistic check-off), stamped now.
    static func update<S: WidgetSection>(_ type: S.Type, _ change: (inout S) -> Void) {
        guard var section = read(S.self) else { return }
        change(&section)
        guard let data = try? encoder.encode(section) else { return }
        _ = save(S.key, data)
    }

    /// Sign-out: nothing of the owner's stays behind for the widgets.
    static func removeAll() {
        for key in [AskSection.key, AppointmentSection.key, ShoppingSection.key, WeightSection.key, TrainingSection.key,
                    TasksSection.key, PokemonSection.key, FilmsSection.key, ShoppingEmojiSection.key,
                    CountdownPinsSection.key, CountdownUpcomingSection.key, ApprovalsSection.key] {
            if let url = fileURL(key) { try? FileManager.default.removeItem(at: url) }
            try? keychain(key)?.delete()
        }
        if let images = imageDirectory { try? FileManager.default.removeItem(at: images) }
    }

    /// The app changed this data itself: the next widget reload fetches it again, even when the
    /// stored copy is younger than the reuse window.
    static func changed(_ kind: WidgetKind) {
        switch kind {
        case .ask: markStale(AskSection.self)
        case .appointment: markStale(AppointmentSection.self)
        case .shopping: markStale(ShoppingSection.self)
        case .weight: markStale(WeightSection.self)
        case .training: break
        case .tasks: markStale(TasksSection.self)
        case .memo: break
        case .pokemon: markStale(PokemonSection.self)
        case .films: markStale(FilmsSection.self)
        case .countdown: markStale(CountdownUpcomingSection.self)
        case .approvals: markStale(ApprovalsSection.self)
        }
        reload([kind])
    }

    private static func markStale<S: WidgetSection>(_ type: S.Type) {
        guard let section = read(S.self), let data = try? encoder.encode(section),
              var object = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return }
        object["fetchedAt"] = 0
        guard let stale = try? JSONSerialization.data(withJSONObject: object) else { return }
        _ = save(S.key, stale)
    }

    /// Asks WidgetKit to rebuild the widgets that show `kinds` (all when empty).
    static func reload(_ kinds: [WidgetKind] = []) {
        #if canImport(WidgetKit)
        if kinds.isEmpty {
            WidgetCenter.shared.reloadAllTimelines()
        } else {
            for kind in kinds { WidgetCenter.shared.reloadTimelines(ofKind: kind.rawValue) }
        }
        #endif
    }

    // MARK: Images (posters, the game screen): files in the group, else this process's caches

    static var imageDirectory: URL? {
        let base = groupContainer?.appendingPathComponent("Library/Caches", isDirectory: true)
            ?? FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask).first
        guard let dir = base?.appendingPathComponent("WidgetImages", isDirectory: true) else { return nil }
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir
    }

    static func imageURL(_ name: String) -> URL? {
        let safe = name.map { $0.isLetter || $0.isNumber || $0 == "-" || $0 == "_" ? $0 : "_" }
        return imageDirectory?.appendingPathComponent(String(safe))
    }

    // MARK: Backends

    private static let encoder: JSONEncoder = {
        let e = JSONEncoder()
        e.dateEncodingStrategy = .millisecondsSince1970
        return e
    }()

    private static let decoder: JSONDecoder = {
        let d = JSONDecoder()
        d.dateDecodingStrategy = .millisecondsSince1970
        return d
    }()

    private static func fileURL(_ key: String) -> URL? {
        guard let dir = groupContainer?.appendingPathComponent("Library/Widgets", isDirectory: true) else { return nil }
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir.appendingPathComponent("\(key).json")
    }

    private static func keychain(_ key: String) -> KeychainStore? {
        BridgeCredentials.accessGroup.map { KeychainStore(service: "tech.getsecretary.shared.widgets", account: key, accessGroup: $0) }
    }

    private static func load(_ key: String) -> Data? {
        if let url = fileURL(key) { return try? Data(contentsOf: url) }
        guard let text = try? keychain(key)?.read() else { return nil }
        return Data(text.utf8)
    }

    private static func save(_ key: String, _ data: Data) -> Bool {
        if let url = fileURL(key) {
            do {
                try data.write(to: url, options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
                return true
            } catch {
                return false
            }
        }
        guard let store = keychain(key), let text = String(data: data, encoding: .utf8) else { return false }
        do {
            try store.save(text)
            return true
        } catch {
            return false
        }
    }
}
