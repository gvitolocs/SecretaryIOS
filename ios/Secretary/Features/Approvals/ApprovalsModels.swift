import Foundation

// Owner approvals (the contract: "Owner approvals: contract v1, 2026-10-28"; server:
// crates/secretary-server/src/approvals.rs). Lucina, the secrets broker or agentd ask the owner for
// permission; this iPhone shows a prompt, and a sensitive "yes" is signed with a Face ID key.
// Everything below is decoded leniently: a field the server adds, drops or mistypes never hides a request.

enum ApprovalRisk: String, Sendable {
    case low, medium, high

    init(raw: String?) {
        self = raw.flatMap { Self(rawValue: $0.lowercased()) } ?? .medium
    }
}

enum ApprovalStatus: String, Sendable {
    case pending, approved, denied, expired

    init?(raw: String?) {
        guard let raw, let value = Self(rawValue: raw.lowercased()) else { return nil }
        self = value
    }
}

enum ApprovalDecision: String, Sendable {
    case approve, deny
}

/// Who is asking: Lucina (`lucina`), the Infisical broker (`broker`), agentd, an MCP client or a device.
struct ApprovalRequester: Equatable, Sendable {
    var kind: String
    var name: String
    var detail: String

    /// The name to show: the requester's own, else its kind.
    var displayName: String {
        if !name.isEmpty { return name }
        return kind.isEmpty ? String(localized: "Something") : kind.capitalized
    }

    var symbol: String {
        switch kind.lowercased() {
        case "lucina": return "sparkles"
        case "broker": return "key"
        case "agentd": return "terminal"
        case "mcp": return "puzzlepiece.extension"
        case "device": return "iphone"
        default: return "questionmark.circle"
        }
    }
}

/// One request, as `GET /v1/approvals`, `GET /v1/approvals/{id}` and the decision answers show it.
struct ApprovalItem: Decodable, Equatable, Identifiable, Sendable {
    let id: String
    var requester: ApprovalRequester
    var action: String
    var summary: String
    var details: String
    var risk: ApprovalRisk
    var sensitive: Bool
    var payloadHash: String
    var createdAt: Date?
    var expiresAt: Date?
    var escalated: Bool
    var youAreMain: Bool
    /// `GET /v1/approvals/{id}` also returns the answered and expired ones.
    var status: ApprovalStatus?
    var decidedBy: String?

    init(id: String, requester: ApprovalRequester = .init(kind: "lucina", name: "Lucina", detail: ""), action: String = "",
         summary: String, details: String = "", risk: ApprovalRisk = .medium, sensitive: Bool = false,
         payloadHash: String = "sha256:0", createdAt: Date? = nil, expiresAt: Date? = nil, escalated: Bool = false,
         youAreMain: Bool = true, status: ApprovalStatus? = nil, decidedBy: String? = nil) {
        self.id = id
        self.requester = requester
        self.action = action
        self.summary = summary
        self.details = details
        self.risk = risk
        self.sensitive = sensitive
        self.payloadHash = payloadHash
        self.createdAt = createdAt
        self.expiresAt = expiresAt
        self.escalated = escalated
        self.youAreMain = youAreMain
        self.status = status
        self.decidedBy = decidedBy
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        if let who = try? c.decodeIfPresent(Requester.self, forKey: .requester) {
            requester = ApprovalRequester(kind: who.kind ?? "", name: who.name ?? "", detail: who.detail ?? "")
        } else if let name = try? c.decodeIfPresent(String.self, forKey: .requester) {
            requester = ApprovalRequester(kind: "", name: name, detail: "")
        } else {
            requester = ApprovalRequester(kind: "", name: "", detail: "")
        }
        action = (try? c.decodeIfPresent(String.self, forKey: .action)) ?? ""
        summary = (try? c.decodeIfPresent(String.self, forKey: .summary)) ?? ""
        details = (try? c.decodeIfPresent(String.self, forKey: .details)) ?? ""
        risk = ApprovalRisk(raw: try? c.decodeIfPresent(String.self, forKey: .risk))
        // A request whose sensitivity is unknown is treated as sensitive: it can only be approved with the proof.
        sensitive = (try? c.decodeIfPresent(Bool.self, forKey: .sensitive)) ?? true
        payloadHash = (try? c.decodeIfPresent(String.self, forKey: .payloadHash)) ?? ""
        createdAt = Self.date(c, .createdAt)
        expiresAt = Self.date(c, .expiresAt)
        escalated = (try? c.decodeIfPresent(Bool.self, forKey: .escalated)) ?? false
        youAreMain = (try? c.decodeIfPresent(Bool.self, forKey: .youAreMain)) ?? false
        status = ApprovalStatus(raw: try? c.decodeIfPresent(String.self, forKey: .status))
        decidedBy = (try? c.decodeIfPresent(String.self, forKey: .decidedBy)).flatMap { $0 }
    }

