import Foundation
import Observation
import UIKit

/// Feed choices and explanatory text shared by the browser and its tests.
enum PixivBrowseChoices {
    static let safeModes = ["day", "week", "month", "day_male", "day_female", "original", "rookie"]
    static let adultModes = ["day_r18", "week_r18", "day_male_r18", "day_female_r18"]

    static func modes(content: PixivSettings.Content, adult: Bool) -> [String] {
        guard adult else { return safeModes }
        switch content {
        case .safe: return safeModes
        case .r18: return adultModes
        case .all: return safeModes + adultModes
        }
    }

    static func label(_ value: String) -> String {
        switch value {
        case "ranking": return String(localized: "Ranking")
        case "search": return String(localized: "Search")
        case "bookmarks": return String(localized: "Bookmarks")
        case "safe": return String(localized: "All ages")
        case "r18": return String(localized: "R-18")
        case "all": return String(localized: "All")
        case "day": return String(localized: "Daily")
        case "week": return String(localized: "Weekly")
        case "month": return String(localized: "Monthly")
        case "day_male": return String(localized: "Popular with men")
        case "day_female": return String(localized: "Popular with women")
        case "original": return String(localized: "Original")
        case "rookie": return String(localized: "Rookies")
        case "day_r18": return String(localized: "Daily R-18")
        case "week_r18": return String(localized: "Weekly R-18")
        case "day_male_r18": return String(localized: "Popular with men R-18")
        case "day_female_r18": return String(localized: "Popular with women R-18")
        case "date_d": return String(localized: "Newest")
        case "date": return String(localized: "Oldest")
        case "popular": return String(localized: "Popular")
        case "illust": return String(localized: "Illustrations")
        case "manga": return String(localized: "Manga")
        case "ugoira": return String(localized: "Ugoira")
        default: return value
        }
    }

    static func notice(_ code: String?) -> String? {
        switch code {
        case "r18_needs_session": return String(localized: "Connect your Pixiv account in Settings to see R-18 works.")
        case "r18_disabled_in_account": return String(localized: "Enable R-18 viewing in your account on pixiv.net.")
        case "all_ages_only": return String(localized: "Without a connected Pixiv account, only all-ages works are shown.")
        default: return nil
        }
    }

    static func sortNote(_ used: String?) -> String? {
        switch used {
        case "pixiv_popular": return String(localized: "Popular order from Pixiv Premium.")
        case "pixiv_popular_preview": return String(localized: "Popular preview first, then newest works.")
        case "users_tags": return String(localized: "Popular among recent works, based on bookmark tags, then newest works.")
        case "date_d_fallback": return String(localized: "No bookmark tier tags found. Showing newest works.")
        default: return nil
        }
    }

    static func errorText(_ error: APIError) -> String {
        if error.status == 403 {
            if error.code == "adult_off" { return String(localized: "Turn on Show adult plugins in Settings to see this work.") }
            if case let .server(_, _, message) = error, message.contains("adult_off") {
                return String(localized: "Turn on Show adult plugins in Settings to see this work.")
            }
        }
        return error.userMessage
    }

    static func saveText(_ result: PixivSaveResult) -> String {
        let duplicates = result.saved.filter(\.duplicate).count
        let saved = result.saved.count - duplicates
        return String(localized: "Saved: \(saved). Duplicates: \(duplicates). Failed: \(result.failed.count).")
    }

    static func createdDate(_ text: String) -> Date? {
        let formatter = ISO8601DateFormatter()
        if let date = formatter.date(from: text) { return date }
        formatter.formatOptions.insert(.withFractionalSeconds)
        return formatter.date(from: text)
    }
}

/// Paged feeds retain their place across tabs; answers are scoped to the choices that requested them.
@Observable @MainActor
final class PixivBrowseStore {
    enum Tab: String, CaseIterable { case ranking, search, bookmarks }
    struct Feed {
        var items: [PixivItem] = []
        var page = 0
        var hasMore = false
        var started = false
        var loading = false
        var error: String?
        var notice: String?
        var sort: PixivListing.Sort?
        var generation = 0
    }

    var tab: Tab = .ranking
    var content: PixivSettings.Content = .safe
    var rankingMode = "day"
    var searchText = ""
    var query = ""
    var order = "date_d"
    var type = "all"
    var connection: PixivConnection?
    var statusError: String?
    var actionError: String?
    var message: String?
    private(set) var saving = false
    private(set) var bookmarking: Set<String> = []
    private var feeds: [String: Feed] = [:]
    private var bookmarkChanges: [String: Bool] = [:]
    @ObservationIgnored private let api: () -> PixivAPI?
    @ObservationIgnored private let adultShown: () -> Bool
    @ObservationIgnored private let onSessionEnded: (APIError) -> Void
    @ObservationIgnored private var loadedDefaults = false

    init(api: @escaping () -> PixivAPI?, adultShown: @escaping () -> Bool = { AdultPlugins.isShown },
         onSessionEnded: @escaping (APIError) -> Void = { _ in }) {
        self.api = api; self.adultShown = adultShown; self.onSessionEnded = onSessionEnded
    }

