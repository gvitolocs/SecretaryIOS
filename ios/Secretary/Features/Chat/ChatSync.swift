import Foundation

/// Shared bounded cursor protocol for the main chat and read-only conversations.
@MainActor
enum ChatSync {
    static func run(api: SecretaryAPI, cursor: String?, conversation: String? = nil,
                    apply: @MainActor (TimelinePage, Bool) async throws -> Void) async throws {
        func fetch(after: String?) async throws -> TimelinePage {
            try Task.checkCancellation()
            return try await api.timeline(limit: after == nil ? 50 : 200, after: after,
                                          scope: conversation == nil ? "mine" : nil, conversation: conversation)
        }
        var cursor = cursor
        if cursor == nil {
            let page = try await fetch(after: nil)
            try await apply(page, false)
            return
        }
        for _ in 0..<10 {
            do {
                let page = try await fetch(after: cursor)
                try await apply(page, false)
                if !page.hasMore { return }
                guard let next = page.cursor, next != cursor else { break }
                cursor = next
            } catch {
                guard APIError.wrap(error).status == 410 else { throw error }
                break
            }
        }
        // An expired or unbounded backlog is replaced on disk by a fresh window.
        let page = try await fetch(after: nil)
        try await apply(page, true)
    }
}
