import Foundation
import Observation

// The device bridge (docs/device-bridge.md, docs/server-api.md "Device"): the phone tells
// Lucina what happens on it (POST /v1/device/events, offline-queued; POST
// /v1/device/state), and runs the commands Lucina sends as timeline items
// (`meta.kind: "device_command"`), acking each one (POST /v1/device/commands/{id}/result).
// Every source is off until the owner turns it on in Settings → Lucina's access.

/// One event for `POST /v1/device/events`.
struct DeviceEvent: Codable, Equatable, Sendable {
    enum Kind: String, Codable, Sendable {
        case webVisit = "web_visit", locationVisit = "location_visit", healthSummary = "health_summary", workout
        case focusChange = "focus_change", screenThreshold = "screen_threshold", calendarChange = "calendar_change"
        case reminderChange = "reminder_change", alarmFired = "alarm_fired", appOpen = "app_open"
    }

    let id: String
    let ts: Int
    let type: Kind
    let source: String
    let data: JSONValue

    init(type: Kind, data: [String: JSONValue] = [:], at date: Date = Date(), source: String = "ios", id: String = UUID().uuidString) {
        self.id = id
        ts = Int(date.timeIntervalSince1970 * 1000)
        self.type = type
        self.source = source
        self.data = .object(data)
    }
}

/// A command Lucina sent (`meta.command: {id, type, args}`).
struct DeviceCommand: Equatable, Identifiable, Sendable {
    enum Kind: String, Sendable {
        case alarmSet = "alarm.set", alarmDelete = "alarm.delete", timerStart = "timer.start", timerCancel = "timer.cancel"
        case reminderCreate = "reminder.create", reminderComplete = "reminder.complete", reminderDelete = "reminder.delete"
        case calendarCreate = "calendar.create", calendarDelete = "calendar.delete"
    }

    let id: String
    let kind: Kind
    let args: [String: JSONValue]
    /// When Lucina sent it (the timeline item's time). Commands expire after 10 minutes.
    let sentAt: Date

    static let lifetime: TimeInterval = 10 * 60

    /// Deletes and anything outside alarms, timers and reminder creation need a tap.
    var needsConfirmation: Bool {
        switch kind {
        case .alarmSet, .timerStart, .reminderCreate, .reminderComplete: return false
        case .alarmDelete, .timerCancel, .reminderDelete, .calendarCreate, .calendarDelete: return true
        }
    }

    func isExpired(now: Date = Date()) -> Bool { now.timeIntervalSince(sentAt) > Self.lifetime }

    /// Reads a command from a timeline item, if it carries one.
    init?(item: TimelineItem) {
        guard item.meta?.kind == "device_command", let command = item.meta?.command,
              let id = command["id"]?.nonEmptyString ?? command["id"]?.numberValue.map({ JSONValue.number($0).displayText }),
              let type = command["type"]?.stringValue, let kind = Kind(rawValue: type) else { return nil }
        self.init(id: id, kind: kind, args: command["args"]?.objectValue ?? [:], sentAt: item.ts)
    }

    init(id: String, kind: Kind, args: [String: JSONValue], sentAt: Date) {
        self.id = id
        self.kind = kind
        self.args = args
        self.sentAt = sentAt
    }

    func string(_ key: String) -> String? { args[key]?.nonEmptyString }
    func number(_ key: String) -> Double? { args[key]?.numberValue ?? args[key]?.stringValue.flatMap(Double.init) }

    /// A short sentence for the confirmation sheet.
    var summary: String {
        let label = string("label") ?? string("title") ?? ""
        switch kind {
        case .alarmSet: return String(localized: "Set an alarm at \(string("time") ?? "?") \(label)")
        case .alarmDelete: return String(localized: "Delete the alarm \(label)")
        case .timerStart: return String(localized: "Start a timer \(label)")
        case .timerCancel: return String(localized: "Cancel the timer \(label)")
        case .reminderCreate: return String(localized: "Add the reminder \u{201C}\(label)\u{201D}")
        case .reminderComplete: return String(localized: "Complete the reminder \(label)")
        case .reminderDelete: return String(localized: "Delete the reminder \(label)")
        case .calendarCreate: return String(localized: "Add \u{201C}\(label)\u{201D} to your calendar")
        case .calendarDelete: return String(localized: "Delete the event \(label)")
        }
    }
}

struct CommandResult: Encodable, Equatable, Sendable {
    let ok: Bool
    let error: String?
    let deviceObjectId: String?

