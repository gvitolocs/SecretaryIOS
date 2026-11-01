import CryptoKit
import Foundation
import LocalAuthentication
import Security

/// How a signature asks for the owner.
enum ApprovalAuthMode: Equatable, Sendable {
    /// Face ID (or the passcode) now, every time.
    case prompt
    /// A Face ID unlock of the iPhone in the last 5 minutes counts (no prompt); otherwise Face ID is asked, or with
    /// `interactive: false` (the app in the background) nothing is shown and the signature fails with
    /// `ApprovalSignError.needsInteraction`.
    case reuseUnlock(interactive: Bool)
}

/// The Face ID (or passcode) proof of a sensitive approval. A protocol so the tests need no Secure Enclave.
protocol ApprovalSigning: Sendable {
    /// The id the server gave this iPhone's registered key, when it has one.
    var keyId: String? { get }
    /// Makes a new Secure Enclave key (kept in memory, not saved) and returns its X9.63 public key.
    func prepareKey() throws -> Data
    /// Signs `message` with the key just prepared (not yet saved), after Face ID or the passcode. Returns the base64 DER signature.
    func signWithPreparedKey(_ message: String, reason: String) async throws -> String
    /// Saves the prepared key once the server registered it under `keyId`.
    func commitKey(keyId: String) throws
    func removeKey() throws
    /// Signs `message` (ASCII bytes) after Face ID or the passcode, as `mode` says. `reason` is what the sheet says.
    func sign(_ message: String, reason: String, mode: ApprovalAuthMode) async throws -> ApprovalProof
}

extension ApprovalSigning {
    /// Face ID or the passcode now.
    func sign(_ message: String, reason: String) async throws -> ApprovalProof {
        try await sign(message, reason: reason, mode: .prompt)
    }
}

enum ApprovalSignError: Error, Equatable {
    /// No registered key on this iPhone.
    case noKey
    /// The owner cancelled Face ID (or the system did): nothing to report.
    case cancelled
    /// Face ID is not enrolled or locked out, or there is no passcode: what to do about it.
    case needsDeviceSecurity
    case unavailable
    /// Signing needed Face ID or the passcode on screen, and the app may not show it (in the background).
    case needsInteraction
    case failed(String)

    var userMessage: String {
        switch self {
        case .noKey: return String(localized: "Face ID or passcode isn't set up for approvals on this iPhone.")
        case .cancelled, .needsInteraction: return ""
        case .needsDeviceSecurity:
            return String(localized: "Set up Face ID in Settings → Face ID & Passcode, or approve with your passcode.")
        case .unavailable: return String(localized: "This iPhone can't keep an approval key.")
        case let .failed(message): return message
        }
    }

    /// What a LocalAuthentication failure means for the owner.
    static func from(_ error: LAError) -> ApprovalSignError {
        switch error.code {
        case .userCancel, .appCancel, .systemCancel: return .cancelled
        case .notInteractive: return .needsInteraction
        case .biometryNotEnrolled, .biometryLockout, .passcodeNotSet, .biometryNotAvailable: return .needsDeviceSecurity
        default: return .failed(error.localizedDescription)
        }
    }
}

/// A P-256 key in the Secure Enclave, usable only after Face ID, or the device passcode when Face ID fails or isn't
/// available (`.userPresence`; it survives a change of the enrolled faces). The key's opaque `dataRepresentation`
/// and the server's key id sit in the Keychain. Keys of earlier versions (1: Face ID only, 2: the same as now,
/// 3: passcode only, tried on 2026-10-29 and dropped the same day) are ignored, so the next setup replaces them.
final class SecureEnclaveApprovalSigner: ApprovalSigning, @unchecked Sendable {
    static let store = KeychainStore(service: "tech.getsecretary.app", account: "approval-key")
    /// 4: `[.privateKeyUsage, .userPresence]`.
    static let keyVersion = 4

    /// The key's access control: Face ID, or the passcode.
    static func accessControl() -> SecAccessControl? {
        var error: Unmanaged<CFError>?
        let access = SecAccessControlCreateWithFlags(
            nil, kSecAttrAccessibleWhenPasscodeSetThisDeviceOnly, [.privateKeyUsage, .userPresence], &error)
        _ = error?.takeRetainedValue()
        return access
    }

    private struct Stored: Codable {
        var data: String
        var keyId: String
        var version: Int?
    }

    private let store: KeychainStore
    private let lock = NSLock()
    private var prepared: SecureEnclave.P256.Signing.PrivateKey?

    init(store: KeychainStore = SecureEnclaveApprovalSigner.store) {
        self.store = store
    }

