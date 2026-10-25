import CoreGraphics
import Foundation
import os
import QuartzCore
import SecretaryCore
import SwiftUI

// Local photos come from PhotoKit. Server renditions use the Rust image viewer core
// (`secretary-image` through `secretary-ffi`): a 256 px JPEG XL grid tile, then a 1440 px
// progressive preview. Encoded bytes stay on disk for offline viewing and resuming.

private let log = Logger(subsystem: "tech.getsecretary.app", category: "GalleryImages")
private let signposter = OSSignposter(subsystem: "tech.getsecretary.app", category: "GalleryImages")

/// `-SecretaryGallerySmooth NO`: the first viewer (no prefetch, every progressive pass shown), for
/// before/after frame-time measurements.
let gallerySmooth = UserDefaults.standard.object(forKey: "SecretaryGallerySmooth") == nil
    || UserDefaults.standard.bool(forKey: "SecretaryGallerySmooth")

/// Decoded pictures shared by the grid and the viewer, bounded by memory.
enum GalleryImageCache {
    /// 1440 px previews (~6 MB each decoded): the current photo and its neighbours.
    static let previews: NSCache<NSString, CGImage> = {
        let c = NSCache<NSString, CGImage>()
        c.totalCostLimit = 160 * 1024 * 1024
        return c
    }()
    /// ThumbHash placeholders (tiny), so a page never decodes one inside `body`.
    static let placeholders: NSCache<NSString, CGImage> = {
        let c = NSCache<NSString, CGImage>()
        c.countLimit = 2000
        return c
    }()

    static let sources: NSCache<NSString, NSString> = {
        let c = NSCache<NSString, NSString>()
        c.countLimit = 2000
        return c
    }()

    static func gridKey(_ item: GalleryItem) -> String { "grid-\(item.id)-\(item.thumbhash ?? "")" }

    static func previewKey(_ item: GalleryItem) -> String { "preview-\(item.id)-\(item.thumbhash ?? "")" }

    static func placeholder(_ item: GalleryItem) -> CGImage? {
        let key = "\(item.id)-\(item.thumbhash ?? "")" as NSString
        if let hit = placeholders.object(forKey: key) { return hit }
        guard let img = ImageCore.placeholder(item.thumbhash) else { return nil }
        placeholders.setObject(img, forKey: key)
        return img
    }
}

/// Fetches and decodes the previews next to the one on screen, off the main thread, two at a time,
/// so a swipe lands on a picture that is already there.
@MainActor
final class PreviewPrefetcher {
    static let shared = PreviewPrefetcher()
    private var inFlight: Set<String> = []
    private var queue: [(String, URL, String?)] = []
    private var running = 0

    func prefetch(_ items: [GalleryItem], api: GalleryAPI) {
        for item in items where item.kind == "photo" {
            let key = GalleryImageCache.previewKey(item)
            guard GalleryImageCache.previews.object(forKey: key as NSString) == nil, !inFlight.contains(key),
                  let url = api.absolute(item.urls?.preview) else { continue }
            inFlight.insert(key)
            queue.append((key, url, api.client.accessKey))
        }
        pump()
    }

    private func pump() {
        while running < 2, !queue.isEmpty {
            let (key, url, token) = queue.removeFirst()
            running += 1
            Task.detached(priority: .utility) {
                try? await GalleryRendition.load(url, accessKey: token, key: key, kind: .preview, progressive: false) { _, _, _ in }
                await MainActor.run {
                    self.inFlight.remove(key)
                    self.running -= 1
                    self.pump()
                }
            }
        }
    }
}

/// Frame times while the viewer is open (CADisplayLink on the main run loop): every 2 s,
/// `GalleryFrames frames=… hitches=… worst_ms=… p95_ms=…`. A hitch is a frame later than 1.5
/// refresh intervals.
final class FrameMeter: NSObject {
    private var link: CADisplayLink?
    private var last: CFTimeInterval = 0
    private var durations: [Double] = []
    private var hitches = 0
    private var started: CFTimeInterval = 0
    /// Page changes in this window (so a quiet window is told apart from a smooth swipe).
    var swipes = 0

