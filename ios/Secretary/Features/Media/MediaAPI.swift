import Foundation
import UIKit

// The media library and player (docs/media-player.md): GET /v1/media/items, POST
// /v1/media/items/{id}/play, GET/POST /v1/media/progress. Play answers carry tokenised
// `/v1/media/s/...` URLs that AVPlayer loads without a Bearer header.

struct MediaItem: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let kind: String
    let title: String
    var mime: String?
    var album: String?
    var size: Int?
    var durationMs: Int?
    var width: Int?
    var height: Int?
    var posterUrl: String?
    var thumbUrl: String?

    private enum CodingKeys: String, CodingKey {
        case id, kind, title, mime, album, size, width, height
        case durationMs = "duration_ms"
        case posterUrl = "poster_url"
        case thumbUrl = "thumb_url"
    }

    var isVideo: Bool { kind == "video" }
    var isAudio: Bool { kind == "audio" }
    var isPhoto: Bool { kind == "photo" }
    var duration: TimeInterval? { durationMs.map { TimeInterval($0) / 1000 } }
}

struct MediaPage: Decodable, Sendable {
    let items: [MediaItem]
    let nextCursor: String?
    let albums: [String]

    private enum CodingKeys: String, CodingKey {
        case items
        case nextCursor = "next_cursor"
        case albums
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        items = try c.decode([MediaItem].self, forKey: .items)
        nextCursor = try c.decodeIfPresent(String.self, forKey: .nextCursor)
        albums = (try? c.decodeIfPresent([String].self, forKey: .albums)) ?? []
    }
}

struct SubtitleTrack: Decodable, Identifiable, Hashable, Sendable {
    let id: String
    let lang: String?
    let label: String
    let forced: Bool
    let url: String

    var displayName: String {
        guard let lang, let name = Locale.current.localizedString(forLanguageCode: lang), !label.localizedCaseInsensitiveContains(name)
        else { return label }
        return label == lang ? name.capitalized : "\(label) (\(name.capitalized))"
    }
}

/// A resume position, same shape as the Rust core and the server.
struct ProgressEntry: Codable, Hashable, Sendable {
    let id: String
    let positionMs: Int
    let durationMs: Int
    let finished: Bool
    let updatedAt: Int
    let device: String?

    private enum CodingKeys: String, CodingKey {
        case id, finished, device
        case positionMs = "position_ms"
        case durationMs = "duration_ms"
        case updatedAt = "updated_at"
    }
}

struct PlayInfo: Decodable, Sendable {
    let id: String
    let kind: String
    let title: String
    /// `direct`, `remux` or `transcode`.
    let mode: String
    let url: String
    let posterUrl: String?
    let durationMs: Int?
    let subtitles: [SubtitleTrack]
    let bitmapSubtitles: Int
    let expiresAt: Int
    let progress: ProgressEntry?
    let resumeMs: Int

    private enum CodingKeys: String, CodingKey {
        case id, kind, title, mode, url, subtitles, progress
        case posterUrl = "poster_url"
        case durationMs = "duration_ms"
        case bitmapSubtitles = "bitmap_subtitles"
        case expiresAt = "expires_at"
        case resumeMs = "resume_ms"
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        kind = try c.decode(String.self, forKey: .kind)
        title = try c.decode(String.self, forKey: .title)
        mode = try c.decode(String.self, forKey: .mode)
        url = try c.decode(String.self, forKey: .url)
        posterUrl = try c.decodeIfPresent(String.self, forKey: .posterUrl)
        durationMs = try c.decodeIfPresent(Int.self, forKey: .durationMs)
        subtitles = (try? c.decodeIfPresent([SubtitleTrack].self, forKey: .subtitles)) ?? []
        bitmapSubtitles = (try? c.decodeIfPresent(Int.self, forKey: .bitmapSubtitles)) ?? 0
        expiresAt = try c.decode(Int.self, forKey: .expiresAt)
        progress = try? c.decodeIfPresent(ProgressEntry.self, forKey: .progress)
        resumeMs = (try? c.decodeIfPresent(Int.self, forKey: .resumeMs)) ?? 0
    }
}

struct ProgressSync: Decodable, Sendable {
    struct Ack: Decodable, Sendable {
        let id: String
        let updatedAt: Int
        private enum CodingKeys: String, CodingKey {
            case id
            case updatedAt = "updated_at"
        }
    }

    let acks: [Ack]
    let entries: [ProgressEntry]

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        acks = (try? c.decodeIfPresent([Ack].self, forKey: .acks)) ?? []
        entries = (try? c.decodeIfPresent([ProgressEntry].self, forKey: .entries)) ?? []
    }

    private enum CodingKeys: String, CodingKey { case acks, entries }
}

struct MediaAPI: Sendable {
    let client: APIClient

    private struct PlayRequest: Encodable {
        let client: String
        let device: String
    }

    private struct ProgressBatch: Encodable {
        let entries: [ProgressEntry]
    }

    func items(kind: String?, query: String = "", cursor: String? = nil, limit: Int = 200) async throws -> MediaPage {
        var q = [URLQueryItem(name: "limit", value: String(limit))]
        if let kind { q.append(URLQueryItem(name: "kind", value: kind)) }
        if !query.isEmpty { q.append(URLQueryItem(name: "q", value: query)) }
        if let cursor { q.append(URLQueryItem(name: "cursor", value: cursor)) }
        return try await client.get("/v1/media/items", query: q)
    }

    func play(id: String) async throws -> PlayInfo {
        let device = await MainActor.run { UIDevice.current.name }
        let path = "/v1/media/items/\(Self.encode(id))/play"
        return try await client.send("POST", path, body: PlayRequest(client: "ios", device: device), idempotent: true)
    }

    func progress(since: Int) async throws -> [ProgressEntry] {
        struct Page: Decodable { let entries: [ProgressEntry] }
        let page: Page = try await client.get("/v1/media/progress", query: [URLQueryItem(name: "since", value: String(since))])
        return page.entries
    }

    func push(_ entries: [ProgressEntry]) async throws -> ProgressSync {
        try await client.send("POST", "/v1/media/progress", body: ProgressBatch(entries: entries), idempotent: true)
    }

    /// A server-relative URL from a play answer, on the endpoint that currently works.
    func absolute(_ path: String) -> URL? {
        URL(string: path, relativeTo: client.baseURL)?.absoluteURL
    }

    static func encode(_ segment: String) -> String {
        segment.addingPercentEncoding(withAllowedCharacters: .alphanumerics.union(CharacterSet(charactersIn: "-_.~"))) ?? segment
    }
}

enum MediaFormat {
    static func time(_ seconds: TimeInterval) -> String {
        guard seconds.isFinite, seconds >= 0 else { return "--:--" }
        let s = Int(seconds.rounded(.down))
        return s >= 3600 ? String(format: "%d:%02d:%02d", s / 3600, s / 60 % 60, s % 60) : String(format: "%d:%02d", s / 60, s % 60)
    }
}
