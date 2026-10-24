import Foundation
import SecretaryCore

/// The Rust player core (`secretary-player` through `secretary-ffi`): the play queue,
/// resume positions and their sync bookkeeping, subtitle parsing. The web and Android
/// players run the same code, so "resume", "finished" and "next" mean the same everywhere.
///
/// Requests are JSON `{"cmd": ...}` and take microseconds; they run synchronously on a
/// private serial queue (one request at a time per handle, as the header requires).
final class PlayerCore: @unchecked Sendable {
    enum CoreError: LocalizedError {
        case open(String)
        case rejected(String)

        var errorDescription: String? {
            switch self {
            case let .open(message): return String(localized: "Couldn't open the player data: \(message)")
            case let .rejected(message): return message
            }
        }
    }

    private let handle: OpaquePointer
    private let serial = DispatchQueue(label: "tech.getsecretary.player-core", qos: .userInitiated)
    private let decoder = JSONDecoder()

    init(fileURL: URL) throws {
        try FileManager.default.createDirectory(at: fileURL.deletingLastPathComponent(), withIntermediateDirectories: true)
        guard let handle = fileURL.path.withCString({ secretary_player_open($0) }) else {
            var message = "unknown error"
            if let raw = secretary_open_error() {
                message = String(cString: raw)
                secretary_string_free(raw)
            }
            throw CoreError.open(message)
        }
        self.handle = handle
    }

    deinit {
        secretary_player_close(handle)
    }

    static var defaultFileURL: URL {
        let base = (try? FileManager.default.url(for: .applicationSupportDirectory, in: .userDomainMask, appropriateFor: nil, create: true))
            ?? FileManager.default.temporaryDirectory
        return base.appendingPathComponent("Secretary", isDirectory: true).appendingPathComponent("player.json")
    }

    private func raw(_ command: [String: Any]) throws -> Data {
        let json = try JSONSerialization.data(withJSONObject: command)
        return serial.sync {
            let text = String(decoding: json, as: UTF8.self)
            guard let out = text.withCString({ secretary_player_request(handle, $0) }) else { return Data() }
            defer { secretary_string_free(out) }
            return Data(String(cString: out).utf8)
        }
    }

    private func call<T: Decodable>(_ command: [String: Any], as type: T.Type = T.self) throws -> T {
        let data = try raw(command)
        let status = try decoder.decode(CoreStatus.self, from: data)
        guard status.ok else { throw CoreError.rejected(status.error ?? "The player core rejected a command.") }
        return try decoder.decode(T.self, from: data)
    }

    private struct CoreStatus: Decodable {
        let ok: Bool
        let error: String?
    }

    private static func json<T: Encodable>(_ value: T) -> Any {
        (try? JSONSerialization.jsonObject(with: JSONEncoder().encode(value))) ?? NSNull()
    }

    // MARK: Queue

    struct QueueView: Decodable {
        let items: [MediaItem]
        let current: Int?
        let hasNext: Bool
        let hasPrev: Bool

        private enum CodingKeys: String, CodingKey {
            case items, current
            case hasNext = "has_next"
            case hasPrev = "has_prev"
        }
    }

    private struct QueueAnswer: Decodable {
        let queue: QueueView
        let moved: Bool?
        let prev: String?
    }

    func setQueue(_ items: [MediaItem], start: Int) throws -> QueueView {
        try call(["cmd": "queue_set", "items": Self.json(items), "start": start], as: QueueAnswer.self).queue
    }

    func queue() throws -> QueueView {
        try call(["cmd": "queue_get"], as: QueueAnswer.self).queue
    }

    /// Moves on. `auto`: the item ended by itself. Returns nil at the end of the queue.
    func next(auto: Bool) throws -> QueueView? {
        let answer = try call(["cmd": "queue_next", "auto": auto], as: QueueAnswer.self)
        return answer.moved == true ? answer.queue : nil
    }

    /// Goes back, or answers `.restart` when the item has played for more than 3 s.
    enum Prev { case moved(QueueView), restart }

