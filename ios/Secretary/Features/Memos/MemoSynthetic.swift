#if DEBUG
import ActivityKit
import AVFoundation
import Foundation

/// Debug builds only: a memo spoken by the iPhone's speech synthesizer, queued like a real
/// recording, so the upload and pipeline can be tested on the device without the microphone.
/// Launch arguments: `-SecretaryMemoSynthetic "text"` and `-SecretaryMemoSyntheticLang it-IT`.
@MainActor
enum MemoSynthetic {
    static func make(text: String, language: String) async throws -> FinishedRecording {
        let synth = AVSpeechSynthesizer()
        let utterance = AVSpeechUtterance(string: text)
        utterance.voice = AVSpeechSynthesisVoice(language: language)
        let clientId = UUID().uuidString.lowercased()
        let url = MemoFiles.audio(clientId)
        let started = Date()
        var file: AVAudioFile?
        var frames: AVAudioFramePosition = 0
        var rate: Double = 22_050
        var failure: Error?
        // write() delivers buffers sentence by sentence (with empty ones between), so the end is
        // when no buffer has arrived for a while.
        var last = Date()
        var any = false
        synth.write(utterance) { buffer in
            guard let pcm = buffer as? AVAudioPCMBuffer, pcm.frameLength > 0 else { return }
            last = Date()
            any = true
            do {
                if file == nil {
                    rate = pcm.format.sampleRate
                    file = try AVAudioFile(forWriting: url, settings: [
                        AVFormatIDKey: kAudioFormatMPEG4AAC,
                        AVSampleRateKey: pcm.format.sampleRate,
                        AVNumberOfChannelsKey: Int(pcm.format.channelCount),
                        AVEncoderBitRateKey: 64_000,
                    ], commonFormat: pcm.format.commonFormat, interleaved: pcm.format.isInterleaved)
                }
                try file?.write(from: pcm)
                frames += AVAudioFramePosition(pcm.frameLength)
            } catch {
                failure = error
            }
        }
        let deadline = Date().addingTimeInterval(90)
        while Date() < deadline, !any || Date().timeIntervalSince(last) < 2 {
            try? await Task.sleep(for: .milliseconds(200))
        }
        file = nil
        if let failure { throw failure }
        return FinishedRecording(clientId: clientId, url: url, startedAt: started, duration: Double(frames) / rate)
    }

    /// Reads the launch arguments and queues the memo once per launch.
    static func runIfRequested(into store: MemosStore, defaults: UserDefaults = .standard) {
        guard let text = defaults.string(forKey: "SecretaryMemoSynthetic"), !text.isEmpty else { return }
        let lang = defaults.string(forKey: "SecretaryMemoSyntheticLang") ?? "it-IT"
        let title = defaults.object(forKey: "SecretaryMemoSyntheticTitle").map { ($0 as? String) ?? "\($0)" }
        MemoLog.write("synthetic: requested (\(text.count) chars, \(lang), title=\(title ?? "nil"))")
        Task { @MainActor in
            do {
                let r = try await make(text: text, language: lang)
                MemoLog.write("synthetic: \(String(format: "%.1f", r.duration)) s recorded")
                store.add(r, title: title)
            } catch {
                MemoLog.write("synthetic failed: \(error)")
            }
        }
    }
}

/// Debug builds only: `-SecretaryMemoRecordTest YES` records about four seconds and checks the
/// Live Activity through the same handler its Lock Screen buttons call (pause, resume), then
/// DISCARDS the recording. Every step goes to Memos/memo-log.txt.
@MainActor
enum MemoRecordSelfTest {
    static func runIfRequested(store: MemosStore, defaults: UserDefaults = .standard) {
        guard defaults.bool(forKey: "SecretaryMemoRecordTest") else { return }
        Task { @MainActor in
            let rec = store.recorder
            await store.record()
            try? await Task.sleep(for: .seconds(1))
            MemoLog.write("selftest: started state=\(rec.state) activities=\(Activity<MemoRecordingAttributes>.activities.count) live=\(rec.activityReport)")
            MemoRecordingCommands.handle?(.pause)
            try? await Task.sleep(for: .seconds(1.5))
            MemoLog.write("selftest: after pause intent state=\(rec.state) live=\(rec.activityReport) elapsed=\(String(format: "%.1f", rec.elapsed))")
            MemoRecordingCommands.handle?(.resume)
            try? await Task.sleep(for: .seconds(1.5))
            MemoLog.write("selftest: after resume intent state=\(rec.state) live=\(rec.activityReport)")
            rec.discard()
            try? await Task.sleep(for: .seconds(1.5))
            MemoLog.write("selftest: discarded state=\(rec.state) activities=\(Activity<MemoRecordingAttributes>.activities.count)")
        }
    }
}
#endif
