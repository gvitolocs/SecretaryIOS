import SwiftUI
import UIKit

/// The Plugins navigation stack owns this screen and its pushed profiles.
struct InstagramScreen: View {
    @Environment(AppModel.self) private var app
    @Environment(\.scenePhase) private var scenePhase
    @State private var store: InstagramStore?
    @State private var query = ""
    @State private var username: String?
    @State private var viewer: IgViewerRequest?
    @State private var invalidSearch = false

    var body: some View {
        Group {
            if let store {
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: LucinaTokens.Space.lg) {
                        HStack {
                            TextField("Username or Instagram link", text: $query)
                                .textInputAutocapitalization(.never).autocorrectionDisabled()
                                .submitLabel(.go).onSubmit { submit(store) }
                                .accessibilityIdentifier("instagram-search")
                            Button("Open") { submit(store) }
                                .disabled(store.loading.contains("resolve"))
                                .accessibilityIdentifier("instagram-open")
                        }
                        .padding().background(Theme.card, in: RoundedRectangle(cornerRadius: 16))
                        if invalidSearch { Text("Enter a username or Instagram link.").foregroundStyle(Theme.textSecondary) }
                        if store.loading.contains("resolve") || store.loading.contains("home") { ProgressView() }
                        IgErrors(store: store, keys: ["home", "resolve", "watch", "badges", "settings"]) { await store.loadHome() }
                        HStack {
                            Text("Watched accounts").font(LucinaTokens.Fonts.headline)
                            if store.badges.total > 0 {
                                Text(verbatim: String(store.badges.total)).padding(6).background(Theme.accent, in: Capsule())
                                    .accessibilityIdentifier("instagram-watched-badge")
                            }
                        }
                        if store.watched.isEmpty { Text("No watched accounts yet.").foregroundStyle(Theme.textSecondary) }
                        // Tiles, not rows (owner, 2026-11-01): picture, names, Private, the new count, the last check.
                        LazyVGrid(columns: [GridItem(.adaptive(minimum: 150), spacing: 12)], spacing: 12) {
                            ForEach(store.watched) { person in
                                watchedTile(store, person, badge: store.badges.accounts[person.username] ?? 0)
                            }
                        }
                        Text("Recent profiles").font(LucinaTokens.Fonts.headline)
                        ForEach(store.recent) { person in
                            personRow(store, username: person.username, name: person.fullName, pic: person.picUrl,
                                      verified: person.isVerified)
                        }
                        if store.recent.isEmpty && !store.loading.contains("home") {
                            ContentUnavailableView("Open an Instagram profile", systemImage: "person.crop.square",
                                                   description: Text("Anonymous browsing. Nothing is marked as seen on Instagram."))
                        }
                    }.padding(LucinaTokens.Space.gutter)
                }
                .refreshable { await store.loadHome() }
                .navigationDestination(isPresented: Binding(get: { username != nil }, set: { if !$0 { username = nil } })) {
                    if let username { InstagramProfileView(store: store, username: username) }
                }
                .fullScreenCover(item: $viewer) { request in InstagramViewer(store: store, request: request) }
                .overlay(alignment: .bottom) { IgNotice(store: store) }
            } else { ProgressView() }
        }
        .lucinaScreen("Instagram")
        .task {
            if store == nil {
                store = InstagramStore(api: { app.pluginClient.map { InstagramAPI(client: $0) } }, onSessionEnded: { app.handle($0) })
            }
            openRequestedProfile()
            await store?.loadHome()
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(120))
                guard !Task.isCancelled else { break }
                if scenePhase == .active { await store?.refreshWatching() }
            }
        }
        .onChange(of: scenePhase) { _, phase in
            if phase == .active { Task { await store?.refreshWatching() } }
        }
        .onChange(of: app.openInstagramUser) { _, _ in openRequestedProfile() }
    }

    /// `/instagram/<username>` (a link or Lucina's route): push that profile once the store exists.
    private func openRequestedProfile() {
        guard store != nil, let requested = app.openInstagramUser else { return }
        app.openInstagramUser = nil
        username = requested
    }

    private func submit(_ store: InstagramStore) {
        guard let target = InstagramText.parse(query) else { invalidSearch = true; return }
        invalidSearch = false
        switch target {
        case let .profile(name): username = name
        case let .resolve(url, owner):
            Task {
                if let items = await store.resolve(url), !items.isEmpty {
                    viewer = IgViewerRequest(items: items, start: 0, username: owner ?? "instagram")
                }
            }
        }
    }
    private func watchedTile(_ store: InstagramStore, _ person: IgWatch, badge: Int) -> some View {
        Button { username = person.username } label: {
            VStack(spacing: 6) {
                IgPicture(store: store, path: person.picUrl).frame(width: 68, height: 68).clipShape(Circle())
                    .overlay(alignment: .topTrailing) {
                        if badge > 0 {
                            Text(verbatim: String(badge)).font(.caption2.bold()).foregroundStyle(.white)
                                .padding(.horizontal, 6).padding(.vertical, 2).background(Color.red, in: Capsule()).offset(x: 6, y: -2)
                        }
                    }
                    .padding(.bottom, 4)
                HStack(spacing: 4) {
                    Text(verbatim: person.fullName.isEmpty ? person.username : person.fullName)
                        .font(.subheadline.weight(.semibold)).lineLimit(2).multilineTextAlignment(.center)
                    if person.isVerified { Image(systemName: "checkmark.seal.fill").foregroundStyle(Theme.accent).accessibilityLabel("Verified") }
                }
                Text(verbatim: "@" + person.username).font(.caption).foregroundStyle(Theme.textSecondary).lineLimit(1)
                if person.gone {
                    Text("Gone").font(.caption2)
                } else if person.isPrivate == true {
                    Label("Private", systemImage: "lock.fill").font(.caption2.weight(.semibold))
                        .padding(.horizontal, 8).padding(.vertical, 3).background(Theme.page, in: Capsule())
                }
                Group {
                    if let last = person.lastCheck {
                        Text("Checked \(Date(timeIntervalSince1970: last).formatted(.relative(presentation: .named)))")
                    } else {
                        Text("Not checked yet")
                    }
                }
                .font(.caption2).foregroundStyle(Theme.textSecondary)
            }
            .frame(maxWidth: .infinity).padding(.vertical, 14).padding(.horizontal, 8)
            .background(Theme.card, in: RoundedRectangle(cornerRadius: 16))
        }
        .buttonStyle(.plain).accessibilityIdentifier("instagram-account-" + person.username)
        .contextMenu { personMenu(store, person.username) }
    }
    @ViewBuilder private func personMenu(_ store: InstagramStore, _ name: String) -> some View {
        Button("Open") { username = name }
        if let link = InstagramText.profileLink(name) {
            Button("Copy link") { UIPasteboard.general.url = link; store.notice = String(localized: "Link copied") }
        }
        Button(store.entry(name) == nil ? String(localized: "Watch") : String(localized: "Stop watching")) {
            Task { await store.changeWatch(name) }
        }.disabled(store.changingWatch)
    }
    private func personRow(_ store: InstagramStore, username name: String, name fullName: String, pic: String?,
                           verified: Bool, badge: Int = 0, isPrivate: Bool = false, gone: Bool = false) -> some View {
        Button { username = name } label: {
            HStack(spacing: 12) {
                IgPicture(store: store, path: pic).frame(width: 48, height: 48).clipShape(Circle())
                VStack(alignment: .leading, spacing: 3) {
                    HStack {
                        Text(verbatim: fullName.isEmpty ? name : fullName)
                        if verified { Image(systemName: "checkmark.seal.fill").foregroundStyle(Theme.accent).accessibilityLabel("Verified") }
                    }
                    Text(verbatim: "@" + name).font(.caption).foregroundStyle(Theme.textSecondary)
                    if gone { Text("Gone").font(.caption) }
                    else if isPrivate { Text("Private").font(.caption) }
                }
                Spacer()
                if badge > 0 { Text(verbatim: String(badge)).padding(6).background(Theme.accent, in: Capsule()) }
            }.padding(12).background(Theme.card, in: RoundedRectangle(cornerRadius: 16))
        }
        .buttonStyle(.plain).accessibilityIdentifier("instagram-account-" + name)
        .contextMenu { personMenu(store, name) }
    }
}

