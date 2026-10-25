import Foundation
import UIKit

/// lucina-media's item JSON (`GET /v1/media/items/{id}`), the parts the chat bubbles need.
struct ChatMediaInfo: Codable, Equatable, Sendable {
    struct URLs: Codable, Equatable, Sendable {
        var grid: String?
        var preview: String?
        var thumb: String?
        var large: String?
        var original: String?
    }

    var id: String?
    var kind: String?
    var mime: String?
    var title: String?
    var size: Int64?
    var width: Int?
    var height: Int?
    var durationMs: Int?
    var thumbhash: String?
    var urls: URLs?

    init(id: String? = nil, kind: String? = nil, mime: String? = nil, title: String? = nil, size: Int64? = nil, width: Int? = nil,
         height: Int? = nil, durationMs: Int? = nil, thumbhash: String? = nil, urls: URLs? = nil) {
        self.id = id; self.kind = kind; self.mime = mime; self.title = title; self.size = size; self.width = width
        self.height = height; self.durationMs = durationMs; self.thumbhash = thumbhash; self.urls = urls
    }

    private enum CodingKeys: String, CodingKey {
        case id, kind, mime, title, size, width, height, durationMs, thumbhash, urls
        case durationSnake = "duration_ms"
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = c.lenientOptional(String.self, .id)
        kind = c.lenientOptional(String.self, .kind)
        mime = c.lenientOptional(String.self, .mime)
        title = c.lenientOptional(String.self, .title)
        size = c.lenientOptional(Int64.self, .size)
        width = c.lenientOptional(Int.self, .width)
        height = c.lenientOptional(Int.self, .height)
        durationMs = c.lenientOptional(Int.self, .durationMs) ?? c.lenientOptional(Int.self, .durationSnake)
        thumbhash = c.lenientOptional(String.self, .thumbhash)
        urls = c.lenientOptional(URLs.self, .urls)
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encodeIfPresent(id, forKey: .id)
        try c.encodeIfPresent(kind, forKey: .kind)
        try c.encodeIfPresent(mime, forKey: .mime)
        try c.encodeIfPresent(title, forKey: .title)
        try c.encodeIfPresent(size, forKey: .size)
        try c.encodeIfPresent(width, forKey: .width)
        try c.encodeIfPresent(height, forKey: .height)
        try c.encodeIfPresent(durationMs, forKey: .durationMs)
        try c.encodeIfPresent(thumbhash, forKey: .thumbhash)
        try c.encodeIfPresent(urls, forKey: .urls)
    }
}

/// Authenticated media for the chat: item JSON (memory + disk), renditions (JPEG XL grid/preview through
/// ImageIO, JPEG thumbs as the fallback), originals for Quick Look, and tokenised play URLs. Every request
/// carries the device token and falls back across the server's endpoints (public, then tailnet), like the
/// rest of the app.
enum ChatMedia {
    enum Rendition: String, Sendable {
        case grid, preview

        var fallbackSize: Int { self == .grid ? 256 : 1024 }
    }

    enum Failure: Error { case status(Int), unreachable, undecodable }

    private static let infos = NSCache<NSString, Box>()
    private static let images: NSCache<NSString, UIImage> = {
        let cache = NSCache<NSString, UIImage>()
        cache.totalCostLimit = 64 * 1024 * 1024
        return cache
    }()

    private final class Box {
        let info: ChatMediaInfo
        init(_ info: ChatMediaInfo) { self.info = info }
    }

