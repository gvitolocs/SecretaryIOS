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
