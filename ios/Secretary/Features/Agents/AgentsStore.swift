import Foundation
import Observation

/// The agents list and the one stream (`GET /v1/agents/stream`, SSE) behind the list and the open agent.
///
/// The stream runs while an Agents screen is on and the app is active (`retain`/`release`; a screen change
/// that releases and retains again within 1.5 s keeps the connection). It resumes with `Last-Event-ID`,
/// refetches on `stream.reset`, reconnects with backoff, and while it is down the screens poll.
@Observable
@MainActor
final class AgentsStore {
    private(set) var items: [AgentListItem] = []
    private(set) var loaded = false
    var loadError: String?
    /// The stream is connected.
    private(set) var isLive = false
    /// `showArchived` changes what the list asks for.
    private(set) var showArchived = false
    /// A bump that screens watch to catch up after a reset.
    private(set) var resetCount = 0
    /// requestId → the owner approval the bridge filed for it.
    private(set) var approvalIds: [String: String] = [:]
    /// The server has no `/v1/agents` (404): the entry says so instead of failing.
    private(set) var unavailable = false

    @ObservationIgnored var onTransportError: ((APIError) -> Void)?
    @ObservationIgnored private let api: () -> AgentsAPI?
    @ObservationIgnored private(set) var lastEventId: String?
    @ObservationIgnored private var observers: [String: (AgentBusEvent) -> Void] = [:]
    @ObservationIgnored private var users = 0
    @ObservationIgnored private var streamTask: Task<Void, Never>?
    @ObservationIgnored private var stopTask: Task<Void, Never>?
    @ObservationIgnored private var generation = 0

    static let backoff: [Duration] = [.seconds(1), .seconds(2), .seconds(5), .seconds(10), .seconds(30)]

    init(api: @escaping () -> AgentsAPI?) {
        self.api = api
    }

    var apiOrNil: AgentsAPI? { api() }

    // MARK: - List

    /// Needs-input first, then failed, running, finished, done; newest first inside each.
    var sections: [(bucket: AgentBucket, items: [AgentListItem])] {
        let order: [AgentBucket] = [.needsInput, .failed, .running, .attention, .done]
        return order.compactMap { bucket in
            let rows = items.filter { $0.bucket == bucket }.sorted(by: Self.newerFirst)
            return rows.isEmpty ? nil : (bucket, rows)
        }
    }

    static func newerFirst(_ a: AgentListItem, _ b: AgentListItem) -> Bool {
        let (x, y) = (AgentDates.parse(a.updatedAt), AgentDates.parse(b.updatedAt))
        if let x, let y, x != y { return x > y }
        return a.updatedAt > b.updatedAt
    }

    func refresh() async {
        guard let api = api() else { return }
        generation += 1
        let mine = generation
        let archived = showArchived
        do {
            let list = try await api.list(archived: archived)
            guard mine == generation else { return }
            items = list
            loaded = true
            loadError = nil
            unavailable = false
        } catch {
            guard mine == generation else { return }
            let e = APIError.wrap(error)
            if e.status == 404 { unavailable = true }
            loadError = e.userMessage
            if e.endsSession { onTransportError?(e) }
        }
    }

    func setArchived(_ on: Bool) async {
        guard on != showArchived else { return }
        showArchived = on
        items = []
        loaded = false
        await refresh()
    }

    func reset() {
        streamTask?.cancel()
        streamTask = nil
        stopTask?.cancel()
        users = 0
        items = []
        loaded = false
        loadError = nil
        isLive = false
        lastEventId = nil
        approvalIds = [:]
        showArchived = false
        unavailable = false
        observers = [:]
    }

    // MARK: - Events

    func observe(_ agentId: String, _ handler: @escaping (AgentBusEvent) -> Void) { observers[agentId] = handler }
    func stopObserving(_ agentId: String) { observers[agentId] = nil }