    var modes: [String] { PixivBrowseChoices.modes(content: content, adult: adultShown()) }
    var key: String { [tab.rawValue, content.rawValue, rankingMode, query, order, type, adultShown() ? "1" : "0"].joined(separator: "\u{1F}") }
    var feed: Feed { feeds[key] ?? Feed() }

    func loadStatus() async {
        guard let api = api() else { return }
        let previousContent = content
        let previousOrder = order
        do {
            let status = try await api.status()
            guard !Task.isCancelled else { return }
            connection = status.connection; statusError = nil
            if !loadedDefaults {
                loadedDefaults = true
                if content == previousContent { content = adultShown() ? status.settings.defaultContent : .safe }
                if order == previousOrder { order = status.settings.defaultSort.rawValue }
            }
            normalizeContent()
        } catch { if !Task.isCancelled { statusError = handle(error) } }
    }

    func normalizeContent() {
        if !adultShown() { content = .safe }
        if !modes.contains(rankingMode) { rankingMode = modes[0] }
    }

    func searchFor(_ text: String) {
        searchText = text; query = text.trimmingCharacters(in: .whitespacesAndNewlines); tab = .search
    }

    func start() async {
        normalizeContent()
        guard !feed.started else { return }
        await reload()
    }

    func reload() async {
        normalizeContent()
        let k = key
        let generation = (feeds[k]?.generation ?? 0) + 1
        feeds[k] = Feed(generation: generation)
        await loadMore()
    }

    func loadMore() async {
        let k = key
        guard let api = api(), !feed.loading, !feed.started || feed.hasMore || feed.error != nil else { return }
        if tab == .search && query.isEmpty { return }
        let generation = feed.generation
        let page = feed.page + 1
        let requestedTab = tab
        feeds[k, default: Feed()].loading = true
        feeds[k]?.error = nil
        defer { if feeds[k]?.generation == generation { feeds[k]?.loading = false } }
        do {
            let listing: PixivListing
            switch requestedTab {
            case .ranking: listing = try await api.ranking(mode: rankingMode, page: page)
            case .search: listing = try await api.search(q: query, order: order, type: type, mode: content.rawValue, page: page)
            case .bookmarks: listing = try await api.bookmarks(content: content)
            }
            guard !Task.isCancelled, feeds[k]?.generation == generation else { return }
            var next = feeds[k] ?? Feed()
            var seen = Set(next.items.map(\.id))
            for var item in listing.items where seen.insert(item.id).inserted {
                if let changed = bookmarkChanges[item.id] { item.bookmarked = changed }
                next.items.append(item)
            }
            next.page = listing.page; next.hasMore = requestedTab != .bookmarks && listing.hasMore
            next.started = true; next.notice = listing.notice; next.sort = listing.sort
            feeds[k] = next
        } catch {
            guard !Task.isCancelled, feeds[k]?.generation == generation else { return }
            feeds[k]?.error = handle(error)
        }
    }

    func isBookmarked(_ id: String, fallback: Bool) -> Bool { bookmarkChanges[id] ?? fallback }

    func toggleBookmark(id: String, fallback: Bool) async {
        guard let api = api(), !bookmarking.contains(id) else { return }
        let old = isBookmarked(id, fallback: fallback)
        bookmarking.insert(id); patchBookmark(id, value: !old); actionError = nil
        defer { bookmarking.remove(id) }
        do {
            if old { try await api.removeBookmark(id: id) } else { try await api.addBookmark(id: id) }
            // Refresh bookmarked lists on their next visit, so removed and newly added works agree.
            for k in Array(feeds.keys) where k.hasPrefix("bookmarks\u{1F}") { feeds[k]?.started = false }
            if tab == .bookmarks { await reload() }
        } catch { patchBookmark(id, value: old); actionError = handle(error) }
    }

    private func patchBookmark(_ id: String, value: Bool) {
        bookmarkChanges[id] = value
        for k in Array(feeds.keys) {
            guard let index = feeds[k]?.items.firstIndex(where: { $0.id == id }) else { continue }
            feeds[k]?.items[index].bookmarked = value
        }
    }

    func save(id: String, pages: [Int]? = nil) async {
        guard let api = api(), !saving else { return }
        saving = true; actionError = nil
        defer { saving = false }
        do { message = PixivBrowseChoices.saveText(try await api.save(id: id, pages: pages)) }
        catch { actionError = handle(error) }
    }

    func copyImage(_ url: URL?) async {
        guard let url, let image = await SignedImageLoader.shared.image(url) else {
            actionError = String(localized: "Couldn't load the image."); return
        }
        UIPasteboard.general.image = image
        message = String(localized: "Image copied")
    }

    func copyLink(_ id: String) {
        UIPasteboard.general.url = PixivBrowseQuery.publicURL(id)
        message = String(localized: "Link copied")
    }

    func handle(_ error: Error) -> String {
        let wrapped = APIError.wrap(error)
        if wrapped.endsSession { onSessionEnded(wrapped) }
        return PixivBrowseChoices.errorText(wrapped)
    }
}
