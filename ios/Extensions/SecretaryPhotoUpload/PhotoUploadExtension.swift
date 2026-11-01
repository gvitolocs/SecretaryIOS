import ExtensionFoundation
import Foundation
import os
import Photos
import Synchronization

// iOS's background upload extension for the photo backup (iOS 27, `com.apple.photos.background-upload`).
// iOS calls `processJobs()` when there is upload work, and performs the uploads itself, also while
// Secretary is closed. Each new photo (and Live Photo motion half) becomes one upload job to
// lucina-media's `POST /v1/media/bgupload` (crates/lucina-media/src/bgupload.rs), with the same asset
// ids and metadata the app's own backup sends (PhotoBackup.swift).
//
// - Only photos added after the extension was turned on (and edits of photos it sent): the app keeps the
//   backlog. Its state (change token, jobs, what it sent) is a JSON file in its own container.
// - Sent photos are handed to the app through the shared keychain (`BackgroundUpload.Sent`).
// - A failed job is retried once with a fresh request; after that the app's backup sends the photo.

/// `log stream --predicate 'subsystem == "tech.getsecretary.app" AND category == "PhotoUpload"'`
private let log = Logger(subsystem: "tech.getsecretary.app", category: "PhotoUpload")

@main
final class PhotoUploadExtension: PHBackgroundResourceUploadJobExtension {
    private let cancelled = Atomic<Bool>(false)

    required init() {}

    func processJobs() async -> PHBackgroundResourceUploadProcessingResult {
        cancelled.store(false, ordering: .releasing)
        guard let config = BackgroundUpload.readConfig() else {
            log.info("PhotoUpload no configuration; nothing to do")
            return .completed
        }
        let uploader = Uploader(config: config, cancelled: { [unowned self] in self.cancelled.load(ordering: .acquiring) })
        defer { uploader.state.save() }
        do {
            try await uploader.retryFailed()
            try await uploader.acknowledgeFinished()
            let more = try await uploader.createJobs()
            return more ? .processing : .completed
        } catch PHPhotosError.limitExceeded {
            log.info("PhotoUpload job limit reached; more later")
            return .processing
        } catch {
            log.error("PhotoUpload failed: \(String(describing: error), privacy: .public)")
            return .failure
        }
    }

    func willTerminate() async {
        cancelled.store(true, ordering: .releasing)
    }
}

/// What the extension remembers between runs.
private struct UploadState: Codable {
    var generation: String?
    /// PhotoKit change token (archived) the next run reads changes from.
    var token: Data?
    /// Assets found in the changes that have no job yet.
    var pending: [String] = []
    /// Job local identifier -> what it uploads.
    var jobs: [String: Job] = [:]
    /// Ledger key -> signature of what was uploaded.
    var sent: [String: String] = [:]

    struct Job: Codable {
        var key: String
        var signature: String
        var meta: String
        var modified: Date?
        var favorite: Bool?
    }

    private static var url: URL {
        let dir = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        return dir.appendingPathComponent("photo-upload-state.json")
    }

    static func load() -> UploadState {
        (try? Data(contentsOf: url)).flatMap { try? JSONDecoder().decode(UploadState.self, from: $0) } ?? UploadState()
    }

    func save() {
        let url = Self.url
        try? FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        if let data = try? JSONEncoder().encode(self) { try? data.write(to: url, options: .atomic) }
    }
}

/// The metadata lucina-media reads (its `ClientMeta`), as PhotoBackup sends it.
private struct Meta: Encodable {
    var filename: String
    var mime: String?
    var takenAt: String?
    var lat: Double?
    var lon: Double?
    var width: Int?
    var height: Int?
    var durationMs: Int?
    var device: String
    var assetId: String
    var livePairAsset: String?
    var liveMotion: Bool?
    var favorite: Bool?
    var screenshot: Bool?
    var source = "phone-ios"

    enum CodingKeys: String, CodingKey {
        case filename, mime, lat, lon, width, height, device, favorite, screenshot, source
        case takenAt = "taken_at"
        case durationMs = "duration_ms"
        case assetId = "asset_id"
        case livePairAsset = "live_pair_asset"
        case liveMotion = "live_motion"
    }
}

