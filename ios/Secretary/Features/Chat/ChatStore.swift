import Foundation
import Observation

struct ChatJob: Codable, Equatable, Sendable {
    var id: String
    var status: String
    var action: String?
    var result: String?
    var error: String?
    var imageUrl: String?
    /// A transient polling problem, shown under the card while it keeps trying.
    var pollError: String?

    var isTerminal: Bool { JobInfo.isTerminal(status) }
}

/// The one conversation with Lucina, across every channel.
///
/// Source of truth: the server timeline (`GET /v1/timeline` pages, `/v1/timeline/stream`
/// live). Messages written here are sent with `POST /v1/chat` (`X-Secretary-Client: ios`),
/// shown optimistically and reconciled with the server's copies by `clientMessageId`,
/// `userItemId` and `lucinaItemId`. The last 50 items are cached for a fast first paint.
///
/// A server without the timeline (404) puts the store in local-only mode: the
/// conversation is kept on the phone as before, and the timeline is tried again on the
/// next foreground.
@Observable
@MainActor
final class ChatStore {
    enum Mode: Equatable { case unknown, server, localOnly }

    nonisolated static let maxCharacters = 4000
    nonisolated static let pollInterval: Duration = .seconds(3)
    nonisolated static let pageSize = 50
    nonisolated static let cacheSize = 50
    nonisolated static let sessionIdKey = "secretary.chat.sessionId"

    /// Oldest first.
    private(set) var items: [TimelineItem] = []
    /// Job cards, keyed by the Lucina item that started them.
    private(set) var jobs: [String: ChatJob] = [:]
    private(set) var mode: Mode = .unknown
    private(set) var isSending = false
    private(set) var isLoadingOlder = false
    private(set) var hasOlder = false
    /// The live stream is connected.
    private(set) var isLive = false
    private(set) var loadError: String?

    @ObservationIgnored private let api: () -> SecretaryAPI
    @ObservationIgnored private let fileURL: URL
    @ObservationIgnored private let defaults: UserDefaults
    @ObservationIgnored private var pollers: [String: Task<Void, Never>] = [:]
    @ObservationIgnored private let pollInterval: Duration
    @ObservationIgnored private var nextBefore: String?
    @ObservationIgnored private var lastEventId: String?
    @ObservationIgnored private var streamTask: Task<Void, Never>?
    @ObservationIgnored var onTransportError: ((APIError) -> Void)?
    @ObservationIgnored var onTransportSuccess: (() -> Void)?

    init(api: @escaping () -> SecretaryAPI, fileURL: URL = ChatStore.defaultFileURL, defaults: UserDefaults = .standard,
         pollInterval: Duration = ChatStore.pollInterval) {
        self.api = api
        self.fileURL = fileURL
        self.defaults = defaults
        self.pollInterval = pollInterval
        let cache = Self.load(from: fileURL)
        items = cache.items
        jobs = cache.jobs
        lastEventId = cache.lastEventId
        if cache.localOnly { mode = .localOnly }
    }

    nonisolated static var defaultFileURL: URL {
        let base = (try? FileManager.default.url(for: .applicationSupportDirectory, in: .userDomainMask, appropriateFor: nil, create: true))
            ?? FileManager.default.temporaryDirectory
        return base.appendingPathComponent("Secretary", isDirectory: true).appendingPathComponent("timeline.json")
    }

    /// Stable per install: `[A-Za-z0-9_-]{1,64}` as the server requires.
    var sessionId: String {
        if let existing = defaults.string(forKey: Self.sessionIdKey), Self.isValidSessionId(existing) { return existing }
        let fresh = "ios-" + UUID().uuidString.replacingOccurrences(of: "-", with: "").prefix(16).lowercased()
        defaults.set(fresh, forKey: Self.sessionIdKey)
        return fresh
    }

    nonisolated static func isValidSessionId(_ value: String) -> Bool {
        !value.isEmpty && value.count <= 64 && value.allSatisfy { $0.isASCII && ($0.isLetter || $0.isNumber || $0 == "_" || $0 == "-") }
    }

    /// The server accepts 1-4000 characters and no control characters other than newline and tab.
    nonisolated static func clean(_ text: String) -> String {
        let normalized = text.replacingOccurrences(of: "\r\n", with: "\n").replacingOccurrences(of: "\r", with: "\n")
        let scalars = normalized.unicodeScalars.filter { scalar in
            scalar == "\n" || scalar == "\t" || !(scalar.value < 0x20 || scalar.value == 0x7F)
        }
        return String(String.UnicodeScalarView(scalars)).trimmingCharacters(in: .whitespacesAndNewlines)
    }

