import BackgroundTasks
import Foundation
import Network
import Observation
import os
import Photos
import UIKit

// Automatic backup of the iPhone's photos and videos to the owner's server, like Google Photos.
//
// - Reads the library with PhotoKit, newest first. Originals are sent as they are (HEIC, MOV,
//   Live Photo halves); edited assets send their edited rendering as a new version of the
//   same server item (the server keeps the earlier original).
// - Transfers run in a background URLSession, so they continue while Secretary is suspended.
//   A BGProcessingTask and the app refresh task pick the work up again later.
// - Rules: Wi-Fi only (default on), only while charging (default off), include videos.
// - Never deletes anything on the iPhone. The library access is read-only in practice.

/// `log stream --predicate 'subsystem == "tech.getsecretary.app" AND category == "PhotoBackup"'` on a Mac, or
/// `pymobiledevice3 syslog live --match PhotoBackup` for a device.
private let log = Logger(subsystem: "tech.getsecretary.app", category: "PhotoBackup")

@Observable
@MainActor
final class PhotoBackup {
    static let processingIdentifier = "tech.getsecretary.app.photobackup"

    struct Settings: Codable, Equatable {
        var enabled = false
        var wifiOnly = true
        var chargingOnly = false
        var includeVideos = true
    }

    enum Status: Equatable {
        case off
        case needsAccess
        case idle
        case scanning
        case uploading(done: Int, of: Int)
        case waiting(String)
        case failed(String)
    }

    /// One phone asset (or the motion half of a Live Photo) as the backup knows it.
    struct Entry: Codable {
        /// PhotoKit `modificationDate` when it was last sent.
        var modified: Date?
        /// Which resource was sent (edited renderings have their own type).
        var signature: String?
        var itemId: String?
        var favorite: Bool?
        /// An upload in flight (survives the app being killed).
        var uploadId: String?
        var tempFile: String?
    }

    var settings: Settings {
        didSet {
            guard settings != oldValue else { return }
            save()
            if settings.enabled { Task { await requestAccessAndRun() } } else { status = .off }
            Self.scheduleProcessing(chargingOnly: settings.chargingOnly)
        }
    }
    private(set) var status: Status = .off
    private(set) var backedUp = 0
    private(set) var remaining = 0
    private(set) var lastBackup: Date?
    private(set) var lastError: String?

    @ObservationIgnored private let api: () -> GalleryAPI
    @ObservationIgnored private var ledger: [String: Entry] = [:]
    @ObservationIgnored private var running: Task<Void, Never>?
    @ObservationIgnored private let path = NWPathMonitor()
    @ObservationIgnored private var currentPath: NWPath?
    @ObservationIgnored private var observer: LibraryObserver?
    @ObservationIgnored let deviceId: String

    private static let settingsKey = "secretary.photoBackup.settings"
    private static var ledgerURL: URL {
        let dir = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        return dir.appendingPathComponent("photo-backup.json")
    }
    private static var spoolDir: URL {
        FileManager.default.temporaryDirectory.appendingPathComponent("photo-backup", isDirectory: true)
    }

    init(api: @escaping () -> GalleryAPI, defaults: UserDefaults = .standard) {
        self.api = api
        settings = (defaults.data(forKey: Self.settingsKey)).flatMap { try? JSONDecoder().decode(Settings.self, from: $0) } ?? Settings()
        deviceId = Self.stableDeviceId()
        if let data = try? Data(contentsOf: Self.ledgerURL),
           let saved = try? JSONDecoder().decode(Ledger.self, from: data) {
            ledger = saved.entries
            lastBackup = saved.lastBackup
        }
        backedUp = ledger.values.filter { $0.itemId != nil }.count
        status = settings.enabled ? .idle : .off
        // A pass that waited for Wi-Fi (or for the first path update) starts again when the network allows it.
        path.pathUpdateHandler = { [weak self] p in
            Task { @MainActor in
                guard let self else { return }
                self.currentPath = p
                if case .waiting = self.status, self.blockedReason() == nil { self.kick() }
            }
        }
        path.start(queue: .global(qos: .utility))
        UIDevice.current.isBatteryMonitoringEnabled = true
        BackgroundUploader.shared.onFinishedEvents = { [weak self] in Task { @MainActor in self?.kick() } }
    }

