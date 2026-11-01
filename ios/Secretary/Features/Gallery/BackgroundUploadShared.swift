import Foundation
import Photos
import Security

/// What the app's photo backup (PhotoBackup.swift) and its PhotoKit background upload extension
/// (Extensions/SecretaryPhotoUpload, iOS 27) share. There is no App Group, so it goes through the
/// shared keychain group ($(AppIdentifierPrefix)tech.getsecretary.shared):
/// - the app writes `Config` (server, key, device id) whenever the backup runs;
/// - the extension writes one `Sent` item per photo iOS uploaded for it, and the app's next pass
///   moves them into its ledger, so it doesn't send those photos again.
enum BackgroundUpload {
    private static let service = "tech.getsecretary.bgupload"
    private static let sentService = "tech.getsecretary.bgupload.sent"

    struct Config: Codable, Equatable, Sendable {
        var baseURL: String
        var accessKey: String
        var deviceId: String
        var includeVideos: Bool
        /// Changes when the extension is turned on again: it starts over from `startToken`.
        var generation: String
        /// PhotoKit's change token when the extension was turned on (an archived `PHPersistentChangeToken`):
        /// photos from before are the app's.
        var startToken: Data?
    }

    struct Sent: Codable, Sendable {
        var key: String
        var itemId: String
        var modified: Date?
        var signature: String
        var favorite: Bool?
    }

    private static var configStore: KeychainStore? {
        BridgeCredentials.accessGroup.map { KeychainStore(service: service, account: "config", accessGroup: $0) }
    }

    static func readConfig() -> Config? {
        guard let raw = try? configStore?.read(), let data = raw.data(using: .utf8) else { return nil }
        return try? JSONDecoder().decode(Config.self, from: data)
    }

    static func writeConfig(_ config: Config?) {
        guard let store = configStore else { return }
        if let config, let data = try? JSONEncoder().encode(config), let text = String(data: data, encoding: .utf8) {
            try? store.save(text)
        } else {
            try? store.delete()
        }
    }

    static func recordSent(_ sent: Sent) {
        guard let group = BridgeCredentials.accessGroup,
              let data = try? JSONEncoder().encode(sent), let text = String(data: data, encoding: .utf8) else { return }
        try? KeychainStore(service: sentService, account: sent.key, accessGroup: group).save(text)
    }

    /// Every `Sent` item, removed from the keychain as it is read.
    static func drainSent() -> [Sent] {
        guard let group = BridgeCredentials.accessGroup else { return [] }
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: sentService,
            kSecAttrAccessGroup as String: group,
            kSecMatchLimit as String: kSecMatchLimitAll,
            kSecReturnAttributes as String: true,
            kSecReturnData as String: true,
        ]
        var result: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &result) == errSecSuccess,
              let items = result as? [[String: Any]] else { return [] }
        var sent: [Sent] = []
        for item in items {
            guard let account = item[kSecAttrAccount as String] as? String else { continue }
            if let data = item[kSecValueData as String] as? Data, let s = try? JSONDecoder().decode(Sent.self, from: data) {
                sent.append(s)
            }
            try? KeychainStore(service: sentService, account: account, accessGroup: group).delete()
        }
        return sent
    }

    // MARK: Rules both use, so the server sees the same asset ids and files from either.

    /// The edited rendering when there is one, else the original.
    static func mainResource(_ asset: PHAsset, _ resources: [PHAssetResource]) -> PHAssetResource? {
        let order: [PHAssetResourceType] = asset.mediaType == .video ? [.fullSizeVideo, .video] : [.fullSizePhoto, .photo]
        for t in order {
            if let r = resources.first(where: { $0.type == t }) { return r }
        }
        return resources.first
    }

    /// A Live Photo's motion half.
    static func pairedResource(_ resources: [PHAssetResource]) -> PHAssetResource? {
        resources.first(where: { $0.type == .fullSizePairedVideo }) ?? resources.first(where: { $0.type == .pairedVideo })
    }

    /// What was sent, to tell an edit from the same file: the resource type and its file name.
    static func signature(_ resource: PHAssetResource) -> String {
        "\(resource.type.rawValue):\(resource.originalFilename)"
    }

    static func mime(forUTI uti: String) -> String? {
        switch uti {
        case "public.heic": "image/heic"
        case "public.jpeg": "image/jpeg"
        case "public.png": "image/png"
        case "com.apple.quicktime-movie": "video/quicktime"
        case "public.mpeg-4": "video/mp4"
        case "com.adobe.raw-image", "com.adobe.dng": "image/x-adobe-dng"
        default: nil
        }
    }
}
