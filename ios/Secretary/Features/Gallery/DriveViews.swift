import QuickLook
import SwiftUI

// The Drive plugin (owner, 2026-10-26): every file in the owner's Drive folders (lecture PDFs and
// slides in Lezioni, the Tracer code project, old Nextcloud files, audio), browsed folder by folder
// as in Google Drive, plus the Drive collections that are not folders on disk (Voice memos, imports).
// The library is three places that never mix: the Gallery (the phones' backups), Films (the player's
// plugin) and this; the server splits them with `scope=gallery|films|drive`.
//
// Each folder level is a pushed view (DriveBrowser.swift), so the back gesture walks up; the
// breadcrumb bar jumps several levels at once through `DriveNavigator`. Previews: DrivePreviews.swift.
// A server before the Drive browser (`GET /v1/media/drive` 404s) keeps the old album-based list.

/// The Drive's navigation state, shared by every level (and set by `/drive/...` links).
@Observable
@MainActor
final class DriveNavigator {
    /// The pushed levels above the Drive screen.
    var stack: [DriveDestination] = []
    /// The sort every level uses (nil: the server's default).
    var sort: DriveSort?
    var order: DriveOrder?
    /// The Drive roots, kept while the app runs.
    var roots: DriveRoots?
    /// The server has no Drive browser: the old album list.
    var legacy = false

    func push(_ destination: DriveDestination, at index: Int) {
        stack = DriveStack.set(stack, at: index, to: destination)
    }

    func jump(to location: DriveLocation) {
        stack = DriveStack.jump(stack, to: location)
    }

    /// The level pushed at `index` (0: from the Drive screen).
    func destination(at index: Int) -> Binding<DriveDestination?> {
        Binding(get: { index < self.stack.count ? self.stack[index] : nil },
                set: { self.stack = DriveStack.set(self.stack, at: index, to: $0) })
    }
}

/// A pushed Drive level.
struct DriveDestinationView: View {
    let destination: DriveDestination
    let depth: Int

    var body: some View {
        switch destination {
        case let .folder(location):
            DriveFolderView(location: location, depth: depth)
        case let .album(id, title):
            DriveLegacyList(album: id, albumTitle: title, depth: depth)
        }
    }
}

extension View {
    /// Pushes the Drive level `index` when the navigator has one.
    func driveDestination(_ nav: DriveNavigator, store: GalleryStore, at index: Int) -> some View {
        navigationDestination(item: nav.destination(at: index)) { destination in
            DriveDestinationView(destination: destination, depth: index)
                .environment(store)
                .environment(nav)
        }
    }
}

/// The Drive plugin: the roots (folders, then the other collections) and a search over the whole Drive.
struct DriveScreen: View {
    @Environment(AppModel.self) private var app
    @Environment(GalleryStore.self) private var store
    @Environment(DriveNavigator.self) private var nav
    @Environment(MediaPlayer.self) private var media
    @State private var loading = false
    @State private var error: String?
    @State private var search = ""
    @State private var query = ""
    @State private var type: DriveFileType?
    @State private var results = DriveSearchModel()
    @State private var previewer = DrivePreviewer()

    var body: some View {
        // A container, not a Group: the modifiers below must not run again when the content switches.
        ZStack {
            if nav.legacy {
                DriveLegacyList(album: nil, albumTitle: nil)
            } else {
                browser
            }
        }
        .driveDestination(nav, store: store, at: 0)
        .task {
            await loadRoots()
            openRequestedFile()
        }
        .onChange(of: app.openDriveFileID) { _, _ in openRequestedFile() }
        .drivePreviews(previewer, playsMedia: nav.stack.isEmpty && !nav.legacy)
    }

    private var searching: Bool { !query.trimmingCharacters(in: .whitespaces).isEmpty }

