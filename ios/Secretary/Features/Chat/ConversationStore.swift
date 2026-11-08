import Foundation
import Observation

@Observable
@MainActor
final class ConversationStore {
    private(set) var items: [TimelineItem] = []
    private(set) var hasOlder = false
    private(set) var isLoadingOlder = false
    private(set) var error: String?
    private var nextBefore: String?
    private var syncing = false
    private var prepared = false
    private let key: String
    private let database: ChatDatabase
    private let api: () -> SecretaryAPI

    init(key: String, database: ChatDatabase, api: @escaping () -> SecretaryAPI) {
        self.key = key
        self.database = database
        self.api = api
    }

    private var scope: ChatDatabase.Scope { .conversation(key) }
    private var olderKey: String { "older:" + scope.stateKey }

    func open() async {
        if !prepared {
            let cached = (try? await database.read(scope)) ?? []
            guard !Task.isCancelled else { return }
            prepared = true
            items = TimelineMerge.merge(cached, items).items
            nextBefore = try? await database.value(olderKey)
            if nextBefore?.isEmpty == true { nextBefore = nil }
            hasOlder = cached.count == 50 || nextBefore != nil
        }
        await refresh()
    }

    func refresh() async {
        guard !syncing else { return }
        syncing = true
        defer { syncing = false }
        do {
            let cursor = try? await database.value(scope.stateKey)
            try await ChatSync.run(api: api(), cursor: cursor, conversation: key) { page, reset in
                try Task.checkCancellation()
                var state = page.cursor.map { [self.scope.stateKey: $0] } ?? [:]
                // After-pages do not define the older-history boundary.
                if cursor == nil || reset {
                    state[self.olderKey] = page.nextBefore ?? ""
                }
                try await self.database.write(page.items, state: state, reset: reset ? self.scope : nil)
                try Task.checkCancellation()
                self.items = TimelineMerge.merge(self.items, page.items).items
                if cursor == nil || reset {
                    self.nextBefore = page.nextBefore
                    self.hasOlder = page.nextBefore != nil
                }
                self.error = nil
            }
        } catch is CancellationError {
            return
        } catch { self.error = APIError.wrap(error).userMessage }
    }

    func loadOlder() async {
        guard hasOlder, !isLoadingOlder else { return }
        isLoadingOlder = true
        defer { isLoadingOlder = false }
        do {
            let before = items.map(\.id).min() ?? nextBefore
            let cached = (try? await database.read(scope, before: before)) ?? []
            try Task.checkCancellation()
            if !cached.isEmpty {
                items = TimelineMerge.merge(items, cached).items
                nextBefore = cached.map(\.id).min()
                return
            }
            guard let before = nextBefore ?? before else { hasOlder = false; return }
            let page = try await api().timeline(before: before, limit: 50, conversation: key)
            try Task.checkCancellation()
            try await database.write(page.items, state: [olderKey: page.nextBefore ?? ""])
            try Task.checkCancellation()
            items = TimelineMerge.merge(items, page.items).items
            nextBefore = page.nextBefore
            hasOlder = page.nextBefore != nil
            error = nil
        } catch is CancellationError {
            return
        } catch { self.error = APIError.wrap(error).userMessage }
    }
}
