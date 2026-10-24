import AVFoundation
import MediaPlayer
import Observation
import UIKit

/// Playback of the owner's videos and audio from the server (docs/media-player.md).
///
/// AVPlayer does the playing: hardware decoding, HLS, AirPlay, Picture in Picture, the lock
/// screen. The Rust core (`PlayerCore`) owns the queue, resume positions and subtitle
/// parsing, so the web and Android players behave the same. Positions are pushed to
/// `/v1/media/progress` when the core says so (every 15 s, on pause, seek and end).
@Observable
@MainActor
final class MediaPlayer {
    let player = AVPlayer()

    private(set) var queue: [MediaItem] = []
    private(set) var currentIndex: Int?
    private(set) var hasNext = false
    private(set) var hasPrev = false
    private(set) var info: PlayInfo?
    private(set) var loading = false
    var error: String?
    private(set) var isPlaying = false
    private(set) var position: TimeInterval = 0
    private(set) var duration: TimeInterval = 0
    /// Furthest point the player can seek to now (an HLS remux still being written grows).
    private(set) var seekableEnd: TimeInterval = 0
    private(set) var subtitle: SubtitleTrack?
    private(set) var cueText: String?
    private(set) var rate: Float = 1
    /// The full-screen player is up.
    var presented = false
    /// Bumped when saved positions change, so lists redraw their progress bars.
    private(set) var progressRevision = 0
    private(set) var coreError: String?

    var current: MediaItem? { currentIndex.flatMap { queue.indices.contains($0) ? queue[$0] : nil } }
    var subtitleTracks: [SubtitleTrack] { info?.subtitles ?? [] }

    @ObservationIgnored let api: () -> MediaAPI
    @ObservationIgnored private let core: PlayerCore?
    @ObservationIgnored private var cues: [PlayerCore.Cue] = []
    @ObservationIgnored private var timeObserver: Any?
    @ObservationIgnored private var statusObservation: NSKeyValueObservation?
    @ObservationIgnored private var endObserver: NSObjectProtocol?
    @ObservationIgnored private var lastReport: TimeInterval = 0
    @ObservationIgnored private var pushing = false
    @ObservationIgnored private var loadTask: Task<Void, Never>?
    @ObservationIgnored private var artwork: MPMediaItemArtwork?
    @ObservationIgnored private let deviceName = UIDevice.current.name

    init(api: @escaping () -> MediaAPI, coreFile: URL = PlayerCore.defaultFileURL) {
        self.api = api
        do {
            core = try PlayerCore(fileURL: coreFile)
        } catch {
            core = nil
            coreError = error.localizedDescription
        }
        player.allowsExternalPlayback = true
        timeObserver = player.addPeriodicTimeObserver(forInterval: CMTime(value: 1, timescale: 4), queue: .main) { [weak self] time in
            MainActor.assumeIsolated { self?.tick(time) }
        }
        statusObservation = player.observe(\.timeControlStatus, options: [.new]) { [weak self] player, _ in
            let playing = player.timeControlStatus != .paused
            Task { @MainActor in self?.playingChanged(playing) }
        }
        if let queue = try? core?.queue() { apply(queue) }
        setUpRemoteCommands()
    }

    // MARK: Queue

    /// Plays `items` from `index` (the list the owner tapped in becomes the queue).
    func play(_ items: [MediaItem], startAt index: Int) {
        reportNow(playing: false)
        guard let core else {
            error = coreError
            return
        }
        do {
            apply(try core.setQueue(items, start: index))
            load()
        } catch {
            self.error = error.localizedDescription
        }
    }

    func next() {
        reportNow(playing: false)
        guard let view = try? core?.next(auto: false) else { return }
        apply(view)
        load()
    }

    func previous() {
        guard let core else { return }
        switch try? core.prev(positionMs: Int(position * 1000)) {
        case let .moved(view):
            reportNow(playing: false)
            apply(view)
            load()
        default:
            seek(to: 0)
        }
    }

    func jump(to index: Int) {
        reportNow(playing: false)
        guard let view = try? core?.jump(to: index) else { return }
        apply(view)
        load()
    }

    /// Stops and forgets the current item (the mini player's close button).
    func stop() {
        reportNow(playing: false)
        loadTask?.cancel()
        player.pause()
        player.replaceCurrentItem(with: nil)
        info = nil
        currentIndex = nil
        cues = []
        cueText = nil
        MPNowPlayingInfoCenter.default().nowPlayingInfo = nil
    }

