import SwiftUI

// One Drive folder level (`GET /v1/media/drive/browse`): the breadcrumb bar, the type chips, the
// subfolders, then the files; a search in the folder's subtree; the sort menu (shared by every
// level); paging by `next_offset`. Subfolders push another level (DriveNavigator).

/// A folder's listing, page by page. Answers to a query that is no longer current are dropped.
@Observable
@MainActor
final class DriveBrowseModel {
    private(set) var folders: [DriveFolder] = []
    private(set) var files: [GalleryItem] = []
    private(set) var crumbs: [DriveCrumb]
    private(set) var total: Int?
    private(set) var nextOffset: Int?
    private(set) var loading = false
    var error: String?
    /// The query whose answer is on screen.
    private(set) var shown: DriveQuery?
    @ObservationIgnored private var requested: DriveQuery?
    let location: DriveLocation

    init(location: DriveLocation) {
        self.location = location
        crumbs = DriveBreadcrumbs.build(folder: location.folder, path: location.path)
    }

    /// Loads `query` unless it is what is on screen (coming back from a subfolder).
    func ensure(_ query: DriveQuery, api: GalleryAPI) async {
        guard shown != query || error != nil else { return }
        await reload(query, api: api)
    }

    func reload(_ query: DriveQuery, api: GalleryAPI) async {
        requested = query
        loading = true
        defer { if requested == query { loading = false } }
        do {
            let page = try await api.browse(query)
            guard requested == query else { return }
            folders = page.folders
            files = page.files
            total = page.total
            nextOffset = page.nextOffset
            crumbs = DriveBreadcrumbs.resolve(page.breadcrumbs, folder: location.folder, path: location.path)
            shown = query
            error = nil
        } catch is CancellationError {
        } catch {
            guard requested == query else { return }
            if shown != query {
                folders = []
                files = []
                total = nil
                nextOffset = nil
            }
            self.error = error.localizedDescription
        }
    }

    func loadMore(api: GalleryAPI) async {
        guard let next = nextOffset, let base = shown, !loading else { return }
        var query = base
        query.offset = next
        loading = true
        defer { loading = false }
        guard let page = try? await api.browse(query), shown == base else { return }
        let known = Set(files.map(\.id))
        files += page.files.filter { !known.contains($0.id) }
        nextOffset = page.nextOffset
    }
}

struct DriveFolderView: View {
    @Environment(GalleryStore.self) private var store
    @Environment(DriveNavigator.self) private var nav
    @Environment(MediaPlayer.self) private var media
    let location: DriveLocation
    /// This level's index in the navigator's stack.
    let depth: Int
    @State private var model: DriveBrowseModel
    @State private var type: DriveFileType?
    @State private var search = ""
    @State private var query = ""
    @State private var previewer = DrivePreviewer()

    init(location: DriveLocation, depth: Int) {
        self.location = location
        self.depth = depth
        _model = State(initialValue: DriveBrowseModel(location: location))
    }

    private var current: DriveQuery {
        DriveQuery(folder: location.folder, path: location.path, sort: nav.sort, order: nav.order, type: type, q: query)
    }

    private var searching: Bool { !current.trimmedQuery.isEmpty }