    func start() {
        guard link == nil else { return }
        let l = CADisplayLink(target: self, selector: #selector(tick(_:)))
        l.add(to: .main, forMode: .common)
        link = l
    }

    @objc private func tick(_ l: CADisplayLink) {
        if last > 0 {
            let d = (l.timestamp - last) * 1000
            let interval = max((l.targetTimestamp - l.timestamp) * 1000, 1000.0 / 120)
            durations.append(d)
            if d > interval * 1.5 { hitches += 1 }
        } else {
            started = l.timestamp
        }
        last = l.timestamp
        if l.timestamp - started >= 2 { flush() }
    }

    private func flush() {
        guard !durations.isEmpty else { return }
        let sorted = durations.sorted()
        let p95 = sorted[min(sorted.count - 1, Int(Double(sorted.count) * 0.95))]
        log.notice("GalleryFrames mode=\(gallerySmooth ? "smooth" : "legacy", privacy: .public) swipes=\(self.swipes) frames=\(sorted.count) hitches=\(self.hitches) worst_ms=\(Int(sorted.last ?? 0)) p95_ms=\(String(format: "%.1f", p95), privacy: .public)")
        durations.removeAll(keepingCapacity: true)
        hitches = 0
        swipes = 0
        started = last
    }

    func stop() {
        flush()
        link?.invalidate()
        link = nil
        last = 0
    }
}

enum ImageCore {
    /// RGBA8 rows (no padding) → CGImage. Copies the bytes (the Rust buffer is reused).
    static func cgImage(width: Int, height: Int, rgba: UnsafeRawPointer) -> CGImage? {
        let data = Data(bytes: rgba, count: width * height * 4)
        guard let provider = CGDataProvider(data: data as CFData) else { return nil }
        return CGImage(width: width, height: height, bitsPerComponent: 8, bitsPerPixel: 32, bytesPerRow: width * 4,
                       space: CGColorSpace(name: CGColorSpace.sRGB)!,
                       bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.premultipliedLast.rawValue),
                       provider: provider, decode: nil, shouldInterpolate: true, intent: .defaultIntent)
    }

    /// The item's ThumbHash (base64 from the gallery JSON) as a tiny blurred picture.
    static func placeholder(_ base64: String?) -> CGImage? {
        guard let base64, let hash = Data(base64Encoded: base64) else { return nil }
        var w: UInt32 = 0, h: UInt32 = 0, len = 0
        var buf: UnsafeMutablePointer<UInt8>?
        let rc = hash.withUnsafeBytes { raw in
            secretary_image_thumbhash(raw.bindMemory(to: UInt8.self).baseAddress, hash.count, &w, &h, &buf, &len)
        }
        guard rc == 1, let buf else { return nil }
        defer { secretary_image_buffer_free(buf, len) }
        return cgImage(width: Int(w), height: Int(h), rgba: buf)
    }

    static var lastError: String {
        guard let raw = secretary_image_last_error() else { return "unknown error" }
        defer { secretary_string_free(raw) }
        return String(cString: raw)
    }
}

/// One picture being decoded. Not thread-safe: feed it from one task.
final class ProgressiveDecoder {
    private let handle: OpaquePointer

    init?() {
        guard let h = secretary_image_decoder_new() else { return nil }
        handle = h
    }

    deinit { secretary_image_decoder_free(handle) }

