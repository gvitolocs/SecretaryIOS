import Foundation

/// Owner-only plugin calls; cold upstream reads can take 35 seconds.
struct InstagramAPI: Sendable {
    let client: APIClient

    func read<T: Decodable>(_ path: String, query: [URLQueryItem] = []) async throws -> T {
        let response = try await client.send("GET", "/v1/instagram" + path, query: query, body: nil,
                                             timeout: 60, passThrough: [429])
        if response.status == 429 {
            let body = try? JSONSerialization.jsonObject(with: response.data) as? [String: Any]
            let seconds = APIClient.parseRetryAfter(response.headers["retry-after"])
                ?? (body?["retryAfterSeconds"] as? Int) ?? (body?["retryAfter"] as? Int)
            throw APIError.rateLimited(code: "rate_limited", retryAfter: seconds, message: "")
        }
        return try client.decode(IgEnvelope<T>.self, from: response).data
    }

    private func write<T: Decodable, B: Encodable>(_ method: String, _ path: String, _ body: B) async throws -> T {
        let answer: IgEnvelope<T> = try await client.send(method, "/v1/instagram" + path, body: body, timeout: 60)
        return answer.data
    }

    func profile(_ username: String) async throws -> IgProfileBundle { try await read("/profile/" + Self.segment(username)) }
    func page(_ tab: IgTab, username: String, cursor: String? = nil) async throws -> IgPage {
        try await read("/\(tab.rawValue)/" + Self.segment(username), query: Self.cursorQuery(cursor))
    }
    func highlights(_ username: String) async throws -> [IgHighlight] {
        let result: IgList<IgHighlight> = try await read("/highlights/" + Self.segment(username))
        return result.items
    }
    func highlightItems(_ id: String, cursor: String? = nil) async throws -> IgPage {
        try await read("/highlights/items", query: [URLQueryItem(name: "id", value: id)] + Self.cursorQuery(cursor))
    }
    func resolve(_ url: String) async throws -> IgPage { try await read("/resolve", query: [URLQueryItem(name: "url", value: url)]) }
    func recent() async throws -> [IgRecent] {
        let result: IgList<IgRecent> = try await read("/recent")
        return result.items
    }
    func watchList() async throws -> [IgWatch] { try await read("/watch") }
    func badges() async throws -> IgBadges { try await read("/badges") }
    func watchSettings() async throws -> IgWatchSettings { try await read("/watch-settings") }
    func watch(_ username: String, autoSave: Bool? = nil) async throws -> IgWatch {
        try await write("PUT", "/watch/" + Self.segment(username), IgWatchBody(autoSave: autoSave))
    }
    func unwatch(_ username: String) async throws {
        let _: IgRemoved = try await write("DELETE", "/watch/" + Self.segment(username), IgEmpty())
    }
    func opened(_ username: String) async throws -> IgBadges {
        try await write("POST", "/watch/" + Self.segment(username) + "/opened", IgEmpty())
    }
    func setWatchSettings(_ hours: Int) async throws -> IgWatchSettings {
        try await write("PUT", "/watch-settings", IgIntervalBody(intervalHours: hours))
    }
    func save(_ items: [IgSaveItem]) async throws -> IgSaveResult {
        try await write("POST", "/save", IgSaveBody(items: items))
    }
    /// A new link for one the service signed that has expired; nil when it did not sign it.
    func resign(_ url: String) async throws -> String? {
        let answer: IgResigned = try await write("POST", "/resign", IgResignBody(urls: [url]))
        return answer.urls[url]
    }
    static func segment(_ value: String) -> String {
        value.addingPercentEncoding(withAllowedCharacters: .alphanumerics.union(CharacterSet(charactersIn: "-._~"))) ?? ""
    }
    static func cursorQuery(_ cursor: String?) -> [URLQueryItem] {
        guard let cursor, !cursor.isEmpty else { return [] }
        return [URLQueryItem(name: "cursor", value: cursor)]
    }
}

