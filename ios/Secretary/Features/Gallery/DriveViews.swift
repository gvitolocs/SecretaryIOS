import QuickLook
import SwiftUI

// The Drive plugin (owner, 2026-10-26): what is not the phones' own photos and videos (imported
// folders such as the films, files shared to Lucina, voice memos) as folders and a file list, as
// in Google Drive. The Gallery shows only the phone backups; the server splits them with
// `scope=gallery` / `scope=drive`. Photos and videos open in the Gallery's viewer, audio in the
// media player, other files in Quick Look.

struct DriveScreen: View {
    @Environment(GalleryStore.self) private var store
    @Environment(MediaPlayer.self) private var media
    @State private var viewing: GalleryItem?
    @State private var preview: URL?
    @State private var opening: String?
    @State private var search = ""

    var body: some View {
        @Bindable var store = store
        List {
            if store.filter.album == nil, store.filter.query.isEmpty, !store.albums.isEmpty {
                Section("Folders") {
                    ForEach(store.albums) { album in
                        Button { store.filter.album = album.id } label: { folderRow(album) }
                            .accessibilityIdentifier("drive-folder-\(album.id)")
                    }
                }
            }
            Section {
                ForEach(store.items) { item in
                    Button { open(item) } label: { fileRow(item) }
                        .accessibilityIdentifier("drive-\(item.id)")
                }
                if store.canLoadMore {
                    ProgressView().frame(maxWidth: .infinity).task { await store.loadMore() }
                }
            } header: {
                kinds
            }
        }
        .listStyle(.insetGrouped)
        .overlay {
            if store.items.isEmpty, !store.loading {
                ContentUnavailableView("Nothing here yet", systemImage: "externaldrive",
                                       description: Text("Films, imported folders and files shared to Lucina appear here."))
            }
        }
        .navigationTitle(folderTitle.map { LocalizedStringKey($0) } ?? "Drive")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            if store.filter.album != nil {
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
            await store.reload()
            await store.loadAlbums()
        }
        .refreshable {
            await store.reload()
            await store.loadAlbums()
        }
        .fullScreenCover(item: $viewing) { item in
            GalleryViewer(start: item).presentationBackground(.clear)
        }
        .quickLookPreview($preview)
        .fullScreenCover(isPresented: Binding(get: { media.presented }, set: { media.presented = $0 })) { MediaPlayerScreen() }
    }

    private var folderTitle: String? {
        store.filter.album.flatMap { id in store.albums.first { $0.id == id }?.title }
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

    private func folderRow(_ album: GalleryAlbum) -> some View {
        HStack(spacing: 12) {
            Image(systemName: "folder.fill")
                .font(.title2)
                .foregroundStyle(Theme.accent)
                .frame(width: 44, height: 44)
            VStack(alignment: .leading, spacing: 2) {
                Text(album.title).font(LucinaTokens.Fonts.body).foregroundStyle(Theme.text).lineLimit(1)
                Text("\(album.count) items").font(LucinaTokens.Fonts.caption).foregroundStyle(Theme.textSecondary)
            }
        }
    }

    private func fileRow(_ item: GalleryItem) -> some View {
        HStack(spacing: 12) {
            ZStack {
                if item.urls?.grid != nil {
                    RustTile(item: item)
                } else {
                    Rectangle().fill(Theme.field)
                    Image(systemName: Self.icon(item)).foregroundStyle(Theme.textSecondary)
                }
            }
            .frame(width: 44, height: 44)
            .clipShape(RoundedRectangle(cornerRadius: 6, style: .continuous))
            VStack(alignment: .leading, spacing: 2) {
                Text(item.title).font(LucinaTokens.Fonts.body).foregroundStyle(Theme.text).lineLimit(1)
                Text(Self.detail(item)).font(LucinaTokens.Fonts.caption).foregroundStyle(Theme.textSecondary).lineLimit(1)
            }
            Spacer(minLength: 0)
            if opening == item.id { ProgressView() }
        }
    }

    static func icon(_ item: GalleryItem) -> String {
        switch item.kind {
        case "video": "film"
        case "audio": "waveform"
        case "photo": "photo"
        default: "doc"
        }
    }

    /// "1:23:45 · 1.2 GB · 24 Oct 2026"
    static func detail(_ item: GalleryItem) -> String {
        var parts: [String] = []
        if let ms = item.durationMs, item.isVideo || item.isAudio { parts.append(MediaFormat.time(TimeInterval(ms) / 1000)) }
        if let size = item.size { parts.append(ByteCountFormatter.string(fromByteCount: Int64(size), countStyle: .file)) }
        if let date = item.date { parts.append(date.formatted(date: .abbreviated, time: .omitted)) }
        return parts.joined(separator: " · ")
    }

    private func open(_ item: GalleryItem) {
        switch item.kind {
        case "photo", "video":
            viewing = item
        case "audio":
            let playable = store.items.filter { $0.isVideo || $0.isAudio }
            media.play(playable.map(\.mediaItem), startAt: playable.firstIndex { $0.id == item.id } ?? 0)
            media.presented = true
        default:
            guard opening == nil else { return }
            opening = item.id
            Task {
                defer { opening = nil }
                do {
                    preview = try await store.api().downloadOriginal(item)
                } catch {
                    store.error = error.localizedDescription
                }
            }
        }
    }
}
