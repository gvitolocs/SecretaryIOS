import EventKit
import Foundation

/// The iPhone's own calendar, for the event detail. The server's copy of an `iphone:` event is read-only and a snapshot
/// of it; on the phone the detail reads the event itself (all its notes, its alerts, show as) and changes it through
/// EventKit, so the change reaches the event's real calendar and the next snapshot brings it to the server.
@MainActor
final class CalendarDeviceEvents {
    static let shared = CalendarDeviceEvents()
    private let store = EKEventStore()

    struct Choice: Identifiable, Equatable {
        let id: String
        let title: String
    }

    struct Details: Equatable {
        /// EventKit's id (it can change when the event moves to another calendar).
        var identifier: String
        var notes: String?
        var url: String?
        var alarms: [Int]
        var free: Bool
        var calendarId: String
        /// The event's calendar takes changes (a subscribed timetable does not).
        var writable: Bool
        var availabilityEditable: Bool
        /// Where it can move, with its own calendar; empty for repeating events and invitations.
        var calendars: [Choice]
    }

    enum Failure: Error { case notFound, noAccess }

    private var hasAccess: Bool { EKEventStore.authorizationStatus(for: .event) == .fullAccess }

    /// The occurrence nearest `start` (EventKit gives every occurrence of a repeating event the same id).
    private func find(_ identifier: String, start: Date) -> EKEvent? {
        let day: TimeInterval = 86_400
        let predicate = store.predicateForEvents(withStart: start.addingTimeInterval(-day), end: start.addingTimeInterval(day), calendars: nil)
        return store.events(matching: predicate)
            .filter { $0.eventIdentifier == identifier }
            .min { abs($0.startDate.timeIntervalSince(start)) < abs($1.startDate.timeIntervalSince(start)) }
    }

    /// The event behind a merged `iphone:` entry, nil for other sources, without calendar access or when it is gone.
    func details(for entry: CalendarEntry) -> Details? {
        guard entry.source == "iphone", hasAccess, let identifier = CalendarMath.deviceEventIdentifier(entry.id),
              let event = find(identifier, start: entry.startDate) else { return nil }
        return details(of: event, identifier: identifier)
    }

    private func details(of event: EKEvent, identifier: String) -> Details {
        let writable = event.calendar?.allowsContentModifications ?? false
        let supported = event.calendar?.supportedEventAvailabilities ?? []
        let movable = writable && !event.hasRecurrenceRules && !event.hasAttendees
        let calendars = movable
            ? store.calendars(for: .event).filter(\.allowsContentModifications).map { Choice(id: $0.calendarIdentifier, title: $0.title) }
            : []
        let alarms = (event.alarms ?? []).filter { $0.absoluteDate == nil }.map(\.relativeOffset)
        return Details(identifier: event.eventIdentifier ?? identifier, notes: event.notes, url: event.url?.absoluteString,
                       alarms: CalendarMath.alarmMinutes(offsets: alarms), free: event.availability == .free,
                       calendarId: event.calendar?.calendarIdentifier ?? "", writable: writable,
                       availabilityEditable: writable && supported.contains(.busy) && supported.contains(.free), calendars: calendars)
    }

    /// Alerts and show as, on this occurrence. Alerts at a fixed date are left alone.
    func apply(_ patch: CalendarSettingsPatch, identifier: String, start: Date) throws {
        guard hasAccess else { throw Failure.noAccess }
        guard let event = find(identifier, start: start) else { throw Failure.notFound }
        if case let .some(.some(minutes)) = patch.alarms {
            for alarm in event.alarms ?? [] where alarm.absoluteDate == nil { event.removeAlarm(alarm) }
            for offset in CalendarMath.alarmOffsets(minutes: minutes) { event.addAlarm(EKAlarm(relativeOffset: offset)) }
        }
        if let free = patch.free { event.availability = free ? .free : .busy }
        try store.save(event, span: .thisEvent, commit: true)
    }

    /// Moves the event to another of the phone's calendars; its details afterwards.
    func move(identifier: String, start: Date, to calendarId: String) throws -> Details {
        guard hasAccess else { throw Failure.noAccess }
        guard let event = find(identifier, start: start), let calendar = store.calendar(withIdentifier: calendarId) else { throw Failure.notFound }
        event.calendar = calendar
        try store.save(event, span: .thisEvent, commit: true)
        return details(of: event, identifier: identifier)
    }

    func calendarTitle(_ calendarId: String) -> String? { store.calendar(withIdentifier: calendarId)?.title }
}
