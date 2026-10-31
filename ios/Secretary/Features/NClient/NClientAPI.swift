import Foundation

/// Only service-returned signed paths may be used for pictures.
enum NClientImagePath {
    static func validated(_ value: String?) -> String? {
        guard let value, let range = value.range(of: #"^/v1/nclient/s/[0-9]+/(thumb|cover|page|pthumb)/[0-9]+/[0-9]+/[A-Za-z0-9_-]+$"#, options: .regularExpression),
              range == value.startIndex..<value.endIndex else { return nil }
        return value
    }
}

private struct NClientKey: CodingKey {
    let stringValue: String
    let intValue: Int? = nil
    init(_ value: String) { stringValue = value }
    init?(stringValue: String) { self.init(stringValue) }
    init?(intValue: Int) { return nil }
}

private extension KeyedDecodingContainer where Key == NClientKey {
    func value<T: Decodable>(_ key: String, _ fallback: T) -> T { (try? decode(T.self, forKey: NClientKey(key))) ?? fallback }
    func number(_ key: String, _ fallback: Int = 0) -> Int {
        if let n = try? decode(Int.self, forKey: NClientKey(key)) { return n }
        return Int(value(key, "")) ?? fallback
    }
}

struct NClientTag: Decodable, Equatable, Sendable, Identifiable {
    var id: Int = 0
    var name: String = ""
    var type: String = "tag"
    var count: Int = 0
    init(id: Int = 0, name: String = "", type: String = "tag", count: Int = 0) {
        self.id = id
        self.name = name
        self.type = type
        self.count = count
    }
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: NClientKey.self)
        id = c.number("id", 0)
        name = c.value("name", "")
        type = c.value("type", "tag")
        count = c.number("count", 0)
    }
}

struct NClientProgress: Decodable, Equatable, Sendable {
    var page: Int = 0
    var pages: Int = 0
    var updated_at: Int = 0
    init(page: Int = 0, pages: Int = 0, updated_at: Int = 0) {
        self.page = page
        self.pages = pages
        self.updated_at = updated_at
    }
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: NClientKey.self)
        page = c.number("page", 0)
        pages = c.number("pages", 0)
        updated_at = c.number("updated_at", 0)
    }
}

struct NClientSummary: Decodable, Equatable, Sendable, Identifiable {
    var id: Int = 0
    var title: String = ""
    var pages: Int = 0
    var language: String? = nil
    var tags: [NClientTag] = []
    var thumbUrl: String = ""
    var favorite: Bool = false
    var progress: NClientProgress? = nil
    init(id: Int = 0, title: String = "", pages: Int = 0, language: String? = nil, tags: [NClientTag] = [], thumbUrl: String = "", favorite: Bool = false, progress: NClientProgress? = nil) {
        self.id = id
        self.title = title
        self.pages = pages
        self.language = language
        self.tags = tags
        self.thumbUrl = thumbUrl
        self.favorite = favorite
        self.progress = progress
    }
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: NClientKey.self)
        id = c.number("id", 0)
        title = c.value("title", "")
        pages = c.number("pages", 0)
        language = c.value("language", nil)
        tags = c.value("tags", [])
        thumbUrl = c.value("thumbUrl", "")
        favorite = c.value("favorite", false)
        progress = c.value("progress", nil)
    }
}

struct NClientPage: Decodable, Equatable, Sendable, Identifiable {
    var index: Int = 0
    var width: Int = 0
    var height: Int = 0
    var url: String = ""
    var thumbUrl: String = ""
    var id: Int { index }
    init(index: Int = 0, width: Int = 0, height: Int = 0, url: String = "", thumbUrl: String = "") {
        self.index = index
        self.width = width
        self.height = height
        self.url = url
        self.thumbUrl = thumbUrl
    }
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: NClientKey.self)
        index = c.number("index", 0)
        width = c.number("width", 0)
        height = c.number("height", 0)
        url = c.value("url", "")
        thumbUrl = c.value("thumbUrl", "")
    }
}

struct NClientTitles: Decodable, Equatable, Sendable {
    var english: String = ""
    var japanese: String = ""
    var pretty: String = ""
    init(english: String = "", japanese: String = "", pretty: String = "") {
        self.english = english
        self.japanese = japanese
        self.pretty = pretty
    }
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: NClientKey.self)
        english = c.value("english", "")
        japanese = c.value("japanese", "")
        pretty = c.value("pretty", "")
    }
}

