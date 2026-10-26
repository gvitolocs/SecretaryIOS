import CryptoKit
import Foundation

// Voice memos (`/v1/memos`, served by lucina-memo on nezopt through p1's Caddy; docs/memos.md).

/// Where a memo is in the server's pipeline.
enum MemoStatus: String, Codable, Sendable {
    case uploading, queued, transcribing, summarizing, rendering, publishing, done, failed

    var isProcessing: Bool { [.queued, .transcribing, .summarizing, .rendering, .publishing].contains(self) }

    var label: String {
        switch self {
        case .uploading: String(localized: "Uploading")
        case .queued: String(localized: "Waiting")
        case .transcribing: String(localized: "Transcribing")
        case .summarizing: String(localized: "Lucina is summarising")
        case .rendering: String(localized: "Making the PDF")
        case .publishing: String(localized: "Saving to Obsidian")
        case .done: String(localized: "Ready")
        case .failed: String(localized: "Failed")
        }
    }
}

struct ServerMemo: Codable, Identifiable, Equatable, Sendable {
    let id: String
    let clientId: String
    var title: String
    let recordedAt: Date
    var durationSec: Double
    var sizeBytes: Int64
    var receivedBytes: Int64
    var status: MemoStatus
    var error: String?
    var language: String?
    var preview: String?
    var actionItems: Int?
    var tags: [String]?
    var obsidianNote: String?
    /// The calendar event it was recorded in, if any.
    var event: MemoEvent?
    /// `auto` (linked by the server) or `owner` (chosen on the memo screen; with no event = "none").
    var eventSource: String?
}

/// Where the iPhone was when a recording started (only with location access already granted).
struct MemoGeoFix: Codable, Equatable, Sendable {
    let lat: Double
    let lon: Double
    let accuracyM: Double?
}

/// The calendar event a memo belongs to: the server links the event that overlaps the
/// recording (a lecture, a meeting); the owner can pick another one.
struct MemoEvent: Codable, Equatable, Sendable, Hashable, Identifiable {
    let id: String
    let title: String
    let calendar: String?
    let startMs: Int64?
    let endMs: Int64?
    let location: String?
    let attendees: [String]?
    /// `lecture`, `meeting` or `other`.
    let kind: String?

    var start: Date? { startMs.map { Date(timeIntervalSince1970: Double($0) / 1000) } }
    var end: Date? { endMs.map { Date(timeIntervalSince1970: Double($0) / 1000) } }

    var kindLabel: String? {
        switch kind {
        case "lecture": String(localized: "Lecture")
        case "meeting": String(localized: "Meeting")
        default: nil
        }
    }
}

/// `GET /v1/memos/{id}/events`: the events around the recording, best match first.
struct MemoEventChoices: Decodable, Sendable {
    struct Candidate: Decodable, Sendable, Identifiable {
        let event: MemoEvent
        let overlapMs: Int64
        let locMatch: Bool
        var id: String { event.id }
    }

    let current: MemoEvent?
    let eventSource: String?
    let candidates: [Candidate]
}

struct MemoItem: Codable, Equatable, Sendable, Hashable {
    let text: String
    let at: Double?
    let owner: String?
    let due: String?
}

struct MemoSummary: Codable, Equatable, Sendable {
    let title: String
    let language: String
    let overview: String
    let keyPoints: [MemoItem]
    let decisions: [MemoItem]
    let actionItems: [MemoItem]
    let tags: [String]
    /// `lecture` or `meeting` when the memo is linked to such an event.
    let template: String?
    let definitions: [MemoItem]?
    /// Lectures: questions to review.
    let questions: [MemoItem]?
}

struct MemoSegment: Codable, Equatable, Sendable, Identifiable {
    var id: Double { start }
    let start: Double
    let end: Double
    let text: String
    let speaker: String?
}

struct MemoTranscript: Codable, Equatable, Sendable {
    let language: String
    let engine: String
    let durationSec: Double
    let segments: [MemoSegment]
}