    private struct Requester: Decodable {
        let kind: String?
        let name: String?
        let detail: String?
    }

    private enum CodingKeys: String, CodingKey {
        case id, requester, action, summary, details, risk, sensitive, payloadHash, createdAt, expiresAt
        case escalated, youAreMain, status, decidedBy
    }

    /// RFC 3339 (the server writes nanoseconds), or epoch seconds/milliseconds.
    private static func date(_ c: KeyedDecodingContainer<CodingKeys>, _ key: CodingKeys) -> Date? {
        if let text = (try? c.decodeIfPresent(String.self, forKey: key)) ?? nil { return QrSignInInfo.date(text) }
        if let number = (try? c.decodeIfPresent(Double.self, forKey: key)) ?? nil {
            return Date(timeIntervalSince1970: number > 1e11 ? number / 1000 : number)
        }
        return nil
    }

    /// The text signed for a decision (the contract: ASCII bytes of this exact string).
    func signingString(for decision: ApprovalDecision) -> String {
        ApprovalProof.signingString(id: id, payloadHash: payloadHash, decision: decision)
    }

    /// What the card shows when a request could not be loaded: nothing to answer (no payload hash).
    static func placeholder(id: String) -> ApprovalItem {
        ApprovalItem(id: id, summary: "", payloadHash: "")
    }

    /// Approve and Deny need the request's payload hash (the error placeholder has none).
    var canAnswer: Bool { !payloadHash.isEmpty }

    func isExpired(at now: Date) -> Bool {
        guard let expiresAt else { return false }
        return expiresAt <= now
    }
}

/// The list answer: a bare array, or an object holding the array (`approvals`, `items`, `requests`, `data`).
struct ApprovalList: Decodable, Sendable {
    let items: [ApprovalItem]
    /// The server's version of this list; sent back as `since` to long-poll until it changes (nil: an older server).
    var cursor: String?

    init(items: [ApprovalItem], cursor: String? = nil) {
        self.items = items
        self.cursor = cursor
    }

    init(from decoder: Decoder) throws {
        if var array = try? decoder.unkeyedContainer() {
            items = Self.drain(&array)
            return
        }
        let c = try decoder.container(keyedBy: AnyKey.self)
        if let key = AnyKey(stringValue: "cursor") { cursor = try? c.decodeIfPresent(String.self, forKey: key) }
        for name in ["approvals", "items", "requests", "data", "pending"] {
            if let key = AnyKey(stringValue: name), c.contains(key), var array = try? c.nestedUnkeyedContainer(forKey: key) {
                items = Self.drain(&array)
                return
            }
        }
        items = []
    }

    private static func drain(_ container: inout UnkeyedDecodingContainer) -> [ApprovalItem] {
        var out: [ApprovalItem] = []
        while !container.isAtEnd {
            if let item = try? container.decode(ApprovalItem.self) {
                out.append(item)
            } else {
                _ = try? container.decode(JSONValue.self)
            }
        }
        return out
    }
}

/// One item, bare or wrapped as `{"approval": {…}}`.
struct ApprovalEnvelope: Decodable, Sendable {
    let item: ApprovalItem

    init(from decoder: Decoder) throws {
        if let direct = try? ApprovalItem(from: decoder) {
            item = direct
            return
        }
        let c = try decoder.container(keyedBy: AnyKey.self)
        for name in ["approval", "item", "request", "data"] {
            if let key = AnyKey(stringValue: name), let nested = try? c.decode(ApprovalItem.self, forKey: key) {
                item = nested
                return
            }
        }
        throw DecodingError.dataCorruptedError(in: try decoder.singleValueContainer(), debugDescription: "no approval in the answer")
    }
}

