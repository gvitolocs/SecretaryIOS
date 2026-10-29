import Foundation

/// Where the app leaves the label list for the Call Directory extension.
///
/// Like the widgets (`WidgetStore`): a file in the App Group container `group.tech.getsecretary.shared` once
/// that group is registered on the team and in every entitlement, until then one item in the shared keychain
/// group the extensions already use (readable after the first unlock, never synced or backed up).
enum CallerDirectoryStore {
    static let appGroup = "group.tech.getsecretary.shared"
    static let fileName = "caller-directory.txt"
    static let keychainService = "tech.getsecretary.shared.callers"
    static let keychainAccount = "directory"

    private static var fileURL: URL? {
        guard let root = FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: appGroup) else { return nil }
        let dir = root.appendingPathComponent("Library/Callers", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir.appendingPathComponent(fileName)
    }

    private static var keychain: KeychainStore? {
        BridgeCredentials.accessGroup.map {
            KeychainStore(service: keychainService, account: keychainAccount, accessGroup: $0)
        }
    }

    /// The saved list; nil when there is none or it is not readable.
    static func read() -> CallerDirectorySnapshot? {
        if let url = fileURL, let data = try? Data(contentsOf: url) { return CallerDirectoryList.decode(data) }
        guard let text = try? keychain?.read() else { return nil }
        return CallerDirectoryList.decode(Data(text.utf8))
    }

    /// Saves the list (already normalized by `encode`). Throws when neither backend takes it.
    static func write(_ snapshot: CallerDirectorySnapshot) throws {
        let data = CallerDirectoryList.encode(snapshot)
        if let url = fileURL {
            try data.write(to: url, options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
            try? keychain?.delete()
            return
        }
        guard let keychain else { throw CocoaError(.fileWriteNoPermission) }
        try keychain.save(String(decoding: data, as: UTF8.self))
    }

    /// Sign-out: no labels stay behind.
    static func remove() {
        if let url = fileURL { try? FileManager.default.removeItem(at: url) }
        try? keychain?.delete()
    }
}