    private struct Ledger: Codable {
        var entries: [String: Entry]
        var lastBackup: Date?
    }

    private func save() {
        if let data = try? JSONEncoder().encode(settings) { UserDefaults.standard.set(data, forKey: Self.settingsKey) }
    }

    private func saveLedger() {
        let dir = Self.ledgerURL.deletingLastPathComponent()
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        if let data = try? JSONEncoder().encode(Ledger(entries: ledger, lastBackup: lastBackup)) {
            try? data.write(to: Self.ledgerURL, options: .atomic)
        }
    }

    /// One id per iPhone that survives reinstalls (Keychain), so edits keep versioning the same items.
    private static func stableDeviceId() -> String {
        let store = KeychainStore(service: "tech.getsecretary.app", account: "photo-backup-device")
        if let id = try? store.read(), !id.isEmpty { return id }
        let id = "ios-" + (UIDevice.current.identifierForVendor?.uuidString ?? UUID().uuidString).lowercased()
        try? store.save(id)
        return id
    }

    // MARK: Triggers

    /// App start, foreground, library change, background task: run if allowed and not running.
    func kick() {
        log.info("PhotoBackup kick enabled=\(self.settings.enabled) running=\(self.running != nil)")
        guard settings.enabled, running == nil else { return }
        running = Task { [weak self] in
            await self?.run()
            await MainActor.run { self?.running = nil }
        }
    }

    func requestAccessAndRun() async {
        let current = PHPhotoLibrary.authorizationStatus(for: .readWrite)
        let granted: PHAuthorizationStatus = current == .notDetermined
            ? await PHPhotoLibrary.requestAuthorization(for: .readWrite) : current
        log.info("PhotoBackup access status=\(granted.rawValue)")
        guard granted == .authorized || granted == .limited else {
            status = .needsAccess
            return
        }
        if observer == nil {
            observer = LibraryObserver { [weak self] in Task { @MainActor in self?.kick() } }
            PHPhotoLibrary.shared().register(observer!)
        }
        kick()
    }

    /// The BGProcessingTask / app refresh entry point. With a budget, returns after it (the pass
    /// goes on while iOS lets the app run, and transfers continue in the background session).
    func runInBackground(budget: Duration?) async {
        guard settings.enabled else { return }
        let status = PHPhotoLibrary.authorizationStatus(for: .readWrite)
        guard status == .authorized || status == .limited else { return }
        kick()
        guard let task = running else { return }
        if let budget {
            try? await Task.sleep(for: budget)
        } else {
            await withTaskCancellationHandler { await task.value } onCancel: { task.cancel() }
        }
    }

    func stop() {
        running?.cancel()
        running = nil
    }

    /// Between two assets: 3 requests each at most, so a backup uses about 36 of the 60 a minute.
    static let pace: Duration = .seconds(4)
    @ObservationIgnored private var retryTask: Task<Void, Never>?

    private func retry(after delay: Duration) {
        retryTask?.cancel()
        retryTask = Task { [weak self] in
            try? await Task.sleep(for: delay)
            guard !Task.isCancelled else { return }
            self?.kick()
        }
    }

    static func scheduleProcessing(chargingOnly: Bool) {
        let request = BGProcessingTaskRequest(identifier: processingIdentifier)
        request.requiresNetworkConnectivity = true
        request.requiresExternalPower = chargingOnly
        request.earliestBeginDate = Date(timeIntervalSinceNow: 10 * 60)
        try? BGTaskScheduler.shared.submit(request)
    }

    // MARK: Rules

