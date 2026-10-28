import Foundation

// Wire types of the notification routes (docs/server-api.md, "Notifications";
// docs/notifications.md). Decoded defensively: unknown categories are skipped,
// unknown fields ignored, and unknown settings keys survive a round trip.

enum NotificationCategory: String, Codable, CaseIterable, Identifiable, Sendable {
    case lucinaMessage = "lucina_message"
    case reminder
    case calendar
    case deviceConfirm = "device_confirm"
    case shopping
    case presence
    case system

    var id: String { rawValue }

    var title: String {
        switch self {
        case .lucinaMessage: return "Lucina"
        case .reminder: return String(localized: "Reminder")
        case .calendar: return String(localized: "Coming up")
        case .deviceConfirm: return String(localized: "Lucina needs your OK")
        case .shopping: return String(localized: "Shopping list")
        case .presence: return String(localized: "Lucina's status")
        case .system: return "Secretary"
        }
    }

    var detail: String {
        switch self {
        case .lucinaMessage: return String(localized: "Replies and messages from Lucina on any channel, while you're not in the app.")
        case .reminder: return String(localized: "When one of your reminders is due.")
        case .calendar: return String(localized: "Before an event on your calendar starts.")
        case .deviceConfirm: return String(localized: "When Lucina wants to do something on your iPhone that needs a tap.")
        case .shopping: return String(localized: "When Lucina or someone else changes a shopping list.")
        case .presence: return String(localized: "When Lucina goes offline or comes back.")
        case .system: return String(localized: "Security notices, like a new device signing in.")
        }
    }

    var symbol: String {
        switch self {
        case .lucinaMessage: return "sparkles"
        case .reminder: return "alarm"
        case .calendar: return "calendar"
        case .deviceConfirm: return "checkmark.shield"
        case .shopping: return "cart"
        case .presence: return "waveform.path.ecg"
        case .system: return "gearshape"
        }
    }

    /// Off until the owner turns them on.
    var defaultOn: Bool { self != .presence && self != .system }
}

/// One inbox entry.
struct AppNotification: Decodable, Identifiable, Equatable, Sendable {
    let id: String
    let date: Date
    let category: NotificationCategory
    let title: String?
    let body: String
    let route: String
    let channel: String?
    let timelineId: String?
    let dedupKey: String?
    var read: Bool
    /// A decision Lucina asks the owner about (`data: {kind: "decision", decisionId, options}`).
    var decisionId: String? = nil
    var decisionOptions: [String] = []

    var displayTitle: String { title ?? category.title }

    init(id: String, date: Date, category: NotificationCategory, title: String? = nil, body: String, route: String = "/",
         channel: String? = nil, timelineId: String? = nil, dedupKey: String? = nil, read: Bool = false) {
        self.id = id
        self.date = date
        self.category = category
        self.title = title
        self.body = body
        self.route = route
        self.channel = channel
        self.timelineId = timelineId
        self.dedupKey = dedupKey
        self.read = read
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        let ms = try c.decode(Double.self, forKey: .ts)
        date = Date(timeIntervalSince1970: ms / 1000)
        category = try c.decode(NotificationCategory.self, forKey: .category)
        title = (try? c.decodeIfPresent(String.self, forKey: .title)).flatMap { $0 }.flatMap { $0.isEmpty ? nil : $0 }
        body = (try? c.decodeIfPresent(String.self, forKey: .body)) ?? ""
        let rawRoute = (try? c.decodeIfPresent(String.self, forKey: .route)) ?? "/"
        route = NotificationRoute.isSafe(rawRoute) ? rawRoute : "/"
        channel = try? c.decodeIfPresent(String.self, forKey: .channel)
        timelineId = try? c.decodeIfPresent(String.self, forKey: .timelineId)
        dedupKey = try? c.decodeIfPresent(String.self, forKey: .dedupKey)
        read = (try? c.decodeIfPresent(Bool.self, forKey: .read)) ?? false
        if let d = try? c.decodeIfPresent(DecisionData.self, forKey: .data), d.kind == "decision", DecisionAnswer.isId(d.decisionId) {
            decisionId = d.decisionId
            decisionOptions = d.options ?? []
        }
    }

    private struct DecisionData: Decodable {
        let kind: String
        let decisionId: String
        let options: [String]?
    }

    private enum CodingKeys: String, CodingKey { case id, ts, category, title, body, route, channel, timelineId, dedupKey, read, data }
}

/// Decodes an array, dropping elements that fail (unknown category, ...).
struct LossyArray<Element: Decodable & Sendable>: Decodable, Sendable {
    let values: [Element]

    init(from decoder: Decoder) throws {
        var c = try decoder.unkeyedContainer()
        var out: [Element] = []
        while !c.isAtEnd {
            if let v = try? c.decode(Element.self) {
                out.append(v)
            } else {
                _ = try? c.decode(JSONValue.self)
            }
        }
        values = out
    }
}

