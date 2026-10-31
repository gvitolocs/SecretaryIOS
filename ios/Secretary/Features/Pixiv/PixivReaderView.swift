import SwiftUI

/// Work metadata and vertically arranged pages; a tap opens the page at Fit.
struct PixivReaderView: View {
    let id: String
    let store: PixivBrowseStore
    let client: APIClient?
    let onSearch: (String) -> Void
    @Environment(\.openURL) private var openURL
    @State private var work: PixivIllust?
    @State private var loading = false
    @State private var error: String?
    @State private var zoomPage: PixivPage?

    var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: LucinaTokens.Space.lg) {
                if let work {
                    metadata(work)
                    actions(work)
                    if work.ugoira || work.kind == .ugoira {
                        SignedImage(url: client?.absolute(work.coverUrl), contentMode: .fit)
                            .contextMenu {
                                PixivBrowseMenu(id: id, author: work.author, bookmarked: work.bookmarked,
                                                imageURL: client?.absolute(work.coverUrl), store: store,
                                                open: { openURL(PixivBrowseQuery.publicURL(id)) }, search: onSearch)
                            }
                        Text("Animations play on the web.").foregroundStyle(Theme.textSecondary)
                    } else {
                        ForEach(Array(work.pages.enumerated()), id: \.offset) { offset, page in
                            VStack(alignment: .leading, spacing: 8) {
                                Button { zoomPage = page } label: {
                                    SignedImage(url: client?.absolute(page.url.isEmpty ? page.thumbUrl : page.url), contentMode: .fit)
                                        .frame(maxWidth: .infinity)
                                        .frame(minHeight: 120)
                                }
                                .buttonStyle(.plain)
                                .accessibilityLabel(Text("Open page \(offset + 1)"))
                                .accessibilityIdentifier("pixiv-page-\(offset)")
                                Text("Page \(offset + 1) of \(work.pageCount)")
                                    .font(.caption).foregroundStyle(Theme.textSecondary)
                            }
                            .contextMenu {
                                PixivBrowseMenu(id: id, author: work.author, bookmarked: work.bookmarked,
                                                imageURL: client?.absolute(page.url.isEmpty ? page.thumbUrl : page.url),
                                                store: store, page: page.index, open: { zoomPage = page }, search: onSearch)
                            }
                        }
                        if work.pages.isEmpty {
                            SignedImage(url: client?.absolute(work.coverUrl), contentMode: .fit)
                        }
                    }
                }
                if loading { ProgressView().frame(maxWidth: .infinity) }
                if let error {
                    Text(verbatim: error).foregroundStyle(Theme.negative)
                    Button("Try again") { Task { await load() } }
                }
            }
            .padding(LucinaTokens.Space.gutter)
        }
        .lucinaScreen("Pixiv")
        .task(id: id) { await load() }
        .refreshable { await load() }
        .fullScreenCover(item: $zoomPage) { page in
            PixivReaderZoom(page: page, client: client)
        }
        .modifier(PixivBrowseFeedback(store: store))
    }

    private func metadata(_ work: PixivIllust) -> some View {
        VStack(alignment: .leading, spacing: LucinaTokens.Space.md) {
            Text(verbatim: work.title).font(LucinaTokens.Fonts.title2).textSelection(.enabled)
            Button { onSearch(work.author) } label: { Text(verbatim: work.author) }
                .accessibilityIdentifier("pixiv-author")
            if let date = PixivBrowseChoices.createdDate(work.created) {
                Text(date, style: .date).font(.subheadline).foregroundStyle(Theme.textSecondary)
            }
            if work.r18 { Text("R-18").font(.caption.bold()) }
            ScrollView(.horizontal, showsIndicators: false) {
                HStack {
                    ForEach(Array(work.tags.enumerated()), id: \.offset) { _, tag in
                        Button { onSearch(tag.tag) } label: {
                            Text(verbatim: tag.en.isEmpty ? tag.tag : "\(tag.tag) · \(tag.en)")
                                .font(.subheadline).padding(8).background(Theme.field, in: Capsule())
                        }
                        .accessibilityIdentifier("pixiv-tag-\(tag.tag)")
                    }
                }
            }
            StatGrid {
                Card { Text("Views"); Text(work.stats.views, format: .number).font(.headline) }
                Card { Text("Likes"); Text(work.stats.likes, format: .number).font(.headline) }
                Card { Text("Bookmarks"); Text(work.stats.bookmarks, format: .number).font(.headline) }
                Card { Text("Comments"); Text(work.stats.comments, format: .number).font(.headline) }
            }
            if !work.description.isEmpty { Text(verbatim: work.description).textSelection(.enabled) }
        }
    }

    private func actions(_ work: PixivIllust) -> some View {
        VStack(alignment: .leading, spacing: LucinaTokens.Space.sm) {
            HStack {
                Button {
                    Task { await store.toggleBookmark(id: id, fallback: work.bookmarked) }
                } label: {
                    Label(store.isBookmarked(id, fallback: work.bookmarked) ? String(localized: "Unbookmark") : String(localized: "Bookmark"), systemImage: "heart")
                }
                .disabled(store.bookmarking.contains(id))
                .accessibilityIdentifier("pixiv-reader-bookmark")
                Spacer()
                ShareLink(item: PixivBrowseQuery.publicURL(id)) { Label("Share", systemImage: "square.and.arrow.up") }
            }
            Button("Save to Lucina", systemImage: "square.and.arrow.down") { Task { await store.save(id: id) } }
                .disabled(store.saving).accessibilityIdentifier("pixiv-reader-save")
            if store.saving { ProgressView("Saving…") }
            Button("Open on pixiv", systemImage: "safari") { openURL(PixivBrowseQuery.publicURL(id)) }
                .accessibilityIdentifier("pixiv-reader-open-web")
        }
        .buttonStyle(.bordered)
    }

    private func load() async {
        guard let client, !loading else { return }
        loading = true; error = nil
        defer { loading = false }
        do {
            let answer = try await PixivAPI(client: client).illust(id: id)
            if !Task.isCancelled { work = answer }
        } catch { if !Task.isCancelled { self.error = store.handle(error) } }
    }
}