    static func success(_ objectId: String? = nil) -> CommandResult { CommandResult(ok: true, error: nil, deviceObjectId: objectId) }
    static func failure(_ message: String) -> CommandResult {
        CommandResult(ok: false, error: String(message.prefix(500)), deviceObjectId: nil)
    }
}

/// `POST /v1/device/state`.
struct DeviceState: Encodable, Sendable {
    var alarms: [JSONValue] = []
    var timers: [JSONValue] = []
    var reminders: [JSONValue] = []
    var events: [JSONValue] = []
    var focus: JSONValue = .null
    var ts = Int(Date().timeIntervalSince1970 * 1000)

    /// How far ahead the snapshot reaches, and how far ahead events bring their notes.
    static let eventDaysAhead: Double = 120
    static let notesDaysAhead: Double = 14
    /// Under the route's 256 KB limit (secretary-server `BODY_LIMIT`).
    static let bodyLimit = 240_000

    /// The JSON to send, within the body limit: a snapshot over it would be refused whole, and the server
    /// would keep the last one. Notes go first, then the farthest events (the list is sorted by start).
    func encodedWithinLimit(limit: Int = DeviceState.bodyLimit) throws -> Data {
        var data = try APIClient.encoder.encode(self)
        guard data.count > limit else { return data }
        var trimmed = self
        trimmed.events = events.map { event in
            guard case var .object(fields) = event else { return event }
            fields["notes"] = nil
            return .object(fields)
        }
        data = try APIClient.encoder.encode(trimmed)
        while data.count > limit, !trimmed.events.isEmpty {
            trimmed.events.removeLast(max(1, trimmed.events.count / 10))
            data = try APIClient.encoder.encode(trimmed)
        }
        return data
    }
}

/// Runs commands with the matching framework. Implemented by `DeviceExecutors`.
@MainActor
protocol DeviceCommandExecuting: AnyObject {
    func execute(_ command: DeviceCommand) async -> CommandResult
    func currentState() async -> DeviceState
}

struct DeviceClient: Sendable {
    let client: APIClient

    func send(events: [DeviceEvent]) async throws {
        struct Body: Encodable { let events: [DeviceEvent] }
        _ = try await client.send("POST", "/v1/device/events", body: try APIClient.encoder.encode(Body(events: events)),
                                  timeout: 30, idempotent: true)
    }

    func ack(_ id: String, _ result: CommandResult) async throws {
        let escaped = id.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed.subtracting(CharacterSet(charactersIn: "/"))) ?? id
        _ = try await client.send("POST", "/v1/device/commands/\(escaped)/result", body: try APIClient.encoder.encode(result), idempotent: true)
    }

    func report(_ state: DeviceState) async throws {
        _ = try await client.send("POST", "/v1/device/state", body: try state.encodedWithinLimit(), idempotent: true)
    }
}

@Observable
@MainActor
final class DeviceBridge {
    /// Commands waiting for the owner's tap (deletes, calendar).
    private(set) var pendingConfirmations: [DeviceCommand] = []
    /// The last few commands and what happened, for the access screen.
    private(set) var recent: [(command: DeviceCommand, result: CommandResult)] = []
    private(set) var queuedEvents = 0
    private(set) var serverSupportsDevice = true

    @ObservationIgnored private let client: () -> DeviceClient
    @ObservationIgnored weak var executor: DeviceCommandExecuting?
    @ObservationIgnored private let fileURL: URL
    @ObservationIgnored private var queue: [DeviceEvent] = []
    @ObservationIgnored private var handled: [String] = []
    @ObservationIgnored private var flushing = false
    @ObservationIgnored private var lastStateReport = Date.distantPast
    /// Commands sent before this install or sign-in started the bridge are never run.
    @ObservationIgnored private var activatedAt = Date()

    static let maxQueue = 2000

    init(client: @escaping () -> DeviceClient, fileURL: URL = DeviceBridge.defaultFileURL, activatedAt: Date = Date()) {
        self.client = client
        self.fileURL = fileURL
        self.activatedAt = activatedAt
        if !restore() { persist() } // first launch: remember when the bridge started
    }

    nonisolated static var defaultFileURL: URL {
        let base = (try? FileManager.default.url(for: .applicationSupportDirectory, in: .userDomainMask, appropriateFor: nil, create: true))
            ?? FileManager.default.temporaryDirectory
        return base.appendingPathComponent("Secretary", isDirectory: true).appendingPathComponent("device-bridge.json")
    }

