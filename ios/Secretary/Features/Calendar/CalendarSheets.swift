import MapKit
import SwiftUI

struct CalendarDetail: View {
    let app: AppModel
    let store: CalendarStore
    let edit: (CalendarEntry) -> Void
    @State private var event: CalendarEntry
    @Environment(\.dismiss) private var dismiss
    @Environment(\.openURL) private var openURL
    @State private var deleting = false
    @State private var notesOpen = false
    @State private var opened = false
    @State private var resolved: PlaceResolver.Resolved?
    @State private var guess: TravelGuess?
    @State private var guessing = false
    /// The iPhone's own event behind an `iphone:` entry: its full notes, and its settings change through EventKit.
    @State private var device: CalendarDeviceEvents.Details?

    /// A travel time worked out for the event's place.
    struct TravelGuess: Equatable {
        let minutes: Int
        let mode: String
    }

    init(app: AppModel, store: CalendarStore, event: CalendarEntry, edit: @escaping (CalendarEntry) -> Void) {
        self.app = app; self.store = store; self.edit = edit
        _event = State(initialValue: event)
    }

    private var location: String? { event.location?.trimmingCharacters(in: .whitespacesAndNewlines).nonEmptyCalendarText }
    private var place: Place? {
        guard let address = location, address.count <= 200, !address.contains(where: { $0.isNewline }), !address.hasPrefix("https://") else { return nil }
        return Place(title: String(event.title.prefix(120)), address: address, when: event.allDay ? nil : event.startDate,
                     end: event.allDay ? nil : event.endDate, eventId: event.id, kind: .event)
    }
    private var color: Color { CalendarMath.color(name: event.calendarName, source: event.source) }
    private var phones: [CalendarPhone] { CalendarMath.phones(event) }
    private var notes: String { CalendarMath.plainNotes(event.description) }
    /// Lucina's events go to Google while it is connected, Google's to Lucina; repeating events stay.
    private var canMove: Bool {
        event.editable && !event.recurring && (event.source == "google" || (event.source == "lucina" && store.googleWritable))
    }
    private var leaveCapable: Bool { place != nil && !event.allDay }
    private var canEdit: Bool { event.editable || device?.writable == true }
    /// On the phone's own events "time to leave" is an alert at the travel time (EventKit has no travel time).
    private var leaveOn: Bool {
        guard device != nil else { return event.leaveAlert }
        return guess.map { event.alarms.contains($0.minutes) } ?? false
    }