    var body: some View {
        List {
            Section {
                if !searching, !model.folders.isEmpty {
                    ForEach(model.folders) { folder in
                        Button {
                            nav.push(.folder(DriveLocation(folder: location.folder, path: folder.path)), at: depth + 1)
                        } label: {
                            DriveFolderRow(name: folder.name, detail: DriveFormat.folderDetail(files: folder.files, bytes: folder.bytes))
                        }
                        .accessibilityIdentifier("drive-dir-\(folder.path)")
                    }
                }
                ForEach(model.files) { item in
                    Button { previewer.open(item, among: model.files, api: store.api, media: media) } label: {
                        DriveFileRow(item: item, showsLocation: searching, opening: previewer.opening == item.id)
                    }
                    .accessibilityIdentifier("drive-\(item.id)")
                }
                if model.nextOffset != nil {
                    ProgressView().frame(maxWidth: .infinity).task { await model.loadMore(api: store.api()) }
                }
            } header: {
                VStack(alignment: .leading, spacing: LucinaTokens.Space.xs) {
                    DriveTypeChips(selection: $type)
                    if let total = model.total, model.shown == current, total > 0 {
                        Text(searching ? String(localized: "\(total) results") : String(localized: "\(total) files"))
                            .font(LucinaTokens.Fonts.caption)
                            .foregroundStyle(Theme.textSecondary)
                    }
                }
                .textCase(nil)
            }
            if let error = model.error {
                Section { DriveErrorRow(message: error) { Task { await model.reload(current, api: store.api()) } } }
            }
        }
        .lucinaList()
        .safeAreaInset(edge: .top, spacing: 0) {
            DriveBreadcrumbBar(crumbs: model.crumbs, root: { nav.stack = [] }) { crumb in
                nav.jump(to: DriveLocation(folder: location.folder, path: crumb.path))
            }
        }
        .overlay {
            if model.files.isEmpty, model.folders.isEmpty || searching, model.error == nil {
                if model.loading || model.shown != current {
                    ProgressView()
                } else if searching {
                    ContentUnavailableView.search(text: query)
                } else if type == nil {
                    ContentUnavailableView("This folder is empty", systemImage: "folder")
                } else {
                    ContentUnavailableView("No files of this type here", systemImage: "folder")
                }
            }
        }
        .navigationTitle(location.name)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                DriveSortMenu(sort: current.effectiveSort, order: current.effectiveOrder) { sort in
                    nav.sort = sort
                    nav.order = nil
                } setOrder: { order in
                    nav.order = order
                }
            }
        }
        .searchable(text: $search, prompt: Text("Search in \(location.name)"))
        .onSubmit(of: .search) { query = search }
        .onChange(of: search) { _, new in if new.isEmpty, !query.isEmpty { query = "" } }
        .task(id: current) { await model.ensure(current, api: store.api()) }
        .refreshable { await model.reload(current, api: store.api()) }
        .driveDestination(nav, store: store, at: depth + 1)
        .drivePreviews(previewer, playsMedia: nav.stack.count == depth + 1)
    }
}

// MARK: - Rows and bars

/// The breadcrumb bar: the Drive, then each folder down to this one; scrolls to the end.
struct DriveBreadcrumbBar: View {
    let crumbs: [DriveCrumb]
    let root: () -> Void
    let jump: (DriveCrumb) -> Void

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: LucinaTokens.Space.xs) {
                    Button(action: root) {
                        Image(systemName: "externaldrive").font(LucinaTokens.Fonts.subheadline)
                    }
                    .accessibilityLabel("Drive")
                    .accessibilityIdentifier("drive-crumb-root")
                    ForEach(Array(crumbs.enumerated()), id: \.offset) { index, crumb in
                        Image(systemName: "chevron.right")
                            .font(.caption2.weight(.semibold))
                            .foregroundStyle(Theme.textTertiary)
                            .accessibilityHidden(true)
                        if index == crumbs.count - 1 {
                            Text(verbatim: crumb.name)
                                .font(LucinaTokens.Fonts.subheadline.weight(.semibold))
                                .foregroundStyle(Theme.text)
                                .lineLimit(1)
                                .id(index)
                        } else {
                            Button { jump(crumb) } label: {
                                Text(verbatim: crumb.name).font(LucinaTokens.Fonts.subheadline).lineLimit(1)
                            }
                            .id(index)
                            .accessibilityIdentifier("drive-crumb-\(index)")
                        }
                    }
                }
                .padding(.horizontal, LucinaTokens.Space.gutter)
                .padding(.vertical, LucinaTokens.Space.sm)
            }
            .onAppear { proxy.scrollTo(crumbs.count - 1, anchor: .trailing) }
            .onChange(of: crumbs) { _, new in proxy.scrollTo(new.count - 1, anchor: .trailing) }
        }
        .tint(Theme.accent)
        .background(.bar)
    }
}

/// All, then one chip per file type.
struct DriveTypeChips: View {
    @Binding var selection: DriveFileType?

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: LucinaTokens.Space.sm) {
                ChipButton(title: "All", isOn: selection == nil) { selection = nil }
                    .accessibilityIdentifier("drive-type-all")
                ForEach(DriveFileType.allCases, id: \.self) { type in
                    ChipButton(title: type.chipTitle, icon: type.symbol, isOn: selection == type) {
                        selection = selection == type ? nil : type
                    }
                    .accessibilityIdentifier("drive-type-\(type.rawValue)")
                }
            }
            .padding(.vertical, 4)
        }
        .textCase(nil)
        .listRowInsets(EdgeInsets())
    }
}

/// Name / modified / size / type, ascending or descending.
struct DriveSortMenu: View {
    let sort: DriveSort
    let order: DriveOrder
    let setSort: (DriveSort) -> Void
    let setOrder: (DriveOrder) -> Void

