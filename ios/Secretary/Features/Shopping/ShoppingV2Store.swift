import Foundation
import Observation

/// Shopping v2 routes. Writes send `X-Secretary-Client: ios` so items show "via iPhone".
struct ShoppingV2Client: Sendable {
    let client: APIClient
    static let headers = ["X-Secretary-Client": "ios"]

    private static func escape(_ id: String) -> String {
        id.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed.subtracting(CharacterSet(charactersIn: "/"))) ?? id
    }

    /// `nil` catalog when the server answered 304 for `etag`.
    func catalog(lang: String, etag: String?) async throws -> (catalog: ShoppingCatalog?, etag: String?) {
        var headers: [String: String] = [:]
        if let etag { headers["If-None-Match"] = etag }
        let response = try await client.send("GET", "/v1/shopping/catalog", query: [URLQueryItem(name: "lang", value: lang)],
                                              body: nil, passThrough: [304], headers: headers)
        let tag = response.headers["etag"] ?? etag
        if response.status == 304 { return (nil, tag) }
        let json = try client.decode(JSONValue.self, from: response)
        return (ShoppingCatalog(json: json), tag)
    }

    func lists() async throws -> [ShopList] {
        let json: JSONValue = try await client.get("/v1/shopping/lists")
        return ShopList.lists(from: json)
    }

    func state(_ listId: String) async throws -> ListState {
        let json: JSONValue = try await client.get("/v1/shopping/lists/\(Self.escape(listId))")
        return ListState(json: json, fallbackId: listId)
    }

    func ops(_ listId: String, expectedRevision: Int?, ops: [ShopOp]) async throws -> OpsResponse {
        struct Body: Encodable { let expectedRevision: Int?; let ops: [ShopOp] }
        let json: JSONValue = try await client.send("POST", "/v1/shopping/lists/\(Self.escape(listId))/ops",
                                                    body: Body(expectedRevision: expectedRevision, ops: ops),
                                                    idempotent: true, headers: Self.headers)
        return OpsResponse(json: json)
    }

    func createList(name: String, theme: ListTheme) async throws -> ShopList? {
        struct Body: Encodable { let name: String; let theme: String }
        let json: JSONValue = try await client.send("POST", "/v1/shopping/lists", body: Body(name: name, theme: theme.rawValue),
                                                    headers: Self.headers)
        return ShopList(json: json["list"] ?? json)
    }

    func updateList(_ id: String, name: String?, theme: ListTheme?) async throws -> ShopList? {
        struct Body: Encodable { let name: String?; let theme: String? }
        let json: JSONValue = try await client.send("PATCH", "/v1/shopping/lists/\(Self.escape(id))",
                                                    body: Body(name: name, theme: theme?.rawValue), headers: Self.headers)
        return ShopList(json: json["list"] ?? json)
    }

    func deleteList(_ id: String) async throws {
        _ = try await client.send("DELETE", "/v1/shopping/lists/\(Self.escape(id))", body: nil, headers: Self.headers)
    }
}

/// Shopping v2 state: lists, the catalog, and an outbox of ops that survives restarts.
///
/// What is shown is the last server state of a list with the pending ops applied on top
/// (the same semantics as the server), so every tap is instant and works offline. The
/// outbox is replayed in order with the same op ids (the server dedups them); on
/// `409 revision_conflict` the list is refetched and the unapplied ops are rebased with
/// new op ids.
@Observable
@MainActor
final class ShoppingV2Store {
    enum Availability: Equatable { case unknown, available, unavailable }

    struct OutboxEntry: Codable, Equatable, Sendable {
        var listId: String
        var op: ShopOp
        var queuedAt: Date
    }

    struct Undo: Equatable, Identifiable {
        let id = UUID()
        let message: String
        let listId: String
        let ops: [ShopOp]
    }

    private(set) var availability: Availability = .unknown
    private(set) var lists: [ShopList] = []
    var currentListId: String = ShopList.defaultId { didSet { persist() } }
    private(set) var server: [String: ListState] = [:]
    private(set) var outbox: [OutboxEntry] = []
    private(set) var catalog: ShoppingCatalog = .empty
    private(set) var isLoading = false
    private(set) var isSyncing = false
    /// The outbox could not be delivered (offline); shown as "waiting to sync".
    private(set) var syncProblem: String?
    var error: String?
    var undo: Undo?

    @ObservationIgnored private let api: () -> ShoppingV2Client
    @ObservationIgnored private let fileURL: URL
    @ObservationIgnored private var catalogETag: String?
    @ObservationIgnored private var catalogLang: String?
    @ObservationIgnored private var flushTask: Task<Void, Never>?
    @ObservationIgnored private var retryTask: Task<Void, Never>?
    @ObservationIgnored private(set) var search = CatalogSearch(catalog: .empty, lang: "it")
    @ObservationIgnored var onTransportError: ((APIError) -> Void)?
    @ObservationIgnored let now: () -> Date

