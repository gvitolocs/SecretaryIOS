import AVFoundation
import MediaPlayer
import Observation
import os
import UIKit

private let localPlayLog = Logger(subsystem: "tech.getsecretary.app", category: "GalleryImages")
/// `MediaTiming` lines: where the wait before the first frame goes (server answer, AVPlayer ready,
/// resume seek, first frame), for every play.
private let timingLog = Logger(subsystem: "tech.getsecretary.app", category: "MediaTiming")

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
    /// Item id → this iPhone's Photos asset (the photo backup's ledger): those videos play from the phone,
    /// with nothing downloaded, also offline.
    @ObservationIgnored var localVideo: ((String) -> String?)?
    @ObservationIgnored private var playingLocal = false

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
    /// Shown while a track loads: "Reading subtitles…" (an OCR run can take minutes).
    private(set) var subtitleStatus: String?
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
    @ObservationIgnored private var subtitleTask: Task<Void, Never>?
    @ObservationIgnored private var timeObserver: Any?
    @ObservationIgnored private var statusObservation: NSKeyValueObservation?
    @ObservationIgnored private var endObserver: NSObjectProtocol?
    @ObservationIgnored private var lastReport: TimeInterval = 0
    @ObservationIgnored private var pushing = false
    @ObservationIgnored private var loadTask: Task<Void, Never>?
    @ObservationIgnored private var artwork: MPMediaItemArtwork?
    @ObservationIgnored private let deviceName = UIDevice.current.name
    /// The play waiting for its first frame: item, when the tap was, the steps so far.
    @ObservationIgnored private var firstFrame: (id: String, start: ContinuousClock.Instant, from: TimeInterval, steps: String)?
    /// Milliseconds from the tap to the first frame of the last play (the self-test reads it).
    private(set) var lastFirstFrameMs: Int?

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

    /// Plays `items` from `index` (the list the owner tapped in becomes the queue). `fromStart`
    /// forgets the saved minute of that item first ("Play from the beginning").
    func play(_ items: [MediaItem], startAt index: Int, fromStart: Bool = false) {
        reportNow(playing: false)
        guard let core else {
            error = coreError
            return
        }
        if fromStart, items.indices.contains(index) {
            let item = items[index]
            _ = try? core.position(id: item.id, positionMs: 0, durationMs: item.durationMs ?? 0, playing: false, device: deviceName)
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
        playingLocal = false
        error = nil
        cues = []
        subtitle = nil
        cueText = nil
        subtitleStatus = nil
        subtitleTask?.cancel()
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
        let start = ContinuousClock.now
        let ms = { Int((ContinuousClock.now - start) / .milliseconds(1)) }
        lastFirstFrameMs = nil
        do {
            if item.isVideo, let identifier = localVideo?(item.id), let asset = await LocalVideo.asset(identifier) {
                try await openLocal(item, asset)
                return
            }
            let info = try await api().play(id: item.id)
            let apiMs = ms()
            guard !Task.isCancelled, current?.id == item.id else { return }
            if let saved = info.progress { _ = try? core?.merge([saved]) }
            let resumeMs = (try? core?.resume(id: item.id, durationMs: info.durationMs)) ?? info.resumeMs
            guard var url = api().absolute(info.url) else { throw APIError.invalidURL }
            // A finished HLS playlist then starts at the resume point (EXT-X-START), so AVPlayer
            // loads that segment first instead of segment 0 and a seek afterwards.
            if resumeMs > 0, info.mode != "direct", var parts = URLComponents(url: url, resolvingAgainstBaseURL: false) {
                parts.queryItems = (parts.queryItems ?? []) + [URLQueryItem(name: "start_ms", value: String(resumeMs))]
                url = parts.url ?? url
            }
            self.info = info
            if let ms = info.durationMs { duration = TimeInterval(ms) / 1000 }
            activateAudioSession(video: item.isVideo)
            let avItem = AVPlayerItem(url: url)
            observeEnd(of: avItem)
            player.replaceCurrentItem(with: avItem)
            try await waitUntilReady(avItem)
            let readyMs = ms()
            guard !Task.isCancelled, current?.id == item.id else { return }
            // Still needed when the server ignored start_ms (a playlist still being written, an old server).
            let target = TimeInterval(resumeMs) / 1000
            if resumeMs > 0, abs(player.currentTime().seconds - target) > 1.5 { await seekWhenAvailable(target, in: avItem) }
            guard !Task.isCancelled, current?.id == item.id else { return }
            firstFrame = (item.id, start, TimeInterval(resumeMs) / 1000,
                          "mode=\(info.mode) api_ms=\(apiMs) ready_ms=\(readyMs) seek_ms=\(ms()) resume_s=\(resumeMs / 1000)")
            player.playImmediately(atRate: rate)
            if let pick = core?.pickSubtitle(id: item.id, tracks: info.subtitles),
               let track = info.subtitles.first(where: { $0.id == pick }) {
                selectSubtitle(track, remember: false)
            }
            updateNowPlaying()
            await loadArtwork(info.posterUrl, for: item.id)
        } catch is CancellationError {
        } catch {
            guard current?.id == item.id else { return }
            self.error = error.localizedDescription
            localPlayLog.error("GalleryImages player failed id=\(item.id, privacy: .public): \(String(describing: error), privacy: .public)")
        }
    }

    /// A video that is on this iPhone plays from Photos (resume positions still come from the core).
    private func openLocal(_ item: MediaItem, _ asset: AVAsset) async throws {
        guard !Task.isCancelled, current?.id == item.id else { return }
        let seconds = (try? await asset.load(.duration))?.seconds ?? 0
        let durationMs = seconds.isFinite && seconds > 0 ? Int(seconds * 1000) : item.durationMs
        let resumeMs = (try? core?.resume(id: item.id, durationMs: durationMs)) ?? 0
        playingLocal = true
        if let durationMs { duration = TimeInterval(durationMs) / 1000 }
        activateAudioSession(video: true)
        let avItem = AVPlayerItem(asset: asset)
        observeEnd(of: avItem)
        player.replaceCurrentItem(with: avItem)
        try await waitUntilReady(avItem)
        guard !Task.isCancelled, current?.id == item.id else { return }
        if resumeMs > 0 { await seekWhenAvailable(TimeInterval(resumeMs) / 1000, in: avItem) }
        guard !Task.isCancelled, current?.id == item.id else { return }
        player.playImmediately(atRate: rate)
        updateNowPlaying()
        localPlayLog.notice("GalleryImages source=local video id=\(item.id, privacy: .public)")
    }

    private func waitUntilReady(_ item: AVPlayerItem) async throws {
        for _ in 0..<2400 {
            switch item.status {
            case .readyToPlay: return
            case .failed: throw item.error ?? APIError.network(message: String(localized: "This file can't be played."))
            default: try await Task.sleep(for: .milliseconds(25))
            }
        }
        throw APIError.network(message: String(localized: "The server took too long to start this file."))
    }

    /// Seeks to `target`, waiting (up to 30 s) while an HLS remux is still being written
    /// and does not reach that far yet.
    private func seekWhenAvailable(_ target: TimeInterval, in item: AVPlayerItem) async {
        for _ in 0..<150 {
            let end = item.seekableTimeRanges.last.map { $0.timeRangeValue.end.seconds } ?? 0
            if end >= target - 1 {
                let time = CMTime(seconds: target, preferredTimescale: 600)
                _ = await player.seek(to: time, toleranceBefore: .zero, toleranceAfter: CMTime(seconds: 1, preferredTimescale: 600))
                position = target
                return
            }
            try? await Task.sleep(for: .milliseconds(200))
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

    /// Shows `track` (nil = off). A choice from the menu is remembered for this file and
    /// its language becomes the default for others (`remember`).
    func selectSubtitle(_ track: SubtitleTrack?, remember: Bool = true) {
        if remember, let id = current?.id { core?.chooseSubtitle(id: id, track: track) }
        subtitle = track
        cues = []
        cueText = nil
        subtitleStatus = nil
        subtitleTask?.cancel()
        guard let track, let url = api().absolute(track.url) else { return }
        subtitleStatus = track.isImage ? String(localized: "Reading the subtitles…") : String(localized: "Loading subtitles…")
        subtitleTask = Task { [weak self] in await self?.loadSubtitles(track, from: url) }
    }

    /// Fetches the track. The server answers 202 while it converts, and a partial file
    /// (`X-Subtitles-Partial`) while OCR is still reading: keep asking until it is whole.
    private func loadSubtitles(_ track: SubtitleTrack, from url: URL) async {
        var failures = 0
        while !Task.isCancelled, subtitle?.id == track.id {
            do {
                var request = URLRequest(url: url)
                request.timeoutInterval = 60
                request.cachePolicy = .reloadIgnoringLocalCacheData
                let (data, response) = try await URLSession.shared.data(for: request)
                guard let http = response as? HTTPURLResponse else { throw URLError(.badServerResponse) }
                guard subtitle?.id == track.id else { return }
                switch http.statusCode {
                case 200:
                    cues = try core?.parseSubtitles(String(decoding: data, as: UTF8.self)) ?? []
                    updateCue()
                    guard http.value(forHTTPHeaderField: "X-Subtitles-Partial") != nil else {
                        subtitleStatus = nil
                        return
                    }
                    subtitleStatus = cues.isEmpty ? String(localized: "Reading the subtitles…") : nil
                    try await Task.sleep(for: .seconds(15))
                case 202:
                    try await Task.sleep(for: .seconds(5))
                default:
                    throw APIError.network(message: String(localized: "Subtitles are unavailable."))
                }
                failures = 0
            } catch is CancellationError {
                return
            } catch {
                failures += 1
                if failures >= 3 {
                    if subtitle?.id == track.id {
                        subtitleStatus = nil
                        self.error = error.localizedDescription
                    }
                    return
                }
                try? await Task.sleep(for: .seconds(5))
            }
        }
    }

    // MARK: Time and positions

    private func tick(_ time: CMTime) {
        guard info != nil || playingLocal, time.isNumeric else { return }
        position = time.seconds
        if let item = player.currentItem {
            if item.duration.isNumeric, item.duration.seconds.isFinite, item.duration.seconds > 0 { duration = item.duration.seconds }
            seekableEnd = item.seekableTimeRanges.last.map { $0.timeRangeValue.end.seconds } ?? 0
        }
        updateCue()
        if let f = firstFrame, f.id == current?.id, isPlaying, position > f.from + 0.1 {
            let total = Int((ContinuousClock.now - f.start) / .milliseconds(1))
            lastFirstFrameMs = total
            timingLog.notice("MediaTiming id=\(f.id, privacy: .public) \(f.steps, privacy: .public) first_frame_ms=\(total)")
            firstFrame = nil
        }
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
        guard let core, let item = current, info != nil || playingLocal, item.kind != "photo" else { return }
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

    #if DEBUG || SCREENSHOTS
    // For MediaSelfTest.
    var cueCount: Int { cues.count }
    func cueStart(after seconds: TimeInterval) -> TimeInterval? {
        cues.first { $0.startMs >= Int(seconds * 1000) }.map { TimeInterval($0.startMs) / 1000 }
    }
    func savedPositionMs(id: String) -> Int? { core?.entry(id: id)?.positionMs }
    /// Sets the saved position as if the player had stopped there (and pushes it).
    func setSavedPosition(id: String, ms: Int, durationMs: Int) async {
        _ = try? core?.position(id: id, positionMs: ms, durationMs: durationMs, playing: false, device: deviceName)
        await pushProgress()
    }
    #endif

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
        guard let item = current, info != nil || playingLocal else {
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