struct IgEnvelope<T: Decodable>: Decodable {
    let data: T
    private enum CodingKeys: String, CodingKey { case ok, data }
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        if (try? c.decode(Bool.self, forKey: .ok)) == false {
            throw APIError.server(status: 200, code: "instagram_not_ok", message: "")
        }
        data = try c.decode(T.self, forKey: .data)
    }
}
private struct IgEmpty: Encodable {}
private struct IgRemoved: Decodable { let removed: Bool? }
private struct IgWatchBody: Encodable { let autoSave: Bool? }
private struct IgIntervalBody: Encodable { let intervalHours: Int }
private struct IgSaveBody: Encodable { let items: [IgSaveItem] }
struct IgSaveItem: Encodable, Equatable, Sendable {
    let url: String
    let kind: String
    let username: String
    let takenAt: Double?
    let caption: String?
}

/// String keys keep the tolerant wire readers small and local to this plugin.
private struct IgKey: CodingKey {
    let stringValue: String
    let intValue: Int? = nil
    init(_ value: String) { stringValue = value }
    init?(stringValue: String) { self.init(stringValue) }
    init?(intValue: Int) { return nil }
}
private extension KeyedDecodingContainer where Key == IgKey {
    func value<T: Decodable>(_ key: String, _ fallback: T) -> T { (try? decode(T.self, forKey: IgKey(key))) ?? fallback }
    func optional<T: Decodable>(_ key: String) -> T? { try? decode(T.self, forKey: IgKey(key)) }
    func identifier(_ key: String) -> String {
        if let string: String = optional(key) { return string }
        if let number: Int64 = optional(key) { return String(number) }
        return ""
    }
}
struct IgList<T: Decodable>: Decodable {
    let items: [T]
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: IgKey.self)
        items = c.value("items", [])
    }
}

struct IgChild: Decodable, Equatable, Sendable, Identifiable {
    var id: String = ""
    var kind: String = "image"
    var url: String = ""
    var thumbUrl: String? = nil
    init(id: String = "", kind: String = "image", url: String = "", thumbUrl: String? = nil) {
        self.id = id
        self.kind = kind
        self.url = url
        self.thumbUrl = thumbUrl
    }
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: IgKey.self)
        id = c.identifier("id")
        kind = c.value("kind", "image")
        url = c.value("url", "")
        thumbUrl = c.optional("thumbUrl")
    }
}

struct IgItem: Decodable, Equatable, Sendable, Identifiable {
    var id: String = ""
    var kind: String = "image"
    var url: String = ""
    var thumbUrl: String? = nil
    var takenAt: Double? = nil
    var caption: String? = nil
    var likeCount: Int? = nil
    var commentCount: Int? = nil
    var permalink: String? = nil
    var children: [IgChild] = []
    init(id: String = "", kind: String = "image", url: String = "", thumbUrl: String? = nil, takenAt: Double? = nil, caption: String? = nil, likeCount: Int? = nil, commentCount: Int? = nil, permalink: String? = nil, children: [IgChild] = []) {
        self.id = id
        self.kind = kind
        self.url = url
        self.thumbUrl = thumbUrl
        self.takenAt = takenAt
        self.caption = caption
        self.likeCount = likeCount
        self.commentCount = commentCount
        self.permalink = permalink
        self.children = children
    }
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: IgKey.self)
        id = c.identifier("id")
        kind = c.value("kind", "image")
        url = c.value("url", "")
        thumbUrl = c.optional("thumbUrl")
        takenAt = c.optional("takenAt")
        caption = c.optional("caption")
        likeCount = c.optional("likeCount")
        commentCount = c.optional("commentCount")
        permalink = c.optional("permalink")
        children = c.value("children", [])
    }
}

struct IgPage: Decodable, Equatable, Sendable {
    var items: [IgItem] = []
    var nextCursor: String? = nil
    var exhausted: Bool = false
    init(items: [IgItem] = [], nextCursor: String? = nil, exhausted: Bool = false) {
        self.items = items
        self.nextCursor = nextCursor
        self.exhausted = exhausted
    }
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: IgKey.self)
        items = c.value("items", [])
        nextCursor = c.optional("nextCursor")
        exhausted = c.value("exhausted", false)
    }
}