    init(api: @escaping () -> ShoppingV2Client, fileURL: URL = ShoppingV2Store.defaultFileURL, now: @escaping () -> Date = Date.init) {
        self.api = api
        self.fileURL = fileURL
        self.now = now
        restore()
    }

    nonisolated static var defaultFileURL: URL {
        let base = (try? FileManager.default.url(for: .applicationSupportDirectory, in: .userDomainMask, appropriateFor: nil, create: true))
            ?? FileManager.default.temporaryDirectory
        return base.appendingPathComponent("Secretary", isDirectory: true).appendingPathComponent("shopping-v2.json")
    }

    // MARK: - What is shown

    func pending(_ listId: String) -> [ShopOp] { outbox.filter { $0.listId == listId }.map(\.op) }

    /// The server state with the pending ops applied.
    func display(_ listId: String) -> ListState? {
        guard var state = server[listId] else { return nil }
        let ops = pending(listId)
        if !ops.isEmpty { state.items = ShopOps.apply(state.items, ops, context: context(listId)).items }
        return state
    }

    var current: ListState? { display(currentListId) }

    var currentList: ShopList? { lists.first { $0.id == currentListId } ?? current?.list }

    var needed: [ShopItem] { (current?.items ?? []).filter { $0.status == .needed }.sorted { $0.position < $1.position } }

    var recent: [ShopItem] {
        (current?.items ?? []).filter { $0.status == .recent }.sorted { ($0.updatedAt, $0.id) > ($1.updatedAt, $1.id) }
    }

    func icon(for item: ShopItem) -> String {
        item.icon ?? catalog.item(item.catalogId)?.icon ?? ShoppingCatalog.defaultIcon
    }

    private func context(_ listId: String) -> ShopOps.Context {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        let catalog = catalog
        return ShopOps.Context(listId: listId, now: formatter.string(from: now()), catalog: { catalog.item($0) })
    }

    // MARK: - Loading

    func load(lang: String) async {
        isLoading = server[currentListId] == nil
        defer { isLoading = false }
        await loadCatalog(lang: lang)
        do {
            let lists = try await api().lists()
            availability = .available
            self.lists = lists
            if !lists.contains(where: { $0.id == currentListId }) { currentListId = lists.first?.id ?? ShopList.defaultId }
            try await refreshState(currentListId)
            error = nil
            persist()
            await flush()
        } catch {
            let apiError = APIError.wrap(error)
            if apiError.status == 404 {
                availability = .unavailable // the server only has the legacy list
                return
            }
            onTransportError?(apiError)
            if server[currentListId] == nil { self.error = apiError.userMessage }
        }
    }

    func loadCatalog(lang: String) async {
        do {
            let result = try await api().catalog(lang: lang, etag: catalogLang == lang ? catalogETag : nil)
            if let catalog = result.catalog {
                self.catalog = catalog
                catalogLang = lang
            }
            catalogETag = result.etag
            search = CatalogSearch(catalog: catalog, lang: lang)
        } catch {
            // The cached catalog (if any) keeps working.
            search = CatalogSearch(catalog: catalog, lang: lang)
        }
    }

    private func refreshState(_ listId: String) async throws {
        server[listId] = try await api().state(listId)
    }

    func select(_ listId: String) async {
        currentListId = listId
        undo = nil
        do {
            try await refreshState(listId)
            persist()
        } catch {
            onTransportError?(APIError.wrap(error))
        }
    }

    // MARK: - Actions

    /// Queues ops (applied at once on screen) and sends them.
    func perform(_ ops: [ShopOp], on listId: String? = nil, undo: Undo? = nil) {
        let list = listId ?? currentListId
        guard !ops.isEmpty else { return }
        outbox.append(contentsOf: ops.map { OutboxEntry(listId: list, op: $0, queuedAt: now()) })
        if let undo { self.undo = undo }
        persist()
        scheduleFlush()
    }

    /// Adds a catalog item, or free text (matched to the catalog when the name is exact).
    func add(_ text: String, catalogItem: CatalogItem? = nil) {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        let item = catalogItem ?? search.exactMatch(trimmed)
        let name = item.map { $0.displayName(search.lang) } ?? trimmed
        guard !name.isEmpty else { return }
        let itemId = UUID().uuidString.lowercased()
        let existing = current?.items.first {
            $0.isActive && TextFold.itemKey(catalogId: $0.catalogId, name: $0.name) == TextFold.itemKey(catalogId: item?.id, name: name)
        }
        let op = ShopOp.add(itemId: existing?.id ?? itemId, catalogId: item?.id, name: name, section: item?.section)
        var undo: Undo?
        if existing == nil {
            undo = Undo(message: "Added \(name)", listId: currentListId, ops: [ShopOp(kind: .remove, itemId: itemId)])
        } else if existing?.status == .recent, let id = existing?.id {
            undo = Undo(message: "Added \(name) back", listId: currentListId, ops: [ShopOp(kind: .purchase, itemId: id)])
        }
        perform([op], undo: undo)
    }

