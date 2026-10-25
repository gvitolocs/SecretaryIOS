import Foundation

// The photo library on the owner's server (lucina-media on nezopt, docs/media-library.md):
// GET /v1/media/items (timeline, search, albums), /v1/media/albums, and the resumable upload
// that the iPhone backup uses. Thumbnails come as tokenised `/v1/media/s/...` URLs (no header).

struct GalleryPlace: Codable, Hashable, Sendable {
    let name: String
    var admin1: String?
    var country: String?
    let lat: Double
    let lon: Double
}

struct GalleryItem: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let kind: String
    let title: String
    var caption: String?
    var mime: String?
    var size: Int?
    var takenAt: String?
    var createdAt: String?
    var width: Int?
    var height: Int?
    var durationMs: Int?
    var place: GalleryPlace?
    var camera: String?
    var favorite: Bool?
    var albums: [String]?
    var posterUrl: String?
    var thumbUrl: String?
    var album: String?
    var urls: URLs?
    /// ThumbHash (base64): decoded by the Rust core into the placeholder.
    var thumbhash: String?

    struct URLs: Codable, Hashable, Sendable {
        var original: String?
        var thumb: String?
        var large: String?
        /// JPEG XL renditions for the Rust viewer: 256 px grid tile, 1440 px progressive preview.
        var grid: String?
        var preview: String?
    }

    private enum CodingKeys: String, CodingKey {
        case id, kind, title, caption, mime, size, width, height, place, camera, favorite, albums, album, urls, thumbhash
        case takenAt = "taken_at"
        case createdAt = "created_at"
        case durationMs = "duration_ms"
        case posterUrl = "poster_url"
        case thumbUrl = "thumb_url"
    }

    var isVideo: Bool { kind == "video" }
    var isAudio: Bool { kind == "audio" }
    var date: Date? { (takenAt ?? createdAt).flatMap(GalleryFormat.parse) }

    /// The player's model, for videos and audio.
    var mediaItem: MediaItem {
        MediaItem(id: id, kind: kind, title: title, mime: mime, album: album, size: size, durationMs: durationMs,
                  width: width, height: height, posterUrl: posterUrl, thumbUrl: thumbUrl)
    }
}

struct GalleryPage: Decodable, Sendable {
    let items: [GalleryItem]
    let nextCursor: String?
    let total: Int?

    private enum CodingKeys: String, CodingKey {
        case items, total
        case nextCursor = "next_cursor"
    }
}

struct GalleryAlbum: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let title: String
    var systemKey: String?
    var coverItem: String?
    var count: Int

    private enum CodingKeys: String, CodingKey {
        case id, title, count
        case systemKey = "system_key"
        case coverItem = "cover_item"
    }
}

struct GalleryAPI: Sendable {
    let client: APIClient

    struct Filter: Hashable, Sendable {
        var kind: String?
        var album: String?
        var query: String = ""
        var favorites = false
    }

    func items(_ filter: Filter, cursor: String? = nil, limit: Int = 200) async throws -> GalleryPage {
        var q: [URLQueryItem] = [URLQueryItem(name: "limit", value: String(limit))]
        if let kind = filter.kind { q.append(URLQueryItem(name: "kind", value: kind)) }
        if let album = filter.album { q.append(URLQueryItem(name: "album", value: album)) }
        if !filter.query.trimmingCharacters(in: .whitespaces).isEmpty { q.append(URLQueryItem(name: "q", value: filter.query)) }
        if filter.favorites { q.append(URLQueryItem(name: "favorite", value: "true")) }
        if let cursor { q.append(URLQueryItem(name: "cursor", value: cursor)) }
        return try await client.get("/v1/media/items", query: q)
    }

    func albums() async throws -> [GalleryAlbum] {
        struct Page: Decodable { let albums: [GalleryAlbum] }
        let page: Page = try await client.get("/v1/media/albums")
        return page.albums
    }