struct IgProfile: Decodable, Equatable, Sendable {
    var username: String = ""
    var fullName: String = ""
    var biography: String? = nil
    var picUrl: String? = nil
    var profileUrl: String = ""
    var mediaCount: Int = 0
    var followerCount: Int = 0
    var followingCount: Int = 0
    var isPrivate: Bool = false
    var hasStories: Bool = false
    var isVerified: Bool = false
    var userId: String = ""
    var externalUrl: String? = nil
    init(username: String = "", fullName: String = "", biography: String? = nil, picUrl: String? = nil, profileUrl: String = "", mediaCount: Int = 0, followerCount: Int = 0, followingCount: Int = 0, isPrivate: Bool = false, hasStories: Bool = false, isVerified: Bool = false, userId: String = "", externalUrl: String? = nil) {
        self.username = username
        self.fullName = fullName
        self.biography = biography
        self.picUrl = picUrl
        self.profileUrl = profileUrl
        self.mediaCount = mediaCount
        self.followerCount = followerCount
        self.followingCount = followingCount
        self.isPrivate = isPrivate
        self.hasStories = hasStories
        self.isVerified = isVerified
        self.userId = userId
        self.externalUrl = externalUrl
    }
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: IgKey.self)
        username = c.value("username", "")
        fullName = c.value("fullName", "")
        biography = c.optional("biography")
        picUrl = c.optional("picUrl")
        profileUrl = c.value("profileUrl", "")
        mediaCount = c.value("mediaCount", 0)
        followerCount = c.value("followerCount", 0)
        followingCount = c.value("followingCount", 0)
        isPrivate = c.value("isPrivate", false)
        hasStories = c.value("hasStories", false)
        isVerified = c.value("isVerified", false)
        userId = c.identifier("userId")
        externalUrl = c.optional("externalUrl")
    }
}

struct IgHighlight: Decodable, Equatable, Sendable, Identifiable {
    var id: String = ""
    var title: String = ""
    var coverUrl: String? = nil
    var mediaCount: Int? = nil
    init(id: String = "", title: String = "", coverUrl: String? = nil, mediaCount: Int? = nil) {
        self.id = id
        self.title = title
        self.coverUrl = coverUrl
        self.mediaCount = mediaCount
    }
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: IgKey.self)
        id = c.identifier("id")
        title = c.value("title", "")
        coverUrl = c.optional("coverUrl")
        mediaCount = c.optional("mediaCount")
    }
}

struct IgResignBody: Encodable, Sendable { var urls: [String] }
struct IgResigned: Decodable, Sendable { var urls: [String: String] = [:] }

struct IgProfileBundle: Decodable, Equatable, Sendable {
    var profile: IgProfile = IgProfile()
    var posts: IgPage = IgPage()
    var highlights: [IgHighlight] = []
    /// The server answered from its store and is asking the backend behind it: ask again shortly.
    var refreshing = false
    init(profile: IgProfile = IgProfile(), posts: IgPage = IgPage(), highlights: [IgHighlight] = []) {
        self.profile = profile
        self.posts = posts
        self.highlights = highlights
    }
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: IgKey.self)
        refreshing = c.value("refreshing", false)
        profile = c.value("profile", IgProfile())
        posts = c.value("posts", IgPage())
        highlights = c.value("highlights", [])
    }
}

struct IgRecent: Decodable, Equatable, Sendable, Identifiable {
    var username: String = ""
    var fullName: String = ""
    var isVerified: Bool = false
    var picUrl: String? = nil
    var openedAt: Double = 0
    var id: String { username }
    init(username: String = "", fullName: String = "", isVerified: Bool = false, picUrl: String? = nil, openedAt: Double = 0) {
        self.username = username
        self.fullName = fullName
        self.isVerified = isVerified
        self.picUrl = picUrl
        self.openedAt = openedAt
    }
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: IgKey.self)
        username = c.value("username", "")
        fullName = c.value("fullName", "")
        isVerified = c.value("isVerified", false)
        picUrl = c.optional("picUrl")
        openedAt = c.value("openedAt", 0)
    }
}