    /// One frame of the stream (also what the tests feed).
    func apply(_ event: AgentBusEvent) {
        switch event {
        case let .state(snapshot):
            let item = snapshot.item
            let belongs = item.isArchived == showArchived
            if let i = items.firstIndex(where: { $0.id == item.id }) {
                if belongs { items[i] = item } else { items.remove(at: i) }
            } else if belongs, loaded {
                items.append(item)
            }
            for p in snapshot.pendingPermissions {
                if let a = p.approvalId { approvalIds[p.id] = a }
            }
            observers[snapshot.id]?(event)
        case let .deleted(id):
            items.removeAll { $0.id == id }
            observers[id]?(event)
        case let .permission(view):
            if let a = view.approvalId { approvalIds[view.id] = a }
            if let id = view.agentId { observers[id]?(event) }
        case let .timeline(id, _, _), let .permissionRequested(id, _), let .permissionResolved(id, _):
            if case let .permissionRequested(_, p) = event, let a = p.approvalId { approvalIds[p.id] = a }
            observers[id]?(event)
        case .reset:
            resetCount += 1
            Task { await refresh() }
            for handler in observers.values { handler(.reset) }
        case .ignored:
            break
        }
    }

    /// Reads one SSE frame: remembers its id, then applies the event.
    func handle(_ frame: SSEEvent) {
        if let id = frame.id { lastEventId = id }
        guard let event = AgentBusEvent.decode(frame) else { return }
        apply(event)
    }

    // MARK: - The stream

    /// A screen wants the stream.
    func retain() {
        users += 1
        stopTask?.cancel()
        stopTask = nil
        if streamTask == nil { streamTask = Task { [weak self] in await self?.runStream() } }
    }

    /// A screen no longer does. The stream stops 1.5 s after the last one.
    func release() {
        users = max(0, users - 1)
        guard users == 0 else { return }
        stopTask?.cancel()
        stopTask = Task { [weak self] in
            try? await Task.sleep(for: .milliseconds(1500))
            guard !Task.isCancelled, let self, self.users == 0 else { return }
            self.streamTask?.cancel()
            self.streamTask = nil
            self.isLive = false
        }
    }

    private func runStream() async {
        var attempt = 0
        while !Task.isCancelled {
            var wait = Self.backoff[min(attempt, Self.backoff.count - 1)]
            do {
                guard let api = api() else { throw APIError.unauthorized(code: "unauthorized", message: "") }
                let bytes = try await api.client.openStream("/v1/agents/stream", lastEventId: lastEventId)
                isLive = true
                attempt = 0
                try await Self.readEvents(bytes) { [weak self] frame in self?.handle(frame) }
            } catch is CancellationError {
                break
            } catch {
                let e = APIError.wrap(error)
                if e.endsSession { onTransportError?(e); break }
                if e.status == 404 { unavailable = true; break }
                if case let .rateLimited(_, retryAfter?, _) = e { wait = max(wait, .seconds(retryAfter)) }
            }
            attempt += 1
            isLive = false
            if Task.isCancelled { break }
            try? await Task.sleep(for: wait)
        }
        isLive = false
        streamTask = nil
    }

    /// SSE lines off the byte stream (not `bytes.lines`, which drops the blank lines that end each event).
    nonisolated private static func readEvents(_ bytes: URLSession.AsyncBytes, onEvent: @MainActor @escaping (SSEEvent) -> Void) async throws {
        var parser = SSEParser()
        var buffer: [UInt8] = []
        buffer.reserveCapacity(4096)
        for try await byte in bytes {
            buffer.append(byte)
            guard byte == 0x0A || byte == 0x0D else { continue }
            for line in SSEParser.lines(from: &buffer) {
                if let frame = parser.feed(line) { await onEvent(frame) }
            }
        }
    }
}