    private func stored() -> Stored? {
        guard let text = try? store.read(), let data = text.data(using: .utf8),
              let blob = try? JSONDecoder().decode(Stored.self, from: data),
              blob.version == Self.keyVersion else { return nil }
        return blob
    }

    var keyId: String? { stored()?.keyId }

    func prepareKey() throws -> Data {
        guard SecureEnclave.isAvailable else { throw ApprovalSignError.unavailable }
        // Creating a key bound to the passcode fails when the iPhone has none.
        guard let access = Self.accessControl() else { throw ApprovalSignError.needsDeviceSecurity }
        let key: SecureEnclave.P256.Signing.PrivateKey
        do {
            key = try SecureEnclave.P256.Signing.PrivateKey(accessControl: access)
        } catch {
            // errSecAuthFailed (-25293) / errSecNotAvailable (-25291): no passcode, or the Secure Enclave refused.
            let code = (error as NSError).code
            if code == Int(errSecAuthFailed) || code == Int(errSecNotAvailable) || code == Int(errSecParam) {
                throw ApprovalSignError.needsDeviceSecurity
            }
            throw ApprovalSignError.failed("\(error.localizedDescription) (\(code))")
        }
        lock.withLock { prepared = key }
        return key.publicKey.x963Representation
    }

    func commitKey(keyId: String) throws {
        guard let key = lock.withLock({ prepared }) else { throw ApprovalSignError.noKey }
        let blob = Stored(data: key.dataRepresentation.base64EncodedString(), keyId: keyId, version: Self.keyVersion)
        guard let json = try? JSONEncoder().encode(blob), let text = String(data: json, encoding: .utf8) else {
            throw ApprovalSignError.failed("encode")
        }
        try store.save(text)
        lock.withLock { prepared = nil }
    }

    func removeKey() throws {
        lock.withLock { prepared = nil }
        try store.delete()
    }

    func sign(_ message: String, reason: String, mode: ApprovalAuthMode) async throws -> ApprovalProof {
        guard let stored = stored(), let blob = Data(base64Encoded: stored.data) else { throw ApprovalSignError.noKey }
        let signature = try await Self.sign(message, reason: reason, keyData: blob, mode: mode)
        return ApprovalProof(keyId: stored.keyId, signature: signature)
    }

    func signWithPreparedKey(_ message: String, reason: String) async throws -> String {
        guard let key = lock.withLock({ prepared }) else { throw ApprovalSignError.noKey }
        return try await Self.sign(message, reason: reason, keyData: key.dataRepresentation, mode: .prompt)
    }

    /// Face ID, or the passcode, then the Secure Enclave signature (base64 DER) over `message`. Asked once, here,
    /// when the owner tapped Approve. `.reuseUnlock`: a Face ID unlock of the iPhone in the last 5 minutes satisfies
    /// the policy without a prompt (the key, its access control and the server's check are the same).
    private static func sign(_ message: String, reason: String, keyData blob: Data, mode: ApprovalAuthMode) async throws -> String {
        let context = LAContext()
        context.localizedReason = reason
        context.localizedCancelTitle = String(localized: "Cancel")
        var interactive = true
        switch mode {
        case .prompt:
            context.touchIDAuthenticationAllowableReuseDuration = 0
        case let .reuseUnlock(canAsk):
            context.touchIDAuthenticationAllowableReuseDuration = LATouchIDAuthenticationMaximumAllowableReuseDuration
            interactive = canAsk
            // In the background: never try to show Face ID or the passcode.
            if !canAsk { context.interactionNotAllowed = true }
        }
        let signature: Data
        do {
            // Face ID first; the passcode when Face ID fails, is locked out or is not set up.
            try await context.evaluatePolicy(.deviceOwnerAuthentication, localizedReason: reason)
            // Off the main thread: the key may still ask.
            signature = try await Task.detached {
                let key = try SecureEnclave.P256.Signing.PrivateKey(dataRepresentation: blob, authenticationContext: context)
                return try key.signature(for: Data(message.utf8)).derRepresentation
            }.value
        } catch let error as LAError {
            throw ApprovalSignError.from(error)
        } catch {
            // The Keychain reports a cancelled prompt as errSecUserCanceled (-128).
            let code = (error as NSError).code
            if code == -128 { throw ApprovalSignError.cancelled }
            // errSecInteractionNotAllowed: the key wanted Face ID or the passcode on screen.
            if code == Int(errSecInteractionNotAllowed) || (!interactive && code == Int(errSecAuthFailed)) {
                throw ApprovalSignError.needsInteraction
            }
            if code == Int(errSecAuthFailed) { throw ApprovalSignError.cancelled }
            throw ApprovalSignError.failed("\(error.localizedDescription) (\(code))")
        }
        return signature.base64EncodedString()
    }
}
