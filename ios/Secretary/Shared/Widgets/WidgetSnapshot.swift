import Foundation

// What the Home Screen, Lock Screen and StandBy widgets show (Extensions/SecretaryWidgets), one
// section per widget. Compiled into the app and the widget extension. Each section carries the
// time it was fetched; `WidgetStore` keeps the newest copy of each, whoever wrote it (the
// extension fetches from the server on its own; the app publishes what only it knows, like the
// on-device training engine, and pushes fresh copies after changes).

enum WidgetKind: String, CaseIterable, Sendable {
    case ask = "tech.getsecretary.widget.ask"
    case appointment = "tech.getsecretary.widget.appointment"
    case shopping = "tech.getsecretary.widget.shopping"
    case weight = "tech.getsecretary.widget.weight"
    case training = "tech.getsecretary.widget.training"
    case tasks = "tech.getsecretary.widget.tasks"
    case memo = "tech.getsecretary.widget.memo"
    case pokemon = "tech.getsecretary.widget.pokemon"
    case films = "tech.getsecretary.widget.films"
    case countdown = "tech.getsecretary.widget.countdown"
    case server = "tech.getsecretary.widget.server"
}

protocol WidgetSection: Codable, Equatable, Sendable {
    static var key: String { get }
    var fetchedAt: Date { get }
}

/// Lucina's last chat reply.
struct AskSection: WidgetSection {
    static let key = "ask"
    var fetchedAt: Date
    var reply: String?
    var replyAt: Date?
}

/// The next calendar event (today or tomorrow), with the travel time when the server has one.
struct AppointmentSection: WidgetSection {
    static let key = "appointment"
    struct Event: Codable, Equatable, Sendable {
        var id: String
        var title: String
        var location: String?
        var start: Date
        var end: Date
        var allDay: Bool
        /// Minutes from home to `location` (maps routes, the quickest mode).
        var travelMinutes: Int?
        /// `drive`, `walk`, `bike`, `transit`.
        var travelMode: String?
        /// The server's leave time (start - travel - 5 min); else start minus the travel time.
        var leaveBy: Date?
        var leaveAt: Date? { leaveBy ?? travelMinutes.map { start.addingTimeInterval(-Double($0) * 60) } }
    }

    var fetchedAt: Date
    var next: Event?
    /// The one after, for the medium widget.
    var then: Event?
}

struct ShoppingSection: WidgetSection {
    static let key = "shopping"
    struct Item: Codable, Equatable, Sendable, Identifiable {
        var id: String
        var name: String
        var spec: String
        /// The catalogue emoji (`ShoppingEmojiSection`); nil in a copy saved before it existed.
        var icon: String?
        /// The catalogue item's illustration (`item-<catalogId>` in ItemIcons.xcassets), drawn instead of
        /// the emoji when bundled; nil for free text, custom items and hand-picked emoji.
        var art: String?

        var emoji: String { icon ?? ShoppingEmojiSection.defaultEmoji }
    }

    struct Deal: Codable, Equatable, Sendable, Identifiable {
        var id: String { item + store }
        /// The list item it matches.
        var item: String
        var name: String
        var store: String
        var priceDkk: Double?
        var discountPct: Double?

        /// The chains send "HONNINGSKINKE MINIMUM": shown as "Honningskinke minimum".
        var displayName: String {
            guard name == name.uppercased() else { return name }
            let lower = name.lowercased()
            return lower.prefix(1).uppercased() + lower.dropFirst()
        }
    }

    var fetchedAt: Date
    var listId: String
    var listName: String
    /// Still needed, in list order.
    var items: [Item]
    var deals: [Deal]
}

struct WeightSection: WidgetSection {
    static let key = "weight"
    var fetchedAt: Date
    /// `kg` or `lb`.
    var unit: String
    /// "80.4 kg"
    var currentLabel: String?
    var lastAt: Date?
    /// "-0.6 kg" over the last week.
    var weeklyChangeLabel: String?
    var weeklyChangeGrams: Int?
    /// The 7-day average, oldest first, in `unit` (the last 30 days).
    var trend: [Double]
    var loggedToday: Bool {
        guard let lastAt else { return false }
        return Calendar.current.isDateInToday(lastAt)
    }
}

struct TrainingSection: WidgetSection {
    static let key = "training"
    struct Session: Codable, Equatable, Sendable {
        var title: String
        var day: Date
    }

    var fetchedAt: Date
    var weekCount: Int
    var weeklyGoal: Int
    /// This week's finished workouts, oldest first.
    var sessions: [Session]
    /// The workout in progress, if any.
    var activeTitle: String?
    var activeSince: Date?
}

struct TasksSection: WidgetSection {
    static let key = "tasks"
    struct Item: Codable, Equatable, Sendable, Identifiable {
        enum Kind: String, Codable, Sendable { case task, reminder }
        var id: String
        var kind: Kind
        var title: String
        /// A time today, when it has one.
        var due: Date?
        var overdue: Bool
        /// 1 (urgent) … 4 (none), Todoist's scale; reminders are 4.
        var priority: Int
    }

    var fetchedAt: Date
    var items: [Item]
    /// Done today (Todoist's daily goal).
    var completedToday: Int
}

struct PokemonSection: WidgetSection {
    static let key = "pokemon"
    struct Mon: Codable, Equatable, Sendable {
        var name: String
        var level: Int
        var hp: Int
        var maxHp: Int
    }

    var fetchedAt: Date
    var running: Bool
    var place: String
    var badges: Int
    var objective: String?
    var party: [Mon]
    var lastNote: String?
}

struct FilmsSection: WidgetSection {
    static let key = "films"
    struct Film: Codable, Equatable, Sendable, Identifiable {
        var id: String
        var title: String
        var positionMs: Int
        var durationMs: Int
        var updatedAt: Date
        /// Server-relative poster or thumbnail URL.
        var posterPath: String?
        var progress: Double { durationMs > 0 ? min(1, Double(positionMs) / Double(durationMs)) : 0 }
        var remainingMinutes: Int { max(0, (durationMs - positionMs) / 60_000) }
    }

    var fetchedAt: Date
    var films: [Film]
}

/// The homelab's status (`GET /v1/system/overview`): worst state, how many things need attention, the first titles.
struct ServerSection: WidgetSection {
    static let key = "server"
    var fetchedAt: Date
    /// `ok`, `warn`, `crit` (or `unknown`).
    var overall: String
    var attentionCount: Int
    /// The first attention items, worst first.
    var titles: [String]
    /// When the server built the overview.
    var generatedAt: Date?
}
