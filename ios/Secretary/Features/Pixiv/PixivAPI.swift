import Foundation

/// `/v1/pixiv/*` with this device's token (Bearer: no CSRF header needed). Answers are `{ok, data}`.
struct PixivAPI: Sendable {
    let client: APIClient

    func status() async throws -> PixivStatus {
        try await client.get("/v1/pixiv/status", timeout: 20, unwrap: true)
    }

    /// Makes the PKCE login URL (the verifier stays on the server).
    func connectStart() async throws -> PixivConnectStart {
        try await client.send("POST", "/v1/pixiv/connect/start", body: EmptyBody(), unwrap: true)
    }

    private struct EmptyBody: Encodable {}

    private struct FinishBody: Encodable {
        let state: String
        let code: String
    }

    /// Hands pixiv's one-time code to the server at once (it lives well under a minute). The code is never logged.
    func connectFinish(state: String, code: String) async throws -> PixivConnection {
        try await client.send("POST", "/v1/pixiv/connect/finish", body: FinishBody(state: state, code: code), timeout: 45, unwrap: true)
    }

    func disconnect() async throws -> PixivConnection {
        try await client.send("POST", "/v1/pixiv/disconnect", body: EmptyBody(), unwrap: true)
    }

    private struct SettingsBody: Encodable {
        let defaultContent: String
        let defaultSort: String
    }

    func saveSettings(_ settings: PixivSettings) async throws -> PixivSettings {
        try await client.send("PUT", "/v1/pixiv/settings",
                              body: SettingsBody(defaultContent: settings.defaultContent.rawValue, defaultSort: settings.defaultSort.rawValue),
                              unwrap: true)
    }
}

// MARK: - Answer envelope

/// `{ok: true, data}`. A body without the envelope (`ok` missing) is read as the data itself.
private struct PixivEnvelope<T: Decodable>: Decodable {
    let value: T

    private enum CodingKeys: String, CodingKey { case ok, data }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        if let ok = try c.decodeIfPresent(Bool.self, forKey: .ok) {
            guard ok else { throw APIError.server(status: 200, code: "pixiv_not_ok", message: "") }
            value = try c.decode(T.self, forKey: .data)
        } else if c.contains(.data) {
            value = try c.decode(T.self, forKey: .data)
        } else {
            value = try T(from: decoder)
        }
    }
}

private extension APIClient {
    func get<T: Decodable>(_ path: String, timeout: TimeInterval, unwrap: Bool) async throws -> T {
        let envelope: PixivEnvelope<T> = try await get(path, timeout: timeout)
        return envelope.value
    }

    func send<T: Decodable, B: Encodable>(_ method: String, _ path: String, body: B, timeout: TimeInterval = APIClient.defaultTimeout,
                                          unwrap: Bool) async throws -> T {
        let envelope: PixivEnvelope<T> = try await send(method, path, body: body, timeout: timeout)
        return envelope.value
    }
}

// MARK: - Models

struct PixivConnectStart: Decodable, Equatable, Sendable {
    let loginUrl: String
    let state: String
    let expiresInSecs: Int?

    /// Only pixiv's own login page may be opened.
    var url: URL? {
        guard let url = URL(string: loginUrl), url.scheme?.lowercased() == "https",
              url.host?.lowercased() == "app-api.pixiv.net" else { return nil }
        return url
    }
}

struct PixivUser: Decodable, Equatable, Sendable {
    var id: String?
    var name: String?
    var account: String?

    init(id: String? = nil, name: String? = nil, account: String? = nil) {
        self.id = id; self.name = name; self.account = account
    }

    private enum CodingKeys: String, CodingKey { case id, name, account }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        // The id may be a number or a string.
        if let s = try? c.decodeIfPresent(String.self, forKey: .id) {
            id = s
        } else if let n = try? c.decodeIfPresent(Int64.self, forKey: .id) {
            id = String(n)
        } else {
            id = nil
        }
        name = (try? c.decodeIfPresent(String.self, forKey: .name)) ?? nil
        account = (try? c.decodeIfPresent(String.self, forKey: .account)) ?? nil
    }
}

struct PixivConnection: Decodable, Equatable, Sendable {
    enum State: String, Sendable { case disconnected, connected, expired, unavailable }
    enum Progress: String, Sendable {
        case saved
        case waitingApproval = "waiting_approval"
    }

