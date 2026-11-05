import CryptoKit
import Foundation
import LocalAuthentication
import Security
import os

/// Where a signature's failure is logged (its error domain and code; never the message or the key).
private let signLog = Logger(subsystem: "tech.getsecretary.app", category: "Approvals")

/// How every signature's LocalAuthentication context is set: Face ID (or the passcode) asked now, on screen. A Face
/// ID unlock of the iPhone is never reused (`touchIDAuthenticationAllowableReuseDuration` 0): the Secure Enclave key
/// (`.privateKeyUsage, .userPresence`) refuses such a context (CryptoTokenKit -5, seen on an iPhone 16e, iOS 27.2).
enum ApprovalSigningContext {
    static func configure(_ context: LAContext) {
        context.touchIDAuthenticationAllowableReuseDuration = 0
        context.interactionNotAllowed = false
    }
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
    /// Signs `message` (ASCII bytes) after Face ID or the passcode, asked now on the card. `reason` is what the sheet says.
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
    /// Face ID or the key refused the signature for another reason (its error domain and code): nothing was sent; the
    /// card says so, and Approve there tries again.
    case notSigned(domain: String, code: Int)
    case failed(String)

    var userMessage: String {
        switch self {
        case .noKey: return String(localized: "Face ID or passcode isn't set up for approvals on this iPhone.")
        case .cancelled: return ""
        case .needsDeviceSecurity:
            return String(localized: "Set up Face ID in Settings → Face ID & Passcode, or approve with your passcode.")
        case .unavailable: return String(localized: "This iPhone can't keep an approval key.")
        case .notSigned: return String(localized: "Face ID didn't confirm it. Nothing was sent. Tap Approve to try again.")
        case let .failed(message): return message
        }
    }

    /// What a LocalAuthentication failure means for the owner.
    static func from(_ error: LAError) -> ApprovalSignError {
        switch error.code {
        case .userCancel, .appCancel, .systemCancel: return .cancelled
        case .biometryNotEnrolled, .biometryLockout, .passcodeNotSet, .biometryNotAvailable: return .needsDeviceSecurity
        default: return .notSigned(domain: LAError.errorDomain, code: error.code.rawValue)
        }
    }