    private func blockedReason() -> String? {
        if settings.wifiOnly {
            // No path update yet (right after launch): don't block; the transfers' own session enforces Wi-Fi.
            if let p = currentPath {
                if p.status != .satisfied { return String(localized: "Waiting for a network") }
                if p.isExpensive || p.isConstrained { return String(localized: "Waiting for Wi-Fi") }
            }
        }
        if settings.chargingOnly {
            let s = UIDevice.current.batteryState
            if s != .charging && s != .full { return String(localized: "Waiting for the charger") }
        }
        return nil
    }

    // MARK: The pass

    private struct Work {
        let key: String
        let asset: PHAsset
        let resource: PHAssetResource
        let motion: Bool
        let pairKey: String?
        let signature: String
    }

    private func run() async {
        status = .scanning
        let work = scan()
        log.info("PhotoBackup scan found \(work.count) to send, \(self.ledger.count) known")
        remaining = work.count
        if work.isEmpty {
            status = .idle
            return
        }
        var done = 0
        for w in work {
            if Task.isCancelled { break }
            if let reason = blockedReason() {
                log.info("PhotoBackup waiting: \(reason, privacy: .public)")
                status = .waiting(reason)
                return
            }
            status = .uploading(done: done, of: work.count)
            do {
                try await send(w)
                done += 1
                remaining = work.count - done
                lastError = nil
                // Every /v1/media request spends one of the device's 60 a minute (p1's login check):
                // leave most of them to the app itself.
                try await Task.sleep(for: Self.pace)
            } catch is CancellationError {
                break
            } catch let APIError.rateLimited(_, retryAfter, _) {
                log.info("PhotoBackup rate limited, retry in \(retryAfter ?? 60) s")
                status = .waiting(String(localized: "Pausing for the server"))
                retry(after: .seconds(max(retryAfter ?? 60, 10)))
                saveLedger()
                return
            } catch let error as APIError where error.isTransport {
                // The server or the network is away: stop, and try the whole pass again later.
                log.error("PhotoBackup paused: \(String(describing: error), privacy: .public)")
                lastError = error.localizedDescription
                status = .failed(error.localizedDescription)
                retry(after: .seconds(120))
                saveLedger()
                return
            } catch {
                log.error("PhotoBackup send failed: \(String(describing: error), privacy: .public)")
                lastError = error.localizedDescription
                // This asset only (export failed, odd file): skip it; the next pass retries it.
                done += 1
            }
        }
        status = lastError.map { .failed($0) } ?? .idle
        if done > 0 { lastBackup = Date() }
        saveLedger()
    }

    /// What needs sending: new assets, edited ones, and Live Photo motion halves.
    private func scan() -> [Work] {
        let options = PHFetchOptions()
        options.sortDescriptors = [NSSortDescriptor(key: "creationDate", ascending: false)]
        options.includeHiddenAssets = false
        let types: [PHAssetMediaType] = settings.includeVideos ? [.image, .video] : [.image]
        options.predicate = NSPredicate(format: "mediaType IN %@", types.map(\.rawValue))
        let assets = PHAsset.fetchAssets(with: options)
        var work: [Work] = []
        var favoritesChanged: [(String, String, Bool)] = []
        assets.enumerateObjects { asset, _, _ in
            let resources = PHAssetResource.assetResources(for: asset)
            let key = asset.localIdentifier
            guard let main = Self.mainResource(asset, resources) else { return }
            let signature = "\(main.type.rawValue):\(main.originalFilename)"
            let known = self.ledger[key]
            let edited = known?.modified != asset.modificationDate && known?.signature != signature
            if known?.itemId == nil || edited || known?.uploadId != nil {
                let pair = resources.contains { $0.type == .pairedVideo || $0.type == .fullSizePairedVideo } ? key + "#motion" : nil
                work.append(Work(key: key, asset: asset, resource: main, motion: false, pairKey: pair, signature: signature))
            } else if let item = known?.itemId, known?.favorite != asset.isFavorite {
                favoritesChanged.append((key, item, asset.isFavorite))
            }
            if let paired = resources.first(where: { $0.type == .fullSizePairedVideo }) ?? resources.first(where: { $0.type == .pairedVideo }),
               self.settings.includeVideos {
                let mkey = key + "#motion"
                let msig = "\(paired.type.rawValue):\(paired.originalFilename)"
                let mknown = self.ledger[mkey]
                if mknown?.itemId == nil || mknown?.signature != msig || mknown?.uploadId != nil {
                    work.append(Work(key: mkey, asset: asset, resource: paired, motion: true, pairKey: key, signature: msig))
                }
            }
        }
        if !favoritesChanged.isEmpty {
            let api = api()
            Task {
                for (key, item, fav) in favoritesChanged {
                    if (try? await api.setFavorite(item, fav)) != nil {
                        await MainActor.run { self.ledger[key]?.favorite = fav; self.saveLedger() }
                    }
                }
            }
        }
        return work
    }

