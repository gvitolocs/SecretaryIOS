import SwiftUI

/// Films (owner, 2026-10-26): the film folders on the server (`scope=films`, the "Film" and "Dansk
/// film" folders), apart from the Gallery (phone backups) and the Drive (the archive). Continue
/// watching comes first; a tap plays from the saved minute.
struct FilmsView: View {
    @Environment(MediaPlayer.self) private var media
    @Environment(GalleryStore.self) private var gallery
    @State private var folders: [Folder] = []
    @State private var query = ""
    @State private var loading = false
    @State private var loadError: String?

    struct Folder: Identifiable {
        let id: String
        let title: String
        let films: [MediaItem]
    }

    var body: some View {
        @Bindable var media = media
        ScrollView {
            LazyVStack(alignment: .leading, spacing: LucinaTokens.Space.lg) {
                if let loadError {
                    Text(loadError).font(LucinaTokens.Fonts.footnote).foregroundStyle(Theme.textSecondary)
                        .padding(.horizontal, LucinaTokens.Space.gutter)
                }
                let resume = query.isEmpty ? media.continueItems(in: allFilms) : []
                if !resume.isEmpty {
                    SectionHeader("Continue watching").padding(.horizontal, LucinaTokens.Space.gutter)
                    ScrollView(.horizontal, showsIndicators: false) {
                        LazyHStack(spacing: LucinaTokens.Space.gridGap) {
                            ForEach(resume) { film in
                                FilmCard(film: film, showsRemaining: true) { play(film) }
                                    .frame(width: 260)
                            }
                        }
                        .padding(.horizontal, LucinaTokens.Space.gutter)
                    }
                }
                ForEach(shownFolders) { folder in
                    SectionHeader(LocalizedStringKey(folder.title)) {
                        Text("\(folder.films.count)").font(LucinaTokens.Fonts.footnote).foregroundStyle(Theme.textSecondary)
                    }
                    .padding(.horizontal, LucinaTokens.Space.gutter)
                    LazyVGrid(columns: [GridItem(.adaptive(minimum: 160), spacing: LucinaTokens.Space.gridGap)],
                              alignment: .leading, spacing: LucinaTokens.Space.lg) {
                        ForEach(folder.films) { film in
                            FilmCard(film: film) { play(film) }
                        }
                    }
                    .padding(.horizontal, LucinaTokens.Space.gutter)
                }
                if folders.isEmpty, !loading, loadError == nil {
                    ContentUnavailableView("No films yet", systemImage: "film",
                                           description: Text("Films in the Film and Dansk film folders on the server show up here."))
                }
            }
            .padding(.bottom, LucinaTokens.Space.xxl)
        }
        .lucinaScreen("Films")
        .overlay { if loading && folders.isEmpty { ProgressView() } }
        .searchable(text: $query, prompt: Text("Search films"))
        .safeAreaInset(edge: .bottom, spacing: 0) {
            if media.current != nil, !media.presented { MiniPlayer() }
        }
        .task {
            await load()
            #if DEBUG || SCREENSHOTS
            // Not cancelled when the player cover hides this screen.
            Task { @MainActor in
                await MediaSelfTest.runIfRequested(media: media, items: allFilms)
                await MediaSelfTest.timeToFirstFrame(media: media, items: allFilms)
            }
            #endif
        }
        .task { await media.syncProgress() }
        .refreshable {
            await load()
            await media.syncProgress()
        }
        .fullScreenCover(isPresented: $media.presented) { MediaPlayerScreen() }
    }

    private var allFilms: [MediaItem] {
        var seen = Set<String>()
        return folders.flatMap(\.films).filter { seen.insert($0.id).inserted }
    }

    private var shownFolders: [Folder] {
        let q = query.trimmingCharacters(in: .whitespaces)
        guard !q.isEmpty else { return folders }
        return folders.map { Folder(id: $0.id, title: $0.title, films: $0.films.filter { $0.title.localizedCaseInsensitiveContains(q) }) }
            .filter { !$0.films.isEmpty }
    }

    /// One film at a time: the next file in a folder is another film, not a next episode.
    private func play(_ film: MediaItem, fromStart: Bool = false) {
        media.play([film], startAt: 0, fromStart: fromStart)
        media.presented = true
    }

