import Foundation

// The unified conversation with Lucina across every channel (iOS, web, Telegram,
// Discord, Alexa): `GET /v1/timeline` pages and the `GET /v1/timeline/stream` SSE feed.
// This file holds the wire types and the pure logic (merge/dedupe, SSE parsing) so it
// can be unit-tested without a network.

enum TimelineChannel: String, Codable, CaseIterable, Sendable {
    case telegram, discord, web, ios, alexa, other

    init(raw: String) { self = TimelineChannel(rawValue: raw.lowercased()) ?? .other }

    var symbol: String {
        switch self {
        case .web: return "globe"
        case .ios: return "iphone"
        case .telegram: return "paperplane"
        case .discord: return "gamecontroller"
        case .alexa: return "homepod"
        case .other: return "ellipsis.bubble"
        }
    }

    var label: String {
        switch self {
        case .web: return "Web"
        case .ios: return "iPhone"
        case .telegram: return "Telegram"
        case .discord: return "Discord"
        case .alexa: return "Alexa"
        case .other: return "Other"
        }
    }
}

struct TimelineAttachment: Codable, Equatable, Hashable, Sendable {
    let kind: String
    let name: String?
}

/// A button on a proactive message: a feedback tap (`useful`, `not_now`, `stop`) or an
/// in-app link (`route`).
struct ProactiveAction: Codable, Equatable, Hashable, Sendable {
    let id: String
    let label: String
    var route: String?
    var feedback: Bool?

    static let feedbackIds = ["useful", "not_now", "stop"]
    var isFeedback: Bool { feedback == true && Self.feedbackIds.contains(id) }
}

struct TimelineMeta: Codable, Equatable, Hashable, Sendable {
    var via: String?
    var caller: String?
    var chatTitle: String?
    var guildName: String?
    var channelName: String?
    var isDm: Bool?
    var kind: String?
    /// `{id, type, args}` when `kind == "device_command"`.
    var command: JSONValue?
    /// With `kind == "avatar_outfit"`: the outfit / hairstyle Lucina put on (neither: back to automatic).
    var outfit: String?
    var hair: String?
    /// With `kind == "note"`: the note Lucina saved or read.
    var slug: String?
    /// With `kind == "proactive"` (Lucina reached out on her own): `po_…`, `low|normal|high`,
    /// what it is about, why she wrote, and the buttons.
    var outreachId: String?
    var priority: String?
    var topic: String?
    var why: String?
    var actions: [ProactiveAction]?

    var isReminder: Bool { kind == "reminder" }
    var isProactive: Bool {
        guard kind == "proactive", let id = outreachId else { return false }
        return id.count == 29 && id.hasPrefix("po_")
            && id.dropFirst(3).allSatisfy { ("0"..."9").contains($0) || ("A"..."Z").contains($0) && !"ILOU".contains($0) }
    }
    var isDeviceCommand: Bool { kind == "device_command" }

    init(chatTitle: String? = nil, guildName: String? = nil, channelName: String? = nil, isDm: Bool? = nil, kind: String? = nil) {
        self.chatTitle = chatTitle
        self.guildName = guildName
        self.channelName = channelName
        self.isDm = isDm
        self.kind = kind
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        via = c.lenientOptional(String.self, .via)
        caller = c.lenientOptional(String.self, .caller)
        chatTitle = c.lenientOptional(String.self, .chatTitle)
        guildName = c.lenientOptional(String.self, .guildName)
        channelName = c.lenientOptional(String.self, .channelName)
        isDm = c.lenientOptional(Bool.self, .isDm)
        kind = c.lenientOptional(String.self, .kind)
        command = c.lenientOptional(JSONValue.self, .command)
        outfit = c.lenientOptional(String.self, .outfit)
        hair = c.lenientOptional(String.self, .hair)
        slug = c.lenientOptional(String.self, .slug)
        outreachId = c.lenientOptional(String.self, .outreachId)
        priority = c.lenientOptional(String.self, .priority)
        topic = c.lenientOptional(String.self, .topic)
        why = c.lenientOptional(String.self, .why)
        actions = c.lenientOptional([ProactiveAction].self, .actions)
    }

    private enum CodingKeys: String, CodingKey {
        case via, caller, chatTitle, guildName, channelName, isDm, kind, command, outfit, hair, slug
        case outreachId, priority, topic, why, actions
    }
}