    // MARK: - Timeline

    /// Loads the newest page (first paint comes from the cache). A 404 switches to local-only mode.
    func refresh() async {
        do {
            let page = try await api().timeline(limit: Self.pageSize)
            onTransportSuccess?()
            loadError = nil
            if mode != .server {
                // Coming from local-only or a cold start: the server copy replaces the cache,
                // keeping only messages still waiting to be (re)sent.
                items = items.filter { $0.isLocal && $0.localStatus != .sent }
                mode = .server
            }
            apply(page.items)
            // Only the first load decides the cursor; later refreshes just fill gaps.
            if nextBefore == nil || !hasOlder {
                nextBefore = page.nextBefore
                hasOlder = page.nextBefore != nil
            }
        } catch {
            let apiError = APIError.wrap(error)
            if apiError.status == 404 {
                mode = .localOnly
                hasOlder = false
                stopStream()
                persist()
                return
            }
            onTransportError?(apiError)
            loadError = apiError.userMessage
        }
    }

    /// Infinite scroll upward.
    func loadOlder() async {
        guard mode == .server, hasOlder, !isLoadingOlder, let cursor = nextBefore else { return }
        isLoadingOlder = true
        defer { isLoadingOlder = false }
        do {
            let page = try await api().timeline(before: cursor, limit: Self.pageSize)
            apply(page.items, persistAfter: false)
            nextBefore = page.nextBefore
            hasOlder = page.nextBefore != nil
        } catch {
            onTransportError?(APIError.wrap(error))
        }
    }

    /// Merges server items in (dedupe by id, reconcile optimistic bubbles).
    func apply(_ incoming: [TimelineItem], persistAfter: Bool = true) {
        guard !incoming.isEmpty else { return }
        let result = TimelineMerge.merge(items, incoming)
        items = result.items
        for (old, new) in result.replaced {
            if let job = jobs.removeValue(forKey: old), jobs[new] == nil { jobs[new] = job }
        }
        if persistAfter { persist() }
    }

    // MARK: - Live stream

    /// Starts or stops the SSE connection (foreground only; the view calls this on scene changes).
    func setForeground(_ active: Bool) {
        if active {
            Task {
                if mode != .server { await refresh() }
                if mode == .server { startStream() }
            }
            resumePolling()
        } else {
            stopStream()
        }
    }

    func startStream() {
        guard streamTask == nil, mode == .server else { return }
        streamTask = Task { [weak self] in
            await self?.runStream()
        }
    }

    func stopStream() {
        streamTask?.cancel()
        streamTask = nil
        isLive = false
    }

    private func runStream() async {
        // docs/server-api.md: reconnect with backoff 1 s, 2 s, 5 s, 10 s, 30 s.
        let backoff: [Duration] = [.seconds(1), .seconds(2), .seconds(5), .seconds(10), .seconds(30)]
        var attempt = 0
        while !Task.isCancelled {
            var wait = backoff[min(attempt, backoff.count - 1)]
            do {
                let bytes = try await api().client.openStream("/v1/timeline/stream", lastEventId: lastEventId)
                isLive = true
                attempt = 0
                // Fill anything missed while disconnected (in case Last-Event-ID isn't honoured).
                await refresh()
                try await Self.readEvents(bytes) { [weak self] event in
                    self?.handle(event)
                }
            } catch is CancellationError {
                break
            } catch {
                let apiError = APIError.wrap(error)
                if apiError.endsSession { onTransportError?(apiError); break }
                if apiError.status == 404 { break } // no stream on this server; pages still work
                // 429 too_many_streams says when to come back.
                if case let .rateLimited(_, retryAfter?, _) = apiError { wait = max(wait, .seconds(retryAfter)) }
            }
            attempt += 1
            isLive = false
            if Task.isCancelled { break }
            try? await Task.sleep(for: wait)
        }
        isLive = false
        streamTask = nil
    }

    /// Reads SSE lines off the byte stream (not `bytes.lines`, which drops the blank lines
    /// that end each event).
    nonisolated private static func readEvents(_ bytes: URLSession.AsyncBytes,
                                               onEvent: @MainActor @escaping (SSEEvent) -> Void) async throws {
        var parser = SSEParser()
        var buffer: [UInt8] = []
        buffer.reserveCapacity(4096)
        for try await byte in bytes {
            buffer.append(byte)
            guard byte == 0x0A || byte == 0x0D else { continue }
            for line in SSEParser.lines(from: &buffer) {
                if let event = parser.feed(line) { await onEvent(event) }
            }
        }
    }