    /// Tap: buy a needed item, or put a recent one back on the list.
    func toggle(_ item: ShopItem) {
        if item.status == .needed {
            perform([ShopOp(kind: .purchase, itemId: item.id)],
                    undo: Undo(message: "Bought \(item.name)", listId: currentListId, ops: [ShopOp(kind: .need, itemId: item.id)]))
        } else {
            perform([ShopOp(kind: .need, itemId: item.id)],
                    undo: Undo(message: "\(item.name) is back on the list", listId: currentListId, ops: [ShopOp(kind: .purchase, itemId: item.id)]))
        }
    }

    func remove(_ item: ShopItem) {
        // A removed row never comes back, so undo re-adds it as a new row.
        let restore = ShopOp(kind: .add, itemId: UUID().uuidString.lowercased(), catalogId: item.catalogId, name: item.name,
                             spec: item.spec.isEmpty ? nil : item.spec, quantity: item.quantity, unit: item.unit, section: item.section)
        var ops = [restore]
        if item.status == .recent, let id = restore.itemId { ops.append(ShopOp(kind: .purchase, itemId: id)) }
        perform([ShopOp(kind: .remove, itemId: item.id)], undo: Undo(message: "Deleted \(item.name)", listId: currentListId, ops: ops))
    }

    func setSpec(_ item: ShopItem, _ spec: String) {
        guard spec != item.spec else { return }
        perform([ShopOp(kind: .setSpec, itemId: item.id, spec: spec)])
    }

    func setQuantity(_ item: ShopItem, _ quantity: Double?, unit: String?) {
        guard quantity != item.quantity || unit != item.unit else { return }
        perform([ShopOp(kind: .setQuantity, itemId: item.id, quantity: quantity, unit: quantity == nil ? nil : unit)])
    }

    func setSection(_ item: ShopItem, _ section: String?) {
        guard section != item.section else { return }
        perform([ShopOp(kind: .setSection, itemId: item.id, section: section)])
    }

    func rename(_ item: ShopItem, _ name: String) {
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, trimmed != item.name else { return }
        perform([ShopOp(kind: .rename, itemId: item.id, name: trimmed)])
    }

    func move(_ item: ShopItem, to listId: String) {
        guard listId != currentListId else { return }
        perform([ShopOp(kind: .move, itemId: item.id, toListId: listId)])
    }

    func clearRecent() {
        perform([ShopOp(kind: .clearRecent)])
    }

    func applyUndo() {
        guard let undo else { return }
        self.undo = nil
        perform(undo.ops, on: undo.listId)
    }

    // MARK: - Lists

    func createList(name: String, theme: ListTheme) async -> Bool {
        do {
            guard let list = try await api().createList(name: name, theme: theme) else { return false }
            lists.append(list)
            await select(list.id)
            return true
        } catch {
            self.error = APIError.wrap(error).userMessage
            return false
        }
    }

    func updateList(_ id: String, name: String, theme: ListTheme) async -> Bool {
        do {
            if let list = try await api().updateList(id, name: name, theme: theme), let index = lists.firstIndex(where: { $0.id == id }) {
                lists[index] = list
            }
            persist()
            return true
        } catch {
            self.error = APIError.wrap(error).userMessage
            return false
        }
    }

    func deleteList(_ id: String) async {
        guard id != ShopList.defaultId else { return }
        do {
            try await api().deleteList(id)
            lists.removeAll { $0.id == id }
            outbox.removeAll { $0.listId == id }
            server[id] = nil
            if currentListId == id { await select(ShopList.defaultId) }
            persist()
        } catch {
            self.error = APIError.wrap(error).userMessage
        }
    }

    // MARK: - Outbox

    private func scheduleFlush() {
        guard flushTask == nil else { return }
        flushTask = Task { [weak self] in
            await self?.flush()
            self?.flushTask = nil
            // Ops queued while that flush was in flight.
            if let self, !self.outbox.isEmpty, self.syncProblem == nil { self.scheduleFlush() }
        }
    }

    /// Sends the outbox, list by list, in order.
    func flush() async {
        guard !isSyncing, !outbox.isEmpty else { return }
        isSyncing = true
        defer { isSyncing = false }
        var seenLists: [String] = []
        for entry in outbox where !seenLists.contains(entry.listId) { seenLists.append(entry.listId) }
        for listId in seenLists {
            guard await flush(listId) else { return }
        }
        syncProblem = nil
    }