/// One open agent: its snapshot, its timeline, and the actions on it.
@Observable
@MainActor
final class AgentDetailModel {
    let agentId: String
    private(set) var snapshot: AgentSnapshot?
    private(set) var rows: [AgentTimelineRow] = []
    private(set) var entries: [AgentEntry] = []
    private(set) var epoch: String?
    private(set) var loaded = false
    var loadError: String?
    var actionError: String?
    var draft = ""
    private(set) var sending = false
    /// request ids being answered right now.
    private(set) var answering: Set<String> = []
    private(set) var diff: AgentDiff?
    private(set) var diffError: String?
    private(set) var diffLoading = false

    @ObservationIgnored private let store: AgentsStore
    @ObservationIgnored private var catchingUp = false
    @ObservationIgnored private var loading = false

    static let pageSize = 500

    init(agentId: String, store: AgentsStore) {
        self.agentId = agentId
        self.store = store
    }

    var lastSeq: Int { rows.last?.seq ?? 0 }
    var isRunning: Bool { snapshot?.status == .running }
    var isArchived: Bool { snapshot?.archivedAt != nil }
    var pending: [AgentPermission] { snapshot?.pendingPermissions ?? [] }

    func approvalId(for permission: AgentPermission) -> String? { permission.approvalId ?? store.approvalIds[permission.id] }

    func attach() {
        store.observe(agentId) { [weak self] event in self?.apply(event) }
    }

    func detach() { store.stopObserving(agentId) }

    // MARK: Loading

    /// Everything: the snapshot, then the whole timeline in pages.
    func load() async {
        guard let api = store.apiOrNil, !loading else { return }
        loading = true
        defer { loading = false }
        do {
            let snap = try await api.snapshot(agentId)
            var all: [AgentTimelineRow] = []
            var after = 0
            var newEpoch: String?
            while true {
                let page = try await api.timeline(agentId, afterSeq: after, limit: Self.pageSize)
                newEpoch = page.epoch
                all.append(contentsOf: page.rows)
                guard page.rows.count >= Self.pageSize, let last = page.rows.last, last.seq > after else { break }
                after = last.seq
            }
            snapshot = snap
            rows = all
            epoch = newEpoch
            rebuild()
            loaded = true
            loadError = nil
        } catch {
            fail(error)
        }
    }

    /// Rows after the last one we have; the snapshot too. A new epoch means the timeline was rebuilt: reload.
    func catchUp() async {
        guard loaded else { await load(); return }
        guard let api = store.apiOrNil, !catchingUp else { return }
        catchingUp = true
        defer { catchingUp = false }
        do {
            let snap = try await api.snapshot(agentId)
            var after = lastSeq
            var fresh: [AgentTimelineRow] = []
            var newEpoch = epoch
            while true {
                let page = try await api.timeline(agentId, afterSeq: after, limit: Self.pageSize)
                newEpoch = page.epoch
                if let known = epoch, !page.epoch.isEmpty, page.epoch != known {
                    catchingUp = false
                    loaded = false
                    await load()
                    return
                }
                fresh.append(contentsOf: page.rows)
                guard page.rows.count >= Self.pageSize, let last = page.rows.last, last.seq > after else { break }
                after = last.seq
            }
            snapshot = snap
            epoch = newEpoch
            merge(fresh)
            loadError = nil
        } catch {
            fail(error)
        }
    }

    private func fail(_ error: Error) {
        guard !(error is CancellationError) else { return }
        let e = APIError.wrap(error)
        loadError = e.userMessage
        if e.endsSession { store.onTransportError?(e) }
    }

    private func rebuild() { entries = AgentTimeline.entries(rows) }

    /// Rows with a seq above ours, in order; duplicates are dropped.
    private func merge(_ fresh: [AgentTimelineRow]) {
        var changed = false
        for row in fresh where row.seq > lastSeq {
            rows.append(row)
            changed = true
        }
        if changed { rebuild() }
    }

    // MARK: Stream events

