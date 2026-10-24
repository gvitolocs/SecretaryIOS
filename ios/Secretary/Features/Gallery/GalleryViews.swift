import SwiftUI

// The Gallery plugin: the owner's photos, videos and audio on their server, newest first and
// grouped by month like Google Photos; albums; search ("Copenhagen", "July 2025"); a viewer
// that pages through the timeline and hands videos and audio to the media player.

@Observable
@MainActor
final class GalleryStore {
    private(set) var items: [GalleryItem] = []
    private(set) var albums: [GalleryAlbum] = []
    private(set) var total: Int?
    private(set) var loading = false
    var error: String?
    var filter = GalleryAPI.Filter()
    @ObservationIgnored private var cursor: String?
    @ObservationIgnored let api: () -> GalleryAPI

    init(api: @escaping () -> GalleryAPI) { self.api = api }

    var canLoadMore: Bool { cursor != nil }

    func reload() async {
        loading = true
        defer { loading = false }
        do {
            let page = try await api().items(filter)
            items = page.items
            cursor = page.nextCursor
            total = page.total
            error = nil
        } catch is CancellationError {
        } catch {
            self.error = error.localizedDescription
        }
    }

    func loadMore() async {
        guard let cursor, !loading else { return }
        loading = true
        defer { loading = false }
        if let page = try? await api().items(filter, cursor: cursor) {
            let known = Set(items.map(\.id))
            items += page.items.filter { !known.contains($0.id) }
            self.cursor = page.nextCursor
        }
    }

    func loadAlbums() async {
        if let a = try? await api().albums() { albums = a }
    }

    func replace(_ item: GalleryItem) {
        if let i = items.firstIndex(where: { $0.id == item.id }) { items[i] = item }
    }

    func remove(_ id: String) {
        items.removeAll { $0.id == id }
    }

    /// Items grouped by month, newest first.
    var sections: [(title: String, items: [GalleryItem])] {
        var out: [(String, [GalleryItem])] = []
        for item in items {
            let title = item.date.map(GalleryFormat.month) ?? String(localized: "No date")
            if out.last?.0 == title { out[out.count - 1].1.append(item) } else { out.append((title, [item])) }
        }
        return out
    }
}

/// The Gallery screen. A plugin: the Plugins tab (or, until it exists, `galleryEntry()`) pushes it.
struct GalleryScreen: View {
    @Environment(GalleryStore.self) private var store
    @Environment(PhotoBackup.self) private var backup
    @State private var viewing: GalleryItem?
    @State private var showBackup = false
    @State private var showAlbums = false
    @State private var search = ""

    var body: some View {
        @Bindable var store = store
        ScrollView {
            BackupBanner { showBackup = true }
                .padding(.horizontal)
                .padding(.top, 4)
            FilterChips()
                .padding(.vertical, LucinaTokens.Space.sm)
            if store.items.isEmpty, !store.loading {
                ContentUnavailableView(
                    store.filter.query.isEmpty ? "No photos yet" : "No results",
                    systemImage: "photo.on.rectangle",
                    description: Text(store.filter.query.isEmpty
                        ? "Turn on backup and your iPhone's photos appear here."
                        : "Try a place, a month or an album name.")
                )
                .padding(.top, 40)
            }
            LazyVStack(alignment: .leading, spacing: 8, pinnedViews: [.sectionHeaders]) {
                ForEach(store.sections, id: \.title) { section in
                    Section {
                        GalleryGrid(items: section.items) { viewing = $0 }
                    } header: {
                        Text(section.title)
                            .font(LucinaTokens.Fonts.headline)
                            .foregroundStyle(Theme.text)
                            .padding(.horizontal)
                            .padding(.vertical, 6)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .background(Theme.page)
                    }
                }
                if store.canLoadMore {
                    ProgressView()
                        .frame(maxWidth: .infinity)
                        .padding()
                        .task { await store.loadMore() }
                }
            }
        }
        .lucinaScreen()
        .navigationTitle(store.filter.album.flatMap { id in store.albums.first { $0.id == id }?.title } ?? String(localized: "Gallery"))
        .navigationBarTitleDisplayMode(.large)
        .searchable(text: $search, prompt: "Places, months, albums")
        .onSubmit(of: .search) { store.filter.query = search }
        .onChange(of: search) { _, new in if new.isEmpty { store.filter.query = "" } }
        .onChange(of: store.filter) { Task { await store.reload() } }
        .refreshable {
            backup.kick()
            await store.reload()
        }
        .toolbar {
            ToolbarItemGroup(placement: .topBarTrailing) {
                Button { showAlbums = true } label: { Image(systemName: "rectangle.stack") }
                    .accessibilityLabel("Albums")
                    .accessibilityIdentifier("galleryAlbums")
                Button { showBackup = true } label: { Image(systemName: backup.settings.enabled ? "icloud.and.arrow.up" : "icloud.slash") }
                    .accessibilityLabel("Backup")
                    .accessibilityIdentifier("galleryBackup")
            }
        }
        .task {
            if store.items.isEmpty { await store.reload() }
            await store.loadAlbums()
        }
        .sheet(isPresented: $showBackup) { NavigationStack { BackupSettingsView() }.lucinaSheet() }
        .sheet(isPresented: $showAlbums) {
            NavigationStack {
                AlbumsView { album in
                    store.filter.album = album.id
                    showAlbums = false
                }
            }
            .lucinaSheet()
        }
        .fullScreenCover(item: $viewing) { item in
            GalleryViewer(start: item)
        }
    }
}

