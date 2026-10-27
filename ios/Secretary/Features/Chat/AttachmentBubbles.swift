import SwiftUI
import AVFoundation
import MapKit
import QuickLook

/// One attachment of a message, with its position among the file parts (local previews are keyed by it).
struct BubbleEntry: Identifiable, Hashable {
    let id: Int
    let attachment: TimelineAttachment
    let fileIndex: Int?

    static func entries(_ attachments: [TimelineAttachment]) -> [BubbleEntry] {
        var fileIndex = 0
        return attachments.enumerated().map { offset, attachment in
            defer { if attachment.isFilePart { fileIndex += 1 } }
            return BubbleEntry(id: offset, attachment: attachment, fileIndex: attachment.isFilePart ? fileIndex : nil)
        }
    }

    var displayName: String {
        if let name = attachment.name, !name.isEmpty { return name }
        return attachment.kind.capitalized
    }

    /// "PDF", "MP3"…
    var typeLabel: String {
        let ext = (displayName as NSString).pathExtension
        if !ext.isEmpty { return ext.uppercased() }
        return AttachmentKind(rawValue: attachment.normalizedKind)?.rawValue.capitalized ?? attachment.kind.capitalized
    }

    var sizeText: String? { attachment.bytes.map { ByteCountFormatter.string(fromByteCount: $0, countStyle: .file) } }
}

/// The attachments of one chat message: photos and videos as one rounded bubble (a single picture or an
/// album mosaic, with the caption under it), then audio players, file cards, links and locations.
struct AttachmentMessage: View {
    @Environment(ChatStore.self) private var store
    @Environment(MediaPlayer.self) private var media
    let item: TimelineItem
    let isUser: Bool
    @State private var viewerStart: ViewerStart?
    @State private var showPlayer = false

    struct ViewerStart: Identifiable {
        let index: Int
        var id: Int { index }
    }

    private var entries: [BubbleEntry] { BubbleEntry.entries(item.attachments) }
    private var visual: [BubbleEntry] { entries.filter { $0.attachment.isVisual } }
    private var others: [BubbleEntry] { entries.filter { !$0.attachment.isVisual } }
    private var shareId: String? { item.meta?.shareId }
    private var uploading: Double? { item.localStatus == .sending ? shareId.flatMap { store.shareProgress[$0] } : nil }

    var body: some View {
        VStack(alignment: isUser ? .trailing : .leading, spacing: 6) {
            if !visual.isEmpty { mediaBubble }
            ForEach(others) { entry in
                card(entry)
            }
        }
        .fullScreenCover(item: $viewerStart) { start in
            ChatMediaViewer(entries: visual.filter { $0.attachment.isImage }, shareId: shareId, start: start.index)
        }
        .fullScreenCover(isPresented: $showPlayer, onDismiss: { media.stop() }) { MediaPlayerScreen() }
    }