private struct InstagramProfileView: View {
    let store: InstagramStore
    let username: String
    @State private var tab: IgTab = PluginScreenshotHooks.instagramTab ?? .posts
    @State private var viewer: IgViewerRequest?
    @State private var picture = false
    /// The highlight opened by `-SecretaryInstagramHighlight first` (screenshots).
    @State private var autoHighlight: IgHighlight?

    var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: LucinaTokens.Space.md) {
                if let profile = store.bundle?.profile, profile.username.lowercased() == username.lowercased() {
                    header(profile)
                    Picker("Content", selection: $tab) {
                        ForEach(IgTab.allCases) { Text($0.title).tag($0) }
                    }.pickerStyle(.segmented).accessibilityIdentifier("instagram-tabs")
                    if tab == .highlights {
                        highlights
                    } else {
                        // Posts stop at the newest twelve (StoryView ignores the cursor): say it against the profile's count.
                        if tab == .posts, let total = store.bundle?.profile.mediaCount,
                           let shown = store.pages[tab.rawValue]?.items.count, shown > 0, total > shown {
                            Text("\(shown) of \(InstagramText.compact(total)) posts").font(.caption).foregroundStyle(Theme.textSecondary)
                        }
                        InstagramTileGrid(store: store, items: store.pages[tab.rawValue]?.items ?? [], username: username,
                                          archived: tab == .archive) { index in
                            viewer = IgViewerRequest(items: store.pages[tab.rawValue]?.items ?? [], start: index,
                                                      username: username, archived: tab == .archive, tab: tab)
                        }
                        if store.pages[tab.rawValue]?.items.isEmpty == true {
                            ContentUnavailableView(tab == .archive ? "Nothing saved from this account yet." : "No items yet", systemImage: "photo.on.rectangle")
                        }
                    }
                    IgErrors(store: store, keys: [tab.rawValue]) { await store.load(tab, username: username, refresh: true) }
                    if store.loading.contains(tab.rawValue) { ProgressView().frame(maxWidth: .infinity) }
                    if store.pages[tab.rawValue]?.nextCursor != nil && store.pages[tab.rawValue]?.exhausted != true {
                        Button("Load more") { Task { await store.load(tab, username: username, more: true) } }
                            .accessibilityIdentifier("instagram-load-more")
                            .onAppear { Task { await store.load(tab, username: username, more: true) } }
                    } else if tab != .highlights, let page = store.pages[tab.rawValue], !page.items.isEmpty,
                              !store.loading.contains(tab.rawValue), store.errors[tab.rawValue] == nil {
                        // The end of the list, said.
                        Group {
                            if tab == .posts, let total = store.bundle?.profile.mediaCount, total > page.items.count {
                                Text("Showing the newest \(page.items.count) of \(total) posts: the anonymous source doesn't give older ones (reels go further).")
                            } else {
                                Text("All \(page.items.count) loaded.")
                            }
                        }
                        .font(.caption).foregroundStyle(Theme.textSecondary).multilineTextAlignment(.center)
                        .frame(maxWidth: .infinity).accessibilityIdentifier("instagram-list-end")
                    }
                }
                if store.loading.contains("profile") { ProgressView().frame(maxWidth: .infinity) }
                IgErrors(store: store, keys: ["profile", "watch", "settings"]) { await store.open(username) }
            }.padding(LucinaTokens.Space.gutter)
        }
        .lucinaScreen("Instagram")
        .refreshable { await store.open(username); await store.load(tab, username: username, refresh: true); await store.refreshWatching() }
        .task(id: username) { await store.open(username); await store.load(tab, username: username) }
        .task(id: tab) { if store.bundle != nil { await store.load(tab, username: username) } }
        .task {
            // The first warmer check lands shortly after Watch. Keep its next-check line current.
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(15))
                guard !Task.isCancelled else { break }
                if store.entry(username)?.lastCheck == nil { await store.refreshWatching() }
            }
        }
        .fullScreenCover(item: $viewer) { InstagramViewer(store: store, request: $0) }
        .fullScreenCover(isPresented: $picture) {
            InstagramProfilePicture(store: store, path: store.bundle?.profile.picUrl)
        }
        .overlay(alignment: .bottom) { IgNotice(store: store) }
    }
    private func header(_ profile: IgProfile) -> some View {
        Card {
            HStack(alignment: .top, spacing: 16) {
                Button { picture = true } label: {
                    IgPicture(store: store, path: profile.picUrl).frame(width: 84, height: 84).clipShape(Circle())
                }.accessibilityLabel("Open profile picture").accessibilityIdentifier("instagram-profile-picture")
                VStack(alignment: .leading, spacing: 6) {
                    HStack {
                        Text(verbatim: profile.fullName.isEmpty ? profile.username : profile.fullName).font(.headline)
                        if profile.isVerified { Image(systemName: "checkmark.seal.fill").foregroundStyle(Theme.accent).accessibilityLabel("Verified") }
                    }
                    Text(verbatim: "@" + profile.username).foregroundStyle(Theme.textSecondary)
                }
            }
            HStack {
                count("Posts", profile.mediaCount)
                count("Followers", profile.followerCount)
                count("Following", profile.followingCount)
            }
            if let bio = profile.biography, !bio.isEmpty { Text(verbatim: bio).textSelection(.enabled) }
            if let link = store.absolute(profile.externalUrl) { Link(destination: link) { Text(verbatim: profile.externalUrl ?? "") }.lineLimit(2) }
            if profile.isPrivate { Label("This account is private.", systemImage: "lock") }
            if store.entry(username)?.gone == true { Text("This account no longer answers.") }
            Button(store.entry(username) == nil ? String(localized: "Watch") : String(localized: "Watching")) {
                Task { await store.changeWatch(username) }
            }.buttonStyle(.borderedProminent).disabled(store.changingWatch).accessibilityIdentifier("instagram-watch")
            if let entry = store.entry(username) {
                Toggle("Save new stories to Lucina", isOn: Binding(get: { store.entry(username)?.autoSave ?? false }, set: { value in
                    Task { await store.changeWatch(username, autoSave: value) }
                })).disabled(store.changingWatch).accessibilityIdentifier("instagram-auto-save")
                Picker("Check", selection: Binding(get: { store.settings.intervalHours }, set: { hours in Task { await store.setInterval(hours) } })) {
                    ForEach(store.settings.choices, id: \.self) { hours in
                        if hours == 23 { Text("About once a day").tag(hours) }
                        else { Text("Every \(hours) hours").tag(hours) }
                    }
                }.disabled(store.loading.contains("settings")).accessibilityIdentifier("instagram-check-interval")
                Text("Applies to all watched accounts.").font(.caption).foregroundStyle(Theme.textSecondary)
                if let seconds = entry.nextCheck {
                    if seconds < Date().timeIntervalSince1970 + 60 { Text("Next check: in a moment.").font(.caption) }
                    else { Text("Next check: \(Date(timeIntervalSince1970: seconds).formatted(date: .abbreviated, time: .shortened))").font(.caption) }
                }
                Text("Nothing is marked as seen on Instagram.").font(.caption).foregroundStyle(Theme.textSecondary)
            }
        }
    }
    private func count(_ title: LocalizedStringKey, _ value: Int) -> some View {
        VStack { Text(verbatim: InstagramText.compact(value)).font(.headline); Text(title).font(.caption) }.frame(maxWidth: .infinity)
    }
    private var highlights: some View {
        LazyVGrid(columns: Array(repeating: GridItem(.flexible(), spacing: 10), count: 3), spacing: 16) {
            ForEach(store.bundle?.highlights ?? []) { highlight in
                NavigationLink {
                    InstagramHighlightView(store: store, username: username, highlight: highlight)
                } label: {
                    VStack {
                        IgPicture(store: store, path: highlight.coverUrl).aspectRatio(1, contentMode: .fit).clipShape(Circle())
                        Text(verbatim: highlight.title).font(.caption).lineLimit(2)
                        if let count = highlight.mediaCount, count > 0 { Text("\(count) items").font(.caption2) }
                    }
                }.buttonStyle(.plain).accessibilityIdentifier("instagram-highlight-" + highlight.id)
            }
            if store.bundle?.highlights.isEmpty == true && !store.loading.contains("highlights") {
                Button("Try again") { Task { await store.load(.highlights, username: username, refresh: true) } }
            }
        }
        .navigationDestination(isPresented: Binding(get: { autoHighlight != nil }, set: { if !$0 { autoHighlight = nil } })) {
            if let autoHighlight { InstagramHighlightView(store: store, username: username, highlight: autoHighlight) }
        }
        .onChange(of: store.bundle?.highlights.first?.id, initial: true) { _, id in
            guard id != nil, PluginScreenshotHooks.opensFirstHighlight, autoHighlight == nil else { return }
            autoHighlight = store.bundle?.highlights.first
        }
    }
}