    /// Feeds bytes; returns the new best picture (and whether it is complete) when there is one.
    func feed(_ chunk: Data, done: Bool) throws -> (CGImage, Bool)? {
        let rc = chunk.withUnsafeBytes { raw in
            secretary_image_decoder_feed(handle, raw.bindMemory(to: UInt8.self).baseAddress, chunk.count, done)
        }
        if rc < 0 { throw NSError(domain: "ImageCore", code: Int(rc), userInfo: [NSLocalizedDescriptionKey: ImageCore.lastError]) }
        guard rc == 1 else { return nil }
        var w: UInt32 = 0, h: UInt32 = 0, complete = false
        var px: UnsafePointer<UInt8>?
        guard secretary_image_decoder_frame(handle, &w, &h, &px, &complete) == 1, let px,
              let img = ImageCore.cgImage(width: Int(w), height: Int(h), rgba: px) else { return nil }
        return (img, complete)
    }
}

/// Decoded grid tiles, kept while memory allows (a tile is ~256 KB of pixels).
private let tileCache: NSCache<NSString, CGImage> = {
    let c = NSCache<NSString, CGImage>()
    c.totalCostLimit = 96 * 1024 * 1024
    return c
}()

/// A page and its prefetch share a cache key: only one may update that key's files at a time.
private actor GalleryRenditionQueue {
    static let shared = GalleryRenditionQueue()
    private var waiters: [String: [CheckedContinuation<Void, Never>]] = [:]

    func isLoading(_ key: String) -> Bool { waiters[key] != nil }

    func acquire(_ key: String) async {
        if waiters[key] == nil { waiters[key] = []; return }
        await withCheckedContinuation { waiters[key, default: []].append($0) }
    }

    func release(_ key: String) {
        if var queued = waiters[key], !queued.isEmpty {
            let next = queued.removeFirst()
            waiters[key] = queued
            next.resume()
        } else { waiters.removeValue(forKey: key) }
    }
}

/// Shared by on-screen pictures and prefetching; a cancelled stream keeps its resumable prefix.
private enum GalleryRendition {
    static func load(_ url: URL?, accessKey: String?, key: String, kind: GalleryDiskCache.Kind,
                     progressive: Bool, receive: @escaping @Sendable (CGImage, Bool, String) async -> Void) async throws {
        if kind == .preview, await GalleryRenditionQueue.shared.isLoading(key),
           let partial = await GalleryDiskCache.shared.partial(key), let decoder = ProgressiveDecoder(),
           let (image, _) = try? decoder.feed(partial.data, done: false) {
            // A prefetch may still own the stream; its saved prefix can already serve this page.
            await receive(image, false, "partial")
        }
        await GalleryRenditionQueue.shared.acquire(key)
        do {
            try Task.checkCancellation()
            try await fetch(url, accessKey: accessKey, key: key, kind: kind, progressive: progressive, receive: receive)
            await GalleryRenditionQueue.shared.release(key)
        } catch {
            await GalleryRenditionQueue.shared.release(key)
            throw error
        }
    }

