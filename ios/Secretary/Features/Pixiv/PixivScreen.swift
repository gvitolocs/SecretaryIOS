import SwiftUI

/// Ranking, search and the owner's bookmarks, pushed from Plugins.
struct PixivScreen: View {
    @Environment(AppModel.self) private var app

    var body: some View { PixivBrowseContent(app: app) }
}

private struct PixivBrowseContent: View {
    let app: AppModel
    @State private var store: PixivBrowseStore
    @State private var selectedID: String?
    @AppStorage(AdultPlugins.key) private var adultShown = true
    /// `-SecretaryPixivOpen first` opened its work already (screenshots).
    @State private var openedFirst = false

    init(app: AppModel) {
        self.app = app
        _store = State(initialValue: PixivBrowseStore(api: { app.pluginClient.map { PixivAPI(client: $0) } },
                                                    onSessionEnded: { app.handle($0) }))
    }

    var body: some View {
        @Bindable var store = store
        ScrollView {
            LazyVStack(alignment: .leading, spacing: LucinaTokens.Space.md) {
                Picker("Pixiv sections", selection: $store.tab) {
                    ForEach(PixivBrowseStore.Tab.allCases, id: \.self) { tab in
                        Text(verbatim: PixivBrowseChoices.label(tab.rawValue)).tag(tab)
                    }
                }
                .pickerStyle(.segmented)
                .accessibilityIdentifier("pixiv-tabs")
                filters
                if let connection = store.connection, connection.state != .connected {
                    Card {
                        Text("Connect your Pixiv account in Settings")
                        Button("Settings") { app.showSettings = true }
                            .accessibilityIdentifier("pixiv-connect-settings")
                    }
                }
                if let error = store.statusError {
                    Text(verbatim: error).font(.footnote).foregroundStyle(Theme.textSecondary)
                    Button("Try again") { Task { await store.loadStatus() } }
                }
                if let notice = PixivBrowseChoices.notice(store.feed.notice) {
                    Text(verbatim: notice).font(.callout).foregroundStyle(Theme.textSecondary)
                    if store.feed.notice == "r18_needs_session" {
                        Button("Settings") { app.showSettings = true }
                    }
                }
                if let note = PixivBrowseChoices.sortNote(store.feed.sort?.used) {
                    Text(verbatim: note).font(.footnote).foregroundStyle(Theme.textSecondary)
                }
                grid
                footer
            }
            .padding(LucinaTokens.Space.gutter)
        }
        .lucinaScreen("Pixiv")
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button("Settings", systemImage: "gearshape") { app.showSettings = true }
                    .accessibilityIdentifier("pixiv-settings")
            }
        }
        .refreshable { await store.loadStatus(); await store.reload() }
        .task { await store.loadStatus() }
        .task(id: store.key) { await store.start() }
        .onChange(of: adultShown) { _, _ in store.normalizeContent() }
        .onChange(of: store.content) { _, _ in store.normalizeContent() }
        .navigationDestination(item: $selectedID) { id in
            PixivReaderView(id: id, store: store, client: app.pluginClient) { text in
                selectedID = nil
                store.searchFor(text)
            }
        }
        .modifier(PixivBrowseFeedback(store: store))
        // `/pixiv/<id>` (a link or Lucina's route) opens that work over the list.
        .onChange(of: app.openPixivWork, initial: true) { _, id in
            guard let id else { return }
            app.openPixivWork = nil
            selectedID = id
        }
        .onChange(of: store.feed.items.first?.id) { _, id in
            guard let id, PluginScreenshotHooks.opensFirstPixivWork, !openedFirst else { return }
            openedFirst = true
            selectedID = id
        }
    }

    private var filters: some View {
        @Bindable var store = store
        return VStack(alignment: .leading, spacing: LucinaTokens.Space.sm) {
            Picker("Content", selection: $store.content) {
                Text("All ages").tag(PixivSettings.Content.safe)
                if adultShown {
                    Text("R-18").tag(PixivSettings.Content.r18)
                    Text("All").tag(PixivSettings.Content.all)
                }
            }
            .pickerStyle(.menu)
            .accessibilityIdentifier("pixiv-content")
            if store.tab == .ranking {
                Picker("Ranking mode", selection: $store.rankingMode) {
                    ForEach(store.modes, id: \.self) { mode in
                        Text(verbatim: PixivBrowseChoices.label(mode)).tag(mode)
                    }
                }
                .pickerStyle(.menu)
                .accessibilityIdentifier("pixiv-ranking-mode")
            }
            if store.tab == .search {
                HStack {
                    TextField("Search works or tags", text: $store.searchText)
                        .textFieldStyle(.roundedBorder)
                        .submitLabel(.search)
                        .onSubmit { store.searchFor(store.searchText) }
                        .accessibilityIdentifier("pixiv-search-field")
                    Button("Search") { store.searchFor(store.searchText) }
                        .accessibilityIdentifier("pixiv-search-submit")
                }
                HStack {
                    Picker("Order", selection: $store.order) {
                        ForEach(["date_d", "date", "popular"], id: \.self) { value in
                            Text(verbatim: PixivBrowseChoices.label(value)).tag(value)
                        }
                    }
                    .accessibilityIdentifier("pixiv-search-order")
                    Picker("Type", selection: $store.type) {
                        ForEach(["all", "illust", "manga", "ugoira"], id: \.self) { value in
                            Text(verbatim: PixivBrowseChoices.label(value)).tag(value)
                        }
                    }
                    .accessibilityIdentifier("pixiv-search-type")
                }
                .pickerStyle(.menu)
            }
        }
    }

    private var grid: some View {
        LazyVGrid(columns: [GridItem(.flexible()), GridItem(.flexible())], spacing: LucinaTokens.Space.md) {
            ForEach(store.feed.items) { item in
                PixivBrowseTile(item: item, client: app.pluginClient, store: store,
                                open: { selectedID = item.id }, search: { store.searchFor($0) })
            }
        }
    }

    @ViewBuilder private var footer: some View {
        if store.feed.loading { ProgressView().frame(maxWidth: .infinity) }
        if let error = store.feed.error {
            Text(verbatim: error).foregroundStyle(Theme.negative)
            Button("Try again") { Task { await store.loadMore() } }
                .accessibilityIdentifier("pixiv-retry")
        } else if store.feed.hasMore {
            Button("Load more") { Task { await store.loadMore() } }
                .accessibilityIdentifier("pixiv-load-more")
                .task(id: store.feed.page) { await store.loadMore() }
        } else if store.feed.started && !store.feed.loading && store.feed.items.isEmpty {
            ContentUnavailableView("No works found", systemImage: "photo.on.rectangle")
        } else if store.feed.started && !store.feed.loading && !store.feed.items.isEmpty {
            Text("That's all for now.").font(.footnote).foregroundStyle(Theme.textSecondary)
        } else if store.tab == .search && store.query.isEmpty {
            ContentUnavailableView("Search Pixiv", systemImage: "magnifyingglass", description: Text("Search for a title, author or tag."))
        }
    }
}