    var body: some View {
        Form {
            header
            if !phones.isEmpty {
                Section {
                    ForEach(phones, id: \.tel) { phone in phoneRow(phone) }
                }
            }
            if let location { locationSection(location) }
            if let meeting = CalendarMath.meetingURL(event) {
                Section {
                    Link(destination: meeting) { Label("Join call", systemImage: "video") }
                        .simultaneousGesture(TapGesture().onEnded { CalendarDebug.tap("join", event) })
                        .accessibilityIdentifier("calendar-meeting")
                }
            }
            if let strip = CalendarMath.dayStrip(for: event, among: store.events) {
                Section {
                    CalendarDayStripView(strip: strip)
                        .padding(.vertical, 6)
                        .accessibilityIdentifier("calendar-day-strip")
                }
            }
            settingsSection
            if !notes.isEmpty { notesSection }
            if !event.attendees.isEmpty {
                Section("Guests") { ForEach(Array(event.attendees.enumerated()), id: \.offset) { _, guest in Text(guest).textSelection(.enabled) } }
            }
            Section {
                if let addedBy = event.addedBy { Text("Added by \(addedBy)") }
                if !canEdit { Text("This calendar is read-only.").foregroundStyle(.secondary) }
                if event.source == "google", let raw = event.url, let url = URL(string: raw), url.scheme == "https" {
                    Link("Open in Google Calendar", destination: url)
                }
                ShareLink(item: event.publicLink)
            }
            if let error = store.error { Text(error).foregroundStyle(.red) }
            if event.editable {
                Section {
                    Button("Edit", systemImage: "pencil") { CalendarDebug.tap("edit", event); edit(event) }.accessibilityIdentifier("calendar-edit")
                    Button("Delete event", systemImage: "trash", role: .destructive) { CalendarDebug.tap("delete", event); deleting = true }
                        .accessibilityIdentifier("calendar-delete")
                }.disabled(store.busy)
            }
        }
        .navigationTitle("Event").navigationBarTitleDisplayMode(.inline).lucinaSheet()
        .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } } }
        .alert("Delete event?", isPresented: $deleting) {
            Button("Cancel", role: .cancel) {}
            Button("Delete", role: .destructive) { Task { if await store.delete(event) { dismiss() } } }
        } message: { Text(event.recurring ? String(localized: "Only this occurrence will be deleted.") : String(localized: "This event will be deleted.")) }
        .onAppear {
            guard !opened else { return }
            opened = true
            loadDevice()
            CalendarDebug.open(event, phones: phones.count, hasLocation: location != nil)
        }
        .task(id: place?.searchQuery) { await resolvePlace() }
    }

    // MARK: Sections

    private var header: some View {
        Section {
            HStack(alignment: .top, spacing: 12) {
                RoundedRectangle(cornerRadius: 2).fill(color).frame(width: 4)
                VStack(alignment: .leading, spacing: 4) {
                    Text(event.title).font(.title2.bold()).textSelection(.enabled).accessibilityIdentifier("calendar-detail-title")
                    let (start, end) = CalendarMath.bounds(event)
                    if event.allDay {
                        Text(CalendarMath.whenLabel(event))
                        Text("All day").foregroundStyle(.secondary)
                    } else if Calendar.current.isDate(start, inSameDayAs: end.addingTimeInterval(-1)) {
                        Text(CalendarMath.longDay(start))
                        Text(CalendarMath.format(start, template: "HHmm") + "–" + CalendarMath.format(end, template: "HHmm"))
                    } else {
                        Text(CalendarMath.whenLabel(event))
                    }
                    if event.recurring { Label("Repeating event", systemImage: "repeat").foregroundStyle(.secondary).font(.subheadline) }
                }
            }
            .fixedSize(horizontal: false, vertical: true)
            .listRowBackground(Color.clear)
            .listRowInsets(EdgeInsets(top: 4, leading: 4, bottom: 4, trailing: 4))
        }
    }

    private func phoneRow(_ phone: CalendarPhone) -> some View {
        HStack(spacing: 12) {
            Image(systemName: "phone.fill").foregroundStyle(.white)
                .frame(width: 36, height: 36).background(Color.green, in: RoundedRectangle(cornerRadius: 9))
                .accessibilityHidden(true)
            Text(phone.display).font(.body.monospacedDigit()).textSelection(.enabled)
            Spacer(minLength: 8)
            Button("Call") {
                CalendarDebug.tap("call", event)
                if let url = phone.url { openURL(url) }
            }
            .buttonStyle(.bordered).accessibilityIdentifier("calendar-call")
            ShareLink(item: phone.display) { Image(systemName: "square.and.arrow.up").accessibilityLabel(Text("Share")) }
                .buttonStyle(.borderless)
                .simultaneousGesture(TapGesture().onEnded { CalendarDebug.tap("share_phone", event) })
        }
    }

    private func locationSection(_ location: String) -> some View {
        Section("Location") {
            Button { openInMaps(location) } label: {
                HStack(spacing: 12) {
                    Text(location).multilineTextAlignment(.leading).foregroundStyle(.primary)
                        .frame(maxWidth: .infinity, alignment: .leading)
                    if let coordinate = resolved?.coordinate { CalendarMapThumb(coordinate: coordinate) }
                }
            }
            .accessibilityIdentifier("calendar-location-card")
            if let place {
                Button("Show on map", systemImage: "map") { CalendarDebug.tap("show_on_map", event); dismiss(); app.showPlace(place) }
                    .accessibilityIdentifier("calendar-map")
            }
            if let google = CalendarMath.googleMapsURL(location) {
                Link(destination: google) { Label("Google Maps", systemImage: "arrow.up.right.square") }
                    .simultaneousGesture(TapGesture().onEnded { CalendarDebug.tap("google_maps", event) })
            }
        }
    }

    private var notesSection: some View {
        let fold = CalendarMath.notesFold(notes)
        return Section("More details") {
            Text(CalendarMath.liveNotes(notes))
                .lineLimit(fold && !notesOpen ? CalendarMath.notesFoldLines : nil)
                .textSelection(.enabled)
                .environment(\.openURL, OpenURLAction { url in
                    CalendarDebug.tap("notes_link", event, kind: url.host == "maps.apple.com" ? "address" : url.scheme)
                    return .systemAction
                })
                .accessibilityIdentifier("calendar-notes")
            if fold {
                Button(notesOpen ? LocalizedStringKey("Show less") : LocalizedStringKey("Show more")) {
                    notesOpen.toggle()
                    CalendarDebug.tap(notesOpen ? "notes_more" : "notes_less", event)
                }
                .accessibilityIdentifier("calendar-notes-more")
            }
        }
    }

    // MARK: Settings

    private var settingsSection: some View {
        Section {
            calendarRow
            if event.editable || device?.availabilityEditable == true {
                Picker(selection: Binding(get: { event.free }, set: { free in
                    change(CalendarSettingsPatch(free: free), "show_as", free ? "free" : "busy")
                })) {
                    Text("Busy").tag(false)
                    Text("Free").tag(true)
                } label: { Label("Show as", systemImage: "hand.raised") }
                .accessibilityIdentifier("calendar-show-as")
            } else {
                LabeledContent { Text(event.free ? LocalizedStringKey("Free") : LocalizedStringKey("Busy")) } label: { Label("Show as", systemImage: "hand.raised") }
            }
            alertRows
            if leaveCapable && (canEdit || event.leaveAlert) {
                Toggle(isOn: Binding(get: { leaveOn }, set: { setLeave($0) })) {
                    Label("Time to leave", systemImage: "figure.walk.departure")
                }
                .disabled(!canEdit || guessing)
                .accessibilityIdentifier("calendar-leave-alert")
            }
            travelRow
        } header: {
            Text("Settings")
        } footer: {
            if event.alarmsDefault && !event.alarms.isEmpty { Text("The calendar's default alerts.") }
        }
        .disabled(store.busy)
    }

    @ViewBuilder private var calendarRow: some View {
        let label = Label("Calendar", systemImage: "calendar")
        if let device, device.calendars.count > 1 {
            Picker(selection: Binding(get: { device.calendarId }, set: { moveOnPhone(to: $0) })) {
                ForEach(device.calendars) { Text($0.title).tag($0.id) }
            } label: { label }
            .accessibilityIdentifier("calendar-move")
        } else if canMove {
            Picker(selection: Binding(get: { event.source }, set: { move(to: $0) })) {
                Text("Lucina").tag("lucina")
                Text("Google").tag("google")
            } label: { label }
            .accessibilityIdentifier("calendar-move")
        } else {
            LabeledContent {
                HStack(spacing: 6) {
                    Circle().fill(color).frame(width: 9, height: 9)
                    Text(event.calendarName).lineLimit(1)
                }
            } label: { label }
        }
    }

    /// One row per alert, and an empty one to add another (while there are fewer than five).
    @ViewBuilder private var alertRows: some View {
        let alarms = event.alarms
        if canEdit {
            ForEach(Array(alarms.enumerated()), id: \.offset) { index, minutes in
                Picker(selection: Binding(get: { minutes }, set: { value in
                    var next = alarms
                    if value < 0 { next.remove(at: index) } else { next[index] = value }
                    setAlarms(next)
                })) {
                    Text("None").tag(-1)
                    ForEach(Self.choices(CalendarMath.alarmChoices, with: minutes), id: \.self) { Text(CalendarMath.alarmLabel($0)).tag($0) }
                } label: { Label("Alert", systemImage: "bell") }
                .accessibilityIdentifier("calendar-alert-\(index)")
            }
            if alarms.count < CalendarMath.maxAlarms {
                let isDefault = event.alarmsDefault && alarms.isEmpty
                Picker(selection: Binding(get: { isDefault ? -2 : -1 }, set: { value in
                    if value >= 0 { setAlarms(alarms + [value]) }
                })) {
                    if isDefault { Text("Default").tag(-2) }
                    Text("None").tag(-1)
                    ForEach(CalendarMath.alarmChoices.filter { !alarms.contains($0) }, id: \.self) { Text(CalendarMath.alarmLabel($0)).tag($0) }
                } label: { Label("Alert", systemImage: "bell") }
                .accessibilityIdentifier("calendar-alert-add")
            }
        } else if alarms.isEmpty {
            LabeledContent { Text("None") } label: { Label("Alert", systemImage: "bell") }
        } else {
            ForEach(alarms, id: \.self) { minutes in
                LabeledContent { Text(CalendarMath.alarmLabel(minutes)) } label: { Label("Alert", systemImage: "bell") }
            }
        }
    }

    @ViewBuilder private var travelRow: some View {
        let current = event.travelMin ?? 0
        if event.editable && !event.allDay {
            Picker(selection: Binding(get: { current }, set: { setTravel($0) })) {
                ForEach(Self.choices(CalendarMath.travelChoices, with: current, guess?.minutes), id: \.self) { minutes in
                    if let guess, guess.minutes == minutes {
                        Text(CalendarMath.travelLabel(minutes) + " · " + guess.mode).tag(minutes)
                    } else {
                        Text(CalendarMath.travelLabel(minutes)).tag(minutes)
                    }
                }
            } label: { Label("Travel time", systemImage: "car") }
            .accessibilityIdentifier("calendar-travel")
        } else {
            LabeledContent {
                if event.travelMin == nil, let guess {
                    Text(CalendarMath.travelLabel(guess.minutes) + " · " + guess.mode)
                } else {
                    Text(CalendarMath.travelLabel(event.travelMin))
                }
            } label: { Label("Travel time", systemImage: "car") }
        }
    }

    /// The fixed choices plus the values in use, ascending.
    private static func choices(_ base: [Int], with extra: Int?...) -> [Int] {
        Array(Set(base + extra.compactMap { $0 }.filter { $0 >= 0 })).sorted()
    }

    // MARK: Actions

    /// Reads the phone's own event: what it says wins over the server's snapshot of it.
    private func loadDevice() {
        guard let found = CalendarDeviceEvents.shared.details(for: event) else {
            if event.source == "iphone" { CalendarDebug.deviceEvent(event, found: false, writable: false) }
            return
        }
        device = found
        if let notes = found.notes, !notes.isEmpty { event.description = notes }
        if event.url == nil { event.url = found.url }
        event.alarms = found.alarms
        event.free = found.free
        CalendarDebug.deviceEvent(event, found: true, writable: found.writable)
    }

    private func change(_ patch: CalendarSettingsPatch, _ setting: String, _ value: String) {
        let before = event
        if let device {
            do {
                try CalendarDeviceEvents.shared.apply(patch, identifier: device.identifier, start: before.startDate)
                event = patch.applied(to: before)
                store.adopt(event)
                CalendarDebug.setting(setting, before, value: value, ok: true)
            } catch {
                store.error = String(localized: "Couldn't save this on the iPhone.")
                CalendarDebug.setting(setting, before, value: value, ok: false, error: String((error as NSError).code))
            }
            return
        }
        event = patch.applied(to: before)
        Task {
            if let saved = await store.update(before, settings: patch) {
                event = saved
                CalendarDebug.setting(setting, before, value: value, ok: true)
            } else {
                event = before
                CalendarDebug.setting(setting, before, value: value, ok: false, error: "failed")
            }
        }
    }

    private func setAlarms(_ alarms: [Int]) {
        let list = Array(Set(alarms)).sorted()
        change(CalendarSettingsPatch(alarms: .some(list)), "alarms", list.map(String.init).joined(separator: ","))
    }

    private func setTravel(_ minutes: Int) {
        var patch = CalendarSettingsPatch(travelMin: minutes)
        // A leave alert needs a travel time.
        if minutes == 0 && event.leaveAlert { patch.leaveAlert = false }
        change(patch, "travel", String(minutes))
    }

    private func setLeave(_ on: Bool) {
        if device != nil { return setLeaveOnPhone(on) }
        guard on else { return change(CalendarSettingsPatch(leaveAlert: false), "leave_alert", "off") }
        if let travel = event.travelMin { return change(CalendarSettingsPatch(leaveAlert: true), "leave_alert", "on_\(travel)") }
        Task {
            guessing = true
            defer { guessing = false }
            var found = guess
            if found == nil { found = await travelGuess() }
            guard let found else {
                store.error = String(localized: "Couldn't work out the travel time. Pick one under Travel time.")
                CalendarDebug.setting("leave_alert", event, value: "on", ok: false, error: "no_travel_time")
                return
            }
            guess = found
            change(CalendarSettingsPatch(travelMin: found.minutes, leaveAlert: true), "leave_alert", "on_\(found.minutes)")
        }
    }

    private func setLeaveOnPhone(_ on: Bool) {
        Task {
            guessing = true
            defer { guessing = false }
            var found = guess
            if found == nil { found = await travelGuess() }
            guard let found else {
                store.error = String(localized: "Couldn't work out the travel time. Pick one under Travel time.")
                CalendarDebug.setting("leave_alert", event, value: "on", ok: false, error: "no_travel_time")
                return
            }
            guess = found
            let others = event.alarms.filter { $0 != found.minutes }
            setAlarms(on ? others + [found.minutes] : others)
        }
    }

    private func moveOnPhone(to calendarId: String) {
        guard let current = device, calendarId != current.calendarId else { return }
        do {
            let moved = try CalendarDeviceEvents.shared.move(identifier: current.identifier, start: event.startDate, to: calendarId)
            device = moved
            event.calendar = CalendarDeviceEvents.shared.calendarTitle(calendarId) ?? event.calendar
            store.adopt(event)
            CalendarDebug.setting("calendar", event, value: "phone", ok: true)
        } catch {
            store.error = String(localized: "Couldn't save this on the iPhone.")
            CalendarDebug.setting("calendar", event, value: "phone", ok: false, error: String((error as NSError).code))
        }
    }

    private func move(to target: String) {
        guard target != event.source else { return }
        let before = event
        Task {
            if let moved = await store.move(before, to: target) {
                event = moved
                CalendarDebug.setting("calendar", before, value: target, ok: true)
            } else {
                CalendarDebug.setting("calendar", before, value: target, ok: false, error: "failed")
            }
        }
    }

    private func openInMaps(_ location: String) {
        CalendarDebug.tap("map", event, kind: resolved == nil ? "search" : "pin")
        if let coordinate = resolved?.coordinate {
            let item = MKMapItem(placemark: MKPlacemark(coordinate: coordinate))
            item.name = location
            item.openInMaps()
        } else if let url = CalendarMath.mapsURL(location) {
            openURL(url)
        }
    }

    // MARK: Place and travel time

    private func resolvePlace() async {
        guard let place else { resolved = nil; return }
        let found = await PlaceResolver.shared.resolve(place, api: app.api)
        guard !Task.isCancelled else { return }
        resolved = found
        CalendarDebug.map(event, found: found != nil)
        if found != nil, canEdit, !event.allDay, event.endDate > Date(), guess == nil {
            guess = await travelGuess()
        }
    }

    /// The travel time by bike from here (else home) to the place.
    private func travelGuess() async -> TravelGuess? {
        guard let destination = resolved?.coordinate else {
            CalendarDebug.travel(event, via: "none", minutes: nil, mode: nil)
            return nil
        }
        await PlaceResolver.shared.loadHome(api: app.api)
        PlaceLocation.shared.refresh()
        let home = PlaceResolver.shared.home ?? CLLocationCoordinate2D(latitude: 56.19633, longitude: 10.2011)
        guard let origin = PlaceOrigin.current(home: home) else { return nil }
        guard let route = await PlaceRoute.calculate(.bike, from: origin.coordinate, to: destination),
              let minutes = CalendarMath.travelMinutes(seconds: route.seconds) else {
            CalendarDebug.travel(event, via: "none", minutes: nil, mode: nil)
            return nil
        }
        CalendarDebug.travel(event, via: route.source.rawValue, minutes: minutes, mode: PlaceTravelMode.bike.rawValue)
        return TravelGuess(minutes: minutes, mode: route.modeText)
    }
}

private extension String {
    var nonEmptyCalendarText: String? { isEmpty ? nil : self }
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