struct NotificationPage: Decodable, Sendable {
    let items: [AppNotification]
    let unread: Int
    let nextBefore: String?

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        items = (try? c.decodeIfPresent(LossyArray<AppNotification>.self, forKey: .items))?.values ?? []
        unread = (try? c.decodeIfPresent(Int.self, forKey: .unread)) ?? items.filter { !$0.read }.count
        nextBefore = try? c.decodeIfPresent(String.self, forKey: .nextBefore)
    }

    private enum CodingKeys: String, CodingKey { case items, unread, nextBefore }
}

struct ReadResult: Decodable, Sendable {
    let updated: Int
    let unread: Int
}

/// `GET|PUT /v1/notifications/settings`'s `settings`.
struct NotificationSettings: Codable, Equatable, Sendable {
    struct QuietHours: Codable, Equatable, Sendable {
        var enabled = false
        var start = "22:30"
        var end = "07:30"
    }

    struct DevicePrefs: Codable, Equatable, Sendable {
        var enabled = true
        var categories: [String: Bool] = [:]
    }

    /// Category name → on/off (unknown names from newer servers are kept).
    var categories: [String: Bool] = Dictionary(uniqueKeysWithValues: NotificationCategory.allCases.map { ($0.rawValue, $0.defaultOn) })
    var quietHours = QuietHours()
    var timeZone = "Europe/Rome"
    /// Do Not Disturb until this epoch ms.
    var dndUntil: Int64?
    var calendarLeadMinutes = 10
    var remindersBypassQuiet = true
    var digest = true
    var previews = true
    var language = "en"
    var devices: [String: DevicePrefs] = [:]

    init() {}

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        var cats = NotificationSettings().categories
        if let given = try? c.decodeIfPresent([String: Bool].self, forKey: .categories) {
            cats.merge(given) { _, new in new }
        }
        categories = cats
        quietHours = (try? c.decodeIfPresent(QuietHours.self, forKey: .quietHours)) ?? QuietHours()
        timeZone = (try? c.decodeIfPresent(String.self, forKey: .timeZone)) ?? "Europe/Rome"
        dndUntil = (try? c.decodeIfPresent(Double.self, forKey: .dndUntil)).flatMap { $0 }.map { Int64($0) }
        calendarLeadMinutes = (try? c.decodeIfPresent(Int.self, forKey: .calendarLeadMinutes)) ?? 10
        remindersBypassQuiet = (try? c.decodeIfPresent(Bool.self, forKey: .remindersBypassQuiet)) ?? true
        digest = (try? c.decodeIfPresent(Bool.self, forKey: .digest)) ?? true
        previews = (try? c.decodeIfPresent(Bool.self, forKey: .previews)) ?? true
        language = (try? c.decodeIfPresent(String.self, forKey: .language)) ?? "en"
        devices = (try? c.decodeIfPresent([String: DevicePrefs].self, forKey: .devices)) ?? [:]
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(categories, forKey: .categories)
        try c.encode(quietHours, forKey: .quietHours)
        try c.encode(timeZone, forKey: .timeZone)
        // `null` turns Do Not Disturb off.
        try c.encode(dndUntil, forKey: .dndUntil)
        try c.encode(calendarLeadMinutes, forKey: .calendarLeadMinutes)
        try c.encode(remindersBypassQuiet, forKey: .remindersBypassQuiet)
        try c.encode(digest, forKey: .digest)
        try c.encode(previews, forKey: .previews)
        try c.encode(language, forKey: .language)
        try c.encode(devices, forKey: .devices)
    }

    func isOn(_ category: NotificationCategory) -> Bool { categories[category.rawValue] ?? category.defaultOn }

    mutating func set(_ category: NotificationCategory, _ on: Bool) { categories[category.rawValue] = on }

    private enum CodingKeys: String, CodingKey {
        case categories, quietHours, timeZone, dndUntil, calendarLeadMinutes, remindersBypassQuiet, digest, previews, language, devices
    }
}

struct PushTarget: Decodable, Identifiable, Equatable, Sendable {
    let id: String
    let kind: String
    let deviceId: String
    let label: String
    let service: String
    let lastSuccessAt: Double?
    let failures: Int
}

/// The whole settings answer.
struct NotificationSettingsState: Decodable, Sendable {
    struct Device: Decodable, Sendable {
        let id: String
        let name: String
    }

    struct APNs: Decodable, Sendable {
        let enabled: Bool
        let environment: String?
    }

    var settings: NotificationSettings
    let subscriptions: [PushTarget]
    let currentDevice: Device?
    let apns: APNs?