    private var mediaBubble: some View {
        let first = visual.first?.attachment
        let aspect: CGFloat? = {
            if let w = first?.width, let h = first?.height, w > 0, h > 0 { return CGFloat(w) / CGFloat(h) }
            if let hash = first?.thumbhash, let ratio = ThumbHash.aspectRatio(base64: hash) { return CGFloat(ratio) }
            if let image = ChatPreviews.image(shareId: shareId, index: visual.first?.fileIndex ?? 0), image.size.height > 0 {
                return image.size.width / image.size.height
            }
            return nil
        }()
        let layout = AlbumLayout.layout(count: visual.count, aspect: aspect)
        return VStack(alignment: .leading, spacing: 0) {
            ZStack(alignment: .topLeading) {
                ForEach(Array(visual.enumerated()), id: \.element.id) { index, entry in
                    let frame = layout.frames[min(index, layout.frames.count - 1)]
                    MediaTile(entry: entry, shareId: shareId)
                        .frame(width: frame.width, height: frame.height)
                        .clipped()
                        .contentShape(Rectangle())
                        .onTapGesture { open(entry) }
                        .offset(x: frame.minX, y: frame.minY)
                        .accessibilityAddTraits(.isButton)
                }
            }
            .frame(width: layout.size.width, height: layout.size.height, alignment: .topLeading)
            .overlay {
                if let uploading { UploadRing(progress: uploading, size: 46) }
            }
            if !item.displayText.isEmpty {
                Text(isUser ? LinkText.attributed(item.displayText) : ChatMarkdown.inline(item.displayText))
                    .font(.body)
                    .foregroundStyle(isUser ? Theme.onAccent : Theme.text)
                    .tint(isUser ? Theme.onAccent : Theme.accent)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 8)
                    .frame(width: layout.size.width, alignment: .leading)
                    .accessibilityIdentifier(isUser ? "userMessage" : "lucinaMessage")
            }
        }
        .background(isUser ? Theme.accentFill : Theme.card)
        .clipShape(RoundedRectangle(cornerRadius: Theme.cornerRadius, style: .continuous))
        .opacity(item.localStatus == .sending ? 0.85 : 1)
    }

    private func open(_ entry: BubbleEntry) {
        if entry.attachment.isVideo {
            guard let id = entry.attachment.mediaId else { return }
            let video = MediaItem(id: id, kind: "video", title: entry.displayName, mime: entry.attachment.mime,
                                  durationMs: entry.attachment.durationMs, width: entry.attachment.width, height: entry.attachment.height)
            media.play([video], startAt: 0)
            showPlayer = true
        } else {
            let images = visual.filter { $0.attachment.isImage }
            viewerStart = ViewerStart(index: images.firstIndex(of: entry) ?? 0)
        }
    }

    @ViewBuilder private func card(_ entry: BubbleEntry) -> some View {
        let kind = entry.attachment.normalizedKind
        if entry.attachment.isVoice {
            VoiceNoteBubble(item: item, entry: entry, shareId: shareId, isUser: isUser)
        } else if entry.attachment.isAudio {
            AudioBubble(entry: entry, shareId: shareId, isUser: isUser)
        } else if kind == "link", let url = entry.attachment.url.flatMap(ShareItemReader.webURL) {
            ChatLinkCard(entry: entry, url: url, isUser: isUser)
        } else if kind == "location", let coordinate = entry.attachment.coordinate {
            LocationCard(name: entry.attachment.name, lat: coordinate.lat, lon: coordinate.lon, isUser: isUser)
        } else if kind == "text" {
            EmptyView()
        } else {
            FileCard(entry: entry, shareId: shareId, isUser: isUser)
        }
    }
}

/// The circular upload progress on an outgoing bubble.
struct UploadRing: View {
    let progress: Double
    var size: CGFloat = 22

    var body: some View {
        ZStack {
            Circle().fill(.black.opacity(0.45))
            Circle().stroke(.white.opacity(0.3), lineWidth: size / 12)
                .padding(size / 6)
            Circle().trim(from: 0, to: max(0.03, progress))
                .stroke(.white, style: StrokeStyle(lineWidth: size / 12, lineCap: .round))
                .rotationEffect(.degrees(-90))
                .padding(size / 6)
                .animation(.linear(duration: 0.2), value: progress)
        }
        .frame(width: size, height: size)
        .accessibilityElement()
        .accessibilityLabel(Text("Sending…"))
        .accessibilityValue(Text("\(Int((progress * 100).rounded())) %"))
    }
}

// MARK: - Photos and videos

/// One picture of a bubble: the local preview when this phone sent it, else the ThumbHash placeholder at
/// once, then the grid rendition (JPEG XL), then the JPEG thumb.
struct MediaTile: View {
    @Environment(AppModel.self) private var app
    let entry: BubbleEntry
    let shareId: String?
    @State private var image: UIImage?
    @State private var thumbhash: String?

    init(entry: BubbleEntry, shareId: String?) {
        self.entry = entry
        self.shareId = shareId
        _image = State(initialValue: entry.fileIndex.flatMap { ChatPreviews.image(shareId: shareId, index: $0) })
        _thumbhash = State(initialValue: entry.attachment.thumbhash)
    }

