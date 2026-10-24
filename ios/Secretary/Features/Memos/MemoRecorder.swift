import ActivityKit
import AVFoundation
import Foundation
import Observation

/// Where recordings live until (and after) they are uploaded: Application Support/Memos.
enum MemoFiles {
    static var directory: URL {
        let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        let dir = base.appendingPathComponent("Memos", isDirectory: true)
        if !FileManager.default.fileExists(atPath: dir.path) {
            try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
            var values = URLResourceValues()
            values.isExcludedFromBackup = false
            var d = dir
            try? d.setResourceValues(values)
        }
        return dir
    }

    static func audio(_ clientId: String) -> URL { directory.appendingPathComponent("\(clientId).m4a") }

    /// Downloaded copies of memos recorded elsewhere.
    static func cached(_ serverId: String) -> URL {
        let dir = FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask)[0].appendingPathComponent("Memos", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir.appendingPathComponent("\(serverId).m4a")
    }
}

/// A finished recording, ready for the upload queue.
struct FinishedRecording: Sendable {
    let clientId: String
    let url: URL
    let startedAt: Date
    let duration: TimeInterval
}

/// Records one memo at a time like Voice Memos: AAC mono, live level meter, pause and
/// resume, and it keeps going with the screen locked or the app in the background
/// (`UIBackgroundModes: audio`). A phone call or Siri pauses it; it resumes when iOS says so.
@Observable
@MainActor
final class MemoRecorder: NSObject, AVAudioRecorderDelegate {
    enum State: Equatable { case idle, recording, paused }

    private(set) var state: State = .idle
    /// Recorded time (pauses excluded).
    private(set) var elapsed: TimeInterval = 0
    /// Recent levels, 0...1, about 20 per second (the live waveform).
    private(set) var levels: [Float] = []
    private(set) var error: String?
    /// Paused by an interruption (call, Siri, another app's audio), not by the owner.
    private(set) var interrupted = false

    static let maxLevels = 160

    @ObservationIgnored private var recorder: AVAudioRecorder?
    @ObservationIgnored private var current: (clientId: String, url: URL, startedAt: Date)?
    @ObservationIgnored private var meterTask: Task<Void, Never>?
    @ObservationIgnored private var observers: [NSObjectProtocol] = []
    /// The Lock Screen / Dynamic Island presence while recording.
    @ObservationIgnored private var activity: Activity<MemoRecordingAttributes>?

    var isActive: Bool { state != .idle }

    static let settings: [String: Any] = [
        AVFormatIDKey: kAudioFormatMPEG4AAC,
        AVSampleRateKey: 44_100,
        AVNumberOfChannelsKey: 1,
        AVEncoderBitRateKey: 96_000,
        AVEncoderAudioQualityKey: AVAudioQuality.high.rawValue,
    ]

    func start() async {
        guard state == .idle else { return }
        error = nil
        guard await AVAudioApplication.requestRecordPermission() else {
            error = String(localized: "Secretary can't use the microphone. Allow it in Settings › Privacy › Microphone.")
            return
        }
        let session = AVAudioSession.sharedInstance()
        do {
            try session.setCategory(.playAndRecord, mode: .default, options: [.defaultToSpeaker, .allowBluetoothHFP])
            try session.setActive(true)
        } catch {
            self.error = String(localized: "The microphone is busy: \(error.localizedDescription)")
            return
        }
        let clientId = UUID().uuidString.lowercased()
        let url = MemoFiles.audio(clientId)
        do {
            let r = try AVAudioRecorder(url: url, settings: Self.settings)
            r.isMeteringEnabled = true
            r.delegate = self
            guard r.prepareToRecord(), r.record() else {
                throw NSError(domain: "MemoRecorder", code: 1, userInfo: [NSLocalizedDescriptionKey: String(localized: "Recording did not start.")])
            }
            recorder = r
        } catch {
            self.error = error.localizedDescription
            try? session.setActive(false, options: .notifyOthersOnDeactivation)
            return
        }
        current = (clientId, url, Date())
        levels = []
        elapsed = 0
        interrupted = false
        state = .recording
        startActivity()
        observe()
        meterTask = Task { [weak self] in await self?.meter() }
    }

    func pause() {
        guard state == .recording else { return }
        recorder?.pause()
        state = .paused
        interrupted = false
        elapsed = recorder?.currentTime ?? elapsed
        updateActivity()
    }

    func resume() {
        guard state == .paused, let recorder else { return }
        try? AVAudioSession.sharedInstance().setActive(true)
        if recorder.record() {
            state = .recording
            interrupted = false
            updateActivity()
        } else {
            error = String(localized: "Recording could not resume.")
        }
    }