/// One message in the timeline. Server items have server ids; optimistic ones made on
/// this phone start with `local-` and carry a `localStatus` until the server's copy
/// replaces them.
struct TimelineItem: Codable, Identifiable, Equatable, Sendable {
    enum Role: String, Codable, Sendable { case user, lucina }
    enum LocalStatus: String, Codable, Sendable { case sending, sent, error }

    let id: String
    var ts: Date
    var channel: TimelineChannel
    var role: Role
    var text: String
    var clientMessageId: String?
    var replyTo: String?
    var attachments: [TimelineAttachment]
    var meta: TimelineMeta?
    var backfilled: Bool
    /// Lucina message format v1 (`meta.format == "v1"`): bubbles, widgets and buttons.
    var rich: RichMessage?

    // Local only (never on the wire).
    var localStatus: LocalStatus?
    var localError: String?

    var isLocal: Bool { id.hasPrefix(TimelineItem.localPrefix) }

    static let localPrefix = "local-"

    init(id: String, ts: Date, channel: TimelineChannel, role: Role, text: String, clientMessageId: String? = nil,
         replyTo: String? = nil, attachments: [TimelineAttachment] = [], meta: TimelineMeta? = nil, backfilled: Bool = false,
         localStatus: LocalStatus? = nil, localError: String? = nil, rich: RichMessage? = nil) {
        self.id = id
        self.ts = ts
        self.channel = channel
        self.role = role
        self.text = text
        self.clientMessageId = clientMessageId
        self.replyTo = replyTo
        self.attachments = attachments
        self.meta = meta
        self.backfilled = backfilled
        self.localStatus = localStatus
        self.localError = localError
        self.rich = rich
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(FlexibleID.self, forKey: .id).value
        ts = Self.decodeDate(c) ?? Date(timeIntervalSince1970: 0)
        channel = TimelineChannel(raw: c.lenient(String.self, .channel, "other"))
        // Items with a role other than user/lucina are skipped (docs: timeline item).
        guard let role = Role(rawValue: c.lenient(String.self, .role, "")) else {
            throw DecodingError.dataCorruptedError(forKey: .role, in: c, debugDescription: "unsupported role")
        }
        self.role = role
        text = c.lenient(String.self, .text, "")
        clientMessageId = c.lenientOptional(String.self, .clientMessageId)
        replyTo = c.lenientOptional(FlexibleID.self, .replyTo)?.value
        attachments = c.lenient([TimelineAttachment].self, .attachments, [])
        meta = c.lenientOptional(TimelineMeta.self, .meta)
        backfilled = c.lenient(Bool.self, .backfilled, false)
        // From the server: inside meta. From this phone's cache: under `rich`.
        rich = RichMessage.parse(c.lenientOptional(JSONValue.self, .meta)) ?? c.lenientOptional(RichMessage.self, .rich)
        localStatus = c.lenientOptional(LocalStatus.self, .localStatus)
        localError = c.lenientOptional(String.self, .localError)
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(id, forKey: .id)
        try c.encode(ts.timeIntervalSince1970 * 1000, forKey: .ts)
        try c.encode(channel.rawValue, forKey: .channel)
        try c.encode(role.rawValue, forKey: .role)
        try c.encode(text, forKey: .text)
        try c.encodeIfPresent(clientMessageId, forKey: .clientMessageId)
        try c.encodeIfPresent(replyTo, forKey: .replyTo)
        if !attachments.isEmpty { try c.encode(attachments, forKey: .attachments) }
        try c.encodeIfPresent(meta, forKey: .meta)
        if backfilled { try c.encode(true, forKey: .backfilled) }
        try c.encodeIfPresent(localStatus, forKey: .localStatus)
        try c.encodeIfPresent(localError, forKey: .localError)
        try c.encodeIfPresent(rich, forKey: .rich)
    }

    private enum CodingKeys: String, CodingKey {
        case id, ts, channel, role, text, clientMessageId, replyTo, attachments, meta, backfilled, localStatus, localError, rich
    }

    /// `ts` as an ISO 8601 string (with or without fractions) or epoch milliseconds.
    private static func decodeDate(_ c: KeyedDecodingContainer<CodingKeys>) -> Date? {
        if let ms = c.lenientOptional(Double.self, .ts) { return Date(timeIntervalSince1970: ms / 1000) }
        guard let text = c.lenientOptional(String.self, .ts) else { return nil }
        let fractional = ISO8601DateFormatter()
        fractional.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        if let date = fractional.date(from: text) { return date }
        return ISO8601DateFormatter().date(from: text)
    }
}

