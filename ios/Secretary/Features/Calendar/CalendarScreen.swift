import SwiftUI
import UIKit

struct CalendarScreen: View {
    @Environment(AppModel.self) private var app
    var body: some View {
        CalendarContent(app: app)
        #if DEBUG || SCREENSHOTS
            .task { await CalendarSelfTest.runIfRequested(app) }
        #endif
    }
}

private enum CalendarSheet: Identifiable {
    case detail(CalendarEntry), edit(CalendarEntry), new(Date)
    var id: String {
        switch self {
        case .detail(let e): return "detail:" + e.id
        case .edit(let e): return "edit:" + e.id
        case .new(let date): return "new:" + String(date.timeIntervalSince1970)
        }
    }
}

private struct CalendarContent: View {
    let app: AppModel
    @Environment(\.scenePhase) private var scenePhase
    @AppStorage("secretary.calendar.view") private var view = "month"
    @State private var store: CalendarStore
    @State private var selectedDay = Calendar.current.startOfDay(for: Date())
    @State private var showProposals = false
    @State private var agendaStart = Calendar.current.startOfDay(for: Date())
    @State private var agendaEnd = Calendar.current.date(byAdding: .day, value: 30, to: Date())!
    @State private var sheet: CalendarSheet?
    @State private var deleting: CalendarEntry?
    @State private var notice: String?
    @State private var copied = false
    @State private var lastVisibleAgendaDay: Date?

