import Foundation

// Calls to /v1/notebooks (docs/design/notebooks-api.md). p1 relays them to Lucina's notebooks
// service with the same method, query, body and status. Bodies are snake_case; the app's encoder
// does not convert keys, so every body names its keys.

struct NotebooksAPI: Sendable {
    let client: APIClient
    private static let headers = ["X-Secretary-Client": "ios"]
    /// Chat answers wait for the model (DeepSeek or the local one), transformations queue behind it.
    static let answerTimeout: TimeInterval = 240
    static let uploadTimeout: TimeInterval = 300
    /// The server accepts files up to 200 MB.
    static let maxUploadBytes = 200 * 1024 * 1024

    static func segment(_ key: String) -> String {
        key.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed.subtracting(CharacterSet(charactersIn: "/"))) ?? key
    }

    private static let root = "/v1/notebooks"

    private func path(_ parts: String...) -> String {
        Self.root + parts.map { "/" + Self.segment($0) }.joined()
    }

    enum Archived: String, Sendable { case exclude, include, only }

    private struct Notebooks: Decodable {
        let items: [Notebook]
        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            items = c.lenientList(Notebook.self, .notebooks)
        }
        private enum CodingKeys: String, CodingKey { case notebooks }
    }

    private struct Wrapped<T: Decodable>: Decodable {
        let items: [T]
        let key: String
        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: AnyKey.self)
            // The one array in the object: the route's name for it differs per route.
            var found: [T] = []
            var foundKey = ""
            for k in c.allKeys {
                if let list = try? c.decode(LenientList<T>.self, forKey: k) { found = list.items; foundKey = k.stringValue; break }
            }
            items = found
            key = foundKey
        }
    }

    private struct AnyKey: CodingKey {
        var stringValue: String
        var intValue: Int? { nil }
        init?(stringValue: String) { self.stringValue = stringValue }
        init?(intValue: Int) { nil }
    }

    private struct Body: Encodable {
        var values: [String: JSONValue]
        init(_ values: [String: JSONValue]) { self.values = values }
        func encode(to encoder: Encoder) throws {
            var c = encoder.container(keyedBy: AnyKey.self)
            for (k, v) in values { try c.encode(v, forKey: AnyKey(stringValue: k)!) }
        }
    }

    private struct Empty: Encodable {}

    // MARK: Notebooks

    func list(_ archived: Archived = .exclude) async throws -> [Notebook] {
        let result: Notebooks = try await client.get(Self.root, query: [URLQueryItem(name: "archived", value: archived.rawValue)])
        return result.items
    }

    func create(name: String, description: String) async throws -> Notebook {
        var values: [String: JSONValue] = ["name": .string(name)]
        if !description.isEmpty { values["description"] = .string(description) }
        return try await client.send("POST", Self.root, body: Body(values), idempotent: false, headers: Self.headers)
    }

    func detail(_ id: String) async throws -> NotebookDetail {
        try await client.get(path(id))
    }

    func update(_ id: String, name: String? = nil, description: String? = nil, archived: Bool? = nil) async throws -> Notebook {
        var values: [String: JSONValue] = [:]
        if let name { values["name"] = .string(name) }
        if let description { values["description"] = .string(description) }
        if let archived { values["archived"] = .bool(archived) }
        return try await client.send("PATCH", path(id), body: Body(values), headers: Self.headers)
    }

    /// `deleteSources`: also delete the sources no other notebook uses.
    func delete(_ id: String, deleteSources: Bool) async throws {
        _ = try await client.send("DELETE", path(id), query: [URLQueryItem(name: "sources", value: deleteSources ? "delete-exclusive" : "keep")],
                                  body: nil, headers: Self.headers)
    }

    // MARK: Sources

    enum NewSource: Sendable, Equatable {
        case url(String)
        case text(title: String, text: String)
        /// A file on Lucina's computer, under /home/cua/.
        case sandbox(path: String)
        case note(slug: String)
        case existing(id: String)

        var body: [String: JSONValue] {
            switch self {
            case let .url(url): ["kind": .string("url"), "url": .string(url)]
            case let .text(title, text):
                title.isEmpty ? ["kind": .string("text"), "text": .string(text)]
                              : ["kind": .string("text"), "title": .string(title), "text": .string(text)]
            case let .sandbox(path): ["kind": .string("sandbox"), "path": .string(path)]
            case let .note(slug): ["kind": .string("note"), "slug": .string(slug)]
            case let .existing(id): ["kind": .string("source"), "id": .string(id)]
            }
        }
    }

    func addSource(_ notebook: String, _ source: NewSource, transformations: [String] = []) async throws -> NotebookSource {
        var values = source.body
        if !transformations.isEmpty { values["transformations"] = .array(transformations.map(JSONValue.string)) }
        return try await client.send("POST", path(notebook, "sources"), body: Body(values), idempotent: false, headers: Self.headers)
    }

    func upload(_ notebook: String, data: Data, filename: String, title: String? = nil) async throws -> NotebookSource {
        let multipart = Multipart(parts: [
            .text(name: "title", value: title ?? ""),
            .file(name: "file", filename: filename, data: data),
        ].filter { if case let .text(_, value) = $0 { return !value.isEmpty } else { return true } })
        let response = try await client.send("POST", path(notebook, "sources", "upload"), body: multipart.data,
                                             timeout: Self.uploadTimeout, idempotent: false,
                                             headers: Self.headers.merging(["Content-Type": multipart.contentType]) { $1 })
        return try client.decode(NotebookSource.self, from: response)
    }

    func setContext(_ notebook: String, source: String, _ context: SourceContext) async throws -> NotebookSource {
        try await client.send("PATCH", path(notebook, "sources", source), body: Body(["context": .string(context.rawValue)]), headers: Self.headers)
    }

    func removeSource(_ notebook: String, source: String) async throws {
        _ = try await client.send("DELETE", path(notebook, "sources", source), body: nil, headers: Self.headers)
    }

    func source(_ id: String) async throws -> NotebookSource {
        try await client.get(path("sources", id))
    }

    func retry(_ source: String) async throws -> NotebookSource {
        try await client.send("POST", path("sources", source, "retry"), body: Empty(), idempotent: false, headers: Self.headers)
    }

    func chunks(_ source: String, from: Int = 0, limit: Int = 40, around: Int? = nil) async throws -> [SourceChunk] {
        var query = [URLQueryItem(name: "limit", value: String(limit))]
        if let around { query.append(URLQueryItem(name: "around", value: String(around))) } else { query.append(URLQueryItem(name: "from", value: String(from))) }
        let result: Wrapped<SourceChunk> = try await client.get(path("sources", source, "chunks"), query: query)
        return result.items
    }

    func insights(_ source: String) async throws -> [SourceInsight] {
        let result: Wrapped<SourceInsight> = try await client.get(path("sources", source, "insights"))
        return result.items
    }

    func runTransformation(_ source: String, transformation: String) async throws -> SourceInsight {
        try await client.send("POST", path("sources", source, "insights"), body: Body(["transformation_id": .string(transformation)]),
                              timeout: Self.answerTimeout, idempotent: false, headers: Self.headers)
    }

    func deleteInsight(_ id: String) async throws {
        _ = try await client.send("DELETE", path("insights", id), body: nil, headers: Self.headers)
    }

    func saveInsight(_ id: String) async throws -> NotebookNote {
        try await client.send("POST", path("insights", id, "note"), body: Empty(), idempotent: false, headers: Self.headers)
    }

    func transformations() async throws -> [Transformation] {
        let result: Wrapped<Transformation> = try await client.get(path("transformations"))
        return result.items
    }

    // MARK: Notes

    func writeNote(_ notebook: String, title: String, text: String) async throws -> NotebookNote {
        var values: [String: JSONValue] = ["text": .string(text)]
        if !title.isEmpty { values["title"] = .string(title) }
        return try await client.send("POST", path(notebook, "notes"), body: Body(values), idempotent: false, headers: Self.headers)
    }

    func linkNote(_ notebook: String, slug: String) async throws -> NotebookNote {
        try await client.send("POST", path(notebook, "notes"), body: Body(["slug": .string(slug)]), idempotent: false, headers: Self.headers)
    }

    func unlinkNote(_ notebook: String, slug: String) async throws {
        _ = try await client.send("DELETE", path(notebook, "notes", slug), body: nil, headers: Self.headers)
    }

    // MARK: Chat

    func chats(_ notebook: String) async throws -> [NotebookChat] {
        let result: Wrapped<NotebookChat> = try await client.get(path(notebook, "chats"))
        return result.items
    }

    func createChat(_ notebook: String, title: String? = nil) async throws -> NotebookChat {
        let values: [String: JSONValue] = title.map { ["title": .string($0)] } ?? [:]
        return try await client.send("POST", path(notebook, "chats"), body: Body(values), idempotent: false, headers: Self.headers)
    }

    func chat(_ id: String) async throws -> NotebookChatDetail {
        try await client.get(path("chats", id))
    }

    func renameChat(_ id: String, title: String) async throws -> NotebookChat {
        try await client.send("PATCH", path("chats", id), body: Body(["title": .string(title)]), headers: Self.headers)
    }

    func deleteChat(_ id: String) async throws {
        _ = try await client.send("DELETE", path("chats", id), body: nil, headers: Self.headers)
    }

    func ask(_ chat: String, text: String, model: NotebookModel) async throws -> NotebookExchange {
        try await client.send("POST", path("chats", chat, "messages"),
                              body: Body(["text": .string(text), "model": .string(model.rawValue)]),
                              timeout: Self.answerTimeout, idempotent: false, headers: Self.headers)
    }

    func saveAnswer(chat: String, message: String, title: String? = nil) async throws -> NotebookNote {
        let values: [String: JSONValue] = title.map { ["title": .string($0)] } ?? [:]
        return try await client.send("POST", path("chats", chat, "messages", message, "note"), body: Body(values), idempotent: false,
                                     headers: Self.headers)
    }

    // MARK: Podcasts

    func profiles() async throws -> PodcastProfiles {
        try await client.get(path("podcast-profiles"))
    }

    func podcasts(_ notebook: String) async throws -> [NotebookPodcast] {
        let result: Wrapped<NotebookPodcast> = try await client.get(path(notebook, "podcasts"))
        return result.items
    }

    func createPodcast(_ notebook: String, episodeProfile: String?, speakerProfile: String?, briefing: String, minutes: Double?) async throws -> NotebookPodcast {
        var values: [String: JSONValue] = [:]
        if let episodeProfile { values["episode_profile"] = .string(episodeProfile) }
        if let speakerProfile { values["speaker_profile"] = .string(speakerProfile) }
        if !briefing.isEmpty { values["briefing"] = .string(briefing) }
        if let minutes { values["minutes"] = .number(minutes) }
        return try await client.send("POST", path(notebook, "podcasts"), body: Body(values), idempotent: false, headers: Self.headers)
    }

    func podcast(_ id: String) async throws -> NotebookPodcast {
        try await client.get(path("podcasts", id))
    }

    func deletePodcast(_ id: String) async throws {
        _ = try await client.send("DELETE", path("podcasts", id), body: nil, headers: Self.headers)
    }

    /// The MP3, with the Bearer token (an `AVPlayer` pointed at the URL would not send it).
    func podcastAudio(_ id: String) async throws -> Data {
        let response = try await client.send("GET", path("podcasts", id, "audio"), body: nil, timeout: 120)
        return response.data
    }
}