private struct InstagramHighlightView: View {
    let store: InstagramStore
    let username: String
    let highlight: IgHighlight
    @State private var viewer: IgViewerRequest?
    var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 12) {
                HStack {
                    IgPicture(store: store, path: highlight.coverUrl).frame(width: 60, height: 60).clipShape(Circle())
                    VStack(alignment: .leading) {
                        Text(verbatim: highlight.title).font(.headline)
                        Text("\(store.pages[highlight.id]?.items.count ?? 0) items").font(.caption)
                    }
                }.padding(.horizontal, LucinaTokens.Space.gutter)
                InstagramTileGrid(store: store, items: store.pages[highlight.id]?.items ?? [], username: username) { index in
                    viewer = IgViewerRequest(items: store.pages[highlight.id]?.items ?? [], start: index, username: username, highlight: highlight)
                }
                .padding(.horizontal, LucinaTokens.Space.gutter)
                IgErrors(store: store, keys: [highlight.id]) { await load(refresh: true) }
                if store.loading.contains(highlight.id) { ProgressView().frame(maxWidth: .infinity) }
                if store.pages[highlight.id]?.items.isEmpty == true { ContentUnavailableView("No items yet", systemImage: "photo") }
                if store.pages[highlight.id]?.nextCursor != nil {
                    Button("Load more") { Task { await load(more: true) } }.onAppear { Task { await load(more: true) } }
                }
            }.padding(.vertical)
        }.lucinaScreen("Highlights")
            .task {
                await load()
                let items = store.pages[highlight.id]?.items ?? []
                if PluginScreenshotHooks.opensHighlightStory, viewer == nil, !items.isEmpty {
                    viewer = IgViewerRequest(items: items, start: 0, username: username, highlight: highlight)
                }
            }
            .refreshable { await load(refresh: true) }
            .fullScreenCover(item: $viewer) { InstagramViewer(store: store, request: $0) }
            .overlay(alignment: .bottom) { IgNotice(store: store) }
    }
    private func load(more: Bool = false, refresh: Bool = false) async {
        await store.load(.highlights, username: username, more: more, refresh: refresh, highlight: highlight)
    }
}