    private static var root: URL? {
        FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask).first?.appendingPathComponent("ChatMedia", isDirectory: true)
    }

    private static func cacheFile(_ folder: String, _ name: String) -> URL? {
        guard let root else { return nil }
        let directory = root.appendingPathComponent(folder, isDirectory: true)
        try? FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        return directory.appendingPathComponent(MediaAPI.encode(name))
    }

    static func itemPath(_ id: String, _ suffix: String = "") -> String {
        "/v1/media/items/" + MediaAPI.encode(id) + suffix
    }

    // MARK: Item JSON

    /// Cached in memory and on disk once it has a thumbhash (before that, renditions are still being made).
    static func info(_ id: String, client: APIClient) async -> ChatMediaInfo? {
        if let box = infos.object(forKey: id as NSString) { return box.info }
        if let file = cacheFile("items", id + ".json"), let data = try? Data(contentsOf: file),
           let info = try? JSONDecoder().decode(ChatMediaInfo.self, from: data) {
            infos.setObject(Box(info), forKey: id as NSString)
            return info
        }
        guard let data = try? await fetch(itemPath(id), client: client),
              let info = try? JSONDecoder().decode(ChatMediaInfo.self, from: data) else { return nil }
        if info.thumbhash != nil {
            infos.setObject(Box(info), forKey: id as NSString)
            if let file = cacheFile("items", id + ".json"), let encoded = try? JSONEncoder().encode(info) {
                try? encoded.write(to: file, options: .atomic)
            }
        }
        return info
    }

    // MARK: Renditions

    /// `urls.grid|preview` (JPEG XL), else `/thumb?size=256|1024` (JPEG). Grid images are kept on disk.
    static func image(_ id: String, rendition: Rendition, info: ChatMediaInfo?, client: APIClient) async -> UIImage? {
        let key = "\(id)#\(rendition.rawValue)" as NSString
        if let image = images.object(forKey: key) { return image }
        if rendition == .grid, let file = cacheFile("grid", id), let data = try? Data(contentsOf: file), let image = UIImage(data: data) {
            images.setObject(image, forKey: key, cost: data.count)
            return image
        }
        let primary = (rendition == .grid ? info?.urls?.grid : info?.urls?.preview) ?? itemPath(id, "/r/" + rendition.rawValue)
        let fallback = itemPath(id, "/thumb?size=\(rendition.fallbackSize)")
        for path in [primary, fallback] {
            if Task.isCancelled { return nil }
            guard let data = try? await fetch(path, client: client), let image = await decode(data) else { continue }
            images.setObject(image, forKey: key, cost: data.count)
            if rendition == .grid, let file = cacheFile("grid", id) { try? data.write(to: file, options: .atomic) }
            return image
        }
        return nil
    }

    /// Decodes off the main thread (ImageIO reads JPEG XL on iOS 17+), forcing the decode now.
    private static func decode(_ data: Data) async -> UIImage? {
        await Task.detached(priority: .userInitiated) {
            guard let image = UIImage(data: data) else { return nil }
            return image.preparingForDisplay() ?? image
        }.value
    }

    // MARK: Originals and playback

    /// Downloads the original once (for Quick Look and saving), named like the file the owner sent.
    static func original(_ id: String, name: String?, info: ChatMediaInfo?, client: APIClient) async throws -> URL {
        let fileName = (name?.isEmpty == false ? name! : id).replacingOccurrences(of: "/", with: "_")
        guard let root else { throw Failure.unreachable }
        let directory = root.appendingPathComponent("files", isDirectory: true).appendingPathComponent(MediaAPI.encode(id), isDirectory: true)
        let target = directory.appendingPathComponent(fileName)
        if FileManager.default.fileExists(atPath: target.path) { return target }
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let data = try await fetch(info?.urls?.original ?? itemPath(id, "/original"), client: client, timeout: 300)
        try data.write(to: target, options: .atomic)
        return target
    }

    /// `POST /v1/media/items/{id}/play` → a tokenised URL AVPlayer can load without a header.
    static func playURL(_ id: String, client: APIClient) async -> URL? {
        let api = MediaAPI(client: client)
        guard let info = try? await api.play(id: id) else { return nil }
        return api.absolute(info.url)
    }

    // MARK: Transport

    static func fetch(_ path: String, client: APIClient, timeout: TimeInterval = 30) async throws -> Data {
        guard let token = client.accessKey else { throw Failure.status(401) }
        let candidates: [URL]
        if let absolute = URL(string: path), absolute.scheme != nil {
            candidates = [absolute]
        } else {
            let base = client.baseURL
            candidates = ([base] + client.endpoints.filter { $0 != base }).compactMap { endpoint in
                var root = endpoint.absoluteString
                while root.hasSuffix("/") { root.removeLast() }
                return URL(string: root + (path.hasPrefix("/") ? path : "/" + path))
            }
        }
        var lastError: Error = Failure.unreachable
        for url in candidates {
            var request = URLRequest(url: url, timeoutInterval: timeout)
            request.setValue("Bearer " + token, forHTTPHeaderField: "Authorization")
            do {
                let (data, response) = try await URLSession.shared.data(for: request)
                let status = (response as? HTTPURLResponse)?.statusCode ?? 0
                if (200..<300).contains(status) {
                    if candidates.count > 1, let endpoint = endpointOf(url, in: client) { client.markWorking(endpoint) }
                    return data
                }
                throw Failure.status(status)
            } catch let error as URLError where APIClient.unreachedCodes.contains(error.code) || error.code == .timedOut {
                lastError = error
                continue
            }
        }
        throw lastError
    }

    private static func endpointOf(_ url: URL, in client: APIClient) -> URL? {
        client.endpoints.first { url.absoluteString.hasPrefix($0.absoluteString.hasSuffix("/") ? String($0.absoluteString.dropLast()) : $0.absoluteString) }
    }
}
