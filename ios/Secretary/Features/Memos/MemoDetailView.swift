import SwiftUI

/// One memo: player, Lucina's summary (key points, decisions, action items → Tasks),
/// the timed transcript (tap a line to jump there), the PDF and a new summary on request.
struct MemoDetailView: View {
    @Environment(MemosStore.self) private var store
    @Environment(AppModel.self) private var app
    let memoId: String

    @State private var detail: MemoDetail?
    @State private var transcript: MemoTranscript?
    @State private var error: String?
    @State private var renaming = false
    @State private var newTitle = ""
    @State private var askingResummary = false
    @State private var instructions = ""
    @State private var pdfURL: URL?
    @State private var addedTasks: Set<String> = []
    @State private var choosingEvent = false
    /// Whether this server links memos to calendar events (older lucina-memo builds don't).
    @State private var eventsSupported = MemoDetailView.serverHasEvents ?? false
    @MainActor private static var serverHasEvents: Bool?

    private var row: MemoRow? { store.rows.first { $0.serverId == memoId } }

    var body: some View {
        List {
            if let error {
                Section { ErrorBanner(message: error) { self.error = nil }.listRowInsets(EdgeInsets()).listRowBackground(Color.clear) }
            }
            if let d = detail {
                Section {
                    VStack(alignment: .leading, spacing: 6) {
                        Text(d.memo.title).font(.title2.bold())
                        HStack(spacing: 6) {
                            Text(d.memo.recordedAt, format: .dateTime.weekday(.wide).day().month().hour().minute())
                            Text("·")
                            Text(MemoTime.clock(d.memo.durationSec))
                            if let lang = d.memo.language { Text("·"); Text(lang.uppercased()) }
                        }
                        .font(.footnote).foregroundStyle(Theme.textSecondary)
                        if d.memo.status != .done {
                            HStack(spacing: 6) {
                                if d.memo.status.isProcessing { ProgressView().controlSize(.small) }
                                Text(d.memo.status.label)
                                if let e = d.memo.error, d.memo.status == .failed || d.memo.status.isProcessing { Text(e).lineLimit(2) }
                            }
                            .font(.footnote).foregroundStyle(d.memo.status == .failed ? Theme.warning : Theme.accent)
                        }
                    }
                    if let row { InlinePlayer(row: row) }
                }
                if d.memo.event != nil || (eventsSupported && (d.memo.status == .done || d.memo.status == .failed)) {
                    Section("Event") {
                        if let e = d.memo.event {
                            MemoEventRow(event: e)
                        } else {
                            Text(d.memo.eventSource == "owner" ? "Not linked to an event" : "No calendar event at the time of the recording")
                                .font(.footnote).foregroundStyle(Theme.textSecondary)
                        }
                        Button { choosingEvent = true } label: {
                            Label(d.memo.event == nil ? "Link an event" : "Change event", systemImage: "calendar")
                        }
                        .accessibilityIdentifier("memoChangeEvent")
                    }
                }
                if let s = d.summary {
                    Section(s.template == "lecture" ? "Lecture notes" : s.template == "meeting" ? "Minutes" : "Summary") {
                        Text(s.overview).font(.body)
                        if !s.tags.isEmpty {
                            Text(s.tags.map { "#\($0)" }.joined(separator: "  ")).font(.footnote).foregroundStyle(Theme.accent)
                        }
                    }
                    if !s.keyPoints.isEmpty {
                        Section(s.template == "lecture" ? "Key concepts" : "Key points") { ForEach(s.keyPoints, id: \.self) { ItemRow(item: $0, seek: seek) } }
                    }
                    if let defs = s.definitions, !defs.isEmpty {
                        Section("Definitions") { ForEach(defs, id: \.self) { ItemRow(item: $0, seek: seek) } }
                    }
                    if let qs = s.questions, !qs.isEmpty {
                        Section("Questions to review") { ForEach(qs, id: \.self) { ItemRow(item: $0, seek: seek) } }
                    }
                    if !s.decisions.isEmpty {
                        Section("Decisions") { ForEach(s.decisions, id: \.self) { ItemRow(item: $0, seek: seek) } }
                    }
                    if !s.actionItems.isEmpty {
                        Section {
                            ForEach(s.actionItems, id: \.self) { item in
                                HStack(alignment: .top) {
                                    ItemRow(item: item, seek: seek)
                                    Spacer()
                                    Button {
                                        Task { await addTask(item) }
                                    } label: {
                                        Image(systemName: addedTasks.contains(item.text) ? "checkmark.circle.fill" : "plus.circle")
                                    }
                                    .buttonStyle(.borderless)
                                    .disabled(addedTasks.contains(item.text))
                                    .accessibilityLabel("Add to Tasks")
                                }
                            }
                        } header: {
                            Text("Action items")
                        } footer: {
                            Text("+ adds the item to your Tasks (Todoist).")
                        }
                    }
                }
                if let t = transcript, !t.segments.isEmpty {
                    Section {
                        ForEach(t.segments) { seg in
                            Button { seek(seg.start) } label: {
                                HStack(alignment: .firstTextBaseline, spacing: 10) {
                                    Text(MemoTime.clock(seg.start)).font(.caption.monospacedDigit()).foregroundStyle(Theme.textSecondary)
                                        .frame(width: 44, alignment: .leading)
                                    VStack(alignment: .leading) {
                                        if let sp = seg.speaker { Text(sp).font(.caption.bold()) }
                                        Text(seg.text).foregroundStyle(current(seg) ? Theme.accent : Theme.text)
                                    }
                                }
                            }
                            .buttonStyle(.plain)
                        }
                    } header: {
                        Text("Transcript")
                    } footer: {
                        Text("Transcribed on Lucina's server with \(t.engine). Names and technical words may be wrong.")
                    }
                }
            } else if error == nil {
                Section { ProgressView().frame(maxWidth: .infinity) }
            }
        }
        .lucinaList()
        .navigationTitle(detail?.memo.title ?? String(localized: "Memo"))
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Menu {
                    if let pdfURL {
                        ShareLink(item: pdfURL) { Label("Share PDF", systemImage: "square.and.arrow.up") }
                    } else if detail?.pdf == true {
                        Button { Task { pdfURL = await store.pdf(memoId, title: detail?.memo.title ?? "Memo") } } label: {
                            Label("Get the PDF", systemImage: "doc.richtext")
                        }
                    }
                    Button { newTitle = detail?.memo.title ?? ""; renaming = true } label: { Label("Rename", systemImage: "pencil") }
                    Button { instructions = ""; askingResummary = true } label: { Label("New summary", systemImage: "sparkles") }
                        .disabled(detail?.memo.status == .uploading)
                } label: {
                    Image(systemName: "ellipsis.circle")
                }
                .accessibilityIdentifier("memoMenu")
            }
        }
        .alert("Rename memo", isPresented: $renaming) {
            TextField("Title", text: $newTitle)
            Button("Save") { Task { await store.rename(memoId, to: newTitle); await load() } }
            Button("Cancel", role: .cancel) {}
        }
        .alert("New summary", isPresented: $askingResummary) {
            TextField("What should Lucina focus on? (optional)", text: $instructions)
            Button("Summarise again") {
                Task {
                    await store.resummarize(memoId, instructions: instructions.trimmingCharacters(in: .whitespaces))
                    await load()
                }
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Lucina writes the summary, PDF and Obsidian note again and tells you when it's done.")
        }
        .sheet(isPresented: $choosingEvent) {
            MemoEventPicker(memoId: memoId, current: detail?.memo.event) { await load() }
        }
        .task(id: memoId) {
            await load()
            // Follow the pipeline until it is done.
            while !Task.isCancelled, let d = detail, d.memo.status != .done && d.memo.status != .failed {
                try? await Task.sleep(for: .seconds(4))
                await load()
            }
        }
    }

    private func load() async {
        do {
            let d = try await store.detail(memoId)
            detail = d
            if Self.serverHasEvents == nil {
                // One probe per launch: a 404 means this lucina-memo has no calendar link yet.
                do { _ = try await store.events(memoId); Self.serverHasEvents = true } catch let e as APIError {
                    Self.serverHasEvents = e.status != 404
                } catch {}
                eventsSupported = Self.serverHasEvents ?? false
            }
            if d.summary != nil || [.done, .rendering, .publishing, .summarizing].contains(d.memo.status) {
                if let t = try? await store.transcript(memoId) { transcript = t }
            }
            // Until the server has one: the iPhone's own transcript.
            if transcript == nil, let c = row?.clientId, let local = store.localTranscripts[c] {
                transcript = MemoTranscript(language: local.language, engine: local.engine, durationSec: local.durationSec,
                                            segments: local.segments)
            }
            error = nil
        } catch let e as APIError {
            error = e.userMessage
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func seek(_ t: Double) {
        guard let row else { return }
        Task {
            if store.player.memoId != row.id { await store.play(row) }
            store.player.seek(t)
            if !store.player.playing { store.player.play() }
        }
    }

    private func current(_ seg: MemoSegment) -> Bool {
        guard let row, store.player.memoId == row.id, store.player.playing else { return false }
        return store.player.position >= seg.start && store.player.position < seg.end
    }

    private func addTask(_ item: MemoItem) async {
        let due = item.due.flatMap { d in d.count == 10 && d.dropFirst(4).first == "-" ? d : nil }
        let ok = await app.tasks.run(.quickAdd(text: item.text, defaultDue: due))
        if ok { addedTasks.insert(item.text) } else { error = app.tasks.actionError }
    }
}

/// A linked calendar event: title, time, place, people.
private struct MemoEventRow: View {
    let event: MemoEvent

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(spacing: 6) {
                Text(event.title).font(.headline)
                if let k = event.kindLabel {
                    Text(k).font(.caption.weight(.semibold)).padding(.horizontal, 6).padding(.vertical, 2)
                        .background(Capsule().fill(Theme.accent.opacity(0.15))).foregroundStyle(Theme.accent)
                }
            }
            if let s = event.start, let e = event.end {
                Text("\(s.formatted(.dateTime.weekday(.abbreviated).day().month().hour().minute()))–\(e.formatted(date: .omitted, time: .shortened))")
                    .font(.footnote).foregroundStyle(Theme.textSecondary)
            }
            if let place = event.location, !place.isEmpty {
                Label(place, systemImage: "mappin.and.ellipse").font(.footnote).foregroundStyle(Theme.textSecondary)
            }
            if let people = event.attendees, !people.isEmpty {
                Label(people.prefix(6).joined(separator: ", ") + (people.count > 6 ? " +\(people.count - 6)" : ""), systemImage: "person.2")
                    .font(.footnote).foregroundStyle(Theme.textSecondary).lineLimit(2)
            }
        }
    }
}

