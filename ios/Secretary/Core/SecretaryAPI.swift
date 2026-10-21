import Foundation

// Wire types and calls for the Lucina-backed routes (session, health, chat, jobs,
// shopping). See docs/server-api.md. Unknown fields are ignored everywhere and
// every optional field is decoded defensively.

struct SessionInfo: Decodable, Equatable, Sendable {
    let authenticated: Bool
    let deviceName: String?

    init(authenticated: Bool, deviceName: String?) {
        self.authenticated = authenticated
        self.deviceName = deviceName
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        authenticated = (try? c.decode(Bool.self, forKey: .authenticated)) ?? false
        deviceName = try? c.decodeIfPresent(String.self, forKey: .deviceName)
    }

    private enum CodingKeys: String, CodingKey { case authenticated, deviceName }
}

struct HealthInfo: Decodable, Equatable, Sendable {
    struct Lucina: Decodable, Equatable, Sendable {
        let reachable: Bool
        let latencyMs: Int?
        let error: String?
        let apiVersion: Int?

        init(reachable: Bool, latencyMs: Int?, error: String?, apiVersion: Int?) {
            self.reachable = reachable
            self.latencyMs = latencyMs
            self.error = error
            self.apiVersion = apiVersion
        }

        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            reachable = (try? c.decode(Bool.self, forKey: .reachable)) ?? false
            latencyMs = (try? c.decodeIfPresent(Double.self, forKey: .latencyMs)).map { Int($0.rounded()) }
            error = try? c.decodeIfPresent(String.self, forKey: .error)
            apiVersion = try? c.decodeIfPresent(Int.self, forKey: .apiVersion)
        }

        private enum CodingKeys: String, CodingKey { case reachable, latencyMs, error, apiVersion }
    }

    let ok: Bool
    let version: String?
    let lucina: Lucina?

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        ok = (try? c.decode(Bool.self, forKey: .ok)) ?? false
        version = try? c.decodeIfPresent(String.self, forKey: .version)
        lucina = try? c.decodeIfPresent(Lucina.self, forKey: .lucina)
    }

    private enum CodingKeys: String, CodingKey { case ok, version, lucina }
}

struct ChatRequest: Encodable, Equatable, Sendable {
    let message: String
    let sessionId: String
    let clientMessageId: String
}

struct JobRef: Codable, Equatable, Sendable {
    let id: String
    let status: String

    init(id: String, status: String) {
        self.id = id
        self.status = status
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(FlexibleID.self, forKey: .id).value
        status = (try? c.decodeIfPresent(String.self, forKey: .status)) ?? "queued"
    }

    private enum CodingKeys: String, CodingKey { case id, status }
}

struct ChatResponse: Decodable, Equatable, Sendable {
    let reply: String
    /// `reply`, `grocery`, `list_reminders`, `schedule_reminder`, an async action, or anything
    /// new (treated like `reply`).
    let action: String?
    let job: JobRef?
    let sessionId: String?
    let duplicate: Bool

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        reply = (try? c.decodeIfPresent(String.self, forKey: .reply)) ?? ""
        action = try? c.decodeIfPresent(String.self, forKey: .action)
        job = try? c.decodeIfPresent(JobRef.self, forKey: .job)
        sessionId = try? c.decodeIfPresent(String.self, forKey: .sessionId)
        duplicate = (try? c.decodeIfPresent(Bool.self, forKey: .duplicate)) ?? false
    }

    private enum CodingKeys: String, CodingKey { case reply, action, job, sessionId, duplicate }
}

struct JobInfo: Decodable, Equatable, Sendable {
    let id: String?
    /// `queued`, `running`, `done` or `error`.
    let status: String
    let action: String?
    let result: JSONValue?
    let error: String?
    let imageUrl: String?

    var isTerminal: Bool { JobInfo.isTerminal(status) }

    static func isTerminal(_ status: String) -> Bool { status == "done" || status == "error" }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = (try? c.decodeIfPresent(FlexibleID.self, forKey: .id))?.value
        status = (try? c.decodeIfPresent(String.self, forKey: .status)) ?? "queued"
        action = try? c.decodeIfPresent(String.self, forKey: .action)
        result = try? c.decodeIfPresent(JSONValue.self, forKey: .result)
        error = try? c.decodeIfPresent(String.self, forKey: .error)
        imageUrl = try? c.decodeIfPresent(String.self, forKey: .imageUrl)
    }

    private enum CodingKeys: String, CodingKey { case id, status, action, result, error, imageUrl }
}

struct DeviceLoginRequest: Encodable, Equatable, Sendable {
    let email: String
    let password: String
    let deviceName: String
}

/// `POST /v1/auth/device`: the device token to keep in the Keychain and send as Bearer.
struct DeviceToken: Decodable, Equatable, Sendable {
    let token: String
    let deviceId: String?
    let deviceName: String?

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        token = try c.decode(String.self, forKey: .token)
        deviceId = (try? c.decodeIfPresent(FlexibleID.self, forKey: .deviceId))?.value
        deviceName = try? c.decodeIfPresent(String.self, forKey: .deviceName)
    }

    private enum CodingKeys: String, CodingKey { case token, deviceId, deviceName }
}

struct ChangePasswordRequest: Encodable, Equatable, Sendable {
    let currentPassword: String
    let newPassword: String
}

/// The Lucina-backed calls. Gym and body live in their own transports.
struct SecretaryAPI: Sendable {
    let client: APIClient

    /// Exchanges the owner's email and password for a device token. Sent without a
    /// Bearer even if one is set (a stale token must not decide the answer).
    func login(_ request: DeviceLoginRequest) async throws -> DeviceToken {
        let body = try APIClient.encoder.encode(request)
        let response = try await client.send("POST", "/v1/auth/device", body: body, authenticated: false)
        return try client.decode(DeviceToken.self, from: response)
    }

    func changePassword(_ request: ChangePasswordRequest) async throws {
        let body = try APIClient.encoder.encode(request)
        _ = try await client.send("POST", "/v1/session/password", body: body)
    }

    func session() async throws -> SessionInfo {
        try await client.get("/v1/session")
    }

    func health() async throws -> HealthInfo {
        try await client.get("/v1/health", timeout: 10)
    }

    func chat(_ request: ChatRequest) async throws -> ChatResponse {
        try await client.send("POST", "/v1/chat", body: request, timeout: APIClient.chatTimeout, idempotent: true)
    }

    func job(_ id: String) async throws -> JobInfo {
        let escaped = id.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed.subtracting(CharacterSet(charactersIn: "/"))) ?? id
        return try await client.get("/v1/jobs/\(escaped)")
    }

    func shoppingList() async throws -> [ShoppingItem] {
        let value: JSONValue = try await client.get("/v1/shopping")
        return ShoppingItem.list(from: value)
    }

    func addShopping(rawName: String) async throws -> ShoppingItem? {
        struct Body: Encodable { let rawName: String }
        let value: JSONValue = try await client.send("POST", "/v1/shopping", body: Body(rawName: rawName))
        return ShoppingItem(json: value["item"] ?? value, index: 0)
    }

    func setShoppingStatus(id: String, status: String) async throws -> ShoppingItem? {
        struct Body: Encodable { let status: String }
        let escaped = id.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed.subtracting(CharacterSet(charactersIn: "/"))) ?? id
        let value: JSONValue = try await client.send("PATCH", "/v1/shopping/\(escaped)", body: Body(status: status))
        return ShoppingItem(json: value["item"] ?? value, index: 0)
    }
}