/// Holds a value set inside a `performChanges` block.
private final class Box<T>: @unchecked Sendable {
    var value: T
    init(_ value: T) { self.value = value }
}

private final class Uploader {
    let config: BackgroundUpload.Config
    let cancelled: () -> Bool
    var state: UploadState
    private let library = PHPhotoLibrary.shared()

    private static let iso: ISO8601DateFormatter = {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return f
    }()

    init(config: BackgroundUpload.Config, cancelled: @escaping () -> Bool) {
        self.config = config
        self.cancelled = cancelled
        var state = UploadState.load()
        if state.generation != config.generation {
            // Turned on (again): only what comes after.
            state = UploadState(generation: config.generation, token: config.startToken)
        }
        self.state = state
    }

    private func destination(meta: String) -> URLRequest? {
        guard let base = URL(string: config.baseURL) else { return nil }
        var request = URLRequest(url: base.appendingPathComponent("v1/media/bgupload"))
        request.httpMethod = "POST"
        request.setValue("Bearer " + config.accessKey, forHTTPHeaderField: "Authorization")
        request.setValue("application/octet-stream", forHTTPHeaderField: "Content-Type")
        request.setValue(meta, forHTTPHeaderField: "X-Lucina-Meta")
        return request
    }

    /// Failed jobs not retried yet: once more, with the current key.
    func retryFailed() async throws {
        let jobs = PHAssetResourceUploadJob.fetchJobs(action: .retry, options: nil)
        guard jobs.count > 0 else { return }
        var retries: [(PHAssetResourceUploadJob, URLRequest?)] = []
        jobs.enumerateObjects { job, _, _ in
            let fresh = self.state.jobs[job.localIdentifier].flatMap { self.destination(meta: $0.meta) }
            log.info("PhotoUpload retrying \(job.localIdentifier, privacy: .public): \(String(describing: job.error), privacy: .public)")
            retries.append((job, fresh))
        }
        let work = retries
        try await library.performChanges {
            for (job, request) in work {
                PHAssetResourceUploadJobChangeRequest(for: job)?.retry(destination: request)
            }
        }
    }

    /// Finished jobs: record what was sent (for the app too), then free their places.
    func acknowledgeFinished() async throws {
        let jobs = PHAssetResourceUploadJob.fetchJobs(action: .acknowledge, options: nil)
        guard jobs.count > 0 else { return }
        var finished: [PHAssetResourceUploadJob] = []
        jobs.enumerateObjects { job, _, _ in
            finished.append(job)
            guard let info = self.state.jobs.removeValue(forKey: job.localIdentifier) else { return }
            guard job.state == .succeeded else {
                // The app's backup sends it.
                log.error("PhotoUpload \(info.key, privacy: .public) failed: \(String(describing: job.error), privacy: .public)")
                return
            }
            self.state.sent[info.key] = info.signature
            if let item = job.responseHeaderFields?["x-lucina-item"] {
                BackgroundUpload.recordSent(.init(key: info.key, itemId: item, modified: info.modified,
                                                  signature: info.signature, favorite: info.favorite))
            }
            log.info("PhotoUpload sent \(info.key, privacy: .public)")
        }
        let done = finished
        try await library.performChanges {
            for job in done { PHAssetResourceUploadJobChangeRequest(for: job)?.acknowledge() }
        }
    }

    private struct Work {
        let key: String
        let asset: PHAsset
        let resource: PHAssetResource
        let motion: Bool
        let pairKey: String?
    }