    func apply(_ event: AgentBusEvent) {
        switch event {
        case let .state(s):
            snapshot = s
        case let .timeline(_, row, rowEpoch):
            guard loaded else { return }
            if let known = epoch, !rowEpoch.isEmpty, rowEpoch != known {
                loaded = false
                Task { await load() }
                return
            }
            if row.seq <= lastSeq { return }
            if row.seq == lastSeq + 1 {
                rows.append(row)
                rebuild()
            } else {
                // A row went missing: fetch from where we are.
                Task { await catchUp() }
            }
        case let .permissionRequested(_, p):
            guard var s = snapshot, !s.pendingPermissions.contains(where: { $0.id == p.id }) else { return }
            s.pendingPermissions.append(p)
            snapshot = s
        case let .permissionResolved(_, requestId):
            removePending(requestId)
        case let .permission(view):
            if view.status != "pending" { removePending(view.id) }
        case .deleted:
            snapshot = nil
            loadError = String(localized: "This agent was deleted.")
        case .reset:
            Task { await catchUp() }
        case .ignored:
            break
        }
    }

    private func removePending(_ id: String) {
        guard var s = snapshot, s.pendingPermissions.contains(where: { $0.id == id }) else { return }
        s.pendingPermissions.removeAll { $0.id == id }
        snapshot = s
    }

    // MARK: Actions

    /// Sends the draft. While the agent runs it steers the running turn.
    @discardableResult
    func send() async -> Bool {
        let text = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty, !sending, let api = store.apiOrNil else { return false }
        sending = true
        defer { sending = false }
        do {
            try await api.send(agentId, prompt: text, steer: isRunning)
            draft = ""
            actionError = nil
            if !store.isLive { await catchUp() }
            return true
        } catch {
            actionError = APIError.wrap(error).userMessage
            return false
        }
    }

    func stop() async {
        guard let api = store.apiOrNil else { return }
        do {
            try await api.stop(agentId)
            actionError = nil
            if !store.isLive { await catchUp() }
        } catch {
            actionError = APIError.wrap(error).userMessage
        }
    }

    func setArchived(_ archive: Bool) async {
        guard let api = store.apiOrNil else { return }
        do {
            if archive { try await api.archive(agentId) } else { try await api.unarchive(agentId) }
            actionError = nil
            await catchUp()
            await store.refresh()
        } catch {
            actionError = APIError.wrap(error).userMessage
        }
    }

    func answer(_ permission: AgentPermission, allow: Bool) async {
        guard let api = store.apiOrNil, !answering.contains(permission.id) else { return }
        answering.insert(permission.id)
        defer { answering.remove(permission.id) }
        do {
            try await api.answer(agentId, permission: permission, allow: allow)
            removePending(permission.id)
            actionError = nil
            if !store.isLive { await catchUp() }
        } catch {
            let e = APIError.wrap(error)
            switch e.serverCode {
            case "PERMISSION_ALREADY_ANSWERED", "PERMISSION_NOT_FOUND", "PERMISSION_EXPIRED":
                // Answered elsewhere (the web, the Approvals sheet), or it timed out.
                removePending(permission.id)
                actionError = e.userMessage
                await catchUp()
            default:
                actionError = e.userMessage
            }
        }
    }

    func loadDiff() async {
        guard let workspaceId = snapshot?.workspaceId, let api = store.apiOrNil else {
            diffError = String(localized: "This agent has no workspace to diff.")
            return
        }
        diffLoading = true
        defer { diffLoading = false }
        do {
            let d = try await api.diff(workspaceId: workspaceId)
            diff = d
            diffError = d.error
        } catch {
            diffError = APIError.wrap(error).userMessage
        }
    }
}

extension APIError {
    /// agentd's error code (`PERMISSION_ALREADY_ANSWERED`, `AGENT_NOT_FOUND`, …) of a server answer.
    var serverCode: String? {
        if case let .server(_, code, _) = self { return code }
        return nil
    }
}
