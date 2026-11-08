import Foundation

// Lucina Agents (docs/design/agents-api.md on server-live): the owner's coding agents on nezopt.
// `/v1/agents/*` is lucina-agentd behind secretary-server; the JSON is camelCase and has Paseo 0.13.2's
// shapes (AgentSnapshot, AgentListItem, TimelineRow, PermissionRequest) plus Lucina's fields.
// Everything is decoded leniently: a field a newer daemon adds, or one of an unknown kind, never
// hides an agent or a row.

// MARK: - Status

enum AgentStatus: String, Sendable {
    case initializing, idle, running, error, closed, unknown

    init(raw: String?) { self = raw.flatMap(AgentStatus.init(rawValue:)) ?? .unknown }
}

/// Lucina's state bucket for lists (`bucket.rs`): the first matching rule wins.
enum AgentBucket: String, Sendable {
    case needsInput = "needs_input"
    case failed, running, attention, done

    init(raw: String?) { self = raw.flatMap(AgentBucket.init(rawValue:)) ?? .done }

    /// 0 = most urgent.
    var priority: Int {
        switch self {
        case .needsInput: return 0
        case .failed: return 1
        case .running: return 2
        case .attention: return 3
        case .done: return 4
        }
    }

    /// `deriveAgentStateBucket`, for an agent whose state arrived as a snapshot.
    static func derive(status: AgentStatus, pendingPermissions: Int, requiresAttention: Bool, attentionReason: String?) -> AgentBucket {
        if pendingPermissions > 0 || attentionReason == "permission" { return .needsInput }
        if status == .error || attentionReason == "error" { return .failed }
        if status == .running { return .running }
        if requiresAttention { return .attention }
        return .done
    }
}

// MARK: - Dates

enum AgentDates {
    static func parse(_ iso: String?) -> Date? {
        guard let iso, !iso.isEmpty else { return nil }
        if let d = try? Date(iso, strategy: Date.ISO8601FormatStyle(includingFractionalSeconds: true)) { return d }
        return try? Date(iso, strategy: Date.ISO8601FormatStyle())
    }

    static func relative(_ iso: String?, now: Date = Date()) -> String? {
        guard let date = parse(iso) else { return nil }
        if abs(now.timeIntervalSince(date)) < 45 { return String(localized: "now") }
        let f = RelativeDateTimeFormatter()
        f.unitsStyle = .abbreviated
        return f.localizedString(for: date, relativeTo: now)
    }
}

// MARK: - List item

struct AgentListItem: Decodable, Identifiable, Equatable, Sendable {
    let id: String
    let shortId: String
    var title: String?
    let provider: String
    var model: String?
    var status: AgentStatus
    let cwd: String
    let createdAt: String
    var updatedAt: String
    var lastUserMessageAt: String?
    var archivedAt: String?
    var requiresAttention: Bool
    var attentionReason: String?
    var bucket: AgentBucket
    var external: Bool
    var host: String?

    init(id: String, shortId: String? = nil, title: String? = nil, provider: String, model: String? = nil, status: AgentStatus,
         cwd: String = "", createdAt: String = "", updatedAt: String = "", lastUserMessageAt: String? = nil, archivedAt: String? = nil,
         requiresAttention: Bool = false, attentionReason: String? = nil, bucket: AgentBucket, external: Bool = false, host: String? = nil) {
        self.id = id
        self.shortId = shortId ?? String(id.prefix(7))
        self.title = title
        self.provider = provider
        self.model = model
        self.status = status
        self.cwd = cwd
        self.createdAt = createdAt
        self.updatedAt = updatedAt
        self.lastUserMessageAt = lastUserMessageAt
        self.archivedAt = archivedAt
        self.requiresAttention = requiresAttention
        self.attentionReason = attentionReason
        self.bucket = bucket
        self.external = external
        self.host = host
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        shortId = c.lenientOptional(String.self, .shortId) ?? String(id.prefix(7))
        title = c.lenientOptional(String.self, .title)
        provider = c.lenientOptional(String.self, .provider) ?? ""
        model = c.lenientOptional(String.self, .model)
        status = AgentStatus(raw: c.lenientOptional(String.self, .status))
        cwd = c.lenientOptional(String.self, .cwd) ?? ""
        createdAt = c.lenientOptional(String.self, .createdAt) ?? ""
        updatedAt = c.lenientOptional(String.self, .updatedAt) ?? ""
        lastUserMessageAt = c.lenientOptional(String.self, .lastUserMessageAt)
        archivedAt = c.lenientOptional(String.self, .archivedAt)
        requiresAttention = c.lenientOptional(Bool.self, .requiresAttention) ?? false
        attentionReason = c.lenientOptional(String.self, .attentionReason)
        bucket = AgentBucket(raw: c.lenientOptional(String.self, .bucket))
        external = c.lenientOptional(Bool.self, .external) ?? false
        host = c.lenientOptional(String.self, .host)
    }