private struct InstagramTileGrid: View {
    let store: InstagramStore
    let items: [IgItem]
    let username: String
    var archived = false
    let open: (Int) -> Void
    var body: some View {
        let chips = InstagramText.monthChips(items)
        LazyVGrid(columns: Array(repeating: GridItem(.flexible(), spacing: 3), count: 3), spacing: 3) {
            ForEach(Array(items.enumerated()), id: \.offset) { index, item in
                Button { open(index) } label: {
                    // As on the web (InstagramTiles): the month chip bottom-left, the play badge top-right.
                    Color.clear.aspectRatio(1, contentMode: .fit)
                        .overlay { IgPicture(store: store, path: item.thumbUrl ?? item.url) }
                        .clipShape(RoundedRectangle(cornerRadius: 3))
                        .overlay(alignment: .bottomLeading) {
                            if let chip = chips[index] {
                                Text(verbatim: chip).font(.caption2.bold()).foregroundStyle(.white)
                                    .padding(.horizontal, 7).padding(.vertical, 3).background(.black.opacity(0.65), in: Capsule()).padding(6)
                            }
                        }
                        .overlay(alignment: .topTrailing) {
                            if item.kind == "video" || item.kind == "carousel" {
                                Image(systemName: item.kind == "video" ? "play.fill" : "square.on.square.fill")
                                    .font(.caption).padding(6).background(.black.opacity(0.5), in: Circle()).padding(4)
                            }
                        }
                }
                .buttonStyle(.plain).accessibilityLabel("Open item \(index + 1)")
                .accessibilityIdentifier("instagram-tile-\(index)")
                .contextMenu {
                    Button("Open") { open(index) }
                    Button("Copy image") { Task { await copy(item) } }
                    if let url = item.permalink.flatMap({ store.absolute($0) }) {
                        Button("Copy link") { UIPasteboard.general.url = url; store.notice = String(localized: "Link copied") }
                        ShareLink(item: url) { Label("Share", systemImage: "square.and.arrow.up") }
                    } else if let url = store.absolute(item.url) { ShareLink(item: url) { Label("Share", systemImage: "square.and.arrow.up") } }
                    if !archived {
                        Button("Save to Lucina") { Task { await store.save(item, username: username) } }.disabled(store.saving)
                    }
                }
            }
        }
    }
    private func copy(_ item: IgItem) async {
        if let image = await InstagramPictureLoader.image(store: store, path: item.thumbUrl ?? item.url) {
            UIPasteboard.general.image = image; store.notice = String(localized: "Image copied")
        } else { store.notice = String(localized: "Couldn't copy the image.") }
    }
}

