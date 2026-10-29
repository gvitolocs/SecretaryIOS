import CryptoKit
import Foundation
import LocalAuthentication
import Security

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
    /// Signs `message` (ASCII bytes) after Face ID or the passcode. `reason` is what the sheet says.
    func sign(_ message: String, reason: String) async throws -> ApprovalProof
}

enum ApprovalSignError: Error, Equatable {
    /// No registered key on this iPhone.
    case noKey
    /// The owner cancelled Face ID (or the system did): nothing to report.
    case cancelled
    /// Face ID is not enrolled or locked out, or there is no passcode: what to do about it.
    case needsDeviceSecurity
    case unavailable
    case failed(String)

    var userMessage: String {
        switch self {
        case .noKey: return String(localized: "Approvals with your passcode aren't set up on this iPhone.")
        case .cancelled: return ""
        case .needsDeviceSecurity:
            return String(localized: "Set a passcode in Settings → Face ID & Passcode first.")
        case .unavailable: return String(localized: "This iPhone can't keep an approval key.")
        case let .failed(message): return message
        }
    }

    /// What a LocalAuthentication failure means for the owner.
    static func from(_ error: LAError) -> ApprovalSignError {
        switch error.code {
        case .userCancel, .appCancel, .systemCancel: return .cancelled
        case .biometryNotEnrolled, .biometryLockout, .passcodeNotSet, .biometryNotAvailable: return .needsDeviceSecurity
        default: return .failed(error.localizedDescription)
        }
    }
}

/// A P-256 key in the Secure Enclave, usable only after the owner types the iPhone's passcode
/// (`.devicePasscode`: no Face ID, the owner's choice of 2026-10-29). The key's opaque `dataRepresentation` and
/// the server's key id sit in the Keychain. Keys of earlier versions (1: Face ID only, 2: Face ID or passcode) are
/// ignored, so the next setup replaces them.
final class SecureEnclaveApprovalSigner: ApprovalSigning, @unchecked Sendable {
    static let store = KeychainStore(service: "tech.getsecretary.app", account: "approval-key")
    /// 3: `[.privateKeyUsage, .devicePasscode]`.
    static let keyVersion = 3

    /// The key's access control: the device passcode, nothing else.
    static func accessControl() -> SecAccessControl? {
        var error: Unmanaged<CFError>?
        let access = SecAccessControlCreateWithFlags(
            nil, kSecAttrAccessibleWhenPasscodeSetThisDeviceOnly, [.privateKeyUsage, .devicePasscode], &error)
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

    func sign(_ message: String, reason: String) async throws -> ApprovalProof {
        guard let stored = stored(), let blob = Data(base64Encoded: stored.data) else { throw ApprovalSignError.noKey }
        let signature = try await Self.sign(message, reason: reason, keyData: blob)
        return ApprovalProof(keyId: stored.keyId, signature: signature)
    }

    func signWithPreparedKey(_ message: String, reason: String) async throws -> String {
        guard let key = lock.withLock({ prepared }) else { throw ApprovalSignError.noKey }
        return try await Self.sign(message, reason: reason, keyData: key.dataRepresentation)
    }

    /// The passcode, then the Secure Enclave signature (base64 DER) over `message`. The prompt is the key's own
    /// access control (passcode only), evaluated once, here, when the owner tapped Approve.
    private static func sign(_ message: String, reason: String, keyData blob: Data) async throws -> String {
        guard let access = accessControl() else { throw ApprovalSignError.needsDeviceSecurity }
        let context = LAContext()
        context.localizedReason = reason
        context.localizedCancelTitle = String(localized: "Cancel")
        let signature: Data
        do {
            _ = try await context.evaluateAccessControl(access, operation: .useKeySign, localizedReason: reason)
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
            if code == Int(errSecAuthFailed) { throw ApprovalSignError.cancelled }
            throw ApprovalSignError.failed("\(error.localizedDescription) (\(code))")
        }
        return signature.base64EncodedString()
    }
}
