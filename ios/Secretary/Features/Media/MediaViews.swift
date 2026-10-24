import AVKit
import SwiftUI

// The media library (videos, audio, photos on the owner's server) and the player.
// Video plays in AVPlayerViewController (AirPlay, Picture in Picture, native scrubbing);
// audio gets its own screen; both continue in the background and on the lock screen.

/// Media opens from a button next to the chat title, like Notes and Tasks.
private struct MediaEntry: ViewModifier {
    @Environment(AppModel.self) private var app

    func body(content: Content) -> some View {
        @Bindable var app = app
        content
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button { app.showMedia = true } label: { Image(systemName: "play.rectangle") }
                        .accessibilityLabel("Media")
                        .accessibilityIdentifier("mediaButton")
                }
            }
            .navigationDestination(isPresented: $app.showMedia) { MediaLibraryView() }
    }
}

extension View {
    /// The Media button and library, on the chat.
    func mediaEntry() -> some View { modifier(MediaEntry()) }
}

struct MediaLibraryView: View {
    enum Kind: String, CaseIterable, Identifiable {
        case video, audio, photo
        var id: String { rawValue }
        var label: LocalizedStringKey {
            switch self {
            case .video: "Videos"
            case .audio: "Audio"
            case .photo: "Photos"
            }
        }
    }

    @Environment(MediaPlayer.self) private var media
    @State private var kind: Kind = .video
    @State private var items: [MediaItem] = []
    @State private var query = ""
    @State private var loading = false
    @State private var loadError: String?
    @State private var photo: MediaItem?

    var body: some View {
        @Bindable var media = media
        Group {
            if kind == .photo {
                photoGrid
            } else {
                list
            }
        }
        .navigationTitle("Media")
        .navigationBarTitleDisplayMode(.inline)
        .searchable(text: $query, prompt: Text("Search"))
        .safeAreaInset(edge: .top, spacing: 0) {
            Picker("Kind", selection: $kind) {
                ForEach(Kind.allCases) { Text($0.label).tag($0) }
            }
            .pickerStyle(.segmented)
            .padding(.horizontal)
            .padding(.vertical, 6)
            .background(.bar)
        }
        .safeAreaInset(edge: .bottom, spacing: 0) {
            if media.current != nil, !media.presented { MiniPlayer() }
        }
        .task(id: kind) { await load() }
        .task { await media.syncProgress() }
        .refreshable {
            await load()
            await media.syncProgress()
        }
        .fullScreenCover(isPresented: $media.presented) { MediaPlayerScreen() }
        .fullScreenCover(item: $photo) { item in
            PhotoViewer(items: filtered, start: item)
        }
    }

    private var filtered: [MediaItem] {
        let q = query.trimmingCharacters(in: .whitespaces)
        guard !q.isEmpty else { return items }
        return items.filter { $0.title.localizedCaseInsensitiveContains(q) || ($0.album ?? "").localizedCaseInsensitiveContains(q) }
    }

    private var albums: [(String, [MediaItem])] {
        var order: [String] = []
        var groups: [String: [MediaItem]] = [:]
        for item in filtered {
            let album = item.album ?? ""
            if groups[album] == nil { order.append(album) }
            groups[album, default: []].append(item)
        }
        return order.map { ($0, groups[$0] ?? []) }
    }

    private var list: some View {
        List {
            if let loadError {
                Section { Text(loadError).foregroundStyle(.secondary) }
            }
            let resume = query.isEmpty ? media.continueItems(in: items) : []
            if !resume.isEmpty {
                Section("Continue") {
                    ForEach(resume) { item in row(item, in: resume) }
                }
            }
            ForEach(albums, id: \.0) { album, entries in
                Section(album.isEmpty ? "Library" : album) {
                    ForEach(entries) { item in row(item, in: entries) }
                }
            }
            if items.isEmpty, !loading, loadError == nil {
                ContentUnavailableView(kind == .video ? "No videos yet" : "No audio yet", systemImage: kind == .video ? "film" : "waveform")
            }
        }
        .overlay { if loading && items.isEmpty { ProgressView() } }
    }