struct IgErrors: View {
    let store: InstagramStore
    let keys: [String]
    let retry: () async -> Void
    var body: some View {
        ForEach(keys, id: \.self) { key in
            if let error = store.errors[key] {
                VStack(alignment: .leading, spacing: 8) {
                    Text(verbatim: error).foregroundStyle(Theme.textSecondary)
                    Button("Try again") { Task { await retry() } }.accessibilityIdentifier("instagram-retry-" + key)
                }
            }
        }
    }
}
struct IgNotice: View {
    let store: InstagramStore
    var body: some View {
        if let notice = store.notice {
            Text(verbatim: notice).font(.callout).padding().background(Theme.card, in: Capsule()).padding()
                .accessibilityIdentifier("instagram-notice")
                .task(id: notice) {
                    try? await Task.sleep(for: .seconds(3))
                    if !Task.isCancelled && store.notice == notice { store.notice = nil }
                }
        }
    }
}

/// Gallery archive thumbnails need the device token; signed plugin media do not.
@MainActor enum InstagramPictureLoader {
    static func image(store: InstagramStore, path: String?) async -> UIImage? {
        guard let path, let url = store.absolute(path) else { return nil }
        if path.hasPrefix("/v1/media/"), let client = store.client {
            guard let response = try? await client.send("GET", path, body: nil) else { return nil }
            return UIImage(data: response.data)
        }
        return await SignedImageLoader.shared.image(url)
    }
}
struct IgPicture: View {
    let store: InstagramStore
    let path: String?
    @State private var image: UIImage?
    var body: some View {
        Group {
            if path?.hasPrefix("/v1/media/") == true {
                if let image { Image(uiImage: image).resizable().scaledToFill() }
                else { SignedImagePlaceholder() }
            } else { SignedImage(url: store.absolute(path)) }
        }
        .task(id: path) {
            if path?.hasPrefix("/v1/media/") == true { image = await InstagramPictureLoader.image(store: store, path: path) }
        }
    }
}