struct MemoDetail: Decodable, Equatable, Sendable {
    let memo: ServerMemo
    let summary: MemoSummary?
    let pdf: Bool
}

enum MemoTime {
    /// `1:05`, `1:02:05`.
    static func clock(_ seconds: Double) -> String {
        let s = max(0, Int(seconds.isFinite ? seconds : 0))
        let (h, m, sec) = (s / 3600, (s / 60) % 60, s % 60)
        return h > 0 ? String(format: "%d:%02d:%02d", h, m, sec) : String(format: "%d:%02d", m, sec)
    }

    /// The server writes RFC 3339 with up to nanoseconds; Foundation reads milliseconds.
    static func parse(_ raw: String) -> Date? {
        let plain = ISO8601DateFormatter()
        plain.formatOptions = [.withInternetDateTime]
        guard let dot = raw.firstIndex(of: ".") else { return plain.date(from: raw) }
        let digits = raw[raw.index(after: dot)...].prefix { $0.isNumber }
        let rest = raw[raw.index(after: dot)...].dropFirst(digits.count)
        guard let base = plain.date(from: String(raw[..<dot]) + rest) else { return nil }
        let fraction = Double("0." + digits) ?? 0
        return base.addingTimeInterval(fraction)
    }

    static let decoder: JSONDecoder = {
        let d = JSONDecoder()
        d.dateDecodingStrategy = .custom { decoder in
            let c = try decoder.singleValueContainer()
            let s = try c.decode(String.self)
            guard let date = parse(s) else { throw DecodingError.dataCorruptedError(in: c, debugDescription: "bad date \(s)") }
            return date
        }
        return d
    }()

    static let encoder: JSONEncoder = {
        let e = JSONEncoder()
        e.dateEncodingStrategy = .iso8601
        e.outputFormatting = [.sortedKeys]
        return e
    }()
}

/// The memo endpoints. Always the public URL: the tailnet fallback reaches secretary-server
/// directly, without the Caddy route to lucina-memo.
struct MemosAPI: Sendable {
    let client: APIClient

    /// `client`: AppModel's memo client (the primary URL only, the device key kept in sync).
    init(client: APIClient) {
        self.client = client
    }

    private static let headers = ["X-Secretary-Client": "ios"]

    private func decode<T: Decodable>(_ type: T.Type, _ r: APIClient.Response) throws -> T {
        do { return try MemoTime.decoder.decode(T.self, from: r.data) } catch {
            throw APIError.decoding(message: String(describing: error))
        }
    }

    private struct One: Decodable { let memo: ServerMemo }
    private struct Many: Decodable { let memos: [ServerMemo] }
    private struct Transcript: Decodable { let transcript: MemoTranscript }

    struct Create: Encodable, Sendable {
        let clientId: String
        let title: String?
        let recordedAt: Date
        let timezone: String
        let durationSec: Double
        let mime: String
        let sizeBytes: Int64
        let source: String
        /// Wall-clock end (pauses included), for matching the calendar.
        let endedAt: Date?
        let location: MemoGeoFix?
    }

    func list(limit: Int = 100) async throws -> [ServerMemo] {
        let r = try await client.send("GET", "/v1/memos", query: [URLQueryItem(name: "limit", value: String(limit))], body: nil)
        return try decode(Many.self, r).memos
    }

    func detail(_ id: String) async throws -> MemoDetail {
        try decode(MemoDetail.self, try await client.send("GET", "/v1/memos/\(id)", query: [], body: nil))
    }

    func transcript(_ id: String) async throws -> MemoTranscript {
        try decode(Transcript.self, try await client.send("GET", "/v1/memos/\(id)/transcript", query: [], body: nil)).transcript
    }

    /// Idempotent by `clientId`: a retried create returns the same memo.
    func create(_ body: Create) async throws -> ServerMemo {
        let data = try MemoTime.encoder.encode(body)
        return try decode(One.self, try await client.send("POST", "/v1/memos", query: [], body: data, idempotent: true, headers: Self.headers)).memo
    }

