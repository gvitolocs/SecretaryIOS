import Foundation
import Security

/// Stores the device access key as a generic password, readable after the first
/// unlock and never migrated to other devices or backups.
struct KeychainStore: Sendable {
    let service: String
    let account: String
    /// A shared access group (the Safari and Screen Time extensions read the bridge item).
    var accessGroup: String? = nil

    static let accessKey = KeychainStore(service: "tech.getsecretary.app", account: "device-access-key")

    enum KeychainError: Error, Equatable {
        case unexpectedStatus(OSStatus)
        case badData
    }

    private var baseQuery: [String: Any] {
        var query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
        if let accessGroup { query[kSecAttrAccessGroup as String] = accessGroup }
        return query
    }

    func read() throws -> String? {
        var query = baseQuery
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        switch status {
        case errSecSuccess:
            guard let data = result as? Data, let value = String(data: data, encoding: .utf8) else {
                throw KeychainError.badData
            }
            return value
        case errSecItemNotFound:
            return nil
        default:
            throw KeychainError.unexpectedStatus(status)
        }
    }

    func save(_ value: String) throws {
        let data = Data(value.utf8)
        let attributes: [String: Any] = [
            kSecValueData as String: data,
            kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
        ]
        let status = SecItemUpdate(baseQuery as CFDictionary, attributes as CFDictionary)
        if status == errSecSuccess { return }
        guard status == errSecItemNotFound else { throw KeychainError.unexpectedStatus(status) }
        var add = baseQuery
        add.merge(attributes) { $1 }
        let addStatus = SecItemAdd(add as CFDictionary, nil)
        guard addStatus == errSecSuccess else { throw KeychainError.unexpectedStatus(addStatus) }
    }

    func delete() throws {
        let status = SecItemDelete(baseQuery as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else {
            throw KeychainError.unexpectedStatus(status)
        }
    }

    /// The accessibility class of the stored item (for tests).
    func accessibility() throws -> String? {
        var query = baseQuery
        query[kSecReturnAttributes as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        if status == errSecItemNotFound { return nil }
        guard status == errSecSuccess, let attributes = result as? [String: Any] else {
            throw KeychainError.unexpectedStatus(status)
        }
        return attributes[kSecAttrAccessible as String] as? String
    }
}

/// What the app's extensions need to report to the server on their own: the device token
/// and the server addresses, in the shared keychain group. Includes each enabled source
/// that uses an extension, plus explicit shares at every sign-in; removed on sign-out.
struct BridgeCredentials: Codable, Equatable, Sendable {
    var token: String
    var endpoints: [String]
    var sources: [String]

    /// `$(AppIdentifierPrefix)tech.getsecretary.shared`, from Info.plist.
    static var accessGroup: String? {
        guard let group = Bundle.main.object(forInfoDictionaryKey: "SecretaryKeychainGroup") as? String,
              !group.hasPrefix("$(") else { return nil }
        return group
    }

    static var store: KeychainStore? {
        accessGroup.map { KeychainStore(service: "tech.getsecretary.shared", account: "bridge", accessGroup: $0) }
    }

    static func read() -> BridgeCredentials? {
        guard let raw = try? store?.read(), let data = raw.data(using: .utf8) else { return nil }
        return try? JSONDecoder().decode(BridgeCredentials.self, from: data)
    }

    static func write(_ credentials: BridgeCredentials?) {
        guard let store else { return }
        if let credentials, let data = try? JSONEncoder().encode(credentials), let text = String(data: data, encoding: .utf8) {
            try? store.save(text)
        } else {
            try? store.delete()
        }
    }
}