    var body: some View {
        ZStack {
            Theme.field
            if let image {
                Image(uiImage: image).resizable().scaledToFill()
            } else if let thumbhash, let placeholder = ThumbHash.image(base64: thumbhash) {
                Image(uiImage: placeholder).resizable().scaledToFill()
            } else {
                Image(systemName: entry.attachment.isVideo ? "video" : "photo")
                    .font(.title2)
                    .foregroundStyle(Theme.textSecondary)
            }
            if entry.attachment.isVideo {
                Image(systemName: "play.fill")
                    .font(.system(size: 20, weight: .semibold))
                    .foregroundStyle(.white)
                    .frame(width: 44, height: 44)
                    .background(.black.opacity(0.45), in: Circle())
                if let ms = entry.attachment.durationMs {
                    Text(AttachmentTrayCard.duration(ms))
                        .font(.caption2.monospacedDigit().weight(.semibold))
                        .foregroundStyle(.white)
                        .padding(.horizontal, 6)
                        .padding(.vertical, 2)
                        .background(.black.opacity(0.55), in: Capsule())
                        .padding(6)
                        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .bottomLeading)
                }
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text(label))
        .task(id: entry.attachment.mediaId) { await load() }
    }

    private var label: String {
        if entry.attachment.isVideo {
            let duration = entry.attachment.durationMs.map { AttachmentTrayCard.duration($0) } ?? ""
            return String(localized: "Video, \(entry.displayName) \(duration)")
        }
        return String(localized: "Photo, \(entry.displayName)")
    }

    private func load() async {
        guard image == nil, let id = entry.attachment.mediaId, !id.isEmpty else { return }
        var info: ChatMediaInfo?
        // Renditions are made in the background after an upload: try again a few times.
        for attempt in 0..<4 {
            if thumbhash == nil || info == nil {
                info = await ChatMedia.info(id, client: app.client)
                if thumbhash == nil { thumbhash = info?.thumbhash }
            }
            if let loaded = await ChatMedia.image(id, rendition: .grid, info: info, client: app.client) {
                image = loaded
                return
            }
            if Task.isCancelled { return }
            try? await Task.sleep(for: .seconds(2 << attempt))
        }
    }
}

/// Full screen, swipe between the album's photos; share and save the one on screen.
struct ChatMediaViewer: View {
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss
    let entries: [BubbleEntry]
    let shareId: String?
    @State private var index: Int
    @State private var loaded: [Int: UIImage] = [:]
    @State private var notice: String?
    @State private var saving = false

    init(entries: [BubbleEntry], shareId: String?, start: Int) {
        self.entries = entries
        self.shareId = shareId
        _index = State(initialValue: start)
    }

    var body: some View {
        NavigationStack {
            TabView(selection: $index) {
                ForEach(Array(entries.enumerated()), id: \.offset) { offset, entry in
                    ViewerPage(entry: entry, shareId: shareId) { image in loaded[offset] = image }
                        .tag(offset)
                }
            }
            .tabViewStyle(.page(indexDisplayMode: entries.count > 1 ? .automatic : .never))
            .background(Color.black.ignoresSafeArea())
            .overlay(alignment: .bottom) {
                if let notice {
                    Text(notice)
                        .font(.footnote.weight(.semibold))
                        .padding(.horizontal, 14)
                        .padding(.vertical, 8)
                        .background(.regularMaterial, in: Capsule())
                        .padding(.bottom, 24)
                        .transition(.opacity)
                }
            }
            .toolbarBackground(.visible, for: .navigationBar, .bottomBar)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Close") { dismiss() } }
                ToolbarItem(placement: .principal) {
                    if entries.count > 1 { Text("\(index + 1) of \(entries.count)").font(.subheadline.weight(.semibold)) }
                }
                ToolbarItemGroup(placement: .bottomBar) {
                    if let image = loaded[index] {
                        ShareLink(item: Image(uiImage: image), preview: SharePreview(currentName, image: Image(uiImage: image))) {
                            Image(systemName: "square.and.arrow.up")
                        }
                        .accessibilityLabel(Text("Share"))
                    }
                    Spacer()
                    Button { Task { await save() } } label: {
                        if saving { ProgressView() } else { Image(systemName: "square.and.arrow.down") }
                    }
                    .disabled(saving || (loaded[index] == nil && currentEntry?.attachment.mediaId == nil))
                    .accessibilityLabel(Text("Save to Photos"))
                }
            }
        }
        .preferredColorScheme(.dark)
    }

    private var currentEntry: BubbleEntry? { entries.indices.contains(index) ? entries[index] : nil }
    private var currentName: String { currentEntry?.displayName ?? String(localized: "Photo") }

    /// The original when the server has it (full quality), else what is on screen.
    private func save() async {
        guard let entry = currentEntry else { return }
        saving = true
        defer { saving = false }
        var file: URL?
        if let id = entry.attachment.mediaId {
            file = try? await ChatMedia.original(id, name: entry.attachment.name, info: nil, client: app.client)
        }
        let ok = await PhotoSaver.save(file: file, image: loaded[index])
        withAnimation { notice = ok ? String(localized: "Saved to Photos") : String(localized: "Couldn't save to Photos") }
        try? await Task.sleep(for: .seconds(2))
        withAnimation { notice = nil }
    }
}

