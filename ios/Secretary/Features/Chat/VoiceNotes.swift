import AVFoundation
import CoreGraphics
import Foundation
import Observation

// Voice notes in Lucina's chat, Telegram style: hold the mic to record, release to send. This file holds
// the pure parts (files, waveform bytes, the gesture's thresholds, when Lucina's voice reply plays) so they
// can be unit-tested, and the player for her spoken replies.

/// One recorded note, ready to send.
struct VoiceNote: Sendable {
    var url: URL
    var duration: TimeInterval
    /// Loudness 0...1, about 20 per second (the recorder's meter); empty: read from the file.
    var levels: [Float] = []
    var recordedAt = Date()

    /// Notes up to this long are transcribed on the iPhone before they are sent; longer ones go to Whisper.
    static let transcribeLimit: TimeInterval = 60
    /// The on-device transcription gives up after this.
    static let transcribeTimeout: Duration = .seconds(30)
    /// Recording stops and sends at this length.
    static let maxDuration: TimeInterval = 10 * 60
}

/// Where the chat's recorder writes: Caches/ChatNotes (never Application Support/Memos, whose upload
/// queue would adopt anything it finds there as a memo).
enum VoiceNoteFiles {
    static var directory: URL {
        let caches = FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask).first ?? FileManager.default.temporaryDirectory
        let dir = caches.appendingPathComponent("ChatNotes", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir
    }

    /// `voice-20261026-143012.aac`: the part's filename on the wire.
    static func fileName(for date: Date) -> String {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.timeZone = .current
        formatter.dateFormat = "yyyyMMdd-HHmmss"
        return "voice-\(formatter.string(from: date)).aac"
    }

    /// Leftovers of notes that never left (the app was killed while recording or before sending).
    static func purge(olderThan age: TimeInterval = 24 * 60 * 60, now: Date = Date(), in directory: URL = VoiceNoteFiles.directory) {
        guard let entries = try? FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: [.contentModificationDateKey])
        else { return }
        for entry in entries {
            let date = (try? entry.resourceValues(forKeys: [.contentModificationDateKey]))?.contentModificationDate ?? .distantPast
            if now.timeIntervalSince(date) > age { try? FileManager.default.removeItem(at: entry) }
        }
    }
}

/// The waveform that rides with a note (`meta.files[i].waveform`): up to 64 loudness bytes, base64.
enum VoiceWaveform {
    static let buckets = 64

    /// The levels squeezed into at most `count` buckets (the loudest level of each), 0...1 → 0...255.
    /// Fewer levels than buckets: one byte per level.
    static func bytes(_ levels: [Float], count: Int = buckets) -> [UInt8] {
        resample(levels, count: count).map { UInt8((max(0, min(1, $0.isFinite ? $0 : 0)) * 255).rounded()) }
    }

    static func base64(_ levels: [Float], count: Int = buckets) -> String? {
        let out = bytes(levels, count: count)
        return out.isEmpty ? nil : Data(out).base64EncodedString()
    }

    /// The bubble's bars from the attachment's base64 (nil for anything that isn't base64).
    static func decode(_ base64: String?) -> [Float]? {
        guard let base64, let data = Data(base64Encoded: base64), !data.isEmpty else { return nil }
        return data.map { Float($0) / 255 }
    }

    /// At most `count` values: the maximum of each of `count` equal slices.
    static func resample(_ levels: [Float], count: Int) -> [Float] {
        guard count > 0, !levels.isEmpty else { return [] }
        guard levels.count > count else { return levels }
        return (0..<count).map { bucket in
            let start = bucket * levels.count / count
            let end = max(start + 1, (bucket + 1) * levels.count / count)
            return levels[start..<min(end, levels.count)].max() ?? 0
        }
    }

    /// Levels read from an audio file (for a clip that wasn't recorded with the meter): RMS of 50 ms
    /// windows on the recorder's scale (-50 dB ... 0 dB, the same curve).
    static func levels(from url: URL) -> [Float] {
        guard let file = try? AVAudioFile(forReading: url) else { return [] }
        let format = file.processingFormat
        let window = AVAudioFrameCount(max(1, format.sampleRate / 20))
        guard let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: window) else { return [] }
        var out: [Float] = []
        while (try? file.read(into: buffer, frameCount: window)) != nil, buffer.frameLength > 0 {
            guard let channel = buffer.floatChannelData?[0] else { break }
            var sum: Float = 0
            for i in 0..<Int(buffer.frameLength) { sum += channel[i] * channel[i] }
            let rms = sqrt(sum / Float(buffer.frameLength))
            let db = 20 * log10(max(rms, 0.000_01))
            out.append(Float(pow(Double(max(0, min(1, (db + 50) / 50))), 1.6)))
            if out.count > Int(VoiceNote.maxDuration * 20) { break }
        }
        return out
    }
}

/// Where the finger is decides the note's fate: left past 100 pt cancels, up past 80 pt locks, letting go
/// anywhere else sends.
enum VoiceGesture {
    static let cancelDistance: CGFloat = 100
    static let lockDistance: CGFloat = 80
    /// A touch that moved less than this is a tap (the locked recording's send button).
    static let tapSlop: CGFloat = 12

    enum Outcome: Equatable { case keep, cancel, lock }
    enum Release: Equatable { case send, cancel, lock }