    func handle(_ event: SSEEvent) {
        if let id = event.id { lastEventId = id }
        guard event.event == "item" || event.event == "message" else { return } // pings and unknown events
        guard let item = try? JSONDecoder().decode(TimelineItem.self, from: Data(event.data.utf8)) else { return }
        apply([item])
    }

    // MARK: - Sending

    /// Adds an optimistic bubble and sends it. Returns false when nothing was sent.
    @discardableResult
    func send(_ text: String) async -> Bool {
        await post(text) != nil
    }

    /// Sends like `send` and returns Lucina's reply text (nil when nothing was sent or it
    /// failed). Used by the voice stage to speak the answer.
    func ask(_ text: String) async -> String? {
        guard let outcome = await post(text) else { return nil }
        return outcome
    }

    /// nil: not sent. Otherwise the reply text ("" when the send failed; the bubble shows why).
    private func post(_ text: String) async -> String? {
        let message = Self.clean(text)
        guard !message.isEmpty, message.count <= Self.maxCharacters, !isSending else { return nil }
        let clientId = UUID().uuidString
        items.append(TimelineItem(id: TimelineItem.localPrefix + clientId, ts: Date(), channel: .ios, role: .user, text: message,
                                  clientMessageId: clientId, localStatus: .sending))
        persist()
        return await deliver(clientId: clientId, text: message) ?? ""
    }

    /// Resends a failed message with the same `clientMessageId` (idempotent on the server).
    func retry(_ itemId: String) async {
        guard let index = items.firstIndex(where: { $0.id == itemId }), items[index].localStatus == .error,
              let clientId = items[index].clientMessageId, !isSending else { return }
        items[index].localStatus = .sending
        items[index].localError = nil
        persist()
        await deliver(clientId: clientId, text: items[index].text)
    }

    @discardableResult
    private func deliver(clientId: String, text: String) async -> String? {
        isSending = true
        defer { isSending = false }
        let localId = TimelineItem.localPrefix + clientId
        do {
            let response = try await api().chat(ChatRequest(message: text, sessionId: sessionId, clientMessageId: clientId))
            onTransportSuccess?()
            guard let index = items.firstIndex(where: { $0.id == localId || $0.clientMessageId == clientId && $0.role == .user })
            else { return response.reply } // cleared (sign-out) or already replaced by the stream
            let sentAt = items[index].ts
            let userId = response.userItemId ?? localId
            let replyId = response.lucinaItemId ?? (TimelineItem.localPrefix + "reply-" + clientId)
            var user = TimelineItem(id: userId, ts: sentAt, channel: .ios, role: .user, text: text, clientMessageId: clientId,
                                    localStatus: response.userItemId == nil ? .sent : nil)
            if items[index].id == userId { user.localStatus = items[index].isLocal ? .sent : nil }
            let reply = TimelineItem(id: replyId, ts: max(Date(), sentAt.addingTimeInterval(0.001)), channel: .ios, role: .lucina,
                                     text: response.reply, clientMessageId: clientId, replyTo: userId,
                                     localStatus: response.lucinaItemId == nil ? .sent : nil)
            if userId != localId { items.removeAll { $0.id == localId } }
            // The stream may have delivered the real items first; never downgrade them.
            var incoming: [TimelineItem] = []
            if !items.contains(where: { $0.id == userId && !$0.isLocal }) { incoming.append(user) }
            if !items.contains(where: { $0.id == replyId && !$0.isLocal })
                && !items.contains(where: { $0.role == .lucina && !$0.isLocal && ($0.replyTo == userId || $0.clientMessageId == clientId) }) {
                incoming.append(reply)
            }
            let merged = TimelineMerge.merge(items.filter { !incoming.map(\.id).contains($0.id) }, incoming)
            items = merged.items
            if let job = response.job {
                jobs[replyId] = ChatJob(id: job.id, status: job.status, action: response.action)
                if !JobInfo.isTerminal(job.status) { startPolling(itemId: replyId, delay: pollInterval) }
            }
            persist()
            return response.reply
        } catch {
            let apiError = APIError.wrap(error)
            onTransportError?(apiError)
            guard let index = items.firstIndex(where: { $0.id == localId }) else { return nil }
            items[index].localStatus = .error
            items[index].localError = Self.describe(apiError)
            persist()
            return nil
        }
    }