    /// What a Secure Enclave / Keychain failure means: errSecUserCanceled (-128) is a cancel, anything else was not
    /// signed.
    static func from(keyError error: NSError) -> ApprovalSignError {
        error.code == -128 ? .cancelled : .notSigned(domain: error.domain, code: error.code)
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

    func sign(_ message: String, reason: String) async throws -> ApprovalProof {
        guard let stored = stored(), let blob = Data(base64Encoded: stored.data) else { throw ApprovalSignError.noKey }
        let signature = try await Self.sign(message, reason: reason, keyData: blob)
        return ApprovalProof(keyId: stored.keyId, signature: signature)
    }

    func signWithPreparedKey(_ message: String, reason: String) async throws -> String {
        guard let key = lock.withLock({ prepared }) else { throw ApprovalSignError.noKey }
        return try await Self.sign(message, reason: reason, keyData: key.dataRepresentation)
    }

    /// Face ID, or the passcode, then the Secure Enclave signature (base64 DER) over `message`. Asked once, here,
    /// when the owner tapped Approve on the card (the app in front).
    private static func sign(_ message: String, reason: String, keyData blob: Data) async throws -> String {
        let context = LAContext()
        context.localizedReason = reason
        context.localizedCancelTitle = String(localized: "Cancel")
        ApprovalSigningContext.configure(context)
        let signature: Data
        do {
            // Face ID first; the passcode when Face ID fails, is locked out or is not set up.
            DebugEvents.record("auth.prompt", ["signer": "face_id", "policy": "device_owner"])
            try await context.evaluatePolicy(.deviceOwnerAuthentication, localizedReason: reason)
            DebugEvents.record("auth.result", ["signer": "face_id", "ok": true])
            // Off the main thread: the key may still ask.
            signature = try await Task.detached {
                let key = try SecureEnclave.P256.Signing.PrivateKey(dataRepresentation: blob, authenticationContext: context)
                return try key.signature(for: Data(message.utf8)).derRepresentation
            }.value
        } catch let error as LAError {
            DebugEvents.record("auth.result", ["signer": "face_id", "ok": false, "error": error, "cancelled": [LAError.Code.userCancel, .appCancel, .systemCancel].contains(error.code)])
            signLog.info("sign error \(LAError.errorDomain, privacy: .public) \(error.code.rawValue, privacy: .public)")
            throw ApprovalSignError.from(error)
        } catch {
            let e = error as NSError
            DebugEvents.record("auth.result", ["signer": "face_id", "stage": "sign", "ok": false, "error": error, "cancelled": e.code == -128])
            signLog.info("sign error \(e.domain, privacy: .public) \(e.code, privacy: .public)")
            throw ApprovalSignError.from(keyError: e)
        }
        return signature.base64EncodedString()
    }
}

/// The unlock key's proof: one-tap Approve from a notification, which can't show the Face ID key's own prompt (iOS
/// runs the action in the background after asking Face ID or the passcode to unlock). A separate protocol, so the
/// in-place approve can only ever reach this key, never the Face ID one.
protocol UnlockApprovalSigning: Sendable {
    /// The id the server gave this iPhone's unlock key, when it has one.
    var keyId: String? { get }
    /// Makes a new Secure Enclave key (kept in memory, not saved) and returns its X9.63 public key.
    func prepareKey() throws -> Data
    /// Saves the prepared key once the server enrolled it under `keyId`.
    func commitKey(keyId: String) throws
    func removeKey() throws
    /// Signs `message` (ASCII bytes) at once: no prompt, the iPhone just has to be unlocked.
    func sign(_ message: String) throws -> ApprovalProof
}

/// No unlock key (fixtures, and stores that never approve in place).
struct NoUnlockApprovalSigner: UnlockApprovalSigning {
    var keyId: String? { nil }
    func prepareKey() throws -> Data { throw ApprovalSignError.unavailable }
    func commitKey(keyId: String) throws { throw ApprovalSignError.noKey }
    func removeKey() throws {}
    func sign(_ message: String) throws -> ApprovalProof { throw ApprovalSignError.noKey }
}

/// A P-256 key in the Secure Enclave usable whenever this iPhone is unlocked (`kSecAttrAccessibleWhenUnlocked
/// ThisDeviceOnly`, `.privateKeyUsage` only: no Face ID or passcode at signing time). The owner chose this trade-off
/// for the notification's one-tap Approve: anyone who can unlock the iPhone can approve with one tap. The server
/// enrolls it only with the Face ID key's proof, and never accepts it for adding approver devices. Stored like the
/// Face ID key (its opaque `dataRepresentation` and the server's key id in the Keychain).
final class UnlockApprovalSigner: UnlockApprovalSigning, @unchecked Sendable {
    static let store = KeychainStore(service: "tech.getsecretary.app", account: "approval-unlock-key")
    static let keyVersion = 1

    static func accessControl() -> SecAccessControl? {
        var error: Unmanaged<CFError>?
        let access = SecAccessControlCreateWithFlags(nil, kSecAttrAccessibleWhenUnlockedThisDeviceOnly, [.privateKeyUsage], &error)
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

    init(store: KeychainStore = UnlockApprovalSigner.store) {
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
        guard SecureEnclave.isAvailable, let access = Self.accessControl() else { throw ApprovalSignError.unavailable }
        do {
            let key = try SecureEnclave.P256.Signing.PrivateKey(accessControl: access)
            lock.withLock { prepared = key }
            return key.publicKey.x963Representation
        } catch {
            let code = (error as NSError).code
            throw ApprovalSignError.failed("\(error.localizedDescription) (\(code))")
        }
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

    func sign(_ message: String) throws -> ApprovalProof {
        guard let stored = stored(), let blob = Data(base64Encoded: stored.data) else { throw ApprovalSignError.noKey }
        do {
            let key = try SecureEnclave.P256.Signing.PrivateKey(dataRepresentation: blob)
            let signature = try key.signature(for: Data(message.utf8)).derRepresentation
            return ApprovalProof(keyId: stored.keyId, signature: signature.base64EncodedString())
        } catch {
            let e = error as NSError
            DebugEvents.record("auth.result", ["signer": "unlock", "stage": "sign", "ok": false, "error": error])
            signLog.info("unlock sign error \(e.domain, privacy: .public) \(e.code, privacy: .public)")
            throw ApprovalSignError.notSigned(domain: e.domain, code: e.code)
        }
    }
}
