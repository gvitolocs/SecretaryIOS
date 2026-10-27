import SwiftUI

/// Links: every connection Lucina has (calendars, chat channels, AI clients, Google
/// services, the owner's own links), with the real state from secretary-server.
struct LinksView: View {
    @Environment(AppModel.self) private var app
    @State private var store: LinksStore?
    @State private var addingCalendar = false
    @State private var addingLink = false
    @State private var pastingToken = false
    @State private var confirming: LinkCard?

    var body: some View {
        Group {
            if let store {
                content(store)
            } else {
                ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .navigationTitle("Links")
        .background(Theme.page)
        .task {
            if store == nil { store = LinksStore(client: app.client) }
            await store?.load()
        }
        .onOpenURL { url in
            if let outcome = GoogleReturn(url: url) {
                store?.handle(outcome)
                Task { await store?.load() }
            }
        }
    }

    @ViewBuilder
    private func content(_ store: LinksStore) -> some View {
        List {
            if let notice = store.notice {
                Section {
                    Label(notice, systemImage: "checkmark.circle.fill")
                        .foregroundStyle(.green)
                        .onTapGesture { store.notice = nil }
                }
            }
            if let error = store.error {
                Section {
                    Label(error, systemImage: "exclamationmark.triangle.fill")
                        .foregroundStyle(.orange)
                        .onTapGesture { store.error = nil }
                }
            }
            if let overview = store.overview {
                if !overview.proposals.isEmpty {
                    Section("Waiting for your OK") {
                        ForEach(overview.proposals) { proposal in
                            ProposalRow(proposal: proposal, busy: store.busyCard == proposal.id) { accept in
                                Task { await store.answer(proposal, accept: accept) }
                            }
                        }
                    }
                }
                ForEach(overview.groups) { group in
                    Section {
                        if group.id == "calendars" { UpcomingRows(events: store.events) }
                        ForEach(group.cards) { card in
                            LinkRow(card: card, busy: store.busyCard == card.id) { action in
                                perform(action, on: card, store: store)
                            }
                        }
                        if group.id == "calendars" {
                            Button { addingCalendar = true } label: {
                                Label("Subscribe to a calendar", systemImage: "plus.circle")
                            }
                        }
                        if group.id == "custom" {
                            Button { addingLink = true } label: {
                                Label("Add a link", systemImage: "plus.circle")
                            }
                        }
                    } header: {
                        Text(Self.title(group.id))
                    } footer: {
                        Text(Self.hint(group.id))
                    }
                }
            } else if store.loading {
                Section { ProgressView() }
            }
        }
        .lucinaList()
        .refreshable { await store.load() }
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button {
                    Task { await store.sync() }
                } label: {
                    Label("Sync calendars", systemImage: "arrow.triangle.2.circlepath")
                }
                .disabled(store.busyCard == "sync")
            }
        }
        .confirmationDialog(confirmTitle, isPresented: Binding(get: { confirming != nil }, set: { if !$0 { confirming = nil } }),
                            titleVisibility: .visible, presenting: confirming) { card in
            Button(card.kind == "google" || card.kind == "todoist" ? "Disconnect" : "Remove", role: .destructive) {
                Task {
                    if card.kind == "google" { await store.disconnectGoogle() }
                    else if card.kind == "todoist" { await store.disconnectTodoist() }
                    else { await store.remove(card) }
                }
            }
        }
        .sheet(isPresented: $addingCalendar) {
            AddCalendarSheet { label, url in await store.addSubscription(label: label, url: url) }
        }
        .sheet(isPresented: $pastingToken) {
            TodoistTokenSheet { token in await store.saveTodoistToken(token) }
        }
        .sheet(isPresented: $addingLink) {
            AddLinkSheet { kind, label, url, icon, tags in await store.addLink(kind: kind, label: label, url: url, icon: icon, tags: tags) }
        }
    }

    private var confirmTitle: String {
        guard let card = confirming else { return "" }
        return card.kind == "google" || card.kind == "todoist"
            ? String(localized: "Disconnect \(LinkRow.displayName(card))? Lucina stops reading it.")
            : String(localized: "Remove \(LinkRow.displayName(card))?")
    }

    private func perform(_ action: String, on card: LinkCard, store: LinksStore) {
        if card.kind == "todoist" {
            switch action {
            case "connect", "reconnect": Task { await store.connectTodoist(card) }
            case "token": pastingToken = true
            case "sync": Task { await store.syncTodoist() }
            case "disconnect": confirming = card
            default: break
            }
            return
        }
        switch action {
        case "connect", "reconnect": Task { await store.connectGoogle(card) }
        case "sync": Task { await store.sync() }
        case "disconnect", "remove": confirming = card
        default: break
        }
    }