    private static func fetch(_ url: URL?, accessKey: String?, key: String, kind: GalleryDiskCache.Kind,
                              progressive: Bool, receive: @escaping @Sendable (CGImage, Bool, String) async -> Void) async throws {
        let disk = GalleryDiskCache.shared
        if let data = await disk.read(key, kind: kind), let decoder = ProgressiveDecoder() {
            if let (image, whole) = try? decoder.feed(data, done: true), whole {
                cache(image, key: key, kind: kind, source: "disk")
                await receive(image, true, "disk")
                return
            }
            await disk.remove(key, kind: kind)
        }
        guard var decoder = ProgressiveDecoder() else { throw URLError(.cannotDecodeContentData) }
        var received = Data()
        var etag: String?
        if kind == .preview, let partial = await disk.partial(key) {
            do {
                let frame = try decoder.feed(partial.data, done: false)
                received = partial.data
                etag = partial.etag
                if let (image, _) = frame { await receive(image, false, "partial") }
            } catch {
                await disk.removePartial(key)
                guard let fresh = ProgressiveDecoder() else { throw error }
                decoder = fresh
            }
        }
        let started = Date()
        var firstFrame: TimeInterval?
        do {
            try Task.checkCancellation()
            guard let url else { throw URLError(.badURL) }
            var request = URLRequest(url: url, cachePolicy: .reloadIgnoringLocalCacheData)
            if let accessKey { request.setValue("Bearer \(accessKey)", forHTTPHeaderField: "Authorization") }
            let offset = received.count
            if offset > 0, let etag {
                request.setValue("bytes=\(offset)-", forHTTPHeaderField: "Range")
                request.setValue(etag, forHTTPHeaderField: "If-Range")
            }
            let (stream, response) = try await URLSession.shared.bytes(for: request)
            guard let http = response as? HTTPURLResponse else { throw URLError(.badServerResponse) }
            if offset > 0 {
                log.notice("GalleryImages resume offset=\(offset) status=\(http.statusCode) key=\(key, privacy: .public)")
            }
            if http.statusCode == 200 {
                received = Data()
                etag = http.value(forHTTPHeaderField: "ETag")
                guard let fresh = ProgressiveDecoder() else { throw URLError(.cannotDecodeContentData) }
                decoder = fresh
                if kind == .preview { await disk.removePartial(key) }
            } else if http.statusCode == 206, offset > 0 {
                guard let range = http.value(forHTTPHeaderField: "Content-Range"),
                      range.hasPrefix("bytes \(offset)-"),
                      http.value(forHTTPHeaderField: "ETag").map({ $0 == etag }) ?? true else {
                    throw URLError(.badServerResponse)
                }
            } else { throw URLError(.badServerResponse) }
            let prefixCount = received.count
            let expected = Int(response.expectedContentLength)
            let chunkSize = !progressive ? Int.max / 2
                : (gallerySmooth && expected > 0 ? max(32 * 1024, expected / 3) : 8 * 1024)
            var chunk = Data()
            var best: CGImage?
            var whole = false
            var sinceSleep = 0
            var checkpoint = received.count
            for try await byte in stream {
                received.append(byte)
                chunk.append(byte)
                sinceSleep += 1
                if RemoteImage.throttle > 0, sinceSleep >= 8 * 1024 {
                    sinceSleep = 0
                    try await Task.sleep(for: .milliseconds(8 * 1000 / RemoteImage.throttle))
                }
                if chunk.count >= chunkSize {
                    try Task.checkCancellation()
                    let state = signposter.beginInterval("decode")
                    let frame = try decoder.feed(chunk, done: false)
                    signposter.endInterval("decode", state)
                    if let (image, complete) = frame {
                        best = image
                        whole = complete
                        if firstFrame == nil { firstFrame = Date().timeIntervalSince(started) }
                        await receive(image, false, "network")
                    }
                    chunk.removeAll(keepingCapacity: true)
                }
                if kind == .preview, received.count - checkpoint >= 256 * 1024 {
                    await disk.savePartial(received, etag: etag, key: key)
                    checkpoint = received.count
                }
            }
            try Task.checkCancellation()
            guard expected < 0 || received.count - prefixCount == expected else { throw URLError(.networkConnectionLost) }
            if let (image, complete) = try decoder.feed(chunk, done: true) {
                best = image
                whole = complete
            }
            guard let best, whole else { throw URLError(.cannotDecodeContentData) }
            await disk.write(received, key: key, kind: kind)
            cache(best, key: key, kind: kind, source: "network")
            await receive(best, true, "network")
            let total = Date().timeIntervalSince(started)
            log.notice("GalleryImages \(url.lastPathComponent, privacy: .public) bytes=\(received.count) first_ms=\(Int((firstFrame ?? total) * 1000)) total_ms=\(Int(total * 1000))")
        } catch {
            if kind == .preview {
                await disk.savePartial(received, etag: etag, key: key)
                if !received.isEmpty {
                    log.notice("GalleryImages partial saved bytes=\(received.count) key=\(key, privacy: .public)")
                }
                // A drop can happen before the next normal decoding threshold.
                if !Task.isCancelled, !received.isEmpty, let prefixDecoder = ProgressiveDecoder(),
                   let (image, _) = try? prefixDecoder.feed(received, done: false) {
                    await receive(image, false, "partial")
                }
            }
            throw error
        }
    }