/// Pick the event a memo belongs to (the server's best match first), or none.
private struct MemoEventPicker: View {
    @Environment(MemosStore.self) private var store
    @Environment(\.dismiss) private var dismiss
    let memoId: String
    let current: MemoEvent?
    let changed: () async -> Void

    @State private var choices: MemoEventChoices?
    @State private var error: String?
    @State private var saving = false

    var body: some View {
        NavigationStack {
            List {
                if let error { Section { ErrorBanner(message: error) { self.error = nil } } }
                if let c = choices {
                    Section {
                        ForEach(c.candidates) { cand in
                            Button { Task { await choose(cand.event.id) } } label: {
                                HStack {
                                    MemoEventRow(event: cand.event)
                                    Spacer()
                                    if cand.locMatch { Image(systemName: "location.fill").foregroundStyle(Theme.accent).accessibilityLabel("Here") }
                                    if cand.event.id == current?.id { Image(systemName: "checkmark").foregroundStyle(Theme.accent) }
                                }
                            }
                            .buttonStyle(.plain)
                        }
                        if c.candidates.isEmpty {
                            Text("No calendar events around the time of this recording.").foregroundStyle(Theme.textSecondary)
                        }
                    } footer: {
                        Text("Lucina writes the summary again for the event you pick: lecture notes for a lecture, minutes for a meeting.")
                    }
                    Section {
                        Button("No event", role: .destructive) { Task { await choose(nil) } }
                    }
                } else if error == nil {
                    ProgressView().frame(maxWidth: .infinity)
                }
            }
            .lucinaList()
            .disabled(saving)
            .navigationTitle("Event")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } } }
            .task {
                do { choices = try await store.events(memoId) } catch let e as APIError { error = e.userMessage } catch { self.error = error.localizedDescription }
            }
        }
    }

    private func choose(_ id: String?) async {
        saving = true
        defer { saving = false }
        do {
            try await store.setEvent(memoId, eventId: id)
            await changed()
            dismiss()
        } catch let e as APIError {
            error = e.userMessage
        } catch {
            self.error = error.localizedDescription
        }
    }
}

private struct ItemRow: View {
    let item: MemoItem
    let seek: (Double) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(item.text)
            HStack(spacing: 8) {
                if let owner = item.owner { Label(owner == "me" ? String(localized: "You") : owner, systemImage: "person") }
                if let due = item.due { Label(due, systemImage: "calendar") }
                if let at = item.at {
                    Button { seek(at) } label: { Label(MemoTime.clock(at), systemImage: "play.circle") }.buttonStyle(.borderless)
                }
            }
            .font(.caption)
            .foregroundStyle(Theme.textSecondary)
        }
    }
}