    /// The edited rendering when there is one, else the original.
    private static func mainResource(_ asset: PHAsset, _ resources: [PHAssetResource]) -> PHAssetResource? {
        let order: [PHAssetResourceType] = asset.mediaType == .video ? [.fullSizeVideo, .video] : [.fullSizePhoto, .photo]
        for t in order {
            if let r = resources.first(where: { $0.type == t }) { return r }
        }
        return resources.first
    }

    private func send(_ w: Work) async throws {
        let api = api()
        var entry = ledger[w.key] ?? Entry()
        // An upload left over from an earlier run: finish it or resume it.
        if let uploadId = entry.uploadId, let temp = entry.tempFile, FileManager.default.fileExists(atPath: temp) {
            if let state = try? await api.uploadState(uploadId), !state.finished {
                do {
                    if state.offset < state.size {
                        try await transfer(uploadId: uploadId, file: URL(fileURLWithPath: temp), offset: state.offset)
                    }
                    try await complete(w, uploadId: uploadId, temp: URL(fileURLWithPath: temp), entry: &entry)
                    return
                } catch let APIError.server(status, _, _) where status == 410 || status == 404 {
                    // The server lost this upload: start the asset again (its dedupe finds the item if it is in).
                    log.info("PhotoBackup upload \(uploadId, privacy: .public) gone, starting again")
                }
            }
            try? FileManager.default.removeItem(atPath: temp)
            entry.uploadId = nil
            entry.tempFile = nil
            ledger[w.key] = entry
        }
        let temp = try await export(w.resource)
        defer { if entry.uploadId == nil { try? FileManager.default.removeItem(at: temp) } }
        let size = (try FileManager.default.attributesOfItem(atPath: temp.path)[.size] as? NSNumber)?.int64Value ?? 0
        var meta = GalleryAPI.UploadMeta()
        meta.filename = w.resource.originalFilename
        meta.mime = w.resource.uniformTypeIdentifier.isEmpty ? nil : Self.mime(forUTI: w.resource.uniformTypeIdentifier)
        if let d = w.asset.creationDate { meta.takenAt = GalleryFormat.string(d) }
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
        meta.device = deviceId
        meta.assetId = w.key
        meta.livePairAsset = w.pairKey
        meta.liveMotion = w.motion ? true : nil
        meta.source = "phone-ios"
        log.info("PhotoBackup exported \(size) bytes, creating upload")
        let created = try await api.createUpload(.init(filename: w.resource.originalFilename, size: size, meta: meta))
        if created.duplicate, let item = created.itemId {
            record(w, item: item, entry: &entry)
            return
        }
        guard let uploadId = created.uploadId else { throw APIError.decoding(message: "no upload id") }
        // Keep the exported file until the server has it.
        let kept = Self.spoolDir.appendingPathComponent(uploadId)
        try? FileManager.default.createDirectory(at: Self.spoolDir, withIntermediateDirectories: true)
        try? FileManager.default.removeItem(at: kept)
        try FileManager.default.moveItem(at: temp, to: kept)
        entry.uploadId = uploadId
        entry.tempFile = kept.path
        ledger[w.key] = entry
        saveLedger()
        // The server decodes JPEG and PNG itself; only HEIC/HEIF needs the phone's rendering.
        let uti = w.resource.uniformTypeIdentifier
        if w.asset.mediaType == .image, !w.motion, uti.contains("heic") || uti.contains("heif"),
           let jpeg = await preview(w.asset) {
            try await api.putPreview(uploadId, jpeg: jpeg)
        }
        try await transfer(uploadId: uploadId, file: kept, offset: 0)
        try await complete(w, uploadId: uploadId, temp: kept, entry: &entry)
    }

