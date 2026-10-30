import Foundation
import Network
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

/// This phone’s conversation with Lucina.
///
/// Source of truth: the server timeline (`GET /v1/timeline` pages, `/v1/timeline/stream`
/// live). Messages written here are sent with `POST /v1/chat` (`X-Secretary-Client: ios`),
/// shown optimistically and reconciled with the server's copies by `clientMessageId`,
/// `userItemId` and `lucinaItemId`. SQLite supplies the first 50 items before sync.
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
    /// The text messages on their way, in order (one on the wire at a time).
    private(set) var sendQueue = ChatSendQueue()
    /// A message is on the wire (the thinking bubble).
    var isSending: Bool { sendQueue.isSending }
    @ObservationIgnored private var sendTasks: [String: Task<Void, Never>] = [:]
    /// `messageActions(for:)`, by item id.
    @ObservationIgnored private var derivedActions: [String: (item: TimelineItem, speech: String?, actions: [MessageAction])] = [:]
    /// The network coming back resets stuck sends, like the foreground does.
    @ObservationIgnored private let pathMonitor = NWPathMonitor()
    @ObservationIgnored private var online = true
    /// Callers waiting for their message's outcome, by client id.
    @ObservationIgnored private var sendWaiters: [String: [CheckedContinuation<String, Never>]] = [:]
    private(set) var isLoadingOlder = false
    private(set) var hasOlder = false
    /// The live stream is connected.
    private(set) var isLive = false
    private(set) var loadError: String?
    /// Text a rich-reply button wants in the composer ("Ricordami alle 21:30 di …", "Modifica").
    private(set) var prefill: PrefillRequest?
    /// Buttons already used, by Lucina item id (persisted).
    private(set) var choices: [String: String] = [:]
    /// Rich replies that just arrived: their bubbles appear one by one, once.
    @ObservationIgnored private var revealIds: Set<String> = []
    /// Attachments the composer is uploading: 0...1 by share id.
    private(set) var shareProgress: [String: Double] = [:]
    /// Where Lucina is with a share (`GET /v1/share/{id}`), while it is being read.
    private(set) var shareStatus: [String: ShareStatus] = [:]
    /// Shares sent from this phone's composer, by share id (Shares/Chat/<id>/outgoing.json).
    @ObservationIgnored private(set) var outgoing: [String: OutgoingShare] = [:]
    @ObservationIgnored private var sharePollers: [String: Task<Void, Never>] = [:]
    @ObservationIgnored private(set) var isForeground = false
    /// The mic button's recorder: voice notes go to Caches/ChatNotes, without a Live Activity. Separate from
    /// the memos' recorder; only one of the two may hold the microphone.
    let noteRecorder = MemoRecorder(directory: VoiceNoteFiles.directory, liveActivity: false)
    /// Lucina's spoken answers to voice notes (`meta.voiceReply`).
    let voiceReplies: LucinaVoiceReplies
    /// "Read aloud" on a message (swipe right, the menu).
    let readAloud: ChatReadAloud
    /// The message the composer replies to (swipe left, the menu's Reply, VoiceOver): the quote bar above
    /// the field. Its server id rides with the next send as `replyTo`.
    private(set) var replyingTo: TimelineItem?
    /// Messages hidden on this phone (the menu's Hide; Settings › Unhide all), by item id.
    private(set) var hiddenIds: Set<String> = []
    nonisolated static let hiddenKey = "secretary.chat.hidden.v1"
    /// Every `POST /v1/chat` body just before it is sent (the self-test logs `replyTo`).
    @ObservationIgnored var onChatRequest: ((ChatRequest) -> Void)?

    struct PrefillRequest: Equatable {
        let text: String
        let id = UUID()
    }

    nonisolated static let choicesKey = "secretary.chat.choices.v1"

    @ObservationIgnored private let api: () -> SecretaryAPI
    let database: ChatDatabase
    nonisolated static let deviceIdKey = "secretary.chat.deviceId"
    private(set) var deviceId: String?
    private(set) var messageSource = "cache"
    @ObservationIgnored private var deviceHydrationTask: Task<Void, Never>?
    @ObservationIgnored private var hydrationTask: Task<Void, Never>?
    @ObservationIgnored private var persistenceTask: Task<Void, Never>?
    @ObservationIgnored private var syncing = false
    @ObservationIgnored private var generation = 0
    @ObservationIgnored private var persistedItems: [String: TimelineItem] = [:]
    @ObservationIgnored private var sentClientIDs: Set<String> = []
    @ObservationIgnored private let fileURL: URL
    @ObservationIgnored private let defaults: UserDefaults
    @ObservationIgnored private var pollers: [String: Task<Void, Never>] = [:]
    @ObservationIgnored private let pollInterval: Duration
    @ObservationIgnored private var nextBefore: String?
    @ObservationIgnored private var lastEventId: String?
    @ObservationIgnored private var streamTask: Task<Void, Never>?
    @ObservationIgnored var onTransportError: ((APIError) -> Void)?
    @ObservationIgnored var onTransportSuccess: (() -> Void)?
    /// Every batch of timeline items that arrives (the device bridge looks for commands).
    @ObservationIgnored var onItems: (([TimelineItem]) -> Void)?

    init(api: @escaping () -> SecretaryAPI, fileURL: URL = ChatStore.defaultFileURL, defaults: UserDefaults = .standard,
         pollInterval: Duration = ChatStore.pollInterval, database: ChatDatabase? = nil) {
        self.database = database ?? ChatDatabase(url: fileURL == Self.defaultFileURL ? ChatDatabase.defaultURL : fileURL.deletingPathExtension().appendingPathExtension("sqlite"))
        self.deviceId = defaults.string(forKey: Self.deviceIdKey)
        self.api = api
        self.fileURL = fileURL
        self.defaults = defaults
        self.pollInterval = pollInterval
        voiceReplies = LucinaVoiceReplies(defaults: defaults)
        readAloud = ChatReadAloud()
        choices = (defaults.dictionary(forKey: Self.choicesKey) as? [String: String]) ?? [:]
        hiddenIds = Set(defaults.stringArray(forKey: Self.hiddenKey) ?? [])
        outgoing = ChatShareOutbox.loadAll()
        // One sound at a time across the chat's players.
        ChatAudio.shared.register(voiceReplies) { [weak voiceReplies] in voiceReplies?.stop() }
        ChatAudio.shared.register(readAloud) { [weak readAloud] in readAloud?.stop() }
        // Read aloud refuses while the memo recorder or this chat's voice-note recorder has the microphone.
        readAloud.refusal = { [noteRecorder] in
            if let notice = ChatAudio.recordingNotice { return notice }
            return noteRecorder.isActive ? String(localized: "Finish the recording first.") : nil
        }
        hydrationTask = Task { [weak self] in
            guard let self else { return }
            await hydrate()
        }
        pathMonitor.pathUpdateHandler = { [weak self] path in
            Task { @MainActor in
                guard let self else { return }
                let now = path.status == .satisfied
                let cameBack = now && !self.online
                self.online = now
                if cameBack { self.resumeSends() }
            }
        }
        pathMonitor.start(queue: DispatchQueue(label: "chat-send-path"))
    }

    // MARK: - Rich replies

    func requestPrefill(_ text: String) {
        prefill = PrefillRequest(text: text)
    }

    func chosenAction(for itemId: String) -> String? { choices[itemId] }

    func choose(_ actionId: String, for itemId: String) {
        choices[itemId] = actionId
        if choices.count > 200 { choices = Dictionary(uniqueKeysWithValues: choices.suffix(200).map { ($0.key, $0.value) }) }
        defaults.set(choices, forKey: Self.choicesKey)
    }

    /// True once for a rich reply that arrived while the chat was open.
    func consumeReveal(_ itemId: String) -> Bool {
        revealIds.remove(itemId) != nil
    }

    /// A Lucina message's button the owner answered by typing its text in the next message.
    func inferredChoice(after item: TimelineItem) -> String? {
        guard let rich = item.rich, !rich.actions.isEmpty, let index = items.firstIndex(where: { $0.id == item.id }),
              let next = items[(index + 1)...].first(where: { $0.role == .user }) else { return nil }
        let said = next.text.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        return rich.actions.first { ($0.kind == .reply || $0.kind == .confirm) && $0.text?.lowercased() == said }?.id
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

    /// Hydration is shared by launch and sync, so no timeline request can beat the local page.
    func prepare() async {
        await hydrationTask?.value
        await deviceHydrationTask?.value
    }

    func flushPersistence() async { await persistenceTask?.value }

    private var mineScope: ChatDatabase.Scope { .mine(deviceId) }

    private func hydrate() async {
        let epoch = generation
        do {
            // Migration runs off-main, including decoding the legacy cache once.
            let url = fileURL
            let cache = await Task.detached { Self.load(from: url) }.value
            guard epoch == generation, !Task.isCancelled else { return }
            if FileManager.default.fileExists(atPath: url.path) {
                let metadata = Cache(items: [], jobs: cache.jobs, lastEventId: cache.lastEventId, localOnly: cache.localOnly)
                let text = String(decoding: try JSONEncoder().encode(metadata), as: UTF8.self)
                try await database.write(cache.items, state: ["chatMetadata": text])
                try? FileManager.default.removeItem(at: url)
            }
            let page = try await database.read(mineScope)
            let metadata = try await database.value("chatMetadata")
            let olderCursor = try await database.value("older:mine")
            guard epoch == generation, !Task.isCancelled else { return }
            if let metadata, let cache = try? JSONDecoder().decode(Cache.self, from: Data(metadata.utf8)) {
                jobs = cache.jobs
                lastEventId = cache.lastEventId
                if cache.localOnly { mode = .localOnly }
            }
            persistedItems = Dictionary(uniqueKeysWithValues: page.map { ($0.id, $0) })
            let restored = page.map { original in
                var item = original
                if item.localStatus == .sending, item.meta?.isShare != true {
                    item.localStatus = .error
                    item.localError = String(localized: "Not sent: the app closed before Lucina answered.")
                }
                return item
            }
            items = TimelineMerge.merge(restored.filter { isMine($0, deviceId: deviceId, sentClientIDs: sentClientIDs) }, items).items
            sentClientIDs.formUnion(items.compactMap(\.clientMessageId))
            hasOlder = page.count == Self.pageSize || olderCursor?.isEmpty == false
            nextBefore = (olderCursor?.isEmpty == false ? olderCursor : nil) ?? page.first(where: { !$0.isLocal })?.id
            reconcileOutbox()
        } catch {
            // Offline storage failure must not prevent a server sync.
        }
    }

    func setDeviceId(_ id: String?) {
        let changed = deviceId != id
        deviceId = id
        defaults.set(id, forKey: Self.deviceIdKey)
        items = items.filter { isMine($0, deviceId: id, sentClientIDs: sentClientIDs) }
        guard changed, id != nil else { return }
        let epoch = generation
        deviceHydrationTask = Task {
            await hydrationTask?.value
            guard epoch == generation else { return }
            guard let cached = try? await database.read(.mine(id)), epoch == generation, deviceId == id else { return }
            for item in cached { persistedItems[item.id] = item }
            items = TimelineMerge.merge(cached, items).items
        }
    }

    /// Incremental sync; all received items reach the bridge and disk, only mine reach Chat.
    func refresh() async {
        await prepare()
        guard !syncing else { return }
        syncing = true
        let epoch = generation
        defer { syncing = false }
        do {
            await flushPersistence()
            let scope = mineScope
            let cursor = try? await database.value(scope.stateKey)
            try await ChatSync.run(api: api(), cursor: cursor) { page, reset in
                guard epoch == self.generation else { throw CancellationError() }
                await self.flushPersistence()
                guard epoch == self.generation else { throw CancellationError() }
                var state = page.cursor.map { [scope.stateKey: $0] } ?? [:]
                if cursor == nil || reset { state["older:mine"] = page.nextBefore ?? "" }
                try? await self.database.write(page.items, state: state, reset: reset ? scope : nil)
                guard epoch == self.generation else { throw CancellationError() }
                self.onTransportSuccess?()
                self.loadError = nil
                self.mode = .server
                self.apply(page.items)
                if page.items.isEmpty { self.persist() }
                PerfLog.chatSynced()
                if cursor == nil || reset {
                    self.nextBefore = page.nextBefore
                    self.hasOlder = page.nextBefore != nil
                }
            }
        } catch is CancellationError {
            return
        } catch {
            guard epoch == generation else { return }
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
        await prepare()
        guard hasOlder, !isLoadingOlder else { return }
        isLoadingOlder = true
        let epoch = generation
        defer { isLoadingOlder = false }
        do {
            await flushPersistence()
            let before = items.filter { !$0.isLocal }.map(\.id).min() ?? nextBefore
            let cached = (try? await database.read(mineScope, before: before)) ?? []
            guard epoch == generation else { return }
            if !cached.isEmpty {
                apply(cached)
                nextBefore = cached.first?.id
                return
            }
            guard mode == .server, let cursor = nextBefore ?? before else { return }
            let page = try await api().timeline(before: cursor, limit: Self.pageSize, scope: "mine")
            guard epoch == generation else { return }
            try await database.write(page.items, state: ["older:mine": page.nextBefore ?? ""])
            guard epoch == generation else { return }
            apply(page.items)
            nextBefore = page.nextBefore
            hasOlder = page.nextBefore != nil
        } catch { onTransportError?(APIError.wrap(error)) }
    }

    /// Merges server items in (dedupe by id, reconcile optimistic bubbles).
    func apply(_ incoming: [TimelineItem], persistAfter: Bool = true) {
        guard !incoming.isEmpty else { return }
        onItems?(incoming)
        let visible = incoming.filter { isMine($0, deviceId: deviceId, sentClientIDs: sentClientIDs) }
        if !visible.isEmpty, items.isEmpty { messageSource = "server" }
        let result = TimelineMerge.merge(items, visible)
        items = result.items
        for (old, new) in result.replaced {
            if let job = jobs.removeValue(forKey: old), jobs[new] == nil { jobs[new] = job }
            // The server has this message: a failed or waiting copy is not sent again.
            guard old.hasPrefix(TimelineItem.localPrefix) else { continue }
            let clientId = String(old.dropFirst(TimelineItem.localPrefix.count))
            if let state = sendQueue.state(of: clientId), state == .queued || state == .failed {
                sendQueue.remove(clientId)
                resumeWaiters(clientId, "")
            }
        }
        if persistAfter { persist(additional: incoming, removing: Array(result.replaced.keys)) }
        voiceReplies.consider(visible, items: items, foreground: isForeground, api: api)
    }

    // MARK: - Live stream

    /// Starts or stops the SSE connection (foreground only; the view calls this on scene changes).
    func setForeground(_ active: Bool) {
        isForeground = active
        if active {
            Task {
                await refresh()
                if mode == .server { startStream() }
            }
            resumeSends()
            resumePolling()
            reconcileOutbox()
            resumeSharePolling()
        } else {
            stopStream()
            stopSharePolling()
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
        if item.role == .lucina, item.rich != nil, !items.contains(where: { $0.id == item.id }) { revealIds.insert(item.id) }
        apply([item])
    }

    // MARK: - Sending

    /// Adds an optimistic bubble and sends it. Returns false when nothing was sent.
    /// `replyTo`: the server id of the message this one answers (the quote bar).
    @discardableResult
    func send(_ text: String, replyTo: String? = nil) async -> Bool {
        await post(text, replyTo: replyTo) != nil
    }

    /// Sends like `send` and returns Lucina's reply text (nil when nothing was sent or it
    /// failed). Used by the voice stage to speak the answer.
    func ask(_ text: String, replyTo: String? = nil) async -> String? {
        guard let outcome = await post(text, replyTo: replyTo) else { return nil }
        return outcome
    }

    /// nil: not sent. Otherwise the reply text ("" when the send failed; the bubble shows why).
    /// Never refused because another message is on its way: the bubble appears at once and waits its turn.
    private func post(_ text: String, replyTo: String?) async -> String? {
        let message = Self.clean(text)
        guard !message.isEmpty, message.count <= Self.maxCharacters else { return nil }
        LucinaDonations.donate()
        let clientId = UUID().uuidString
        sentClientIDs.insert(clientId)
        items.append(TimelineItem(id: TimelineItem.localPrefix + clientId, ts: Date(), channel: .ios, role: .user, text: message,
                                  clientMessageId: clientId, replyTo: replyTo, localStatus: .sending))
        persist()
        return await enqueue(clientId: clientId, text: message, replyTo: replyTo)
    }

    /// Feedback on a message Lucina sent on her own; `true` when the server took it.
    func outreachFeedback(_ outreachId: String, _ action: String) async -> Bool {
        do {
            try await api().outreachFeedback(id: outreachId, action: action)
            return true
        } catch {
            return false
        }
    }

    /// Resends a failed message with the same `clientMessageId` (idempotent on the server), behind any
    /// message already waiting.
    func retry(_ itemId: String) async {
        if let shareId = items.first(where: { $0.id == itemId })?.meta?.shareId, outgoing[shareId] != nil {
            retryShare(shareId)
            return
        }
        guard let index = items.firstIndex(where: { $0.id == itemId }), items[index].localStatus == .error,
              let clientId = items[index].clientMessageId else { return }
        items[index].localStatus = .sending
        items[index].localError = nil
        let text = items[index].text
        let replyTo = items[index].replyTo
        persist()
        await enqueue(clientId: clientId, text: text, replyTo: replyTo)
    }

    /// The message waits behind another one (its bubble says so).
    func isQueued(_ item: TimelineItem) -> Bool {
        guard item.localStatus == .sending, let clientId = item.clientMessageId else { return false }
        return sendQueue.state(of: clientId) == .queued
    }

    /// Queues the message, moves the line and waits until this message is answered or failed: the reply's
    /// spoken text ("" when it failed).
    @discardableResult
    private func enqueue(clientId: String, text: String, replyTo: String?) async -> String {
        guard sendQueue.enqueue(clientId: clientId, text: text, replyTo: replyTo) else { return "" }
        return await withCheckedContinuation { continuation in
            sendWaiters[clientId, default: []].append(continuation)
            pumpSends()
        }
    }

    /// Starts the next waiting message unless one is on the wire. Each send gets `ChatSendQueue.timeout`
    /// (the request's own timeout, then cancellation), so the line always moves.
    private func pumpSends() {
        failExpiredSends()
        guard let entry = sendQueue.start(now: Date()) else { return }
        let request = ChatRequest(message: entry.text, sessionId: sessionId, clientMessageId: entry.clientId, replyTo: entry.replyTo)
        onChatRequest?(request)
        let client = api()
        let timeout = sendQueue.timeout
        sendTasks[entry.clientId]?.cancel()
        sendTasks[entry.clientId] = Task { [weak self] in
            let result: Result<ChatResponse, Error>
            do {
                result = .success(try await ChatSendQueue.within(timeout) { try await client.chat(request) })
            } catch {
                result = .failure(error)
            }
            self?.finishSend(entry, result)
        }
    }

    private func finishSend(_ entry: ChatSendQueue.Entry, _ result: Result<ChatResponse, Error>) {
        let clientId = entry.clientId
        // The attempt this result belongs to; a newer one (a Retry) may already be on the wire.
        let current = sendQueue.state(of: clientId) == entry.state
        if current { sendTasks[clientId] = nil }
        switch result {
        case let .success(response):
            // The server has it, even if the bubble was already failed: show the answer, never send it again.
            if !sendQueue.succeed(clientId) { sendQueue.remove(clientId) }
            onTransportSuccess?()
            let reply = applyResponse(response, clientId: clientId, text: entry.text, replyTo: entry.replyTo)
            resumeWaiters(clientId, reply)
        case let .failure(error):
            guard current, sendQueue.fail(clientId) else { break } // already failed by the timeout sweep
            let apiError = APIError.wrap(error)
            onTransportError?(apiError)
            markFailed(clientId, Self.describe(apiError))
            resumeWaiters(clientId, "")
        }
        pumpSends()
    }

    /// Fails every send past its timeout, so the next message goes.
    private func failExpiredSends(now: Date = Date()) {
        for clientId in sendQueue.expire(now: now) {
            sendTasks.removeValue(forKey: clientId)?.cancel()
            markFailed(clientId, Self.describe(.timeout))
            resumeWaiters(clientId, "")
        }
    }

    /// Foreground and network back: nothing stays "Sending…" for longer than the timeout, and the line moves.
    func resumeSends(now: Date = Date()) {
        failExpiredSends(now: now)
        // A bubble that says "Sending…" with no send behind it (it cannot be retried otherwise).
        var changed = false
        for index in items.indices where items[index].localStatus == .sending && items[index].meta?.isShare != true {
            guard let clientId = items[index].clientMessageId, sendQueue.state(of: clientId) == nil,
                  now.timeIntervalSince(items[index].ts) >= sendQueue.timeout else { continue }
            items[index].localStatus = .error
            items[index].localError = Self.describe(.timeout)
            changed = true
        }
        if changed { persist() }
        pumpSends()
    }

    private func markFailed(_ clientId: String, _ reason: String) {
        guard let index = items.firstIndex(where: { $0.id == TimelineItem.localPrefix + clientId }),
              items[index].localStatus == .sending else { return }
        items[index].localStatus = .error
        items[index].localError = reason
        persist()
    }

    private func resumeWaiters(_ clientId: String, _ reply: String) {
        for waiter in sendWaiters.removeValue(forKey: clientId) ?? [] { waiter.resume(returning: reply) }
    }

    /// Puts the server's answer in place of the optimistic bubble; returns what her voice reads.
    private func applyResponse(_ response: ChatResponse, clientId: String, text: String, replyTo: String?) -> String {
        let localId = TimelineItem.localPrefix + clientId
        guard let index = items.firstIndex(where: { $0.id == localId || $0.clientMessageId == clientId && $0.role == .user })
        else { return response.reply } // cleared (sign-out) or already replaced by the stream
        let sentAt = items[index].ts
        let userId = response.userItemId ?? localId
        let replyId = response.lucinaItemId ?? (TimelineItem.localPrefix + "reply-" + clientId)
        var user = TimelineItem(id: userId, ts: sentAt, channel: .ios, role: .user, text: text, clientMessageId: clientId,
                                replyTo: replyTo, localStatus: response.userItemId == nil ? .sent : nil)
        if items[index].id == userId { user.localStatus = items[index].isLocal ? .sent : nil }
        let reply = TimelineItem(id: replyId, ts: max(Date(), sentAt.addingTimeInterval(0.001)), channel: .ios, role: .lucina,
                                 text: response.reply, clientMessageId: clientId, replyTo: userId,
                                 localStatus: response.lucinaItemId == nil ? .sent : nil, rich: response.rich)
        if userId != localId { items.removeAll { $0.id == localId } }
        // The stream may have delivered the real items first; never downgrade them.
        var incoming: [TimelineItem] = []
        if !items.contains(where: { $0.id == userId && !$0.isLocal }) { incoming.append(user) }
        if !items.contains(where: { $0.id == replyId && !$0.isLocal })
            && !items.contains(where: { $0.role == .lucina && !$0.isLocal && ($0.replyTo == userId || $0.clientMessageId == clientId) }) {
            incoming.append(reply)
            if response.rich != nil { revealIds.insert(replyId) }
        }
        let merged = TimelineMerge.merge(items.filter { !incoming.map(\.id).contains($0.id) }, incoming)
        items = merged.items
        if let job = response.job {
            jobs[replyId] = ChatJob(id: job.id, status: job.status, action: response.action)
            if !JobInfo.isTerminal(job.status) { startPolling(itemId: replyId, delay: pollInterval) }
        }
        persist()
        // Her voice reads only the bubbles (never the widgets), without markdown.
        return response.rich?.speechText ?? RichMessage.stripMarkdown(response.reply)
    }

    /// Stops every send (sign-out); whoever waits gets an empty reply.
    private func cancelSends() {
        sendTasks.values.forEach { $0.cancel() }
        sendTasks.removeAll()
        sendQueue.removeAll()
        let waiters = sendWaiters
        sendWaiters.removeAll()
        for waiter in waiters.values.flatMap({ $0 }) { waiter.resume(returning: "") }
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
        generation += 1
        hydrationTask?.cancel()
        let priorHydration = hydrationTask
        let priorPersistence = persistenceTask
        persistenceTask = Task {
            await priorHydration?.value
            await priorPersistence?.value
            try? await database.clear()
        }
        persistedItems = [:]
        derivedActions = [:]
        sentClientIDs = []
        setDeviceId(nil)
        messageSource = "cache"
        if noteRecorder.isActive { noteRecorder.discard() }
        voiceReplies.stop()
        readAloud.stop()
        replyingTo = nil
        hiddenIds = []
        defaults.removeObject(forKey: Self.hiddenKey)
        cancelSends()
        stopPolling()
        stopStream()
        stopSharePolling()
        outgoing = [:]
        shareProgress = [:]
        shareStatus = [:]
        ChatShareOutbox.removeAll()
        ChatPreviews.removeAll()
        items = []
        jobs = [:]
        lastEventId = nil
        nextBefore = nil
        hasOlder = false
        mode = .unknown
        try? FileManager.default.removeItem(at: fileURL)
        try? FileManager.default.removeItem(at: fileURL.deletingLastPathComponent().appendingPathComponent("chat.json"))
    }

    struct Cache: Codable, Sendable {
        var items: [TimelineItem]
        var jobs: [String: ChatJob]
        var lastEventId: String?
        var localOnly: Bool
    }

    private func persist(additional: [TimelineItem] = [], removing: [String] = []) {
        let snapshot = TimelineMerge.merge(items, additional).items
        let ids = Set(snapshot.map(\.id))
        let removed = Set(removing).union(persistedItems.keys.filter { $0.hasPrefix(TimelineItem.localPrefix) && !ids.contains($0) })
        let changed = snapshot.filter { persistedItems[$0.id] != $0 }
        for id in removed { persistedItems[id] = nil }
        for item in changed { persistedItems[item.id] = item }
        let metadata = Cache(items: [], jobs: jobs, lastEventId: lastEventId, localOnly: mode == .localOnly)
        let previous = persistenceTask
        persistenceTask = Task {
            await previous?.value
            let text = (try? JSONEncoder().encode(metadata)).map { String(decoding: $0, as: UTF8.self) }
            try? await database.write(changed, state: text.map { ["chatMetadata": $0] } ?? [:], removeIDs: Array(removed))
        }
    }

    nonisolated static func load(from url: URL) -> Cache {
        guard let data = try? Data(contentsOf: url), var cache = try? JSONDecoder().decode(Cache.self, from: data) else {
            return Cache(items: [], jobs: [:], lastEventId: nil, localOnly: false)
        }
        cache.items = cache.items.map { item in
            var item = item
            // Attachments keep uploading in the background session; reconcileOutbox() settles them.
            if item.localStatus == .sending, item.meta?.isShare != true {
                item.localStatus = .error
                item.localError = String(localized: "Not sent: the app closed before Lucina answered.")
            }
            return item
        }
        return cache
    }
}

// MARK: - Attachments, Telegram style

/// Files from the composer go through `POST /v1/share` (source `ios-chat`) on the background upload
/// session, so they keep uploading while the app is suspended. The bubble shows at once with the local
/// previews and a progress ring; a failure offers Retry with the same share id (the server dedupes); the
/// 202 turns the bubble into the server's user item; `GET /v1/share/{id}` says when Lucina is reading.
extension ChatStore: ShareUploadObserver {
    nonisolated static let sharePollInterval: Duration = .seconds(2)
    nonisolated static let sharePollLimit: TimeInterval = 10 * 60

    /// Sends the tray (files, links, the location) with the caption. Returns false when nothing was sent.
    @discardableResult
    func sendShare(caption: String, attachments: [PendingAttachment], urls: [String] = [], location: ShareMeta.Location? = nil,
                   replyTo: String? = nil) -> Bool {
        guard let share = stageShare(caption: caption, attachments: attachments, urls: urls, location: location, replyTo: replyTo)
        else { return false }
        upload(share, retry: false)
        return true
    }

    /// Sends a voice note from the mic button (and the self-test) through the same share pipeline as a
    /// composer attachment: one `audio/aac` part `voice-<timestamp>.aac`, no caption, and `meta.files[0]`
    /// with `voice`, `durationMs`, `waveform` and, for notes up to a minute, the on-device `transcript`
    /// (`transcribe: false` leaves it out so the server's Whisper runs). The bubble shows at once; the
    /// upload starts once the transcript is in. Returns false when nothing was sent.
    @discardableResult
    func sendVoiceNote(_ note: VoiceNote, transcribe: Bool = true, replyTo: String? = nil) async -> Bool {
        let size = Int64((try? note.url.resourceValues(forKeys: [.fileSizeKey]))?.fileSize ?? 0)
        guard size > 0 else { return false }
        let url = note.url
        let levels = note.levels.isEmpty ? await Task.detached(priority: .userInitiated) { VoiceWaveform.levels(from: url) }.value : note.levels
        let attachment = PendingAttachment(url: note.url, name: VoiceNoteFiles.fileName(for: note.recordedAt), mime: "audio/aac", kind: .audio,
                                           size: size, durationMs: Int((note.duration * 1000).rounded()), voice: true,
                                           waveform: VoiceWaveform.base64(levels))
        guard var share = stageShare(caption: "", attachments: [attachment], replyTo: replyTo) else { return false }
        // The note now lives in the share's folder (until the server has it); nothing stays in ChatNotes.
        try? FileManager.default.removeItem(at: note.url)
        if transcribe, note.duration <= VoiceNote.transcribeLimit,
           let folder = try? ChatShareOutbox.directory(share.shareId), let file = share.localFile(at: 0, in: folder) {
            let transcript = await AudioTranscriber.transcribe(file, limit: VoiceNote.transcribeTimeout)
            // Signed out (or cleared) meanwhile: nothing left to send.
            guard let current = outgoing[share.shareId] else { return false }
            share = current
            if let transcript, !transcript.isEmpty {
                share.files[0].transcript = transcript
                outgoing[share.shareId] = share
                try? ChatShareOutbox.save(share)
                if let index = items.firstIndex(where: { $0.id == TimelineItem.localPrefix + share.shareId }),
                   let voice = items[index].attachments.firstIndex(where: \.isVoice) {
                    items[index].attachments[voice].transcript = transcript
                }
                persist()
            }
        }
        upload(share, retry: false)
        return true
    }

    /// The words of a voice note: its own `transcript` (on-device, known at once), else what Lucina heard
    /// (`meta.transcript` on her reply), else the share status's attachment while it is being read.
    func voiceTranscript(for item: TimelineItem, attachment: TimelineAttachment, fileIndex: Int?) -> String? {
        if let text = attachment.transcript?.trimmingCharacters(in: .whitespacesAndNewlines), !text.isEmpty { return text }
        let shareId = item.meta?.shareId
        let userItemId = item.isLocal ? shareId.flatMap { outgoing[$0]?.userItemId } : item.id
        if let userItemId, let reply = items.first(where: { $0.role == .lucina && $0.replyTo == userItemId }),
           let text = reply.meta?.transcript?.trimmingCharacters(in: .whitespacesAndNewlines), !text.isEmpty {
            return text
        }
        guard let shareId, let status = shareStatus[shareId] else { return nil }
        let match = status.attachments.first { $0.index == fileIndex }
            ?? (status.attachments.count == 1 ? status.attachments.first : nil)
        guard let text = match?.transcript?.trimmingCharacters(in: .whitespacesAndNewlines), !text.isEmpty else { return nil }
        return text
    }

    /// Writes the share's folder and record and shows the optimistic bubble; the caller starts the upload.
    private func stageShare(caption: String, attachments: [PendingAttachment], urls: [String] = [],
                            location: ShareMeta.Location? = nil, replyTo: String? = nil) -> OutgoingShare? {
        let text = Self.clean(caption)
        guard !attachments.isEmpty || !urls.isEmpty || location != nil, text.count <= Self.maxCharacters else { return nil }
        let shareId = UUID().uuidString
        let files: [OutgoingShare.File]
        do {
            let folder = try ChatShareOutbox.directory(shareId)
            files = try attachments.enumerated().map { try ChatShareOutbox.adopt($0.element, index: $0.offset, into: folder) }
        } catch {
            ChatShareOutbox.remove(shareId)
            return nil
        }
        for (index, attachment) in attachments.enumerated() where attachment.kind.isVisual {
            if let thumbnail = attachment.thumbnail { ChatPreviews.store(thumbnail, shareId: shareId, index: index) }
        }
        let share = OutgoingShare(shareId: shareId, caption: text, files: files, urls: urls, location: location, createdAt: Date(),
                                  replyTo: replyTo)
        try? ChatShareOutbox.save(share)
        outgoing[shareId] = share
        LucinaDonations.donate()
        items.append(share.localItem())
        persist()
        return share
    }

    /// "Not sent — Retry": the same share id, a fresh body from the kept files.
    func retryShare(_ shareId: String) {
        guard let share = outgoing[shareId], share.acceptedAt == nil,
              let index = items.firstIndex(where: { $0.id == TimelineItem.localPrefix + shareId }) else { return }
        items[index].localStatus = .sending
        items[index].localError = nil
        persist()
        upload(share, retry: true)
    }

    private func upload(_ share: OutgoingShare, retry: Bool) {
        let client = api().client
        guard let token = client.accessKey, !token.isEmpty else {
            markShareFailed(share.shareId, String(localized: "You were signed out."))
            return
        }
        let base = client.baseURL
        let endpoints = ([base] + client.endpoints.filter { $0 != base }).map(\.absoluteString)
        let credentials = BridgeCredentials(token: token, endpoints: endpoints, sources: ["share"])
        let sessionId = sessionId
        shareProgress[share.shareId] = 0
        Task {
            do {
                let record = try await Task.detached(priority: .userInitiated) {
                    let folder = try ChatShareOutbox.directory(share.shareId)
                    return try ShareRequestBuilder.build(payload: share.payload(in: folder), intent: "ask", note: share.caption,
                                                         source: "ios-chat", credentials: credentials,
                                                         directory: ShareRequestBuilder.directory(), shareId: share.shareId,
                                                         sessionId: sessionId, meta: share.meta, origin: "chat", replyTo: share.replyTo)
                }.value
                if let directory = try? ShareRequestBuilder.directory() {
                    try? FileManager.default.removeItem(at: ShareResultFile.url(share.shareId, in: directory))
                }
                try ShareUploads.shared.enqueue(record, credentials: credentials,
                                                attempt: retry ? String(UUID().uuidString.prefix(8)) : nil)
            } catch {
                shareProgress[share.shareId] = nil
                markShareFailed(share.shareId, (error as? LocalizedError)?.errorDescription ?? String(localized: "Not sent"))
            }
        }
    }

    private func markShareFailed(_ shareId: String, _ message: String) {
        items = ShareReconcile.fail(items, shareId: shareId, message: message)
        persist()
    }

    nonisolated static func shareFailure(_ status: Int?) -> String {
        guard let status, status > 0 else { return String(localized: "Not sent") }
        if status == 401 { return String(localized: "You were signed out.") }
        return ShareFailure.server(status).localizedDescription
    }

    // MARK: Upload callbacks

    func shareUploadProgressed(_ shareId: String, fraction: Double) {
        guard let share = outgoing[shareId], share.acceptedAt == nil else { return }
        shareProgress[shareId] = min(1, max(0, fraction))
    }

    func shareUploadFinished(_ result: ShareUploadResult) {
        guard outgoing[result.shareId] != nil else { return }
        applyShareResult(shareId: result.shareId, success: result.decision == .success, status: result.status, body: result.body)
    }

    private func applyShareResult(shareId: String, success: Bool, status: Int?, body: Data?) {
        guard var share = outgoing[shareId] else { return }
        shareProgress[shareId] = nil
        if let directory = try? ShareRequestBuilder.directory() {
            try? FileManager.default.removeItem(at: ShareResultFile.url(shareId, in: directory))
        }
        guard success else {
            if share.acceptedAt == nil { markShareFailed(shareId, Self.shareFailure(status)) }
            return
        }
        let accepted = body.flatMap { try? JSONDecoder().decode(ShareAccepted.self, from: $0) }
        if share.acceptedAt == nil {
            share.acceptedAt = Date()
            share.userItemId = accepted?.userItemId
            outgoing[shareId] = share
            try? ChatShareOutbox.save(share)
            ChatShareOutbox.dropFiles(share)
        }
        items = ShareReconcile.accept(items, shareId: shareId, accepted: accepted)
        persist()
        startSharePolling(shareId)
    }

    /// Settles bubbles whose upload finished while nobody listened (suspended or relaunched in the background).
    func reconcileOutbox() {
        guard !outgoing.isEmpty, let directory = try? ShareRequestBuilder.directory() else { return }
        var changed = false
        for (shareId, share) in outgoing where share.acceptedAt == nil {
            if let data = try? Data(contentsOf: ShareResultFile.url(shareId, in: directory)),
               let result = try? JSONDecoder().decode(ShareResultFile.self, from: data) {
                applyShareResult(shareId: shareId, success: result.success, status: result.status, body: result.body)
                continue
            }
            guard let index = items.firstIndex(where: { $0.id == TimelineItem.localPrefix + shareId }) else {
                if let server = items.first(where: { !$0.isLocal && $0.role == .user && $0.meta?.shareId == shareId }) {
                    // The timeline delivered the server's copy first: the server has it.
                    var copy = share
                    copy.acceptedAt = Date()
                    copy.userItemId = server.id
                    outgoing[shareId] = copy
                    try? ChatShareOutbox.save(copy)
                    ChatShareOutbox.dropFiles(copy)
                } else {
                    outgoing[shareId] = nil
                    ChatShareOutbox.remove(shareId)
                }
                continue
            }
            let inFlight = FileManager.default.fileExists(atPath: directory.appendingPathComponent(shareId + ".json").path)
            if !inFlight, items[index].localStatus == .sending, shareProgress[shareId] == nil {
                items[index].localStatus = .error
                items[index].localError = String(localized: "Not sent")
                changed = true
            }
        }
        if changed { persist() }
    }

    // MARK: "Lucina is reading…"

    /// The share's status while Lucina is still reading it and has not answered yet.
    func readingStatus(for item: TimelineItem) -> ShareStatus? {
        guard item.role == .user, let shareId = item.meta?.shareId, let status = shareStatus[shareId], status.isWorking else { return nil }
        let userItemId = item.isLocal ? outgoing[shareId]?.userItemId : item.id
        return ShareReconcile.replied(items, userItemId: userItemId, status: status) ? nil : status
    }

    /// The composer's own copy of a file it sent, while it is still kept (before the server took it).
    func localFile(shareId: String?, fileIndex: Int) -> URL? {
        guard let shareId, let share = outgoing[shareId],
              let folder = try? ChatShareOutbox.root().appendingPathComponent(shareId, isDirectory: true) else { return nil }
        return share.localFile(at: fileIndex, in: folder)
    }

    private func startSharePolling(_ shareId: String) {
        guard isForeground, sharePollers[shareId] == nil, let acceptedAt = outgoing[shareId]?.acceptedAt,
              shareStatus[shareId]?.isFinished != true else { return }
        let deadline = acceptedAt.addingTimeInterval(Self.sharePollLimit)
        guard Date() < deadline else { return }
        sharePollers[shareId] = Task { [weak self] in
            while !Task.isCancelled, Date() < deadline {
                guard let self else { return }
                if !(await self.pollShare(shareId)) { break }
                try? await Task.sleep(for: Self.sharePollInterval)
            }
            self?.sharePollers[shareId] = nil
        }
    }

    private func resumeSharePolling() {
        for (shareId, share) in outgoing where share.acceptedAt != nil { startSharePolling(shareId) }
    }

    private func stopSharePolling() {
        sharePollers.values.forEach { $0.cancel() }
        sharePollers.removeAll()
    }

    /// Returns whether to keep polling.
    private func pollShare(_ shareId: String) async -> Bool {
        do {
            let status: ShareStatus = try await api().client.get("/v1/share/" + MediaAPI.encode(shareId))
            guard !Task.isCancelled else { return false }
            shareStatus[shareId] = status
            let userItemId = outgoing[shareId]?.userItemId ?? items.first { !$0.isLocal && $0.meta?.shareId == shareId }?.id
            if status.isFinished || ShareReconcile.replied(items, userItemId: userItemId, status: status) {
                if let id = status.lucinaItemId, !items.contains(where: { $0.id == id }) { await refresh() }
                return false
            }
            return true
        } catch {
            let apiError = APIError.wrap(error)
            if apiError.status == 404 || apiError.endsSession {
                shareStatus[shareId] = nil
                return false
            }
            return true
        }
    }
}

// MARK: - Message actions (reply, hide, read aloud, send to Lucina)

extension ChatStore {
    /// Starts a reply to `item` (the quote bar). False for an optimistic bubble: the server doesn't know its id yet.
    @discardableResult
    func beginReply(_ item: TimelineItem) -> Bool {
        guard !item.isLocal else { return false }
        replyingTo = item
        return true
    }

    func cancelReply() {
        replyingTo = nil
    }

    /// The reply target for the send that is starting; the bar closes. Give it back with `beginReply` if
    /// nothing was sent.
    func takeReply() -> TimelineItem? {
        defer { replyingTo = nil }
        return replyingTo
    }

    func isHidden(_ itemId: String) -> Bool { hiddenIds.contains(itemId) }

    /// Hides a message on this phone only (the server keeps it).
    func hide(_ itemId: String) {
        hiddenIds.insert(itemId)
        if replyingTo?.id == itemId { replyingTo = nil }
        if readAloud.isActive(itemId) { readAloud.stop() }
        defaults.set(Array(hiddenIds), forKey: Self.hiddenKey)
    }

    func unhideAll() {
        hiddenIds = []
        defaults.removeObject(forKey: Self.hiddenKey)
    }

    /// The words of the message's voice note, if it has one.
    func voiceTranscript(of item: TimelineItem) -> String? {
        guard let entry = BubbleEntry.entries(item.attachments).first(where: { $0.attachment.isVoice }) else { return nil }
        return voiceTranscript(for: item, attachment: entry.attachment, fileIndex: entry.fileIndex)
    }

    /// What "Read aloud" would say for the message (nil: nothing to read).
    func speechText(for item: TimelineItem) -> String? {
        ChatSpeech.text(for: item, transcript: voiceTranscript(of: item))
    }

    /// The read-aloud text and the long-press actions of a message, worked out once per version of it: every
    /// visible row asks on each redraw, and markdown to plain text is regex work. Not kept for voice notes,
    /// whose transcript can arrive later from another message.
    func messageActions(for item: TimelineItem) -> (speech: String?, actions: [MessageAction]) {
        if let cached = derivedActions[item.id], cached.item == item { return (cached.speech, cached.actions) }
        let speech = speechText(for: item)
        let actions = MessageAction.available(for: item, speech: speech)
        if !item.attachments.contains(where: \.isVoice) { derivedActions[item.id] = (item, speech, actions) }
        return (speech, actions)
    }

    /// The right swipe's action: Lucina reads the message (or stops, when it is the one reading).
    func toggleReadAloud(_ item: TimelineItem) async -> ChatReadAloud.Outcome {
        await readAloud.toggle(itemId: item.id, text: speechText(for: item), lang: item.rich?.lang, api: api())
    }

    /// "Save to library" / "Save as note": the message's content goes to Lucina through the share pipeline
    /// (`POST /v1/share`, background session) with that intent. True when the upload was queued.
    func forward(intent: String, texts: [String], urls: [String], files: [ShareFile]) async -> Bool {
        let client = api().client
        guard let token = client.accessKey, !token.isEmpty, !(texts.isEmpty && urls.isEmpty && files.isEmpty) else { return false }
        let base = client.baseURL
        let endpoints = ([base] + client.endpoints.filter { $0 != base }).map(\.absoluteString)
        let credentials = BridgeCredentials(token: token, endpoints: endpoints, sources: ["share"])
        let sessionId = sessionId
        let payload = SharePayload(texts: texts, urls: urls, files: files)
        do {
            let record = try await Task.detached(priority: .userInitiated) {
                try ShareRequestBuilder.build(payload: payload, intent: intent, note: "", source: "ios-chat-action", credentials: credentials,
                                              directory: ShareRequestBuilder.directory(), sessionId: sessionId)
            }.value
            try ShareUploads.shared.enqueue(record, credentials: credentials)
            LucinaDonations.donate()
            return true
        } catch {
            return false
        }
    }
}
