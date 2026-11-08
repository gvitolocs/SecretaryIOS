import Foundation
import Observation

@Observable @MainActor
final class CalendarStore {
    private(set) var events: [CalendarEntry] = []
    private(set) var proposals: [CalendarPendingProposal] = []
    private(set) var sources: JSONValue?
    private(set) var loading = false
    private(set) var hasLoaded = false
    private(set) var busy = false
    var error: String?
    private(set) var hidden: Set<String>
    @ObservationIgnored private let api: () -> CalendarAPI?
    @ObservationIgnored private let onSessionEnded: (APIError) -> Void
    @ObservationIgnored private let defaults: UserDefaults
    @ObservationIgnored private var cache: [Date: [CalendarEntry]] = [:]
    @ObservationIgnored private var cacheOrder: [Date] = []
    @ObservationIgnored private var generation = 0
    @ObservationIgnored private var range: (Date, Date)?

    init(api: @escaping () -> CalendarAPI?, defaults: UserDefaults = .standard, onSessionEnded: @escaping (APIError) -> Void = { _ in }) {
        self.api = api; self.defaults = defaults; self.onSessionEnded = onSessionEnded
        hidden = Set(defaults.stringArray(forKey: "secretary.calendar.hidden") ?? [])
    }
    var visible: [CalendarEntry] { events.filter { !hidden.contains($0.calendarName) } }
    struct Legend: Identifiable {
        var name: String
        var source: String
        var id: String { name }
    }
    var calendars: [Legend] {
        var names = ["Lucina": "lucina", "Google": "google", "iPhone": "iphone"]
        for event in events { names[event.calendarName] = event.source }
        for sub in sources?["ics"]?.arrayValue ?? [] {
            if let label = sub["label"]?.nonEmptyString { names[label] = "ics" }
        }
        return names.map { Legend(name: $0.key, source: $0.value) }.sorted { $0.name < $1.name }
    }
    func toggle(_ name: String) {
        if hidden.contains(name) { hidden.remove(name) } else { hidden.insert(name) }
        defaults.set(Array(hidden).sorted(), forKey: "secretary.calendar.hidden")
    }
    func load(from: Date, to: Date, refresh: Bool = false) async {
        guard let api = api(), !Task.isCancelled else { return }
        generation += 1; let mine = generation
        let sameRange = range?.0 == from && range?.1 == to
        range = (from, to)
        let calendar = Calendar.current
        var month = calendar.dateInterval(of: .month, for: from)!.start
        var months: [Date] = []
        while month < to {
            months.append(month)
            month = calendar.date(byAdding: .month, value: 1, to: month)!
        }
        func combined(_ pages: [[CalendarEntry]]) -> [CalendarEntry] {
            var seen = Set<String>()
            return pages.flatMap { $0 }.filter { seen.insert($0.id + "@" + String($0.startMs)).inserted }
        }
        let cached = combined(months.compactMap { cache[$0] })
        if !sameRange || !cached.isEmpty { events = cached }
        loading = true; error = nil
        defer { if generation == mine { loading = false } }
        var pages: [[CalendarEntry]] = []
        do {
            for month in months {
                if !refresh, let cached = cache[month] { pages.append(cached); continue }
                let end = calendar.date(byAdding: .month, value: 1, to: month)!
                let page = try await api.events(from: month, to: end)
                guard generation == mine, !Task.isCancelled else { return }
                cache[month] = page.events; sources = page.sources
                cacheOrder.removeAll { $0 == month }; cacheOrder.append(month)
                while cacheOrder.count > 8 { cache.removeValue(forKey: cacheOrder.removeFirst()) }
                pages.append(page.events)
                events = combined(pages)
            }
            guard generation == mine else { return }
            events = combined(pages)
            hasLoaded = true
        } catch {
            if generation == mine && !Task.isCancelled { record(error) }
        }
    }
    func loadProposals() async {
        guard let api = api() else { return }
        do { proposals = try await api.proposals() } catch { if !Task.isCancelled { record(error) } }
    }
    func refresh() async {
        if let range { await load(from: range.0, to: range.1, refresh: true) }
        await loadProposals()
    }
    func save(_ draft: CalendarEventDraft, id: String?) async -> Bool {
        await perform { api in
            if let event = try await api.save(draft, id: id) { self.upsert(event) }
        }
    }
    /// An event changed on the phone (EventKit): shown at once, the next snapshot brings it to the server.
    func adopt(_ event: CalendarEntry) { upsert(event) }
    /// Google can take events (a move from Lucina's calendar) while it is connected.
    var googleWritable: Bool { sources?["google"]?["status"]?.stringValue == "connected" }
    /// Changes alerts, show-as, travel time or the leave alert; the event as it now reads, nil when it failed.
    func update(_ event: CalendarEntry, settings: CalendarSettingsPatch) async -> CalendarEntry? {
        var updated = settings.applied(to: event)
        let ok = await perform { api in
            if let saved = try await api.update(event.id, settings: settings) { updated = saved }
            self.upsert(updated)
        }
        guard ok else { return nil }
        // The reloaded range has what a single event's answer leaves out (a Google calendar's default alerts).
        return events.first { $0.id == updated.id && $0.startMs == updated.startMs } ?? updated
    }
    /// Moves the event to `target` ("lucina" or "google"); the new event (with its new id), nil when it failed.
    func move(_ event: CalendarEntry, to target: String) async -> CalendarEntry? {
        var moved: CalendarEntry?
        let ok = await perform { api in
            moved = try await api.move(event.id, to: target)
            self.events.removeAll { $0.id == event.id && $0.startMs == event.startMs }
            if let moved { self.upsert(moved) }
        }
        guard ok, let moved else { return nil }
        return events.first { $0.id == moved.id } ?? moved
    }
    func delete(_ event: CalendarEntry) async -> Bool {
        await perform { api in
            try await api.delete(event.id)
            self.events.removeAll { $0.id == event.id }
        }
    }
    func proposal(_ proposal: CalendarPendingProposal, accept: Bool) async {
        _ = await perform { api in
            if accept {
                if let event = try await api.accept(proposal.id) { self.upsert(event) }
            } else { try await api.dismiss(proposal.id) }
            self.proposals.removeAll { $0.id == proposal.id }
        }
    }
    func sync() async {
        _ = await perform { api in
            let result = try await api.sync()
            if result["google"]?["error"]?.stringValue == "reconnect_needed" {
                self.error = String(localized: "Reconnect Google in Links, then try again.")
            } else if result["google"]?["ok"]?.boolValue == false || (result["ics"]?.arrayValue ?? []).contains(where: { $0["ok"]?.boolValue == false }) {
                self.error = String(localized: "Some calendars could not sync. Try again.")
            }
        }
    }
    private func upsert(_ event: CalendarEntry) {
        events.removeAll { $0.id == event.id && $0.startMs == event.startMs }; events.append(event)
    }
    private func perform(_ operation: (CalendarAPI) async throws -> Void) async -> Bool {
        guard !busy, let api = api() else { return false }
        busy = true; error = nil
        defer { busy = false }
        do {
            try await operation(api)
            cache.removeAll(); cacheOrder.removeAll()
            // Keep the mutation visible while the range reloads.
            let actionError = error
            await refresh()
            if let actionError { error = actionError }
            return true
        } catch { record(error); return false }
    }
    private func record(_ error: Error) {
        let apiError = APIError.wrap(error)
        if apiError.endsSession { onSessionEnded(apiError) }
        switch apiError.code {
        case "read_only_source": self.error = String(localized: "This calendar is read-only.")
        case "google_reconnect_needed", "google_not_connected": self.error = String(localized: "Reconnect Google in Links, then try again.")
        case "recurring_event": self.error = String(localized: "A repeating event can't be moved to another calendar.")
        case "event_not_found": self.error = String(localized: "This event is no longer in the calendar.")
        case "google_rejected": self.error = String(localized: "Google could not save this event. Check the details.")
        case "google_unreachable", "google_error": self.error = String(localized: "Google is unavailable. Try again later.")
        default: self.error = apiError.userMessage
        }
    }
}