    var state: State = .unavailable
    var user: PixivUser?
    var premium: Bool?
    var r18: Bool?
    var r18g: Bool?
    /// The refresh token in the secrets store.
    var store: Progress?
    /// A Disconnect's delete waiting for Face ID (only `waiting_approval` exists).
    var removal: Progress?
    /// The server's error code, kept as text so a code this app doesn't know yet still arrives.
    var error: String?
    var connectedAt: String?

    init(state: State = .unavailable, user: PixivUser? = nil, premium: Bool? = nil, r18: Bool? = nil, r18g: Bool? = nil,
         store: Progress? = nil, removal: Progress? = nil, error: String? = nil, connectedAt: String? = nil) {
        self.state = state; self.user = user; self.premium = premium; self.r18 = r18; self.r18g = r18g
        self.store = store; self.removal = removal; self.error = error; self.connectedAt = connectedAt
    }

    private enum CodingKeys: String, CodingKey { case state, user, premium, r18, r18g, store, removal, error, connectedAt }

    /// Every field is optional; an unknown or missing `state` means unavailable.
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let rawState = (try? c.decodeIfPresent(String.self, forKey: .state)) ?? nil
        state = rawState.flatMap(State.init(rawValue:)) ?? .unavailable
        user = (try? c.decodeIfPresent(PixivUser.self, forKey: .user)) ?? nil
        premium = (try? c.decodeIfPresent(Bool.self, forKey: .premium)) ?? nil
        r18 = (try? c.decodeIfPresent(Bool.self, forKey: .r18)) ?? nil
        r18g = (try? c.decodeIfPresent(Bool.self, forKey: .r18g)) ?? nil
        store = ((try? c.decodeIfPresent(String.self, forKey: .store)) ?? nil).flatMap(Progress.init(rawValue:))
        removal = ((try? c.decodeIfPresent(String.self, forKey: .removal)) ?? nil).flatMap(Progress.init(rawValue:))
        error = (try? c.decodeIfPresent(String.self, forKey: .error)) ?? nil
        connectedAt = (try? c.decodeIfPresent(String.self, forKey: .connectedAt)) ?? nil
    }

    /// A save or a removal waits for the owner's Face ID.
    var isWaitingForApproval: Bool { store == .waitingApproval || removal == .waitingApproval }

    /// Disconnect is offered while connected, expired, or while a token is still saved.
    var canDisconnect: Bool { state == .connected || state == .expired || store == .saved }
}

struct PixivSettings: Codable, Equatable, Sendable {
    enum Content: String, CaseIterable, Codable, Sendable { case safe, r18, all }
    enum Sort: String, CaseIterable, Codable, Sendable { case dateDesc = "date_d", popular }

    var defaultContent: Content = .safe
    var defaultSort: Sort = .dateDesc

    init(defaultContent: Content = .safe, defaultSort: Sort = .dateDesc) {
        self.defaultContent = defaultContent; self.defaultSort = defaultSort
    }

    private enum CodingKeys: String, CodingKey { case defaultContent, defaultSort }

    /// Missing or unknown values fall back to the server's defaults (safe, newest).
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        defaultContent = ((try? c.decodeIfPresent(String.self, forKey: .defaultContent)) ?? nil).flatMap(Content.init(rawValue:)) ?? .safe
        defaultSort = ((try? c.decodeIfPresent(String.self, forKey: .defaultSort)) ?? nil).flatMap(Sort.init(rawValue:)) ?? .dateDesc
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(defaultContent, forKey: .defaultContent)
        try c.encode(defaultSort, forKey: .defaultSort)
    }
}

/// `GET /status`: today's fields plus `connection` and `settings` (the rest is ignored).
struct PixivStatus: Decodable, Equatable, Sendable {
    var connection: PixivConnection
    var settings: PixivSettings

    private enum CodingKeys: String, CodingKey { case connection, settings }

    init(connection: PixivConnection = PixivConnection(), settings: PixivSettings = PixivSettings()) {
        self.connection = connection; self.settings = settings
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        connection = (try? c.decodeIfPresent(PixivConnection.self, forKey: .connection)) ?? PixivConnection()
        settings = (try? c.decodeIfPresent(PixivSettings.self, forKey: .settings)) ?? PixivSettings()
    }
}

// MARK: - Callback code