/// Square cells, 3 across on iPhone.
struct GalleryGrid: View {
    let items: [GalleryItem]
    let open: (GalleryItem) -> Void

    var body: some View {
        LazyVGrid(columns: [GridItem(.adaptive(minimum: 110), spacing: 2)], spacing: 2) {
            ForEach(items) { item in
                Button { open(item) } label: {
                    // A square cell whatever the picture's shape: the thumb fills it and is cropped.
                    Color.clear
                        .aspectRatio(1, contentMode: .fit)
                        .overlay { GalleryThumb(item: item) }
                        .clipped()
                        .contentShape(Rectangle())
                        .overlay(alignment: .bottomTrailing) {
                            if item.isVideo || item.isAudio {
                                HStack(spacing: 3) {
                                    Image(systemName: item.isVideo ? "play.fill" : "waveform")
                                    if let ms = item.durationMs { Text(MediaFormat.time(TimeInterval(ms) / 1000)) }
                                }
                                .font(.caption2.weight(.semibold))
                                .foregroundStyle(.white)
                                .padding(4)
                                .shadow(radius: 2)
                            }
                        }
                        .overlay(alignment: .topTrailing) {
                            if item.favorite == true {
                                Image(systemName: "heart.fill").font(.caption2).foregroundStyle(.white).padding(4).shadow(radius: 2)
                            }
                        }
                }
                .buttonStyle(.plain)
                .accessibilityLabel(item.title)
                .accessibilityIdentifier("gallery-\(item.id)")
            }
        }
    }
}

struct GalleryThumb: View {
    @Environment(GalleryStore.self) private var store
    let item: GalleryItem
    var large = false

    var body: some View {
        let path = large ? (item.posterUrl ?? item.thumbUrl) : (item.thumbUrl ?? item.posterUrl)
        AsyncImage(url: store.api().absolute(path)) { phase in
            if let image = phase.image {
                image.resizable().aspectRatio(contentMode: large ? .fit : .fill)
            } else {
                ZStack {
                    Rectangle().fill(Theme.field)
                    Image(systemName: item.isAudio ? "waveform" : item.isVideo ? "film" : "photo").foregroundStyle(Theme.textSecondary)
                }
            }
        }
    }
}

/// Backup status above the timeline.
private struct BackupBanner: View {
    @Environment(PhotoBackup.self) private var backup
    let open: () -> Void

