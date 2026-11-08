import AVFoundation
import Foundation
import Observation
import Speech

// The audio side of the streaming voice socket: one engine tap at the hardware format, converted to 16 kHz mono
// s16le, cut into 60 ms frames for the socket and written to a WAV file as well (the pending bubble and the
// fallback upload); and a player for the 24 kHz s16le audio the server sends back.

/// Cuts a byte stream into frames of exactly `frameBytes` (the last, partial one comes out of `flush`).
struct PCMPacker: Equatable {
    /// 60 ms of 16 kHz s16le mono: inside the 40-100 ms the protocol asks for.
    static let defaultFrameBytes = 16_000 * 2 * 60 / 1000

    let frameBytes: Int
    private var pending = Data()

    init(frameBytes: Int = PCMPacker.defaultFrameBytes) {
        precondition(frameBytes > 0 && frameBytes % 2 == 0 && frameBytes <= 8192, "frames are whole samples, at most 8 KiB")
        self.frameBytes = frameBytes
    }

    mutating func append(_ bytes: Data) -> [Data] {
        pending.append(bytes)
        var frames: [Data] = []
        while pending.count >= frameBytes {
            frames.append(Data(pending.prefix(frameBytes)))
            pending.removeFirst(frameBytes)
        }
        return frames
    }

    /// What is left, whole samples only (nil when nothing is).
    mutating func flush() -> Data? {
        let rest = pending.count - pending.count % 2
        defer { pending = Data() }
        return rest > 0 ? Data(pending.prefix(rest)) : nil
    }
}

/// The arithmetic of the conversion, apart from AVFoundation so it can be tested.
enum PCM16 {
    /// Frames the converter may produce for `inFrames` at `inRate` (rounded up, plus a little for its filter).
    static func outputCapacity(inFrames: Int, inRate: Double, outRate: Double = 16_000) -> Int {
        guard inRate > 0 else { return 0 }
        return Int((Double(inFrames) * outRate / inRate).rounded(.up)) + 32
    }

    static func sample(_ x: Float) -> Int16 {
        guard x.isFinite else { return 0 }
        return Int16((max(-1, min(1, x)) * 32767).rounded())
    }

    /// Seconds of audio in `bytes` of 16 kHz s16le mono.
    static func seconds(bytes: Int, rate: Double = 16_000) -> TimeInterval {
        TimeInterval(bytes / 2) / rate
    }

    /// 0...1 loudness of s16le samples, on the voice-note meter's scale (-50 dB room noise ... 0 dB, a curve so
    /// speech fills the bar).
    static func level(of pcm: Data) -> Float {
        let count = pcm.count / 2
        guard count > 0 else { return 0 }
        var sum = 0.0
        pcm.withUnsafeBytes { raw in
            for i in 0..<count {
                let value = Double(Int16(littleEndian: raw.loadUnaligned(fromByteOffset: i * 2, as: Int16.self))) / 32768
                sum += value * value
            }
        }
        let rms = (sum / Double(count)).squareRoot()
        let db = rms > 0 ? 20 * log10(rms) : -160
        return Float(pow(max(0, min(1, (db + 50) / 50)), 1.6))
    }
}

/// A 16 kHz mono PCM WAV written as it arrives; `finish` patches the sizes in the header.
final class WAVWriter: @unchecked Sendable {
    let url: URL
    private let rate: UInt32
    private let lock = NSLock()
    private var handle: FileHandle?
    private(set) var dataBytes: UInt32 = 0

    init?(url: URL, rate: UInt32 = 16_000) {
        self.url = url
        self.rate = rate
        try? FileManager.default.removeItem(at: url)
        guard FileManager.default.createFile(atPath: url.path, contents: Self.header(dataBytes: 0, rate: rate)),
              let handle = try? FileHandle(forWritingTo: url) else { return nil }
        try? handle.seekToEnd()
        self.handle = handle
    }

