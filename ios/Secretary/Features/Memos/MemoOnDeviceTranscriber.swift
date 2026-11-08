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

    /// The language to transcribe in (ISO 639-1): the one most of the owner's recent memos are
    /// in (Whisper detects it on the server), else the app's language. A memo in another language
    /// gets a poor transcript here; the server's Whisper replaces it.
    static func language(recent: [String]) -> String {
        let counts = Dictionary(recent.prefix(8).map { ($0, 1) }, uniquingKeysWith: +)
        return counts.max { $0.value < $1.value || ($0.value == $1.value && $0.key > $1.key) }?.key ?? AppLanguage.effectiveCode
    }

    static func locale(for language: String) -> Locale {
        switch language {
        case "it": Locale(identifier: "it-IT")
        case "da": Locale(identifier: "da-DK")
        case "de": Locale(identifier: "de-DE")
        case "es": Locale(identifier: "es-ES")
        case "fr": Locale(identifier: "fr-FR")
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
    static func transcribe(_ url: URL, clientId: String, language: String) async -> MemoDeviceTranscript? {
        guard #available(iOS 26.0, *) else { MemoLog.write("transcribe: iOS < 26"); return nil }
        do {
            guard let t = try await run(url, locale: locale(for: language)) else { MemoLog.write("transcribe \(clientId): no transcript"); return nil }
            MemoLog.write("transcribe \(clientId): \(t.segments.count) segments, \(t.text.count) chars, \(t.language)")
            if let data = try? MemoTime.encoder.encode(t) { try? data.write(to: file(for: clientId), options: .atomic) }
            return t
        } catch {
            MemoLog.write("transcribe \(clientId) failed: \(error)")
            return nil
        }
    }

    @available(iOS 26.0, *)
    private static func run(_ url: URL, locale wanted: Locale) async throws -> MemoDeviceTranscript? {
        // SpeechTranscriber (long-form) first; DictationTranscriber covers more languages.
        if let locale = await SpeechTranscriber.supportedLocale(equivalentTo: wanted) {
            let module = SpeechTranscriber(locale: locale, transcriptionOptions: [], reportingOptions: [], attributeOptions: [.audioTimeRange])
            let segments = try await analyze(url, modules: [module], results: module.results.map { ($0.text, $0.range, $0.isFinal) })
            return transcript(segments, url: url, locale: locale, engine: "Apple SpeechTranscriber (iPhone)")
        }
        if let locale = await DictationTranscriber.supportedLocale(equivalentTo: wanted) {
            let module = DictationTranscriber(locale: locale, contentHints: [], transcriptionOptions: [.punctuation], reportingOptions: [],
                                              attributeOptions: [.audioTimeRange])
            let segments = try await analyze(url, modules: [module], results: module.results.map { ($0.text, $0.range, $0.isFinal) })
            return transcript(segments, url: url, locale: locale, engine: "Apple DictationTranscriber (iPhone)")
        }
        MemoLog.write("transcribe: no on-device model for \(wanted.identifier)")
        return nil
    }

    @available(iOS 26.0, *)
    private static func analyze<S: AsyncSequence>(_ url: URL, modules: [any SpeechModule], results: S) async throws -> [MemoSegment]
    where S.Element == (AttributedString, CMTimeRange, Bool) {
        if let install = try await AssetInventory.assetInstallationRequest(supporting: modules) {
            MemoLog.write("transcribe: installing the speech model")
            try await install.downloadAndInstall()
        }
        let analyzer = SpeechAnalyzer(modules: modules)
        let collect = Task { () throws -> [MemoSegment] in
            var segments: [MemoSegment] = []
            for try await (text, range, final) in results where final {
                let t = String(text.characters).trimmingCharacters(in: .whitespacesAndNewlines)
                guard !t.isEmpty else { continue }
                let start = range.start.seconds
                let end = range.end.seconds
                segments.append(MemoSegment(start: start.isFinite ? start : 0, end: end.isFinite ? end : 0, text: t, speaker: nil))
            }
            return segments
        }
        let audio = try AVAudioFile(forReading: url)
        if let last = try await analyzer.analyzeSequence(from: audio) {
            try await analyzer.finalizeAndFinish(through: last)
        } else {
            await analyzer.cancelAndFinishNow()
        }
        return try await collect.value
    }

    private static func transcript(_ segments: [MemoSegment], url: URL, locale: Locale, engine: String) -> MemoDeviceTranscript? {
        guard !segments.isEmpty, let audio = try? AVAudioFile(forReading: url) else { return nil }
        let duration = Double(audio.length) / audio.processingFormat.sampleRate
        return MemoDeviceTranscript(language: String(locale.identifier.prefix(2)), engine: engine, durationSec: duration, segments: segments)
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