    var body: some View {
        Button(action: open) {
            HStack(spacing: 10) {
                Image(systemName: icon).foregroundStyle(tint)
                VStack(alignment: .leading, spacing: 2) {
                    Text(title).font(LucinaTokens.Fonts.subheadline.weight(.semibold)).foregroundStyle(Theme.text)
                    if let detail { Text(detail).font(LucinaTokens.Fonts.caption).foregroundStyle(Theme.textSecondary) }
                }
                Spacer()
                if case .uploading = backup.status { ProgressView() }
            }
            .padding(LucinaTokens.Space.md)
            .background(Theme.card, in: RoundedRectangle(cornerRadius: Theme.cornerRadius, style: .continuous))
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier("backupBanner")
    }

    private var icon: String {
        switch backup.status {
        case .off: "icloud.slash"
        case .needsAccess: "exclamationmark.triangle"
        case .failed: "exclamationmark.icloud"
        case .waiting: "pause.circle"
        default: "checkmark.icloud"
        }
    }
    private var tint: Color {
        switch backup.status {
        case .needsAccess, .failed: Theme.warning
        case .off: Theme.textSecondary
        default: Theme.accent
        }
    }
    private var title: String {
        switch backup.status {
        case .off: String(localized: "Backup is off")
        case .needsAccess: String(localized: "Allow photo access to back up")
        case .idle: String(localized: "Backup is up to date")
        case .scanning: String(localized: "Looking for new photos…")
        case let .uploading(done, of): String(localized: "Backing up \(done + 1) of \(of)")
        case let .waiting(reason): reason
        case let .failed(message): String(localized: "Backup paused: \(message)")
        }
    }
    private var detail: String? {
        var parts: [String] = []
        if backup.backedUp > 0 { parts.append(String(localized: "\(backup.backedUp) backed up")) }
        if let d = backup.lastBackup { parts.append(d.formatted(.relative(presentation: .named))) }
        return parts.isEmpty ? nil : parts.joined(separator: " · ")
    }
}

struct BackupSettingsView: View {
    @Environment(PhotoBackup.self) private var backup
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        @Bindable var backup = backup
        Form {
            Section {
                Toggle("Back up this iPhone", isOn: $backup.settings.enabled)
                    .accessibilityIdentifier("backupToggle")
            } footer: {
                Text("Photos and videos go to your own server (nezopt), in full quality. Nothing is ever deleted from this iPhone.")
            }
            Section("When") {
                Toggle("Only on Wi-Fi", isOn: $backup.settings.wifiOnly)
                Toggle("Only while charging", isOn: $backup.settings.chargingOnly)
                Toggle("Include videos", isOn: $backup.settings.includeVideos)
            }
            Section("Status") {
                LabeledContent("Backed up", value: "\(backup.backedUp)")
                if backup.remaining > 0 { LabeledContent("Remaining", value: "\(backup.remaining)") }
                if let d = backup.lastBackup { LabeledContent("Last backup", value: d.formatted(date: .abbreviated, time: .shortened)) }
                if let e = backup.lastError { Text(e).font(LucinaTokens.Fonts.caption).foregroundStyle(Theme.warning) }
                Button("Back up now") { backup.kick() }
                    .disabled(!backup.settings.enabled)
                if backup.status == .needsAccess {
                    Button("Open Settings") {
                        if let url = URL(string: UIApplication.openSettingsURLString) { UIApplication.shared.open(url) }
                    }
                }
            }
        }
        .lucinaList()
        .navigationTitle("Photo backup")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } } }
    }
}

struct AlbumsView: View {
    @Environment(GalleryStore.self) private var store
    @Environment(\.dismiss) private var dismiss
    let pick: (GalleryAlbum) -> Void
    @State private var newTitle = ""
    @State private var creating = false

    var body: some View {
        List {
            ForEach(store.albums.filter { $0.count > 0 || $0.systemKey == nil }) { album in
                Button { pick(album) } label: {
                    HStack {
                        Image(systemName: icon(album)).foregroundStyle(Theme.accent).frame(width: 28)
                        Text(album.title).foregroundStyle(Theme.text)
                        Spacer()
                        Text("\(album.count)").foregroundStyle(Theme.textSecondary)
                    }
                }
                .foregroundStyle(.primary)
            }
        }
        .lucinaList()
        .navigationTitle("Albums")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .cancellationAction) { Button("Close") { dismiss() } }
            ToolbarItem(placement: .primaryAction) { Button { creating = true } label: { Image(systemName: "plus") } }
        }
        .alert("New album", isPresented: $creating) {
            TextField("Title", text: $newTitle)
            Button("Create") {
                let title = newTitle
                newTitle = ""
                Task {
                    _ = try? await store.api().createAlbum(title: title, items: [])
                    await store.loadAlbums()
                }
            }
            Button("Cancel", role: .cancel) {}
        }
        .task { await store.loadAlbums() }
    }

    private func icon(_ a: GalleryAlbum) -> String {
        switch a.systemKey {
        case "camera": "camera"
        case "videos": "video"
        case "screenshots": "camera.viewfinder"
        case "voice-memos": "mic"
        case "favorites": "heart"
        default: "rectangle.stack"
        }
    }
}