    nonisolated static func describe(_ error: APIError) -> String {
        switch error.code {
        case "lucina_unreachable": return String(localized: "Lucina is offline. Not delivered.")
        case "lucina_timeout": return String(localized: "Lucina didn't answer in time.")
        case "lucina_error": return String(localized: "Lucina ran into a problem.")
        case "timeout": return String(localized: "No answer in time.")
        default: break
        }
        if case .hostNotFound = error { return error.userMessage }
        if error.isConnectivity { return String(localized: "Couldn't reach the server.") }
        if error.status == 401 { return String(localized: "You were signed out.") }
        if error.status == 429 { return String(localized: "Too many messages; wait a moment.") }
        return error.userMessage
    }

    // MARK: - Jobs

    func resumePolling() {
        for (itemId, job) in jobs where !job.isTerminal && pollers[itemId] == nil {
            startPolling(itemId: itemId, delay: .zero)
        }
    }

    func stopPolling() {
        pollers.values.forEach { $0.cancel() }
        pollers.removeAll()
    }

    private func startPolling(itemId: String, delay: Duration) {
        pollers[itemId]?.cancel()
        pollers[itemId] = Task { [weak self] in
            var wait = delay
            while !Task.isCancelled {
                if wait > .zero { try? await Task.sleep(for: wait) }
                guard let self, !Task.isCancelled else { return }
                let keepGoing = await self.pollOnce(itemId: itemId)
                if !keepGoing { break }
                wait = self.pollInterval
            }
            self?.pollers[itemId] = nil
        }
    }

    /// Returns whether to keep polling.
    private func pollOnce(itemId: String) async -> Bool {
        // The card may have moved to a server id since polling started.
        let key = jobs[itemId] != nil ? itemId : (jobs.first { $0.value.id == itemId }?.key ?? itemId)
        guard let jobId = jobs[key]?.id else { return false }
        do {
            let info = try await api().job(jobId)
            guard var job = jobs[key] else { return false }
            job.status = info.status
            job.pollError = nil
            if let action = info.action { job.action = action }
            if let result = info.result, !result.isNull { job.result = result.displayText }
            job.error = info.error
            if let url = info.imageUrl { job.imageUrl = url }
            jobs[key] = job
            persist()
            return !job.isTerminal
        } catch {
            let apiError = APIError.wrap(error)
            if apiError.code == "job_not_found" || apiError.status == 404 {
                jobs[key]?.status = "error"
                jobs[key]?.pollError = String(localized: "This job no longer exists.")
                persist()
                return false
            }
            if apiError.endsSession {
                onTransportError?(apiError)
                return false
            }
            jobs[key]?.pollError = String(localized: "Couldn't check on this job. Retrying…")
            return true
        }
    }

    // MARK: - Persistence

    func clear() {
        stopPolling()
        stopStream()
        items = []
        jobs = [:]
        lastEventId = nil
        nextBefore = nil
        hasOlder = false
        mode = .unknown
        try? FileManager.default.removeItem(at: fileURL)
        try? FileManager.default.removeItem(at: fileURL.deletingLastPathComponent().appendingPathComponent("chat.json"))
    }

    struct Cache: Codable {
        var items: [TimelineItem]
        var jobs: [String: ChatJob]
        var lastEventId: String?
        var localOnly: Bool
    }

    private func persist() {
        do {
            try FileManager.default.createDirectory(at: fileURL.deletingLastPathComponent(), withIntermediateDirectories: true)
            // Server mode: the newest 50 server items plus anything still unsent.
            // Local-only mode: the phone is the only copy, so keep more.
            let keep: [TimelineItem]
            if mode == .localOnly {
                keep = Array(items.suffix(500))
            } else {
                let server = items.filter { !$0.isLocal }.suffix(Self.cacheSize)
                let unsent = items.filter { $0.isLocal && $0.localStatus != .sent }
                keep = TimelineMerge.sorted(Array(server) + unsent)
            }
            let ids = Set(keep.map(\.id))
            let cache = Cache(items: keep, jobs: jobs.filter { ids.contains($0.key) }, lastEventId: lastEventId,
                              localOnly: mode == .localOnly)
            let data = try JSONEncoder().encode(cache)
            try data.write(to: fileURL, options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
        } catch {
            // Losing the local cache is not fatal; the server keeps the conversation.
        }
    }

    static func load(from url: URL) -> Cache {
        guard let data = try? Data(contentsOf: url), var cache = try? JSONDecoder().decode(Cache.self, from: data) else {
            return Cache(items: [], jobs: [:], lastEventId: nil, localOnly: false)
        }
        cache.items = cache.items.map { item in
            var item = item
            if item.localStatus == .sending {
                item.localStatus = .error
                item.localError = String(localized: "Not sent: the app closed before Lucina answered.")
            }
            return item
        }
        return cache
    }
}
