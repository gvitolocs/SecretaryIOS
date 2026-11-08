import Foundation
import os

// The streaming voice socket's messages (docs/lucinavoice-protocol.md sections 2 and 10), as pure values so they
// can be unit-tested: the `start` JSON, the server's messages, the reconnect backoff and the timing marks.

/// What the client asks for in `start`. The codec is always `pcm16` at 16 kHz.
struct VoiceStart: Equatable, Sendable {
    enum Mode: String, Sendable { case ptt, vad }
    enum Purpose: String, Sendable { case command, note }
    enum Thread: String, Sendable { case voice, main }

    var mode: Mode
    var purpose: Purpose
    var thread: Thread = .main
    var lang: String?
    var sessionId: String?
    /// Note only: the timeline id the note answers (swipe to reply).
    var replyTo: String?
    /// Note only: `ios-chat`.
    var source: String?
    /// Command only: also send the reply as text.
    var text: Bool?
    /// Command only: false = no speech at all.
    var speak: Bool?

    static let rate = 16_000

    /// A voice note from the chat mic: held to talk, answered in the chat.
    static func note(sessionId: String?, replyTo: String?, lang: String? = nil) -> VoiceStart {
        VoiceStart(mode: .ptt, purpose: .note, thread: .main, lang: lang, sessionId: sessionId, replyTo: replyTo, source: "ios-chat")
    }

    /// A spoken command from the Lucina tab: the server's VAD ends it.
    static func command(sessionId: String?, speak: Bool, lang: String = "it") -> VoiceStart {
        VoiceStart(mode: .vad, purpose: .command, thread: .main, lang: lang, sessionId: sessionId, text: true, speak: speak)
    }

    /// `[A-Za-z0-9_-]{1,64}`: what the server accepts as an id (a bad value is `bad_request`, so it is left out).
    static func isSafeId(_ id: String?) -> Bool {
        guard let id, (1...64).contains(id.utf8.count) else { return false }
        return id.utf8.allSatisfy { ($0 >= 48 && $0 <= 57) || ($0 >= 65 && $0 <= 90) || ($0 >= 97 && $0 <= 122) || $0 == 95 || $0 == 45 }
    }

    /// The text frame, keys sorted.
    func json() -> String {
        var object: [String: Any] = [
            "type": "start", "mode": mode.rawValue, "purpose": purpose.rawValue, "codec": "pcm16", "rate": Self.rate,
            "thread": thread.rawValue,
        ]
        if let lang, !lang.isEmpty { object["lang"] = lang }
        if Self.isSafeId(sessionId) { object["sessionId"] = sessionId }
        if purpose == .note {
            if Self.isSafeId(replyTo) { object["replyTo"] = replyTo }
            if let source { object["source"] = source }
        } else {
            if let text { object["text"] = text }
            if let speak { object["speak"] = speak }
        }
        let data = (try? JSONSerialization.data(withJSONObject: object, options: [.sortedKeys])) ?? Data("{}".utf8)
        return String(decoding: data, as: UTF8.self)
    }

    static let end = #"{"type":"end"}"#
    static let cancel = #"{"type":"cancel"}"#
}

/// One message from the server.
enum VoiceEvent: Equatable, Sendable {
    struct Listening: Equatable, Sendable {
        var purpose: String?
        var codec: String?
        var rate: Int?
        var turn: String?
        /// A server from before section 10 answers a bare `listening`.
        var understandsStreaming: Bool { codec != nil }
    }

    case listening(Listening)
    case endpoint
    case transcript(String)
    /// `{"type":"audio","rate":24000}`: the binary frames that follow are s16le mono at this rate.
    case audioFormat(rate: Int)
    case audio(Data)
    case delta(String)
    case answer(String, replaces: Bool)
    case reply(text: String, shareId: String?, itemId: String?)
    case note(shareId: String, userItemId: String?)
    case done(ms: [String: Int], shareId: String?)
    case error(code: String, message: String, shareId: String?)
    /// A message type this client does not know: ignored.
    case unknown(String)
    /// The socket closed (local, never sent by the server).
    case closed

    /// The note a message belongs to, when it carries a `shareId` (a new turn may already be running).
    var shareId: String? {
        switch self {
        case let .reply(_, shareId, _): return shareId
        case let .done(_, shareId): return shareId
        case let .error(_, _, shareId): return shareId
        case let .note(shareId, _): return shareId
        default: return nil
        }
    }

    static func parse(data: Data) -> VoiceEvent { .audio(data) }

    static func parse(text: String) -> VoiceEvent? {
        guard let data = text.data(using: .utf8),
              let object = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any],
              let type = object["type"] as? String else { return nil }
        let shareId = object["shareId"] as? String
        switch type {
        case "listening":
            return .listening(Listening(purpose: object["purpose"] as? String, codec: object["codec"] as? String,
                                        rate: (object["rate"] as? NSNumber)?.intValue, turn: object["turn"] as? String))
        case "endpoint": return .endpoint
        case "transcript": return .transcript(object["text"] as? String ?? "")
        case "audio": return .audioFormat(rate: (object["rate"] as? NSNumber)?.intValue ?? 24_000)
        case "delta": return .delta(object["text"] as? String ?? "")
        case "answer": return .answer(object["text"] as? String ?? "", replaces: object["replaces"] as? Bool ?? false)
        case "reply": return .reply(text: object["text"] as? String ?? "", shareId: shareId, itemId: object["itemId"] as? String)
        case "note":
            guard let shareId else { return .unknown(type) }
            return .note(shareId: shareId, userItemId: object["userItemId"] as? String)
        case "done":
            var ms: [String: Int] = [:]
            for (key, value) in object["ms"] as? [String: Any] ?? [:] {
                if let number = value as? NSNumber, !(value is Bool) { ms[key] = number.intValue }
            }
            return .done(ms: ms, shareId: shareId)
        case "error":
            return .error(code: object["code"] as? String ?? "error", message: object["message"] as? String ?? "", shareId: shareId)
        default:
            return .unknown(type)
        }
    }
}

/// Reconnect delays: 1, 2, 4, 8, 16, 30, 30 … seconds.
enum VoiceBackoff {
    static let maximum: TimeInterval = 30

    static func delay(attempt: Int) -> TimeInterval {
        let exponent = max(0, min(attempt, 6))
        return min(maximum, TimeInterval(1 << exponent))
    }
}

/// `end → transcript`, `end → first delta/answer`, `end → first audio`, `end → note`, `end → reply`, logged at
/// debug level (never the audio, the text or a token).
struct VoiceTimings {
    enum Mark: String, Sendable { case transcript, text, audio, note, reply }

    private(set) var ended: ContinuousClock.Instant?
    private var seen: Set<Mark> = []

    mutating func end(at now: ContinuousClock.Instant = .now) {
        ended = now
        seen = []
    }

    /// Milliseconds since `end` for the first time `mark` is seen; nil before `end` or for a repeat.
    @discardableResult
    mutating func mark(_ mark: Mark, at now: ContinuousClock.Instant = .now) -> Int? {
        guard let ended, seen.insert(mark).inserted else { return nil }
        let parts = (now - ended).components
        return Int(parts.seconds * 1000 + parts.attoseconds / 1_000_000_000_000_000)
    }

    static let log = Logger(subsystem: "tech.getsecretary.voice", category: "stream")

    mutating func log(_ mark: Mark, purpose: String, at now: ContinuousClock.Instant = .now) {
        if let ms = self.mark(mark, at: now) {
            Self.log.debug("\(purpose, privacy: .public): end -> \(mark.rawValue, privacy: .public) \(ms) ms")
        }
    }
}