    private static func cache(_ image: CGImage, key: String, kind: GalleryDiskCache.Kind, source: String) {
        let target = kind == .preview ? GalleryImageCache.previews : tileCache
        target.setObject(image, forKey: key as NSString, cost: image.width * image.height * 4)
        GalleryImageCache.sources.setObject(source as NSString, forKey: key as NSString)
    }

    static func grid(_ key: String) async -> CGImage? {
        if let hit = tileCache.object(forKey: key as NSString) { return hit }
        guard let data = await GalleryDiskCache.shared.read(key, kind: .grid),
              let decoder = ProgressiveDecoder(),
              let (image, whole) = try? decoder.feed(data, done: true), whole else { return nil }
        cache(image, key: key, kind: .grid, source: "disk")
        return image
    }
}

/// Streams one rendition and hands out better pictures as they decode.
@Observable
@MainActor
final class RemoteImage {
    private(set) var image: CGImage?
    private(set) var complete = false
    private(set) var failed = false
    @ObservationIgnored private var task: Task<Void, Never>?
    @ObservationIgnored private var generation = UUID()
    @ObservationIgnored private var logged = false
    @ObservationIgnored private var pictureKey = ""

    /// `-SecretaryImageThrottleKBps N`: feed at most N KB/s, to see progressive decoding.
    nonisolated static let throttle = UserDefaults.standard.integer(forKey: "SecretaryImageThrottleKBps")

    func load(_ url: URL?, accessKey: String?, cacheKey: String?, previewKey: String? = nil,
              progressive: Bool = true, fallbackKey: String? = nil) {
        guard task == nil, !complete else { return }
        let key = previewKey ?? cacheKey
        guard let key else { return }
        pictureKey = key
        let kind: GalleryDiskCache.Kind = previewKey == nil ? .grid : .preview
        let memory = kind == .preview ? GalleryImageCache.previews : tileCache
        if let hit = memory.object(forKey: key as NSString) {
            image = hit
            complete = true
            if kind == .preview { report(GalleryImageCache.sources.object(forKey: key as NSString).map { $0 as String } ?? "network") }
            return
        }
        failed = false
        let generation = UUID()
        self.generation = generation
        let receive: @Sendable (CGImage, Bool, String) async -> Void = { [weak self] image, whole, source in
            await self?.update(image, complete: whole, source: kind == .preview ? source : nil, generation: generation)
        }
        task = Task.detached(priority: .userInitiated) { [weak self] in
            do {
                try await GalleryRendition.load(url, accessKey: accessKey, key: key, kind: kind, progressive: progressive, receive: receive)
            } catch {
                if !Task.isCancelled {
                    let fallback: CGImage?
                    if let fallbackKey { fallback = await GalleryRendition.grid(fallbackKey) } else { fallback = nil }
                    await self?.finishFailure(fallback, generation: generation)
                    log.error("GalleryImages failed: \(String(describing: error), privacy: .public)")
                }
            }
            await self?.finish(generation: generation)
        }
    }

    private func finish(generation: UUID) {
        if self.generation == generation { task = nil }
    }

    private func update(_ image: CGImage, complete: Bool, source: String?, generation: UUID) {
        guard self.generation == generation else { return }
        self.image = image
        self.complete = complete
        if let source { report(source) }
    }

    private func report(_ source: String) {
        guard !logged else { return }
        logged = true
        log.notice("GalleryImages source=\(source, privacy: .public) key=\(self.pictureKey, privacy: .public)")
    }