    private func complete(_ w: Work, uploadId: String, temp: URL, entry: inout Entry) async throws {
        let done = try await api().finish(uploadId)
        try? FileManager.default.removeItem(at: temp)
        entry.uploadId = nil
        entry.tempFile = nil
        record(w, item: done.itemId, entry: &entry)
    }

    private func record(_ w: Work, item: String, entry: inout Entry) {
        entry.itemId = item
        entry.modified = w.asset.modificationDate
        entry.signature = w.signature
        entry.favorite = w.motion ? nil : w.asset.isFavorite
        ledger[w.key] = entry
        backedUp = ledger.values.filter { $0.itemId != nil }.count
        saveLedger()
    }

    /// PUT the file (from `offset`) through the background session.
    private func transfer(uploadId: String, file: URL, offset: Int64) async throws {
        var source = file
        if offset > 0 {
            // Send only the missing tail.
            let tail = file.appendingPathExtension("tail")
            try? FileManager.default.removeItem(at: tail)
            FileManager.default.createFile(atPath: tail.path, contents: nil)
            let input = try FileHandle(forReadingFrom: file)
            let output = try FileHandle(forWritingTo: tail)
            try input.seek(toOffset: UInt64(offset))
            while let chunk = try input.read(upToCount: 4 << 20), !chunk.isEmpty { try output.write(contentsOf: chunk) }
            try input.close()
            try output.close()
            source = tail
        }
        defer { if source != file { try? FileManager.default.removeItem(at: source) } }
        let client = api().client
        guard var components = URLComponents(url: client.baseURL.appendingPathComponent("v1/media/uploads/\(uploadId)"),
                                              resolvingAgainstBaseURL: false) else { throw APIError.invalidURL }
        components.queryItems = [URLQueryItem(name: "offset", value: String(offset))]
        guard let url = components.url else { throw APIError.invalidURL }
        var request = URLRequest(url: url)
        request.httpMethod = "PUT"
        request.setValue("application/octet-stream", forHTTPHeaderField: "Content-Type")
        if let key = client.accessKey { request.setValue("Bearer \(key)", forHTTPHeaderField: "Authorization") }
        let status = try await BackgroundUploader.shared.upload(request, fromFile: source, cellular: !settings.wifiOnly)
        if status == 429 { throw APIError.rateLimited(code: "rate_limited", retryAfter: nil, message: "Too many requests") }
        guard (200..<300).contains(status) else { throw APIError.network(message: "Upload failed (HTTP \(status)).") }
    }

    private func export(_ resource: PHAssetResource) async throws -> URL {
        let dir = Self.spoolDir
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let url = dir.appendingPathComponent(UUID().uuidString + "-" + resource.originalFilename)
        let options = PHAssetResourceRequestOptions()
        options.isNetworkAccessAllowed = true
        try await PHAssetResourceManager.default().writeData(for: resource, toFile: url, options: options)
        return url
    }

    /// A JPEG rendering for the server's thumbnails (it can't decode HEIC).
    private func preview(_ asset: PHAsset) async -> Data? {
        await withCheckedContinuation { cont in
            let options = PHImageRequestOptions()
            options.deliveryMode = .highQualityFormat
            options.isNetworkAccessAllowed = true
            options.isSynchronous = false
            options.resizeMode = .fast
            PHImageManager.default().requestImage(for: asset, targetSize: CGSize(width: 1600, height: 1600),
                                                  contentMode: .aspectFit, options: options) { image, info in
                if (info?[PHImageResultIsDegradedKey] as? Bool) == true { return }
                cont.resume(returning: image?.jpegData(compressionQuality: 0.82))
            }
        }
    }

