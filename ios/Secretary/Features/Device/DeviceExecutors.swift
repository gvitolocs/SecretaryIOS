import EventKit
import Foundation
import SwiftUI
#if canImport(AlarmKit)
import AlarmKit
#endif

/// Executes Lucina's device commands: alarms and timers with AlarmKit (iOS 26; they appear
/// like Clock alarms, but Clock's own alarms are out of reach), reminders and calendar
/// events with EventKit (full access). Also gathers `device_state`.
@MainActor
final class DeviceExecutors: DeviceCommandExecuting {
    private let eventStore = EKEventStore()
    private let access: DeviceAccess
    /// AlarmKit alarms carry no label; ours are remembered here (id → label/kind).
    private var alarmNotes: [String: [String: String]] {
        get { UserDefaults.standard.dictionary(forKey: "secretary.device.alarmNotes") as? [String: [String: String]] ?? [:] }
        set { UserDefaults.standard.set(newValue, forKey: "secretary.device.alarmNotes") }
    }

    init(access: DeviceAccess) {
        self.access = access
    }

    func execute(_ command: DeviceCommand) async -> CommandResult {
        do {
            switch command.kind {
            case .alarmSet: return try await setAlarm(command)
            case .alarmDelete, .timerCancel: return try cancelAlarm(command)
            case .timerStart: return try await startTimer(command)
            case .reminderCreate: return try await createReminder(command)
            case .reminderComplete, .reminderDelete: return try await changeReminder(command)
            case .calendarCreate: return try await createEvent(command)
            case .calendarDelete: return try await deleteEvent(command)
            }
        } catch let error as BridgeError {
            return .failure(error.message)
        } catch {
            return .failure(error.localizedDescription)
        }
    }

    struct BridgeError: Error {
        let message: String
    }

    // MARK: - Alarms and timers (AlarmKit)

    private func requireAlarms() async throws {
        guard access.isOn(.alarms) else { throw BridgeError(message: "alarms are turned off in Lucina's access") }
        #if canImport(AlarmKit)
        guard #available(iOS 26.0, *) else { throw BridgeError(message: "alarms need iOS 26") }
        let manager = AlarmManager.shared
        if manager.authorizationState == .notDetermined { _ = try await manager.requestAuthorization() }
        guard manager.authorizationState == .authorized else { throw BridgeError(message: "AlarmKit is not authorized") }
        #else
        throw BridgeError(message: "AlarmKit is not available")
        #endif
    }

    /// `time`: "07:00", or an ISO 8601 date for a one-off alarm. `repeat`: ["mon", "wed", …].
    private func setAlarm(_ command: DeviceCommand) async throws -> CommandResult {
        try await requireAlarms()
        #if canImport(AlarmKit)
        guard #available(iOS 26.0, *) else { throw BridgeError(message: "alarms need iOS 26") }
        guard let time = command.string("time") else { throw BridgeError(message: "alarm.set needs time") }
        let label = command.string("label") ?? String(localized: "Alarm")
        let schedule: Alarm.Schedule
        if let parts = Self.clock(time) {
            let weekdays = (command.args["repeat"]?.arrayValue ?? []).compactMap { $0.stringValue.flatMap(Self.weekday) }
            schedule = .relative(.init(time: .init(hour: parts.hour, minute: parts.minute), repeats: weekdays.isEmpty ? .never : .weekly(weekdays)))
        } else if let date = Self.date(time) {
            schedule = .fixed(date)
        } else {
            throw BridgeError(message: "unreadable time \(time)")
        }
        let id = UUID()
        let attributes = AlarmAttributes<LucinaAlarmMetadata>(
            presentation: AlarmPresentation(alert: Self.alert(label)),
            metadata: LucinaAlarmMetadata(label: label), tintColor: Color("AccentFill"))
        _ = try await AlarmManager.shared.schedule(id: id, configuration: .alarm(schedule: schedule, attributes: attributes))
        alarmNotes[id.uuidString] = ["label": label, "kind": "alarm", "time": time]
        return .success(id.uuidString)
        #else
        throw BridgeError(message: "AlarmKit is not available")
        #endif
    }

    private func startTimer(_ command: DeviceCommand) async throws -> CommandResult {
        try await requireAlarms()
        #if canImport(AlarmKit)
        guard #available(iOS 26.0, *) else { throw BridgeError(message: "timers need iOS 26") }
        guard let seconds = command.number("seconds"), seconds >= 1 else { throw BridgeError(message: "timer.start needs seconds") }
        let label = command.string("label") ?? String(localized: "Timer")
        let id = UUID()
        let attributes = AlarmAttributes<LucinaAlarmMetadata>(
            presentation: AlarmPresentation(alert: Self.alert(label),
                                            countdown: .init(title: LocalizedStringResource(stringLiteral: label))),
            metadata: LucinaAlarmMetadata(label: label), tintColor: Color("AccentFill"))
        _ = try await AlarmManager.shared.schedule(id: id, configuration: .timer(duration: seconds, attributes: attributes))
        alarmNotes[id.uuidString] = ["label": label, "kind": "timer", "seconds": String(Int(seconds)), "ends": ISO8601DateFormatter().string(from: Date().addingTimeInterval(seconds))]
        return .success(id.uuidString)
        #else
        throw BridgeError(message: "AlarmKit is not available")
        #endif
    }

