import CryptoKit
import Foundation

/// Encoded renditions survive memory pressure and app restarts. All file work stays on this actor.
actor GalleryDiskCache {
    static let shared = GalleryDiskCache()

    enum Kind: String, Hashable, Sendable {
        case grid, preview
        var limit: Int { self == .grid ? 200 * 1024 * 1024 : 1024 * 1024 * 1024 }
    }

    struct Partial: Sendable {
        let data: Data
        let etag: String
    }

    nonisolated static var rootURL: URL {
        FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("gallery", isDirectory: true)
    }

    /// Every cached rendition and prefix, before the actor is used (device self-tests start from nothing).
    nonisolated static func removeAllNow() {
        try? FileManager.default.removeItem(at: rootURL)
    }

    private let root = GalleryDiskCache.rootURL
    private var checked: Set<Kind> = []
    /// Bytes written per kind since its last prune (a prune lists the whole directory).
    private var unpruned: [Kind: Int] = [:]

    private func file(_ key: String, _ kind: Kind, _ ext: String) -> URL {
        let hash = SHA256.hash(data: Data(key.utf8)).map { String(format: "%02x", $0) }.joined()
        return root.appendingPathComponent(kind.rawValue, isDirectory: true)
            .appendingPathComponent(hash).appendingPathExtension(ext)
    }

    func read(_ key: String, kind: Kind) -> Data? {
        check(kind)
        let url = file(key, kind, "jxl")
        guard let data = try? Data(contentsOf: url) else { return nil }
        touch(url)
        return data
    }

    func partial(_ key: String) -> Partial? {
        check(.preview)
        let url = file(key, .preview, "part")
        guard let data = try? Data(contentsOf: url), !data.isEmpty,
              let etag = try? String(contentsOf: file(key, .preview, "etag"), encoding: .utf8), !etag.isEmpty else { return nil }
        touch(url)
        return Partial(data: data, etag: etag)
    }

    func write(_ data: Data, key: String, kind: Kind) {
        let url = file(key, kind, "jxl")
        try? FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        do {
            try data.write(to: url, options: .atomic)
            if kind == .preview { removePartial(key) }
        } catch { return }
        checked.insert(kind)
        pruneSoon(kind, adding: data.count)
    }

    private func pruneSoon(_ kind: Kind, adding bytes: Int) {
        unpruned[kind, default: 0] += bytes
        if unpruned[kind, default: 0] >= 16 * 1024 * 1024 {
            unpruned[kind] = 0
            prune(kind)
        }
    }

    func savePartial(_ data: Data, etag: String?, key: String) {
        guard !data.isEmpty, let etag, !etag.isEmpty,
              !FileManager.default.fileExists(atPath: file(key, .preview, "jxl").path) else { return }
        if let old = partial(key), old.etag == etag, old.data.count >= data.count { return }
        let url = file(key, .preview, "part")
        try? FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        removePartial(key)
        do {
            try etag.write(to: file(key, .preview, "etag"), atomically: true, encoding: .utf8)
            try data.write(to: url, options: .atomic)
        } catch { removePartial(key) }
        pruneSoon(.preview, adding: data.count)
    }

    func remove(_ key: String, kind: Kind) {
        try? FileManager.default.removeItem(at: file(key, kind, "jxl"))
    }

    func removePartial(_ key: String) {
        try? FileManager.default.removeItem(at: file(key, .preview, "part"))
        try? FileManager.default.removeItem(at: file(key, .preview, "etag"))
    }

    private func touch(_ url: URL) {
        try? FileManager.default.setAttributes([.modificationDate: Date()], ofItemAtPath: url.path)
    }

    private func check(_ kind: Kind) {
        if checked.insert(kind).inserted { prune(kind) }
    }

    private func prune(_ kind: Kind) {
        let dir = root.appendingPathComponent(kind.rawValue, isDirectory: true)
        guard let urls = try? FileManager.default.contentsOfDirectory(at: dir,
            includingPropertiesForKeys: [.fileSizeKey, .contentModificationDateKey]) else { return }
        // Treat a prefix and its ETag as one LRU entry.
        let entries = urls.filter { $0.pathExtension != "etag" }.compactMap { url -> (URL, Int, Date)? in
            guard let values = try? url.resourceValues(forKeys: [.fileSizeKey, .contentModificationDateKey]) else { return nil }
            let metadata = url.deletingPathExtension().appendingPathExtension("etag")
            let extra = url.pathExtension == "part"
                ? ((try? metadata.resourceValues(forKeys: [.fileSizeKey]))?.fileSize ?? 0) : 0
            return (url, (values.fileSize ?? 0) + extra, values.contentModificationDate ?? .distantPast)
        }
        var size = entries.reduce(0) { $0 + $1.1 }
        for (url, bytes, _) in entries.sorted(by: { $0.2 < $1.2 }) where size > kind.limit {
            do { try FileManager.default.removeItem(at: url) } catch { continue }
            size -= bytes
            if url.pathExtension == "part" {
                try? FileManager.default.removeItem(at: url.deletingPathExtension().appendingPathExtension("etag"))
            }
        }
        for url in urls where url.pathExtension == "etag" {
            let prefix = url.deletingPathExtension().appendingPathExtension("part")
            if !FileManager.default.fileExists(atPath: prefix.path) { try? FileManager.default.removeItem(at: url) }
        }
    }
}