private struct ViewerPage: View {
    @Environment(AppModel.self) private var app
    let entry: BubbleEntry
    let shareId: String?
    let onLoad: (UIImage) -> Void
    @State private var image: UIImage?
    @State private var placeholder: UIImage?

    var body: some View {
        ZStack {
            if let shown = image ?? placeholder {
                Image(uiImage: shown).resizable().scaledToFit().frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                ProgressView().tint(.white)
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text("Photo, \(entry.displayName)"))
        .task(id: entry.attachment.mediaId) {
            let local = entry.fileIndex.flatMap { ChatPreviews.image(shareId: shareId, index: $0) }
            placeholder = local ?? entry.attachment.thumbhash.flatMap(ThumbHash.image(base64:))
            if let local, entry.attachment.mediaId == nil { image = local; onLoad(local); return }
            guard let id = entry.attachment.mediaId else { return }
            let info = await ChatMedia.info(id, client: app.client)
            if placeholder == nil {
                placeholder = await ChatMedia.image(id, rendition: .grid, info: info, client: app.client)
                    ?? info?.thumbhash.flatMap(ThumbHash.image(base64:))
            }
            if let full = await ChatMedia.image(id, rendition: .preview, info: info, client: app.client) {
                image = full
                onLoad(full)
            } else if let placeholder {
                onLoad(placeholder)
            }
        }
    }
}

// MARK: - Audio

/// One inline audio player for the whole chat: the bubble that plays shows its progress.
@Observable
@MainActor
final class ChatAudio {
    static let shared = ChatAudio()

    private(set) var currentKey: String?
    private(set) var loadingKey: String?
    private(set) var isPlaying = false
    private(set) var position: Double = 0
    private(set) var duration: Double = 0
    /// Voice notes' speed (1×, 1.5×, 2×), for every bubble like Telegram.
    private(set) var rate: Float = 1
    /// Why the last play didn't start (a recording has the microphone).
    private(set) var notice: String?
    @ObservationIgnored private let player = AVPlayer()
    @ObservationIgnored private var timeObserver: Any?
    @ObservationIgnored private var endObserver: NSObjectProtocol?

    private init() {
        timeObserver = player.addPeriodicTimeObserver(forInterval: CMTime(value: 1, timescale: 5), queue: .main) { [weak self] time in
            MainActor.assumeIsolated {
                guard let self else { return }
                self.position = time.seconds.isFinite ? time.seconds : 0
                if let seconds = self.player.currentItem?.duration.seconds, seconds.isFinite, seconds > 0 { self.duration = seconds }
                self.isPlaying = self.player.timeControlStatus != .paused
            }
        }
    }

    static let rates: [Float] = [1, 1.5, 2]

    /// 1× → 1.5× → 2× → 1×.
    func cycleRate() {
        let index = Self.rates.firstIndex(of: rate) ?? 0
        rate = Self.rates[(index + 1) % Self.rates.count]
        player.defaultRate = rate
        if isPlaying { player.rate = rate }
    }

    /// Jumps within the bubble that is playing (0...1).
    func seek(_ key: String, fraction: Double) {
        guard key == currentKey, duration > 0 else { return }
        let target = max(0, min(1, fraction)) * duration
        position = target
        player.seek(to: CMTime(seconds: target, preferredTimescale: 600))
    }

    /// Stops whatever plays (a voice note is about to be recorded, or Lucina is about to speak).
    func stop() {
        guard currentKey != nil || isPlaying else { return }
        player.pause()
        player.replaceCurrentItem(with: nil)
        currentKey = nil
        loadingKey = nil
        isPlaying = false
        position = 0
        deactivate()
    }

