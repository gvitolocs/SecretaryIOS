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
    var filter: GalleryAPI.Filter
    /// `gallery` (the Gallery: the phones' photos and videos) or `drive` (the Drive plugin).
    let scope: String?
    /// This iPhone's device id (the photo backup's): the Gallery opens on its photos.
    let thisDevice: String?
    private(set) var devices: [GalleryDevice] = []
    /// What the store shows when nothing is filtered: this device's photos, in this scope.
    var defaultFilter: GalleryAPI.Filter { GalleryAPI.Filter(scope: scope, device: thisDevice) }
    @ObservationIgnored private var cursor: String?
    @ObservationIgnored let api: () -> GalleryAPI

    @ObservationIgnored private var showingCachedListing = false
    /// A page from the server arrived: the saved listing (read in the background at launch) is not shown.
    @ObservationIgnored private var hasServerPage = false

    private struct Listing: Codable, Sendable {
        let items: [GalleryItem]
        let nextCursor: String?
        let total: Int?
    }

    private var listingURL: URL {
        FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("\(scope ?? "gallery")-listing.json")
    }

    /// `items`: a fixed list for the viewer (the photos and videos of a Drive folder), not the saved listing.
    init(api: @escaping () -> GalleryAPI, scope: String? = nil, thisDevice: String? = nil, items fixed: [GalleryItem]? = nil) {
        self.api = api
        self.scope = scope
        self.thisDevice = thisDevice
        filter = GalleryAPI.Filter(scope: scope, device: thisDevice)
        if let fixed {
            items = fixed
            return
        }
        // The saved listing can be thousands of items: decoded off the main thread, not at launch.
        let url = listingURL
        Task { [weak self] in
            let saved = await Task.detached(priority: .utility) { () -> Listing? in
                guard let data = try? Data(contentsOf: url) else { return nil }
                return try? JSONDecoder().decode(Listing.self, from: data)
            }.value
            guard let self, let saved, !self.hasServerPage, self.items.isEmpty, self.filter == GalleryAPI.Filter(scope: self.scope) else { return }
            self.items = saved.items
            self.cursor = saved.nextCursor
            self.total = saved.total
            self.showingCachedListing = true
            if self.error != nil { self.error = String(localized: "Offline — showing saved photos") }
        }
    }

    var canLoadMore: Bool { cursor != nil }

    func reload() async {
        loading = true
        defer { loading = false }
        let requestedFilter = filter
        do {
            let page = try await api().items(requestedFilter)
            guard filter == requestedFilter, !Task.isCancelled else { return }
            items = page.items
            cursor = page.nextCursor
            total = page.total
            error = nil
            showingCachedListing = false
            hasServerPage = true
            if requestedFilter == defaultFilter {
                let saved = Listing(items: page.items, nextCursor: page.nextCursor, total: page.total)
                let url = listingURL
                await Task.detached(priority: .utility) {
                    try? FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
                    if let data = try? JSONEncoder().encode(saved) { try? data.write(to: url, options: .atomic) }
                }.value
            }
        } catch is CancellationError {
        } catch {
            guard filter == requestedFilter else { return }
            self.error = showingCachedListing ? String(localized: "Offline — showing saved photos") : error.localizedDescription
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

    func loadDevices() async {
        if let d = try? await api().devices() { devices = d }
    }

    func loadAlbums() async {
        if let a = try? await api().albums(scope: scope) { albums = a }
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

/// The Gallery screen. A plugin: the Plugins tab's card pushes it (`app.showGallery`).
struct GalleryScreen: View {
    @Environment(GalleryStore.self) private var store
    @Environment(PhotoBackup.self) private var backup
    @State private var viewing: GalleryItem?
    /// The photo on screen in the viewer: the one it zooms back into, kept scrolled into view.
    @State private var shownID: String?
    @Namespace private var zoom
    @State private var showBackup = false
    @State private var showAlbums = false
    @State private var search = ""

    var body: some View {
        @Bindable var store = store
        ScrollViewReader { proxy in
        ScrollView {
            BackupBanner { showBackup = true }
                .padding(.horizontal)
                .padding(.top, 4)
            FilterChips()
                .padding(.vertical, LucinaTokens.Space.sm)
            if let error = store.error {
                Text(error)
                    .font(LucinaTokens.Fonts.caption)
                    .foregroundStyle(Theme.textSecondary)
                    .padding(.horizontal)
            }
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
                        GalleryGrid(items: section.items, zoom: zoom) { open($0) }
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
            ToolbarItem(placement: .topBarLeading) { DeviceMenu() }
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
            await store.reload()
            await store.loadAlbums()
            // `-SecretaryGalleryOpenFirst YES` (device checks without UI tests): open the newest photo.
            if UserDefaults.standard.bool(forKey: "SecretaryGalleryOpenFirst"), viewing == nil {
                if let first = store.items.first(where: { $0.kind == "photo" }) { open(first) }
            }
            // `-SecretaryGalleryOpen <item id>`: open that item (it must be on the first page).
            if let id = UserDefaults.standard.string(forKey: "SecretaryGalleryOpen"), viewing == nil {
                if let item = store.items.first(where: { $0.id == id }) { open(item) }
            }
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
        // Google Photos: behind the viewer the grid follows the photo on screen, so closing zooms
        // into its tile.
        // No anchor: the grid moves only when that tile is off screen. Centring a visible tile moved
        // it while the zoom ran from it, and the zoom lost its source both ways.
        .onChange(of: shownID) { _, id in
            if let id { proxy.scrollTo(id) }
        }
        }
        .fullScreenCover(item: $viewing) { item in
            GalleryViewer(start: item, shown: $shownID, zoomDismiss: true)
                .presentationBackground(.clear)
                .zoomTransition(from: shownID ?? item.id, in: zoom)
        }
    }

    /// The viewer grows out of the tapped tile: the zoom source must be that tile before it opens
    /// (it still held the last photo viewed, which was off screen, so the open did not zoom).
    private func open(_ item: GalleryItem) {
        shownID = item.id
        viewing = item
    }
}

/// Square cells, 3 across on iPhone.
struct GalleryGrid: View {
    let items: [GalleryItem]
    var zoom: Namespace.ID?
    let open: (GalleryItem) -> Void

    var body: some View {
        // Google Photos on a phone: 4 columns (photos_photogrid_calculator_compact_column_count);
        // wider screens get more, about 90 pt each.
        LazyVGrid(columns: [GridItem(.adaptive(minimum: 86), spacing: 2)], spacing: 2) {
            ForEach(items) { item in
                Button { open(item) } label: {
                    // A square cell whatever the picture's shape: the thumb fills it and is cropped.
                    Color.clear
                        .aspectRatio(1, contentMode: .fit)
                        .overlay { RustTile(item: item) }
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
                .zoomSource(item.id, in: zoom)
                .id(item.id)
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

/// The owner's devices (2026-10-27): the Gallery opens on this iPhone's photos; the menu shows another
/// device's (a Mac, an Android, the old backups) or every device's. Everything is in the Lucina cloud.
private struct DeviceMenu: View {
    @Environment(GalleryStore.self) private var store

    var body: some View {
        @Bindable var store = store
        Menu {
            Picker("Device", selection: $store.filter.device) {
                if let mine = store.thisDevice {
                    Label("This iPhone", systemImage: "iphone").tag(Optional(mine))
                }
                ForEach(store.devices.filter { $0.id != store.thisDevice }) { d in
                    Label("\(d.name) · \(d.photos + d.videos)", systemImage: d.icon).tag(Optional(d.id))
                }
                Label("All devices", systemImage: "square.stack.3d.up").tag(String?.none)
            }
        } label: {
            Label(title, systemImage: icon)
                .labelStyle(.iconOnly)
        }
        .accessibilityLabel("Device: \(title)")
        .accessibilityIdentifier("galleryDevice")
        .task { await store.loadDevices() }
    }

    private var title: String {
        guard let id = store.filter.device else { return String(localized: "All devices") }
        if id == store.thisDevice { return String(localized: "This iPhone") }
        return store.devices.first { $0.id == id }?.name ?? id
    }

    private var icon: String {
        guard let id = store.filter.device else { return "square.stack.3d.up" }
        if id == store.thisDevice { return "iphone" }
        return store.devices.first { $0.id == id }?.icon ?? "photo.on.rectangle"
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

/// The gradient behind the viewer's bars, so the date and the buttons read on any photo.
private struct ViewerScrim: View {
    let top: Bool

    var body: some View {
        LinearGradient(colors: [.black.opacity(0.75), .black.opacity(0)], startPoint: top ? .top : .bottom,
                       endPoint: top ? .bottom : .top)
            // Google Photos' lightbox_top_gradient_height and lightbox_bottom_gradient_height.
            .frame(height: top ? 140 : 300)
            .frame(maxWidth: .infinity)
            .ignoresSafeArea()
            .allowsHitTesting(false)
    }
}

/// Full screen, swipe between items. Photos zoom; videos and audio open the media player.
struct GalleryViewer: View {
    @Environment(GalleryStore.self) private var store
    @Environment(PhotoBackup.self) private var backup
    @Environment(MediaPlayer.self) private var media
    @Environment(\.dismiss) private var dismiss
    let start: GalleryItem
    @State private var current: String
    @State private var showInfo = false
    @State private var addToAlbum = false
    @State private var confirmTrash = false
    @State private var meter = FrameMeter()
    /// One tap on the photo hides the bars and the status bar (black, picture only); another brings them back.
    @State private var chromeHidden = false
    /// The photo is being swiped down or up (bars hidden, the grid fading in behind).
    @State private var dragging = false
    /// The photo that is zoomed, if any: there the drag pans instead of closing.
    @State private var zoomedID: String?

    /// The photo on screen, for the grid (the tile the viewer zooms back into).
    @Binding var shown: String?
    /// Presented with the zoom transition (the Gallery grid): its own interactive dismiss does the swipe down.
    let zoomDismiss: Bool

    init(start: GalleryItem, shown: Binding<String?> = .constant(nil), zoomDismiss: Bool = false) {
        self.start = start
        _shown = shown
        self.zoomDismiss = zoomDismiss
        _current = State(initialValue: start.id)
    }

    var body: some View {
        let bare = chromeHidden || dragging
        let item = store.items.first { $0.id == current } ?? start
        NavigationStack {
            pager
            // Google Photos: swipe down to close, up for the details.
            // From the grid the swipe down is the zoom transition's: the photo follows the finger and
            // shrinks back into its tile (owning it ourselves, 69c33557, only faded it out). Elsewhere
            // (Drive) there is no zoom transition, so our pan does both directions.
            .photoDragOut(enabled: zoomedID != current, systemDismiss: zoomDismiss, dragging: $dragging,
                          onDismiss: { dismiss() }, onInfo: { showInfo = true })
            .interactiveDismissDisabled(!zoomDismiss || zoomedID == current)
            // The top bar floats over a dark gradient; the bottom stays free.
            .overlay(alignment: .top) { ViewerScrim(top: true).opacity(bare ? 0 : 1) }
            .clearNavigationBackground()
            .ignoresSafeArea()
            .toolbarBackground(.hidden, for: .navigationBar)
            // White date and buttons on the scrims, in light mode too.
            .toolbarColorScheme(.dark, for: .navigationBar)
            .toolbar(bare ? .hidden : .visible, for: .navigationBar)
            .statusBarHidden(bare)
            .animation(.easeInOut(duration: 0.2), value: bare)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Button { dismiss() } label: { Image(systemName: "chevron.left") }
                        .accessibilityLabel("Close")
                }
                ToolbarItem(placement: .principal) {
                    VStack(spacing: 0) {
                        Text(item.date.map { $0.formatted(date: .abbreviated, time: .shortened) } ?? item.title).font(.subheadline.weight(.semibold))
                        if let p = item.place { Text(p.name).font(.caption).foregroundStyle(.secondary) }
                    }
                }
                ToolbarItemGroup(placement: .topBarTrailing) {
                    Button { showInfo = true } label: { Image(systemName: "info.circle") }
                        .accessibilityLabel("Info")
                    Button { toggleFavorite(item) } label: { Image(systemName: item.favorite == true ? "heart.fill" : "heart") }
                        .accessibilityLabel("Favourite")
                    Button(role: .destructive) { confirmTrash = true } label: { Image(systemName: "trash") }
                        .accessibilityLabel("Move to the server's trash")
                    Menu {
                        Button { addToAlbum = true } label: { Label("Add to album", systemImage: "rectangle.stack.badge.plus") }
                    } label: {
                        Image(systemName: "ellipsis.circle")
                    }
                    .accessibilityLabel("More")
                }
            }
        }
        .onAppear {
            shown = start.id
            #if DEBUG || SCREENSHOTS
            // `-SecretaryGalleryAutoPlay YES` (device checks): press play on the opened video, muted.
            if UserDefaults.standard.bool(forKey: "SecretaryGalleryAutoPlay"), start.isVideo || start.isAudio {
                Task { @MainActor in
                    try? await Task.sleep(for: .seconds(2))
                    media.player.isMuted = true
                    play(start)
                }
            }
            #endif
            meter.start()
            prefetchNeighbours(of: start.id)
            autoSwipe()
        }
        .onDisappear { meter.stop() }
        // The grid learns the photo once a swipe settles: updating it during the page animation
        // redrew the whole grid screen each swipe (25 hitches in 20 swipes on the iPhone, worst 83 ms).
        .task(id: current) {
            try? await Task.sleep(for: .milliseconds(500))
            if !Task.isCancelled { shown = current }
        }
        .onChange(of: current) { _, id in
            meter.swipes += 1
            prefetchNeighbours(of: id)
        }
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

    private func play(_ it: GalleryItem) {
        let playable = store.items.filter { $0.isVideo || $0.isAudio }
        let queue = playable.map(\.mediaItem)
        media.play(queue, startAt: playable.firstIndex { $0.id == it.id } ?? 0)
        media.presented = true
    }

    private static let pageGap: CGFloat = 24

    /// Smooth: a lazy paging scroll view builds only the pages near the screen. A paged TabView
    /// (legacy) re-created every page from the first one to the current one on each change (seen
    /// on the iPhone: 9 page loads for one swipe at the 9th photo, growing with the position).
    @ViewBuilder
    private var pager: some View {
        if gallerySmooth {
            // Google Photos leaves a 24 dp gap between photos while swiping (photos_pager_page_margin):
            // each page is the screen plus 12 pt of black on both sides, and the scroll view is that
            // much wider than the screen, so paging still lands on whole photos.
            GeometryReader { geo in
                ScrollView(.horizontal) {
                    LazyHStack(spacing: 0) {
                        ForEach(store.items) { it in
                            page(it)
                                .frame(width: geo.size.width, height: geo.size.height)
                                .padding(.horizontal, Self.pageGap / 2)
                                .id(it.id)
                        }
                    }
                    .scrollTargetLayout()
                }
                .frame(width: geo.size.width + Self.pageGap)
                .offset(x: -Self.pageGap / 2)
                .scrollTargetBehavior(.paging)
                .scrollPosition(id: Binding(get: { current }, set: { if let id = $0 { current = id } }))
                .scrollIndicators(.hidden)
                // A zoomed photo pans; the pager waits until it is fitted again.
                .scrollDisabled(zoomedID == current)
            }
        } else {
            TabView(selection: $current) {
                ForEach(store.items) { it in
                    page(it).tag(it.id)
                }
            }
            .tabViewStyle(.page(indexDisplayMode: .never))
        }
    }

    /// `-SecretaryGalleryAutoSwipe N` (device measurements without UI tests): page forward through N
    /// photos, one every 1.2 s, with the pager's own animation; frame times go to the GalleryFrames log.
    private func autoSwipe() {
        let n = UserDefaults.standard.integer(forKey: "SecretaryGalleryAutoSwipe")
        guard n > 0 else { return }
        Task { @MainActor in
            try? await Task.sleep(for: .seconds(2))
            for _ in 0..<n {
                guard let i = store.items.firstIndex(where: { $0.id == current }), i + 1 < store.items.count else { break }
                withAnimation(.easeInOut(duration: 0.35)) { current = store.items[i + 1].id }
                try? await Task.sleep(for: .milliseconds(1200))
            }
        }
    }

    /// The two photos either side of the one on screen, decoded ahead of the swipe.
    private func prefetchNeighbours(of id: String) {
        guard gallerySmooth, let i = store.items.firstIndex(where: { $0.id == id }) else { return }
        let near = [i + 1, i - 1, i + 2, i - 2].filter { store.items.indices.contains($0) }.map { store.items[$0] }
        PreviewPrefetcher.shared.prefetch(near.filter { backup.localAsset(for: $0.id) == nil }, api: store.api())
    }

    @ViewBuilder
    private func page(_ it: GalleryItem) -> some View {
        if it.isVideo || it.isAudio {
            ZStack {
                if it.isVideo {
                    // The poster as a photo: from Photos for a phone video, else the cached JPEG XL renditions.
                    RustPhoto(item: it, onTap: { chromeHidden.toggle() })
                } else {
                    GalleryThumb(item: it, large: true)
                        .contentShape(Rectangle())
                        .onTapGesture { chromeHidden.toggle() }
                }
                Button { play(it) } label: {
                    Image(systemName: "play.circle.fill").font(.system(size: 72)).foregroundStyle(.white).shadow(radius: 6)
                }
                .accessibilityIdentifier("galleryPlay")
            }
        } else {
            RustPhoto(item: it, onTap: { chromeHidden.toggle() }, onZoom: { zoomed in
                if zoomed { zoomedID = it.id } else if zoomedID == it.id { zoomedID = nil }
            })
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
                ChipButton(title: "Favourites", icon: "heart", isOn: store.filter.favorites) { store.filter.favorites.toggle() }
            }
            .padding(.horizontal)
        }
    }
}

