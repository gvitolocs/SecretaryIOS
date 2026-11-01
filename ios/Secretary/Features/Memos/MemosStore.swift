import Foundation
import Observation

/// One line of the memo list: a recording still on its way up, or a memo on the server.
struct MemoRow: Identifiable, Equatable {
    enum Phase: Equatable {
        case uploading(Double)
        case waiting(String?)
        case processing(MemoStatus)
        case ready
        case failed(String?)
    }

    let id: String
    let serverId: String?
    let clientId: String?
    let title: String
    let recordedAt: Date
    let duration: Double
    let phase: Phase
    let preview: String?
    let actionItems: Int
}

/// Voice memos: the recorder, the upload queue, the server list and playback.
@Observable
@MainActor
final class MemosStore {
    /// Shared with `MemoRecordingCenter`: a recording started from the Lock Screen before
    /// AppModel existed is this same one.
    let recorder = MemoRecordingCenter.recorder
    let player = MemoPlayer()
    let queue: MemoUploadQueue
    private(set) var memos: [ServerMemo] = []
    private(set) var loaded = false
    var loadError: String?
    var actionError: String?
    /// The memo whose detail screen is open (`/memos/<id>` routes here).
    var openMemoId: String?
    /// On-device transcripts by client id (shown until the server's summary is there).
    private(set) var localTranscripts: [String: MemoDeviceTranscript] = [:]
    /// Recordings being transcribed on the iPhone right now.
    private(set) var transcribing: Set<String> = []

    @ObservationIgnored private let api: () -> MemosAPI?

    init(api: @escaping () -> MemosAPI?) {
        self.api = api
        queue = MemoUploadQueue(api: api)
        queue.onUploaded = { [weak self] _ in Task { await self?.refresh() } }
        for m in queue.items {
            if let t = MemoOnDeviceTranscriber.saved(m.clientId) { localTranscripts[m.clientId] = t }
        }
    }

    var rows: [MemoRow] {
        var out: [MemoRow] = []
        let serverIds = Set(memos.map(\.id))
        for m in queue.pending where m.serverId.map({ !serverIds.contains($0) }) ?? true {
            out.append(MemoRow(id: "local:\(m.clientId)", serverId: m.serverId, clientId: m.clientId,
                               title: m.title ?? String(localized: "New recording"), recordedAt: m.recordedAt,
                               duration: m.duration, phase: phase(of: m), preview: localPreview(m.clientId), actionItems: 0))
        }
        for s in memos {
            let local = queue.local(serverId: s.id)
            let phase: MemoRow.Phase
            switch s.status {
            case .uploading: phase = local.map { self.phase(of: $0) } ?? .waiting(nil)
            case .done: phase = .ready
            case .failed: phase = .failed(s.error)
            default: phase = .processing(s.status)
            }
            out.append(MemoRow(id: s.id, serverId: s.id, clientId: local?.clientId, title: s.title, recordedAt: s.recordedAt,
                               duration: s.durationSec, phase: phase, preview: s.preview ?? local.flatMap { localPreview($0.clientId) },
                               actionItems: s.actionItems ?? 0))
        }
        return out.sorted { $0.recordedAt > $1.recordedAt }
    }

    private func localPreview(_ clientId: String) -> String? {
        if transcribing.contains(clientId) { return String(localized: "Transcribing on the iPhone…") }
        return localTranscripts[clientId].map { "“\($0.text.prefix(400))”" }
    }

    private func phase(of m: LocalMemo) -> MemoRow.Phase {
        if m.phase == .uploading { return .uploading(m.progress) }
        return .waiting(queue.online ? m.lastError : String(localized: "Waiting for the network"))
    }

    var anyProcessing: Bool { memos.contains { $0.status.isProcessing } || !queue.pending.isEmpty }

    func refresh() async {
        guard let api = api() else { return }
        do {
            memos = try await api.list()
            loadError = nil
        } catch let error as APIError {
            loadError = error.userMessage
        } catch {
            loadError = error.localizedDescription
        }
        loaded = true
    }

    /// Refreshes every few seconds while a memo is on its way (the list is on screen).
    func pollWhileBusy() async {
        while !Task.isCancelled {
            try? await Task.sleep(for: .seconds(anyProcessing ? 4 : 30))
            if Task.isCancelled { break }
            await refresh()
        }
    }

    // MARK: Recording

    func record() async {
        player.pause()
        await recorder.start()
    }