private struct PixivBrowseTile: View {
    let item: PixivItem
    let client: APIClient?
    let store: PixivBrowseStore
    let open: () -> Void
    let search: (String) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Button(action: open) {
                // The picture is an overlay of a fixed box: a wide work must not widen its grid column.
                Color.clear.frame(maxWidth: .infinity).frame(height: 220)
                    .overlay { SignedImage(url: client?.absolute(item.thumbUrl)) }
                    .clipShape(RoundedRectangle(cornerRadius: 8))
                    .contentShape(Rectangle())
                    .overlay(alignment: .bottomLeading) {
                        HStack(spacing: 5) {
                            if item.pages > 1 { Text("\(item.pages)") }
                            if item.r18 { Text("R-18") }
                            if let tier = item.popularTier { Text(verbatim: "♥ \(tier)users入り") }
                        }
                        .font(.caption2.bold())
                        .padding(5).background(.ultraThinMaterial, in: RoundedRectangle(cornerRadius: 5))
                        .padding(6)
                    }
            }
            .buttonStyle(.plain)
            .accessibilityLabel(Text("Open \(item.title) by \(item.author)"))
            .accessibilityIdentifier("pixiv-work-\(item.id)")
            .overlay(alignment: .topTrailing) {
                Button {
                    Task { await store.toggleBookmark(id: item.id, fallback: item.bookmarked) }
                } label: {
                    Image(systemName: store.isBookmarked(item.id, fallback: item.bookmarked) ? "heart.fill" : "heart")
                        .padding(10).background(.ultraThinMaterial, in: Circle())
                }
                .buttonStyle(.plain).padding(6)
                .disabled(store.bookmarking.contains(item.id))
                .accessibilityLabel(store.isBookmarked(item.id, fallback: item.bookmarked) ? Text("Unbookmark") : Text("Bookmark"))
                .accessibilityIdentifier("pixiv-bookmark-\(item.id)")
            }
            Text(verbatim: item.title).font(LucinaTokens.Fonts.subheadline).lineLimit(2)
            Text(verbatim: item.author).font(LucinaTokens.Fonts.caption).foregroundStyle(Theme.textSecondary).lineLimit(1)
        }
        .contextMenu {
            PixivBrowseMenu(id: item.id, author: item.author, bookmarked: item.bookmarked,
                            imageURL: client?.absolute(item.thumbUrl), store: store, open: open, search: search)
        }
    }
}