struct NClientDetail: Decodable, Equatable, Sendable, Identifiable {
    var id: Int = 0
    var title: String = ""
    var language: String? = nil
    var tags: [NClientTag] = []
    var thumbUrl: String = ""
    var favorite: Bool = false
    var progress: NClientProgress? = nil
    var coverUrl: String = ""
    var pages: [NClientPage] = []
    var related: [NClientSummary] = []
    var titles: NClientTitles? = nil
    var uploaded: Int = 0
    var favorites: Int = 0
    init(id: Int = 0, title: String = "", language: String? = nil, tags: [NClientTag] = [], thumbUrl: String = "", favorite: Bool = false, progress: NClientProgress? = nil, coverUrl: String = "", pages: [NClientPage] = [], related: [NClientSummary] = [], titles: NClientTitles? = nil, uploaded: Int = 0, favorites: Int = 0) {
        self.id = id
        self.title = title
        self.language = language
        self.tags = tags
        self.thumbUrl = thumbUrl
        self.favorite = favorite
        self.progress = progress
        self.coverUrl = coverUrl
        self.pages = pages
        self.related = related
        self.titles = titles
        self.uploaded = uploaded
        self.favorites = favorites
    }
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: NClientKey.self)
        id = c.number("id", 0)
        title = c.value("title", "")
        language = c.value("language", nil)
        tags = c.value("tags", [])
        thumbUrl = c.value("thumbUrl", "")
        favorite = c.value("favorite", false)
        progress = c.value("progress", nil)
        coverUrl = c.value("coverUrl", "")
        pages = c.value("pages", [])
        related = c.value("related", [])
        titles = c.value("titles", nil)
        uploaded = c.number("uploaded", 0)
        favorites = c.number("favorites", 0)
    }
}

struct NClientResults: Decodable, Equatable, Sendable {
    var items: [NClientSummary] = []
    var page: Int = 1
    var numPages: Int = 1
    init(items: [NClientSummary] = [], page: Int = 1, numPages: Int = 1) {
        self.items = items
        self.page = page
        self.numPages = numPages
    }
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: NClientKey.self)
        items = c.value("items", [])
        page = c.number("page", 1)
        numPages = c.number("numPages", 1)
    }
}

struct NClientSaveCapabilities: Decodable, Equatable, Sendable {
    var gallery: Bool = true
    var library: Bool = false
    init(gallery: Bool = true, library: Bool = false) {
        self.gallery = gallery
        self.library = library
    }
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: NClientKey.self)
        gallery = c.value("gallery", true)
        library = c.value("library", false)
    }
}

struct NClientStatus: Decodable, Equatable, Sendable {
    var enabled: Bool = false
    var save: NClientSaveCapabilities? = nil
    init(enabled: Bool = false, save: NClientSaveCapabilities? = nil) {
        self.enabled = enabled
        self.save = save
    }
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: NClientKey.self)
        enabled = c.value("enabled", false)
        save = c.value("save", nil)
    }
}

struct NClientSavedPage: Decodable, Equatable, Sendable {
    var index: Int = 0
    var itemId: String = ""
    var duplicate: Bool = false
    init(index: Int = 0, itemId: String = "", duplicate: Bool = false) {
        self.index = index
        self.itemId = itemId
        self.duplicate = duplicate
    }
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: NClientKey.self)
        index = c.number("index", 0)
        itemId = (try? c.decode(String.self, forKey: NClientKey("itemId"))) ?? (try? c.decode(Int64.self, forKey: NClientKey("itemId"))).map(String.init) ?? ""
        duplicate = c.value("duplicate", false)
    }
}

struct NClientFailedPage: Decodable, Equatable, Sendable {
    var index: Int = 0
    var error: String = ""
    init(index: Int = 0, error: String = "") {
        self.index = index
        self.error = error
    }
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: NClientKey.self)
        index = c.number("index", 0)
        error = c.value("error", "")
    }
}

struct NClientSaveStatus: Decodable, Equatable, Sendable {
    var job: String = ""
    var state: String = "unknown"
    var total: Int = 0
    var done: Int = 0
    var saved: [NClientSavedPage] = []
    var failed: [NClientFailedPage] = []
    var error: String? = nil
    init(job: String = "", state: String = "unknown", total: Int = 0, done: Int = 0, saved: [NClientSavedPage] = [], failed: [NClientFailedPage] = [], error: String? = nil) {
        self.job = job
        self.state = state
        self.total = total
        self.done = done
        self.saved = saved
        self.failed = failed
        self.error = error
    }
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: NClientKey.self)
        job = c.value("job", "")
        state = c.value("state", "unknown")
        total = c.number("total", 0)
        done = c.number("done", 0)
        saved = c.value("saved", [])
        failed = c.value("failed", [])
        error = c.value("error", nil)
    }
}

struct NClientTagResults: Decodable, Equatable, Sendable {
    var tags: [NClientTag] = []
    init(tags: [NClientTag] = []) {
        self.tags = tags
    }
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: NClientKey.self)
        tags = c.value("tags", [])
    }
}

struct NClientItems: Decodable, Equatable, Sendable {
    var items: [NClientSummary] = []
    init(items: [NClientSummary] = []) {
        self.items = items
    }
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: NClientKey.self)
        items = c.value("items", [])
    }
}

struct NClientFavorite: Decodable, Equatable, Sendable {
    var favorite: Bool = false
    init(favorite: Bool = false) {
        self.favorite = favorite
    }
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: NClientKey.self)
        favorite = c.value("favorite", false)
    }
}

