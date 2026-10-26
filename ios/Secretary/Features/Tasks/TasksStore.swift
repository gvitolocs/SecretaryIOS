import Foundation
import Observation

/// Tasks (Today, Inbox, Upcoming) and the commands behind the screen. A command that cannot
/// reach the server is kept (with its op_id) and sent again in order when the connection is
/// back; the engine's receipts make a replay harmless.
@Observable
@MainActor
final class TasksStore {
    static let outboxKey = "secretary.tasks.outbox"

    private(set) var state: TasksState?
    var view: TaskViewKind = .today
    var loadError: String?
    var actionError: String?
    struct Undo: Equatable { let text: String; let command: TaskCommand }
    /// "1 task completed · Undo".
    var undo: Undo?
    private(set) var outbox: [Queued] = []

    struct Queued: Codable, Equatable {
        let opId: String
        let command: Data
        let hidesTask: String?
    }

    @ObservationIgnored var onTransportError: ((APIError) -> Void)?
    @ObservationIgnored private let api: () -> TasksAPI
    @ObservationIgnored private let defaults: UserDefaults
    @ObservationIgnored private var generation = 0
    @ObservationIgnored private var flushing = false

    init(api: @escaping () -> TasksAPI, defaults: UserDefaults = .standard) {
        self.api = api
        self.defaults = defaults
        if let data = defaults.data(forKey: Self.outboxKey), let q = try? JSONDecoder().decode([Queued].self, from: data) {
            outbox = q
        }
    }

    /// Tasks hidden while their completion or deletion waits to be sent.
    var hidden: Set<String> { Set(outbox.compactMap(\.hidesTask)) }

    func groups() -> [TaskGroup] { state?.list.groups ?? [] }

    func visible(_ group: TaskGroup) -> [TaskItem] {
        let hide = hidden.union(state?.undo ?? [])
        return group.tasks.filter { !hide.contains($0.id) }
    }

    func refresh() async {
        generation += 1
        let mine = generation
        let v = view
        do {
            let s = try await api().state(v)
            guard mine == generation else { return }
            state = s
            loadError = nil
        } catch let error as APIError {
            guard mine == generation else { return }
            loadError = error.userMessage
            onTransportError?(error)
        } catch {
            guard mine == generation else { return }
            loadError = error.localizedDescription
        }
    }

    func show(_ kind: TaskViewKind) async {
        view = kind
        await refresh()
    }

    private func save() {
        defaults.set(try? JSONEncoder().encode(outbox), forKey: Self.outboxKey)
    }

    private static func offline(_ error: APIError) -> Bool {
        switch error {
        case .network, .timeout, .hostNotFound, .lucinaOffline: return true
        case let .server(status, _, _): return status == 502 || status == 503 || status == 504
        default: return false
        }
    }

    /// Send a command; true when it reached the server (or was queued for later).
    @discardableResult
    func run(_ command: TaskCommand) async -> Bool {
        actionError = nil
        let hides: String? = switch command {
        case let .complete(id), let .delete(id): id
        default: nil
        }
        let opId = UUID().uuidString.lowercased()
        if !outbox.isEmpty {
            enqueue(opId, command, hides)
            await flush()
            return true
        }
        do {
            let r = try await api().command(command, opId: opId, view: view)
            if let v = r.view { state = v }
            if !r.ok {
                actionError = r.error
                return false
            }
            return true
        } catch let error as APIError {
            if Self.offline(error) {
                enqueue(opId, command, hides)
                return true
            }
            actionError = error.userMessage
            if case .server(422, _, _) = error { await refresh() }
            return false
        } catch {
            actionError = error.localizedDescription
            return false
        }
    }

    private func enqueue(_ opId: String, _ command: TaskCommand, _ hides: String?) {
        guard let data = try? JSONEncoder().encode(command) else { return }
        outbox.append(Queued(opId: opId, command: data, hidesTask: hides))
        save()
    }

    /// Send what is waiting, in order; stops at the first command that cannot reach the server.
    func flush() async {
        guard !flushing else { return }
        flushing = true
        defer { flushing = false }
        while let head = outbox.first {
            do {
                let raw = try JSONSerialization.jsonObject(with: head.command)
                let r = try await api().commandRaw(raw, opId: head.opId, view: view)
                if let v = r.view { state = v }
            } catch let error as APIError {
                if Self.offline(error) { return }
                actionError = error.userMessage
            } catch {
                actionError = error.localizedDescription
            }
            outbox.removeFirst()
            save()
        }
        await refresh()
    }

    // MARK: - Actions

    func toggle(_ task: TaskItem) async {
        if task.checked {
            await run(.uncomplete(task.id))
            return
        }
        if await run(.complete(task.id)) {
            undo = Undo(text: String(localized: "1 task completed"), command: .uncomplete(task.id))
        }
    }

    func delete(_ task: TaskItem) async {
        if await run(.delete(task.id)) {
            undo = Undo(text: String(localized: "Task deleted"), command: .undoDelete(task.id))
        }
    }

    func performUndo() async {
        guard let u = undo else { return }
        undo = nil
        await run(u.command)
    }

    /// Date choices of the reschedule menu: today, tomorrow, next Monday.
    static func choices(today iso: String) -> [(label: String, date: String)] {
        let cal = Calendar(identifier: .gregorian)
        guard let today = TaskDates.day(iso) else { return [] }
        let tomorrow = cal.date(byAdding: .day, value: 1, to: today) ?? today
        let weekday = cal.component(.weekday, from: today) // 1 = Sunday
        let toMonday = (9 - weekday) % 7 == 0 ? 7 : (9 - weekday) % 7
        let nextWeek = cal.date(byAdding: .day, value: toMonday, to: today) ?? today
        return [
            (String(localized: "Today"), iso),
            (String(localized: "Tomorrow"), TaskDates.iso(tomorrow)),
            (String(localized: "Next week"), TaskDates.iso(nextWeek)),
        ]
    }
}

extension TasksAPI {
    /// A queued command, already encoded.
    func commandRaw(_ command: Any, opId: String, view: TaskViewKind) async throws -> TasksResponse {
        let body: [String: Any] = ["op_id": opId, "timezone": Self.timeZone, "command": command, "view": ["kind": view.rawValue]]
        let data = try JSONSerialization.data(withJSONObject: body, options: [.sortedKeys])
        let response = try await client.send("POST", "/v1/tasks/command", body: data, idempotent: true,
                                              headers: ["X-Secretary-Client": "ios"])
        return try client.decode(TasksResponse.self, from: response)
    }
}