    func finishRecording() {
        MemoRecordingCenter.logCost()
        guard let r = recorder.stop() else { return }
        add(r)
    }

    /// Queues a finished recording for upload and, if it is short, transcribes it here.
    func add(_ r: FinishedRecording, title: String? = nil) {
        queue.add(r, title: title)
        // Short memos: transcribe here at once; the server swaps in Whisper's later.
        guard r.duration <= MemoOnDeviceTranscriber.maxSeconds else { return }
        transcribing.insert(r.clientId)
        Task {
            if !loaded { await refresh() }
            let language = MemoOnDeviceTranscriber.language(recent: memos.compactMap(\.language))
            let t = await MemoOnDeviceTranscriber.transcribe(r.url, clientId: r.clientId, language: language)
            transcribing.remove(r.clientId)
            if let t { localTranscripts[r.clientId] = t }
            queue.kick()
        }
    }

    // MARK: Playback

    /// The local file to play: this iPhone's recording, or a cached download.
    func audioFile(for row: MemoRow) async -> URL? {
        if let c = row.clientId, let file = MemoFiles.existingAudio(c) { return file }
        guard let id = row.serverId, let api = api() else { return nil }
        let cached = MemoFiles.cached(id)
        if FileManager.default.fileExists(atPath: cached.path) { return cached }
        do {
            try await api.download(id, "audio", to: cached)
            return cached
        } catch {
            actionError = (error as? APIError)?.userMessage ?? error.localizedDescription
            return nil
        }
    }

    func play(_ row: MemoRow) async {
        if player.memoId == row.id { player.toggle(); return }
        guard let url = await audioFile(for: row) else { return }
        player.load(url, memoId: row.id)
        player.play()
    }

    // MARK: Server actions

    func detail(_ id: String) async throws -> MemoDetail {
        guard let api = api() else { throw APIError.unauthorized(code: "unauthorized", message: "") }
        return try await api.detail(id)
    }

    func transcript(_ id: String) async throws -> MemoTranscript {
        guard let api = api() else { throw APIError.unauthorized(code: "unauthorized", message: "") }
        return try await api.transcript(id)
    }

    func rename(_ id: String, to title: String) async {
        await act { api in
            let m = try await api.rename(id, title: title)
            if let i = self.memos.firstIndex(where: { $0.id == id }) { self.memos[i] = m }
        }
    }

    func resummarize(_ id: String, instructions: String?) async {
        await act { api in
            let m = try await api.resummarize(id, instructions: instructions)
            if let i = self.memos.firstIndex(where: { $0.id == id }) { self.memos[i] = m }
        }
    }

    func events(_ id: String) async throws -> MemoEventChoices {
        guard let api = api() else { throw APIError.unauthorized(code: "unauthorized", message: "") }
        return try await api.events(id)
    }

    /// Links a memo to another calendar event, or to none; Lucina summarises it again.
    func setEvent(_ id: String, eventId: String?) async throws {
        guard let api = api() else { throw APIError.unauthorized(code: "unauthorized", message: "") }
        let m = try await api.setEvent(id, eventId: eventId)
        if let i = memos.firstIndex(where: { $0.id == id }) { memos[i] = m }
    }

    func delete(_ row: MemoRow) async {
        if player.memoId == row.id { player.stop() }
        if let id = row.serverId {
            await act { api in
                try await api.delete(id)
                self.memos.removeAll { $0.id == id }
            }
        }
        if let c = row.clientId, actionError == nil { queue.remove(clientId: c) }
    }

    /// Downloads the PDF to a temporary file for sharing.
    func pdf(_ id: String, title: String) async -> URL? {
        guard let api = api() else { return nil }
        let safe = title.components(separatedBy: CharacterSet(charactersIn: "/\\:?%*|\"<>")).joined(separator: " ")
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("\(safe.prefix(80)).pdf")
        do {
            try await api.download(id, "pdf", to: url)
            return url
        } catch {
            actionError = (error as? APIError)?.userMessage ?? error.localizedDescription
            return nil
        }
    }

    private func act(_ f: (MemosAPI) async throws -> Void) async {
        guard let api = api() else { return }
        actionError = nil
        do { try await f(api) } catch let error as APIError {
            actionError = error.userMessage
        } catch {
            actionError = error.localizedDescription
        }
    }

    func reset() {
        if recorder.isActive { recorder.discard() }
        player.stop()
        memos = []
        loaded = false
        openMemoId = nil
    }
}