    private func row(_ item: MediaItem, in context: [MediaItem]) -> some View {
        Button {
            media.play(context, startAt: context.firstIndex(of: item) ?? 0)
            media.presented = true
        } label: {
            HStack(spacing: 12) {
                Poster(path: item.posterUrl, kind: item.kind)
                    .frame(width: item.isVideo ? 96 : 54, height: 54)
                    .clipShape(RoundedRectangle(cornerRadius: 6))
                VStack(alignment: .leading, spacing: 4) {
                    Text(item.title).font(.subheadline.weight(.medium)).lineLimit(2).foregroundStyle(.primary)
                    HStack(spacing: 6) {
                        if let d = item.duration { Text(MediaFormat.time(d)) }
                        if media.isFinished(item) { Image(systemName: "checkmark.circle.fill").accessibilityLabel("Watched") }
                        if media.current?.id == item.id { Image(systemName: media.isPlaying ? "speaker.wave.2.fill" : "pause.fill") }
                    }
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    if let p = media.progress(of: item) {
                        ProgressView(value: p).tint(.accentColor).frame(maxWidth: 160)
                    }
                }
            }
        }
        .accessibilityIdentifier("media-\(item.id)")
    }

    private var photoGrid: some View {
        ScrollView {
            LazyVGrid(columns: [GridItem(.adaptive(minimum: 104), spacing: 2)], spacing: 2) {
                ForEach(filtered) { item in
                    Button { photo = item } label: {
                        Poster(path: item.thumbUrl, kind: item.kind)
                            .aspectRatio(1, contentMode: .fill)
                            .clipped()
                    }
                }
            }
            if items.isEmpty, !loading {
                ContentUnavailableView("No photos yet", systemImage: "photo")
            }
        }
    }

    private func load() async {
        loading = true
        defer { loading = false }
        do {
            var all: [MediaItem] = []
            var cursor: String?
            repeat {
                let page = try await media.api().items(kind: kind.rawValue, cursor: cursor)
                all += page.items
                cursor = page.nextCursor
            } while cursor != nil && all.count < 2000
            items = all
            loadError = nil
        } catch is CancellationError {
        } catch {
            loadError = error.localizedDescription
        }
    }
}

/// A poster or thumbnail from a tokenised URL (no auth header needed).
struct Poster: View {
    @Environment(MediaPlayer.self) private var media
    let path: String?
    let kind: String

    var body: some View {
        AsyncImage(url: path.flatMap { media.api().absolute($0) }) { phase in
            if let image = phase.image {
                image.resizable().scaledToFill()
            } else {
                ZStack {
                    Rectangle().fill(.quaternary)
                    Image(systemName: kind == "audio" ? "waveform" : kind == "photo" ? "photo" : "film")
                        .foregroundStyle(.secondary)
                }
            }
        }
    }
}

/// The bar above the bottom edge while something plays and the player is closed.
private struct MiniPlayer: View {
    @Environment(MediaPlayer.self) private var media

    var body: some View {
        if let item = media.current {
            HStack(spacing: 12) {
                Poster(path: item.posterUrl, kind: item.kind)
                    .frame(width: 44, height: 44)
                    .clipShape(RoundedRectangle(cornerRadius: 6))
                VStack(alignment: .leading, spacing: 2) {
                    Text(item.title).font(.subheadline.weight(.medium)).lineLimit(1)
                    Text("\(MediaFormat.time(media.position)) / \(MediaFormat.time(media.duration))")
                        .font(.caption.monospacedDigit()).foregroundStyle(.secondary)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .contentShape(Rectangle())
                .onTapGesture { media.presented = true }
                Button { media.togglePlay() } label: {
                    Image(systemName: media.isPlaying ? "pause.fill" : "play.fill").font(.title3)
                }
                .accessibilityLabel(media.isPlaying ? "Pause" : "Play")
                Button { media.stop() } label: { Image(systemName: "xmark").font(.body.weight(.semibold)) }
                    .foregroundStyle(.secondary)
                    .accessibilityLabel("Close")
            }
            .padding(.horizontal)
            .padding(.vertical, 8)
            .background(.bar)
        }
    }
}

struct MediaPlayerScreen: View {
    @Environment(MediaPlayer.self) private var media
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        Group {
            if media.current?.isVideo == true {
                videoScreen
            } else {
                AudioPlayerScreen()
            }
        }
        .onAppear { if media.current?.isVideo == true { OrientationLock.set(.allButUpsideDown) } }
        .onDisappear { OrientationLock.set(.portrait) }
    }