    private enum CodingKeys: String, CodingKey {
        case id, shortId, title, provider, model, status, cwd, createdAt, updatedAt, lastUserMessageAt, archivedAt
        case requiresAttention, attentionReason, bucket, external, host
    }

    /// The title, else the folder the agent works in, else its short id.
    var displayTitle: String {
        if let t = title?.trimmingCharacters(in: .whitespacesAndNewlines), !t.isEmpty { return t }
        let folder = (cwd as NSString).lastPathComponent
        return folder.isEmpty ? shortId : folder
    }

    var isArchived: Bool { archivedAt != nil }

    /// The line under the title: "claude · opus-5-5".
    var providerLine: String {
        [provider, model].compactMap { $0 }.filter { !$0.isEmpty }.joined(separator: " · ")
    }

    /// What the list shows as the agent's state.
    var stateLabel: String {
        switch bucket {
        case .needsInput: return String(localized: "Needs permission")
        case .failed: return String(localized: "Error")
        case .running: return String(localized: "Running")
        case .attention: return String(localized: "Finished")
        case .done:
            switch status {
            case .closed: return String(localized: "Closed")
            case .initializing: return String(localized: "Starting")
            default: return String(localized: "Idle")
            }
        }
    }

    /// The list item an `agent_state` snapshot stands for.
    init(_ s: AgentSnapshot) {
        self.init(id: s.id, title: s.title, provider: s.provider, model: s.model, status: s.status, cwd: s.cwd,
                  createdAt: s.createdAt, updatedAt: s.updatedAt, lastUserMessageAt: s.lastUserMessageAt, archivedAt: s.archivedAt,
                  requiresAttention: s.requiresAttention, attentionReason: s.attentionReason,
                  bucket: AgentBucket.derive(status: s.status, pendingPermissions: s.pendingPermissions.count,
                                             requiresAttention: s.requiresAttention, attentionReason: s.attentionReason),
                  external: s.external, host: s.host)
    }
}

struct AgentListResponse: Decodable, Sendable {
    let agents: [AgentListItem]

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        // One bad item must not hide the list.
        var out: [AgentListItem] = []
        if var items = try? c.nestedUnkeyedContainer(forKey: .agents) {
            while !items.isAtEnd {
                if let a = try? items.decode(AgentListItem.self) { out.append(a) } else { _ = try? items.decode(JSONValue.self) }
            }
        }
        agents = out
    }

    private enum CodingKeys: String, CodingKey { case agents }
}

// MARK: - Snapshot

struct AgentGate: Decodable, Equatable, Sendable {
    /// running | verified | refused | suspicious | unverified
    let verdict: String
    let details: [String]
    let round: Int

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        verdict = c.lenientOptional(String.self, .verdict) ?? "unverified"
        details = c.lenientOptional([String].self, .details) ?? []
        round = c.lenientOptional(Int.self, .round) ?? 0
    }

    private enum CodingKeys: String, CodingKey { case verdict, details, round }

    var label: String {
        switch verdict {
        case "running": return String(localized: "Checks running")
        case "verified": return String(localized: "Verified")
        case "refused": return String(localized: "Refused by the checks")
        case "suspicious": return String(localized: "Suspicious")
        default: return String(localized: "Not verified")
        }
    }
}

