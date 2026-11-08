import Foundation
import Observation

/// All browsing state is confined to this screen's lifetime.
@Observable @MainActor
final class NClientStore {
    var query = ""
    var sort: NClientSort = .recent
    var include: [NClientTag] = []
    var exclude: [NClientTag] = []
    var favoritesOnly = false
    private(set) var status: NClientStatus?
    private(set) var items: [NClientSummary] = []
    private(set) var page = 0
    private(set) var numPages = 1
    private(set) var loading = false
    private(set) var detailLoading = false
    private(set) var details: [Int: NClientDetail] = [:]
    private(set) var saves: [Int: NClientSaveStatus] = [:]
    private(set) var saving: Set<Int> = []
    private(set) var favoriteBusy: Set<Int> = []
    var error: String?
    var detailError: String?
    var progressError: String?
    var saveErrors: [Int: String] = [:]
    @ObservationIgnored let api: () -> NClientAPI?
    @ObservationIgnored private let onSessionEnded: (APIError) -> Void
    @ObservationIgnored private var generation = 0
    @ObservationIgnored private var detailGeneration = 0
    @ObservationIgnored private var activeSearch = NClientSearch()

    init(api: @escaping () -> NClientAPI?, onSessionEnded: @escaping (APIError) -> Void) {
        self.api = api; self.onSessionEnded = onSessionEnded
    }

    func report(_ error: Error) -> String {
        let wrapped = APIError.wrap(error)
        if wrapped.endsSession { onSessionEnded(wrapped) }
        return wrapped.status == 404 ? String(localized: "This gallery is not available.") : wrapped.userMessage
    }

    func load() async {
        guard let api = api() else { return }
        loading = true
        error = nil
        do {
            status = try await api.status()
            loading = false
            if status?.enabled == true { await reload() }
            else { items = []; details = [:] }
        } catch {
            loading = false
            self.error = report(error)
        }
    }

    func reload() async {
        guard status?.enabled == true, let api = api() else { return }
        generation += 1
        let mine = generation
        let mode = favoritesOnly
        activeSearch = NClientSearch(q: query, sort: sort, tags: include.map(\.id), exclude: exclude.map(\.id))
        let search = activeSearch
        loading = true; error = nil; items = []; page = 0
        defer { if generation == mine { loading = false } }
        do {
            let result: NClientResults
            if mode { result = NClientResults(items: try await api.favorites(), page: 1, numPages: 1) }
            else { result = try await api.search(search, page: 1) }
            guard mine == generation, !Task.isCancelled else { return }
            items = result.items; page = result.page; numPages = result.numPages
        } catch {
            if mine == generation, !Task.isCancelled { self.error = report(error) }
        }
    }

    func loadMore() async {
        guard !loading, !favoritesOnly, page < numPages, let api = api() else { return }
        let mine = generation
        loading = true
        defer { if mine == generation { loading = false } }
        do {
            let result = try await api.search(activeSearch, page: page + 1)
            guard mine == generation, !Task.isCancelled else { return }
            let known = Set(items.map(\.id))
            items += result.items.filter { !known.contains($0.id) }
            page = result.page; numPages = result.numPages; error = nil
        } catch { if mine == generation { self.error = report(error) } }
    }

    func open(_ id: Int? = nil) async -> Int? {
        guard status?.enabled == true, let api = api() else { return nil }
        detailGeneration += 1
        let mine = detailGeneration
        detailLoading = true; detailError = nil
        defer { if mine == detailGeneration { detailLoading = false } }
        do {
            let detail: NClientDetail
            if let id { detail = try await api.detail(id) } else { detail = try await api.random() }
            guard mine == detailGeneration, !Task.isCancelled else { return nil }
            details[detail.id] = detail
            return detail.id
        } catch { if mine == detailGeneration { detailError = report(error) }; return nil }
    }

    func choose(_ tag: NClientTag, excluded: Bool) {
        include.removeAll { $0.id == tag.id }; exclude.removeAll { $0.id == tag.id }
        if excluded { exclude.append(tag) } else { include.append(tag) }
        favoritesOnly = false
    }

    func toggleFavorite(_ id: Int, current: Bool) async {
        guard !favoriteBusy.contains(id), let api = api() else { return }
        favoriteBusy.insert(id)
        defer { favoriteBusy.remove(id) }
        do {
            let value = try await api.favorite(id, value: !current)
            details[id]?.favorite = value
            for key in Array(details.keys) {
                guard var detail = details[key] else { continue }
                for index in detail.related.indices where detail.related[index].id == id { detail.related[index].favorite = value }
                details[key] = detail
            }
            for index in items.indices where items[index].id == id { items[index].favorite = value }
            if favoritesOnly, !value { items.removeAll { $0.id == id } }
        } catch { self.error = report(error) }
    }

    func saveProgress(_ id: Int, page: Int, pages: Int) async {
        guard pages > 0, let api = api() else { return }
        let progress = NClientProgress(page: page, pages: pages, updated_at: Int(Date().timeIntervalSince1970))
        details[id]?.progress = progress
        for index in items.indices where items[index].id == id { items[index].progress = progress }
        do {
            try await api.progress(id, page: page, pages: pages)
            progressError = nil
        } catch { progressError = report(error) }
    }

    func startSave(_ id: Int) async {
        guard !saving.contains(id), saves[id]?.finished != false, let api = api() else { return }
        saving.insert(id); saveErrors[id] = nil
        defer { saving.remove(id) }
        do { saves[id] = try await api.save(id) }
        catch { saveErrors[id] = report(error) }
    }

    func pollSave(_ id: Int) async {
        var failures = 0
        while !Task.isCancelled, let status = saves[id], !status.finished, let api = api() {
            do {
                try await Task.sleep(for: .milliseconds(1500))
                try Task.checkCancellation()
                saves[id] = try await api.saveStatus(status.job)
                failures = 0; saveErrors[id] = nil
            } catch {
                guard !Task.isCancelled else { return }
                failures += 1
                saveErrors[id] = report(error)
                if APIError.wrap(error).status == 404 || failures >= 3 { return }
            }
        }
    }

    func cancelSave(_ id: Int) async {
        guard let job = saves[id]?.job, let api = api() else { return }
        do { try await api.cancelSave(job) }
        catch { saveErrors[id] = report(error) }
    }
}
