import AVFoundation
import Foundation
import Observation

/// "Read aloud" (swipe a message right, or the menu): Lucina says the message in her voice (`POST /v1/tts`,
/// Kokoro), cached per message id in Caches/TTS. One message at a time; tapping the bubble's speaker stops
/// it; it refuses while a memo or a chat voice note records. The audio session calls are ChatAudio's.
@Observable
@MainActor
final class ChatReadAloud: NSObject, AVAudioPlayerDelegate {
    enum Outcome: Equatable {
        case started
        case stopped
        /// A memo or a chat voice note is recording (the toast's text).
        case refusedRecording(String)
        case nothingToRead
        case failed(String)
    }

    /// What the last start did (the self-test's log): the TTS call's HTTP status (nil when it came from the
    /// cache), the bytes, whether the cache had it, and the audio session it played in.
    struct Report: Equatable {
        var status: Int?
        var bytes = 0
        var cacheHit = false
        var session = ""
    }

    private(set) var playingId: String?
    private(set) var loadingId: String?
    @ObservationIgnored private(set) var lastReport: Report?
    @ObservationIgnored private var player: AVAudioPlayer?
    @ObservationIgnored private var interruption: NSObjectProtocol?
    @ObservationIgnored let cache: TTSCache
    /// Why it may not play now (a memo or a chat voice note records); ChatStore adds its own recorder.
    @ObservationIgnored var refusal: @MainActor () -> String? = { ChatAudio.recordingNotice }
    /// The self-test plays at volume 0 (it checks the clock, never the speaker).
    @ObservationIgnored var muted = false

    init(cache: TTSCache = .shared) {
        self.cache = cache
        super.init()
    }

    var isPlaying: Bool { player?.isPlaying == true }
    var currentTime: TimeInterval { player?.currentTime ?? 0 }
    var duration: TimeInterval { player?.duration ?? 0 }

    func isActive(_ itemId: String) -> Bool { playingId == itemId || loadingId == itemId }

    /// Reads `text` for the message, or stops it when it is the one already reading.
    func toggle(itemId: String, text: String?, lang: String?, api: SecretaryAPI) async -> Outcome {
        if isActive(itemId) {
            stop()
            return .stopped
        }
        guard let text, !text.isEmpty else { return .nothingToRead }
        if let refusal = refusal() { return .refusedRecording(refusal) }
        ChatAudio.shared.stopAll(except: self)
        stop()
        loadingId = itemId
        var report = Report()
        let data: Data
        if let cached = cache.read(itemId) {
            data = cached
            report.cacheHit = true
        } else {
            do {
                data = try await api.tts(text, lang: lang)
                report.status = 200
                try? cache.write(data, for: itemId)
            } catch {
                report.status = APIError.wrap(error).status
                lastReport = report
                if loadingId == itemId { loadingId = nil }
                return .failed(String(localized: "Couldn't get Lucina's voice."))
            }
        }
        report.bytes = data.count
        // Stopped (another message, a recording) while the voice was on its way.
        guard loadingId == itemId else {
            lastReport = report
            return .stopped
        }
        loadingId = nil
        if let refusal = refusal() {
            lastReport = report
            return .refusedRecording(refusal)
        }
        do {
            try ChatAudio.activate(spoken: true)
            let player = try AVAudioPlayer(data: data)
            player.delegate = self
            player.volume = muted ? 0 : 1
            self.player = player
            playingId = itemId
            report.session = ChatAudio.sessionDescription
            lastReport = report
            observeInterruptions()
            guard player.play() else {
                finish()
                return .failed(String(localized: "Couldn't play Lucina's voice."))
            }
            return .started
        } catch {
            lastReport = report
            finish()
            return .failed(String(localized: "Couldn't play Lucina's voice."))
        }
    }

    func stop() {
        loadingId = nil
        guard player != nil || playingId != nil else { return }
        player?.stop()
        finish()
    }

    private func finish() {
        player = nil
        playingId = nil
        if let interruption { NotificationCenter.default.removeObserver(interruption) }
        interruption = nil
        ChatAudio.deactivate()
    }

    /// A call, Siri or a recording takes the session: the speaker on the bubble goes away.
    private func observeInterruptions() {
        if let interruption { NotificationCenter.default.removeObserver(interruption) }
        interruption = NotificationCenter.default.addObserver(forName: AVAudioSession.interruptionNotification, object: nil,
                                                              queue: .main) { [weak self] note in
            let began = (note.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt) == AVAudioSession.InterruptionType.began.rawValue
            guard began else { return }
            MainActor.assumeIsolated { self?.stop() }
        }
    }

    nonisolated func audioPlayerDidFinishPlaying(_ player: AVAudioPlayer, successfully flag: Bool) {
        Task { @MainActor in
            if self.player === player { self.finish() }
        }
    }

    nonisolated func audioPlayerDecodeErrorDidOccur(_ player: AVAudioPlayer, error: Error?) {
        Task { @MainActor in
            if self.player === player { self.finish() }
        }
    }
}