    // MARK: - Events (phone → Lucina)

    func record(_ event: DeviceEvent) {
        queue.append(event)
        if queue.count > Self.maxQueue { queue.removeFirst(queue.count - Self.maxQueue) }
        queuedEvents = queue.count
        persist()
        Task { await flush() }
    }

    /// Sends the queue in batches of 500; the same batch is resent until it gets a 200.
    func flush() async {
        guard !flushing, !queue.isEmpty else { return }
        flushing = true
        defer { flushing = false }
        while !queue.isEmpty {
            let batch = Array(queue.prefix(500))
            do {
                try await client().send(events: batch)
                let sent = Set(batch.map(\.id))
                queue.removeAll { sent.contains($0.id) }
                queuedEvents = queue.count
                serverSupportsDevice = true
                persist()
            } catch {
                if APIError.wrap(error).status == 404 { serverSupportsDevice = false } // not deployed yet: keep the queue
                return
            }
        }
    }

    // MARK: - Commands (Lucina → phone)

    /// Called with every timeline item that arrives (stream, page, or the cache).
    func consider(_ items: [TimelineItem]) {
        for item in items {
            guard let command = DeviceCommand(item: item), !handled.contains(command.id) else { continue }
            // Older than this install or sign-in: not ours to run (and the timeline replays history).
            if command.sentAt < activatedAt.addingTimeInterval(-5) { markHandled(command.id); continue }
            markHandled(command.id)
            if command.isExpired() {
                Task { await ack(command, .failure("expired before it reached the phone")) }
                continue
            }
            if command.needsConfirmation {
                if !pendingConfirmations.contains(where: { $0.id == command.id }) { pendingConfirmations.append(command) }
            } else {
                Task { await run(command) }
            }
        }
    }

    func approve(_ command: DeviceCommand) async {
        pendingConfirmations.removeAll { $0.id == command.id }
        guard !command.isExpired() else {
            await ack(command, .failure("expired before the owner confirmed it"))
            return
        }
        await run(command)
    }

    func decline(_ command: DeviceCommand) async {
        pendingConfirmations.removeAll { $0.id == command.id }
        await ack(command, .failure("the owner declined it on the phone"))
    }

    private func run(_ command: DeviceCommand) async {
        guard let executor else {
            await ack(command, .failure("the device bridge is not ready"))
            return
        }
        let result = await executor.execute(command)
        await ack(command, result)
        await reportState(force: true)
    }

    private func ack(_ command: DeviceCommand, _ result: CommandResult) async {
        recent.insert((command, result), at: 0)
        if recent.count > 20 { recent.removeLast() }
        try? await client().ack(command.id, result)
    }

    private func markHandled(_ id: String) {
        handled.append(id)
        if handled.count > 500 { handled.removeFirst(handled.count - 500) }
        persist()
    }

    // MARK: - State

    /// Reports alarms, timers, reminders, events and Focus (at most every 15 minutes unless forced).
    func reportState(force: Bool = false) async {
        guard let executor, force || Date().timeIntervalSince(lastStateReport) > 15 * 60 else { return }
        lastStateReport = Date()
        let state = await executor.currentState()
        try? await client().report(state)
    }

    // MARK: - Persistence

    private struct Snapshot: Codable {
        var queue: [DeviceEvent]
        var handled: [String]
        var activatedAt: Date?
    }

    private func persist() {
        do {
            try FileManager.default.createDirectory(at: fileURL.deletingLastPathComponent(), withIntermediateDirectories: true)
            try JSONEncoder().encode(Snapshot(queue: queue, handled: handled, activatedAt: activatedAt))
                .write(to: fileURL, options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
        } catch {}
    }

    @discardableResult
    private func restore() -> Bool {
        guard let data = try? Data(contentsOf: fileURL), let snapshot = try? JSONDecoder().decode(Snapshot.self, from: data) else { return false }
        queue = snapshot.queue
        handled = snapshot.handled
        if let activatedAt = snapshot.activatedAt { self.activatedAt = activatedAt }
        queuedEvents = queue.count
        return true
    }

    func reset() {
        queue = []
        handled = []
        activatedAt = Date()
        pendingConfirmations = []
        recent = []
        queuedEvents = 0
        try? FileManager.default.removeItem(at: fileURL)
    }
}
