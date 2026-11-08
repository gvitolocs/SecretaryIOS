import SwiftUI
import UIKit

struct NClientScreen: View {
    @Environment(AppModel.self) private var app
    @State private var store: NClientStore?
    @State private var loader = NClientImageLoader()
    var body: some View {
        Group {
            if let store { NClientBrowser(store: store, loader: loader) }
            else { ProgressView() }
        }
        .lucinaScreen("NClient")
        .modifier(NClientPrivacy())
        .task {
            guard store == nil, AdultPlugins.isShown else { return }
            let store = NClientStore(api: { app.pluginClient.map { NClientAPI(client: $0) } }, onSessionEnded: { app.handle($0) })
            self.store = store
            await store.load()
        }
    }
}

private struct NClientRoute: Hashable {
    let id: Int
    var read = false
}

private struct NClientBrowser: View {
    @Bindable var store: NClientStore
    let loader: NClientImageLoader
    @State private var route: NClientRoute?
    @State private var tags = false
    @State private var copied = false

    var body: some View {
        ScrollView {
            LazyVStack(spacing: LucinaTokens.Space.md) {
                if store.status?.enabled == true {
                    controls
                    if let error = store.error { errorView(error) }
                    if let error = store.detailError { errorView(error) }
                    if store.detailLoading { ProgressView() }
                    grid(store.items)
                    if store.loading { ProgressView() }
                    else if store.items.isEmpty, store.error == nil {
                        ContentUnavailableView("No galleries", systemImage: "books.vertical")
                    }
                    if !store.loading, !store.favoritesOnly, store.page < store.numPages, store.error == nil {
                        ProgressView().onAppear { Task { await store.loadMore() } }
                    }
                } else if store.loading { ProgressView() }
                else if let error = store.error { errorView(error) }
                else { ContentUnavailableView("NClient is unavailable", systemImage: "books.vertical") }
            }
            .padding()
        }
        .refreshable { await store.load() }
        .navigationDestination(item: $route) { target in
            NClientDetailScreen(id: target.id, readImmediately: target.read, store: store, loader: loader, filter: choose)
        }
        .sheet(isPresented: $tags) { NClientTagPicker(store: store).modifier(NClientPrivacy()) }
        .sensoryFeedback(.success, trigger: copied)
    }

    private var controls: some View {
        Card {
            TextField("Search galleries", text: $store.query)
                .textInputAutocapitalization(.never).autocorrectionDisabled()
                .submitLabel(.search).onSubmit { search() }
                .accessibilityIdentifier("nclient-search")
            HStack {
                Picker("Sort", selection: $store.sort) {
                    ForEach(NClientSort.allCases, id: \.self) { sort in Text(sort.label).tag(sort) }
                }
                .pickerStyle(.menu).accessibilityIdentifier("nclient-sort")
                Spacer()
                Button("Search", systemImage: "magnifyingglass") { search() }
                    .accessibilityIdentifier("nclient-search-submit")
            }
            HStack {
                Button("Tag filters", systemImage: "line.3.horizontal.decrease") { tags = true }
                    .accessibilityIdentifier("nclient-tags")
                Spacer()
                Button("Random", systemImage: "shuffle") {
                    Task { if let id = await store.open() { route = NClientRoute(id: id) } }
                }
                .disabled(store.detailLoading).accessibilityIdentifier("nclient-random")
            }
            Toggle("Favourites", isOn: $store.favoritesOnly)
                .accessibilityIdentifier("nclient-favorites")
            if !store.include.isEmpty || !store.exclude.isEmpty {
                ScrollView(.horizontal) {
                    HStack {
                        ForEach(store.include) { tag in chip(tag, excluded: false) }
                        ForEach(store.exclude) { tag in chip(tag, excluded: true) }
                    }
                }
            }
        }
        .onChange(of: store.favoritesOnly) { _, _ in Task { await store.reload() } }
        .onChange(of: store.sort) { _, _ in search() }
    }
    private func chip(_ tag: NClientTag, excluded: Bool) -> some View {
        Button {
            if excluded { store.exclude.removeAll { $0.id == tag.id } }
            else { store.include.removeAll { $0.id == tag.id } }
            search()
        } label: { Text(verbatim: "\(excluded ? "−" : "+")\(tag.name) ×").font(.caption).padding(8).background(Theme.field, in: Capsule()) }
        .accessibilityLabel(Text("Remove filter: \(tag.name)"))
    }
    private func copyImage(_ item: NClientSummary) {
        Task {
            guard let url = store.api()?.imageURL(item.thumbUrl), let image = await loader.image(url) else {
                store.error = String(localized: "Couldn't load image"); return
            }
            UIPasteboard.general.image = image; copied.toggle()
        }
    }
    private func search() { store.favoritesOnly = false; Task { await store.reload() } }
    private func choose(_ tag: NClientTag) {
        store.choose(tag, excluded: false)
        route = nil
        Task { await store.reload() }
    }
    private func errorView(_ message: String) -> some View {
        VStack {
            Text(verbatim: message).foregroundStyle(Theme.textSecondary)
            Button("Try again") { Task { await store.load() } }
        }
    }
    private func grid(_ items: [NClientSummary]) -> some View {
        LazyVGrid(columns: [GridItem(.adaptive(minimum: 140), spacing: 12)], spacing: 16) {
            ForEach(items) { item in
                Button { route = NClientRoute(id: item.id) } label: {
                    NClientTile(item: item, url: store.api()?.imageURL(item.thumbUrl), loader: loader)
                }
                .buttonStyle(.plain)
                .accessibilityIdentifier("nclient-gallery-\(item.id)")
                .contextMenu {
                    Button("Open", systemImage: "book") { route = NClientRoute(id: item.id) }
                    Button(item.favorite ? LocalizedStringKey("Unfavourite") : LocalizedStringKey("Favourite"), systemImage: "heart") {
                        Task { await store.toggleFavorite(item.id, current: item.favorite) }
                    }.disabled(store.favoriteBusy.contains(item.id))
                    Button("Copy image", systemImage: "doc.on.doc") { copyImage(item) }
                    Button("Copy link", systemImage: "link") { UIPasteboard.general.string = "#\(item.id)"; copied.toggle() }
                    ShareLink(item: "#\(item.id)") { Label("Share", systemImage: "square.and.arrow.up") }
                    Button("Read", systemImage: "book.pages") { route = NClientRoute(id: item.id, read: true) }
                }
            }
        }
    }
}

