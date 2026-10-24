import Foundation
import Network
import Observation
import UIKit

/// A recording on this iPhone and how far its upload got. Saved in Memos/queue.json, so
/// the queue survives the app being killed, the phone restarting and bad connectivity.
struct LocalMemo: Codable, Identifiable, Equatable, Sendable {
    enum Phase: String, Codable, Sendable { case pending, uploading, uploaded }

    var id: String { clientId }
    let clientId: String
    let recordedAt: Date
    let duration: Double
    let sizeBytes: Int64
    var title: String?
    var serverId: String?
    var uploadedBytes: Int64 = 0
    var phase: Phase = .pending
    var attempts = 0
    var lastError: String?
    var nextAttempt: Date?
    /// The on-device transcript reached the server.
    var deviceTranscriptSent: Bool?

    var file: URL { MemoFiles.audio(clientId) }
    var progress: Double { sizeBytes > 0 ? min(1, Double(uploadedBytes) / Double(sizeBytes)) : 0 }
}

/// Uploads recordings one at a time with the resumable protocol: create (idempotent by
/// client id) → PUT chunks at the server's offset → complete with the SHA-256. Any failure
/// keeps the bytes already sent; the next try continues from what the server has. Runs
/// when a recording finishes, when the network comes back, in the foreground, and in the
/// background refresh; each run asks iOS for background time to finish the current file.
@Observable
@MainActor
final class MemoUploadQueue {
    private(set) var items: [LocalMemo] = []
    private(set) var running = false
    private(set) var online = true

    static let chunkSize = 4 << 20
    /// Called when a memo finished uploading (the list refreshes).
    @ObservationIgnored var onUploaded: ((String) -> Void)?

    @ObservationIgnored private let api: () -> MemosAPI?
    @ObservationIgnored private let fileURL: URL
    @ObservationIgnored private let monitor = NWPathMonitor()

    init(api: @escaping () -> MemosAPI?, fileURL: URL = MemoFiles.directory.appendingPathComponent("queue.json")) {
        self.api = api
        self.fileURL = fileURL
        if let data = try? Data(contentsOf: fileURL), let saved = try? MemoTime.decoder.decode([LocalMemo].self, from: data) {
            // A run that was cut off mid-upload continues from the server's offset.
            items = saved.map { var m = $0; if m.phase == .uploading { m.phase = .pending }; return m }
        }
        monitor.pathUpdateHandler = { [weak self] path in
            Task { @MainActor in
                guard let self else { return }
                let now = path.status == .satisfied
                let cameBack = now && !self.online
                self.online = now
                if cameBack { self.retryNow() }
            }
        }
        monitor.start(queue: DispatchQueue(label: "memo-upload-path"))
    }

    var pending: [LocalMemo] { items.filter { $0.phase != .uploaded } }

    func add(_ r: FinishedRecording, title: String? = nil) {
        let size = (try? FileManager.default.attributesOfItem(atPath: r.url.path)[.size] as? Int64) ?? 0
        items.append(LocalMemo(clientId: r.clientId, recordedAt: r.startedAt, duration: r.duration, sizeBytes: size, title: title))
        save()
        kick()
    }

    func local(serverId: String) -> LocalMemo? { items.first { $0.serverId == serverId } }

    /// Clears backoffs and uploads now (network back, owner pulled to refresh).
    func retryNow() {
        for i in items.indices where items[i].phase != .uploaded { items[i].nextAttempt = nil }
        kick()
    }

    func kick() {
        guard !running else { return }
        running = true
        Task { await run() }
    }

    /// Forgets a recording that never reached the server (the owner deleted it).
    func remove(clientId: String) {
        items.removeAll { $0.clientId == clientId }
        try? FileManager.default.removeItem(at: MemoFiles.audio(clientId))
        save()
    }

    func reset() {
        items = []
        save()
    }