    /// The 44-byte RIFF header of 16-bit mono PCM.
    static func header(dataBytes: UInt32, rate: UInt32 = 16_000) -> Data {
        func le32(_ v: UInt32) -> [UInt8] { (0..<4).map { UInt8(truncatingIfNeeded: v >> (8 * UInt32($0))) } }
        func le16(_ v: UInt16) -> [UInt8] { [UInt8(truncatingIfNeeded: v), UInt8(truncatingIfNeeded: v >> 8)] }
        var out: [UInt8] = Array("RIFF".utf8)
        out += le32(36 &+ dataBytes)
        out += Array("WAVEfmt ".utf8)
        out += le32(16) + le16(1) + le16(1) + le32(rate) + le32(rate * 2) + le16(2) + le16(16)
        out += Array("data".utf8)
        out += le32(dataBytes)
        return Data(out)
    }

    func append(_ bytes: Data) {
        lock.lock()
        defer { lock.unlock() }
        guard let handle else { return }
        try? handle.write(contentsOf: bytes)
        dataBytes &+= UInt32(bytes.count)
    }

    func finish() {
        lock.lock()
        defer { lock.unlock() }
        guard let handle else { return }
        try? handle.seek(toOffset: 0)
        try? handle.write(contentsOf: Self.header(dataBytes: dataBytes, rate: rate))
        try? handle.close()
        self.handle = nil
    }
}

/// Hardware buffers to 16 kHz mono s16le bytes.
final class AudioStreamConverter {
    static let output = AVAudioFormat(commonFormat: .pcmFormatInt16, sampleRate: 16_000, channels: 1, interleaved: true)!
    private let converter: AVAudioConverter
    private let inputRate: Double

    init?(input: AVAudioFormat) {
        guard input.sampleRate > 0, let converter = AVAudioConverter(from: input, to: Self.output) else { return nil }
        self.converter = converter
        inputRate = input.sampleRate
    }

    func convert(_ buffer: AVAudioPCMBuffer) -> Data? {
        let capacity = AVAudioFrameCount(PCM16.outputCapacity(inFrames: Int(buffer.frameLength), inRate: inputRate))
        guard capacity > 0, let out = AVAudioPCMBuffer(pcmFormat: Self.output, frameCapacity: capacity) else { return nil }
        var supplied = false
        var failure: NSError?
        let status = converter.convert(to: out, error: &failure) { _, outStatus in
            if supplied {
                outStatus.pointee = .noDataNow
                return nil
            }
            supplied = true
            outStatus.pointee = .haveData
            return buffer
        }
        guard status != .error, failure == nil, out.frameLength > 0, let samples = out.int16ChannelData?[0] else { return nil }
        return Data(bytes: samples, count: Int(out.frameLength) * 2)
    }
}

/// What the engine's tap does with each buffer; lives on the audio thread, so it holds no actor state.
final class CaptureTap: @unchecked Sendable {
    private let lock = NSLock()
    private var converter: AudioStreamConverter
    private var packer = PCMPacker()
    private let writer: WAVWriter
    private let onFrame: @Sendable (Data) -> Void
    private let onBuffer: (@Sendable (AVAudioPCMBuffer) -> Void)?
    private var bytes = 0
    private var peak: Float = 0
    private var closed = false

    init(converter: AudioStreamConverter, writer: WAVWriter, onFrame: @escaping @Sendable (Data) -> Void,
         onBuffer: (@Sendable (AVAudioPCMBuffer) -> Void)?) {
        self.converter = converter
        self.writer = writer
        self.onFrame = onFrame
        self.onBuffer = onBuffer
    }

    func process(_ buffer: AVAudioPCMBuffer) {
        onBuffer?(buffer)
        lock.lock()
        guard !closed, let data = converter.convert(buffer) else {
            lock.unlock()
            return
        }
        writer.append(data)
        bytes += data.count
        peak = max(peak, PCM16.level(of: data))
        let frames = packer.append(data)
        lock.unlock()
        for frame in frames { onFrame(frame) }
    }

    func setConverter(_ converter: AudioStreamConverter) {
        lock.lock()
        self.converter = converter
        lock.unlock()
    }

    var seconds: TimeInterval {
        lock.lock()
        defer { lock.unlock() }
        return PCM16.seconds(bytes: bytes)
    }