struct IgWatch: Decodable, Equatable, Sendable, Identifiable {
    var username: String = ""
    var autoSave: Bool = true
    var lastCheck: Double? = nil
    var nextCheck: Double? = nil
    var newStories: Int = 0
    var newPosts: Int = 0
    var isPrivate: Bool? = nil
    var fullName: String = ""
    var picUrl: String? = nil
    var isVerified: Bool = false
    var gone: Bool = false
    var status: String? = nil
    var archived: Int = 0
    var id: String { username }
    init(username: String = "", autoSave: Bool = true, lastCheck: Double? = nil, nextCheck: Double? = nil, newStories: Int = 0, newPosts: Int = 0, isPrivate: Bool? = nil, fullName: String = "", picUrl: String? = nil, isVerified: Bool = false, gone: Bool = false, status: String? = nil, archived: Int = 0) {
        self.username = username
        self.autoSave = autoSave
        self.lastCheck = lastCheck
        self.nextCheck = nextCheck
        self.newStories = newStories
        self.newPosts = newPosts
        self.isPrivate = isPrivate
        self.fullName = fullName
        self.picUrl = picUrl
        self.isVerified = isVerified
        self.gone = gone
        self.status = status
        self.archived = archived
    }
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: IgKey.self)
        username = c.value("username", "")
        autoSave = c.value("autoSave", true)
        lastCheck = c.optional("lastCheck")
        nextCheck = c.optional("nextCheck")
        newStories = c.value("newStories", 0)
        newPosts = c.value("newPosts", 0)
        isPrivate = c.optional("private")
        fullName = c.value("fullName", "")
        picUrl = c.optional("picUrl")
        isVerified = c.value("isVerified", false)
        gone = c.value("gone", false)
        status = c.optional("status")
        archived = c.value("archived", 0)
    }
}

struct IgWatchSettings: Decodable, Equatable, Sendable {
    var intervalHours: Int = 23
    var defaultHours: Int = 23
    var jitterMinutes: Int = 30
    var choices: [Int] = [6, 12, 23]
    init(intervalHours: Int = 23, defaultHours: Int = 23, jitterMinutes: Int = 30, choices: [Int] = [6, 12, 23]) {
        self.intervalHours = intervalHours
        self.defaultHours = defaultHours
        self.jitterMinutes = jitterMinutes
        self.choices = choices
    }
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: IgKey.self)
        intervalHours = c.value("intervalHours", 23)
        defaultHours = c.value("defaultHours", 23)
        jitterMinutes = c.value("jitterMinutes", 30)
        choices = c.value("choices", [6, 12, 23])
    }
}

struct IgBadges: Decodable, Equatable, Sendable {
    var total: Int = 0
    var accounts: [String: Int] = [:]
    init(total: Int = 0, accounts: [String: Int] = [:]) {
        self.total = total
        self.accounts = accounts
    }
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: IgKey.self)
        total = c.value("total", 0)
        accounts = c.value("accounts", [:])
    }
}

struct IgSaved: Decodable, Equatable, Sendable {
    var index: Int = 0
    var itemId: String = ""
    var duplicate: Bool = false
    init(index: Int = 0, itemId: String = "", duplicate: Bool = false) {
        self.index = index
        self.itemId = itemId
        self.duplicate = duplicate
    }
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: IgKey.self)
        index = c.value("index", 0)
        itemId = c.identifier("itemId")
        duplicate = c.value("duplicate", false)
    }
}

struct IgFailed: Decodable, Equatable, Sendable {
    var index: Int = 0
    var error: String = ""
    init(index: Int = 0, error: String = "") {
        self.index = index
        self.error = error
    }
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: IgKey.self)
        index = c.value("index", 0)
        error = c.value("error", "")
    }
}

struct IgSaveResult: Decodable, Equatable, Sendable {
    var saved: [IgSaved] = []
    var failed: [IgFailed] = []
    init(saved: [IgSaved] = [], failed: [IgFailed] = []) {
        self.saved = saved
        self.failed = failed
    }
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: IgKey.self)
        saved = c.value("saved", [])
        failed = c.value("failed", [])
    }
}