struct AgentSnapshot: Decodable, Equatable, Sendable {
    let id: String
    let provider: String
    let cwd: String
    let workspaceId: String?
    let model: String?
    let createdAt: String
    var updatedAt: String
    var lastUserMessageAt: String?
    var status: AgentStatus
    var activeTurn: Bool
    var pendingPermissions: [AgentPermission]
    var title: String?
    var lastError: String?
    var requiresAttention: Bool
    var attentionReason: String?
    var archivedAt: String?
    var gate: AgentGate?
    var external: Bool
    var host: String?

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        provider = c.lenientOptional(String.self, .provider) ?? ""
        cwd = c.lenientOptional(String.self, .cwd) ?? ""
        workspaceId = c.lenientOptional(String.self, .workspaceId)
        model = c.lenientOptional(String.self, .model)
        createdAt = c.lenientOptional(String.self, .createdAt) ?? ""
        updatedAt = c.lenientOptional(String.self, .updatedAt) ?? ""
        lastUserMessageAt = c.lenientOptional(String.self, .lastUserMessageAt)
        status = AgentStatus(raw: c.lenientOptional(String.self, .status))
        activeTurn = c.lenientOptional(JSONValue.self, .activeTurn).map { if case .object = $0 { return true } else { return false } } ?? false
        var pending: [AgentPermission] = []
        if var items = try? c.nestedUnkeyedContainer(forKey: .pendingPermissions) {
            while !items.isAtEnd {
                if let p = try? items.decode(AgentPermission.self) { pending.append(p) } else { _ = try? items.decode(JSONValue.self) }
            }
        }
        pendingPermissions = pending
        title = c.lenientOptional(String.self, .title)
        lastError = c.lenientOptional(String.self, .lastError)
        requiresAttention = c.lenientOptional(Bool.self, .requiresAttention) ?? false
        attentionReason = c.lenientOptional(String.self, .attentionReason)
        archivedAt = c.lenientOptional(String.self, .archivedAt)
        gate = c.lenientOptional(AgentGate.self, .gate)
        external = c.lenientOptional(Bool.self, .external) ?? false
        host = c.lenientOptional(String.self, .host)
    }

    private enum CodingKeys: String, CodingKey {
        case id, provider, cwd, workspaceId, model, createdAt, updatedAt, lastUserMessageAt, status, activeTurn
        case pendingPermissions, title, lastError, requiresAttention, attentionReason, archivedAt, gate, external, host
    }

    var item: AgentListItem { AgentListItem(self) }
}

// MARK: - Permissions

struct AgentPermissionAction: Decodable, Equatable, Hashable, Sendable {
    let id: String
    let label: String
    /// allow | deny
    let behavior: String

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        label = c.lenientOptional(String.self, .label) ?? id
        behavior = c.lenientOptional(String.self, .behavior) ?? "allow"
    }

    private enum CodingKeys: String, CodingKey { case id, label, behavior }
}

/// A pending permission request of an agent (Paseo `AgentPermissionRequestPayload`).
struct AgentPermission: Decodable, Identifiable, Equatable, Sendable {
    let id: String
    let provider: String
    let name: String
    /// tool | plan | question | mode | other
    let kind: String
    let title: String?
    let description: String?
    let input: [String: JSONValue]
    let actions: [AgentPermissionAction]
    /// The owner approval the agentd bridge filed for this request, when it has one.
    let approvalId: String?

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        provider = c.lenientOptional(String.self, .provider) ?? ""
        name = c.lenientOptional(String.self, .name) ?? ""
        kind = c.lenientOptional(String.self, .kind) ?? "tool"
        title = c.lenientOptional(String.self, .title)
        description = c.lenientOptional(String.self, .description)
        input = c.lenientOptional([String: JSONValue].self, .input) ?? [:]
        actions = c.lenientOptional([AgentPermissionAction].self, .actions) ?? []
        approvalId = c.lenientOptional(String.self, .approvalId)
    }

    private enum CodingKeys: String, CodingKey { case id, provider, name, kind, title, description, input, actions, approvalId }

    var headline: String {
        if let t = title?.trimmingCharacters(in: .whitespacesAndNewlines), !t.isEmpty { return t }
        return name
    }

    /// The tool input as the owner reads it: the command or the path when there is one, else the JSON.
    var inputText: String {
        for key in ["command", "file_path", "filePath", "path", "url", "plan", "question"] {
            if let s = input[key]?.nonEmptyString { return s }
        }
        guard !input.isEmpty, let data = try? APIClient.encoder.encode(input), let text = String(data: data, encoding: .utf8) else { return "" }
        return text
    }
}