    func createAlbum(title: String, items: [String]) async throws -> GalleryAlbum {
        struct Body: Encodable { let title: String; let item_ids: [String] }
        return try await client.send("POST", "/v1/media/albums", body: Body(title: title, item_ids: items))
    }

    func add(_ items: [String], toAlbum album: String) async throws {
        struct Body: Encodable { let item_ids: [String] }
        struct Out: Decodable { let added: Int }
        let _: Out = try await client.send("POST", "/v1/media/albums/\(MediaAPI.encode(album))/items", body: Body(item_ids: items))
    }

    func setFavorite(_ id: String, _ favorite: Bool) async throws -> GalleryItem {
        struct Body: Encodable { let favorite: Bool }
        return try await client.send("PATCH", "/v1/media/items/\(MediaAPI.encode(id))", body: Body(favorite: favorite))
    }

    /// Server trash only: the photo stays on the iPhone, and the original stays on the server for 30 days.
    func trash(_ id: String) async throws {
        _ = try await client.send("DELETE", "/v1/media/items/\(MediaAPI.encode(id))", body: nil)
    }

    func absolute(_ path: String?) -> URL? {
        guard let path else { return nil }
        return URL(string: path, relativeTo: client.baseURL)?.absoluteURL
    }

    // MARK: Upload (used by PhotoBackup)

    struct CreateUpload: Encodable {
        let filename: String
        let size: Int64
        let meta: UploadMeta
    }

    struct UploadMeta: Codable, Sendable {
        var filename: String?
        var mime: String?
        var takenAt: String?
        var lat: Double?
        var lon: Double?
        var width: Int?
        var height: Int?
        var durationMs: Int?
        var device: String?
        var assetId: String?
        var livePairAsset: String?
        var liveMotion: Bool?
        var favorite: Bool?
        var screenshot: Bool?
        var source: String?

        enum CodingKeys: String, CodingKey {
            case filename, mime, lat, lon, width, height, device, favorite, screenshot, source
            case takenAt = "taken_at"
            case durationMs = "duration_ms"
            case assetId = "asset_id"
            case livePairAsset = "live_pair_asset"
            case liveMotion = "live_motion"
        }
    }

    struct UploadState: Decodable, Sendable {
        let uploadId: String?
        let offset: Int64
        let size: Int64
        let finished: Bool
        let duplicate: Bool
        let itemId: String?

        enum CodingKeys: String, CodingKey {
            case offset, size, finished, duplicate
            case uploadId = "upload_id"
            case itemId = "item_id"
        }
    }

    struct Finished: Decodable, Sendable {
        let itemId: String
        let duplicate: Bool
        let version: Int

        enum CodingKeys: String, CodingKey {
            case duplicate, version
            case itemId = "item_id"
        }
    }

    func createUpload(_ body: CreateUpload) async throws -> UploadState {
        try await client.send("POST", "/v1/media/uploads", body: body, idempotent: false)
    }

    func uploadState(_ id: String) async throws -> UploadState {
        try await client.get("/v1/media/uploads/\(id)")
    }

    func putPreview(_ id: String, jpeg: Data) async throws {
        _ = try await client.send("PUT", "/v1/media/uploads/\(id)/preview", body: jpeg, timeout: 60,
                                  headers: ["Content-Type": "image/jpeg"])
    }

    func finish(_ id: String) async throws -> Finished {
        struct Empty: Encodable {}
        return try await client.send("POST", "/v1/media/uploads/\(id)/finish", body: Empty(), timeout: 300, idempotent: true)
    }
}

enum GalleryFormat {
    private static let iso: ISO8601DateFormatter = {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return f
    }()
    private static let isoPlain = ISO8601DateFormatter()

    static func parse(_ s: String) -> Date? { iso.date(from: s) ?? isoPlain.date(from: s) }
    static func string(_ d: Date) -> String { iso.string(from: d) }

    static func month(_ d: Date) -> String {
        d.formatted(.dateTime.month(.wide).year())
    }
}