    /// The loudest level since the last call.
    func drainLevel() -> Float {
        lock.lock()
        defer { lock.unlock() }
        defer { peak = 0 }
        return peak
    }

    /// No more buffers: the last partial frame goes out (unless `flush` is false) and the WAV is closed.
    func finish(flush: Bool = true) {
        lock.lock()
        closed = true
        let rest = flush ? packer.flush() : nil
        lock.unlock()
        if let rest { onFrame(rest) }
        writer.finish()
    }
}

/// What the recording bar reads; both the AAC recorder and the streaming capture have it.
@MainActor
protocol VoiceMeter: AnyObject {
    var state: MemoRecorder.State { get }
    var elapsed: TimeInterval { get }
    var levels: [Float] { get }
    var levelHistory: [Float] { get }
    var isActive: Bool { get }
    var error: String? { get }
}

extension MemoRecorder: VoiceMeter {}

/// One `AVAudioEngine` input tap that feeds the voice socket (16 kHz mono s16le frames) and a WAV file.
@Observable
@MainActor
final class VoiceCapture: VoiceMeter {
    struct Finished {
        var url: URL
        var duration: TimeInterval
        var startedAt: Date
    }

    private(set) var state: MemoRecorder.State = .idle
    private(set) var elapsed: TimeInterval = 0
    private(set) var levels: [Float] = []
    private(set) var error: String?
    @ObservationIgnored private(set) var levelHistory: [Float] = []

    var isActive: Bool { state != .idle }

    @ObservationIgnored private let engine = AVAudioEngine()
    @ObservationIgnored private var tap: CaptureTap?
    @ObservationIgnored private var startedAt = Date()
    @ObservationIgnored private var meterTask: Task<Void, Never>?
    @ObservationIgnored private var observers: [NSObjectProtocol] = []
    @ObservationIgnored private var releasesSession = true
    @ObservationIgnored private var writerURL: URL?
    /// Only one capture holds the microphone (the chat's voice note, the Lucina tab's command).
    private static weak var holder: VoiceCapture?

    /// Starts the microphone. `onFrame` gets 60 ms frames from the audio thread; `onBuffer` the hardware buffers
    /// (for the on-screen live text). False, with `error`, when the microphone could not start.
    func start(category: AVAudioSession.Category, options: AVAudioSession.CategoryOptions = [], releasesSession: Bool = true,
               onFrame: @escaping @Sendable (Data) -> Void, onBuffer: (@Sendable (AVAudioPCMBuffer) -> Void)? = nil) async -> Bool {
        guard state == .idle else { return false }
        error = nil
        if MemoRecorder.isAnyActive || (Self.holder.map { $0 !== self && $0.isActive } ?? false) {
            error = String(localized: "A voice memo is recording. Stop it first.")
            return false
        }
        guard await AVAudioApplication.requestRecordPermission() else {
            error = String(localized: "Secretary can't use the microphone. Allow it in Settings › Privacy › Microphone.")
            return false
        }
        guard state == .idle else { return false }
        let session = AVAudioSession.sharedInstance()
        do {
            try session.setCategory(category, mode: .default, options: options)
            try? session.setPrefersNoInterruptionsFromSystemAlerts(true)
            try session.setActive(true)
        } catch {
            self.error = String(localized: "The microphone is busy: \(error.localizedDescription)")
            return false
        }
        NowPlayingSession.deactivate()
        let format = engine.inputNode.outputFormat(forBus: 0)
        let url = VoiceNoteFiles.directory.appendingPathComponent(VoiceNoteFiles.fileName(for: Date(), ext: "wav"))
        guard format.sampleRate > 0, format.channelCount > 0, let converter = AudioStreamConverter(input: format),
              let writer = WAVWriter(url: url) else {
            self.error = String(localized: "Recording did not start.")
            if releasesSession { try? session.setActive(false, options: .notifyOthersOnDeactivation) }
            return false
        }
        let tap = CaptureTap(converter: converter, writer: writer, onFrame: onFrame, onBuffer: onBuffer)
        engine.inputNode.removeTap(onBus: 0)
        engine.inputNode.installTap(onBus: 0, bufferSize: 1024, format: format) { buffer, _ in tap.process(buffer) }
        engine.prepare()
        do {
            try engine.start()
        } catch {
            engine.inputNode.removeTap(onBus: 0)
            writer.finish()
            try? FileManager.default.removeItem(at: url)
            self.error = error.localizedDescription
            if releasesSession { try? session.setActive(false, options: .notifyOthersOnDeactivation) }
            return false
        }
        self.tap = tap
        self.releasesSession = releasesSession
        writerURL = url
        startedAt = Date()
        levels = []
        levelHistory = []
        elapsed = 0
        state = .recording
        Self.holder = self
        observe()
        meterTask = Task { [weak self] in await self?.meter() }
        return true
    }