/// The permission book's view (`permission` events): the same request with its status and, since the approvals
/// bridge, the `approvalId`.
struct AgentPermissionView: Decodable, Equatable, Sendable {
    let id: String
    let agentId: String?
    /// pending | allowed | denied | expired
    let status: String
    let approvalId: String?

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        agentId = c.lenientOptional(String.self, .agentId)
        status = c.lenientOptional(String.self, .status) ?? "pending"
        approvalId = c.lenientOptional(String.self, .approvalId)
    }

    private enum CodingKeys: String, CodingKey { case id, agentId, status, approvalId }
}

// MARK: - Timeline

/// A tool call's detail (Paseo `ToolCallDetail`): the `type` plus the fields that kind carries.
struct AgentToolDetail: Decodable, Equatable, Sendable {
    let type: String
    var command: String?
    var output: String?
    var filePath: String?
    var content: String?
    var unifiedDiff: String?
    var oldString: String?
    var newString: String?
    var query: String?
    var url: String?
    var text: String?
    var label: String?
    var description: String?
    var log: String?

    init(type: String = "unknown") { self.type = type }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        type = c.lenientOptional(String.self, .type) ?? "unknown"
        command = c.lenientOptional(String.self, .command)
        output = c.lenientOptional(String.self, .output)
        filePath = c.lenientOptional(String.self, .filePath)
        content = c.lenientOptional(String.self, .content)
        unifiedDiff = c.lenientOptional(String.self, .unifiedDiff)
        oldString = c.lenientOptional(String.self, .oldString)
        newString = c.lenientOptional(String.self, .newString)
        query = c.lenientOptional(String.self, .query)
        url = c.lenientOptional(String.self, .url)
        text = c.lenientOptional(String.self, .text)
        label = c.lenientOptional(String.self, .label)
        description = c.lenientOptional(String.self, .description)
        log = c.lenientOptional(String.self, .log)
    }

    private enum CodingKeys: String, CodingKey {
        case type, command, output, filePath, content, unifiedDiff, oldString, newString, query, url, text, label, description, log
    }

    /// One line for the collapsed row.
    var summary: String? {
        switch type {
        case "shell": return command
        case "read", "edit", "write": return filePath
        case "search": return query
        case "fetch": return url
        case "sub_agent": return description
        case "plain_text": return label ?? text.map { String($0.prefix(80)) }
        case "plan": return String(localized: "Plan")
        default: return nil
        }
    }

    /// The text shown when the row opens.
    var body: String {
        var parts: [String] = []
        switch type {
        case "shell":
            let out = (output?.isEmpty == false) ? "\n\(output ?? "")" : ""
            if let command { parts.append("$ \(command)\(out)") } else if !out.isEmpty { parts.append(out) }
        case "read":
            if let filePath { parts.append(filePath) }
            if let content, !content.isEmpty { parts.append(content) }
        case "edit":
            if let filePath { parts.append(filePath) }
            if let unifiedDiff, !unifiedDiff.isEmpty {
                parts.append(unifiedDiff)
            } else {
                if let oldString, !oldString.isEmpty { parts.append(oldString.split(separator: "\n", omittingEmptySubsequences: false).map { "- \($0)" }.joined(separator: "\n")) }
                if let newString, !newString.isEmpty { parts.append(newString.split(separator: "\n", omittingEmptySubsequences: false).map { "+ \($0)" }.joined(separator: "\n")) }
            }
        case "write":
            if let filePath { parts.append(filePath) }
            if let content, !content.isEmpty { parts.append(content) }
        case "search":
            if let query { parts.append(query) }
            if let content, !content.isEmpty { parts.append(content) }
        case "fetch":
            if let url { parts.append(url) }
        case "sub_agent":
            if let description { parts.append(description) }
            if let log, !log.isEmpty { parts.append(log) }
        default:
            if let text, !text.isEmpty { parts.append(text) }
        }
        return Self.clip(parts.joined(separator: "\n\n"))
    }

    static let clipLimit = 4000

    static func clip(_ s: String) -> String {
        s.count > clipLimit ? String(s.prefix(clipLimit)) + "\n…" : s
    }
}