    init(settings: NotificationSettings, subscriptions: [PushTarget] = [], currentDevice: Device? = nil, apns: APNs? = nil) {
        self.settings = settings
        self.subscriptions = subscriptions
        self.currentDevice = currentDevice
        self.apns = apns
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        settings = (try? c.decodeIfPresent(NotificationSettings.self, forKey: .settings)) ?? NotificationSettings()
        subscriptions = (try? c.decodeIfPresent(LossyArray<PushTarget>.self, forKey: .subscriptions))?.values ?? []
        currentDevice = try? c.decodeIfPresent(Device.self, forKey: .currentDevice)
        apns = try? c.decodeIfPresent(APNs.self, forKey: .apns)
    }

    private enum CodingKeys: String, CodingKey { case settings, subscriptions, currentDevice, apns }
}

/// A Lucina reminder (`GET /v1/reminders`), for local scheduling.
struct UpcomingReminder: Decodable, Equatable, Sendable {
    let id: String
    let text: String
    let dueAt: Date
    let status: String

    init(id: String, text: String, dueAt: Date, status: String = "pending") {
        self.id = id
        self.text = text
        self.dueAt = dueAt
        self.status = status
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(FlexibleID.self, forKey: .id).value
        text = (try? c.decodeIfPresent(String.self, forKey: .text)) ?? ""
        let raw = try c.decode(String.self, forKey: .dueAt)
        guard let due = UpcomingReminder.parse(raw) else {
            throw DecodingError.dataCorruptedError(forKey: .dueAt, in: c, debugDescription: "dueAt is not ISO 8601")
        }
        dueAt = due
        status = (try? c.decodeIfPresent(String.self, forKey: .status)) ?? "pending"
    }

    static func parse(_ raw: String) -> Date? {
        let fractional = ISO8601DateFormatter()
        fractional.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return fractional.date(from: raw) ?? ISO8601DateFormatter().date(from: raw)
    }

    private enum CodingKeys: String, CodingKey { case id, text, dueAt, status }
}

struct ReminderList: Decodable, Sendable {
    let reminders: [UpcomingReminder]

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        reminders = (try? c.decodeIfPresent(LossyArray<UpcomingReminder>.self, forKey: .reminders))?.values ?? []
    }

    private enum CodingKeys: String, CodingKey { case reminders }
}

/// A calendar event to notify about (from EventKit, via the device bridge's state).
struct CalendarAlert: Equatable, Sendable {
    let id: String
    let title: String
    let start: Date
}

/// Web routes → app tabs.
enum NotificationRoute {
    static func isSafe(_ route: String) -> Bool {
        let limit = route.hasPrefix("/map/place?") ? 4096 : 200
        return route.hasPrefix("/") && !route.hasPrefix("//") && !route.contains("\\") && route.count <= limit
    }

    static func tab(for route: String) -> AppModel.Tab {
        guard isSafe(route) else { return .chat }
        let first = route.split(separator: "/").first.map(String.init) ?? ""
        switch first {
        case "shopping": return .shopping
        case "gym", "weight": return .fitness
        case "lucina", "avatar", "wardrobe": return .lucina
        case "plugins", "notes", "tasks", "memos", "media", "films", "gallery", "drive": return .plugins
        default: return .chat
        }
    }

    /// `/decisions` or `/decisions/<id>`: the Decisions sheet. Returns the id ("" for the list).
    static func decision(_ route: String) -> String? {
        guard isSafe(route) else { return nil }
        let path = route.split(separator: "?", maxSplits: 1).first.map(String.init) ?? route
        let parts = path.split(separator: "/").map(String.init)
        guard parts.first == "decisions" else { return nil }
        if parts.count >= 2 { return DecisionAnswer.isId(parts[1]) ? parts[1] : nil }
        return ""
    }

    /// `/memos` or `/memos/<id>` (a memo is ready): the memo screen. Returns the id ("" for the list).
    static func memo(_ route: String) -> String? {
        guard isSafe(route) else { return nil }
        let path = route.split(separator: "?", maxSplits: 1).first.map(String.init) ?? route
        let parts = path.split(separator: "/").map(String.init)
        guard parts.first == "memos" else { return nil }
        return parts.count >= 2 ? parts[1] : ""
    }

    /// `/shopping/map` (grocery deal alerts): the deals map on the Shopping tab.
    static func opensDealsMap(_ route: String) -> Bool {
        guard isSafe(route) else { return false }
        let path = route.split(separator: "?", maxSplits: 1).first.map(String.init) ?? route
        let parts = path.split(separator: "/").map(String.init)
        return parts.count >= 2 && parts[0] == "shopping" && parts[1] == "map"
    }
}

extension AppLanguage {
    /// The language in use (`en`, `it` or `da`): the override, else the iPhone's first supported one.
    static var effectiveCode: String {
        if current != .system { return current.rawValue }
        for tag in Locale.preferredLanguages {
            let code = String(tag.prefix(2)).lowercased()
            if ["en", "it", "da"].contains(code) { return code }
        }
        return "en"
    }
}