/// The client or channel shown beneath a message; the stored channel stays unchanged.
struct MessageSource: Equatable, Sendable {
    let label: String
    let symbol: String

    static func mcp(caller: String?) -> MessageSource {
        let normalized = caller?.trimmingCharacters(in: .whitespacesAndNewlines).lowercased() ?? ""
        if normalized.hasPrefix("claude-code") || normalized == "claude code" {
            return MessageSource(label: "Claude Code", symbol: "sparkle")
        }
        if normalized == "claude-desktop" {
            return MessageSource(label: "Claude Desktop", symbol: "sparkle")
        }
        if normalized.hasPrefix("claude") {
            return MessageSource(label: "Claude", symbol: "sparkle")
        }
        if normalized.hasPrefix("chatgpt") || normalized.hasPrefix("openai") {
            return MessageSource(label: "ChatGPT", symbol: "bubble.left.and.bubble.right")
        }
        if normalized.hasPrefix("grok") || normalized.hasPrefix("xai") {
            return MessageSource(label: "Grok", symbol: "bolt")
        }
        if normalized.hasPrefix("codex") {
            return MessageSource(label: "Codex", symbol: "chevron.left.forwardslash.chevron.right")
        }
        if normalized.hasPrefix("cursor") || normalized == "www" {
            return MessageSource(label: "Cursor", symbol: "cursorarrow.rays")
        }
        if normalized.hasPrefix("gemini") {
            return MessageSource(label: "Gemini", symbol: "star")
        }
        return MessageSource(label: normalized.isEmpty ? "MCP" : "MCP · \(caller ?? "")", symbol: "puzzlepiece.extension")
    }
}

extension TimelineItem {
    var isMcp: Bool { meta?.via == "mcp" || legacyMcp != nil }

    var source: MessageSource {
        if meta?.via == "mcp" { return .mcp(caller: meta?.caller) }
        if let legacyMcp { return .mcp(caller: legacyMcp.caller) }

        var label = channel.label
        if let meta {
            if channel == .discord {
                if meta.isDm == true {
                    label = "Discord DM"
                } else {
                    let place = [meta.guildName, meta.channelName.map { "#\($0)" }].compactMap { $0 }.joined(separator: " ")
                    label = place.isEmpty ? "Discord" : String(localized: "Discord · \(place)")
                }
            } else if channel == .telegram, let title = meta.chatTitle, meta.isDm != true {
                label = "Telegram · \(title)"
            }
        }
        return MessageSource(label: label, symbol: channel.symbol)
    }

    var displayText: String { legacyMcp?.text ?? text }

    private var legacyMcp: (caller: String, text: String)? {
        let prefix = "[via Lucina MCP, from "
        guard meta?.via == nil, role == .user, text.hasPrefix(prefix) else { return nil }
        let remainder = text.dropFirst(prefix.count)
        guard let closing = remainder.range(of: "] ") else { return nil }
        let caller = String(remainder[..<closing.lowerBound]).trimmingCharacters(in: .whitespacesAndNewlines)
        return (caller, String(remainder[closing.upperBound...]))
    }
}

struct TimelinePage: Decodable, Equatable, Sendable {
    /// Newest first, as the server sends them.
    let items: [TimelineItem]
    /// Cursor for the next older page; nil when there is none.
    let nextBefore: String?

    init(items: [TimelineItem], nextBefore: String?) {
        self.items = items
        self.nextBefore = nextBefore
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        // One odd item must not hide the page: undecodable ones are skipped.
        items = (try c.decodeIfPresent([Lossy<TimelineItem>].self, forKey: .items) ?? []).compactMap(\.value)
        nextBefore = c.lenientOptional(FlexibleID.self, .nextBefore)?.value
    }

    private enum CodingKeys: String, CodingKey { case items, nextBefore }
}

enum TimelineMerge {
    struct Result: Equatable {
        /// Oldest first, for display.
        var items: [TimelineItem]
        /// Local ids that a server item replaced: `[local id: server id]` (to move job cards).
        var replaced: [String: String]
    }