    private var videoScreen: some View {
        ZStack(alignment: .top) {
            Color.black.ignoresSafeArea()
            VideoSurface(media: media).ignoresSafeArea()
            if media.loading {
                ProgressView().tint(.white).controlSize(.large).frame(maxHeight: .infinity)
            }
            if let error = media.error {
                Text(error).font(.callout).foregroundStyle(.white).padding().background(.black.opacity(0.7), in: RoundedRectangle(cornerRadius: 10))
                    .frame(maxHeight: .infinity)
            }
            HStack {
                Button { dismiss() } label: {
                    Image(systemName: "chevron.down").font(.body.weight(.semibold)).padding(10)
                }
                .accessibilityLabel("Close")
                .accessibilityIdentifier("closePlayer")
                Spacer()
                SubtitleMenu()
            }
            .foregroundStyle(.white)
            .background(.black.opacity(0.35), in: Capsule())
            .padding(.horizontal, 60)
            .padding(.top, 4)
        }
    }
}

private struct SubtitleMenu: View {
    @Environment(MediaPlayer.self) private var media

    var body: some View {
        Menu {
            Button { media.selectSubtitle(nil) } label: {
                if media.subtitle == nil { Label("Off", systemImage: "checkmark") } else { Text("Off") }
            }
            ForEach(media.subtitleTracks) { track in
                Button { media.selectSubtitle(track) } label: {
                    if media.subtitle?.id == track.id { Label(track.displayName, systemImage: "checkmark") } else { Text(track.displayName) }
                }
            }
            if let bitmap = media.info?.bitmapSubtitles, bitmap > 0 {
                Text("\(bitmap) image-based subtitle tracks can't be shown yet")
            }
            Divider()
            Menu("Speed") {
                ForEach([Float(0.75), 1, 1.25, 1.5, 2], id: \.self) { r in
                    Button { media.setRate(r) } label: {
                        let text = r == 1 ? "1×" : "\(r.formatted())×"
                        if media.rate == r { Label(text, systemImage: "checkmark") } else { Text(text) }
                    }
                }
            }
        } label: {
            Image(systemName: media.subtitle == nil ? "captions.bubble" : "captions.bubble.fill").font(.body.weight(.semibold)).padding(10)
        }
        .accessibilityLabel("Subtitles and speed")
    }
}

/// AVPlayerViewController with the subtitle overlay in its content overlay view.
private struct VideoSurface: UIViewControllerRepresentable {
    let media: MediaPlayer

    func makeUIViewController(context: Context) -> AVPlayerViewController {
        let controller = AVPlayerViewController()
        controller.player = media.player
        controller.allowsPictureInPicturePlayback = true
        controller.canStartPictureInPictureAutomaticallyFromInline = true
        // MediaPlayer keeps the lock screen up to date (title, artwork, queue).
        controller.updatesNowPlayingInfoCenter = false
        controller.videoGravity = .resizeAspect
        if let overlay = controller.contentOverlayView {
            let host = UIHostingController(rootView: SubtitleOverlay().environment(media))
            host.view.backgroundColor = .clear
            host.view.isUserInteractionEnabled = false
            host.view.frame = overlay.bounds
            host.view.autoresizingMask = [.flexibleWidth, .flexibleHeight]
            controller.addChild(host)
            overlay.addSubview(host.view)
            host.didMove(toParent: controller)
        }
        return controller
    }

    func updateUIViewController(_ controller: AVPlayerViewController, context: Context) {}
}

private struct SubtitleOverlay: View {
    @Environment(MediaPlayer.self) private var media

