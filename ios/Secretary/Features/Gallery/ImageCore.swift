import CoreGraphics
import Foundation
import os
import QuartzCore
import SecretaryCore
import SwiftUI

// The Rust image viewer core (`secretary-image` through `secretary-ffi`): every picture in the
// Gallery is decoded by the same code the web and Android use, from the bytes the server
// streams. A grid tile is a 256 px JPEG XL (~9 KB) shown after its ThumbHash placeholder; the
// viewer streams the 1440 px progressive JPEG XL and redraws as each pass arrives.

private let log = Logger(subsystem: "tech.getsecretary.app", category: "GalleryImages")
private let signposter = OSSignposter(subsystem: "tech.getsecretary.app", category: "GalleryImages")

/// `-SecretaryGallerySmooth NO`: the first viewer (no prefetch, every progressive pass shown), for
/// before/after frame-time measurements.
let gallerySmooth = UserDefaults.standard.object(forKey: "SecretaryGallerySmooth") as? Bool ?? true

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
                var request = URLRequest(url: url)
                if let token { request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization") }
                if let (data, response) = try? await URLSession.shared.data(for: request),
                   (response as? HTTPURLResponse)?.statusCode == 200,
                   let decoder = ProgressiveDecoder() {
                    let state = signposter.beginInterval("prefetch-decode")
                    if let (img, _) = try? decoder.feed(data, done: true) {
                        GalleryImageCache.previews.setObject(img, forKey: key as NSString, cost: img.width * img.height * 4)
                    }
                    signposter.endInterval("prefetch-decode", state)
                }
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
        log.info("GalleryFrames mode=\(gallerySmooth ? "smooth" : "legacy", privacy: .public) frames=\(sorted.count) hitches=\(self.hitches) worst_ms=\(Int(sorted.last ?? 0)) p95_ms=\(String(format: "%.1f", p95), privacy: .public)")
        durations.removeAll(keepingCapacity: true)
        hitches = 0
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

/// Streams one rendition and hands out better pictures as they decode.
@Observable
@MainActor
final class RemoteImage {
    private(set) var image: CGImage?
    private(set) var complete = false
    private(set) var failed = false
    @ObservationIgnored private var task: Task<Void, Never>?

    /// `-SecretaryImageThrottleKBps N`: feed at most N KB/s, to see progressive decoding.
    static let throttle = UserDefaults.standard.integer(forKey: "SecretaryImageThrottleKBps")

    /// `cacheKey` for tiles (tile cache); `previewKey` for full-screen previews (shared preview cache).
    /// `progressive`: show intermediate passes (at most three in smooth mode).
    func load(_ url: URL?, accessKey: String?, cacheKey: String?, previewKey: String? = nil, progressive: Bool = true) {
        guard let url, task == nil else { return }
        if let cacheKey, let hit = tileCache.object(forKey: cacheKey as NSString) {
            image = hit
            complete = true
            return
        }
        if let previewKey, let hit = GalleryImageCache.previews.object(forKey: previewKey as NSString) {
            image = hit
            complete = true
            return
        }
        let smooth = gallerySmooth
        // Network and decoding off the main thread; only finished pictures hop over.
        task = Task.detached(priority: .userInitiated) { [weak self] in
            let started = Date()
            var bytes = 0
            var firstFrame: TimeInterval?
            do {
                var request = URLRequest(url: url)
                if let accessKey { request.setValue("Bearer \(accessKey)", forHTTPHeaderField: "Authorization") }
                let (stream, response) = try await URLSession.shared.bytes(for: request)
                guard (response as? HTTPURLResponse)?.statusCode == 200, let decoder = ProgressiveDecoder() else {
                    throw URLError(.badServerResponse)
                }
                // Smooth: hand the decoder about a third of the file at a time, so a 1440 px picture is
                // rebuilt at most three times instead of every 8 KB.
                let expected = Int(response.expectedContentLength)
                let chunkSize = !progressive ? Int.max / 2 : (smooth && expected > 0 ? max(32 * 1024, expected / 3) : 8 * 1024)
                var chunk = Data()
                chunk.reserveCapacity(chunkSize)
                func push(_ done: Bool) throws {
                    bytes += chunk.count
                    let state = signposter.beginInterval("decode")
                    defer { signposter.endInterval("decode", state) }
                    if let (img, whole) = try decoder.feed(chunk, done: done) {
                        if firstFrame == nil { firstFrame = Date().timeIntervalSince(started) }
                        Task { @MainActor in
                            self?.image = img
                            self?.complete = whole
                        }
                    }
                    chunk.removeAll(keepingCapacity: true)
                }
                let throttleStep = 8 * 1024
                var sinceSleep = 0
                for try await byte in stream {
                    chunk.append(byte)
                    sinceSleep += 1
                    if Self.throttle > 0, sinceSleep >= throttleStep {
                        sinceSleep = 0
                        try await Task.sleep(for: .milliseconds(throttleStep * 1000 / (Self.throttle * 1024)))
                    }
                    if chunk.count >= chunkSize { try push(false) }
                }
                try push(true)
                if let cacheKey, let img = await self?.image {
                    tileCache.setObject(img, forKey: cacheKey as NSString, cost: img.width * img.height * 4)
                }
                if let previewKey, let img = await self?.image {
                    GalleryImageCache.previews.setObject(img, forKey: previewKey as NSString, cost: img.width * img.height * 4)
                }
                let total = Date().timeIntervalSince(started)
                log.info("GalleryImages \(url.lastPathComponent, privacy: .public) bytes=\(bytes) first_ms=\(Int((firstFrame ?? total) * 1000)) total_ms=\(Int(total * 1000))")
            } catch is CancellationError {
            } catch {
                log.error("GalleryImages failed \(url.path, privacy: .public): \(String(describing: error), privacy: .public)")
                await MainActor.run { self?.failed = true }
            }
        }
    }

    func cancel() {
        task?.cancel()
        task = nil
    }
}

/// A grid tile: ThumbHash at once, then the 256 px JPEG XL decoded in Rust.
struct RustTile: View {
    @Environment(GalleryStore.self) private var store
    let item: GalleryItem
    @State private var remote = RemoteImage()

    var body: some View {
        ZStack {
            if let img = remote.image {
                Image(decorative: img, scale: 1).resizable().scaledToFill()
            } else if let ph = GalleryImageCache.placeholder(item) {
                Image(decorative: ph, scale: 1).resizable().scaledToFill()
            } else if item.urls?.grid == nil {
                // No rendition (audio without art, files): the JPEG poster path or an icon.
                GalleryThumb(item: item)
            } else {
                Rectangle().fill(Theme.field)
            }
        }
        .onAppear {
            // Signed URLs change with every listing: cache by item (the ThumbHash changes with each version).
            remote.load(store.api().absolute(item.urls?.grid), accessKey: store.api().client.accessKey,
                        cacheKey: "grid-\(item.id)-\(item.thumbhash ?? "")")
        }
        .onDisappear { if !remote.complete { remote.cancel() } }
    }
}

/// Full screen: ThumbHash, then the progressive 1440 px preview refining live; zoom and pan by
/// the Rust view (`secretary_image_view`).
struct RustPhoto: View {
    @Environment(GalleryStore.self) private var store
    let item: GalleryItem
    @State private var remote = RemoteImage()
    @State private var view: [String: Double]?
    @State private var rect = CGRect.zero
    @State private var lastMagnify: CGFloat = 1
    @State private var lastDrag: CGSize = .zero

    var body: some View {
        GeometryReader { geo in
            let shown = remote.image ?? (gallerySmooth ? GalleryImageCache.placeholder(item) : ImageCore.placeholder(item.thumbhash))
            ZStack {
                Color.black
                if let shown {
                    Image(decorative: shown, scale: 1)
                        .resizable()
                        .interpolation(remote.image == nil ? .low : .high)
                        .frame(width: rect.width, height: rect.height)
                        .position(x: rect.midX, y: rect.midY)
                }
                if remote.image != nil, !remote.complete {
                    ProgressView().tint(.white).position(x: geo.size.width - 24, y: 24)
                }
            }
            .contentShape(Rectangle())
            .gesture(MagnifyGesture()
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
            .onTapGesture(count: 2) { p in gesture(["op": "double_tap", "x": p.x, "y": p.y], geo.size) }
            .onAppear {
                fit(geo.size)
                remote.load(store.api().absolute(item.urls?.preview ?? item.urls?.large), accessKey: store.api().client.accessKey,
                            cacheKey: nil, previewKey: gallerySmooth ? GalleryImageCache.previewKey(item) : nil)
            }
            .onChange(of: geo.size) { _, s in gesture(resize(s), s) }
            .onChange(of: remote.image?.width) { _, _ in gesture(resize(geo.size), geo.size) }
            // Smooth: a page that slid away finishes its preview into the cache (swiping back is instant).
            .onDisappear { if !gallerySmooth { remote.cancel() } }
        }
    }

    private var zoomed: Bool { (view?["scale"] ?? 1) > 1.01 }

    /// The picture's pixel size: the decoded frame, else the item's, else square.
    private var imageSize: CGSize {
        if let img = remote.image { return CGSize(width: img.width, height: img.height) }
        if let w = item.width, let h = item.height, w > 0, h > 0 { return CGSize(width: w, height: h) }
        return CGSize(width: 1, height: 1)
    }

    private func resize(_ s: CGSize) -> [String: Any] {
        ["op": "resize", "image_w": imageSize.width, "image_h": imageSize.height, "viewport_w": s.width, "viewport_h": s.height]
    }

    private func fit(_ s: CGSize) {
        apply(["new": [imageSize.width, imageSize.height, s.width, s.height]])
    }

    private func gesture(_ g: [String: Any], _ s: CGSize) {
        guard let view else { return fit(s) }
        apply(["view": view, "gesture": g])
    }

    private func apply(_ request: [String: Any]) {
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
        rect = CGRect(x: r["x"] ?? 0, y: r["y"] ?? 0, width: r["w"] ?? 0, height: r["h"] ?? 0)
    }
}