    func prev(positionMs: Int) throws -> Prev {
        let answer = try call(["cmd": "queue_prev", "position_ms": positionMs], as: QueueAnswer.self)
        return answer.prev == "moved" ? .moved(answer.queue) : .restart
    }

    func jump(to index: Int) throws -> QueueView {
        try call(["cmd": "queue_jump", "index": index], as: QueueAnswer.self).queue
    }

    // MARK: Positions

    struct Tick: Decodable {
        /// Push the pending positions to the server now.
        let sync: Bool
        let finished: Bool
    }

    func position(id: String, positionMs: Int, durationMs: Int, playing: Bool, device: String) throws -> Tick {
        try call(["cmd": "position", "id": id, "position_ms": max(0, positionMs), "duration_ms": max(0, durationMs),
                  "playing": playing, "device": device])
    }

    func resume(id: String, durationMs: Int?) throws -> Int {
        struct Answer: Decodable { let position_ms: Int }
        var command: [String: Any] = ["cmd": "resume", "id": id]
        if let durationMs { command["duration_ms"] = durationMs }
        return try call(command, as: Answer.self).position_ms
    }

    /// The saved entry for an item (for progress bars in lists).
    func entry(id: String) -> ProgressEntry? {
        struct Answer: Decodable { let entry: ProgressEntry? }
        return (try? call(["cmd": "resume", "id": id], as: Answer.self))?.entry
    }

    func pending() throws -> [ProgressEntry] {
        struct Answer: Decodable { let entries: [ProgressEntry] }
        return try call(["cmd": "pending"], as: Answer.self).entries
    }

    func acknowledge(_ acks: [ProgressSync.Ack]) throws {
        struct Answer: Decodable { let ok: Bool }
        let list = acks.map { ["id": $0.id, "updated_at": $0.updatedAt] as [String: Any] }
        _ = try call(["cmd": "acknowledge", "entries": list], as: Answer.self)
    }

    /// Adopts server positions that are newer than the local ones.
    @discardableResult
    func merge(_ entries: [ProgressEntry]) throws -> [String] {
        struct Answer: Decodable { let changed: [String] }
        guard !entries.isEmpty else { return [] }
        return try call(["cmd": "merge", "entries": Self.json(entries)], as: Answer.self).changed
    }

    func recent(limit: Int = 10) throws -> [ProgressEntry] {
        struct Answer: Decodable { let entries: [ProgressEntry] }
        return try call(["cmd": "recent", "limit": limit], as: Answer.self).entries
    }

    // MARK: Subtitles

    /// The track to show for an item: the owner's choice for this file, else the language
    /// they last turned on, else a forced track. nil = off.
    func pickSubtitle(id: String, tracks: [SubtitleTrack]) -> String? {
        struct Answer: Decodable { let track: String? }
        let list = tracks.map { ["id": $0.id, "lang": $0.lang ?? NSNull(), "forced": $0.forced, "kind": $0.kind] as [String: Any] }
        return (try? call(["cmd": "subtitle_pick", "id": id, "tracks": list], as: Answer.self))?.track
    }

    /// Remembers the owner's pick (nil = off) for this file.
    func chooseSubtitle(id: String, track: SubtitleTrack?) {
        struct Answer: Decodable { let ok: Bool }
        var command: [String: Any] = ["cmd": "subtitle_choose", "id": id, "track": track?.id ?? NSNull()]
        if let lang = track?.lang { command["lang"] = lang }
        _ = try? call(command, as: Answer.self)
    }

    struct Cue: Decodable, Sendable {
        let startMs: Int
        let endMs: Int
        let text: String

        private enum CodingKeys: String, CodingKey {
            case text
            case startMs = "start_ms"
            case endMs = "end_ms"
        }
    }

    func parseSubtitles(_ text: String) throws -> [Cue] {
        struct Answer: Decodable { let cues: [Cue] }
        return try call(["cmd": "subtitles", "text": text], as: Answer.self).cues
    }
}