    private func finishFailure(_ fallback: CGImage?, generation: UUID) {
        guard self.generation == generation else { return }
        if image == nil, let fallback {
            image = fallback
            report("disk")
        }
        failed = true
    }

    func cancel() {
        generation = UUID()
        task?.cancel()
        task = nil
    }
}

/// A grid tile: ThumbHash at once, then the 256 px JPEG XL decoded in Rust.
struct RustTile: View {
    @Environment(GalleryStore.self) private var store
    @Environment(PhotoBackup.self) private var backup
    let item: GalleryItem
    @State private var remote = RemoteImage()
    @State private var local = LocalGalleryImage()

    var body: some View {
        ZStack {
            if let img = local.image ?? remote.image {
                Image(decorative: img, scale: 1).resizable().scaledToFill()
            } else if let ph = GalleryImageCache.placeholder(item) {
                Image(decorative: ph, scale: 1).resizable().scaledToFill()
            } else if item.urls?.grid == nil, local.unavailable {
                // No rendition (audio without art, files): the JPEG poster path or an icon.
                GalleryThumb(item: item)
            } else {
                Rectangle().fill(Theme.field)
            }
        }
        .onAppear {
            let scale = UIScreen.main.scale
            local.load(item.kind == "photo" ? backup.localAsset(for: item.id) : nil,
                       size: CGSize(width: 256 * scale, height: 256 * scale), tile: true)
            if local.unavailable { loadServer() }
        }
        .onChange(of: local.unavailable) { _, unavailable in if unavailable { loadServer() } }
        .onDisappear {
            local.cancel()
            if !remote.complete { remote.cancel() }
        }
    }

    private func loadServer() {
        remote.load(store.api().absolute(item.urls?.grid), accessKey: store.api().client.accessKey,
                    cacheKey: GalleryImageCache.gridKey(item))
    }
}

/// Full screen: ThumbHash, then the progressive 1440 px preview refining live; zoom and pan by
/// the Rust view (`secretary_image_view`).
struct RustPhoto: View {
    @Environment(GalleryStore.self) private var store
    @Environment(PhotoBackup.self) private var backup
    let item: GalleryItem
    /// One tap (not a double tap): the viewer shows or hides its controls, as in Google Photos.
    var onTap: () -> Void = {}
    @State private var remote = RemoteImage()
    @State private var local = LocalGalleryImage()
    @State private var view: [String: Double]?
    @State private var rect = CGRect.zero
    @State private var lastMagnify: CGFloat = 1
    @State private var lastDrag: CGSize = .zero