    /// Stops and hands back the WAV; the rest of the audio goes out first. Nil when under half a second was recorded.
    func stop() -> Finished? {
        guard state != .idle, let tap, let url = writerURL else { return nil }
        let duration = tap.seconds
        engine.inputNode.removeTap(onBus: 0)
        engine.stop()
        tap.finish()
        teardown()
        guard duration >= 0.5 else {
            try? FileManager.default.removeItem(at: url)
            return nil
        }
        return Finished(url: url, duration: duration, startedAt: startedAt)
    }

    /// Stops and throws the recording away.
    func discard() {
        guard state != .idle else { return }
        engine.inputNode.removeTap(onBus: 0)
        engine.stop()
        tap?.finish(flush: false)
        if let writerURL { try? FileManager.default.removeItem(at: writerURL) }
        teardown()
    }

    private func teardown() {
        meterTask?.cancel()
        meterTask = nil
        for o in observers { NotificationCenter.default.removeObserver(o) }
        observers = []
        tap = nil
        writerURL = nil
        state = .idle
        if Self.holder === self { Self.holder = nil }
        try? AVAudioSession.sharedInstance().setPrefersNoInterruptionsFromSystemAlerts(false)
        if releasesSession { try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation) }
    }

    private func meter() async {
        while !Task.isCancelled, state != .idle {
            if let tap {
                let level = tap.drainLevel()
                levels.append(level)
                levelHistory.append(level)
                if levels.count > MemoRecorder.maxLevels { levels.removeFirst(levels.count - MemoRecorder.maxLevels) }
                elapsed = tap.seconds
            }
            try? await Task.sleep(for: .milliseconds(50))
        }
    }

    private func observe() {
        let center = NotificationCenter.default
        // A route change (headphones, Bluetooth) stops the engine and may change the input format: tap again.
        observers.append(center.addObserver(forName: .AVAudioEngineConfigurationChange, object: engine, queue: .main) { [weak self] _ in
            Task { @MainActor in self?.rebuildTap() }
        })
        observers.append(center.addObserver(forName: AVAudioSession.interruptionNotification, object: nil, queue: .main) { [weak self] note in
            let began = (note.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt) == AVAudioSession.InterruptionType.began.rawValue
            Task { @MainActor in
                guard began, let self, self.state != .idle else { return }
                self.error = String(localized: "Recording was interrupted.")
                self.discard()
            }
        })
    }

    private func rebuildTap() {
        guard state != .idle, let tap else { return }
        engine.inputNode.removeTap(onBus: 0)
        let format = engine.inputNode.outputFormat(forBus: 0)
        guard format.sampleRate > 0, let converter = AudioStreamConverter(input: format) else {
            error = String(localized: "Recording did not start.")
            discard()
            return
        }
        tap.setConverter(converter)
        engine.inputNode.installTap(onBus: 0, bufferSize: 1024, format: format) { buffer, _ in tap.process(buffer) }
        engine.prepare()
        do { try engine.start() } catch {
            self.error = error.localizedDescription
            discard()
        }
    }
}

/// Plays the server's s16le mono audio as it arrives: buffers are scheduled back to back on a player node.
@MainActor
final class PCMStreamPlayer {
    /// Playback starts once this much audio is queued (or the stream ended), so the first words don't stutter.
    nonisolated static let prebuffer: TimeInterval = 0.12