    private static func mime(forUTI uti: String) -> String? {
        switch uti {
        case "public.heic": "image/heic"
        case "public.jpeg": "image/jpeg"
        case "public.png": "image/png"
        case "com.apple.quicktime-movie": "video/quicktime"
        case "public.mpeg-4": "video/mp4"
        case "com.adobe.raw-image", "com.adobe.dng": "image/x-adobe-dng"
        default: nil
        }
    }
}

/// Library changes (a new photo, an edit) start a pass while the app runs.
private final class LibraryObserver: NSObject, PHPhotoLibraryChangeObserver {
    let onChange: () -> Void
    init(onChange: @escaping () -> Void) { self.onChange = onChange }
    func photoLibraryDidChange(_ changeInstance: PHChange) { onChange() }
}

/// The background URLSession for uploads: transfers keep going while the app is suspended.
final class BackgroundUploader: NSObject, URLSessionDataDelegate, @unchecked Sendable {
    static let shared = BackgroundUploader()
    static let wifiIdentifier = "tech.getsecretary.app.backup.wifi"
    static let anyIdentifier = "tech.getsecretary.app.backup.any"

    private let lock = NSLock()
    private var waiters: [String: CheckedContinuation<Int, Error>] = [:]
    /// Set by the AppDelegate when iOS relaunches the app for finished transfers.
    var systemCompletion: [String: () -> Void] = [:]
    var onFinishedEvents: (() -> Void)?

    private lazy var wifi: URLSession = make(Self.wifiIdentifier, cellular: false)
    private lazy var any: URLSession = make(Self.anyIdentifier, cellular: true)

    private func make(_ id: String, cellular: Bool) -> URLSession {
        let config = URLSessionConfiguration.background(withIdentifier: id)
        config.allowsCellularAccess = cellular
        config.allowsExpensiveNetworkAccess = cellular
        config.allowsConstrainedNetworkAccess = cellular
        config.isDiscretionary = false
        config.sessionSendsLaunchEvents = true
        config.timeoutIntervalForResource = 7 * 24 * 3600
        return URLSession(configuration: config, delegate: self, delegateQueue: nil)
    }

    /// Reconnects the sessions after a relaunch so their events are delivered.
    func reattach(_ identifier: String) {
        _ = identifier == Self.wifiIdentifier ? wifi : any
    }

    func upload(_ request: URLRequest, fromFile file: URL, cellular: Bool) async throws -> Int {
        let session = cellular ? any : wifi
        return try await withCheckedThrowingContinuation { cont in
            let task = session.uploadTask(with: request, fromFile: file)
            let key = "\(session.configuration.identifier ?? "")#\(task.taskIdentifier)"
            lock.withLock { waiters[key] = cont }
            task.resume()
        }
    }

    func urlSession(_ session: URLSession, task: URLSessionTask, didCompleteWithError error: Error?) {
        let key = "\(session.configuration.identifier ?? "")#\(task.taskIdentifier)"
        let cont = lock.withLock { waiters.removeValue(forKey: key) }
        if let error {
            cont?.resume(throwing: error)
        } else {
            cont?.resume(returning: (task.response as? HTTPURLResponse)?.statusCode ?? 0)
        }
        if cont == nil { onFinishedEvents?() }
    }

    func urlSessionDidFinishEvents(forBackgroundURLSession session: URLSession) {
        let id = session.configuration.identifier ?? ""
        DispatchQueue.main.async {
            self.systemCompletion.removeValue(forKey: id)?()
            self.onFinishedEvents?()
        }
    }
}

private extension APIError {
    /// The request never got a real answer (network, timeout, 5xx): pause the whole pass.
    var isTransport: Bool {
        switch self {
        case .network, .timeout, .hostNotFound, .lucinaOffline: return true
        // Gateways (p1 or nezopt away). A plain 500 is about this asset: skip it, keep going.
        case let .server(status, _, _): return [502, 503, 504].contains(status)
        default: return false
        }
    }
}