    /// While the finger is down: whichever threshold is crossed further wins.
    static func outcome(_ translation: CGSize) -> Outcome {
        let left = cancelProgress(translation)
        let up = lockProgress(translation)
        guard max(left, up) >= 1 else { return .keep }
        return left >= up ? .cancel : .lock
    }

    /// When the finger lifts.
    static func release(_ translation: CGSize) -> Release {
        switch outcome(translation) {
        case .cancel: return .cancel
        case .lock: return .lock
        case .keep: return .send
        }
    }

    /// 0 at rest, 1 at the cancel threshold (and beyond).
    static func cancelProgress(_ translation: CGSize) -> CGFloat {
        max(0, -translation.width) / cancelDistance
    }

    /// 0 at rest, 1 at the lock threshold (and beyond).
    static func lockProgress(_ translation: CGSize) -> CGFloat {
        max(0, -translation.height) / lockDistance
    }

    /// The "‹ slide to cancel" hint fades out as the finger slides toward the threshold.
    static func cancelHintOpacity(_ translation: CGSize) -> Double {
        Double(max(0, 1 - min(1, cancelProgress(translation))))
    }

    static func isTap(_ translation: CGSize) -> Bool {
        abs(translation.width) < tapSlop && abs(translation.height) < tapSlop
    }
}

/// Lucina answers a voice note with her voice (`meta.voiceReply`), once, while the app is open.
@Observable
@MainActor
final class LucinaVoiceReplies: NSObject, AVAudioPlayerDelegate {
    nonisolated static let settingKey = "secretary.chat.voiceReplies"
    nonisolated static let playedKey = "secretary.chat.voiceRepliesPlayed"
    /// Older replies (a page loaded later, another device's conversation) never start on their own.
    nonisolated static let freshness: TimeInterval = 5 * 60

    private(set) var playingId: String?
    private(set) var loadingId: String?
    private(set) var error: String?
    @ObservationIgnored private var player: AVAudioPlayer?
    @ObservationIgnored private let defaults: UserDefaults
    @ObservationIgnored private var played: [String]

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        played = defaults.stringArray(forKey: Self.playedKey) ?? []
        super.init()
    }

    /// The Settings toggle "Lucina answers voice notes with her voice" (on unless turned off).
    nonisolated static func isEnabled(_ defaults: UserDefaults = .standard) -> Bool {
        defaults.object(forKey: settingKey) as? Bool ?? true
    }

    /// Whether `item` should play by itself now: her fresh reply to a voice note, marked `voiceReply`,
    /// not played before, with the toggle on and the app in the foreground.
    nonisolated static func shouldAutoplay(_ item: TimelineItem, in items: [TimelineItem], enabled: Bool, foreground: Bool,
                                           played: Set<String>, now: Date = Date()) -> Bool {
        guard enabled, foreground, item.role == .lucina, item.meta?.voiceReply == true, !played.contains(item.id),
              abs(now.timeIntervalSince(item.ts)) < freshness, !speech(for: item).isEmpty else { return false }
        guard let replyTo = item.replyTo, let note = items.first(where: { $0.id == replyTo }) else { return false }
        return note.role == .user && note.attachments.contains(where: \.isVoice)
    }

    /// What she says: the bubbles of a rich reply, or the plain text without markdown.
    nonisolated static func speech(for item: TimelineItem) -> String {
        (item.rich?.speechText ?? RichMessage.stripMarkdown(item.text)).trimmingCharacters(in: .whitespacesAndNewlines)
    }

    var playedIds: Set<String> { Set(played) }

    /// Looks at new items; plays the first one that qualifies (and remembers it, so never twice).
    func consider(_ incoming: [TimelineItem], items: [TimelineItem], foreground: Bool, api: () -> SecretaryAPI) {
        let enabled = Self.isEnabled(defaults)
        guard let item = incoming.first(where: {
            Self.shouldAutoplay($0, in: items, enabled: enabled, foreground: foreground, played: playedIds)
        }) else { return }
        markPlayed(item.id)
        // A recording has the audio session: speaking now would cut it off.
        guard !MemoRecorder.isAnyActive else { return }
        play(item, api: api())
    }

    /// Fetches her voice for the item (`POST /v1/tts`) and plays it; tapping again while it plays stops it.
    func play(_ item: TimelineItem, api: SecretaryAPI) {
        if playingId == item.id || loadingId == item.id {
            stop()
            return
        }
        guard !MemoRecorder.isAnyActive else {
            error = String(localized: "Finish the recording first.")
            return
        }
        stop()
        let text = Self.speech(for: item)
        guard !text.isEmpty else { return }
        error = nil
        loadingId = item.id
        Task {
            do {
                let data = try await api.tts(text)
                guard loadingId == item.id else { return }
                loadingId = nil
                guard !MemoRecorder.isAnyActive else { return }
                ChatAudio.shared.stop()
                let session = AVAudioSession.sharedInstance()
                try session.setCategory(.playback, mode: .spokenAudio)
                try session.setActive(true)
                let player = try AVAudioPlayer(data: data)
                player.delegate = self
                self.player = player
                playingId = item.id
                if !player.play() { finish() }
            } catch {
                if loadingId == item.id { loadingId = nil }
                self.error = String(localized: "Couldn't play Lucina's voice.")
            }
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
        try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
    }

    private func markPlayed(_ id: String) {
        played.append(id)
        if played.count > 200 { played.removeFirst(played.count - 200) }
        defaults.set(played, forKey: Self.playedKey)
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