    /// Returns false when the network is the problem (the rest waits for a retry).
    private func flush(_ listId: String) async -> Bool {
        var batch = Array(pending(listId).prefix(200))
        var touchedLists: Set<String> = []
        for attempt in 0..<2 {
            guard !batch.isEmpty else { return true }
            do {
                let response = try await api().ops(listId, expectedRevision: server[listId]?.revision, ops: batch)
                let done = Set(response.applied + response.duplicates)
                outbox.removeAll { $0.listId == listId && done.contains($0.op.opId) }
                for op in batch where op.kind == .move { if let to = op.toListId { touchedLists.insert(to) } }
                if let items = response.items, var state = server[listId] {
                    state.items = items
                    state.revision = response.revision
                    server[listId] = state
                } else {
                    try? await refreshState(listId)
                }
                persist()
                for other in touchedLists where server[other] != nil { try? await refreshState(other) }
                return true
            } catch {
                let apiError = APIError.wrap(error)
                if apiError.status == 409, attempt == 0 {
                    // Rebase the unapplied ops on the fresh list, with new op ids.
                    do {
                        try await refreshState(listId)
                    } catch {
                        return fail(APIError.wrap(error))
                    }
                    let fresh = server[listId]?.items ?? []
                    let rebased = ShopOps.rebase(fresh, batch, context: context(listId))
                    let old = Set(batch.map(\.opId))
                    let queuedAt = outbox.first { $0.listId == listId }?.queuedAt ?? now()
                    let firstIndex = outbox.firstIndex { old.contains($0.op.opId) } ?? outbox.endIndex
                    outbox.removeAll { old.contains($0.op.opId) }
                    outbox.insert(contentsOf: rebased.map { OutboxEntry(listId: listId, op: $0, queuedAt: queuedAt) },
                                  at: min(firstIndex, outbox.endIndex))
                    persist()
                    batch = rebased
                    continue
                }
                if apiError.isConnectivity || apiError.status >= 500 || apiError.status == 429 {
                    return fail(apiError)
                }
                // The server refused these ops (validation, a list or item that is gone):
                // drop them and show the server's copy.
                let old = Set(batch.map(\.opId))
                outbox.removeAll { old.contains($0.op.opId) }
                self.error = apiError.status == 409 ? "This list changed somewhere else; I refreshed it." : apiError.userMessage
                if apiError.endsSession { onTransportError?(apiError) }
                try? await refreshState(listId)
                persist()
                return true
            }
        }
        return true
    }

    private func fail(_ error: APIError) -> Bool {
        onTransportError?(error)
        syncProblem = error.userMessage
        scheduleRetry()
        return false
    }

    private func scheduleRetry() {
        retryTask?.cancel()
        retryTask = Task { [weak self] in
            try? await Task.sleep(for: .seconds(15))
            guard !Task.isCancelled, let self else { return }
            self.syncProblem = nil
            self.scheduleFlush()
        }
    }

    /// On foreground: deliver anything left from an earlier session.
    func resume() {
        syncProblem = nil
        if !outbox.isEmpty { scheduleFlush() }
    }

    // MARK: - Persistence

    private struct Snapshot: Codable {
        var currentListId: String
        var lists: [ShopList]
        var server: [String: ListState]
        var outbox: [OutboxEntry]
        var catalog: ShoppingCatalog?
        var catalogETag: String?
        var catalogLang: String?
    }

    private func persist() {
        let snapshot = Snapshot(currentListId: currentListId, lists: lists, server: server, outbox: outbox,
                                catalog: catalog.items.isEmpty ? nil : catalog, catalogETag: catalogETag, catalogLang: catalogLang)
        do {
            try FileManager.default.createDirectory(at: fileURL.deletingLastPathComponent(), withIntermediateDirectories: true)
            try JSONEncoder().encode(snapshot).write(to: fileURL, options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
        } catch {
            // The outbox only loses durability, never the state on screen.
        }
    }

    private func restore() {
        guard let data = try? Data(contentsOf: fileURL), let snapshot = try? JSONDecoder().decode(Snapshot.self, from: data) else { return }
        currentListId = snapshot.currentListId
        lists = snapshot.lists
        server = snapshot.server
        outbox = snapshot.outbox
        if let catalog = snapshot.catalog {
            self.catalog = catalog
            catalogETag = snapshot.catalogETag
            catalogLang = snapshot.catalogLang
            search = CatalogSearch(catalog: catalog, lang: snapshot.catalogLang ?? catalog.lang)
        }
    }

    func reset() {
        flushTask?.cancel()
        retryTask?.cancel()
        flushTask = nil
        lists = []
        server = [:]
        outbox = []
        undo = nil
        error = nil
        syncProblem = nil
        availability = .unknown
        try? FileManager.default.removeItem(at: fileURL)
    }
}