    func toggle(_ key: String, resolve: @escaping () async -> URL?) {
        // Playing would take the audio session from the recorder (the memo or the voice note would stop).
        if MemoRecorder.isAnyActive, !(key == currentKey && isPlaying) {
            notice = String(localized: "Finish the recording first.")
            return
        }
        notice = nil
        // One sound at a time: Lucina's voice (a reply, a message read aloud) stops.
        stopOthers()
        if key == currentKey, player.currentItem != nil {
            if isPlaying { player.pause() } else { activate(); player.play() }
            isPlaying.toggle()
            return
        }
        player.pause()
        currentKey = key
        loadingKey = key
        position = 0
        duration = 0
        Task {
            let url = await resolve()
            guard currentKey == key else { return }
            loadingKey = nil
            guard let url, !MemoRecorder.isAnyActive else { currentKey = nil; return }
            let item = AVPlayerItem(url: url)
            if let endObserver { NotificationCenter.default.removeObserver(endObserver) }
            endObserver = NotificationCenter.default.addObserver(forName: .AVPlayerItemDidPlayToEndTime, object: item, queue: .main) { [weak self] _ in
                MainActor.assumeIsolated {
                    self?.player.seek(to: .zero)
                    self?.isPlaying = false
                    self?.position = 0
                    self?.deactivate()
                }
            }
            item.audioTimePitchAlgorithm = .spectral
            player.replaceCurrentItem(with: item)
            player.defaultRate = rate
            activate()
            player.play()
            isPlaying = true
        }
    }

    private func activate() {
        try? Self.activate(spoken: false)
    }

    private func deactivate() {
        Self.deactivate()
    }

    // MARK: The chat's other players

    private struct Registered {
        weak var owner: AnyObject?
        let stop: @MainActor () -> Void
    }

    @ObservationIgnored private var others: [Registered] = []

    /// Lucina's voice replies and "Read aloud" register here, so whichever starts stops the rest.
    func register(_ owner: AnyObject, stop: @escaping @MainActor () -> Void) {
        others.removeAll { $0.owner == nil || $0.owner === owner }
        others.append(Registered(owner: owner, stop: stop))
    }

    /// Stops everything that plays in the chat except `owner` (nil: everything, before a recording starts).
    func stopAll(except owner: AnyObject? = nil) {
        if owner !== self { stop() }
        for other in others where other.owner != nil && other.owner !== owner { other.stop() }
    }

    private func stopOthers() {
        for other in others where other.owner != nil { other.stop() }
    }

    // MARK: Audio session

    // The one place the chat touches AVAudioSession, so a shared app-wide helper can take it over later.

    /// Playback (`.spokenAudio` for Lucina's voice, `.default` for voice notes and audio files). No Now
    /// Playing info is set: these are short clips inside the chat.
    static func activate(spoken: Bool) throws {
        let session = AVAudioSession.sharedInstance()
        try session.setCategory(.playback, mode: spoken ? .spokenAudio : .default)
        try session.setActive(true)
    }

    /// Hands the session back (other apps' audio resumes; the recorder sets its own category when it starts).
    static func deactivate() {
        guard !MemoRecorder.isAnyActive else { return }
        try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
    }

    /// "playback/spokenAudio": the self-test's log line.
    static var sessionDescription: String {
        let session = AVAudioSession.sharedInstance()
        return "\(session.category.rawValue)/\(session.mode.rawValue)"
    }
}

struct AudioBubble: View {
    @Environment(AppModel.self) private var app
    @Environment(ChatStore.self) private var store
    let entry: BubbleEntry
    let shareId: String?
    let isUser: Bool
    private var audio: ChatAudio { ChatAudio.shared }
    private var key: String { entry.attachment.mediaId ?? "\(shareId ?? "")#\(entry.fileIndex ?? 0)" }
    private var active: Bool { audio.currentKey == key }
    private var foreground: Color { isUser ? Theme.onAccent : Theme.text }