    /// Stops and hands back the file. Nil when nothing usable was recorded.
    func stop() -> FinishedRecording? {
        guard state != .idle, let recorder, let current else { return nil }
        let duration = recorder.currentTime
        recorder.stop()
        teardown()
        guard duration >= 0.5, let size = try? FileManager.default.attributesOfItem(atPath: current.url.path)[.size] as? Int64, size > 0 else {
            try? FileManager.default.removeItem(at: current.url)
            return nil
        }
        return FinishedRecording(clientId: current.clientId, url: current.url, startedAt: current.startedAt, duration: duration)
    }

    /// Stops and throws the recording away.
    func discard() {
        guard state != .idle, let current else { return }
        recorder?.stop()
        recorder?.deleteRecording()
        try? FileManager.default.removeItem(at: current.url)
        teardown()
    }

    private func teardown() {
        endActivity()
        meterTask?.cancel()
        meterTask = nil
        for o in observers { NotificationCenter.default.removeObserver(o) }
        observers = []
        recorder = nil
        current = nil
        state = .idle
        interrupted = false
        try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
    }

    private func meter() async {
        while !Task.isCancelled, state != .idle {
            if state == .recording, let recorder {
                recorder.updateMeters()
                let db = recorder.averagePower(forChannel: 0)
                // -50 dB (room noise) ... 0 dB, a little curve so speech fills the bar.
                let level = Float(pow(Double(max(0, min(1, (db + 50) / 50))), 1.6))
                levels.append(level)
                if levels.count > Self.maxLevels { levels.removeFirst(levels.count - Self.maxLevels) }
                elapsed = recorder.currentTime
            }
            try? await Task.sleep(for: .milliseconds(50))
        }
    }

    private func observe() {
        let center = NotificationCenter.default
        observers.append(center.addObserver(forName: AVAudioSession.interruptionNotification, object: nil, queue: .main) { [weak self] note in
            let info = note.userInfo ?? [:]
            let type = (info[AVAudioSessionInterruptionTypeKey] as? UInt).flatMap(AVAudioSession.InterruptionType.init(rawValue:))
            let options = AVAudioSession.InterruptionOptions(rawValue: info[AVAudioSessionInterruptionOptionKey] as? UInt ?? 0)
            Task { @MainActor in
                guard let self else { return }
                switch type {
                case .began:
                    if self.state == .recording {
                        self.recorder?.pause()
                        self.state = .paused
                        self.interrupted = true
                        self.updateActivity()
                    }
                case .ended:
                    if self.interrupted && options.contains(.shouldResume) { self.resume() }
                default:
                    break
                }
            }
        })
        observers.append(center.addObserver(forName: AVAudioSession.mediaServicesWereResetNotification, object: nil, queue: .main) { [weak self] _ in
            Task { @MainActor in
                guard let self, self.state != .idle else { return }
                // The recorder is gone with the media server; keep what was written.
                self.error = String(localized: "The audio system restarted. The recording so far was kept.")
                self.state = .paused
                self.interrupted = true
            }
        })
    }

    // MARK: Live Activity

    private var activityState: MemoRecordingAttributes.ContentState {
        .init(paused: state != .recording, elapsed: recorder?.currentTime ?? elapsed, at: Date(), interrupted: interrupted)
    }

    private func startActivity() {
        guard ActivityAuthorizationInfo().areActivitiesEnabled else { return }
        activity = try? Activity.request(attributes: MemoRecordingAttributes(startedAt: Date()),
                                         content: .init(state: activityState, staleDate: nil), pushType: nil)
    }

    private func updateActivity() {
        guard let activity else { return }
        let content = ActivityContent(state: activityState, staleDate: nil)
        Task { await activity.update(content) }
    }

    private func endActivity() {
        guard let activity else { return }
        self.activity = nil
        let content = ActivityContent(state: activityState, staleDate: nil)
        Task { await activity.end(content, dismissalPolicy: .immediate) }
    }

    /// Ends Live Activities a previous run left behind (the app was killed while recording).
    static func endStaleActivities() {
        for a in Activity<MemoRecordingAttributes>.activities {
            Task { await a.end(nil, dismissalPolicy: .immediate) }
        }
    }

    nonisolated func audioRecorderEncodeErrorDidOccur(_ recorder: AVAudioRecorder, error: Error?) {
        let message = error?.localizedDescription ?? "encoder error"
        Task { @MainActor in self.error = message }
    }
}
