import Foundation
import Observation

/// The Notes tab: the list (search, tag filter), and the writes behind the detail view.
@Observable
@MainActor
final class NotesStore {
    /// A Lucina timeline item about notes older than this is history, not news.
    static let liveItemSeconds: TimeInterval = 10 * 60

    private(set) var notes: [NoteSummary] = []
    private(set) var loaded = false
    var loadError: String?
    var query = ""
    var tag = ""
    /// Every tag of the unfiltered list (for the chips).
    private(set) var allTags: [String] = []
    /// Bumped when Lucina saves or adds to a note, so an open note reloads.
    private(set) var version = 0
    struct Trashed: Equatable { let id: String; let title: String }
    /// "Moved to the trash" with an undo.
    var trashed: Trashed?

    @ObservationIgnored var onTransportError: ((APIError) -> Void)?
    @ObservationIgnored private let api: () -> NotesAPI
    @ObservationIgnored private var generation = 0
    @ObservationIgnored private var seenItems = Set<String>()

    init(api: @escaping () -> NotesAPI) {
        self.api = api
    }

    var pinned: [NoteSummary] { notes.filter(\.pinned) }
    var others: [NoteSummary] { notes.filter { !$0.pinned } }

    func refresh() async {
        generation += 1
        let mine = generation
        let (q, t) = (query, tag)
        do {
            let list = try await api().list(query: q, tag: t)
            guard mine == generation else { return }
            notes = list
            loaded = true
            loadError = nil
            if q.trimmingCharacters(in: .whitespaces).isEmpty && t.isEmpty {
                allTags = Array(Set(list.flatMap(\.tags))).sorted()
            }
        } catch let error as APIError {
            guard mine == generation else { return }
            loadError = error.userMessage
            onTransportError?(error)
        } catch {
            guard mine == generation else { return }
            loadError = error.localizedDescription
        }
    }

    /// New timeline items: Lucina saved, appended to or listed notes (`meta.kind` note/notes).
    func consider(_ items: [TimelineItem]) {
        let fresh = items.filter {
            $0.role == .lucina && ($0.meta?.kind == "note" || $0.meta?.kind == "notes")
                && Date().timeIntervalSince($0.ts) < Self.liveItemSeconds && !seenItems.contains($0.id)
        }
        guard !fresh.isEmpty else { return }
        for item in fresh { seenItems.insert(item.id) }
        version += 1
        Task { await refresh() }
    }

    func note(_ key: String) async throws -> Note {
        try await api().get(key)
    }

    func create(_ draft: NoteDraft) async throws -> Note {
        let n = try await api().create(draft)
        await refresh()
        return n
    }

    func update(_ note: Note, title: String, content: String, tags: [String]) async throws -> Note {
        let patch = NotesAPI.Patch(title: title, content: content, tags: tags, baseUpdatedAt: note.updatedAt)
        let n = try await api().update(note.id, patch)
        await refresh()
        return n
    }

    func setPinned(_ id: String, _ pinned: Bool) async throws -> Note {
        let n = try await api().update(id, NotesAPI.Patch(pinned: pinned))
        await refresh()
        return n
    }

    /// Moves a note to the trash; `false` when the server refused.
    @discardableResult
    func remove(id: String, title: String) async -> Bool {
        do {
            try await api().remove(id)
            trashed = Trashed(id: id, title: title)
            await refresh()
            return true
        } catch let error as APIError {
            loadError = error.userMessage
        } catch {
            loadError = error.localizedDescription
        }
        return false
    }

    func undoRemove() async {
        guard let note = trashed else { return }
        trashed = nil
        _ = try? await api().restore(note.id)
        await refresh()
    }

    func trash() async throws -> [NoteSummary] {
        try await api().trash()
    }

    func restore(_ id: String) async throws -> Note {
        let n = try await api().restore(id)
        await refresh()
        return n
    }
}