enum PixivCallback {
    /// `pixiv://account/login?code=…&via=login` → the code, only when it is `[A-Za-z0-9._~-]{1,512}`.
    static func code(from url: URL) -> String? {
        guard let items = URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems,
              let code = items.first(where: { $0.name == "code" })?.value else { return nil }
        return isValid(code) ? code : nil
    }

    static func isValid(_ code: String) -> Bool {
        guard (1...512).contains(code.utf8.count) else { return false }
        return code.utf8.allSatisfy { b in
            (b >= 0x30 && b <= 0x39) || (b >= 0x41 && b <= 0x5A) || (b >= 0x61 && b <= 0x7A)
                || b == 0x2E || b == 0x5F || b == 0x7E || b == 0x2D
        }
    }
}

// MARK: - Texts

enum PixivText {
    /// The one-line status of the connection.
    static func status(_ c: PixivConnection) -> String {
        switch c.state {
        case .connected:
            var text = String(localized: "Connected")
            if let identity = identity(c.user) { text = String(localized: "Connected as \(identity)") }
            if let premium = c.premium {
                text += " \u{00B7} " + (premium ? String(localized: "Premium: Yes") : String(localized: "Premium: No"))
            }
            return text
        case .disconnected:
            return String(localized: "Not connected. Without an account Pixiv shows all-ages works only.")
        case .expired:
            return String(localized: "Pixiv no longer accepts the saved sign-in. Connect again.")
        case .unavailable:
            return c.error == "broker_unconfigured" ? String(localized: "Not set up on the server yet.")
                : String(localized: "Can't reach the secrets service right now.")
        }
    }

    /// "Name (@account)"; either part may be missing.
    static func identity(_ user: PixivUser?) -> String? {
        let name = user?.name.flatMap { $0.isEmpty ? nil : $0 }
        let account = user?.account.flatMap { $0.isEmpty ? nil : $0 }
        switch (name, account) {
        case let (name?, account?): return "\(name) (@\(account))"
        case let (name?, nil): return name
        case let (nil, account?): return "@\(account)"
        default: return nil
        }
    }

    /// Lines under the status: what waits for Face ID, and why the last attempt failed.
    static func notes(_ c: PixivConnection) -> [String] {
        var lines: [String] = []
        if c.store == .waitingApproval { lines.append(String(localized: "Saving the sign-in to your secrets: approve with Face ID.")) }
        if c.removal == .waitingApproval { lines.append(String(localized: "Removing the saved sign-in: approve with Face ID.")) }
        if let error = c.error.flatMap(errorText), !lines.contains(error), error != status(c) { lines.append(error) }
        return lines
    }

    static func errorText(_ code: String) -> String? {
        switch code {
        case "approval_denied": return String(localized: "You declined saving it; nothing was stored.")
        case "approval_expired": return String(localized: "The approval timed out; connect again.")
        case "approval_unavailable": return String(localized: "Approvals weren't reachable; try again later.")
        case "save_failed": return String(localized: "Saving failed; connect again.")
        case "remove_denied": return String(localized: "You declined removing it; the saved sign-in stays until you disconnect again.")
        case "remove_failed": return String(localized: "Removing failed; try again.")
        case "token_rejected": return String(localized: "Pixiv no longer accepts the saved sign-in. Connect again.")
        case "broker_unavailable": return String(localized: "Can't reach the secrets service right now.")
        case "broker_unconfigured": return String(localized: "Not set up on the server yet.")
        default: return nil
        }
    }

    /// What a failed Connect says. The server may answer with the code as `error.code` or as the bare `error` text.
    /// `finishing`: the failed call was `connect/finish` (a 502 there is pixiv refusing the code).
    static func connectError(_ error: APIError, finishing: Bool = true) -> String {
        func has(_ code: String) -> Bool {
            if error.code == code { return true }
            if case let .server(_, _, message) = error { return message == code }
            return false
        }
        if has("broker_unconfigured") { return String(localized: "Not set up on the server yet.") }
        if has("pixiv_login_failed") || (finishing && error.status == 502) { return String(localized: "Pixiv didn't accept the sign-in; try again.") }
        if has("connect_expired") { return String(localized: "The sign-in took too long; try again.") }
        if has("busy") || error.status == 409 { return String(localized: "Approve or decline the pending request first.") }
        return String(localized: "Couldn't finish the Pixiv sign-in.")
    }
}
