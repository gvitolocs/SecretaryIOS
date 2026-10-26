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

    /// New recordings are ADTS AAC (`.aac`): self-framed, so a file cut off when iOS kills the
    /// app still decodes and is recovered on the next launch. Older ones are `.m4a`.
    static func audio(_ clientId: String, ext: String = "m4a") -> URL {
        directory.appendingPathComponent("\(clientId).\(ext)")
    }

    /// This iPhone's file for a recording, whichever format it has.
    static func existingAudio(_ clientId: String) -> URL? {
        ["aac", "m4a"].map { audio(clientId, ext: $0) }.first { FileManager.default.fileExists(atPath: $0.path) }
    }

    /// Downloaded copies of memos recorded elsewhere.
    static func cached(_ serverId: String) -> URL {
        let dir = FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask)[0].appendingPathComponent("Memos", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir.appendingPathComponent("\(serverId).m4a")
    }
}

/// A short diagnostic log next to the recordings (Memos/memo-log.txt, last 300 lines): what
/// on-device transcription and the upload queue did, for when something goes wrong on the phone.
enum MemoLog {
    static func write(_ line: String) {
        let url = MemoFiles.directory.appendingPathComponent("memo-log.txt")
        let stamp = ISO8601DateFormatter().string(from: Date())
        var lines = ((try? String(contentsOf: url, encoding: .utf8)) ?? "").split(separator: "\n").map(String.init)
        lines.append("\(stamp) \(line)")
        if lines.count > 300 { lines.removeFirst(lines.count - 300) }
        try? (lines.joined(separator: "\n") + "\n").write(to: url, atomically: true, encoding: .utf8)
    }
}

/// A finished recording, ready for the upload queue.
struct FinishedRecording: Sendable {
    let clientId: String
    let url: URL
    let startedAt: Date
    let duration: TimeInterval
}