/// The same work actions are available on feed tiles and reader pages.
struct PixivBrowseMenu: View {
    let id: String
    let author: String
    let bookmarked: Bool
    let imageURL: URL?
    let store: PixivBrowseStore
    var page: Int? = nil
    let open: () -> Void
    let search: (String) -> Void

    var body: some View {
        Button("Open", systemImage: "arrow.up.right.square", action: open)
        Button("Copy image", systemImage: "photo.on.rectangle") { Task { await store.copyImage(imageURL) } }
        Button("Copy link", systemImage: "link") { store.copyLink(id) }
        Button {
            Task { await store.toggleBookmark(id: id, fallback: bookmarked) }
        } label: {
            Label(store.isBookmarked(id, fallback: bookmarked) ? String(localized: "Unbookmark") : String(localized: "Bookmark"), systemImage: "heart")
        }
        .disabled(store.bookmarking.contains(id))
        Button("Save to Lucina", systemImage: "square.and.arrow.down") { Task { await store.save(id: id) } }
            .disabled(store.saving)
        if let page {
            Button("Save this page") { Task { await store.save(id: id, pages: [page]) } }.disabled(store.saving)
        }
        ShareLink(item: PixivBrowseQuery.publicURL(id)) { Label("Share", systemImage: "square.and.arrow.up") }
        if !author.isEmpty { Button("More by \(author)") { search(author) } }
    }
}

struct PixivBrowseFeedback: ViewModifier {
    let store: PixivBrowseStore

    func body(content: Content) -> some View {
        content.safeAreaInset(edge: .bottom) {
            VStack(spacing: 8) {
                if let error = store.actionError {
                    HStack {
                        Text(verbatim: error).foregroundStyle(Theme.negative)
                        Button("Dismiss", systemImage: "xmark") { store.actionError = nil }.labelStyle(.iconOnly)
                    }
                    .padding().background(.regularMaterial, in: RoundedRectangle(cornerRadius: 12))
                }
                if let message = store.message {
                    Text(verbatim: message).padding().background(.regularMaterial, in: Capsule())
                        .accessibilityIdentifier("pixiv-confirmation")
                        .task(id: message) {
                            try? await Task.sleep(for: .seconds(4))
                            if !Task.isCancelled && store.message == message { store.message = nil }
                        }
                }
            }
            .font(.callout).padding(.horizontal)
        }
    }
}
