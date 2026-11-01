import SwiftUI

struct CalendarDetail: View {
    let app: AppModel
    let store: CalendarStore
    let event: CalendarEntry
    let edit: () -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var deleting = false

    private var place: Place? {
        guard let address = event.location?.trimmingCharacters(in: .whitespacesAndNewlines), !address.isEmpty,
              address.count <= 200, !address.contains(where: { $0.isNewline }), !address.hasPrefix("https://") else { return nil }
        return Place(title: String(event.title.prefix(120)), address: address, when: event.allDay ? nil : event.startDate,
                     end: event.allDay ? nil : event.endDate, eventId: event.id, kind: .event)
    }
    var body: some View {
        Form {
            Section {
                Text(event.title).font(.title2.bold()).textSelection(.enabled)
                Label(event.calendarName, systemImage: "circle.fill").foregroundStyle(CalendarMath.color(name: event.calendarName, source: event.source))
                Text(CalendarMath.whenLabel(event))
                if event.allDay { Text("All day").foregroundStyle(.secondary) }
                if event.recurring { Label("Repeating event", systemImage: "repeat").foregroundStyle(.secondary) }
            }
            if let location = event.location, !location.isEmpty {
                Section("Location") {
                    Text(location).textSelection(.enabled)
                    if let place {
                        Button("Show on map", systemImage: "map") { dismiss(); app.showPlace(place) }
                            .accessibilityIdentifier("calendar-map")
                    }
                }
            }
            if let meeting = CalendarMath.meetingURL(event) {
                Link(destination: meeting) { Label("Join call", systemImage: "video") }.accessibilityIdentifier("calendar-meeting")
            }
            let notes = CalendarMath.plainNotes(event.description)
            if !notes.isEmpty { Section("Notes") { Text(notes).textSelection(.enabled) } }
            if !event.attendees.isEmpty {
                Section("Guests") { ForEach(Array(event.attendees.enumerated()), id: \.offset) { _, guest in Text(guest).textSelection(.enabled) } }
            }
            Section {
                if let addedBy = event.addedBy { Text("Added by \(addedBy)") }
                if !event.editable { Text("This calendar is read-only.").foregroundStyle(.secondary) }
                if event.source == "google", let raw = event.url, let url = URL(string: raw), url.scheme == "https" {
                    Link("Open in Google Calendar", destination: url)
                }
                ShareLink(item: event.publicLink)
            }
            if let error = store.error { Text(error).foregroundStyle(.red) }
            if event.editable {
                Section {
                    Button("Edit", systemImage: "pencil", action: edit).accessibilityIdentifier("calendar-edit")
                    Button("Delete", systemImage: "trash", role: .destructive) { deleting = true }.accessibilityIdentifier("calendar-delete")
                }.disabled(store.busy)
            }
        }
        .navigationTitle("Event").navigationBarTitleDisplayMode(.inline).lucinaSheet()
        .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } } }
        .alert("Delete event?", isPresented: $deleting) {
            Button("Cancel", role: .cancel) {}
            Button("Delete", role: .destructive) { Task { if await store.delete(event) { dismiss() } } }
        } message: { Text(event.recurring ? String(localized: "Only this occurrence will be deleted.") : String(localized: "This event will be deleted.")) }
    }
}

struct CalendarEditor: View {
    let store: CalendarStore
    let event: CalendarEntry?
    @Environment(\.dismiss) private var dismiss
    @State private var form: CalendarForm
    @State private var attempted = false

    init(store: CalendarStore, day: Date, event: CalendarEntry? = nil) {
        self.store = store; self.event = event
        _form = State(initialValue: CalendarForm(day: day, event: event))
    }
    var body: some View {
        Form {
            TextField("Title", text: $form.title).accessibilityIdentifier("calendar-editor-title")
            Toggle("All day", isOn: $form.allDay).accessibilityIdentifier("calendar-editor-all-day")
            DatePicker("Start", selection: Binding(get: { form.start }, set: { next in
                form.end = CalendarMath.shiftEnd(start: form.start, end: form.end, next: next, allDay: form.allDay)
                form.start = next
            }), displayedComponents: form.allDay ? [.date] : [.date, .hourAndMinute]).accessibilityIdentifier("calendar-editor-start")
            DatePicker("End", selection: $form.end, displayedComponents: form.allDay ? [.date] : [.date, .hourAndMinute])
                .accessibilityIdentifier("calendar-editor-end")
            TextField("Location", text: $form.location).accessibilityIdentifier("calendar-editor-location")
            TextField("Meeting link", text: $form.meeting).keyboardType(.URL).textInputAutocapitalization(.never).autocorrectionDisabled()
                .accessibilityIdentifier("calendar-editor-meeting")
            Section("Notes") {
                TextEditor(text: $form.notes).frame(minHeight: 120).accessibilityIdentifier("calendar-editor-notes")
            }
            if let error = form.error, attempted || !CalendarMath.validEnd(start: form.start, end: form.end, allDay: form.allDay) {
                Text(error).foregroundStyle(.red)
            }
            if let error = store.error { Text(error).foregroundStyle(.red) }
        }
        .lucinaSheet().navigationTitle(event == nil ? Text("New event") : Text("Edit event"))
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() }.disabled(store.busy) }
            ToolbarItem(placement: .confirmationAction) {
                Button("Save") {
                    attempted = true
                    guard form.error == nil else { return }
                    Task { if await store.save(form.draft(editing: event != nil), id: event?.id) { dismiss() } }
                }.disabled(store.busy).accessibilityIdentifier("calendar-editor-save")
            }
        }
        .interactiveDismissDisabled(store.busy)
    }
}
