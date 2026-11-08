import Foundation

/// What the debug event log keeps of the event detail (docs/debug-events.md): the event's id and source, counts, which
/// element was tapped, which setting changed to what, and whether the server took it. Never the event's title, place,
/// notes or phone numbers. A separate file, so the views only gain one-line calls.
enum CalendarDebug {
    /// The detail opened: what it found to show.
    static func open(_ event: CalendarEntry, phones: Int, hasLocation: Bool) {
        DebugEvents.record("calendar.detail_open", ["id": event.id, "source": event.source, "editable": event.editable,
                                                    "phones": phones, "has_location": hasLocation, "alarms": event.alarms.count])
    }

    /// The phone's own event behind an `iphone:` entry: found through EventKit, and whether its calendar takes changes.
    static func deviceEvent(_ event: CalendarEntry, found: Bool, writable: Bool) {
        DebugEvents.record("calendar.detail_device", ["id": event.id, "found": found, "writable": writable])
    }

    /// The map thumbnail: whether the place was found.
    static func map(_ event: CalendarEntry, found: Bool) {
        DebugEvents.record("calendar.detail_map", ["id": event.id, "found": found])
    }

    /// A tap in the detail: `call`, `share_phone`, `map`, `join`, `notes_more`, `notes_less`, `notes_link`, `edit`, `delete`.
    static func tap(_ element: String, _ event: CalendarEntry, kind: String? = nil) {
        var fields: [String: Any] = ["screen": "calendar_detail", "element": element, "id": event.id]
        if let kind { fields["kind"] = kind }
        DebugEvents.record("ui.tap", fields)
    }

    /// A setting changed: `alarms`, `show_as`, `travel`, `leave_alert`, `calendar`; `value` as sent.
    static func setting(_ setting: String, _ event: CalendarEntry, value: String, ok: Bool, error: String? = nil) {
        var fields: [String: Any] = ["id": event.id, "source": event.source, "setting": setting, "value": value, "ok": ok]
        if let error { fields["error"] = error }
        DebugEvents.record("calendar.setting_change", fields)
    }

    /// The travel time for "time to leave": how it was found (`server`, `mapkit`, `none`) and the minutes.
    static func travel(_ event: CalendarEntry, via: String, minutes: Int?, mode: String?) {
        var fields: [String: Any] = ["id": event.id, "via": via]
        if let minutes { fields["minutes"] = minutes }
        if let mode { fields["mode"] = mode }
        DebugEvents.record("calendar.travel_estimate", fields)
    }
}