    private func apply(_ view: PlayerCore.QueueView) {
        queue = view.items
        currentIndex = view.current
        hasNext = view.hasNext
        hasPrev = view.hasPrev
        // A queue restored at launch shows its item before it is loaded again.
        if info == nil { duration = current?.duration ?? 0 }
    }

    // MARK: Loading

    private func load() {
        guard let item = current else { return }
        loadTask?.cancel()
        player.pause()
        player.replaceCurrentItem(with: nil)
        info = nil
        error = nil
        cues = []
        subtitle = nil
        cueText = nil
        position = 0
        seekableEnd = 0
        duration = item.duration ?? 0
        artwork = nil
        loading = true
        loadTask = Task { [weak self] in
            await self?.open(item)
        }
    }

    private func open(_ item: MediaItem) async {
        defer { if current?.id == item.id { loading = false } }
        do {
            let info = try await api().play(id: item.id)
            guard !Task.isCancelled, current?.id == item.id else { return }
            if let saved = info.progress { _ = try? core?.merge([saved]) }
            let resumeMs = (try? core?.resume(id: item.id, durationMs: info.durationMs)) ?? info.resumeMs
            guard let url = api().absolute(info.url) else { throw APIError.invalidURL }
            self.info = info
            if let ms = info.durationMs { duration = TimeInterval(ms) / 1000 }
            activateAudioSession(video: item.isVideo)
            let avItem = AVPlayerItem(url: url)
            observeEnd(of: avItem)
            player.replaceCurrentItem(with: avItem)
            try await waitUntilReady(avItem)
            guard !Task.isCancelled, current?.id == item.id else { return }
            if resumeMs > 0 { await seekWhenAvailable(TimeInterval(resumeMs) / 1000, in: avItem) }
            guard !Task.isCancelled, current?.id == item.id else { return }
            player.playImmediately(atRate: rate)
            if let forced = info.subtitles.first(where: \.forced) { selectSubtitle(forced) }
            updateNowPlaying()
            await loadArtwork(info.posterUrl, for: item.id)
        } catch is CancellationError {
        } catch {
            guard current?.id == item.id else { return }
            self.error = error.localizedDescription
        }
    }

    private func waitUntilReady(_ item: AVPlayerItem) async throws {
        for _ in 0..<600 {
            switch item.status {
            case .readyToPlay: return
            case .failed: throw item.error ?? APIError.network(message: String(localized: "This file can't be played."))
            default: try await Task.sleep(for: .milliseconds(100))
            }
        }
        throw APIError.network(message: String(localized: "The server took too long to start this file."))
    }

    /// Seeks to `target`, waiting (up to 30 s) while an HLS remux is still being written
    /// and does not reach that far yet.
    private func seekWhenAvailable(_ target: TimeInterval, in item: AVPlayerItem) async {
        for _ in 0..<60 {
            let end = item.seekableTimeRanges.last.map { $0.timeRangeValue.end.seconds } ?? 0
            if end >= target - 1 {
                let time = CMTime(seconds: target, preferredTimescale: 600)
                _ = await player.seek(to: time, toleranceBefore: .zero, toleranceAfter: CMTime(seconds: 1, preferredTimescale: 600))
                position = target
                return
            }
            try? await Task.sleep(for: .milliseconds(500))
            if Task.isCancelled { return }
        }
    }

    private func observeEnd(of item: AVPlayerItem) {
        if let endObserver { NotificationCenter.default.removeObserver(endObserver) }
        endObserver = NotificationCenter.default.addObserver(forName: AVPlayerItem.didPlayToEndTimeNotification, object: item, queue: .main) { [weak self] _ in
            MainActor.assumeIsolated { self?.itemEnded() }
        }
    }

    private func itemEnded() {
        position = max(position, duration)
        reportNow(playing: false)
        if let view = try? core?.next(auto: true) {
            apply(view)
            load()
        } else {
            updateNowPlaying()
        }
    }

    // MARK: Controls

    func togglePlay() {
        if player.timeControlStatus == .paused {
            if player.currentItem == nil { load() } else { player.playImmediately(atRate: rate) }
        } else {
            player.pause()
        }
    }