    var body: some View {
        GeometryReader { geo in
            let shown = localSharp ?? remote.image ?? local.image ?? (gallerySmooth ? GalleryImageCache.placeholder(item) : ImageCore.placeholder(item.thumbhash))
            ZStack {
                Color.black
                if let shown {
                    Image(decorative: shown, scale: 1)
                        .resizable()
                        .interpolation(localSharp == nil && remote.image == nil ? .low : .high)
                        .frame(width: rect.width, height: rect.height)
                        .position(x: rect.midX, y: rect.midY)
                }
                if localSharp == nil, remote.image != nil, !remote.complete, !remote.failed {
                    ProgressView().tint(.white).position(x: geo.size.width - 24, y: 24)
                }
            }
            .contentShape(Rectangle())
            // Simultaneous: a pinch must never hold back the pager's one-finger swipe.
            .simultaneousGesture(MagnifyGesture()
                .onChanged { g in
                    gesture(["op": "pinch", "factor": g.magnification / lastMagnify, "x": g.startLocation.x, "y": g.startLocation.y], geo.size)
                    lastMagnify = g.magnification
                }
                .onEnded { _ in lastMagnify = 1 })
            // Smooth: panning only while zoomed, so the pager owns horizontal swipes when fitted.
            .simultaneousGesture(DragGesture(minimumDistance: 4)
                .onChanged { g in
                    gesture(["op": "pan", "dx": g.translation.width - lastDrag.width, "dy": g.translation.height - lastDrag.height], geo.size)
                    lastDrag = g.translation
                }
                .onEnded { _ in lastDrag = .zero }, including: gallerySmooth && !zoomed ? .subviews : .all)
            // Double tap first, so a single tap waits until it is not the start of a double one.
            .onTapGesture(count: 2) { p in gesture(["op": "double_tap", "x": p.x, "y": p.y], geo.size) }
            .onTapGesture(count: 1) { onTap() }
            .onAppear {
                fit(geo.size)
                let scale = UIScreen.main.scale
                local.load(item.kind == "photo" ? backup.localAsset(for: item.id) : nil,
                           size: CGSize(width: geo.size.width * scale, height: geo.size.height * scale), tile: false)
                if local.unavailable { loadServer() }
            }
            .onChange(of: geo.size) { _, s in gesture(resize(s), s) }
            .onChange(of: remote.image?.width) { _, _ in gesture(resize(geo.size), geo.size) }
            .onChange(of: local.pixelSize) { _, _ in gesture(resize(geo.size), geo.size) }
            .onChange(of: local.unavailable) { _, unavailable in if unavailable { loadServer() } }
            .onDisappear {
                local.cancel()
                // A server preview already in flight may finish into the cache for a smooth swipe back.
                if !gallerySmooth { remote.cancel() }
            }
        }
    }

    private func loadServer() {
        remote.load(store.api().absolute(item.urls?.preview ?? item.urls?.large), accessKey: store.api().client.accessKey,
                    cacheKey: nil, previewKey: GalleryImageCache.previewKey(item),
                    fallbackKey: GalleryImageCache.gridKey(item))
    }

    private var zoomed: Bool { (view?["scale"] ?? 1) > 1.01 }

    /// The local picture, unless PhotoKit only had a small copy and the server preview takes over.
    private var localSharp: CGImage? { local.unavailable ? nil : local.image }

    /// The picture's pixel size: the decoded frame, else the item's, else square.
    private var imageSize: CGSize {
        if localSharp != nil, local.pixelSize.width > 0 { return local.pixelSize }
        if let img = remote.image { return CGSize(width: img.width, height: img.height) }
        if let w = item.width, let h = item.height, w > 0, h > 0 { return CGSize(width: w, height: h) }
        return CGSize(width: 1, height: 1)
    }

    private func resize(_ s: CGSize) -> [String: Any] {
        ["op": "resize", "image_w": imageSize.width, "image_h": imageSize.height, "viewport_w": s.width, "viewport_h": s.height]
    }

    private func fit(_ s: CGSize) {
        apply(["new": [imageSize.width, imageSize.height, s.width, s.height], "screen_scale": UIScreen.main.scale])
    }

    private func gesture(_ g: [String: Any], _ s: CGSize) {
        guard let view else { return fit(s) }
        apply(["view": view, "gesture": g, "screen_scale": UIScreen.main.scale])
    }

    private func apply(_ request: [String: Any]) {
        var request = request
        if localSharp != nil { request["preview_px"] = local.previewPixels }
        guard let body = try? JSONSerialization.data(withJSONObject: request),
              let json = String(data: body, encoding: .utf8),
              let raw = json.withCString({ secretary_image_view($0) }) else { return }
        defer { secretary_string_free(raw) }
        guard let data = String(cString: raw).data(using: .utf8),
              let answer = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              answer["ok"] as? Bool == true,
              let v = answer["view"] as? [String: Double],
              let r = answer["rect"] as? [String: Double] else { return }
        view = v
        if answer["wants_original"] as? Bool == true { local.requestOriginal() }
        rect = CGRect(x: r["x"] ?? 0, y: r["y"] ?? 0, width: r["w"] ?? 0, height: r["h"] ?? 0)
    }
}
