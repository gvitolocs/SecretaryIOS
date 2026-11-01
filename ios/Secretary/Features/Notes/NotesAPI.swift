import Foundation

// Notes (docs/server-api.md, "Notes"): briefings, notes and documents from the owner,
// Lucina, ChatGPT, Grok and Claude Code. GET/POST /v1/notes, GET/PATCH/DELETE
// /v1/notes/{id|slug}, the trash and restore. Fields are decoded leniently.

struct NoteSummary: Decodable, Identifiable, Equatable, Hashable, Sendable {
    let id: String
    let slug: String
    let title: String
    let tags: [String]
    let source: String
    let pinned: Bool
    let createdAt: String
    let updatedAt: String
    let size: Int
    let excerpt: String
    let deletedAt: String?
    let purgeAfter: String?

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        slug = try c.decode(String.self, forKey: .slug)
        title = c.lenientOptional(String.self, .title) ?? slug
        tags = c.lenientOptional([String].self, .tags) ?? []
        source = c.lenientOptional(String.self, .source) ?? "owner"
        pinned = c.lenientOptional(Bool.self, .pinned) ?? false
        createdAt = c.lenientOptional(String.self, .createdAt) ?? ""
        updatedAt = c.lenientOptional(String.self, .updatedAt) ?? ""
        size = c.lenientOptional(Int.self, .size) ?? 0
        excerpt = c.lenientOptional(String.self, .excerpt) ?? ""
        deletedAt = c.lenientOptional(String.self, .deletedAt)
        purgeAfter = c.lenientOptional(String.self, .purgeAfter)
    }

    private enum CodingKeys: String, CodingKey {
        case id, slug, title, tags, source, pinned, createdAt, updatedAt, size, excerpt, deletedAt, purgeAfter
    }

    var updated: Date? { NoteDates.parse(updatedAt) }
    var purgeDate: Date? { purgeAfter.flatMap(NoteDates.parse) }
    /// The excerpt as plain text (markdown marks removed, a heading repeating the title dropped).
    var plainExcerpt: String { NoteText.plainExcerpt(excerpt, title: title) }
}

struct Note: Decodable, Identifiable, Equatable, Hashable, Sendable {
    let id: String
    let slug: String
    var title: String
    var content: String
    var tags: [String]
    let source: String
    var pinned: Bool
    let createdAt: String
    let updatedAt: String

    init(id: String, slug: String, title: String, content: String, tags: [String] = [], source: String = "owner",
         pinned: Bool = false, createdAt: String = "", updatedAt: String = "") {
        self.id = id
        self.slug = slug
        self.title = title
        self.content = content
        self.tags = tags
        self.source = source
        self.pinned = pinned
        self.createdAt = createdAt
        self.updatedAt = updatedAt
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        slug = try c.decode(String.self, forKey: .slug)
        title = c.lenientOptional(String.self, .title) ?? slug
        content = c.lenientOptional(String.self, .content) ?? ""
        tags = c.lenientOptional([String].self, .tags) ?? []
        source = c.lenientOptional(String.self, .source) ?? "owner"
        pinned = c.lenientOptional(Bool.self, .pinned) ?? false
        createdAt = c.lenientOptional(String.self, .createdAt) ?? ""
        updatedAt = c.lenientOptional(String.self, .updatedAt) ?? ""
    }

    private enum CodingKeys: String, CodingKey { case id, slug, title, content, tags, source, pinned, createdAt, updatedAt }

    var updated: Date? { NoteDates.parse(updatedAt) }
}

enum NoteDates {
    static func parse(_ text: String) -> Date? {
        let withFraction = ISO8601DateFormatter()
        withFraction.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        if let d = withFraction.date(from: text) { return d }
        return ISO8601DateFormatter().date(from: text)
    }
}

/// Who wrote a note, for the source badge.
enum NoteSource {
    static func label(_ source: String) -> String {
        switch source {
        case "owner", "web", "ios": String(localized: "You")
        case "lucina": "Lucina"
        case "chatgpt": "ChatGPT"
        case "grok": "Grok"
        case "claude-code", "claude": "Claude Code"
        default: source
        }
    }
}

struct NoteDraft: Encodable, Equatable, Sendable {
    var title: String
    var content: String
    var tags: [String]
    var pinned: Bool = false
}

private struct NoteList: Decodable {
    let notes: [NoteSummary]

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        // One bad entry must not hide the others.
        var out: [NoteSummary] = []
        if var items = try? c.nestedUnkeyedContainer(forKey: .notes) {
            while !items.isAtEnd {
                if let n = try? items.decode(NoteSummary.self) { out.append(n) } else { _ = try? items.decode(JSONValue.self) }
            }
        }
        notes = out
    }

    private enum CodingKeys: String, CodingKey { case notes }
}

struct NotesAPI: Sendable {
    let client: APIClient
    private static let headers = ["X-Secretary-Client": "ios"]

    private static func segment(_ key: String) -> String {
        key.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed.subtracting(CharacterSet(charactersIn: "/"))) ?? key
    }

    func list(query: String = "", tag: String = "") async throws -> [NoteSummary] {
        var items: [URLQueryItem] = []
        let q = query.trimmingCharacters(in: .whitespacesAndNewlines)
        if !q.isEmpty { items.append(URLQueryItem(name: "q", value: q)) }
        if !tag.isEmpty { items.append(URLQueryItem(name: "tag", value: tag)) }
        let list: NoteList = try await client.get("/v1/notes", query: items)
        return list.notes
    }

    func get(_ key: String) async throws -> Note {
        try await client.get("/v1/notes/\(Self.segment(key))")
    }

    func create(_ draft: NoteDraft) async throws -> Note {
        try await client.send("POST", "/v1/notes", body: draft, idempotent: false, headers: Self.headers)
    }

    struct Patch: Encodable, Equatable, Sendable {
        var title: String?
        var content: String?
        var tags: [String]?
        var pinned: Bool?
        var baseUpdatedAt: String?
    }

    func update(_ key: String, _ patch: Patch) async throws -> Note {
        try await client.send("PATCH", "/v1/notes/\(Self.segment(key))", body: patch, headers: Self.headers)
    }

    func remove(_ key: String) async throws {
        _ = try await client.send("DELETE", "/v1/notes/\(Self.segment(key))", body: nil, headers: Self.headers)
    }

    func trash() async throws -> [NoteSummary] {
        let list: NoteList = try await client.get("/v1/notes/trash")
        return list.notes
    }

    func restore(_ id: String) async throws -> Note {
        struct Empty: Encodable {}
        return try await client.send("POST", "/v1/notes/trash/\(Self.segment(id))/restore", body: Empty(), idempotent: true,
                                     headers: Self.headers)
    }
}

/// "a, b , #c" → ["a", "b", "c"] (the server normalises again).
func parseNoteTags(_ text: String) -> [String] {
    var out: [String] = []
    for raw in text.split(separator: ",") {
        var t = raw.trimmingCharacters(in: .whitespaces).lowercased()
        if t.hasPrefix("#") { t.removeFirst() }
        t = t.split(whereSeparator: \.isWhitespace).joined(separator: " ")
        if !t.isEmpty && !out.contains(t) { out.append(t) }
    }
    return out
}