    func seek(to seconds: TimeInterval) {
        let target = max(0, duration > 0 ? min(seconds, duration) : seconds)
        position = target
        player.seek(to: CMTime(seconds: target, preferredTimescale: 600), toleranceBefore: .zero, toleranceAfter: .zero) { [weak self] _ in
            Task { @MainActor in
                self?.reportNow(playing: self?.isPlaying ?? false)
                self?.updateNowPlaying()
            }
        }
    }

    func skip(by seconds: TimeInterval) { seek(to: position + seconds) }

    func setRate(_ rate: Float) {
        self.rate = rate
        if isPlaying { player.rate = rate }
        updateNowPlaying()
    }

    func selectSubtitle(_ track: SubtitleTrack?) {
        subtitle = track
        cues = []
        cueText = nil
        guard let track, let url = api().absolute(track.url) else { return }
        Task {
            do {
                // The server may first have to read the whole file for an embedded track.
                var request = URLRequest(url: url)
                request.timeoutInterval = 240
                let (data, response) = try await URLSession.shared.data(for: request)
                guard (response as? HTTPURLResponse)?.statusCode == 200 else { throw APIError.network(message: String(localized: "Subtitles are unavailable.")) }
                let parsed = try core?.parseSubtitles(String(decoding: data, as: UTF8.self)) ?? []
                guard subtitle?.id == track.id else { return }
                cues = parsed
                updateCue()
            } catch {
                self.error = error.localizedDescription
            }
        }
    }

    // MARK: Time and positions

    private func tick(_ time: CMTime) {
        guard info != nil, time.isNumeric else { return }
        position = time.seconds
        if let item = player.currentItem {
            if item.duration.isNumeric, item.duration.seconds.isFinite, item.duration.seconds > 0 { duration = item.duration.seconds }
            seekableEnd = item.seekableTimeRanges.last.map { $0.timeRangeValue.end.seconds } ?? 0
        }
        updateCue()
        if isPlaying, position - lastReport >= 1 || position < lastReport {
            reportNow(playing: true)
        }
    }

    private func updateCue() {
        let ms = Int(position * 1000)
        let active = cues.filter { $0.startMs <= ms && $0.endMs > ms }.map(\.text)
        let text = active.isEmpty ? nil : active.joined(separator: "\n")
        if text != cueText { cueText = text }
    }

    private func playingChanged(_ playing: Bool) {
        guard playing != isPlaying else { return }
        isPlaying = playing
        if !playing { reportNow(playing: false) }
        updateNowPlaying()
    }

    /// Tells the core where we are; pushes to the server when it says so.
    private func reportNow(playing: Bool) {
        guard let core, let item = current, info != nil, item.kind != "photo" else { return }
        lastReport = position
        guard let tick = try? core.position(id: item.id, positionMs: Int(position * 1000), durationMs: Int(duration * 1000),
                                            playing: playing, device: deviceName) else { return }
        if tick.sync {
            progressRevision += 1
            Task { await pushProgress() }
        }
    }

    /// Sends positions the server has not acknowledged.
    func pushProgress() async {
        guard let core, !pushing, let pending = try? core.pending(), !pending.isEmpty else { return }
        pushing = true
        defer { pushing = false }
        do {
            let answer = try await api().push(pending)
            try core.acknowledge(answer.acks)
            if !(try core.merge(answer.entries)).isEmpty { progressRevision += 1 }
        } catch {
            // Offline: the core keeps them pending for the next push.
        }
    }

    /// Adopts positions saved by other devices, then pushes ours.
    func syncProgress() async {
        guard let core else { return }
        if let entries = try? await api().progress(since: 0), let changed = try? core.merge(entries), !changed.isEmpty {
            progressRevision += 1
        }
        await pushProgress()
    }

    /// Saved position for a list row: fraction played, or nil when unseen/finished.
    func progress(of item: MediaItem) -> Double? {
        _ = progressRevision
        guard let entry = core?.entry(id: item.id), !entry.finished, entry.positionMs > 0 else { return nil }
        let total = entry.durationMs > 0 ? entry.durationMs : (item.durationMs ?? 0)
        guard total > 0 else { return nil }
        return min(1, Double(entry.positionMs) / Double(total))
    }

    func isFinished(_ item: MediaItem) -> Bool {
        _ = progressRevision
        return core?.entry(id: item.id)?.finished ?? false
    }

