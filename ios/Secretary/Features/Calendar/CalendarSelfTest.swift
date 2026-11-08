import Foundation
import os

#if DEBUG || SCREENSHOTS
/// `-SecretaryCalendarSelfTest YES` (screenshot builds, `ios/scripts/plugin-shots.sh`): on the live server, creates one
/// event in Lucina's calendar for tomorrow, opens it on screen, renames it, deletes it and checks it is gone. Each step
/// is logged and written to `Documents/calendar-selftest.log`; the event never outlives the run when the server answers.
enum CalendarSelfTest {
    private static let log = Logger(subsystem: "tech.getsecretary.app", category: "CalendarSelfTest")
    /// Seconds each state stays on screen for the screenshot script.
    static let hold: Duration = .seconds(10)

    @MainActor
    static func runIfRequested(_ app: AppModel) async {
        guard UserDefaults.standard.bool(forKey: "SecretaryCalendarSelfTest"), app.client.accessKey?.isEmpty == false else { return }
        UserDefaults.standard.removeObject(forKey: "SecretaryCalendarSelfTest")
        var lines: [String] = []
        func note(_ line: String) {
            lines.append(line)
            log.notice("CalendarSelfTest \(line, privacy: .public)")
        }
        let api = CalendarAPI(client: app.client)
        let calendar = Calendar.current
        let tomorrow = calendar.date(byAdding: .day, value: 1, to: calendar.startOfDay(for: Date()))!
        let start = calendar.date(bySettingHour: 10, minute: 0, second: 0, of: tomorrow)!
        let end = start.addingTimeInterval(3600)
        let title = "Secretary iOS self-test (delete me)"
        var created: CalendarEntry?
        do {
            try await Task.sleep(for: .seconds(4))
            created = try await api.save(CalendarEventDraft(title: title, start: CalendarMath.instant(start), end: CalendarMath.instant(end),
                                                             location: "Aarhus Universitet"), id: nil)
            guard let event = created else { note("create: no event in the answer"); return finish(lines) }
            note("created \(event.id) editable=\(event.editable)")
            app.openCalendar(CalendarLink(eventId: event.id, at: start))
            try await Task.sleep(for: hold)

            let edited = try await api.save(CalendarEventDraft(title: title + " edited"), id: event.id)
            note("edited title=\(edited?.title ?? "?")")
            app.openCalendarEvent = nil
            app.openCalendar(CalendarLink(eventId: event.id, at: start))
            try await Task.sleep(for: hold)

            try await api.delete(event.id)
            created = nil
            let page = try await api.events(from: tomorrow, to: calendar.date(byAdding: .day, value: 1, to: tomorrow)!)
            note(page.events.contains { $0.id == event.id } ? "delete: STILL LISTED" : "deleted and gone")
            app.openCalendarEvent = nil
            app.openCalendar(CalendarLink(eventId: nil, at: start))
        } catch {
            note("failed: \(APIError.wrap(error).userMessage)")
        }
        // A failure halfway must not leave the event behind.
        if let leftover = created {
            do {
                try await api.delete(leftover.id)
                note("cleanup: deleted \(leftover.id)")
            } catch {
                note("cleanup FAILED for \(leftover.id): \(APIError.wrap(error).userMessage)")
            }
        }
        finish(lines)
    }

    private static func finish(_ lines: [String]) {
        guard let dir = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask).first else { return }
        try? (lines.joined(separator: "\n") + "\n").write(to: dir.appending(path: "calendar-selftest.log"), atomically: true, encoding: .utf8)
    }
}
#endif