    private let engine = AVAudioEngine()
    private let node = AVAudioPlayerNode()
    private var format: AVAudioFormat?
    private var rate = 0
    private var scheduled = 0
    private var queued: TimeInterval = 0
    private var playing = false
    private var inputDone = false
    private var spare = Data()
    private var generation = 0
    /// Everything scheduled has been played and no more is coming.
    var onDrained: (() -> Void)?
    private(set) var hasAudio = false

    /// s16le bytes to Float samples (a half sample at the end is dropped).
    nonisolated static func floats(fromS16le data: Data) -> [Float] {
        let count = data.count / 2
        return data.withUnsafeBytes { raw in
            (0..<count).map { Float(Int16(littleEndian: raw.loadUnaligned(fromByteOffset: $0 * 2, as: Int16.self))) / 32768 }
        }
    }

    func schedule(_ bytes: Data, rate: Int) {
        var data = spare + bytes
        spare = Data()
        if data.count % 2 == 1 { spare = Data([data.removeLast()]) }
        guard !data.isEmpty, rate > 0 else { return }
        if format == nil || self.rate != rate {
            guard prepare(rate: rate) else { return }
        }
        guard let format else { return }
        let samples = Self.floats(fromS16le: data)
        guard let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: AVAudioFrameCount(samples.count)),
              let channel = buffer.floatChannelData?[0] else { return }
        samples.withUnsafeBufferPointer { channel.update(from: $0.baseAddress!, count: samples.count) }
        buffer.frameLength = AVAudioFrameCount(samples.count)
        hasAudio = true
        scheduled += 1
        queued += TimeInterval(samples.count) / TimeInterval(rate)
        let mine = generation
        node.scheduleBuffer(buffer, completionCallbackType: .dataPlayedBack) { [weak self] _ in
            Task { @MainActor in
                guard let self, mine == self.generation else { return }
                self.scheduled -= 1
                self.checkDrained()
            }
        }
        if !playing, queued >= Self.prebuffer { play() }
    }

    /// The server is done sending.
    func finishInput() {
        inputDone = true
        if !playing, scheduled > 0 { play() }
        checkDrained()
    }

    func stop() {
        generation += 1
        node.stop()
        if engine.isRunning { engine.stop() }
        scheduled = 0
        queued = 0
        playing = false
        inputDone = false
        spare = Data()
        hasAudio = false
        format = nil
        onDrained = nil
    }

    private func prepare(rate: Int) -> Bool {
        guard let format = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: Double(rate), channels: 1, interleaved: false) else { return false }
        if engine.attachedNodes.contains(node) { engine.disconnectNodeOutput(node) } else { engine.attach(node) }
        engine.connect(node, to: engine.mainMixerNode, format: format)
        self.format = format
        self.rate = rate
        engine.prepare()
        do {
            try engine.start()
            return true
        } catch {
            return false
        }
    }

    private func play() {
        playing = true
        node.play()
    }

    private func checkDrained() {
        guard inputDone, scheduled == 0 else { return }
        let done = onDrained
        onDrained = nil
        done?()
    }
}

/// On-screen live text for the Lucina tab from the same tap (never the transcript the server keeps).
final class LivePartials: @unchecked Sendable {
    private let request = SFSpeechAudioBufferRecognitionRequest()
    private var task: SFSpeechRecognitionTask?

    /// Nil unless speech recognition is already allowed (no prompt) and available.
    init?(onPartial: @escaping @Sendable (String) -> Void) {
        guard SFSpeechRecognizer.authorizationStatus() == .authorized,
              let recognizer = SFSpeechRecognizer(locale: Locale(identifier: "it-IT")), recognizer.isAvailable else { return nil }
        request.shouldReportPartialResults = true
        task = recognizer.recognitionTask(with: request) { result, _ in
            if let result { onPartial(result.bestTranscription.formattedString) }
        }
    }

    func append(_ buffer: AVAudioPCMBuffer) { request.append(buffer) }

    func finish() {
        request.endAudio()
        task?.cancel()
        task = nil
    }
}
