import Foundation
import Observation

enum IgTab: String, CaseIterable, Identifiable, Sendable {
    case stories, highlights, posts, reels, archive
    var id: String { rawValue }
    var title: String {
        switch self {
        case .stories: return String(localized: "Stories")
        case .highlights: return String(localized: "Highlights")
        case .posts: return String(localized: "Posts")
        case .reels: return String(localized: "Reels")
        case .archive: return String(localized: "Archive")
        }
    }
}

/// Search and display helpers have explicit locale and time zone inputs for reproducible tests.
enum InstagramText {
    enum Target: Equatable { case profile(String), resolve(String, username: String?) }
    static func parse(_ raw: String) -> Target? {
        let text = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return nil }
        let reserved: Set<String> = ["p", "reel", "reels", "tv", "stories", "explore", "accounts", "direct"]
        if text.contains("/") || text.lowercased().hasPrefix("instagram.com") || text.lowercased().hasPrefix("www.instagram.com") {
            guard let url = URL(string: text.contains("://") ? text : "https://" + text),
                  ["http", "https"].contains(url.scheme?.lowercased() ?? ""),
                  let host = url.host?.lowercased(),
                  host == "instagram.com" || host.hasSuffix(".instagram.com") || host == "instagr.am",
                  url.user == nil, url.password == nil else { return nil }
            let parts = url.path.split(separator: "/").map(String.init)
            if let index = parts.firstIndex(where: { ["p", "reel", "reels", "tv"].contains($0) }), parts.count > index + 1 {
                let name = index > 0 && validUsername(parts[0]) ? parts[0].lowercased() : nil
                return .resolve(url.absoluteString, username: name)
            }
            guard let name = parts.first?.lowercased(), validUsername(name), !reserved.contains(name) else { return nil }
            return .profile(name)
        }
        let name = text.hasPrefix("@") ? String(text.dropFirst()) : text
        return validUsername(name) ? .profile(name.lowercased()) : nil
    }
    static func validUsername(_ name: String) -> Bool {
        name.range(of: "^[A-Za-z0-9._]{1,30}$", options: .regularExpression) != nil
    }
    static func profileLink(_ username: String) -> URL? {
        guard validUsername(username) else { return nil }
        return URL(string: "https://www.instagram.com/\(username)/")
    }
    static func compact(_ count: Int, locale: Locale = .current) -> String {
        let n = Double(count)
        let magnitude = abs(n)
        let divisor: Double
        let suffix: String
        if magnitude >= 1_000_000_000 { divisor = 1_000_000_000; suffix = "B" }
        else if magnitude >= 1_000_000 { divisor = 1_000_000; suffix = "M" }
        else if magnitude >= 10_000 { divisor = 1_000; suffix = "K" }
        else { return String(count) }
        let formatter = NumberFormatter()
        formatter.locale = locale
        formatter.numberStyle = .decimal
        formatter.usesGroupingSeparator = false
        formatter.maximumFractionDigits = magnitude / divisor >= 100 ? 0 : 1
        return (formatter.string(from: NSNumber(value: n / divisor)) ?? String(count)) + suffix
    }
    static func monthChips(_ items: [IgItem], timeZone: TimeZone = .current, locale: Locale = .current) -> [String?] {
        let formatter = DateFormatter()
        formatter.locale = locale
        formatter.calendar = Calendar(identifier: .gregorian)
        formatter.timeZone = timeZone
        formatter.setLocalizedDateFormatFromTemplate("MMM yyyy")
        var last = ""
        return items.map { item in
            guard let seconds = item.takenAt, seconds.isFinite else { return nil }
            let month = formatter.string(from: Date(timeIntervalSince1970: seconds))
            guard month != last else { return nil }
            last = month
            return month
        }
    }
    static func saveItems(_ item: IgItem, username: String, child: IgChild? = nil) -> [IgSaveItem] {
        let children = child.map { [$0] } ?? (item.children.isEmpty ? [IgChild(id: item.id, kind: item.kind == "video" ? "video" : "image", url: item.url, thumbUrl: item.thumbUrl)] : item.children)
        return children.map { IgSaveItem(url: $0.url, kind: $0.kind == "video" ? "video" : "image", username: username, takenAt: item.takenAt, caption: item.caption) }
    }
    static func error(_ error: APIError) -> String {
        if case let .rateLimited(_, seconds, _) = error {
            return String(localized: "Instagram is busy, try again in \(seconds ?? 60) s")
        }
        if error.status == 404 { return String(localized: "No account with that name.") }
        return error.userMessage
    }
}