struct AnyKey: CodingKey {
    var stringValue: String
    var intValue: Int? { nil }
    init?(stringValue: String) { self.stringValue = stringValue }
    init?(intValue: Int) { nil }
}

/// The biometric proof of a sensitive approval: `keyId` of the registered key and a base64 DER ECDSA signature.
struct ApprovalProof: Codable, Equatable, Sendable {
    let keyId: String
    let signature: String

    /// `secretary-approval-v1\n{id}\n{payloadHash}\n{decision}`, signed as ASCII bytes.
    static func signingString(id: String, payloadHash: String, decision: ApprovalDecision) -> String {
        "secretary-approval-v1\n\(id)\n\(payloadHash)\n\(decision.rawValue)"
    }
}

// MARK: - Settings

struct ApprovalDevice: Decodable, Equatable, Identifiable, Sendable {
    let id: String
    var name: String
    var isMain: Bool
    var hasBiometricKey: Bool
    /// The device's unlock key (one-tap Approve) is enrolled.
    var hasUnlockKey: Bool

    init(id: String, name: String, isMain: Bool = false, hasBiometricKey: Bool = false, hasUnlockKey: Bool = false) {
        self.id = id
        self.name = name
        self.isMain = isMain
        self.hasBiometricKey = hasBiometricKey
        self.hasUnlockKey = hasUnlockKey
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        name = (try? c.decodeIfPresent(String.self, forKey: .name)) ?? ""
        isMain = (try? c.decodeIfPresent(Bool.self, forKey: .isMain)) ?? false
        hasBiometricKey = (try? c.decodeIfPresent(Bool.self, forKey: .hasBiometricKey)) ?? false
        hasUnlockKey = (try? c.decodeIfPresent(Bool.self, forKey: .hasUnlockKey)) ?? false
    }

    private enum CodingKeys: String, CodingKey { case id, name, isMain, hasBiometricKey, hasUnlockKey }
}

struct ApprovalSettings: Decodable, Equatable, Sendable {
    var mainDeviceId: String?
    var escalateAfterSecs: Int
    var devices: [ApprovalDevice]
    var thisDeviceId: String?
    /// A 6-digit approval PIN is set (the fallback when Face ID isn't available).
    var hasPin: Bool

    init(mainDeviceId: String? = nil, escalateAfterSecs: Int = 30, devices: [ApprovalDevice] = [], thisDeviceId: String? = nil, hasPin: Bool = false) {
        self.hasPin = hasPin
        self.mainDeviceId = mainDeviceId
        self.escalateAfterSecs = escalateAfterSecs
        self.devices = devices
        self.thisDeviceId = thisDeviceId
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        mainDeviceId = (try? c.decodeIfPresent(String.self, forKey: .mainDeviceId)).flatMap { $0 }
        escalateAfterSecs = (try? c.decodeIfPresent(Int.self, forKey: .escalateAfterSecs)) ?? 30
        devices = (try? c.decodeIfPresent(LossyArray<ApprovalDevice>.self, forKey: .devices))?.values ?? []
        thisDeviceId = (try? c.decodeIfPresent(String.self, forKey: .thisDeviceId)).flatMap { $0 }
        hasPin = (try? c.decodeIfPresent(Bool.self, forKey: .hasPin)) ?? false
    }

    private enum CodingKeys: String, CodingKey { case mainDeviceId, escalateAfterSecs, devices, thisDeviceId, hasPin }

    var thisDevice: ApprovalDevice? { devices.first { $0.id == thisDeviceId } }

    /// This iPhone is the main device for approvals.
    var thisIsMain: Bool {
        if let thisDeviceId, let mainDeviceId { return thisDeviceId == mainDeviceId }
        return thisDevice?.isMain ?? false
    }

    var serverHasKeyForThisDevice: Bool { thisDevice?.hasBiometricKey ?? false }
    var serverHasUnlockKeyForThisDevice: Bool { thisDevice?.hasUnlockKey ?? false }
}