    static func title(_ group: String) -> LocalizedStringKey {
        switch group {
        case "calendars": "Calendars"
        case "tasks": "Tasks"
        case "channels": "Chat channels"
        case "ai": "AI clients"
        case "google": "Google"
        case "custom": "Your links"
        default: LocalizedStringKey(group)
        }
    }

    static func hint(_ group: String) -> LocalizedStringKey {
        switch group {
        case "calendars": "Lucina reads these to answer “what's on tomorrow?” and to plan your day."
        case "tasks": "Todoist, synced both ways: what you do here shows up in Todoist and the other way round."
        case "channels": "Where Lucina talks with you."
        case "ai": "Assistants that use Lucina through her MCP server."
        case "google": "More Google services with the same sign-in, read-only."
        case "custom": "Bookmarks for quick access, and MCP servers for Lucina to use later."
        default: ""
        }
    }
}

// MARK: - Rows

struct LinkRow: View {
    let card: LinkCard
    let busy: Bool
    let act: (String) -> Void

    static func displayName(_ card: LinkCard) -> String {
        switch card.id {
        case "google-calendar": String(localized: "Google Calendar")
        case "iphone-calendar": String(localized: "iPhone Calendar")
        case "google-gmail": "Gmail"
        case "google-drive": String(localized: "Google Drive")
        default: card.name
        }
    }

    private var symbol: String {
        switch card.id {
        case "google-calendar": "calendar"
        case "iphone-calendar": "iphone"
        case "google-gmail": "envelope"
        case "google-drive": "externaldrive"
        case "channel:telegram": "paperplane"
        case "channel:discord": "gamecontroller"
        case "channel:alexa": "homepod"
        default:
            switch card.kind {
            case "ics": "calendar.badge.clock"
            case "ai": "sparkles"
            case "mcp": "powerplug"
            case "channel": "bubble.left"
            default: "link"
            }
        }
    }

    private var detail: String? {
        switch card.id {
        case "iphone-calendar":
            return card.status == "connected"
                ? String(localized: "\(card.count ?? 0) events from the app's calendar bridge")
                : String(localized: "Allow Calendar in Lucina's access (Settings) to share your events.")
        case "google-calendar", "google-gmail", "google-drive":
            if card.status == "unavailable" { return String(localized: "Google sign-in isn't set up on the server yet.") }
            if card.id == "google-calendar", let n = card.count, card.status != "disconnected" {
                return String(localized: "\(n) items synced")
            }
            return nil
        case "channel:telegram": return String(localized: "Lucina's bot. Reminders arrive here.")
        case "channel:discord": return String(localized: "Lucina on Discord. Presence watcher active.")
        case "channel:alexa": return String(localized: "An Alexa skill is planned.")
        default:
            if card.kind == "ai" {
                return card.status == "unknown" ? String(localized: "The MCP server's status isn't available right now.") : String(localized: "Through the Lucina MCP")
            }
            if card.kind == "mcp" { return String(localized: "Remote MCP server, saved for Lucina. Nothing is called yet.") }
            if card.kind == "ics" {
                let n = String(localized: "\(card.count ?? 0) events")
                return card.detail.map { "\($0) · \(n)" } ?? n
            }
            return card.detail
        }
    }