struct AgentToolCall: Equatable, Sendable {
    let callId: String
    let name: String
    /// running | completed | failed | canceled
    let status: String
    let detail: AgentToolDetail
    let error: String?
}

struct AgentTodo: Equatable, Sendable {
    let text: String
    let completed: Bool
    let inProgress: Bool
}

enum AgentTimelineItem: Equatable, Sendable {
    case userMessage(text: String)
    case assistantMessage(text: String, messageId: String?)
    case reasoning(text: String)
    case toolCall(AgentToolCall)
    case todo([AgentTodo])
    case error(String)
    /// level: info | warning | error
    case notification(level: String, message: String)
    case compaction(done: Bool)
    case other(type: String)
}

extension AgentTimelineItem: Decodable {
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let type = c.lenientOptional(String.self, .type) ?? ""
        switch type {
        case "user_message":
            self = .userMessage(text: c.lenientOptional(String.self, .text) ?? "")
        case "assistant_message":
            self = .assistantMessage(text: c.lenientOptional(String.self, .text) ?? "", messageId: c.lenientOptional(String.self, .messageId))
        case "reasoning":
            self = .reasoning(text: c.lenientOptional(String.self, .text) ?? "")
        case "tool_call":
            let id = c.lenientOptional(String.self, .callId) ?? UUID().uuidString
            var error: String?
            if let value = c.lenientOptional(JSONValue.self, .error) {
                error = value.nonEmptyString ?? { if case .null = value { return nil } else { return String(localized: "Failed") } }()
            }
            self = .toolCall(AgentToolCall(callId: id, name: c.lenientOptional(String.self, .name) ?? "",
                                           status: c.lenientOptional(String.self, .status) ?? "running",
                                           detail: c.lenientOptional(AgentToolDetail.self, .detail) ?? AgentToolDetail(),
                                           error: error))
        case "todo":
            struct Entry: Decodable {
                let text: String?
                let completed: Bool?
                let status: String?
            }
            let entries = c.lenientOptional([Entry].self, .items) ?? []
            self = .todo(entries.map { AgentTodo(text: $0.text ?? "", completed: $0.completed ?? ($0.status == "completed"), inProgress: $0.status == "in_progress") })
        case "error":
            self = .error(c.lenientOptional(String.self, .message) ?? "")
        case "notification":
            self = .notification(level: c.lenientOptional(String.self, .level) ?? "info", message: c.lenientOptional(String.self, .message) ?? "")
        case "compaction":
            self = .compaction(done: c.lenientOptional(String.self, .status) == "completed")
        default:
            self = .other(type: type)
        }
    }

    private enum CodingKeys: String, CodingKey {
        case type, text, messageId, callId, name, status, detail, error, items, message, level
    }
}

struct AgentTimelineRow: Decodable, Equatable, Sendable {
    let seq: Int
    let timestamp: String
    let item: AgentTimelineItem
    let turnId: String?

    init(seq: Int, timestamp: String = "", item: AgentTimelineItem, turnId: String? = nil) {
        self.seq = seq
        self.timestamp = timestamp
        self.item = item
        self.turnId = turnId
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        seq = try c.decode(Int.self, forKey: .seq)
        timestamp = c.lenientOptional(String.self, .timestamp) ?? ""
        item = try c.decode(AgentTimelineItem.self, forKey: .item)
        turnId = c.lenientOptional(String.self, .turnId)
    }

    private enum CodingKeys: String, CodingKey { case seq, timestamp, item, turnId }
}

struct AgentTimelinePage: Decodable, Sendable {
    let epoch: String
    let rows: [AgentTimelineRow]
    let nextSeq: Int

