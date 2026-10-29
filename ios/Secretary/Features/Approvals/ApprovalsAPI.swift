import Foundation

/// `/v1/approvals/*` with this device's token (Bearer: no CSRF header needed).
struct ApprovalsAPI: Sendable {
    let client: APIClient

    /// The requests this device may decide now, newest first.
    func pending() async throws -> [ApprovalItem] {
        try await pending(since: nil, wait: 0).items
    }

    /// With `since` (the last list's cursor) and `wait`, the server holds the request up to `wait` seconds until
    /// the list changes: one request per change or per `wait` instead of a poll every few seconds.
    func pending(since: String?, wait: Int) async throws -> ApprovalList {
        var query = [URLQueryItem(name: "status", value: "pending")]
        if let since, wait > 0 {
            query += [URLQueryItem(name: "since", value: since), URLQueryItem(name: "wait", value: String(wait))]
        }
        return try await client.get("/v1/approvals", query: query,
                                    timeout: wait > 0 ? TimeInterval(wait + 15) : APIClient.defaultTimeout)
    }

    /// One request, also when it was answered or expired.
    func get(_ id: String) async throws -> ApprovalItem {
        let envelope: ApprovalEnvelope = try await client.get("/v1/approvals/\(Self.segment(id))")
        return envelope.item
    }

    private struct DecideBody: Encodable {
        let decision: String
        let payloadHash: String
        let proof: ApprovalProof?
        let pin: String?
    }

    private struct DecideAnswer: Decodable { let status: String? }

    /// Approve or deny. A sensitive approve carries `proof` (Face ID or passcode) or the owner's `pin`. Returns the status the server reports.
    @discardableResult
    func decide(_ id: String, decision: ApprovalDecision, payloadHash: String, proof: ApprovalProof? = nil, pin: String? = nil) async throws -> ApprovalStatus? {
        // Never retried on another endpoint after an ambiguous failure: the answer may have arrived.
        let answer: DecideAnswer = try await client.send("POST", "/v1/approvals/\(Self.segment(id))/decide",
                                                         body: DecideBody(decision: decision.rawValue, payloadHash: payloadHash, proof: proof, pin: pin))
        return ApprovalStatus(raw: answer.status)
    }

    func settings() async throws -> ApprovalSettings {
        try await client.get("/v1/approvals/settings")
    }

    private struct SettingsBody: Encodable {
        let mainDeviceId: String?

        // `null` clears the main device, so the key is always written.
        func encode(to encoder: Encoder) throws {
            var c = encoder.container(keyedBy: CodingKeys.self)
            try c.encode(mainDeviceId, forKey: .mainDeviceId)
        }

        private enum CodingKeys: String, CodingKey { case mainDeviceId }
    }

    /// Sets the main device (this device's id), or clears it with nil.
    func setMainDevice(_ id: String?) async throws {
        let _: JSONValue = try await client.send("PUT", "/v1/approvals/settings", body: SettingsBody(mainDeviceId: id))
    }

    struct SignatureProof: Codable, Equatable, Sendable { let signature: String }

    private struct RegisterBody: Encodable {
        let publicKey: String
        let label: String
        let proof: SignatureProof
    }

    private struct RegisterAnswer: Decodable { let keyId: String? }

    /// What the new key signs to prove it is on this device: `secretary-approval-key-v1\n{thisDeviceId}\n{publicKeyB64}`.
    static func registrationString(deviceId: String, publicKey: String) -> String {
        "secretary-approval-key-v1\n\(deviceId)\n\(publicKey)"
    }

    /// What a registered key signs to change or remove the PIN without the current one.
    static func pinProofString(deviceId: String) -> String {
        "secretary-approval-pin-v1\n\(deviceId)"
    }

    /// Registers this device's Face ID key. No password: the device token authorizes it, and the new key proves
    /// possession by signing `registrationString` (Face ID or the passcode).
    func registerKey(publicKey: String, label: String, signature: String) async throws -> String {
        let answer: RegisterAnswer = try await client.send("POST", "/v1/approvals/keys",
                                                           body: RegisterBody(publicKey: publicKey, label: label, proof: SignatureProof(signature: signature)))
        guard let keyId = answer.keyId, !keyId.isEmpty else {
            throw APIError.decoding(message: "The server did not return a key id.")
        }
        return keyId
    }

