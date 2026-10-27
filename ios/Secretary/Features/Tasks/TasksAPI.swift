import Foundation

// Tasks (docs/server-api.md, "Tasks"): the secretary-tasks engine on the server, synced with
// the owner's Todoist. GET /v1/tasks/state?view=…, POST /v1/tasks/command {op_id, timezone,
// command, view}. The engine computes every list, date and count; this file only moves JSON.
// Wire names are the engine's snake_case.

struct TaskDue: Decodable, Equatable, Hashable, Sendable {
    let date: String
    let time: String?
    let string: String
    let isRecurring: Bool
    let relDays: Int
    let overdue: Bool
    let provisional: Bool

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        date = try c.decode(String.self, forKey: .date)
        time = c.lenientOptional(String.self, .time)
        string = c.lenientOptional(String.self, .string) ?? ""
        isRecurring = c.lenientOptional(Bool.self, .isRecurring) ?? false
        relDays = c.lenientOptional(Int.self, .relDays) ?? 0
        overdue = c.lenientOptional(Bool.self, .overdue) ?? (relDays < 0)
        provisional = c.lenientOptional(Bool.self, .provisional) ?? false
    }

    private enum CodingKeys: String, CodingKey {
        case date, time, string, overdue, provisional
        case isRecurring = "is_recurring", relDays = "rel_days"
    }
}

struct TaskItem: Decodable, Identifiable, Equatable, Hashable, Sendable {
    let id: String
    let content: String
    let description: String
    let projectId: String
    let projectName: String
    let projectColor: String
    let sectionName: String?
    let parentContent: String?
    /// 1 (urgent, red) … 4 (none).
    let priority: Int
    let due: TaskDue?
    let labels: [String]
    let checked: Bool
    let subtasks: Int
    let subtasksDone: Int
    let pending: Bool
    let children: [TaskItem]
    let completedMs: Double?

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        content = c.lenientOptional(String.self, .content) ?? ""
        description = c.lenientOptional(String.self, .description) ?? ""
        projectId = c.lenientOptional(String.self, .projectId) ?? ""
        projectName = c.lenientOptional(String.self, .projectName) ?? "Inbox"
        projectColor = c.lenientOptional(String.self, .projectColor) ?? ""
        sectionName = c.lenientOptional(String.self, .sectionName)
        parentContent = c.lenientOptional(String.self, .parentContent)
        priority = c.lenientOptional(Int.self, .priority) ?? 4
        due = c.lenientOptional(TaskDue.self, .due)
        labels = c.lenientOptional([String].self, .labels) ?? []
        checked = c.lenientOptional(Bool.self, .checked) ?? false
        subtasks = c.lenientOptional(Int.self, .subtasks) ?? 0
        subtasksDone = c.lenientOptional(Int.self, .subtasksDone) ?? 0
        pending = c.lenientOptional(Bool.self, .pending) ?? false
        children = c.lenientOptional([TaskItem].self, .children) ?? []
        completedMs = c.lenientOptional(Double.self, .completedMs)
    }

    private enum CodingKeys: String, CodingKey {
        case id, content, description, priority, due, labels, checked, subtasks, pending, children
        case projectId = "project_id", projectName = "project_name", projectColor = "project_color"
        case sectionName = "section_name", parentContent = "parent_content", subtasksDone = "subtasks_done"
        case completedMs = "completed_ms"
    }
}

struct TaskGroup: Decodable, Identifiable, Equatable, Sendable {
    let key: String
    /// `overdue` | `date` | `section` | `none` | `completed`.
    let kind: String
    let title: String
    let date: String?
    let relDays: Int?
    let tasks: [TaskItem]
    var id: String { key }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        key = try c.decode(String.self, forKey: .key)
        kind = c.lenientOptional(String.self, .kind) ?? "none"
        title = c.lenientOptional(String.self, .title) ?? ""
        date = c.lenientOptional(String.self, .date)
        relDays = c.lenientOptional(Int.self, .relDays)
        // One bad task must not hide the group.
        var out: [TaskItem] = []
        if var items = try? c.nestedUnkeyedContainer(forKey: .tasks) {
            while !items.isAtEnd {
                if let t = try? items.decode(TaskItem.self) { out.append(t) } else { _ = try? items.decode(JSONValue.self) }
            }
        }
        tasks = out
    }

    private enum CodingKeys: String, CodingKey { case key, kind, title, date, tasks, relDays = "rel_days" }
}

struct TasksState: Decodable, Equatable, Sendable {
    struct Counts: Decodable, Equatable, Sendable { let inbox: Int; let today: Int; let overdue: Int; let upcoming: Int }
    struct Goal: Decodable, Equatable, Sendable {
        let dailyGoal: Int
        let completedToday: Int
        let reached: Bool
        private enum CodingKeys: String, CodingKey { case reached, dailyGoal = "daily_goal", completedToday = "completed_today" }
    }
    struct Sync: Decodable, Equatable, Sendable {
        let status: String
        let connected: Bool
        let lastSyncMs: Double?
        let pending: Int
        let error: String?
        private enum CodingKeys: String, CodingKey { case status, connected, pending, error, lastSyncMs = "last_sync_ms" }
    }
    struct Conflict: Decodable, Equatable, Identifiable, Sendable { let id: String; let content: String; let kind: String; let message: String }
    struct List: Decodable, Equatable, Sendable { let kind: String; let title: String; let groups: [TaskGroup]; let warning: String? }
    struct Project: Decodable, Equatable, Identifiable, Sendable { let id: String; let name: String; let inbox: Bool; let count: Int }