    init(app: AppModel) {
        self.app = app
        _store = State(initialValue: CalendarStore(api: { app.client.accessKey?.isEmpty == false ? CalendarAPI(client: app.client) : nil }, onSessionEnded: { app.handle($0) }))
    }
    private var days: [Date] { CalendarMath.monthDays(selectedDay) }
    private var range: DateInterval {
        if view == "agenda" { return DateInterval(start: agendaStart, end: agendaEnd) }
        return DateInterval(start: days[0], end: Calendar.current.date(byAdding: .day, value: 1, to: days[41])!)
    }
    var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 16, pinnedViews: [.sectionHeaders]) {
                controls
                proposals
                if let error = store.error {
                    Text(error).foregroundStyle(.red)
                    Button("Try again") { Task { await store.refresh() } }.accessibilityIdentifier("calendar-retry")
                }
                if let notice { Text(notice).font(.callout).foregroundStyle(.secondary) }
                if store.loading && !store.hasLoaded { ProgressView().frame(maxWidth: .infinity) }
                if view == "month" {
                    monthGrid
                    Text(CalendarMath.dayLabel(selectedDay)).font(.headline)
                    eventList(CalendarMath.events(store.visible, on: selectedDay))
                } else {
                    Button("Earlier events") {
                        agendaStart = Calendar.current.date(byAdding: .day, value: -30, to: agendaStart)!
                    }.accessibilityIdentifier("calendar-earlier")
                    let groups = CalendarMath.agenda(store.visible, from: agendaStart, to: agendaEnd)
                    if groups.isEmpty && !store.loading { empty }
                    ForEach(groups) { group in
                        Section {
                            ForEach(group.events) { event in
                                row(event).onAppear {
                                    if group.id == groups.last?.id, event.id == group.events.last?.id {
                                        lastVisibleAgendaDay = group.day
                                        loadMoreIfNeeded()
                                    }
                                }
                            }
                        } header: {
                            Text(CalendarMath.dayLabel(group.day)).font(.headline)
                                .frame(maxWidth: .infinity, alignment: .leading).padding(.vertical, 8).background(Theme.page)
                        }
                    }
                    Button("Later events") { extendAgenda() }
                        .accessibilityIdentifier("calendar-later")
                }
            }.padding()
        }
        .lucinaScreen("Calendar")
        .refreshable {
            await load(refresh: true)
            await store.loadProposals()
            if view == "agenda" { agendaStart = Calendar.current.date(byAdding: .day, value: -30, to: agendaStart)! }
        }
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button { sheet = .new(selectedDay) } label: { Label("New event", systemImage: "plus") }
                    .accessibilityIdentifier("calendar-new")
            }
            ToolbarItem(placement: .topBarTrailing) {
                Menu {
                    Button("Sync now", systemImage: "arrow.triangle.2.circlepath") { Task { await store.sync() } }.disabled(store.busy)
                    Section("Calendars") {
                        ForEach(store.calendars, id: \.name) { item in
                            Button { store.toggle(item.name) } label: {
                                Label(item.name, systemImage: store.hidden.contains(item.name) ? "circle" : "checkmark.circle.fill")
                            }.tint(CalendarMath.color(name: item.name, source: item.source))
                        }
                    }
                } label: { Label("Calendars", systemImage: "ellipsis.circle") }
                .accessibilityIdentifier("calendar-menu")
            }
        }
        .task(id: range) { if app.openCalendarEvent == nil { await load() } }
        .task {
            await store.loadProposals()
            await consumeLink()
            while !Task.isCancelled {
                do { try await Task.sleep(for: .seconds(300)) } catch { return }
                if scenePhase == .active && app.selectedTab == .plugins && app.showCalendar { await store.refresh() }
            }
        }
        .onChange(of: scenePhase) { _, phase in
            if phase == .active { Task { await store.refresh() } }
        }
        .onChange(of: store.loading) { _, loading in
            if !loading { loadMoreIfNeeded() }
        }
        .onChange(of: app.openCalendarEvent) { _, _ in Task { await consumeLink() } }
        .sheet(item: $sheet) { item in
            NavigationStack {
                switch item {
                case .detail(let event):
                    CalendarDetail(app: app, store: store, event: event, edit: { sheet = .edit($0) })
                case .edit(let event): CalendarEditor(store: store, day: selectedDay, event: event)
                case .new(let day): CalendarEditor(store: store, day: day)
                }
            }.lucinaSheet()
        }
        .alert("Delete event?", isPresented: Binding(get: { deleting != nil }, set: { if !$0 { deleting = nil } })) {
            Button("Cancel", role: .cancel) { deleting = nil }
            Button("Delete", role: .destructive) {
                if let event = deleting { Task { _ = await store.delete(event) } }
                deleting = nil
            }
        } message: {
            Text(deleting?.recurring == true ? String(localized: "Only this occurrence will be deleted.") : String(localized: "This event will be deleted."))
        }
        .overlay(alignment: .bottom) {
            if copied { Text("Copied").padding().background(.regularMaterial, in: Capsule()).padding() }
        }
    }
    private var controls: some View {
        VStack(spacing: 12) {
            HStack {
                Button("Today") {
                    selectedDay = Calendar.current.startOfDay(for: Date())
                    agendaStart = selectedDay; agendaEnd = Calendar.current.date(byAdding: .day, value: 30, to: selectedDay)!
                }.accessibilityIdentifier("calendar-today")
                Spacer()
                Button { changeMonth(-1) } label: { Label("Previous month", systemImage: "chevron.left").labelStyle(.iconOnly) }
                    .accessibilityIdentifier("calendar-previous")
                Text(CalendarMath.format(selectedDay, template: "MMMM yyyy")).font(.headline)
                Button { changeMonth(1) } label: { Label("Next month", systemImage: "chevron.right").labelStyle(.iconOnly) }
                    .accessibilityIdentifier("calendar-next")
            }
            Picker("View", selection: $view) { Text("Month").tag("month"); Text("Agenda").tag("agenda") }
                .pickerStyle(.segmented).accessibilityIdentifier("calendar-view")
        }
    }
    @ViewBuilder private var proposals: some View {
        // Folded by default: Lucina may suggest many, and the calendar comes first.
        if !store.proposals.isEmpty {
            Button { withAnimation { showProposals.toggle() } } label: {
                HStack {
                    Image(systemName: "sparkles").foregroundStyle(Theme.accent)
                    Text("Suggested events").font(.headline)
                    Text(verbatim: String(store.proposals.count)).font(.caption.bold()).padding(.horizontal, 7).padding(.vertical, 2)
                        .background(Theme.accentSoft, in: Capsule())
                    Spacer()
                    Image(systemName: showProposals ? "chevron.up" : "chevron.down").foregroundStyle(Theme.textSecondary)
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("calendar-proposals-toggle")
        }
        if showProposals {
            ForEach(store.proposals) { proposal in
                Card {
                    VStack(alignment: .leading, spacing: 8) {
                        Text(proposal.event.title ?? "").font(.headline)
                        Text(CalendarMath.proposalWhen(proposal)).font(.caption)
                        Text("Proposed by \(proposal.from)").font(.caption).foregroundStyle(.secondary)
                        if let reason = proposal.reason { Text(reason).font(.callout) }
                        if let location = proposal.event.location { Text(location).font(.callout) }
                        let notes = CalendarMath.plainNotes(proposal.event.description)
                        if !notes.isEmpty { Text(notes).font(.callout) }
                        HStack {
                            Button("Add") { Task { await store.proposal(proposal, accept: true) } }
                                .accessibilityIdentifier("calendar-proposal-add")
                            Button("Dismiss") { Task { await store.proposal(proposal, accept: false) } }
                                .accessibilityIdentifier("calendar-proposal-dismiss")
                        }.disabled(store.busy)
                    }
                }
            }
        }
    }
    private var monthGrid: some View {
        Card {
            VStack(spacing: 8) {
                HStack {
                    ForEach(0..<7) { index in
                        let date = days[index]
                        Text(CalendarMath.format(date, template: "EEEEE")).font(.caption).foregroundStyle(.secondary).frame(maxWidth: .infinity)
                    }
                }
                LazyVGrid(columns: Array(repeating: GridItem(.flexible(), spacing: 4), count: 7), spacing: 6) {
                    ForEach(days, id: \.self) { date in
                        Button { selectedDay = date } label: {
                            VStack(spacing: 5) {
                                Text(date, format: .dateTime.day()).font(.callout.weight(Calendar.current.isDateInToday(date) ? .bold : .regular))
                                HStack(spacing: 3) {
                                    ForEach(Array(CalendarMath.events(store.visible, on: date).prefix(3).enumerated()), id: \.offset) { _, event in
                                        Circle().fill(CalendarMath.color(name: event.calendarName, source: event.source)).frame(width: 5, height: 5)
                                    }
                                }.frame(height: 5)
                            }
                            .frame(maxWidth: .infinity, minHeight: 42)
                            .background(Calendar.current.isDate(date, inSameDayAs: selectedDay) ? Theme.accent.opacity(0.25) : Color.clear, in: RoundedRectangle(cornerRadius: 8))
                            .overlay { if Calendar.current.isDateInToday(date) { RoundedRectangle(cornerRadius: 8).stroke(Theme.accent, lineWidth: 1) } }
                            .opacity(Calendar.current.isDate(date, equalTo: selectedDay, toGranularity: .month) ? 1 : 0.4)
                        }.buttonStyle(.plain).accessibilityLabel(date.formatted(date: .complete, time: .omitted))
                            .accessibilityIdentifier("calendar-day-" + CalendarMath.ymd(date))
                    }
                }
            }
        }.gesture(DragGesture(minimumDistance: 40).onEnded { value in
            if abs(value.translation.width) > abs(value.translation.height) { changeMonth(value.translation.width < 0 ? 1 : -1) }
        })
    }
    private var empty: some View { ContentUnavailableView("No events", systemImage: "calendar", description: Text("Your calendar is clear.")) }
    @ViewBuilder private func eventList(_ events: [CalendarEntry]) -> some View {
        if events.isEmpty && !store.loading { empty }
        ForEach(events) { row($0) }
    }
    private func row(_ event: CalendarEntry) -> some View {
        Button { sheet = .detail(event) } label: {
            HStack(alignment: .top, spacing: 12) {
                RoundedRectangle(cornerRadius: 2).fill(CalendarMath.color(name: event.calendarName, source: event.source)).frame(width: 4)
                VStack(alignment: .leading, spacing: 4) {
                    Text(event.title).font(.headline).foregroundStyle(.primary)
                    Text(event.allDay ? String(localized: "All day") : CalendarMath.format(event.startDate, template: "HHmm") + "–" + CalendarMath.format(event.endDate, template: "HHmm"))
                        .font(.subheadline).foregroundStyle(.secondary)
                    if let location = event.location, !location.isEmpty { Text(location).font(.caption).foregroundStyle(.secondary) }
                }
                Spacer(minLength: 0)
            }.padding(.vertical, 6).frame(minHeight: 55)
        }.buttonStyle(.plain).accessibilityIdentifier("calendar-event-" + event.id)
        .contextMenu {
            Button("Open", systemImage: "arrow.up.right.square") { sheet = .detail(event) }
            Button("Copy", systemImage: "doc.on.doc") {
                UIPasteboard.general.string = [event.title, CalendarMath.whenLabel(event), event.location].compactMap { $0 }.joined(separator: "\n")
                confirmCopy()
            }
            Button("Copy link", systemImage: "link") { UIPasteboard.general.url = event.publicLink; confirmCopy() }
            ShareLink(item: event.publicLink)
            if event.editable {
                Button("Edit", systemImage: "pencil") { sheet = .edit(event) }
                Button("Delete", systemImage: "trash", role: .destructive) { deleting = event }
            }
        }
    }
    private func confirmCopy() {
        copied = true
        Task { try? await Task.sleep(for: .seconds(2)); copied = false }
    }
    private func changeMonth(_ value: Int) {
        selectedDay = Calendar.current.date(byAdding: .month, value: value, to: selectedDay)!
        if view == "agenda" { agendaStart = selectedDay; agendaEnd = Calendar.current.date(byAdding: .day, value: 30, to: selectedDay)! }
    }
    private func loadMoreIfNeeded() {
        guard view == "agenda", !store.loading, let day = lastVisibleAgendaDay,
              day >= Calendar.current.date(byAdding: .day, value: -30, to: agendaEnd)! else { return }
        lastVisibleAgendaDay = nil
        extendAgenda()
    }
    private func extendAgenda() { agendaEnd = Calendar.current.date(byAdding: .day, value: 30, to: agendaEnd)! }
    private func load(refresh: Bool = false) async { await store.load(from: range.start, to: range.end, refresh: refresh) }
    private func consumeLink() async {
        guard let link = app.openCalendarEvent else { return }
        defer { if app.openCalendarEvent == link { app.openCalendarEvent = nil } }
        notice = nil
        // A new link replaces whatever sheet is up (an older event's detail).
        sheet = nil
        let date = link.at ?? link.eventId.flatMap { id in store.events.first(where: { $0.id == id }).map { CalendarMath.bounds($0).0 } } ?? Date()
        selectedDay = date; agendaStart = Calendar.current.startOfDay(for: date)
        agendaEnd = Calendar.current.date(byAdding: .day, value: 30, to: agendaStart)!
        await load(refresh: true)
        guard app.openCalendarEvent == link, !Task.isCancelled, let id = link.eventId else { return }
        var event = CalendarMath.matchDeepLink(store.events, id: id, at: link.at)
        // Links without an occurrence date (for example chat replies) need a wider search.
        if event == nil && link.at == nil {
            let from = Calendar.current.date(byAdding: .day, value: -180, to: date)!
            let to = Calendar.current.date(byAdding: .day, value: 180, to: date)!
            do {
                let page = try await CalendarAPI(client: app.client).events(from: from, to: to)
                event = CalendarMath.matchDeepLink(page.events, id: id, at: nil)
            } catch {
                let failure = APIError.wrap(error)
                if failure.endsSession { app.handle(failure) }
                notice = failure.userMessage
                return
            }
        }
        guard app.openCalendarEvent == link, !Task.isCancelled else { return }
        if let event {
            selectedDay = CalendarMath.bounds(event).0
            agendaStart = Calendar.current.startOfDay(for: selectedDay); agendaEnd = Calendar.current.date(byAdding: .day, value: 30, to: agendaStart)!
            await load()
            sheet = .detail(event)
        } else if store.error == nil { notice = String(localized: "This event is no longer in the calendar.") }
    }
}
