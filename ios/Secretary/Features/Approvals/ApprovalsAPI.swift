import Foundation

/// `/v1/approvals/*` with this device's token (Bearer: no CSRF header needed).
struct ApprovalsAPI: Sendable {
    let client: APIClient

    /// The requests this device may decide now, newest first.
    func pending() async throws -> [ApprovalItem] {
        let list: ApprovalList = try await client.get("/v1/approvals", query: [URLQueryItem(name: "status", value: "pending")])
        return list.items
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
    }

    private struct DecideAnswer: Decodable { let status: String? }

    /// Approve or deny. A sensitive approve carries `proof`. Returns the status the server reports.
    @discardableResult
    func decide(_ id: String, decision: ApprovalDecision, payloadHash: String, proof: ApprovalProof? = nil) async throws -> ApprovalStatus? {
        // Never retried on another endpoint after an ambiguous failure: the answer may have arrived.
        let answer: DecideAnswer = try await client.send("POST", "/v1/approvals/\(Self.segment(id))/decide",
                                                         body: DecideBody(decision: decision.rawValue, payloadHash: payloadHash, proof: proof))
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

    private struct RegisterBody: Encodable {
        let publicKey: String
        let label: String
        let password: String
    }

    private struct RegisterAnswer: Decodable { let keyId: String? }

    /// Registers this device's Face ID key. The owner's password is required, so a stolen device token cannot add its own key.
    func registerKey(publicKey: Data, label: String, password: String) async throws -> String {
        let answer: RegisterAnswer = try await client.send("POST", "/v1/approvals/keys",
                                                           body: RegisterBody(publicKey: publicKey.base64EncodedString(), label: label, password: password))
        guard let keyId = answer.keyId, !keyId.isEmpty else {
            throw APIError.decoding(message: "The server did not return a key id.")
        }
        return keyId
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
    /// A sensitive approve needs the Face ID key, and this iPhone has none (or the server refused it).
    case needsSetup
    case failed(String)

    var isFinal: Bool {
        switch self {
        case .approved, .denied, .alreadyDecided, .expired, .gone: return true
        case .needsSetup, .failed: return false
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
        case (403, "not_main_device"):
            return .failed(String(localized: "Your main device answers first. This iPhone can answer in a few seconds."))
        case (403, "self_approval"):
            return .failed(String(localized: "A device can't approve its own request."))
        default:
            return .failed(e.userMessage)
        }
    }
}