@Observable @MainActor
final class InstagramStore {
    private(set) var recent: [IgRecent] = []
    private(set) var watched: [IgWatch] = []
    private(set) var badges = IgBadges()
    private(set) var settings = IgWatchSettings()
    private(set) var bundle: IgProfileBundle?
    private(set) var pages: [String: IgPage] = [:]
    private(set) var loading: Set<String> = []
    private(set) var errors: [String: String] = [:]
    private(set) var changingWatch = false
    private(set) var saving = false
    var notice: String?
    @ObservationIgnored private let api: () -> InstagramAPI?
    @ObservationIgnored private let onSessionEnded: (APIError) -> Void
    @ObservationIgnored private var generation = 0
    @ObservationIgnored private var hasLoadedWatch = false

    init(api: @escaping () -> InstagramAPI?, onSessionEnded: @escaping (APIError) -> Void = { _ in }) {
        self.api = api; self.onSessionEnded = onSessionEnded
    }
    func absolute(_ path: String?) -> URL? { api()?.client.absolute(path) }
    var client: APIClient? { api()?.client }
    func entry(_ username: String) -> IgWatch? { watched.first { $0.username == username } }
    private func failed(_ error: Error, key: String) {
        guard !(error is CancellationError), !Task.isCancelled else { return }
        let wrapped = APIError.wrap(error)
        if wrapped.endsSession { onSessionEnded(wrapped) }
        errors[key] = InstagramText.error(wrapped)
    }
    func loadHome() async {
        guard let api = api(), !loading.contains("home") else { return }
        loading.insert("home")
        defer { loading.remove("home") }
        do { recent = try await api.recent(); errors["home"] = nil } catch { failed(error, key: "home") }
        await refreshWatching()
    }
    func refreshWatching() async {
        guard let api = api() else { return }
        do { watched = try await api.watchList(); hasLoadedWatch = true; errors["watch"] = nil } catch { failed(error, key: "watch") }
        do { badges = try await api.badges(); errors["badges"] = nil } catch { failed(error, key: "badges") }
        do { settings = try await api.watchSettings(); errors["settings"] = nil } catch { failed(error, key: "settings") }
    }
    func open(_ username: String) async {
        guard let api = api() else { return }
        generation += 1
        let mine = generation
        if bundle?.profile.username != username { bundle = nil; pages = [:]; errors = [:] }
        loading.insert("profile")
        defer { if mine == generation { loading.remove("profile") } }
        do {
            let result = try await api.profile(username)
            guard mine == generation, !Task.isCancelled else { return }
            bundle = result; pages[IgTab.posts.rawValue] = result.posts; errors["profile"] = nil
            let profile = result.profile
            recent.removeAll { $0.username == profile.username }
            recent.insert(IgRecent(username: profile.username, fullName: profile.fullName, isVerified: profile.isVerified,
                                   picUrl: profile.picUrl, openedAt: Date().timeIntervalSince1970), at: 0)
            recent = Array(recent.prefix(30))
            if !hasLoadedWatch {
                watched = try await api.watchList()
                hasLoadedWatch = true
            }
            guard mine == generation, !Task.isCancelled else { return }
            if entry(username) != nil { badges = try await api.opened(username) }
            if result.refreshing { await settle(username, generation: mine) }
        } catch { if mine == generation { failed(error, key: "profile") } }
    }
    /// The server answered from its store and refreshes behind it: what it finds comes in as it lands, with nothing on
    /// screen saying so. A failure here leaves what is shown.
    private func settle(_ username: String, generation mine: Int) async {
        guard let api = api() else { return }
        for _ in 0..<30 {
            try? await Task.sleep(for: .seconds(3))
            guard mine == generation, !Task.isCancelled, let fresh = try? await api.profile(username),
                  mine == generation, !Task.isCancelled else { return }
            if fresh.posts.items.map(\.id) != bundle?.posts.items.map(\.id) { pages[IgTab.posts.rawValue] = fresh.posts }
            bundle = fresh
            guard fresh.refreshing else { break }
        }
        for key in pages.keys where key != IgTab.posts.rawValue {
            if let tab = IgTab(rawValue: key) { await load(tab, username: username, refresh: true) }
        }
    }
    /// A new link for an expired one; nil when there is none.
    func resigned(_ path: String) async -> String? {
        guard let api = api(), path.hasPrefix("/v1/instagram/") else { return nil }
        return (try? await api.resign(path)) ?? nil
    }
    func load(_ tab: IgTab, username: String, more: Bool = false, refresh: Bool = false, highlight: IgHighlight? = nil) async {
        guard let api = api() else { return }
        let key = highlight?.id ?? tab.rawValue
        guard !loading.contains(key) else { return }
        if tab == .highlights, highlight == nil {
            guard refresh || bundle?.highlights.isEmpty != false else { return }
            let mine = generation
            loading.insert(key); defer { loading.remove(key) }
            do {
                let tray = try await api.highlights(username)
                guard mine == generation, !Task.isCancelled else { return }
                bundle?.highlights = tray; errors[key] = nil
            } catch { if mine == generation { failed(error, key: key) } }
            return
        }
        if more && (pages[key]?.exhausted == true || pages[key]?.nextCursor == nil) { return }
        if !more && !refresh && pages[key] != nil { return }
        let mine = generation
        let cursor = more ? pages[key]?.nextCursor : nil
        loading.insert(key); defer { loading.remove(key) }
        do {
            var result: IgPage
            if let highlight { result = try await api.highlightItems(highlight.id, cursor: cursor) }
            else { result = try await api.page(tab, username: username, cursor: cursor) }
            guard mine == generation, !Task.isCancelled else { return }
            if more {
                let old = pages[key]?.items ?? []
                let known = Set(old.map(\.id))
                let fresh = result.items.filter { !known.contains($0.id) }
                if fresh.isEmpty || result.nextCursor == cursor { result.exhausted = true }
                result.items = old + fresh
            }
            if result.exhausted { result.nextCursor = nil }
            pages[key] = result; errors[key] = nil
        } catch { if mine == generation { failed(error, key: key) } }
    }
    func resolve(_ url: String) async -> [IgItem]? {
        guard let api = api(), !loading.contains("resolve") else { return nil }
        loading.insert("resolve"); defer { loading.remove("resolve") }
        do { let page = try await api.resolve(url); errors["resolve"] = nil; return page.items }
        catch { failed(error, key: "resolve"); return nil }
    }
    func changeWatch(_ username: String, autoSave: Bool? = nil) async {
        guard let api = api(), !changingWatch else { return }
        changingWatch = true; defer { changingWatch = false }
        do {
            if entry(username) != nil && autoSave == nil {
                try await api.unwatch(username); watched.removeAll { $0.username == username }
            } else {
                let updated = try await api.watch(username, autoSave: autoSave)
                watched.removeAll { $0.username == username }; watched.append(updated)
            }
            badges = try await api.badges(); errors["watch"] = nil
        } catch { failed(error, key: "watch") }
    }
    func setInterval(_ hours: Int) async {
        guard let api = api(), !loading.contains("settings") else { return }
        loading.insert("settings"); defer { loading.remove("settings") }
        do { settings = try await api.setWatchSettings(hours); watched = try await api.watchList(); errors["settings"] = nil }
        catch { failed(error, key: "settings") }
    }
    func save(_ item: IgItem, username: String, child: IgChild? = nil) async {
        guard let api = api(), !saving else { return }
        saving = true; defer { saving = false }
        do {
            let result = try await api.save(InstagramText.saveItems(item, username: username, child: child))
            if let failure = result.failed.first {
                notice = failure.error == "expired" ? String(localized: "Couldn't save: the link expired.") : String(localized: "Couldn't save to the Gallery. Try again.")
            } else if !result.saved.isEmpty {
                notice = result.saved.allSatisfy(\.duplicate) ? String(localized: "Already in the Gallery") : String(localized: "Saved")
                pages[IgTab.archive.rawValue] = nil
            } else { notice = String(localized: "Couldn't save to the Gallery. Try again.") }
        } catch {
            failed(error, key: "save"); notice = errors["save"]
        }
    }
}