    private func cancelAlarm(_ command: DeviceCommand) throws -> CommandResult {
        guard access.isOn(.alarms) else { throw BridgeError(message: "alarms are turned off in Lucina's access") }
        #if canImport(AlarmKit)
        guard #available(iOS 26.0, *) else { throw BridgeError(message: "alarms need iOS 26") }
        guard let raw = command.string("id"), let id = UUID(uuidString: raw) else { throw BridgeError(message: "unknown alarm id") }
        try AlarmManager.shared.cancel(id: id)
        alarmNotes[raw] = nil
        return .success(raw)
        #else
        throw BridgeError(message: "AlarmKit is not available")
        #endif
    }

    #if canImport(AlarmKit)
    @available(iOS 26.0, *)
    static func alert(_ label: String) -> AlarmPresentation.Alert {
        let title = LocalizedStringResource(stringLiteral: label)
        if #available(iOS 26.1, *) { return AlarmPresentation.Alert(title: title) }
        return AlarmPresentation.Alert(title: title, stopButton: AlarmButton(text: "Stop", textColor: .white, systemImageName: "stop.circle"))
    }
    #endif

    static func clock(_ text: String) -> (hour: Int, minute: Int)? {
        let parts = text.split(separator: ":").compactMap { Int($0) }
        guard parts.count == 2, (0..<24).contains(parts[0]), (0..<60).contains(parts[1]), text.count <= 5 else { return nil }
        return (parts[0], parts[1])
    }

    static func date(_ text: String) -> Date? {
        let full = ISO8601DateFormatter()
        full.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return full.date(from: text) ?? ISO8601DateFormatter().date(from: text)
    }

    static func weekday(_ text: String) -> Locale.Weekday? {
        switch text.lowercased().prefix(3) {
        case "mon": return .monday
        case "tue": return .tuesday
        case "wed": return .wednesday
        case "thu": return .thursday
        case "fri": return .friday
        case "sat": return .saturday
        case "sun": return .sunday
        default: return nil
        }
    }

    // MARK: - Reminders and calendar (EventKit)

    private func requireReminders() async throws {
        guard access.isOn(.reminders) else { throw BridgeError(message: "reminders are turned off in Lucina's access") }
        if EKEventStore.authorizationStatus(for: .reminder) != .fullAccess {
            guard try await eventStore.requestFullAccessToReminders() else { throw BridgeError(message: "reminders access was not granted") }
        }
    }

    private func requireCalendar() async throws {
        guard access.isOn(.calendar) else { throw BridgeError(message: "the calendar is turned off in Lucina's access") }
        if EKEventStore.authorizationStatus(for: .event) != .fullAccess {
            guard try await eventStore.requestFullAccessToEvents() else { throw BridgeError(message: "calendar access was not granted") }
        }
    }

    private func createReminder(_ command: DeviceCommand) async throws -> CommandResult {
        try await requireReminders()
        guard let title = command.string("title") else { throw BridgeError(message: "reminder.create needs title") }
        let reminder = EKReminder(eventStore: eventStore)
        reminder.title = title
        if let listName = command.string("list"),
           let list = eventStore.calendars(for: .reminder).first(where: { $0.title.localizedCaseInsensitiveCompare(listName) == .orderedSame }) {
            reminder.calendar = list
        } else {
            reminder.calendar = eventStore.defaultCalendarForNewReminders()
        }
        if let due = command.string("due").flatMap(Self.date) {
            reminder.dueDateComponents = Calendar.current.dateComponents([.year, .month, .day, .hour, .minute], from: due)
            reminder.addAlarm(EKAlarm(absoluteDate: due))
        }
        try eventStore.save(reminder, commit: true)
        return .success(reminder.calendarItemIdentifier)
    }

    private func changeReminder(_ command: DeviceCommand) async throws -> CommandResult {
        try await requireReminders()
        guard let id = command.string("id"), let reminder = eventStore.calendarItem(withIdentifier: id) as? EKReminder else {
            throw BridgeError(message: "unknown reminder id")
        }
        if command.kind == .reminderComplete {
            reminder.isCompleted = true
            try eventStore.save(reminder, commit: true)
        } else {
            try eventStore.remove(reminder, commit: true)
        }
        return .success(id)
    }