    private var footer: String? {
        guard let date = card.lastSyncAt else { return nil }
        let relative = date.formatted(.relative(presentation: .named))
        switch card.kind {
        case "channel", "ai": return String(localized: "Active \(relative)")
        case "bookmark", "mcp": return String(localized: "Added \(relative)")
        default: return String(localized: "Synced \(relative)")
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 12) {
                Group {
                    if let icon = card.icon, !icon.isEmpty {
                        Text(icon).font(.title3)
                    } else {
                        Image(systemName: symbol).font(.body.weight(.semibold)).foregroundStyle(Theme.accent)
                    }
                }
                .frame(width: 36, height: 36)
                .background(Theme.field, in: RoundedRectangle(cornerRadius: 10, style: .continuous))
                .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 2) {
                    Text(Self.displayName(card)).font(.headline).lineLimit(1)
                    if let account = card.account, !account.isEmpty {
                        Text(account).font(.caption.monospaced()).foregroundStyle(.secondary).lineLimit(1)
                    }
                }
                Spacer(minLength: 8)
                StatusBadge(status: card.status)
            }
            if let detail { Text(detail).font(.subheadline).foregroundStyle(.secondary) }
            if let error = card.error { Text(error).font(.footnote).foregroundStyle(.red) }
            if !card.scopes.isEmpty || !card.tags.isEmpty {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 6) {
                        ForEach(card.scopes, id: \.self) { Chip(text: $0, mono: true) }
                        ForEach(card.tags, id: \.self) { Chip(text: "#\($0)", mono: false) }
                    }
                }
            }
            if footer != nil || !card.actions.isEmpty || card.url != nil {
                HStack {
                    if let footer { Text(footer).font(.caption2).foregroundStyle(.tertiary) }
                    Spacer()
                    if busy { ProgressView().controlSize(.small) }
                    if let url = card.url {
                        Link("Open", destination: url).buttonStyle(.bordered).controlSize(.small)
                    }
                    ForEach(card.actions, id: \.self) { action in
                        actionButton(action)
                    }
                }
            }
        }
        .padding(.vertical, 4)
        .opacity(card.status == "planned" || card.status == "unavailable" ? 0.7 : 1)
        .accessibilityElement(children: .contain)
    }

    @ViewBuilder
    private func actionButton(_ action: String) -> some View {
        let label: LocalizedStringKey = switch action {
        case "connect": "Connect"
        case "reconnect": "Reconnect"
        case "disconnect": "Disconnect"
        case "sync": "Sync"
        case "remove": "Remove"
        case "token": "Paste API token"
        default: LocalizedStringKey(action)
        }
        if action == "connect" || action == "reconnect" {
            Button(label) { act(action) }.buttonStyle(.borderedProminent).controlSize(.small).disabled(busy)
        } else {
            Button(label, role: action == "disconnect" || action == "remove" ? .destructive : nil) { act(action) }
                .buttonStyle(.bordered).controlSize(.small).disabled(busy)
        }
    }
}

struct StatusBadge: View {
    let status: String

    private var color: Color {
        switch status {
        case "connected", "saved": .green
        case "reconnect", "error": .orange
        case "available": .accentColor
        case "planned": .purple
        default: .secondary
        }
    }

    private var label: LocalizedStringKey {
        switch status {
        case "connected": "Connected"
        case "disconnected": "Not connected"
        case "reconnect": "Reconnect needed"
        case "error": "Error"
        case "planned": "Planned"
        case "available": "Available"
        case "unavailable": "Not set up"
        case "saved": "Saved"
        default: "Unknown"
        }
    }

    var body: some View {
        HStack(spacing: 5) {
            Circle().fill(color).frame(width: 7, height: 7)
            Text(label).font(.caption2.weight(.semibold))
        }
        .foregroundStyle(color)
        .padding(.horizontal, 8)
        .padding(.vertical, 3)
        .background(color.opacity(0.12), in: Capsule())
    }
}

private struct Chip: View {
    let text: String
    let mono: Bool

    var body: some View {
        Text(text)
            .font(mono ? .caption2.monospaced() : .caption2)
            .padding(.horizontal, 7)
            .padding(.vertical, 2)
            .background(Theme.field, in: Capsule())
            .foregroundStyle(mono ? Theme.accent : .secondary)
    }
}

private struct UpcomingRows: View {
    let events: [CalendarEvent]?

    var body: some View {
        if let events {
            VStack(alignment: .leading, spacing: 6) {
                Text("Next 7 days").font(.subheadline.weight(.semibold)).foregroundStyle(.secondary)
                if events.isEmpty {
                    Text("Nothing on your calendars.").font(.subheadline).foregroundStyle(.secondary)
                }
                ForEach(events.prefix(6)) { event in
                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                        RoundedRectangle(cornerRadius: 1)
                            .fill(event.source == "google" ? Color.accentColor : event.source == "ics" ? .purple : .teal)
                            .frame(width: 3, height: 16)
                        Text(event.startDate.formatted(.dateTime.weekday(.abbreviated).day()))
                            .font(.caption).foregroundStyle(.secondary).frame(width: 56, alignment: .leading)
                        Text(event.allDay ? String(localized: "All day") : event.startDate.formatted(date: .omitted, time: .shortened))
                            .font(.caption.monospaced()).foregroundStyle(Theme.accent).frame(width: 64, alignment: .leading)
                        Text(event.title).font(.subheadline).lineLimit(1)
                        Spacer(minLength: 0)
                    }
                    .accessibilityElement(children: .combine)
                }
                if events.count > 6 {
                    Text("+\(events.count - 6) more").font(.caption).foregroundStyle(.secondary)
                }
            }
            .padding(.vertical, 4)
        }
    }
}

private struct ProposalRow: View {
    let proposal: CalendarProposal
    let busy: Bool
    let answer: (Bool) -> Void

    private var when: String {
        if let date = LinkDates.parse(proposal.event.start) ?? Self.offsetDate(proposal.event.start) {
            return date.formatted(date: .abbreviated, time: .shortened)
        }
        return proposal.event.start
    }