    private var browser: some View {
        List {
            if searching {
                Section {
                    ForEach(results.items) { item in
                        Button { previewer.open(item, among: results.items, api: store.api, media: media) } label: {
                            DriveFileRow(item: item, showsLocation: true, opening: previewer.opening == item.id)
                        }
                        .accessibilityIdentifier("drive-\(item.id)")
                    }
                    if results.canLoadMore {
                        ProgressView().frame(maxWidth: .infinity).task { await results.loadMore(api: store.api()) }
                    }
                } header: {
                    DriveTypeChips(selection: $type)
                }
            } else {
                if let error {
                    Section { DriveErrorRow(message: error) { Task { await loadRoots() } } }
                }
                if let roots = nav.roots {
                    if !roots.folders.isEmpty {
                        Section("Folders") {
                            ForEach(roots.folders) { root in
                                Button { nav.push(.folder(DriveLocation(folder: root.folder)), at: 0) } label: {
                                    DriveFolderRow(name: root.folder, detail: DriveFormat.folderDetail(files: root.files, bytes: root.bytes))
                                }
                                .accessibilityIdentifier("drive-root-\(root.folder)")
                            }
                        }
                    }
                    if !roots.albums.isEmpty {
                        Section("Collections") {
                            ForEach(roots.albums) { album in
                                Button { nav.push(.album(id: album.id, title: album.title), at: 0) } label: {
                                    DriveFolderRow(name: album.title, detail: album.count.map { String(localized: "\($0) items") } ?? "",
                                                   symbol: album.id == "sys-voice-memos" ? "mic.fill" : "rectangle.stack.fill")
                                }
                                .accessibilityIdentifier("drive-album-\(album.id)")
                            }
                        }
                    }
                }
            }
        }
        .lucinaList()
        .overlay {
            if searching {
                if results.items.isEmpty, !results.loading {
                    if let message = results.error {
                        ContentUnavailableView("Couldn't search the Drive", systemImage: "exclamationmark.triangle", description: Text(message))
                    } else {
                        ContentUnavailableView.search(text: query)
                    }
                }
            } else if nav.roots == nil, loading {
                ProgressView()
            } else if nav.roots.map({ $0.folders.isEmpty && $0.albums.isEmpty }) == true {
                ContentUnavailableView("Nothing here yet", systemImage: "externaldrive",
                                       description: Text("Old files, computer backups, audio and files shared to Lucina appear here."))
            }
        }
        .navigationTitle("Drive")
        .navigationBarTitleDisplayMode(.inline)
        .searchable(text: $search, prompt: Text("Search the Drive"))
        .onSubmit(of: .search) { query = search }
        .onChange(of: search) { _, new in if new.isEmpty, !query.isEmpty { query = "" } }
        .task(id: GalleryAPI.Filter(scope: "drive", query: query, type: type?.rawValue)) {
            guard searching else { return }
            await results.search(GalleryAPI.Filter(scope: "drive", query: query, type: type?.rawValue), api: store.api())
        }
        .refreshable { await loadRoots() }
    }

    /// The roots; a 404 means a server before the Drive browser: the old list.
    private func loadRoots() async {
        loading = true
        defer { loading = false }
        do {
            nav.roots = try await store.api().driveRoots()
            nav.legacy = false
            error = nil
        } catch is CancellationError {
        } catch {
            if DriveFiles.isMissing(error) || nav.roots == nil {
                // The old list also has the saved listing for offline use.
                nav.legacy = true
            }
            self.error = error.localizedDescription
        }
    }

    /// `/drive/file/<id>`: the file's preview.
    private func openRequestedFile() {
        guard let id = app.openDriveFileID else { return }
        app.openDriveFileID = nil
        let api = store.api
        Task {
            do {
                let item = try await api().item(id)
                previewer.open(item, among: [item], api: api, media: media)
            } catch is CancellationError {
            } catch {
                previewer.error = error.localizedDescription
            }
        }
    }
}

/// The whole-Drive search at the root (`GET /v1/media/items?scope=drive&q=`).
@Observable
@MainActor
final class DriveSearchModel {
    private(set) var items: [GalleryItem] = []
    private(set) var loading = false
    var error: String?
    @ObservationIgnored private var cursor: String?
    @ObservationIgnored private var requested: GalleryAPI.Filter?

    var canLoadMore: Bool { cursor != nil }

    func search(_ filter: GalleryAPI.Filter, api: GalleryAPI) async {
        requested = filter
        loading = true
        defer { if requested == filter { loading = false } }
        do {
            let page = try await api.items(filter)
            guard requested == filter else { return }
            items = page.items
            cursor = page.nextCursor
            error = nil
        } catch is CancellationError {
        } catch {
            guard requested == filter else { return }
            items = []
            cursor = nil
            self.error = error.localizedDescription
        }
    }

