import AppIntents
import WidgetKit

// The widgets' own buttons, run in the extension (no app launch): tick a shopping item off,
// complete a task or a reminder. Each changes the cached section first, so the widget redraws at
// once, then tells the server; a failure puts the old copy back.

struct CheckShoppingItemIntent: AppIntent {
    static let title: LocalizedStringResource = "Tick off a shopping item"
    static let isDiscoverable = false

    @Parameter(title: "List") var listId: String
    @Parameter(title: "Item") var itemId: String

    init() {}

    init(listId: String, itemId: String) {
        self.listId = listId
        self.itemId = itemId
    }

    func perform() async throws -> some IntentResult {
        let before = WidgetStore.read(ShoppingSection.self)
        WidgetStore.update(ShoppingSection.self) { section in
            section.items.removeAll { $0.id == itemId }
            section.fetchedAt = Date()
        }
        do {
            guard let fetcher = WidgetFetcher.fromBridge() else { throw WidgetIntentError.signedOut }
            try await fetcher.purchase(listId: listId, itemId: itemId)
        } catch {
            if let before { WidgetStore.update(ShoppingSection.self) { $0 = before } }
            throw error
        }
        return .result()
    }
}

struct CompleteTaskIntent: AppIntent {
    static let title: LocalizedStringResource = "Complete a task"
    static let isDiscoverable = false

    @Parameter(title: "Task") var taskId: String
    /// `task` (Todoist) or `reminder` (Lucina's).
    @Parameter(title: "Kind") var kind: String

    init() {}

    init(_ item: TasksSection.Item) {
        taskId = item.id
        kind = item.kind.rawValue
    }

    func perform() async throws -> some IntentResult {
        let before = WidgetStore.read(TasksSection.self)
        let itemKind = TasksSection.Item.Kind(rawValue: kind) ?? .task
        WidgetStore.update(TasksSection.self) { section in
            section.items.removeAll { $0.id == taskId && $0.kind == itemKind }
            if itemKind == .task { section.completedToday += 1 }
            section.fetchedAt = Date()
        }
        do {
            guard let fetcher = WidgetFetcher.fromBridge() else { throw WidgetIntentError.signedOut }
            try await fetcher.complete(taskId, kind: itemKind)
        } catch {
            if let before { WidgetStore.update(TasksSection.self) { $0 = before } }
            throw error
        }
        return .result()
    }
}

enum WidgetIntentError: Error, CustomLocalizedStringResourceConvertible {
    case signedOut

    var localizedStringResource: LocalizedStringResource { "Open Secretary and sign in first." }
}

// MARK: - Which shopping list a widget shows

struct ShoppingListEntity: AppEntity {
    static let typeDisplayRepresentation: TypeDisplayRepresentation = "Shopping list"
    static let defaultQuery = ShoppingListQuery()

    var id: String
    var name: String

    var displayRepresentation: DisplayRepresentation { DisplayRepresentation(title: "\(name)") }
}

struct ShoppingListQuery: EntityQuery {
    func entities(for identifiers: [String]) async throws -> [ShoppingListEntity] {
        try await suggestedEntities().filter { identifiers.contains($0.id) }
    }

    func suggestedEntities() async throws -> [ShoppingListEntity] {
        guard let fetcher = WidgetFetcher.fromBridge() else { return [] }
        let json: JSONValue = try await fetcher.client.get("/v1/shopping/lists")
        return (json.arrayValue ?? json["lists"]?.arrayValue ?? []).compactMap { list in
            guard let id = list["id"]?.nonEmptyString else { return nil }
            return ShoppingListEntity(id: id, name: list["name"]?.nonEmptyString ?? id)
        }
    }
}

struct ShoppingWidgetConfiguration: WidgetConfigurationIntent {
    static let title: LocalizedStringResource = "Shopping list"
    static let description = IntentDescription("Choose the list the widget shows.")
    static let isDiscoverable = false

    @Parameter(title: "List")
    var list: ShoppingListEntity?
}

// MARK: - Which events a countdown widget counts down to

struct CountdownEventEntity: AppEntity {
    static let typeDisplayRepresentation: TypeDisplayRepresentation = "Event"
    static let defaultQuery = CountdownEventQuery()

    var id: String
    var event: CountdownEvent

    init(_ event: CountdownEvent) {
        id = event.id
        self.event = event
    }

    var displayRepresentation: DisplayRepresentation {
        DisplayRepresentation(title: "\(event.title)", subtitle: "\(Countdown.dateText(event))")
    }
}

/// The coming year's events (pinned ones first); a chosen one is found again in the cached list.
struct CountdownEventQuery: EntityStringQuery {
    func entities(for identifiers: [String]) async throws -> [CountdownEventEntity] {
        var known = Self.known(Countdown.pinned + Countdown.upcoming)
        if identifiers.contains(where: { known[$0] == nil }) {
            let fresh = await Countdown.refreshUpcoming(force: true)
            known.merge(Self.known(fresh)) { old, _ in old }
        }
        return identifiers.compactMap { known[$0] }.map(CountdownEventEntity.init)
    }

    func entities(matching string: String) async throws -> [CountdownEventEntity] {
        try await suggestedEntities().filter { $0.event.title.localizedCaseInsensitiveContains(string) }
    }

    func suggestedEntities() async throws -> [CountdownEventEntity] {
        let pinned = Countdown.pinned
        let upcoming = await Countdown.refreshUpcoming()
        let first: [CountdownEvent] = Countdown.visible(pinned, fresher: upcoming, now: Date())
        let rest: [CountdownEvent] = Array(upcoming.filter { event in !pinned.contains { $0.id == event.id } }.prefix(200))
        return (first + rest).map(CountdownEventEntity.init)
    }

    private static func known(_ events: [CountdownEvent]) -> [String: CountdownEvent] {
        Dictionary(events.map { ($0.id, $0) }, uniquingKeysWith: { a, _ in a })
    }
}

struct CountdownWidgetConfiguration: WidgetConfigurationIntent {
    static let title: LocalizedStringResource = "Countdown"
    static let description = IntentDescription("Choose the events to count down to. Without a choice, the widget shows the events you pinned in Secretary.")
    static let isDiscoverable = false

    @Parameter(title: "Events")
    var events: [CountdownEventEntity]?
}