/// A user-facing sentence for a failed notebooks call. A 404 on the collection routes usually means
/// the server running on p1 does not have Notebooks yet.
enum NotebooksError {
    static func message(_ error: Error) -> String {
        if let error = error as? APIError {
            switch error {
            case let .server(status, code, message) where [404, 501, 503].contains(status) && (code.hasPrefix("http_") || code == "unavailable"):
                return message.isEmpty || code.hasPrefix("http_")
                    ? String(localized: "Notebooks aren't available on the server yet.") : message
            default:
                return error.userMessage
            }
        }
        return error.localizedDescription
    }

    /// The server does not know the route at all (as opposed to one missing notebook).
    static func isUnavailable(_ error: Error) -> Bool {
        if case let .server(status, code, _)? = error as? APIError { return [501, 503].contains(status) || (status == 404 && code.hasPrefix("http_")) }
        return false
    }
}

/// `multipart/form-data` for the upload route.
struct Multipart {
    enum Part: Equatable {
        case text(name: String, value: String)
        case file(name: String, filename: String, data: Data)
    }

    let boundary: String
    let data: Data
    var contentType: String { "multipart/form-data; boundary=\(boundary)" }

    init(parts: [Part], boundary: String = "Boundary-\(UUID().uuidString)") {
        self.boundary = boundary
        var body = Data()
        func add(_ string: String) { body.append(Data(string.utf8)) }
        for part in parts {
            add("--\(boundary)\r\n")
            switch part {
            case let .text(name, value):
                add("Content-Disposition: form-data; name=\"\(Self.quoted(name))\"\r\n\r\n\(value)\r\n")
            case let .file(name, filename, data):
                add("Content-Disposition: form-data; name=\"\(Self.quoted(name))\"; filename=\"\(Self.quoted(filename))\"\r\n")
                add("Content-Type: application/octet-stream\r\n\r\n")
                body.append(data)
                add("\r\n")
            }
        }
        add("--\(boundary)--\r\n")
        self.data = body
    }

    private static func quoted(_ value: String) -> String {
        value.replacingOccurrences(of: "\\", with: "\\\\").replacingOccurrences(of: "\"", with: "%22")
            .replacingOccurrences(of: "\r", with: " ").replacingOccurrences(of: "\n", with: " ")
    }
}