    func loadMore(api: GalleryAPI) async {
        guard let cursor, let filter = requested, !loading else { return }
        loading = true
        defer { loading = false }
        guard let page = try? await api.items(filter, cursor: cursor), requested == filter else { return }
        let known = Set(items.map(\.id))
        items += page.items.filter { !known.contains($0.id) }
        self.cursor = page.nextCursor
    }
}

/// The Drive before the folder browser (and its collections such as Voice memos): the album folders
/// and a flat file list, from `GET /items?scope=drive[&album=]`.
struct DriveLegacyList: View {
    @Environment(GalleryStore.self) private var store
    @Environment(DriveNavigator.self) private var nav
    @Environment(MediaPlayer.self) private var media
    /// The collection this list shows; nil: the old Drive root (its folders, then every file).
    let album: String?
    let albumTitle: String?
    /// The pushed level (nil: the Drive screen itself).
    var depth: Int?
    @State private var search = ""
    @State private var previewer = DrivePreviewer()

    var body: some View {
        @Bindable var store = store
        List {
            if album == nil, store.filter.album == nil, store.filter.query.isEmpty, !folders.isEmpty {
                Section("Folders") {
                    ForEach(folders) { album in
                        Button { store.filter.album = album.id } label: {
                            DriveFolderRow(name: album.title, detail: String(localized: "\(album.count) items"))
                        }
                        .accessibilityIdentifier("drive-folder-\(album.id)")
                    }
                }
            }
            Section {
                ForEach(store.items) { item in
                    Button { previewer.open(item, among: store.items, api: store.api, media: media) } label: {
                        DriveFileRow(item: item, showsLocation: false, opening: previewer.opening == item.id)
                    }
                    .accessibilityIdentifier("drive-\(item.id)")
                }
                if store.canLoadMore {
                    ProgressView().frame(maxWidth: .infinity).task { await store.loadMore() }
                }
            } header: {
                kinds
            }
        }
        .lucinaList()
        .overlay {
            if store.items.isEmpty, !store.loading {
                ContentUnavailableView("Nothing here yet", systemImage: "externaldrive",
                                       description: Text("Old files, computer backups, audio and files shared to Lucina appear here."))
            }
        }
        .navigationTitle(title)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            if album == nil, store.filter.album != nil {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("All files") { store.filter.album = nil }
                }
            }
        }
        .searchable(text: $search, prompt: Text("Search the Drive"))
        .onSubmit(of: .search) { store.filter.query = search }
        .onChange(of: search) { _, q in if q.isEmpty, !store.filter.query.isEmpty { store.filter.query = "" } }
        .onChange(of: store.filter) { Task { await store.reload() } }
        .task {
            if store.filter.album != album {
                // onChange reloads.
                store.filter.album = album
            } else {
                await store.reload()
            }
            await store.loadAlbums()
        }
        .refreshable {
            await store.reload()
            await store.loadAlbums()
        }
        .drivePreviews(previewer, playsMedia: nav.stack.count == (depth.map { $0 + 1 } ?? 0))
    }

    private var title: String {
        if let albumTitle { return albumTitle }
        return store.filter.album.flatMap { id in store.albums.first { $0.id == id }?.title } ?? String(localized: "Drive")
    }

    /// The imported folders (Nextcloud, Lyd, Video…) and the voice memos; not the other system albums
    /// ("Videos" repeats the Video folder).
    private var folders: [GalleryAlbum] {
        store.albums.filter { $0.systemKey == nil || $0.systemKey == "voice-memos" }
    }

    private var kinds: some View {
        @Bindable var store = store
        return ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: LucinaTokens.Space.sm) {
                ChipButton(title: "All", isOn: store.filter.kind == nil) { store.filter.kind = nil }
                ChipButton(title: "Videos", icon: "film", isOn: store.filter.kind == "video") { store.filter.kind = "video" }
                ChipButton(title: "Photos", icon: "photo", isOn: store.filter.kind == "photo") { store.filter.kind = "photo" }
                ChipButton(title: "Audio", icon: "waveform", isOn: store.filter.kind == "audio") { store.filter.kind = "audio" }
                ChipButton(title: "Files", icon: "doc", isOn: store.filter.kind == "file") { store.filter.kind = "file" }
            }
            .padding(.vertical, 4)
        }
        .textCase(nil)
        .listRowInsets(EdgeInsets())
    }
}