    init(epoch: String, rows: [AgentTimelineRow], nextSeq: Int) {
        self.epoch = epoch
        self.rows = rows
        self.nextSeq = nextSeq
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        epoch = c.lenientOptional(String.self, .epoch) ?? ""
        var out: [AgentTimelineRow] = []
        if var items = try? c.nestedUnkeyedContainer(forKey: .rows) {
            while !items.isAtEnd {
                if let r = try? items.decode(AgentTimelineRow.self) { out.append(r) } else { _ = try? items.decode(JSONValue.self) }
            }
        }
        rows = out
        nextSeq = c.lenientOptional(Int.self, .nextSeq) ?? out.last?.seq ?? 0
    }

    private enum CodingKeys: String, CodingKey { case epoch, rows, nextSeq }
}

/// What the screen draws: the rows with a tool call's updates folded into one entry and adjacent assistant (or
/// reasoning) chunks joined, like Paseo's own client does.
struct AgentEntry: Identifiable, Equatable, Sendable {
    /// Stable while the entry grows: the seq of its first row.
    let id: Int
    var item: AgentTimelineItem
    var timestamp: String
}

enum AgentTimeline {
    static func entries(_ rows: [AgentTimelineRow]) -> [AgentEntry] {
        var out: [AgentEntry] = []
        var toolIndex: [String: Int] = [:]
        for row in rows {
            switch row.item {
            case let .toolCall(call):
                if let i = toolIndex[call.callId] {
                    out[i].item = row.item
                } else {
                    toolIndex[call.callId] = out.count
                    out.append(AgentEntry(id: row.seq, item: row.item, timestamp: row.timestamp))
                }
            case let .assistantMessage(text, messageId):
                if let last = out.last, case let .assistantMessage(before, beforeId) = last.item, beforeId == messageId {
                    out[out.count - 1].item = .assistantMessage(text: before + text, messageId: messageId)
                } else {
                    out.append(AgentEntry(id: row.seq, item: row.item, timestamp: row.timestamp))
                }
            case let .reasoning(text):
                if let last = out.last, case let .reasoning(before) = last.item {
                    out[out.count - 1].item = .reasoning(text: before + text)
                } else {
                    out.append(AgentEntry(id: row.seq, item: row.item, timestamp: row.timestamp))
                }
            default:
                out.append(AgentEntry(id: row.seq, item: row.item, timestamp: row.timestamp))
            }
        }
        return out
    }
}

// MARK: - Diff

struct AgentDiffLine: Decodable, Equatable, Sendable {
    /// add | remove | context | header
    let kind: String
    let content: String

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        kind = c.lenientOptional(String.self, .type) ?? "context"
        content = c.lenientOptional(String.self, .content) ?? ""
    }

    private enum CodingKeys: String, CodingKey { case type, content }
}

struct AgentDiffHunk: Decodable, Equatable, Sendable {
    let oldStart: Int
    let newStart: Int
    let lines: [AgentDiffLine]

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        oldStart = c.lenientOptional(Int.self, .oldStart) ?? 0
        newStart = c.lenientOptional(Int.self, .newStart) ?? 0
        lines = c.lenientOptional([AgentDiffLine].self, .lines) ?? []
    }

    private enum CodingKeys: String, CodingKey { case oldStart, newStart, lines }
}

struct AgentDiffFile: Decodable, Identifiable, Equatable, Sendable {
    let path: String
    let isNew: Bool
    let isDeleted: Bool
    let additions: Int
    let deletions: Int
    let hunks: [AgentDiffHunk]
    /// ok | too_large | binary
    let status: String
    var id: String { path }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        path = try c.decode(String.self, forKey: .path)
        isNew = c.lenientOptional(Bool.self, .isNew) ?? false
        isDeleted = c.lenientOptional(Bool.self, .isDeleted) ?? false
        additions = c.lenientOptional(Int.self, .additions) ?? 0
        deletions = c.lenientOptional(Int.self, .deletions) ?? 0
        hunks = c.lenientOptional([AgentDiffHunk].self, .hunks) ?? []
        status = c.lenientOptional(String.self, .status) ?? "ok"
    }

    private enum CodingKeys: String, CodingKey { case path, isNew, isDeleted, additions, deletions, hunks, status }
}