    private func createEvent(_ command: DeviceCommand) async throws -> CommandResult {
        try await requireCalendar()
        guard let title = command.string("title"), let start = command.string("start").flatMap(Self.date) else {
            throw BridgeError(message: "calendar.create needs title and start")
        }
        let event = EKEvent(eventStore: eventStore)
        event.title = title
        event.startDate = start
        event.endDate = command.string("end").flatMap(Self.date) ?? start.addingTimeInterval(3600)
        event.isAllDay = command.args["allDay"]?.boolValue ?? false
        event.location = command.string("location")
        event.notes = command.string("notes")
        event.calendar = eventStore.defaultCalendarForNewEvents
        try eventStore.save(event, span: .thisEvent, commit: true)
        return .success(event.eventIdentifier)
    }

    private func deleteEvent(_ command: DeviceCommand) async throws -> CommandResult {
        try await requireCalendar()
        guard let id = command.string("id"), let event = eventStore.event(withIdentifier: id) else { throw BridgeError(message: "unknown event id") }
        try eventStore.remove(event, span: .thisEvent, commit: true)
        return .success(id)
    }

    // MARK: - State

    func currentState() async -> DeviceState {
        var state = DeviceState()
        let iso = ISO8601DateFormatter()
        #if canImport(AlarmKit)
        if #available(iOS 26.0, *), access.isOn(.alarms), AlarmManager.shared.authorizationState == .authorized,
           let alarms = try? AlarmManager.shared.alarms {
            let notes = alarmNotes
            for alarm in alarms {
                let note = notes[alarm.id.uuidString] ?? [:]
                var entry: [String: JSONValue] = ["id": .string(alarm.id.uuidString), "state": .string(String(describing: alarm.state))]
                if let label = note["label"] { entry["label"] = .string(label) }
                if case let .relative(relative)? = alarm.schedule {
                    entry["time"] = .string(String(format: "%02d:%02d", relative.time.hour, relative.time.minute))
                    if case let .weekly(days) = relative.repeats {
                        entry["repeat"] = .array(days.map { .string(String(describing: $0).prefix(3).lowercased()) })
                    }
                } else if case let .fixed(date)? = alarm.schedule {
                    entry["time"] = .string(iso.string(from: date))
                }
                if note["kind"] == "timer" {
                    if let ends = note["ends"] { entry["ends"] = .string(ends) }
                    state.timers.append(.object(entry))
                } else {
                    state.alarms.append(.object(entry))
                }
            }
            // Forget notes of alarms that are gone.
            let live = Set(alarms.map(\.id.uuidString))
            alarmNotes = notes.filter { live.contains($0.key) }
        }
        #endif
        if access.isOn(.reminders), EKEventStore.authorizationStatus(for: .reminder) == .fullAccess {
            let predicate = eventStore.predicateForIncompleteReminders(withDueDateStarting: nil, ending: Date().addingTimeInterval(14 * 86_400), calendars: nil)
            let reminders: [EKReminder] = await withCheckedContinuation { continuation in
                eventStore.fetchReminders(matching: predicate) { continuation.resume(returning: $0 ?? []) }
            }
            state.reminders = reminders.prefix(200).map { reminder in
                var entry: [String: JSONValue] = ["id": .string(reminder.calendarItemIdentifier), "title": .string(reminder.title ?? "")]
                if let due = reminder.dueDateComponents?.date { entry["due"] = .string(iso.string(from: due)) }
                if let list = reminder.calendar?.title { entry["list"] = .string(list) }
                return .object(entry)
            }
        }
        if access.isOn(.calendar), EKEventStore.authorizationStatus(for: .event) == .fullAccess {
            // From three days back: a memo can reach the server hours or days after the lecture it was
            // recorded in, and lucina-memo links it to the event it overlaps (docs/memos.md, step 1b).
            let predicate = eventStore.predicateForEvents(withStart: Date().addingTimeInterval(-3 * 86_400), end: Date().addingTimeInterval(7 * 86_400), calendars: nil)
            state.events = eventStore.events(matching: predicate).sorted { $0.startDate < $1.startDate }.prefix(300).map { event in
                // Alerts as minutes before the start (relative ones only), and "show as free".
                let alarms = (event.alarms ?? []).filter { $0.absoluteDate == nil && $0.relativeOffset <= 0 }
                    .map { JSONValue.number(Double(Int((-$0.relativeOffset / 60).rounded()))) }
                return .object([
                    "id": .string(event.eventIdentifier ?? ""),
                    "title": .string(event.title ?? ""),
                    "start": .string(iso.string(from: event.startDate)),
                    "end": .string(iso.string(from: event.endDate)),
                    "allDay": .bool(event.isAllDay),
                    "calendar": .string(event.calendar?.title ?? ""),
                    "location": .string(event.location ?? ""),
                    "alarms": .array(Array(alarms.prefix(5))),
                    "free": .bool(event.availability == .free),
                ])
            }
        }
        if access.isOn(.focus), let focus = UserDefaults.standard.string(forKey: FocusRecorder.currentKey) {
            state.focus = .object(["mode": .string(focus)])
        }
        return state
    }
}

#if canImport(AlarmKit)
@available(iOS 26.0, *)
struct LucinaAlarmMetadata: AlarmMetadata {
    var label: String
}
#endif