    let revision: Int
    let today: String
    let counts: Counts
    let goal: Goal
    let projects: [Project]
    let sync: Sync
    let conflicts: [Conflict]
    let undo: [String]
    let list: List
}

struct TasksResponse: Decodable, Sendable {
    let ok: Bool
    let code: String?
    let error: String?
    let view: TasksState?
    let taskId: String?
    let queued: Bool?

    private enum CodingKeys: String, CodingKey { case ok, code, error, view, queued, taskId = "task_id" }
}

/// The commands the shells send (`type` + fields, snake_case).
enum TaskCommand: Encodable, Equatable, Sendable {
    case quickAdd(text: String, defaultDue: String?)
    case complete(String)
    case uncomplete(String)
    case reschedule(String, date: String)
    case rescheduleOverdue(date: String)
    case delete(String)
    case undoDelete(String)

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: Key.self)
        switch self {
        case let .quickAdd(text, due):
            try c.encode("quick_add", forKey: .type)
            try c.encode(text, forKey: .text)
            try c.encodeIfPresent(due, forKey: .defaultDue)
        case let .complete(id):
            try c.encode("complete", forKey: .type)
            try c.encode(id, forKey: .taskId)
        case let .uncomplete(id):
            try c.encode("uncomplete", forKey: .type)
            try c.encode(id, forKey: .taskId)
        case let .reschedule(id, date):
            try c.encode("reschedule", forKey: .type)
            try c.encode(id, forKey: .taskId)
            try c.encode(date, forKey: .date)
        case let .rescheduleOverdue(date):
            try c.encode("reschedule_overdue", forKey: .type)
            try c.encode(date, forKey: .date)
        case let .delete(id):
            try c.encode("delete", forKey: .type)
            try c.encode(id, forKey: .taskId)
        case let .undoDelete(id):
            try c.encode("undo_delete", forKey: .type)
            try c.encode(id, forKey: .taskId)
        }
    }

    private enum Key: String, CodingKey { case type, text, date, taskId = "task_id", defaultDue = "default_due" }
}

enum TaskViewKind: String, CaseIterable, Identifiable, Sendable {
    case today, inbox, upcoming
    var id: String { rawValue }
}

struct TasksAPI: Sendable {
    let client: APIClient
    private static let headers = ["X-Secretary-Client": "ios"]

    static var timeZone: String { TimeZone.current.identifier }

    func state(_ view: TaskViewKind) async throws -> TasksState {
        let r: TasksResponse = try await client.get(
            "/v1/tasks/state",
            query: [URLQueryItem(name: "view", value: view.rawValue), URLQueryItem(name: "timezone", value: Self.timeZone)]
        )
        guard let v = r.view else { throw APIError.decoding(message: r.error ?? "no view") }
        return v
    }

    struct Envelope: Encodable, Sendable {
        let op_id: String
        let timezone: String
        let command: TaskCommand
        let view: [String: String]
    }

    /// Commands carry an op_id and no expected_revision (the engine allows it for tasks), so a
    /// queued command can be sent again later without a conflict.
    func command(_ command: TaskCommand, opId: String, view: TaskViewKind) async throws -> TasksResponse {
        let body = Envelope(op_id: opId, timezone: Self.timeZone, command: command, view: ["kind": view.rawValue])
        return try await client.send("POST", "/v1/tasks/command", body: body, idempotent: true, headers: Self.headers)
    }
}

/// Labels for dates, like Todoist: "Today", "Tomorrow", "Monday", "27 Oct" (+ time).
enum TaskDates {
    static func day(_ iso: String) -> Date? {
        let f = DateFormatter()
        f.calendar = Calendar(identifier: .gregorian)
        f.locale = Locale(identifier: "en_US_POSIX")
        f.dateFormat = "yyyy-MM-dd"
        return f.date(from: iso)
    }

    static func label(_ due: TaskDue, locale: Locale = .current) -> String {
        var s: String
        switch due.relDays {
        case 0: s = String(localized: "Today")
        case 1: s = String(localized: "Tomorrow")
        case -1: s = String(localized: "Yesterday")
        case 2...6:
            s = day(due.date).map { $0.formatted(.dateTime.weekday(.wide).locale(locale)) } ?? due.date
        default:
            s = day(due.date).map { $0.formatted(.dateTime.day().month(.abbreviated).locale(locale)) } ?? due.date
        }
        if let t = due.time { s += " \(t)" }
        return s
    }

    /// "22 Oct · Today · Thursday".
    static func header(_ group: TaskGroup, locale: Locale = .current) -> String {
        guard let iso = group.date, let d = day(iso) else { return group.title }
        var parts = [d.formatted(.dateTime.day().month(.abbreviated).locale(locale))]
        switch group.relDays {
        case 0: parts.append(String(localized: "Today"))
        case 1: parts.append(String(localized: "Tomorrow"))
        default: break
        }
        parts.append(d.formatted(.dateTime.weekday(.wide).locale(locale)))
        return parts.joined(separator: " · ")
    }

    static func iso(_ date: Date) -> String {
        let f = DateFormatter()
        f.calendar = Calendar(identifier: .gregorian)
        f.locale = Locale(identifier: "en_US_POSIX")
        f.dateFormat = "yyyy-MM-dd"
        return f.string(from: date)
    }
}