    private func save() {
        guard let data = try? MemoTime.encoder.encode(items) else { return }
        try? data.write(to: fileURL, options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
    }

    private func update(_ clientId: String, _ change: (inout LocalMemo) -> Void) {
        guard let i = items.firstIndex(where: { $0.clientId == clientId }) else { return }
        change(&items[i])
        save()
    }

    private func run() async {
        let background = UIApplication.shared.beginBackgroundTask(withName: "memo-upload")
        defer {
            running = false
            if background != .invalid { UIApplication.shared.endBackgroundTask(background) }
        }
        while let next = items.first(where: { $0.phase != .uploaded && ($0.nextAttempt ?? .distantPast) <= Date() }) {
            guard online, let api = api() else { return }
            do {
                try await upload(next, api: api)
            } catch is CancellationError {
                return
            } catch {
                let message = (error as? APIError)?.userMessage ?? error.localizedDescription
                MemoLog.write("upload \(next.clientId) failed: \(message)")
                update(next.clientId) { m in
                    m.phase = .pending
                    m.attempts += 1
                    m.lastError = message
                    // 5 s, 10 s, 20 s ... capped at 5 minutes.
                    let delay = min(300, 5 * pow(2, Double(min(m.attempts - 1, 6))))
                    m.nextAttempt = Date().addingTimeInterval(delay)
                }
            }
        }
        // On-device transcripts that finished after their upload.
        if online, let api = api() {
            for m in items where m.serverId != nil && m.deviceTranscriptSent != true {
                if let id = m.serverId { await sendTranscript(clientId: m.clientId, serverId: id, api: api) }
            }
        }
        // Something is waiting for its backoff: come back then.
        if let soonest = items.compactMap({ $0.phase != .uploaded ? $0.nextAttempt : nil }).min() {
            let wait = max(1, soonest.timeIntervalSinceNow)
            Task { [weak self] in
                try? await Task.sleep(for: .seconds(wait))
                self?.kick()
            }
        }
    }

    private func upload(_ m: LocalMemo, api: MemosAPI) async throws {
        update(m.clientId) { $0.phase = .uploading }
        var serverId = m.serverId
        var offset: Int64 = 0
        if serverId == nil {
            let created = try await api.create(.init(clientId: m.clientId, title: m.title, recordedAt: m.recordedAt,
                                                     timezone: TimeZone.current.identifier, durationSec: m.duration,
                                                     mime: "audio/mp4", sizeBytes: m.sizeBytes, source: "ios"))
            serverId = created.id
            offset = created.receivedBytes
            MemoLog.write("created \(created.id) for \(m.clientId) title=\(m.title ?? "nil")")
            update(m.clientId) { $0.serverId = created.id; $0.uploadedBytes = created.receivedBytes }
            if created.status != .uploading { return finish(m.clientId, serverId: created.id) }
        } else if let id = serverId {
            let d = try await api.detail(id)
            offset = d.memo.receivedBytes
            if d.memo.status != .uploading { return finish(m.clientId, serverId: id) }
        }
        guard let id = serverId else { return }
        let handle = try FileHandle(forReadingFrom: m.file)
        defer { try? handle.close() }
        var stalls = 0
        while offset < m.sizeBytes {
            try Task.checkCancellation()
            try handle.seek(toOffset: UInt64(offset))
            guard let chunk = try handle.read(upToCount: Self.chunkSize), !chunk.isEmpty else { break }
            let received = try await api.upload(id, offset: offset, chunk: chunk)
            stalls = received > offset ? 0 : stalls + 1
            if stalls >= 3 { throw APIError.network(message: String(localized: "The upload is not moving forward.")) }
            offset = received
            update(m.clientId) { $0.uploadedBytes = received }
        }
        await sendTranscript(clientId: m.clientId, serverId: id, api: api)
        let hash = try MemosAPI.sha256(of: m.file)
        _ = try await api.complete(id, sha256: hash)
        finish(m.clientId, serverId: id)
    }

    private func sendTranscript(clientId: String, serverId: String, api: MemosAPI) async {
        guard let t = MemoOnDeviceTranscriber.saved(clientId) else { return }
        do {
            try await api.deviceTranscript(serverId, t)
            update(clientId) { $0.deviceTranscriptSent = true }
            MemoLog.write("device transcript sent for \(serverId)")
        } catch {
            // Retried on the next run; the server's Whisper transcript doesn't need it.
            MemoLog.write("device transcript for \(serverId) failed: \(error)")
        }
    }

    private func finish(_ clientId: String, serverId: String) {
        update(clientId) { m in
            m.phase = .uploaded
            m.uploadedBytes = m.sizeBytes
            m.lastError = nil
            m.nextAttempt = nil
        }
        onUploaded?(serverId)
    }
}