private extension NClientSort {
    var label: LocalizedStringKey {
        switch self {
        case .recent: return "Recent"
        case .popular: return "Popular"
        case .week: return "Popular this week"
        case .today: return "Popular today"
        case .month: return "Popular this month"
        }
    }
}

private struct NClientTile: View {
    let item: NClientSummary
    let url: URL?
    let loader: NClientImageLoader
    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            // The cover is an overlay of a fixed box: a wide cover must not widen its grid column.
            Color.clear.frame(maxWidth: .infinity).frame(height: 210)
                .overlay { NClientImage(url: url, loader: loader) }
                .clipShape(RoundedRectangle(cornerRadius: 12))
                .overlay(alignment: .topTrailing) {
                    if item.favorite { Image(systemName: "heart.fill").foregroundStyle(.pink).padding(8).accessibilityLabel(Text("Favourite")) }
                }
            Text(verbatim: item.title).font(.subheadline).lineLimit(2)
            HStack {
                Text("\(item.pages) pages")
                if let language = item.language { Text(verbatim: language) }
            }.font(.caption).foregroundStyle(Theme.textSecondary)
            if let progress = item.progress { ProgressView(value: progress.fraction(total: item.pages)) }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

private struct NClientTagPicker: View {
    let store: NClientStore
    @Environment(\.dismiss) private var dismiss
    @State private var query = ""
    @State private var tags: [NClientTag] = []
    @State private var error: String?
    @State private var loading = false
    @State private var retry = 0
    var body: some View {
        VStack {
            HStack {
                Text("Tag filters").font(.headline)
                Spacer()
                Button("Done") { dismiss() }
            }.padding()
            TextField("Find tags", text: $query).textFieldStyle(.roundedBorder).padding(.horizontal)
                .textInputAutocapitalization(.never).autocorrectionDisabled().accessibilityIdentifier("nclient-tag-search")
            if loading { ProgressView() }
            if let error {
                Text(verbatim: error)
                Button("Try again") { retry += 1 }
            }
            List(tags.filter { tag in !store.include.contains(where: { $0.id == tag.id }) && !store.exclude.contains(where: { $0.id == tag.id }) }) { tag in
                HStack {
                    Text(verbatim: tag.name)
                    Spacer()
                    Button("Include", systemImage: "plus") { choose(tag, excluded: false) }
                        .accessibilityIdentifier("nclient-tag-include-\(tag.id)")
                    Button("Exclude", systemImage: "minus") { choose(tag, excluded: true) }
                        .accessibilityIdentifier("nclient-tag-exclude-\(tag.id)")
                }.buttonStyle(.borderless)
            }.lucinaList()
        }
        .lucinaSheet()
        .task(id: "\(query)-\(retry)") {
            loading = true; error = nil
            do {
                try await Task.sleep(for: .milliseconds(250))
                guard let api = store.api() else { loading = false; return }
                let results = try await api.tags(query)
                guard !Task.isCancelled else { return }
                tags = results; loading = false
            } catch {
                guard !Task.isCancelled else { return }
                self.error = store.report(error); loading = false
            }
        }
    }
    private func choose(_ tag: NClientTag, excluded: Bool) {
        store.choose(tag, excluded: excluded)
        Task { await store.reload() }
    }
}

private struct NClientDetailScreen: View {
    let id: Int
    var readImmediately = false
    let store: NClientStore
    let loader: NClientImageLoader
    let filter: (NClientTag) -> Void
    @State private var reading: NClientReading?
    @State private var opened = false
    @State private var visiblePages = 24
    @State private var copied = false
    @State private var relatedRoute: NClientRoute?
    var body: some View {
        ScrollView {
            if let detail = store.details[id] {
                VStack(alignment: .leading, spacing: 16) {
                    NClientImage(url: store.api()?.imageURL(detail.coverUrl), loader: loader, mode: .fit)
                        .frame(height: 300).frame(maxWidth: .infinity)
                    Text(verbatim: detail.title).font(.title2)
                    if let titles = detail.titles {
                        ForEach(Array(Set([titles.pretty, titles.english, titles.japanese].filter { !$0.isEmpty && $0 != detail.title })).sorted(), id: \.self) { title in
                            Text(verbatim: title).font(.subheadline).foregroundStyle(Theme.textSecondary)
                        }
                    }
                    Text("\(detail.pages.count) pages")
                    if let language = detail.language { Text(verbatim: language).font(.caption) }
                    if detail.uploaded > 0 { Text(Date(timeIntervalSince1970: Double(detail.uploaded)), style: .date) }
                    Text("\(detail.favorites) favourites").font(.caption)
                    HStack {
                        Button(detail.progress == nil ? LocalizedStringKey("Read") : LocalizedStringKey("Resume"), systemImage: "book") { read(detail, page: detail.progress?.page ?? 0) }
                            .accessibilityIdentifier("nclient-read")
                        if detail.progress != nil { Button("Read from start") { read(detail, page: 0) } }
                    }.disabled(detail.pages.isEmpty)
                    Button(detail.favorite ? LocalizedStringKey("Unfavourite") : LocalizedStringKey("Favourite"), systemImage: detail.favorite ? "heart.fill" : "heart") {
                        Task { await store.toggleFavorite(id, current: detail.favorite) }
                    }.disabled(store.favoriteBusy.contains(id)).accessibilityIdentifier("nclient-detail-favorite")
                    NClientSaveControl(id: id, store: store)
                    if let error = store.error { Text(verbatim: error).foregroundStyle(Theme.textSecondary) }
                    tagGroups(detail.tags)
                    Text("Pages").font(.headline)
                    LazyVGrid(columns: [GridItem(.adaptive(minimum: 90))]) {
                        ForEach(Array(detail.pages.prefix(visiblePages).enumerated()), id: \.offset) { offset, page in
                            Button { read(detail, page: offset) } label: {
                                VStack {
                                    NClientImage(url: store.api()?.imageURL(page.thumbUrl), loader: loader).frame(height: 140).clipped()
                                    Text("Page \(offset + 1)").font(.caption)
                                }
                            }.buttonStyle(.plain).accessibilityIdentifier("nclient-page-\(offset)")
                        }
                    }
                    if visiblePages < detail.pages.count { Button("More pages") { visiblePages += 24 } }
                    if !detail.related.isEmpty {
                        Text("Related galleries").font(.headline)
                        LazyVGrid(columns: [GridItem(.adaptive(minimum: 140))]) {
                            ForEach(detail.related) { item in
                                Button { relatedRoute = NClientRoute(id: item.id) } label: {
                                    NClientTile(item: item, url: store.api()?.imageURL(item.thumbUrl), loader: loader)
                                }.buttonStyle(.plain)
                                .contextMenu {
                                    Button("Open") { relatedRoute = NClientRoute(id: item.id) }
                                    Button("Read") { relatedRoute = NClientRoute(id: item.id, read: true) }
                                    Button(item.favorite ? LocalizedStringKey("Unfavourite") : LocalizedStringKey("Favourite")) { Task { await store.toggleFavorite(item.id, current: item.favorite) } }
                                    Button("Copy image") {
                                        Task {
                                            guard let url = store.api()?.imageURL(item.thumbUrl), let image = await loader.image(url) else {
                                                store.error = String(localized: "Couldn't load image"); return
                                            }
                                            UIPasteboard.general.image = image; copied.toggle()
                                        }
                                    }
                                    Button("Copy link") { UIPasteboard.general.string = "#\(item.id)"; copied.toggle() }
                                    ShareLink(item: "#\(item.id)") { Label("Share", systemImage: "square.and.arrow.up") }
                                }
                            }
                        }
                    }
                }.padding()
            } else if let error = store.detailError {
                ContentUnavailableView {
                    Label("Couldn't load gallery", systemImage: "book")
                } description: { Text(verbatim: error) } actions: {
                    Button("Try again") { Task { _ = await store.open(id) } }
                }
            } else { ProgressView().padding() }
        }
        .lucinaScreen("NClient")
        .refreshable { _ = await store.open(id) }
        .task {
            if store.details[id] == nil { _ = await store.open(id) }
            if readImmediately, !opened, let detail = store.details[id], !detail.pages.isEmpty {
                opened = true; read(detail, page: detail.progress?.page ?? 0)
            }
        }
        .fullScreenCover(item: $reading) { request in
            NClientReader(detail: request.detail, page: request.page, store: store, loader: loader)
        }
        .navigationDestination(item: $relatedRoute) { target in
            NClientDetailScreen(id: target.id, readImmediately: target.read, store: store, loader: loader, filter: filter)
        }
        .sensoryFeedback(.success, trigger: copied)
        .modifier(NClientPrivacy())
    }
    private func read(_ detail: NClientDetail, page: Int) { reading = NClientReading(detail: detail, page: page) }
    private func tagGroups(_ tags: [NClientTag]) -> some View {
        let grouped = Dictionary(grouping: tags, by: \.type)
        return VStack(alignment: .leading, spacing: 10) {
            ForEach(grouped.keys.sorted(), id: \.self) { type in
                Text(verbatim: type).font(.caption).foregroundStyle(Theme.textSecondary)
                ScrollView(.horizontal) {
                    HStack {
                        ForEach(grouped[type] ?? []) { tag in
                            Button { filter(tag) } label: { Text(verbatim: tag.name).font(.caption).padding(8).background(Theme.field, in: Capsule()) }
                        }
                    }
                }
            }
        }
    }
}

struct NClientSaveControl: View {
    let id: Int
    let store: NClientStore
    @State private var retry = 0
    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            if let status = store.saves[id], !status.finished {
                ProgressView(value: Double(status.done), total: Double(max(1, status.total)))
                if status.state == "queued" { Text("Waiting to save") }
                else { Text("Saving \(status.done) of \(status.total)") }
                Button("Cancel saving") { Task { await store.cancelSave(id) } }
                    .accessibilityIdentifier("nclient-save-cancel")
            } else if store.status?.save?.gallery != false {
                Button("Save to Gallery", systemImage: "square.and.arrow.down") { Task { await store.startSave(id) } }
                    .disabled(store.saving.contains(id)).accessibilityIdentifier("nclient-save")
            }
            if store.saving.contains(id) { ProgressView() }
            if let status = store.saves[id], status.finished {
                Text(NClientSaveText.state(status.state))
                Text("Saved: \(status.saved.count - status.duplicates), duplicates: \(status.duplicates), failed: \(status.failed.count)")
                    .font(.caption)
                if status.error == "media_unavailable" { Text("Gallery is not reachable right now.") }
            }
            if let error = store.saveErrors[id] {
                Text(verbatim: error)
                if store.saves[id]?.finished == false { Button("Try again") { retry += 1 } }
            }
        }
        .task(id: "\(store.saves[id]?.job ?? "")-\(retry)") { await store.pollSave(id) }
    }
}

private enum NClientSaveText {
    static func state(_ state: String) -> LocalizedStringKey {
        switch state {
        case "done": return "Saved to Gallery"
        case "cancelled": return "Saving cancelled"
        default: return "Saving failed"
        }
    }
}