    /// Jobs for the photos added (or edited) since the last run. True when some are left for later.
    func createJobs() async throws -> Bool {
        let now = try NSKeyedArchiver.archivedData(withRootObject: library.currentChangeToken, requiringSecureCoding: true)
        guard let saved = state.token,
              let token = try NSKeyedUnarchiver.unarchivedObject(ofClass: PHPersistentChangeToken.self, from: saved) else {
            state.token = now
            return false
        }
        var identifiers = Set(state.pending)
        do {
            for change in try library.fetchPersistentChanges(since: token) {
                let details = try change.changeDetails(for: .asset)
                identifiers.formUnion(details.insertedLocalIdentifiers)
                // Edits of photos this extension sent; older photos' edits are the app's.
                identifiers.formUnion(details.updatedLocalIdentifiers.filter { state.sent[$0] != nil })
            }
        } catch PHPhotosError.persistentChangeTokenExpired {
            log.info("PhotoUpload change token expired; starting from now")
            state.token = now
            state.pending = []
            return false
        }
        state.token = now
        state.pending = Array(identifiers)
        guard !identifiers.isEmpty else { return false }

        let options = PHFetchOptions()
        options.sortDescriptors = [NSSortDescriptor(key: "creationDate", ascending: true)]
        options.includeHiddenAssets = false
        let types: [PHAssetMediaType] = config.includeVideos ? [.image, .video] : [.image]
        options.predicate = NSPredicate(format: "mediaType IN %@", types.map(\.rawValue))
        var assets: [PHAsset] = []
        PHAsset.fetchAssets(withLocalIdentifiers: Array(identifiers), options: options).enumerateObjects { a, _, _ in assets.append(a) }
        // Deleted or filtered out: nothing to do for them.
        var remaining = Set(assets.map(\.localIdentifier))
        state.pending = Array(remaining)
        let inFlight = Set(state.jobs.values.map(\.key))

        for asset in assets {
            if cancelled() { return true }
            let key = asset.localIdentifier
            let resources = PHAssetResource.assetResources(for: asset)
            var work: [Work] = []
            let paired = config.includeVideos ? BackgroundUpload.pairedResource(resources) : nil
            if let main = BackgroundUpload.mainResource(asset, resources),
               state.sent[key] != BackgroundUpload.signature(main), !inFlight.contains(key) {
                work.append(Work(key: key, asset: asset, resource: main, motion: false, pairKey: paired == nil ? nil : key + "#motion"))
            }
            if let paired, state.sent[key + "#motion"] != BackgroundUpload.signature(paired), !inFlight.contains(key + "#motion") {
                work.append(Work(key: key + "#motion", asset: asset, resource: paired, motion: true, pairKey: key))
            }
            for w in work { try await createJob(w) }
            remaining.remove(key)
            state.pending = Array(remaining)
        }
        return false
    }

    private func createJob(_ w: Work) async throws {
        var meta = Meta(filename: w.resource.originalFilename, device: config.deviceId, assetId: w.key)
        let uti = w.resource.uniformTypeIdentifier
        meta.mime = uti.isEmpty ? nil : BackgroundUpload.mime(forUTI: uti)
        if let d = w.asset.creationDate { meta.takenAt = Self.iso.string(from: d) }
        if let loc = w.asset.location {
            meta.lat = loc.coordinate.latitude
            meta.lon = loc.coordinate.longitude
        }
        if !w.motion {
            meta.width = w.asset.pixelWidth
            meta.height = w.asset.pixelHeight
            meta.favorite = w.asset.isFavorite
            meta.screenshot = w.asset.mediaSubtypes.contains(.photoScreenshot)
        }
        if w.asset.mediaType == .video { meta.durationMs = Int(w.asset.duration * 1000) }
        meta.livePairAsset = w.pairKey
        meta.liveMotion = w.motion ? true : nil
        let encoded = try JSONEncoder().encode(meta).base64EncodedString()
            .replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
        guard let request = destination(meta: encoded) else { return }
        let resource = w.resource
        let created = Box<String?>(nil)
        try await library.performChanges {
            let change = PHAssetResourceUploadJobChangeRequest.creationRequestForJob(destination: request, resource: resource)
            created.value = change.placeholderForCreatedAssetResourceUploadJob?.localIdentifier
        }
        guard let id = created.value else { return }
        state.jobs[id] = .init(key: w.key, signature: BackgroundUpload.signature(w.resource), meta: encoded,
                               modified: w.asset.modificationDate, favorite: w.motion ? nil : w.asset.isFavorite)
        log.info("PhotoUpload job \(id, privacy: .public) for \(w.key, privacy: .public)")
    }
}
