import SwiftUI
import UIKit

// Pictures behind the plugins' signed URLs (`/v1/instagram/s/…`, `/v1/pixiv/s/…`, `/v1/nclient/s/…`): each carries
// its own HMAC, so no auth header is sent (p1's Caddy strips it anyway). Decoded pictures stay in memory for the
// session; the bytes go through URLCache, so a re-shown tile doesn't download again.

/// Loads and caches the plugins' pictures.
actor SignedImageLoader {
    static let shared = SignedImageLoader()

    private let memory: NSCache<NSURL, UIImage> = {
        let cache = NSCache<NSURL, UIImage>()
        cache.totalCostLimit = 150 * 1024 * 1024
        return cache
    }()
    private var inFlight: [URL: Task<UIImage?, Never>] = [:]
    private let session: URLSession = {
        let configuration = URLSessionConfiguration.default
        configuration.urlCache = URLCache(memoryCapacity: 32 * 1024 * 1024, diskCapacity: 256 * 1024 * 1024)
        configuration.requestCachePolicy = .returnCacheDataElseLoad
        configuration.httpCookieAcceptPolicy = .never
        configuration.httpShouldSetCookies = false
        configuration.httpMaximumConnectionsPerHost = 8
        return URLSession(configuration: configuration)
    }()

    nonisolated func cached(_ url: URL) -> UIImage? { memory.object(forKey: url as NSURL) }

    /// The picture, or nil when it could not be loaded (the caller shows its placeholder).
    func image(_ url: URL) async -> UIImage? {
        if let hit = memory.object(forKey: url as NSURL) { return hit }
        if let task = inFlight[url] { return await task.value }
        let session = session
        let task = Task<UIImage?, Never> {
            guard let (data, response) = try? await session.data(from: url),
                  (response as? HTTPURLResponse).map({ (200..<300).contains($0.statusCode) }) ?? false,
                  let image = UIImage(data: data) else { return nil }
            return image.preparingForDisplay() ?? image
        }
        inFlight[url] = task
        let image = await task.value
        inFlight[url] = nil
        if let image {
            let cost = Int(image.size.width * image.size.height * image.scale * image.scale * 4)
            memory.setObject(image, forKey: url as NSURL, cost: cost)
        }
        return image
    }
}

/// A signed plugin picture; `placeholder` until it loads (and when it fails).
struct SignedImage<Placeholder: View>: View {
    let url: URL?
    var contentMode: ContentMode = .fill
    @ViewBuilder var placeholder: () -> Placeholder
    @State private var image: UIImage?

    init(url: URL?, contentMode: ContentMode = .fill, @ViewBuilder placeholder: @escaping () -> Placeholder) {
        self.url = url
        self.contentMode = contentMode
        self.placeholder = placeholder
        _image = State(initialValue: url.flatMap { SignedImageLoader.shared.cached($0) })
    }

    var body: some View {
        Group {
            if let image {
                Image(uiImage: image).resizable().aspectRatio(contentMode: contentMode)
            } else {
                placeholder()
            }
        }
        .task(id: url) {
            guard let url else { image = nil; return }
            if let hit = SignedImageLoader.shared.cached(url) { image = hit; return }
            image = nil
            image = await SignedImageLoader.shared.image(url)
        }
    }
}

extension SignedImage where Placeholder == SignedImagePlaceholder {
    init(url: URL?, contentMode: ContentMode = .fill, symbol: String = "photo") {
        self.init(url: url, contentMode: contentMode) { SignedImagePlaceholder(symbol: symbol) }
    }
}

/// The field colour with a faint symbol.
struct SignedImagePlaceholder: View {
    var symbol = "photo"

    var body: some View {
        ZStack {
            Rectangle().fill(Theme.field)
            Image(systemName: symbol).foregroundStyle(Theme.textTertiary)
        }
    }
}

extension APIClient {
    /// A signed path from a plugin answer (`/v1/…/s/…`) as an absolute URL on the endpoint in use.
    /// Absolute http(s) URLs are kept; anything else is nil.
    func absolute(_ path: String?) -> URL? {
        guard let path, !path.isEmpty else { return nil }
        if path.hasPrefix("/"), !path.hasPrefix("//") { return URL(string: path, relativeTo: baseURL)?.absoluteURL }
        guard let url = URL(string: path), let scheme = url.scheme?.lowercased(), scheme == "https" || scheme == "http" else { return nil }
        return url
    }
}
