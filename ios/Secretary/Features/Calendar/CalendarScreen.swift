import SwiftUI

/// `/calendar?event=<id>&at=<ms>`: the event a notification or a chat link opens the calendar on.
struct CalendarLink: Equatable, Hashable, Sendable {
    var eventId: String?
    /// The occurrence's start (a recurring event's id is shared by every occurrence).
    var at: Date?
}

/// Lucina's own calendar (placeholder: filled in by the Calendar feature).
struct CalendarScreen: View {
    var body: some View {
        Text(verbatim: "Calendar").lucinaScreen("Calendar")
    }
}