    /// Merges server items into what is shown:
    /// - dedupe by id (the incoming copy wins, so edits and backfills update in place);
    /// - an incoming **user** item drops the optimistic local bubble with the same
    ///   `clientMessageId`;
    /// - an incoming **Lucina** item drops the local reply placeholder for the same
    ///   `clientMessageId` or the same `replyTo`;
    /// - the result is ordered by time (user before Lucina on a tie, then by id).
    static func merge(_ current: [TimelineItem], _ incoming: [TimelineItem]) -> Result {
        var byId: [String: TimelineItem] = [:]
        for item in current { byId[item.id] = item }
        var replaced: [String: String] = [:]
        // Users first, so a local reply placeholder is re-pointed at the server's user id
        // before Lucina's server item (same batch or later) is matched against it.
        let ordered = incoming.filter { $0.role == .user } + incoming.filter { $0.role == .lucina }
        for item in ordered {
            byId[item.id] = item
            for local in byId.values where local.isLocal && local.role == item.role && local.id != item.id {
                let sameClient = item.clientMessageId != nil && item.clientMessageId == local.clientMessageId
                let sameReply = item.role == .lucina && item.replyTo != nil && item.replyTo == local.replyTo
                if sameClient || sameReply {
                    byId[local.id] = nil
                    replaced[local.id] = item.id
                    if item.role == .user {
                        for (id, other) in byId where other.isLocal && other.replyTo == local.id {
                            byId[id]?.replyTo = item.id
                        }
                    }
                }
            }
        }
        return Result(items: sorted(Array(byId.values)), replaced: replaced)
    }

    static func sorted(_ items: [TimelineItem]) -> [TimelineItem] {
        items.sorted { a, b in
            if a.ts != b.ts { return a.ts < b.ts }
            if a.role != b.role { return a.role == .user }
            return a.id < b.id
        }
    }
}

/// One Server-Sent Event.
struct SSEEvent: Equatable, Sendable {
    var event: String
    var data: String
    var id: String?
    var retry: Int?
}

/// A line-by-line SSE parser (WHATWG rules): `field: value` lines accumulate, a blank
/// line dispatches, `:` lines are comments, `data` lines join with newlines.
struct SSEParser {
    private var event = ""
    private var data: [String] = []
    private var id: String?
    private var retry: Int?
    private var sawField = false
    /// The last `id` seen (sent back as `Last-Event-ID` on reconnect).
    private(set) var lastEventId: String?

    /// Feeds one line without its terminator. Returns an event when the line was blank
    /// and something was pending.
    mutating func feed(_ rawLine: String) -> SSEEvent? {
        let line = rawLine.hasSuffix("\r") ? String(rawLine.dropLast()) : rawLine
        if line.isEmpty { return dispatch() }
        if line.hasPrefix(":") { return nil }
        let field: Substring
        var value: Substring
        if let colon = line.firstIndex(of: ":") {
            field = line[..<colon]
            value = line[line.index(after: colon)...]
            if value.hasPrefix(" ") { value = value.dropFirst() }
        } else {
            field = Substring(line)
            value = ""
        }
        switch field {
        case "event": event = String(value); sawField = true
        case "data": data.append(String(value)); sawField = true
        case "id":
            if !value.contains("\0") { id = String(value); lastEventId = String(value) }
            sawField = true
        case "retry": retry = Int(value); sawField = true
        default: break
        }
        return nil
    }

    private mutating func dispatch() -> SSEEvent? {
        defer {
            event = ""
            data = []
            id = nil
            retry = nil
            sawField = false
        }
        guard sawField, !data.isEmpty || retry != nil else { return nil }
        return SSEEvent(event: event.isEmpty ? "message" : event, data: data.joined(separator: "\n"), id: id, retry: retry)
    }

    /// Splits a chunk of bytes into lines (LF, CRLF or lone CR), keeping a partial tail.
    static func lines(from buffer: inout [UInt8]) -> [String] {
        var out: [String] = []
        var start = 0
        var index = 0
        while index < buffer.count {
            let byte = buffer[index]
            if byte == 0x0D, index + 1 == buffer.count { break } // maybe half of a CRLF: wait for more
            if byte == 0x0A || byte == 0x0D {
                out.append(String(decoding: buffer[start..<index], as: UTF8.self))
                if byte == 0x0D, index + 1 < buffer.count, buffer[index + 1] == 0x0A { index += 1 }
                start = index + 1
            }
            index += 1
        }
        buffer.removeFirst(start)
        return out
    }
}

/// Decodes a value or nil, so one bad element doesn't fail a whole array.
struct Lossy<Value: Decodable>: Decodable {
    let value: Value?

    init(from decoder: Decoder) throws {
        value = try? Value(from: decoder)
    }
}