    private struct PinBody: Encodable {
        let pin: String?
        let currentPin: String?
        let proof: SignatureProof?
    }

    /// Sets or replaces the 6-digit PIN. With a PIN already set, send `currentPin` or a `proof` from this device's key.
    func setPin(_ pin: String, currentPin: String? = nil, proof: SignatureProof? = nil) async throws {
        let _: JSONValue = try await client.send("PUT", "/v1/approvals/pin", body: PinBody(pin: pin, currentPin: currentPin, proof: proof))
    }

    func deletePin(currentPin: String? = nil, proof: SignatureProof? = nil) async throws {
        let _: JSONValue = try await client.send("DELETE", "/v1/approvals/pin", body: PinBody(pin: nil, currentPin: currentPin, proof: proof))
    }

    func deleteKey() async throws {
        _ = try await client.send("DELETE", "/v1/approvals/keys", body: nil)
    }

    /// A path segment: ids are base64url, but a stray `/` or space must not change the route.
    static func segment(_ id: String) -> String {
        id.addingPercentEncoding(withAllowedCharacters: CharacterSet.alphanumerics.union(CharacterSet(charactersIn: "-._~"))) ?? id
    }
}

/// What a failed answer means for the prompt.
enum ApprovalOutcome: Equatable, Sendable {
    case approved
    case denied
    /// Someone (or another device) answered first.
    case alreadyDecided(ApprovalStatus?, by: String?)
    case expired
    /// The server no longer knows the request.
    case gone
    /// A sensitive approve needs Face ID or a PIN, and this iPhone has neither set up (or the server refused it).
    case needsSetup
    /// Face ID isn't usable here but a PIN is set: the PIN pad takes over.
    case pinRequired
    /// 403 `pin_wrong`: `attemptsLeft` tries before the PIN locks.
    case pinWrong(attemptsLeft: Int?)
    /// 429 `pin_locked`: try again in `minutes`.
    case pinLocked(minutes: Int)
    case failed(String)

    var isFinal: Bool {
        switch self {
        case .approved, .denied, .alreadyDecided, .expired, .gone: return true
        case .needsSetup, .pinRequired, .pinWrong, .pinLocked, .failed: return false
        }
    }

    /// The line under the PIN pad after a refused PIN.
    var pinMessage: String? {
        switch self {
        case let .pinWrong(left?): return left == 1 ? "Wrong PIN, 1 try left" : "Wrong PIN, \(left) tries left"
        case .pinWrong(nil): return "Wrong PIN"
        case let .pinLocked(minutes): return "PIN locked, try again in \(minutes) min"
        default: return nil
        }
    }

    /// 409 `approval_decided`, 410 `approval_expired`, 404, 403 `biometric_required`, ...
    static func from(_ error: Error) -> ApprovalOutcome {
        let e = APIError.wrap(error)
        switch (e.status, e.code) {
        case (409, "payload_mismatch"):
            return .failed(String(localized: "This request changed while you were looking at it. Review it again."))
        case (409, _):
            return .alreadyDecided(nil, by: nil)
        case (410, _):
            return .expired
        case (404, _):
            return .gone
        case (403, "biometric_required"):
            return .needsSetup
        case (403, "pin_wrong"):
            return .pinWrong(attemptsLeft: e.attemptsLeft)
        case (429, "pin_locked"):
            if case let .rateLimited(_, retryAfter, _) = e { return .pinLocked(minutes: max(1, Int((Double(retryAfter ?? 60) / 60).rounded(.up)))) }
            return .pinLocked(minutes: 15)
        case (403, "not_main_device"):
            return .failed(String(localized: "Your main device answers first. This iPhone can answer in a few seconds."))
        case (403, "self_approval"):
            return .failed(String(localized: "A device can't approve its own request."))
        default:
            return .failed(e.userMessage)
        }
    }
}