    var body: some View {
        HStack(spacing: 10) {
            Button { audio.toggle(key, resolve: resolve) } label: {
                ZStack {
                    Circle().fill(isUser ? Color.white.opacity(0.22) : Theme.accentSoft)
                    if audio.loadingKey == key {
                        ProgressView().tint(foreground)
                    } else {
                        Image(systemName: active && audio.isPlaying ? "pause.fill" : "play.fill")
                            .font(.system(size: 16, weight: .semibold))
                            .foregroundStyle(isUser ? Theme.onAccent : Theme.accent)
                    }
                }
                .frame(width: 40, height: 40)
            }
            .buttonStyle(.plain)
            .disabled(!playable)
            .accessibilityLabel(Text(active && audio.isPlaying ? "Pause" : "Play"))
            VStack(alignment: .leading, spacing: 5) {
                ProgressView(value: progress)
                    .tint(isUser ? Theme.onAccent : Theme.accent)
                HStack {
                    Text(entry.displayName).lineLimit(1).truncationMode(.middle)
                    Spacer(minLength: 6)
                    Text(timeText).monospacedDigit()
                }
                .font(.caption)
                .foregroundStyle(foreground.opacity(0.85))
            }
        }
        .padding(10)
        .frame(width: 250)
        .background(isUser ? Theme.accentFill : Theme.card, in: RoundedRectangle(cornerRadius: Theme.cornerRadius, style: .continuous))
        .accessibilityElement(children: .contain)
        .accessibilityLabel(Text("Audio, \(entry.displayName), \(timeText)"))
    }

    private var playable: Bool { entry.attachment.mediaId != nil || store.localFile(shareId: shareId, fileIndex: entry.fileIndex ?? -1) != nil }

    private var progress: Double {
        guard active, audio.duration > 0 else { return 0 }
        return min(1, audio.position / audio.duration)
    }

    private var timeText: String {
        let total = active && audio.duration > 0 ? audio.duration : Double(entry.attachment.durationMs ?? 0) / 1000
        if active, audio.position > 0 { return MediaFormat.time(audio.position) + " / " + MediaFormat.time(total) }
        return total > 0 ? MediaFormat.time(total) : "--:--"
    }

    private func resolve() async -> URL? {
        if let local = store.localFile(shareId: shareId, fileIndex: entry.fileIndex ?? -1) { return local }
        guard let id = entry.attachment.mediaId else { return nil }
        return await ChatMedia.playURL(id, client: app.client)
    }
}

// MARK: - Files, links, locations

/// A document, archive, code file, contact or calendar: type icon, name, size and type; tap opens Quick Look.
struct FileCard: View {
    @Environment(AppModel.self) private var app
    @Environment(ChatStore.self) private var store
    let entry: BubbleEntry
    let shareId: String?
    let isUser: Bool
    @State private var preview: URL?
    @State private var loading = false
    @State private var failed = false

    var body: some View {
        Button(action: open) {
            HStack(spacing: 12) {
                ZStack {
                    RoundedRectangle(cornerRadius: LucinaTokens.Radius.field, style: .continuous)
                        .fill(isUser ? Color.white.opacity(0.2) : Theme.accentSoft)
                    if loading {
                        ProgressView().tint(isUser ? Theme.onAccent : Theme.accent)
                    } else {
                        Image(systemName: TimelineAttachment.symbol(for: entry.attachment.normalizedKind))
                            .font(.title3)
                            .foregroundStyle(isUser ? Theme.onAccent : Theme.accent)
                    }
                }
                .frame(width: 44, height: 44)
                VStack(alignment: .leading, spacing: 3) {
                    Text(entry.displayName)
                        .font(.subheadline.weight(.semibold))
                        .lineLimit(2)
                        .truncationMode(.middle)
                        .multilineTextAlignment(.leading)
                    Text(failed ? String(localized: "Couldn't open this file.") : detail)
                        .font(.caption)
                        .opacity(0.8)
                }
                .foregroundStyle(isUser ? Theme.onAccent : Theme.text)
                Spacer(minLength: 0)
            }
            .padding(10)
            .frame(width: 250, alignment: .leading)
            .background(isUser ? Theme.accentFill : Theme.card, in: RoundedRectangle(cornerRadius: Theme.cornerRadius, style: .continuous))
        }
        .buttonStyle(.plain)
        .disabled(loading)
        .quickLookPreview($preview)
        .accessibilityLabel(Text("\(entry.typeLabel), \(entry.displayName), \(entry.sizeText ?? "")"))
        .accessibilityHint(Text("Opens a preview"))
    }

    private var detail: String {
        [entry.typeLabel, entry.sizeText].compactMap { $0 }.joined(separator: " · ")
    }