/// Full screen, swipe between items. Photos zoom; videos and audio open the media player.
struct GalleryViewer: View {
    @Environment(GalleryStore.self) private var store
    @Environment(MediaPlayer.self) private var media
    @Environment(\.dismiss) private var dismiss
    let start: GalleryItem
    @State private var current: String = ""
    @State private var showInfo = false
    @State private var addToAlbum = false
    @State private var confirmTrash = false

    var body: some View {
        let item = store.items.first { $0.id == current } ?? start
        NavigationStack {
            TabView(selection: $current) {
                ForEach(store.items) { it in
                    page(it).tag(it.id)
                }
            }
            .tabViewStyle(.page(indexDisplayMode: .never))
            .background(.black)
            .ignoresSafeArea(edges: .bottom)
            .toolbarBackground(.visible, for: .navigationBar, .bottomBar)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Close") { dismiss() } }
                ToolbarItem(placement: .principal) {
                    VStack(spacing: 0) {
                        Text(item.date.map { $0.formatted(date: .abbreviated, time: .shortened) } ?? item.title).font(.subheadline.weight(.semibold))
                        if let p = item.place { Text(p.name).font(.caption).foregroundStyle(.secondary) }
                    }
                }
                ToolbarItemGroup(placement: .bottomBar) {
                    Button { toggleFavorite(item) } label: { Image(systemName: item.favorite == true ? "heart.fill" : "heart") }
                        .accessibilityLabel("Favourite")
                    Spacer()
                    Button { showInfo = true } label: { Image(systemName: "info.circle") }
                        .accessibilityLabel("Info")
                    Spacer()
                    Button { addToAlbum = true } label: { Image(systemName: "rectangle.stack.badge.plus") }
                        .accessibilityLabel("Add to album")
                    Spacer()
                    Button(role: .destructive) { confirmTrash = true } label: { Image(systemName: "trash") }
                        .accessibilityLabel("Move to the server's trash")
                }
            }
        }
        .onAppear { current = start.id }
        .sheet(isPresented: $showInfo) { NavigationStack { GalleryInfo(item: item) }.lucinaSheet().presentationDetents([.medium, .large]) }
        .sheet(isPresented: $addToAlbum) {
            NavigationStack {
                AlbumsView { album in
                    addToAlbum = false
                    Task {
                        try? await store.api().add([item.id], toAlbum: album.id)
                        await store.loadAlbums()
                    }
                }
            }
        }
        .confirmationDialog("Move to the server's trash?", isPresented: $confirmTrash, titleVisibility: .visible) {
            Button("Move to trash", role: .destructive) {
                Task {
                    try? await store.api().trash(item.id)
                    store.remove(item.id)
                    if store.items.isEmpty { dismiss() }
                }
            }
        } message: {
            Text("It stays on this iPhone. The server keeps it in the trash for 30 days.")
        }
        .fullScreenCover(isPresented: Binding(get: { media.presented }, set: { media.presented = $0 })) { MediaPlayerScreen() }
    }

    @ViewBuilder
    private func page(_ it: GalleryItem) -> some View {
        if it.isVideo || it.isAudio {
            ZStack {
                GalleryThumb(item: it, large: true)
                Button {
                    let playable = store.items.filter { $0.isVideo || $0.isAudio }
                    let queue = playable.map(\.mediaItem)
                    media.play(queue, startAt: playable.firstIndex { $0.id == it.id } ?? 0)
                    media.presented = true
                } label: {
                    Image(systemName: "play.circle.fill").font(.system(size: 72)).foregroundStyle(.white).shadow(radius: 6)
                }
                .accessibilityIdentifier("galleryPlay")
            }
        } else {
            ZoomableImage(url: store.api().absolute(it.urls?.large ?? it.posterUrl ?? it.thumbUrl))
        }
    }

    private func toggleFavorite(_ item: GalleryItem) {
        Task {
            if let updated = try? await store.api().setFavorite(item.id, !(item.favorite ?? false)) {
                var copy = updated
                copy.posterUrl = copy.posterUrl ?? item.posterUrl
                copy.thumbUrl = copy.thumbUrl ?? item.thumbUrl
                store.replace(copy)
            }
        }
    }
}