    private struct Offset: Decodable { let receivedBytes: Int64 }

    /// Sends `chunk` at `offset`; returns the server's byte count afterwards (also on a 409
    /// "offset mismatch", which carries where the server really is).
    func upload(_ id: String, offset: Int64, chunk: Data) async throws -> Int64 {
        let r = try await client.send("PUT", "/v1/memos/\(id)/audio", query: [], body: chunk, timeout: 180, passThrough: [409],
                                      idempotent: true, headers: Self.headers.merging(["Upload-Offset": String(offset)]) { a, _ in a })
        return try decode(Offset.self, r).receivedBytes
    }

    /// The iPhone's own transcript (short memos), used by the server while its GPU is busy.
    func deviceTranscript(_ id: String, _ t: MemoDeviceTranscript) async throws {
        let data = try MemoTime.encoder.encode(t)
        _ = try await client.send("PUT", "/v1/memos/\(id)/device-transcript", query: [], body: data, timeout: 60,
                                  idempotent: true, headers: Self.headers)
    }

    func complete(_ id: String, sha256: String) async throws -> ServerMemo {
        let data = try MemoTime.encoder.encode(["sha256": sha256])
        return try decode(One.self, try await client.send("POST", "/v1/memos/\(id)/complete", query: [], body: data, timeout: 60,
                                                          idempotent: true, headers: Self.headers)).memo
    }

    func rename(_ id: String, title: String) async throws -> ServerMemo {
        let data = try MemoTime.encoder.encode(["title": title])
        return try decode(One.self, try await client.send("PATCH", "/v1/memos/\(id)", query: [], body: data, headers: Self.headers)).memo
    }

    func events(_ id: String) async throws -> MemoEventChoices {
        try decode(MemoEventChoices.self, try await client.send("GET", "/v1/memos/\(id)/events", query: [], body: nil, timeout: 30))
    }

    private struct EventChoice: Encodable {
        let eventId: String?

        enum CodingKeys: String, CodingKey { case eventId }

        func encode(to encoder: Encoder) throws {
            var c = encoder.container(keyedBy: CodingKeys.self)
            try c.encode(eventId, forKey: .eventId) // null = "no event"
        }
    }

    /// Links the memo to `eventId` (nil: to no event); the server writes the summary again.
    func setEvent(_ id: String, eventId: String?) async throws -> ServerMemo {
        let data = try MemoTime.encoder.encode(EventChoice(eventId: eventId))
        return try decode(One.self, try await client.send("PUT", "/v1/memos/\(id)/event", query: [], body: data, timeout: 30,
                                                          idempotent: true, headers: Self.headers)).memo
    }

    func delete(_ id: String) async throws {
        _ = try await client.send("DELETE", "/v1/memos/\(id)", query: [], body: nil, headers: Self.headers)
    }

    func resummarize(_ id: String, instructions: String?) async throws -> ServerMemo {
        let data = try MemoTime.encoder.encode(["instructions": instructions ?? ""])
        return try decode(One.self, try await client.send("POST", "/v1/memos/\(id)/resummarize", query: [], body: data, idempotent: true,
                                                          headers: Self.headers)).memo
    }

    /// Downloads the audio or the PDF to `to`.
    func download(_ id: String, _ what: String, to: URL) async throws {
        let r = try await client.send("GET", "/v1/memos/\(id)/\(what)", query: [], body: nil, timeout: 300)
        try r.data.write(to: to, options: .atomic)
    }

    static func sha256(of url: URL) throws -> String {
        let handle = try FileHandle(forReadingFrom: url)
        defer { try? handle.close() }
        var hasher = SHA256()
        while let chunk = try handle.read(upToCount: 1 << 20), !chunk.isEmpty { hasher.update(data: chunk) }
        return hasher.finalize().map { String(format: "%02x", $0) }.joined()
    }
}