    /// Started, unfinished items (newest first) among `items`.
    func continueItems(in items: [MediaItem]) -> [MediaItem] {
        _ = progressRevision
        guard let recent = try? core?.recent(limit: 20) else { return [] }
        let byId = Dictionary(items.map { ($0.id, $0) }, uniquingKeysWith: { a, _ in a })
        return recent.compactMap { byId[$0.id] }
    }

    // MARK: System integration

    private func activateAudioSession(video: Bool) {
        let session = AVAudioSession.sharedInstance()
        try? session.setCategory(.playback, mode: video ? .moviePlayback : .default, policy: .longFormAudio)
        try? session.setActive(true)
    }

    private func loadArtwork(_ path: String?, for id: String) async {
        guard let path, let url = api().absolute(path),
              let (data, response) = try? await URLSession.shared.data(from: url),
              (response as? HTTPURLResponse)?.statusCode == 200,
              let image = UIImage(data: data), current?.id == id else { return }
        artwork = MPMediaItemArtwork(boundsSize: image.size) { _ in image }
        updateNowPlaying()
    }

    private func updateNowPlaying() {
        guard let item = current, info != nil else {
            MPNowPlayingInfoCenter.default().nowPlayingInfo = nil
            return
        }
        var now: [String: Any] = [
            MPMediaItemPropertyTitle: item.title,
            MPNowPlayingInfoPropertyElapsedPlaybackTime: position,
            MPNowPlayingInfoPropertyPlaybackRate: isPlaying ? Double(rate) : 0.0,
            MPNowPlayingInfoPropertyDefaultPlaybackRate: Double(rate),
            MPNowPlayingInfoPropertyMediaType: item.isVideo ? MPNowPlayingInfoMediaType.video.rawValue : MPNowPlayingInfoMediaType.audio.rawValue,
        ]
        if duration > 0 { now[MPMediaItemPropertyPlaybackDuration] = duration }
        if let album = item.album { now[MPMediaItemPropertyAlbumTitle] = album }
        if let artwork { now[MPMediaItemPropertyArtwork] = artwork }
        MPNowPlayingInfoCenter.default().nowPlayingInfo = now
    }

    private func setUpRemoteCommands() {
        let center = MPRemoteCommandCenter.shared()
        center.playCommand.addTarget { [weak self] _ in
            MainActor.assumeIsolated { self?.player.playImmediately(atRate: self?.rate ?? 1) }
            return .success
        }
        center.pauseCommand.addTarget { [weak self] _ in
            MainActor.assumeIsolated { self?.player.pause() }
            return .success
        }
        center.togglePlayPauseCommand.addTarget { [weak self] _ in
            MainActor.assumeIsolated { self?.togglePlay() }
            return .success
        }
        center.skipForwardCommand.preferredIntervals = [15]
        center.skipForwardCommand.addTarget { [weak self] _ in
            MainActor.assumeIsolated { self?.skip(by: 15) }
            return .success
        }
        center.skipBackwardCommand.preferredIntervals = [15]
        center.skipBackwardCommand.addTarget { [weak self] _ in
            MainActor.assumeIsolated { self?.skip(by: -15) }
            return .success
        }
        center.nextTrackCommand.addTarget { [weak self] _ in
            MainActor.assumeIsolated { self?.next() }
            return .success
        }
        center.previousTrackCommand.addTarget { [weak self] _ in
            MainActor.assumeIsolated { self?.previous() }
            return .success
        }
        center.changePlaybackPositionCommand.addTarget { [weak self] event in
            guard let event = event as? MPChangePlaybackPositionCommandEvent else { return .commandFailed }
            MainActor.assumeIsolated { self?.seek(to: event.positionTime) }
            return .success
        }
    }
}

/// The app is portrait-only on iPhone except while a video plays full screen.
@MainActor
enum OrientationLock {
    static var mask: UIInterfaceOrientationMask = .portrait

    static func set(_ mask: UIInterfaceOrientationMask) {
        self.mask = mask
        for scene in UIApplication.shared.connectedScenes.compactMap({ $0 as? UIWindowScene }) {
            scene.windows.first?.rootViewController?.setNeedsUpdateOfSupportedInterfaceOrientations()
            if mask == .portrait, scene.interfaceOrientation != .portrait {
                scene.requestGeometryUpdate(.iOS(interfaceOrientations: .portrait)) { _ in }
            }
        }
    }
}
