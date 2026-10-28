import Foundation

/// `/v1/agents/*` with this device's token (Bearer: no CSRF header needed). secretary-server forwards it to
/// lucina-agentd with `X-Lucina-Origin: device:<id>`, so this iPhone may answer permissions.
struct AgentsAPI: Sendable {
    let client: APIClient

    /// A path segment: ids are `agt_…`, but a stray `/` or space must not change the route.
    static func segment(_ id: String) -> String {
        id.addingPercentEncoding(withAllowedCharacters: CharacterSet.alphanumerics.union(CharacterSet(charactersIn: "-._~"))) ?? id
    }

    private func path(_ id: String, _ tail: String = "") -> String { "/v1/agents/\(Self.segment(id))\(tail)" }

    func list(archived: Bool) async throws -> [AgentListItem] {
        let r: AgentListResponse = try await client.get("/v1/agents/items", query: archived ? [URLQueryItem(name: "archived", value: "true")] : [])
        return r.agents
    }

    func snapshot(_ id: String) async throws -> AgentSnapshot {
        try await client.get(path(id))
    }

    func timeline(_ id: String, afterSeq: Int, limit: Int) async throws -> AgentTimelinePage {
        try await client.get(path(id, "/timeline"), query: [URLQueryItem(name: "afterSeq", value: String(afterSeq)),
                                                            URLQueryItem(name: "limit", value: String(limit))])
    }

    // MARK: Bodies

    struct SendBody: Encodable, Equatable {
        let prompt: String
        /// `steer` redirects the running turn; `interrupt` (the daemon's default) stops it and starts a new one.
        let activeTurnBehavior: String
    }

    struct AnswerBody: Encodable, Equatable {
        let behavior: String
        let message: String?
        let selectedActionId: String?
    }

    static func sendBody(prompt: String, steer: Bool) -> SendBody {
        SendBody(prompt: prompt, activeTurnBehavior: steer ? "steer" : "interrupt")
    }

    /// Allow or deny a request. The request's own allow or deny action is chosen when it lists one.
    static func answerBody(_ permission: AgentPermission, allow: Bool, message: String? = nil) -> AnswerBody {
        let behavior = allow ? "allow" : "deny"
        let action = permission.actions.first { $0.behavior == behavior }
        return AnswerBody(behavior: behavior, message: allow ? nil : (message ?? String(localized: "Denied from the Secretary app")), selectedActionId: action?.id)
    }

    // MARK: Commands

    private struct Disposition: Decodable { let disposition: String? }

    /// Sends a message; "queued" for an external agent (its device takes it), "running"/"started" otherwise.
    @discardableResult
    func send(_ id: String, prompt: String, steer: Bool) async throws -> String? {
        // Never retried on another endpoint after an ambiguous failure: the message may have arrived.
        let r: Disposition = try await client.send("POST", path(id, "/send"), body: Self.sendBody(prompt: prompt, steer: steer))
        return r.disposition
    }

    func stop(_ id: String) async throws {
        _ = try await client.send("POST", path(id, "/stop"), body: nil)
    }

    func archive(_ id: String) async throws {
        _ = try await client.send("POST", path(id, "/archive"), body: nil)
    }

    func unarchive(_ id: String) async throws {
        _ = try await client.send("POST", path(id, "/unarchive"), body: nil)
    }

    func answer(_ id: String, permission: AgentPermission, allow: Bool) async throws {
        let _: JSONValue = try await client.send("POST", path(id, "/permissions/\(Self.segment(permission.id))"),
                                                body: Self.answerBody(permission, allow: allow))
    }

    func diff(workspaceId: String) async throws -> AgentDiff {
        try await client.get("/v1/agents/workspaces/\(Self.segment(workspaceId))/diff", query: [URLQueryItem(name: "mode", value: "all")],
                             timeout: 30)
    }
}