    var body: some View {
        Menu {
            Picker("Sort by", selection: Binding(get: { sort }, set: setSort)) {
                ForEach(DriveSort.allCases, id: \.self) { sort in
                    Text(sort.title).tag(sort)
                }
            }
            Picker("Order", selection: Binding(get: { order }, set: setOrder)) {
                Label("Ascending", systemImage: "arrow.up").tag(DriveOrder.asc)
                Label("Descending", systemImage: "arrow.down").tag(DriveOrder.desc)
            }
        } label: {
            Image(systemName: "arrow.up.arrow.down")
        }
        .accessibilityLabel("Sort")
        .accessibilityIdentifier("drive-sort")
    }
}

struct DriveFolderRow: View {
    let name: String
    let detail: String
    var symbol = "folder.fill"

    var body: some View {
        HStack(spacing: LucinaTokens.Space.md) {
            Image(systemName: symbol)
                .font(.title2)
                .foregroundStyle(Theme.accent)
                .frame(width: 44, height: 44)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 2) {
                Text(verbatim: name).font(LucinaTokens.Fonts.body).foregroundStyle(Theme.text).lineLimit(1)
                if !detail.isEmpty {
                    Text(verbatim: detail).font(LucinaTokens.Fonts.caption).foregroundStyle(Theme.textSecondary).lineLimit(1)
                }
            }
            Spacer(minLength: 0)
            Image(systemName: "chevron.right")
                .font(.caption.weight(.semibold))
                .foregroundStyle(Theme.textTertiary)
                .accessibilityHidden(true)
        }
        .contentShape(Rectangle())
    }
}

/// The file's thumbnail (a PDF's first page, a photo) or its type's icon.
struct DriveFileIcon: View {
    let item: GalleryItem
    var size: CGFloat = 44

    var body: some View {
        let type = DriveFileType(item: item)
        ZStack {
            if item.urls?.grid != nil {
                RustTile(item: item)
            } else {
                Rectangle().fill(type.tint.opacity(0.14))
                Image(systemName: type.symbol)
                    .font(.system(size: size * 0.42, weight: .medium))
                    .foregroundStyle(type.tint)
            }
        }
        .frame(width: size, height: size)
        .clipShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
        .accessibilityHidden(true)
    }
}

/// Icon, name, (search) folder and matching text, size · modified.
struct DriveFileRow: View {
    let item: GalleryItem
    let showsLocation: Bool
    let opening: Bool

    var body: some View {
        HStack(spacing: LucinaTokens.Space.md) {
            DriveFileIcon(item: item)
            VStack(alignment: .leading, spacing: 2) {
                Text(verbatim: item.driveName)
                    .font(LucinaTokens.Fonts.body)
                    .foregroundStyle(Theme.text)
                    .lineLimit(1)
                    .truncationMode(.middle)
                if showsLocation, let location = DriveFormat.location(item) {
                    Label { Text(verbatim: location) } icon: { Image(systemName: "folder") }
                        .labelStyle(.titleAndIcon)
                        .font(LucinaTokens.Fonts.caption2)
                        .foregroundStyle(Theme.textSecondary)
                        .lineLimit(1)
                        .truncationMode(.head)
                }
                if let snippet = item.snippet, !snippet.isEmpty {
                    Text(verbatim: snippet)
                        .font(LucinaTokens.Fonts.caption2)
                        .foregroundStyle(Theme.textSecondary)
                        .lineLimit(2)
                }
                Text(verbatim: DriveFormat.detail(item))
                    .font(LucinaTokens.Fonts.caption)
                    .foregroundStyle(Theme.textSecondary)
                    .lineLimit(1)
            }
            Spacer(minLength: 0)
            if opening { ProgressView() }
        }
        .contentShape(Rectangle())
    }
}

/// A load error with Retry.
struct DriveErrorRow: View {
    let message: String
    let retry: () -> Void

    var body: some View {
        HStack(spacing: LucinaTokens.Space.md) {
            Image(systemName: "exclamationmark.triangle").foregroundStyle(Theme.warning)
            Text(verbatim: message)
                .font(LucinaTokens.Fonts.caption)
                .foregroundStyle(Theme.textSecondary)
            Spacer(minLength: 0)
            Button("Retry", action: retry)
                .font(LucinaTokens.Fonts.subheadline.weight(.semibold))
                .buttonStyle(.borderless)
        }
    }
}
