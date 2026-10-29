import AVFoundation
import Foundation
import Observation

/// Plays one memo file at a time (the local recording, or a downloaded copy).
/// TODO(lucina-rust-player): swap for the shared Rust player once it lands on iOS.
@Observable
@MainActor
final class MemoPlayer: NSObject, AVAudioPlayerDelegate {
    private(set) var memoId: String?
    private(set) var playing = false
    private(set) var position: TimeInterval = 0
    private(set) var duration: TimeInterval = 0
    var rate: Float = 1 { didSet { player?.rate = rate } }
    private(set) var error: String?

    @ObservationIgnored private var player: AVAudioPlayer?
    @ObservationIgnored private var ticker: Task<Void, Never>?

    func load(_ url: URL, memoId: String) {
        if self.memoId == memoId, player != nil { return }
        stop()
        do {
            let p = try AVAudioPlayer(contentsOf: url)
            p.enableRate = true
            p.rate = rate
            p.delegate = self
            p.prepareToPlay()
            player = p
            self.memoId = memoId
            duration = p.duration
            position = 0
            error = nil
        } catch {
            self.error = error.localizedDescription
        }
    }

    func toggle() { playing ? pause() : play() }

    func play() {
        guard let player else { return }
        try? AVAudioSession.sharedInstance().setCategory(.playback, mode: .spokenAudio)
        try? AVAudioSession.sharedInstance().setActive(true)
        player.play()
        playing = true
        ticker?.cancel()
        ticker = Task { [weak self] in
            while !Task.isCancelled {
                guard let self, let p = self.player else { return }
                self.position = p.currentTime
                try? await Task.sleep(for: .milliseconds(200))
            }
        }
    }

    func pause() {
        player?.pause()
        playing = false
        ticker?.cancel()
    }

    func seek(_ t: TimeInterval) {
        guard let player else { return }
        player.currentTime = max(0, min(t, player.duration))
        position = player.currentTime
    }

    func skip(_ delta: TimeInterval) { seek(position + delta) }

    func stop() {
        player?.stop()
        player = nil
        memoId = nil
        playing = false
        position = 0
        ticker?.cancel()
    }

    nonisolated func audioPlayerDidFinishPlaying(_ player: AVAudioPlayer, successfully flag: Bool) {
        Task { @MainActor in
            self.playing = false
            self.ticker?.cancel()
            self.position = 0
        }
    }
}