    static func offsetDate(_ s: String) -> Date? {
        let f = DateFormatter()
        f.locale = Locale(identifier: "en_US_POSIX")
        f.dateFormat = "yyyy-MM-dd'T'HH:mm:ssXXXXX"
        return f.date(from: s)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(proposal.event.title).font(.headline)
            Text([when, proposal.event.location].compactMap { $0 }.joined(separator: " · "))
                .font(.subheadline).foregroundStyle(.secondary)
            Text("Proposed by \(proposal.from)").font(.caption).foregroundStyle(.secondary)
            HStack {
                Button("Add to calendar") { answer(true) }.buttonStyle(.borderedProminent)
                Button("Dismiss") { answer(false) }.buttonStyle(.bordered)
                if busy { ProgressView().controlSize(.small) }
            }
            .controlSize(.small)
            .disabled(busy)
        }
        .padding(.vertical, 4)
    }
}

// MARK: - Sheets

private struct TodoistTokenSheet: View {
    @Environment(\.dismiss) private var dismiss
    let save: (String) async -> Bool
    @State private var token = ""
    @State private var saving = false

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    SecureField("API token", text: $token)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .accessibilityIdentifier("todoistToken")
                } footer: {
                    Text("In Todoist: Settings → Integrations → Developer → copy the API token. It is stored encrypted on the server and never shown again.")
                }
            }
            .navigationTitle("Todoist")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Connect") {
                        saving = true
                        Task {
                            if await save(token.trimmingCharacters(in: .whitespacesAndNewlines)) { dismiss() }
                            saving = false
                        }
                    }
                    .disabled(saving || token.trimmingCharacters(in: .whitespaces).count < 20)
                }
            }
        }
        .presentationDetents([.medium])
    }
}

private struct AddCalendarSheet: View {
    @Environment(\.dismiss) private var dismiss
    let add: (String, String) async -> Bool
    @State private var label = ""
    @State private var url = ""
    @State private var saving = false

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("Name", text: $label)
                    TextField("iCal address", text: $url)
                        .keyboardType(.URL).textInputAutocapitalization(.never).autocorrectionDisabled()
                } footer: {
                    Text("Paste a calendar's secret iCal address: Google's “Secret address in iCal format”, Outlook's ICS link or an iCloud public calendar. Read-only, stored encrypted.")
                }
            }
            .navigationTitle("Subscribe to a calendar")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    if saving {
                        ProgressView()
                    } else {
                        Button("Subscribe") {
                            saving = true
                            Task {
                                let ok = await add(label.trimmingCharacters(in: .whitespaces), url.trimmingCharacters(in: .whitespaces))
                                saving = false
                                if ok { dismiss() }
                            }
                        }
                        .disabled(url.trimmingCharacters(in: .whitespaces).isEmpty)
                    }
                }
            }
        }
    }
}

private struct AddLinkSheet: View {
    @Environment(\.dismiss) private var dismiss
    let add: (String, String, String, String, [String]) async -> Bool
    @State private var kind = "bookmark"
    @State private var label = ""
    @State private var url = ""
    @State private var icon = ""
    @State private var tags = ""
    @State private var saving = false

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    Picker("Kind", selection: $kind) {
                        Text("Bookmark").tag("bookmark")
                        Text("MCP server").tag("mcp")
                    }
                    .pickerStyle(.segmented)
                } footer: {
                    if kind == "mcp" { Text("Lucina stores the server address encrypted and can use it later. Nothing is called now.") }
                }
                Section {
                    TextField("Label", text: $label)
                    TextField("Address", text: $url)
                        .keyboardType(.URL).textInputAutocapitalization(.never).autocorrectionDisabled()
                    TextField("Icon (emoji)", text: $icon)
                    TextField("Tags, comma separated", text: $tags).textInputAutocapitalization(.never)
                }
            }
            .navigationTitle("Add a link")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    if saving {
                        ProgressView()
                    } else {
                        Button("Add") {
                            saving = true
                            let tagList = tags.split(separator: ",").map { $0.trimmingCharacters(in: .whitespaces) }.filter { !$0.isEmpty }
                            Task {
                                let ok = await add(kind, label.trimmingCharacters(in: .whitespaces), url.trimmingCharacters(in: .whitespaces),
                                                   icon.trimmingCharacters(in: .whitespaces), tagList)
                                saving = false
                                if ok { dismiss() }
                            }
                        }
                        .disabled(label.trimmingCharacters(in: .whitespaces).isEmpty || url.trimmingCharacters(in: .whitespaces).isEmpty)
                    }
                }
            }
        }
    }
}
