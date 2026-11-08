import Foundation
import Observation

/// `GET /v1/system/overview` with this device's token (`require_device` on secretary-server).
struct ServerAPI: Sendable {
    let client: APIClient

    /// `fresh` skips the server's 30 s cache (pull to refresh).
    func overview(fresh: Bool) async throws -> ServerOverview {
        try await client.get("/v1/system/overview", query: fresh ? [URLQueryItem(name: "fresh", value: "1")] : [], timeout: 20)
    }
}

/// The Server plugin's data: the last overview, kept on screen while a refresh runs or fails, and what a push or
/// link asked to open (applied once the data that holds it has loaded).
@Observable
@MainActor
final class ServerStore {
    private(set) var overview: ServerOverview?
    /// The last load failed (shown as a quiet banner over the last data).
    var error: String?
    private(set) var isLoading = false
    /// When the last overview arrived.
    private(set) var loadedAt: Date?
    var tab: ServerTab = .overview
    var sheet: ServerSheet?
    /// The host whose detail is pushed (`?host=`).
    var openHost: String?

    @ObservationIgnored var onTransportError: ((APIError) -> Void)?
    /// What a push or link asked for, until the data has it.
    @ObservationIgnored private(set) var pending: ServerTarget?
    @ObservationIgnored private let api: () -> ServerAPI?
    @ObservationIgnored private var generation = 0
    @ObservationIgnored private var targetTask: Task<Void, Never>?

    init(api: @escaping () -> ServerAPI?) {
        self.api = api
    }

    func load(fresh: Bool = false) async {
        guard !Task.isCancelled, !isLoading || fresh, let api = api() else { return }
        generation += 1
        let mine = generation
        isLoading = true
        defer { if mine == generation { isLoading = false } }
        do {
            let next = try await api.overview(fresh: fresh)
            guard mine == generation, !Task.isCancelled else { return }
            overview = next
            loadedAt = Date()
            error = nil
            publishWidget(next)
            applyPending(final: true)
        } catch is CancellationError {
            return
        } catch {
            guard mine == generation, !Task.isCancelled else { return }
            let e = APIError.wrap(error)
            self.error = e.userMessage
            if e.endsSession { onTransportError?(e) }
            // Keep the requested target across an outage; the next successful load resolves it.
        }
    }

    /// A push, widget or link: show `tab` and open `target` (now, or once the data has loaded).
    func open(_ link: ServerLink) {
        pending = nil
        sheet = nil
        openHost = nil
        if let tab = link.tab { self.tab = tab }
        guard let target = link.target else { return }
        pending = target
        applyPending(final: false)
        // Not in the data on screen (or nothing loaded yet): look again.
        if pending != nil, !isLoading {
            targetTask?.cancel()
            targetTask = Task { await load() }
        }
    }

    /// Opens the pending target if the current data holds it. `final` (fresh data): a target that isn't there
    /// is dropped, so it never opens later by surprise.
    private func applyPending(final: Bool) {
        guard let target = pending, let overview else { return }
        let resolution = target.resolve(in: overview)
        guard resolution != nil || final else { return }
        pending = nil
        switch resolution {
        case let .sheet(sheet)?:
            tab = .overview
            self.sheet = sheet
        case let .host(id)?:
            tab = .overview
            sheet = nil
            openHost = id
        case nil:
            break
        }
    }

    func reset() {
        targetTask?.cancel()
        targetTask = nil
        generation += 1
        overview = nil
        error = nil
        isLoading = false
        loadedAt = nil
        tab = .overview
        sheet = nil
        openHost = nil
        pending = nil
    }

    // MARK: Widget

    /// Hands the widget the summary; it redraws now only when the status or the count changed.
    private func publishWidget(_ overview: ServerOverview) {
        let section = ServerSection(overview, fetchedAt: Date())
        let old = WidgetStore.read(ServerSection.self)
        WidgetStore.write(section)
        if old?.overall != section.overall || old?.attentionCount != section.attentionCount || old?.titles != section.titles {
            WidgetStore.reload([.server])
        }
    }
}

extension ServerSection {
    init(_ overview: ServerOverview, fetchedAt: Date) {
        self.init(fetchedAt: fetchedAt, overall: overview.overall.rawValue, attentionCount: overview.needCount,
                  titles: Array(overview.attention.prefix(3).map(\.title)), generatedAt: overview.generatedDate)
    }
}
