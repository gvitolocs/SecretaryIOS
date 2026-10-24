import AVFoundation
import Foundation
import Speech

/// Short memos are transcribed on the iPhone right after recording (Apple's on-device
/// SpeechTranscriber, iOS 26), so the owner sees the text at once and Lucina can summarise
/// even while the server's GPU is busy. The server still runs Whisper large-v3 when the GPU
/// is free and replaces this transcript. Long memos go to the server only.
enum MemoOnDeviceTranscriber {
    /// Memos up to this length are transcribed here.
    static let maxSeconds: Double = 600

    /// The language to transcribe in: the app's language (it, en or da).
    static var preferredLocale: Locale {
        switch AppLanguage.effectiveCode {
        case "it": Locale(identifier: "it-IT")
        case "da": Locale(identifier: "da-DK")
        default: Locale(identifier: "en-US")
        }
    }

    static func file(for clientId: String) -> URL {
        MemoFiles.directory.appendingPathComponent("\(clientId).transcript.json")
    }

    static func saved(_ clientId: String) -> MemoDeviceTranscript? {
        guard let data = try? Data(contentsOf: file(for: clientId)) else { return nil }
        return try? MemoTime.decoder.decode(MemoDeviceTranscript.self, from: data)
    }

    /// Transcribes `url` and saves the result next to the recording. Nil when the device
    /// can't (iOS < 26, unsupported language, no speech).
    static func transcribe(_ url: URL, clientId: String) async -> MemoDeviceTranscript? {
        guard #available(iOS 26.0, *) else { return nil }
        do {
            guard let t = try await run(url) else { return nil }
            if let data = try? MemoTime.encoder.encode(t) { try? data.write(to: file(for: clientId), options: .atomic) }
            return t
        } catch {
            print("on-device transcription failed: \(error)")
            return nil
        }
    }

    @available(iOS 26.0, *)
    private static func run(_ url: URL) async throws -> MemoDeviceTranscript? {
        guard let locale = await SpeechTranscriber.supportedLocale(equivalentTo: preferredLocale) else { return nil }
        let transcriber = SpeechTranscriber(locale: locale, transcriptionOptions: [], reportingOptions: [],
                                            attributeOptions: [.audioTimeRange])
        if let install = try await AssetInventory.assetInstallationRequest(supporting: [transcriber]) {
            try await install.downloadAndInstall()
        }
        let analyzer = SpeechAnalyzer(modules: [transcriber])
        let collect = Task { () throws -> [MemoSegment] in
            var segments: [MemoSegment] = []
            for try await result in transcriber.results where result.isFinal {
                let text = String(result.text.characters).trimmingCharacters(in: .whitespacesAndNewlines)
                guard !text.isEmpty else { continue }
                let start = result.range.start.seconds
                let end = result.range.end.seconds
                segments.append(MemoSegment(start: start.isFinite ? start : 0, end: end.isFinite ? end : 0, text: text, speaker: nil))
            }
            return segments
        }
        let audio = try AVAudioFile(forReading: url)
        if let last = try await analyzer.analyzeSequence(from: audio) {
            try await analyzer.finalizeAndFinish(through: last)
        } else {
            await analyzer.cancelAndFinishNow()
        }
        let segments = try await collect.value
        guard !segments.isEmpty else { return nil }
        let duration = Double(audio.length) / audio.processingFormat.sampleRate
        return MemoDeviceTranscript(language: String(locale.identifier.prefix(2)), engine: "Apple SpeechTranscriber (iPhone)",
                                    durationSec: duration, segments: segments)
    }
}

/// What the iPhone sends: `PUT /v1/memos/{id}/device-transcript`.
struct MemoDeviceTranscript: Codable, Equatable, Sendable {
    let language: String
    let engine: String
    let durationSec: Double
    let segments: [MemoSegment]

    var text: String { segments.map(\.text).joined(separator: " ") }
}