    /// The film folders (user albums in `scope=films`, biggest first), then their videos.
    private func load() async {
        loading = true
        defer { loading = false }
        do {
            let api = gallery.api()
            let albums = try await api.albums(scope: "films").filter { $0.systemKey == nil && $0.count > 0 }
            var loaded: [Folder] = []
            for album in albums.sorted(by: { $0.count > $1.count }) {
                var films: [MediaItem] = []
                var cursor: String?
                repeat {
                    let page = try await api.items(GalleryAPI.Filter(scope: "films", kind: "video", album: album.id), cursor: cursor)
                    films += page.items.map(\.mediaItem)
                    cursor = page.nextCursor
                } while cursor != nil && films.count < 2000
                films.sort { $0.title.localizedStandardCompare($1.title) == .orderedAscending }
                if !films.isEmpty { loaded.append(Folder(id: album.id, title: album.title, films: films)) }
            }
            folders = loaded
            loadError = nil
        } catch is CancellationError {
        } catch {
            loadError = error.localizedDescription
        }
    }
}

/// A film: its poster frame (16:9) with the saved progress along the bottom, the title, and the
/// edition after " - " ("Dansk dub + Dansk subs") as a second line.
private struct FilmCard: View {
    @Environment(MediaPlayer.self) private var media
    let film: MediaItem
    var showsRemaining = false
    let play: () -> Void

    var body: some View {
        Button(action: play) {
            VStack(alignment: .leading, spacing: LucinaTokens.Space.xs) {
                Poster(path: film.posterUrl, kind: film.kind)
                    .aspectRatio(16 / 9, contentMode: .fit)
                    .clipShape(RoundedRectangle(cornerRadius: LucinaTokens.Radius.tile))
                    .overlay(alignment: .bottom) { progressBar }
                    .overlay(alignment: .topTrailing) { badge }
                Text(name.title).font(LucinaTokens.Fonts.subheadline.weight(.semibold)).foregroundStyle(Theme.text).lineLimit(2)
                Text(detail).font(LucinaTokens.Fonts.caption2).foregroundStyle(Theme.textSecondary).lineLimit(1)
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .contextMenu {
            Button { play() } label: { Label(media.progress(of: film) == nil ? "Play" : "Resume", systemImage: "play.fill") }
            if media.progress(of: film) != nil || media.isFinished(film) {
                Button {
                    media.play([film], startAt: 0, fromStart: true)
                    media.presented = true
                } label: { Label("Play from the beginning", systemImage: "backward.end.fill") }
            }
        }
        .accessibilityIdentifier("film-\(film.id)")
    }

    private var name: (title: String, edition: String?) {
        guard let range = film.title.range(of: " - ") else { return (film.title, nil) }
        return (String(film.title[..<range.lowerBound]), String(film.title[range.upperBound...]))
    }

    private var detail: String {
        var parts: [String] = []
        if showsRemaining, let p = media.progress(of: film), let d = film.duration {
            parts.append(String(localized: "\(MediaFormat.time(d * (1 - p))) left"))
        } else if let d = film.duration {
            parts.append(MediaFormat.time(d))
        }
        if let edition = name.edition { parts.append(edition) }
        return parts.joined(separator: " · ")
    }

    @ViewBuilder private var progressBar: some View {
        if let p = media.progress(of: film) {
            GeometryReader { geo in
                ZStack(alignment: .leading) {
                    Rectangle().fill(.black.opacity(0.45))
                    Rectangle().fill(Theme.accent).frame(width: geo.size.width * p)
                }
            }
            .frame(height: 4)
            .clipShape(UnevenRoundedRectangle(bottomLeadingRadius: LucinaTokens.Radius.tile, bottomTrailingRadius: LucinaTokens.Radius.tile))
            .accessibilityLabel(Text("\(Int(p * 100)) percent watched"))
        }
    }

    @ViewBuilder private var badge: some View {
        if media.current?.id == film.id {
            Image(systemName: media.isPlaying ? "speaker.wave.2.fill" : "pause.fill")
                .font(LucinaTokens.Fonts.caption).foregroundStyle(.white)
                .padding(6).background(.black.opacity(0.55), in: Circle()).padding(6)
        } else if media.isFinished(film) {
            Image(systemName: "checkmark.circle.fill")
                .font(LucinaTokens.Fonts.callout).foregroundStyle(.white, Theme.accentFill)
                .padding(6)
                .accessibilityLabel("Watched")
        }
    }
}