    private func open() {
        if let local = store.localFile(shareId: shareId, fileIndex: entry.fileIndex ?? -1) {
            preview = local
            return
        }
        guard let id = entry.attachment.mediaId else { return }
        loading = true
        failed = false
        Task {
            do {
                preview = try await ChatMedia.original(id, name: entry.attachment.name, info: nil, client: app.client)
            } catch {
                failed = true
            }
            loading = false
        }
    }
}

struct ChatLinkCard: View {
    let entry: BubbleEntry
    let url: URL
    let isUser: Bool

    var body: some View {
        Link(destination: url) {
            HStack(spacing: 12) {
                Image(systemName: "link")
                    .font(.title3)
                    .foregroundStyle(isUser ? Theme.onAccent : Theme.accent)
                    .frame(width: 44, height: 44)
                    .background(isUser ? Color.white.opacity(0.2) : Theme.accentSoft,
                                in: RoundedRectangle(cornerRadius: LucinaTokens.Radius.field, style: .continuous))
                VStack(alignment: .leading, spacing: 3) {
                    Text(entry.attachment.name?.isEmpty == false ? entry.attachment.name! : (url.host ?? url.absoluteString))
                        .font(.subheadline.weight(.semibold))
                        .lineLimit(2)
                        .multilineTextAlignment(.leading)
                    Text(url.host ?? url.absoluteString).font(.caption).opacity(0.8).lineLimit(1)
                }
                .foregroundStyle(isUser ? Theme.onAccent : Theme.text)
                Spacer(minLength: 0)
            }
            .padding(10)
            .frame(width: 250, alignment: .leading)
            .background(isUser ? Theme.accentFill : Theme.card, in: RoundedRectangle(cornerRadius: Theme.cornerRadius, style: .continuous))
        }
        .accessibilityLabel(Text("Link, \(entry.attachment.name ?? url.host ?? url.absoluteString)"))
    }
}

/// A shared location: a map snapshot (light or dark like the app) and its name; tap opens Maps.
struct LocationCard: View {
    @Environment(\.colorScheme) private var colorScheme
    @Environment(\.openURL) private var openURL
    let name: String?
    let lat: Double
    let lon: Double
    let isUser: Bool
    @State private var snapshot: UIImage?

    var body: some View {
        Button(action: openMaps) {
            VStack(alignment: .leading, spacing: 0) {
                ZStack {
                    Theme.field
                    if let snapshot { Image(uiImage: snapshot).resizable().scaledToFill() }
                    Image(systemName: "mappin.circle.fill")
                        .font(.system(size: 30))
                        .symbolRenderingMode(.palette)
                        .foregroundStyle(.white, Theme.accentFill)
                        .shadow(radius: 2)
                }
                .frame(width: 250, height: 130)
                .clipped()
                Label(name?.isEmpty == false ? name! : String(localized: "Location"), systemImage: "location.fill")
                    .font(.subheadline.weight(.semibold))
                    .lineLimit(1)
                    .foregroundStyle(isUser ? Theme.onAccent : Theme.text)
                    .padding(10)
                    .frame(width: 250, alignment: .leading)
            }
            .background(isUser ? Theme.accentFill : Theme.card)
            .clipShape(RoundedRectangle(cornerRadius: Theme.cornerRadius, style: .continuous))
        }
        .buttonStyle(.plain)
        .accessibilityLabel(Text("Location, \(name ?? "")"))
        .accessibilityHint(Text("Opens Maps"))
        .task(id: colorScheme) { await render() }
    }

    private func render() async {
        let options = MKMapSnapshotter.Options()
        options.region = MKCoordinateRegion(center: CLLocationCoordinate2D(latitude: lat, longitude: lon),
                                            latitudinalMeters: 800, longitudinalMeters: 800)
        options.size = CGSize(width: 250, height: 130)
        options.traitCollection = UITraitCollection(userInterfaceStyle: colorScheme == .dark ? .dark : .light)
        snapshot = try? await MKMapSnapshotter(options: options).start().image
    }

    private func openMaps() {
        var components = URLComponents(string: "https://maps.apple.com/")
        components?.queryItems = [URLQueryItem(name: "ll", value: "\(lat),\(lon)"),
                                  URLQueryItem(name: "q", value: name?.isEmpty == false ? name : "\(lat),\(lon)")]
        if let url = components?.url { openURL(url) }
    }
}