private struct ZoomableImage: View {
    let url: URL?
    @State private var scale: CGFloat = 1
    @State private var base: CGFloat = 1

    var body: some View {
        AsyncImage(url: url) { phase in
            if let image = phase.image {
                image.resizable().scaledToFit()
                    .scaleEffect(scale)
                    .gesture(MagnifyGesture()
                        .onChanged { scale = max(1, base * $0.magnification) }
                        .onEnded { _ in base = scale })
                    .onTapGesture(count: 2) {
                        withAnimation { scale = scale > 1 ? 1 : 2.5 }
                        base = scale
                    }
            } else if phase.error != nil {
                Image(systemName: "exclamationmark.triangle").foregroundStyle(.white)
            } else {
                ProgressView().tint(.white)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

private struct GalleryInfo: View {
    let item: GalleryItem

    var body: some View {
        Form {
            LabeledContent("Name", value: item.title)
            if let d = item.date { LabeledContent("Taken", value: d.formatted(date: .long, time: .shortened)) }
            if let p = item.place {
                LabeledContent("Place", value: [p.name, p.admin1, p.country].compactMap { $0 }.joined(separator: ", "))
            }
            if let w = item.width, let h = item.height { LabeledContent("Size", value: "\(w) × \(h)") }
            if let bytes = item.size { LabeledContent("File", value: ByteCountFormatter.string(fromByteCount: Int64(bytes), countStyle: .file)) }
            if let c = item.camera { LabeledContent("Camera", value: c) }
            if let caption = item.caption { Text(caption) }
        }
        .lucinaList()
        .navigationTitle("Info")
        .navigationBarTitleDisplayMode(.inline)
    }
}

/// What the timeline shows: everything, one kind, favourites, and the album being viewed.
private struct FilterChips: View {
    @Environment(GalleryStore.self) private var store

    var body: some View {
        @Bindable var store = store
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: LucinaTokens.Space.sm) {
                if let album = store.filter.album {
                    ChipButton(title: LocalizedStringKey(store.albums.first { $0.id == album }?.title ?? "Album"),
                               icon: "xmark", isOn: true) { store.filter.album = nil }
                        .accessibilityIdentifier("galleryClearAlbum")
                }
                ChipButton(title: "All", isOn: store.filter.kind == nil && !store.filter.favorites) {
                    store.filter.kind = nil
                    store.filter.favorites = false
                }
                ChipButton(title: "Photos", icon: "photo", isOn: store.filter.kind == "photo") { store.filter.kind = "photo" }
                ChipButton(title: "Videos", icon: "video", isOn: store.filter.kind == "video") { store.filter.kind = "video" }
                ChipButton(title: "Audio", icon: "waveform", isOn: store.filter.kind == "audio") { store.filter.kind = "audio" }
                ChipButton(title: "Favourites", icon: "heart", isOn: store.filter.favorites) { store.filter.favorites.toggle() }
            }
            .padding(.horizontal)
        }
    }
}

/// The Gallery button on the chat, until the Plugins tab has its card.
private struct GalleryEntry: ViewModifier {
    @Environment(AppModel.self) private var app

    func body(content: Content) -> some View {
        @Bindable var app = app
        content
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button { app.showGallery = true } label: { Image(systemName: "photo.on.rectangle.angled") }
                        .accessibilityLabel("Gallery")
                        .accessibilityIdentifier("galleryButton")
                }
            }
            .navigationDestination(isPresented: $app.showGallery) { GalleryScreen() }
    }
}

extension View {
    /// The Gallery button and screen, on the chat.
    func galleryEntry() -> some View { modifier(GalleryEntry()) }
}