extension NClientProgress {
    /// The wire page is zero-based. Clamp malformed or stale progress to the gallery bounds.
    func fraction(total: Int) -> Double {
        guard total > 0 else { return 0 }
        return min(1, max(0, (Double(page) + 1) / Double(total)))
    }
}

extension NClientSaveStatus {
    var finished: Bool { !["queued", "running"].contains(state) }
    var duplicates: Int { saved.filter(\.duplicate).count }
}

enum NClientSort: String, CaseIterable, Sendable {
    case recent, popular
    case week = "popular-week", today = "popular-today", month = "popular-month"
}

struct NClientSearch: Equatable, Sendable {
    var q = ""
    var sort: NClientSort = .recent
    var tags: [Int] = []
    var exclude: [Int] = []
    func query(page: Int) -> [URLQueryItem] {
        var items = [URLQueryItem(name: "q", value: q.trimmingCharacters(in: .whitespacesAndNewlines)),
                     URLQueryItem(name: "page", value: String(page)), URLQueryItem(name: "sort", value: sort.rawValue)]
        if !tags.isEmpty { items.append(URLQueryItem(name: "tags", value: tags.map(String.init).joined(separator: ","))) }
        if !exclude.isEmpty { items.append(URLQueryItem(name: "exclude", value: exclude.map(String.init).joined(separator: ","))) }
        return items
    }
}

struct NClientEnvelope<T: Decodable>: Decodable {
    let data: T
}

/// Pending writes are shared across API values and reader sessions, and survive a failed older write.
actor NClientProgressWriter {
    static let shared = NClientProgressWriter()
    private var pending: [Int: (UUID, Task<Void, Error>)] = [:]
    func write(id: Int, send: @escaping @Sendable () async throws -> Void) async throws {
        let previous = pending[id]?.1
        let token = UUID()
        let task = Task {
            _ = try? await previous?.value
            try await send()
        }
        pending[id] = (token, task)
        do {
            try await task.value
            if pending[id]?.0 == token { pending[id] = nil }
        } catch {
            if pending[id]?.0 == token { pending[id] = nil }
            throw error
        }
    }
}

struct NClientAPI: Sendable {
    let client: APIClient
    private let root = "/v1/nclient"
    private func get<T: Decodable>(_ path: String, query: [URLQueryItem] = []) async throws -> T {
        let response: NClientEnvelope<T> = try await client.get(root + path, query: query)
        return response.data
    }
    func status() async throws -> NClientStatus { try await get("/status") }
    func search(_ search: NClientSearch, page: Int) async throws -> NClientResults { try await get("/search", query: search.query(page: page)) }
    func tags(_ q: String) async throws -> [NClientTag] {
        let response: NClientTagResults = try await get("/tags", query: [URLQueryItem(name: "type", value: "tag"), URLQueryItem(name: "q", value: q), URLQueryItem(name: "limit", value: "30")])
        return response.tags
    }
    func detail(_ id: Int) async throws -> NClientDetail { try await get("/galleries/\(id)") }
    func random() async throws -> NClientDetail { try await get("/random") }
    func favorites() async throws -> [NClientSummary] {
        let response: NClientItems = try await get("/favorites")
        return response.items
    }
    func favorite(_ id: Int, value: Bool) async throws -> Bool {
        let response = try await client.send(value ? "PUT" : "DELETE", root + "/favorites/\(id)", body: nil)
        return try client.decode(NClientEnvelope<NClientFavorite>.self, from: response).data.favorite
    }
    func progress(_ id: Int, page: Int, pages: Int) async throws {
        struct Body: Encodable { let page: Int; let pages: Int }
        let client = client
        let path = root + "/progress/\(id)"
        try await NClientProgressWriter.shared.write(id: id) {
            let _: NClientEnvelope<NClientProgress> = try await client.send("PUT", path, body: Body(page: page, pages: pages))
        }
    }
    func save(_ id: Int) async throws -> NClientSaveStatus {
        struct Body: Encodable { let target = "gallery" }
        let response: NClientEnvelope<NClientSaveStatus> = try await client.send("POST", root + "/galleries/\(id)/save", body: Body())
        return response.data
    }
    static func jobPath(_ job: String) -> String {
        // Job ids are opaque; encode even a slash as part of the one path component.
        let allowed = CharacterSet.alphanumerics.union(CharacterSet(charactersIn: "-_.~"))
        return "/saves/" + (job.addingPercentEncoding(withAllowedCharacters: allowed) ?? "")
    }
    func saveStatus(_ job: String) async throws -> NClientSaveStatus { try await get(Self.jobPath(job)) }
    func cancelSave(_ job: String) async throws { _ = try await client.send("DELETE", root + Self.jobPath(job), body: nil) }
    func imageURL(_ path: String?) -> URL? { client.absolute(NClientImagePath.validated(path)) }
}