/// Records one memo at a time like Voice Memos: 48 kHz AAC from the iPhone's own microphones,
/// in stereo where they have a stereo pattern, live level meter, pause and resume, and it keeps
/// going with the screen locked or the app in the background (`UIBackgroundModes: audio`).
/// A phone call or Siri pauses it; it resumes when iOS says so.
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

    /// Where `start()` writes `<clientId>.aac`.
    let directory: URL
    /// Whether a recording shows on the Lock Screen / Dynamic Island (memos do, chat voice notes don't).
    let liveActivity: Bool

    /// Memos record into Application Support/Memos with a Live Activity; Lucina's chat keeps its own
    /// recorder for voice notes elsewhere (never in Memos/, where the memo queue would adopt them).
    init(directory: URL = MemoFiles.directory, liveActivity: Bool = true) {
        self.directory = directory
        self.liveActivity = liveActivity
        super.init()
    }

    /// Every level of the current (or last) recording, about 20 per second: the voice note's waveform.
    @ObservationIgnored private(set) var levelHistory: [Float] = []

    /// The recorder that holds the microphone. Only one may record at a time: the memos and the chat
    /// voice notes each have one, and the second refuses to start while the first is going.
    private static weak var micHolder: MemoRecorder?

    /// Some recorder (memo or chat voice note) has the microphone now.
    static var isAnyActive: Bool { micHolder?.isActive == true }

    /// The file being recorded (diagnostics).
    var currentFileName: String? { current?.url.lastPathComponent }

    /// Voice Memos' quality: 48 kHz AAC, 128 kbps per channel. The server keeps this file as the
    /// memo and only decodes a 16 kHz copy for Whisper.
    static func settings(channels: Int) -> [String: Any] {
        [
            AVFormatIDKey: kAudioFormatMPEG4AAC,
            AVSampleRateKey: 48_000,
            AVNumberOfChannelsKey: channels,
            AVEncoderBitRateKey: 128_000 * channels,
            AVEncoderAudioQualityKey: AVAudioQuality.max.rawValue,
        ]
    }

    /// Sets the session up the way Voice Memos records: record only (no Bluetooth headset's
    /// narrowband HFP microphone), the built-in microphones, and their stereo polar pattern
    /// when the iPhone has one. Returns the channel count to record and a line for the log.
    private func configureSession(_ session: AVAudioSession) throws -> (channels: Int, route: String) {
        try session.setCategory(.record, mode: .default)
        try? session.setPreferredSampleRate(48_000)
        // An incoming call's ringtone would otherwise interrupt the recording.
        try? session.setPrefersNoInterruptionsFromSystemAlerts(true)
        try session.setActive(true)
        guard let mic = session.availableInputs?.first(where: { $0.portType == .builtInMic }) else {
            return (1, "no built-in mic")
        }
        try? session.setPreferredInput(mic)
        let stereo = (mic.dataSources ?? []).filter { $0.supportedPolarPatterns?.contains(.stereo) == true }
        // The screen-side microphones first: the phone is held or lies face up while you talk.
        if let source = stereo.first(where: { $0.orientation == .front }) ?? stereo.first {
            do {
                try source.setPreferredPolarPattern(.stereo)
                try mic.setPreferredDataSource(source)
                try session.setPreferredInputOrientation(.portrait)
            } catch {
                MemoLog.write("record: stereo pattern refused: \(error)")
            }
        }
        let input = session.currentRoute.inputs.first
        let source = input?.selectedDataSource
        let route = "\(input?.portType.rawValue ?? "?") \(source?.dataSourceName ?? "-") \(source?.selectedPolarPattern?.rawValue ?? "-")"
            + " \(session.inputNumberOfChannels)ch \(Int(session.sampleRate)) Hz"
        return (min(2, max(1, session.inputNumberOfChannels)), route)
    }

    func start() async {
        guard state == .idle else { return }
        error = nil
        guard micIsFree() else { return }
        guard await AVAudioApplication.requestRecordPermission() else {
            error = String(localized: "Secretary can't use the microphone. Allow it in Settings › Privacy › Microphone.")
            return
        }
        // The other recorder may have started while the permission prompt was up.
        guard state == .idle, micIsFree() else { return }
        let session = AVAudioSession.sharedInstance()
        let setup: (channels: Int, route: String)
        do {
            setup = try configureSession(session)
        } catch {
            self.error = String(localized: "The microphone is busy: \(error.localizedDescription)")
            return
        }
        let clientId = UUID().uuidString.lowercased()
        try? FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let url = directory.appendingPathComponent("\(clientId).aac")
        MemoLog.write("record: \(setup.route)")
        do {
            let r = try AVAudioRecorder(url: url, settings: Self.settings(channels: setup.channels))
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
        levelHistory = []
        elapsed = 0
        interrupted = false
        state = .recording
        Self.micHolder = self
        if liveActivity { startActivity() }
        observe()
        meterTask = Task { [weak self] in await self?.meter() }
    }

    /// False (with a short message in `error`) while the other recorder has the microphone.
    private func micIsFree() -> Bool {
        guard let other = Self.micHolder, other !== self, other.isActive else { return true }
        error = other.liveActivity ? String(localized: "A voice memo is recording. Stop it first.")
            : String(localized: "A voice note for Lucina is recording. Finish it first.")
        return false
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
        if Self.micHolder === self { Self.micHolder = nil }
        try? AVAudioSession.sharedInstance().setPrefersNoInterruptionsFromSystemAlerts(false)
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
                levelHistory.append(level)
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
        guard ActivityAuthorizationInfo().areActivitiesEnabled else {
            MemoLog.write("live activity: disabled in Settings")
            return
        }
        do {
            activity = try Activity.request(attributes: MemoRecordingAttributes(startedAt: Date()),
                                            content: .init(state: activityState, staleDate: nil), pushType: nil)
        } catch {
            MemoLog.write("live activity: request failed: \(error)")
        }
    }

    /// The Live Activity as iOS has it now (state, paused flag), for the device self-test.
    var activityReport: String {
        guard let a = activity else { return "none" }
        return "\(a.activityState) paused=\(a.content.state.paused)"
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