struct AgentDiff: Decodable, Equatable, Sendable {
    let files: [AgentDiffFile]
    let diffTooLarge: Bool
    let error: String?

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        var out: [AgentDiffFile] = []
        if var items = try? c.nestedUnkeyedContainer(forKey: .files) {
            while !items.isAtEnd {
                if let f = try? items.decode(AgentDiffFile.self) { out.append(f) } else { _ = try? items.decode(JSONValue.self) }
            }
        }
        files = out
        diffTooLarge = c.lenientOptional(Bool.self, .diffTooLarge) ?? false
        error = c.lenientOptional(String.self, .error)
    }

    private enum CodingKeys: String, CodingKey { case files, diffTooLarge, error }
}

// MARK: - Stream

/// One frame of `GET /v1/agents/stream`, decoded.
enum AgentBusEvent: Equatable, Sendable {
    case state(AgentSnapshot)
    case timeline(agentId: String, row: AgentTimelineRow, epoch: String)
    /// The provider raised a permission request (`permission_requested`).
    case permissionRequested(agentId: String, AgentPermission)
    case permissionResolved(agentId: String, requestId: String)
    case permission(AgentPermissionView)
    case deleted(agentId: String)
    case reset
    case ignored

    /// nil: not JSON this version understands (the frame is skipped).
    static func decode(_ frame: SSEEvent) -> AgentBusEvent? {
        if frame.event == "stream.reset" { return .reset }
        guard let data = frame.data.data(using: .utf8), let wire = try? APIClient.decoder.decode(Wire.self, from: data) else { return nil }
        switch wire.type {
        case "stream.reset": return .reset
        case "agent_state":
            guard let agent = wire.agent else { return nil }
            return .state(agent)
        case "agent_deleted":
            guard let id = wire.agentId else { return nil }
            return .deleted(agentId: id)
        case "permission":
            guard let p = wire.permission else { return nil }
            return .permission(p)
        case "agent_stream":
            guard let id = wire.agentId, let event = wire.event else { return nil }
            switch event.type {
            case "timeline":
                guard let item = event.item, let seq = wire.seq else { return .ignored }
                return .timeline(agentId: id, row: AgentTimelineRow(seq: seq, timestamp: wire.timestamp ?? "", item: item, turnId: event.turnId),
                                 epoch: wire.epoch ?? "")
            case "permission_requested":
                guard let r = event.request else { return .ignored }
                return .permissionRequested(agentId: id, r)
            case "permission_resolved":
                guard let rid = event.requestId else { return .ignored }
                return .permissionResolved(agentId: id, requestId: rid)
            default:
                return .ignored
            }
        default:
            return .ignored
        }
    }

    private struct Wire: Decodable {
        let type: String
        let agent: AgentSnapshot?
        let agentId: String?
        let permission: AgentPermissionView?
        let event: StreamEvent?
        let seq: Int?
        let epoch: String?
        let timestamp: String?

        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            type = c.lenientOptional(String.self, .type) ?? ""
            agent = c.lenientOptional(AgentSnapshot.self, .agent)
            agentId = c.lenientOptional(String.self, .agentId)
            permission = c.lenientOptional(AgentPermissionView.self, .permission)
            event = c.lenientOptional(StreamEvent.self, .event)
            seq = c.lenientOptional(Int.self, .seq)
            epoch = c.lenientOptional(String.self, .epoch)
            timestamp = c.lenientOptional(String.self, .timestamp)
        }

        private enum CodingKeys: String, CodingKey { case type, agent, agentId, permission, event, seq, epoch, timestamp }
    }

    private struct StreamEvent: Decodable {
        let type: String
        let item: AgentTimelineItem?
        let turnId: String?
        let request: AgentPermission?
        let requestId: String?

        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            type = c.lenientOptional(String.self, .type) ?? ""
            item = c.lenientOptional(AgentTimelineItem.self, .item)
            turnId = c.lenientOptional(String.self, .turnId)
            request = c.lenientOptional(AgentPermission.self, .request)
            requestId = c.lenientOptional(String.self, .requestId)
        }

        private enum CodingKeys: String, CodingKey { case type, item, turnId, request, requestId }
    }
}