    var body: some View {
        VStack {
            Spacer()
            if let text = media.cueText {
                Text(text)
                    .font(.system(size: 20, weight: .semibold))
                    .multilineTextAlignment(.center)
                    .foregroundStyle(.white)
                    .shadow(color: .black, radius: 2)
                    .padding(.horizontal, 10)
                    .padding(.vertical, 4)
                    .background(.black.opacity(0.45), in: RoundedRectangle(cornerRadius: 6))
                    .padding(.bottom, 64)
                    .padding(.horizontal, 24)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

private struct AudioPlayerScreen: View {
    @Environment(MediaPlayer.self) private var media
    @Environment(\.dismiss) private var dismiss
    @State private var scrubbing: Double?

    var body: some View {
        VStack(spacing: 24) {
            HStack {
                Button { dismiss() } label: { Image(systemName: "chevron.down").font(.title3.weight(.semibold)) }
                    .accessibilityLabel("Close")
                    .accessibilityIdentifier("closePlayer")
                Spacer()
                RoutePicker().frame(width: 32, height: 32)
            }
            Spacer(minLength: 0)
            if let item = media.current {
                Poster(path: item.posterUrl, kind: item.kind)
                    .aspectRatio(1, contentMode: .fit)
                    .clipShape(RoundedRectangle(cornerRadius: 16))
                    .shadow(radius: 12, y: 6)
                    .frame(maxWidth: 320)
                VStack(spacing: 4) {
                    Text(item.title).font(.title3.weight(.semibold)).multilineTextAlignment(.center)
                    if let album = item.album { Text(album).font(.subheadline).foregroundStyle(.secondary) }
                }
            }
            if let error = media.error {
                Text(error).font(.callout).foregroundStyle(.red).multilineTextAlignment(.center)
            }
            VStack(spacing: 4) {
                Slider(value: Binding(get: { scrubbing ?? media.position }, set: { scrubbing = $0 }),
                       in: 0...max(media.duration, 1)) { editing in
                    if !editing, let target = scrubbing {
                        media.seek(to: target)
                        scrubbing = nil
                    }
                }
                .accessibilityIdentifier("audioScrubber")
                HStack {
                    Text(MediaFormat.time(scrubbing ?? media.position))
                    Spacer()
                    Text("-" + MediaFormat.time(max(0, media.duration - (scrubbing ?? media.position))))
                }
                .font(.caption.monospacedDigit())
                .foregroundStyle(.secondary)
            }
            HStack(spacing: 28) {
                Button { media.previous() } label: { Image(systemName: "backward.end.fill") }
                    .accessibilityLabel("Previous")
                Button { media.skip(by: -15) } label: { Image(systemName: "gobackward.15") }
                    .accessibilityLabel("Back 15 seconds")
                Button { media.togglePlay() } label: {
                    Image(systemName: media.isPlaying ? "pause.circle.fill" : "play.circle.fill").font(.system(size: 64))
                }
                .accessibilityLabel(media.isPlaying ? "Pause" : "Play")
                .accessibilityIdentifier("playPause")
                Button { media.skip(by: 15) } label: { Image(systemName: "goforward.15") }
                    .accessibilityLabel("Forward 15 seconds")
                Button { media.next() } label: { Image(systemName: "forward.end.fill") }
                    .disabled(!media.hasNext)
                    .accessibilityLabel("Next")
            }
            .font(.title2)
            .overlay { if media.loading { ProgressView().offset(y: 52) } }
            Menu {
                ForEach([Float(0.75), 1, 1.25, 1.5, 2], id: \.self) { r in
                    Button(r == 1 ? "1×" : "\(r.formatted())×") { media.setRate(r) }
                }
            } label: {
                Text(media.rate == 1 ? "1×" : "\(media.rate.formatted())×").font(.callout.weight(.semibold).monospacedDigit())
            }
            Spacer(minLength: 0)
        }
        .padding(24)
    }
}

private struct RoutePicker: UIViewRepresentable {
    func makeUIView(context: Context) -> AVRoutePickerView {
        let view = AVRoutePickerView()
        view.prioritizesVideoDevices = false
        return view
    }

    func updateUIView(_ uiView: AVRoutePickerView, context: Context) {}
}

/// Full-screen photos, swiped through. Originals load from the item's `file` URL.
private struct PhotoViewer: View {
    @Environment(MediaPlayer.self) private var media
    @Environment(\.dismiss) private var dismiss
    let items: [MediaItem]
    @State private var selection: String

    init(items: [MediaItem], start: MediaItem) {
        self.items = items
        _selection = State(initialValue: start.id)
    }

    var body: some View {
        ZStack(alignment: .topLeading) {
            Color.black.ignoresSafeArea()
            TabView(selection: $selection) {
                ForEach(items) { item in
                    AsyncImage(url: originalURL(item)) { phase in
                        if let image = phase.image {
                            image.resizable().scaledToFit()
                        } else if phase.error != nil {
                            Image(systemName: "exclamationmark.triangle").foregroundStyle(.white)
                        } else {
                            ProgressView().tint(.white)
                        }
                    }
                    .tag(item.id)
                }
            }
            .tabViewStyle(.page(indexDisplayMode: .never))
            .ignoresSafeArea()
            Button { dismiss() } label: {
                Image(systemName: "xmark").font(.body.weight(.semibold)).foregroundStyle(.white).padding(12)
                    .background(.black.opacity(0.4), in: Circle())
            }
            .padding()
            .accessibilityLabel("Close")
        }
    }

    /// `/v1/media/s/{token}/{id}/thumb.jpg` → `/v1/media/s/{token}/{id}/file` (same token).
    private func originalURL(_ item: MediaItem) -> URL? {
        guard let thumb = item.thumbUrl, let slash = thumb.lastIndex(of: "/") else { return nil }
        return media.api().absolute(String(thumb[..<slash]) + "/file")
    }
}
