import Foundation

/// Shared bounded cursor protocol for the main chat and read-only conversations.
@MainActor
enum ChatSync {
    /// The steps come from the Rust core (`ChatSyncPlan`, `sync_next`); this loop only fetches and applies.
    static func run(api: SecretaryAPI, cursor: String?, conversation: String? = nil,
                    apply: @MainActor (TimelinePage, Bool) async throws -> Void) async throws {
        var plan = ChatSyncPlan(cursor: cursor)
        var outcome = ChatSyncPlan.Outcome.started
        var page: TimelinePage?
        var failure: Error?
        while true {
            switch try plan.next(outcome) {
            case let .fetch(after, limit):
                try Task.checkCancellation()
                do {
                    let fetched = try await api.timeline(limit: limit, after: after,
                                                         scope: conversation == nil ? "mine" : nil, conversation: conversation)
                    page = fetched
                    outcome = .page(cursor: fetched.cursor, hasMore: fetched.hasMore)
                } catch {
                    failure = error
                    outcome = APIError.wrap(error).status == 410 ? .gone410 : .failed
                }
            case let .apply(reset):
                guard let page else { throw ChatKernel.Failure(description: "sync_next: apply before a page") }
                try await apply(page, reset)
                outcome = .applied
            case .done:
                return
            case .abort:
                throw failure ?? ChatKernel.Failure(description: "sync_next: aborted")
            }
        }
    }
}
