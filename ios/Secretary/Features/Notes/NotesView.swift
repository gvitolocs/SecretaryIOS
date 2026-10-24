import SwiftUI

// The Notes tab: briefings and documents from the owner, Lucina, ChatGPT, Grok and Claude Code
// (docs/server-api.md, "Notes"). List with search, tag chips and pinned notes; a note rendered
// as markdown with its source; an editor with a preview; the trash.

struct NotesView: View {
    @Environment(NotesStore.self) private var store
    @State private var creating = false
    @State private var showTrash = false

    var body: some View {
        @Bindable var store = store
        List {
            if let trashed = store.trashed {
                Section {
                    HStack(spacing: 10) {
                        Image(systemName: "trash").foregroundStyle(.secondary).accessibilityHidden(true)
                        Text("“\(trashed.title)” is in the trash.").font(.subheadline)
                            .frame(maxWidth: .infinity, alignment: .leading)
                        Button("Undo") { Task { await store.undoRemove() } }.bold()
                        Button { store.trashed = nil } label: { Image(systemName: "xmark").font(.footnote.weight(.semibold)) }
                            .buttonStyle(.plain).foregroundStyle(.secondary)
                            .accessibilityLabel("Dismiss")
                    }
                }
            }
            if let error = store.loadError {
                Section {
                    ErrorBanner(message: error) { store.loadError = nil }
                        .listRowInsets(EdgeInsets())
                        .listRowBackground(Color.clear)
                }
            }
            if !store.allTags.isEmpty {
                Section {
                    tagChips
                        .listRowInsets(EdgeInsets(top: 4, leading: 0, bottom: 4, trailing: 0))
                        .listRowBackground(Color.clear)
                }
            }
            if !store.loaded && store.loadError == nil {
                Section { ProgressView().frame(maxWidth: .infinity) }
            } else if store.notes.isEmpty {
                Section {
                    if store.query.isEmpty && store.tag.isEmpty {
                        EmptyCard(icon: "doc.text",
                                  title: "No notes yet",
                                  detail: "Briefings and documents saved by Lucina, ChatGPT, Grok or Claude Code show up here. You can write one too.")
                            .listRowInsets(EdgeInsets())
                            .listRowBackground(Color.clear)
                    } else {
                        Text("Nothing matches “\(store.query.isEmpty ? "#" + store.tag : store.query)”.")
                            .foregroundStyle(.secondary)
                    }
                }
            } else {
                if !store.pinned.isEmpty {
                    Section("Pinned") { ForEach(store.pinned) { row($0) } }
                }
                if !store.others.isEmpty {
                    Section(store.pinned.isEmpty ? "" : "Notes") { ForEach(store.others) { row($0) } }
                }
            }
        }
        .lucinaList()
        .searchable(text: $store.query, prompt: Text("Search notes"))
        .refreshable { await store.refresh() }
        .navigationTitle("Notes")
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button { showTrash = true } label: { Image(systemName: "trash") }
                    .accessibilityLabel("Trash")
            }
            ToolbarItem(placement: .topBarTrailing) {
                Button { creating = true } label: { Image(systemName: "square.and.pencil") }
                    .accessibilityLabel("New note")
                    .accessibilityIdentifier("newNoteButton")
            }
        }
        .sheet(isPresented: $creating) { NoteEditorView(note: nil) }
        .sheet(isPresented: $showTrash) { NotesTrashView() }
        // Search as you type, a little after the last keystroke.
        .task(id: "\(store.query)\u{1F}\(store.tag)") {
            if store.loaded { try? await Task.sleep(for: .milliseconds(250)) }
            guard !Task.isCancelled else { return }
            await store.refresh()
        }
    }

    private var tagChips: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 8) {
                chip(label: String(localized: "All"), selected: store.tag.isEmpty) { store.tag = "" }
                ForEach(store.allTags, id: \.self) { tag in
                    chip(label: "#\(tag)", selected: store.tag == tag) { store.tag = store.tag == tag ? "" : tag }
                }
            }
            .padding(.horizontal, 16)
        }
        .accessibilityLabel("Filter by tag")
    }

    private func chip(label: String, selected: Bool, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Text(verbatim: label)
                .font(.subheadline.weight(.semibold))
                .padding(.horizontal, 14)
                .padding(.vertical, 7)
                .foregroundStyle(selected ? Color.white : Color.primary)
                .background(selected ? AnyShapeStyle(Theme.accentFill) : AnyShapeStyle(Theme.card), in: Capsule())
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(selected ? .isSelected : [])
    }

    private func row(_ note: NoteSummary) -> some View {
        // A destination link: this list is itself pushed (from the chat), where value links do not resolve.
        NavigationLink {
            NoteDetailView(key: note.slug)
        } label: {
            VStack(alignment: .leading, spacing: 6) {
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    if note.pinned {
                        Image(systemName: "pin.fill").font(.caption).foregroundStyle(Theme.accent).accessibilityLabel("Pinned")
                    }
                    Text(note.title).font(.headline).lineLimit(2)
                }
                let excerpt = note.plainExcerpt
                if !excerpt.isEmpty {
                    Text(excerpt).font(.subheadline).foregroundStyle(.secondary).lineLimit(2)
                }
                HStack(spacing: 8) {
                    NoteSourceBadge(source: note.source).fixedSize()
                    if let d = note.updated {
                        Text(d.formatted(date: .abbreviated, time: .omitted)).font(.caption).foregroundStyle(.secondary)
                            .fixedSize()
                    }
                    Text(verbatim: note.tags.prefix(3).map { "#\($0)" }.joined(separator: " "))
                        .font(.caption.monospaced()).foregroundStyle(.purple)
                        .lineLimit(1)
                        .layoutPriority(-1)
                }
            }
            .padding(.vertical, 4)
        }
        .accessibilityIdentifier("note-\(note.slug)")
        .swipeActions(edge: .leading) {
            Button(note.pinned ? "Unpin" : "Pin", systemImage: note.pinned ? "pin.slash" : "pin") {
                Task { _ = try? await store.setPinned(note.id, !note.pinned) }
            }
            .tint(Theme.accentFill)
        }
        .swipeActions(edge: .trailing, allowsFullSwipe: false) {
            Button("Delete", systemImage: "trash", role: .destructive) {
                Task { await store.remove(id: note.id, title: note.title) }
            }
        }
    }
}

/// Who wrote the note.
struct NoteSourceBadge: View {
    let source: String

    private var color: Color {
        switch source {
        case "owner", "web", "ios": .cyan
        case "lucina": .pink
        case "chatgpt": .green
        case "grok": .gray
        case "claude-code", "claude": .orange
        default: .secondary
        }
    }

    var body: some View {
        Text(verbatim: NoteSource.label(source).uppercased())
            .font(.caption2.weight(.semibold).monospaced())
            .tracking(1)
            .padding(.horizontal, 7)
            .padding(.vertical, 2)
            .foregroundStyle(color)
            .background(color.opacity(0.12), in: Capsule())
            .overlay(Capsule().strokeBorder(color.opacity(0.35)))
            .accessibilityLabel(Text("Saved by \(NoteSource.label(source))"))
    }
}

// MARK: - One note

struct NoteDetailView: View {
    @Environment(NotesStore.self) private var store
    @Environment(\.dismiss) private var dismiss
    let key: String
    @State private var note: Note?
    @State private var error: String?
    @State private var missing = false
    @State private var editing = false
    @State private var confirmDelete = false
    @State private var busy = false

    var body: some View {
        Group {
            if let note {
                ScrollView {
                    VStack(alignment: .leading, spacing: 14) {
                        Text(note.title).font(.title.bold()).fixedSize(horizontal: false, vertical: true)
                            .accessibilityAddTraits(.isHeader)
                        HStack(spacing: 8) {
                            NoteSourceBadge(source: note.source)
                            if let d = note.updated {
                                Text("Updated \(d.formatted(date: .abbreviated, time: .shortened))")
                                    .font(.caption).foregroundStyle(.secondary)
                            }
                        }
                        Text("Name: \(note.slug)").font(.caption.monospaced()).foregroundStyle(.secondary)
                        if !note.tags.isEmpty {
                            Text(verbatim: note.tags.map { "#\($0)" }.joined(separator: "  "))
                                .font(.caption.monospaced()).foregroundStyle(.purple)
                        }
                        if let error { ErrorBanner(message: error) { self.error = nil } }
                        Divider()
                        NoteMarkdownView(blocks: NoteText.dropTitleHeading(NoteText.parse(note.content), title: note.title))
                        Text("Lucina can recall it: ask her “what's in the note \(note.slug)”.")
                            .font(.footnote).foregroundStyle(.secondary).padding(.top, 8)
                    }
                    .padding(20)
                    .frame(maxWidth: 760, alignment: .leading)
                    .frame(maxWidth: .infinity)
                }
                .background(Theme.page)
                .toolbar {
                    ToolbarItem(placement: .topBarTrailing) {
                        Button { Task { await togglePin() } } label: {
                            Image(systemName: note.pinned ? "pin.fill" : "pin")
                        }
                        .accessibilityLabel(note.pinned ? "Unpin" : "Pin")
                        .disabled(busy)
                    }
                    ToolbarItem(placement: .topBarTrailing) {
                        Menu {
                            Button("Edit", systemImage: "pencil") { editing = true }
                            Button("Delete", systemImage: "trash", role: .destructive) { confirmDelete = true }
                        } label: { Image(systemName: "ellipsis.circle") }
                        .accessibilityLabel("More")
                    }
                }
                .sheet(isPresented: $editing) {
                    NoteEditorView(note: note) { saved in self.note = saved }
                }
                .confirmationDialog("Move “\(note.title)” to the trash? You can restore it for 30 days.",
                                    isPresented: $confirmDelete, titleVisibility: .visible) {
                    Button("Delete", role: .destructive) {
                        Task {
                            if await store.remove(id: note.id, title: note.title) { dismiss() }
                        }
                    }
                }
            } else if missing {
                ContentUnavailableView("This note doesn't exist anymore.", systemImage: "doc.questionmark")
            } else if let error {
                ContentUnavailableView {
                    Label("Couldn't load the note", systemImage: "doc.text")
                } description: { Text(error) } actions: {
                    Button("Try again") { Task { await load() } }
                }
            } else {
                ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .navigationBarTitleDisplayMode(.inline)
        .task(id: key) { await load() }
        .onChange(of: store.version) { _, _ in if !editing { Task { await load() } } }
    }

    private func load() async {
        do {
            note = try await store.note(key)
            missing = false
            error = nil
        } catch let e as APIError where e.status == 404 {
            missing = true
        } catch let e as APIError {
            error = e.userMessage
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func togglePin() async {
        guard let n = note else { return }
        busy = true
        defer { busy = false }
        do {
            note = try await store.setPinned(n.id, !n.pinned)
        } catch let e as APIError {
            error = e.userMessage
        } catch {
            self.error = error.localizedDescription
        }
    }
}

// MARK: - Editor

struct NoteEditorView: View {
    @Environment(NotesStore.self) private var store
    @Environment(\.dismiss) private var dismiss
    let note: Note?
    var onSaved: ((Note) -> Void)?

    @State private var title = ""
    @State private var tags = ""
    @State private var content = ""
    @State private var preview = false
    @State private var error: String?
    @State private var saving = false
    @State private var started = false

    private static let maxBytes = 256 * 1024

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("Title", text: $title).font(.headline)
                        .accessibilityIdentifier("noteTitleField")
                    TextField("Tags, separated by commas", text: $tags)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                }
                Section {
                    Picker("Text (Markdown)", selection: $preview) {
                        Text("Write").tag(false)
                        Text("Preview").tag(true)
                    }
                    .pickerStyle(.segmented)
                    if preview {
                        if content.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                            Text("Nothing to preview yet.").foregroundStyle(.secondary)
                        } else {
                            NoteMarkdownView(blocks: NoteText.parse(content)).padding(.vertical, 6)
                        }
                    } else {
                        TextEditor(text: $content)
                            .font(.system(.body, design: .monospaced))
                            .frame(minHeight: 280)
                            .accessibilityIdentifier("noteContentField")
                    }
                } header: {
                    Text("Text (Markdown)")
                } footer: {
                    Text("\(content.count) characters")
                        .foregroundStyle(content.utf8.count > Self.maxBytes ? Color.red : Color.secondary)
                }
                if let error {
                    Section { Text(error).foregroundStyle(.red) }
                }
            }
            .navigationTitle(note == nil ? "New note" : "Edit")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    Button { Task { await save() } } label: {
                        if saving { ProgressView() } else { Text("Save").bold() }
                    }
                    .disabled(saving)
                    .accessibilityIdentifier("saveNoteButton")
                }
            }
            .onAppear {
                guard !started else { return }
                started = true
                title = note?.title ?? ""
                tags = (note?.tags ?? []).joined(separator: ", ")
                content = note?.content ?? ""
            }
        }
    }

    private func save() async {
        let t = title.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !t.isEmpty else { error = String(localized: "Give the note a title."); return }
        guard content.utf8.count <= Self.maxBytes else { error = String(localized: "The note is too long (256 KB at most)."); return }
        saving = true
        defer { saving = false }
        do {
            let saved: Note
            if let note {
                saved = try await store.update(note, title: t, content: content, tags: parseNoteTags(tags))
            } else {
                saved = try await store.create(NoteDraft(title: t, content: content, tags: parseNoteTags(tags)))
            }
            onSaved?(saved)
            dismiss()
        } catch let e as APIError where e.status == 409 && e.code == "conflict" {
            error = String(localized: "This note changed while you were editing (Lucina may have added to it). Your text is kept here: copy it, close, and edit again.")
        } catch let e as APIError {
            error = e.code == "note_too_large" ? String(localized: "The note is too long (256 KB at most).") : e.userMessage
        } catch {
            self.error = error.localizedDescription
        }
    }
}

// MARK: - Trash

struct NotesTrashView: View {
    @Environment(NotesStore.self) private var store
    @Environment(\.dismiss) private var dismiss
    @State private var items: [NoteSummary]?
    @State private var error: String?

    var body: some View {
        NavigationStack {
            List {
                Section {
                    if let items {
                        if items.isEmpty {
                            Text("The trash is empty.").foregroundStyle(.secondary)
                        }
                        ForEach(items) { n in
                            HStack {
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(n.title).font(.headline)
                                    if let d = n.purgeDate {
                                        Text("Deleted for good on \(d.formatted(date: .abbreviated, time: .omitted))")
                                            .font(.caption).foregroundStyle(.secondary)
                                    }
                                }
                                Spacer()
                                Button("Restore") { Task { await restore(n) } }.buttonStyle(.bordered)
                            }
                        }
                    } else if let error {
                        Text(error).foregroundStyle(.red)
                    } else {
                        ProgressView().frame(maxWidth: .infinity)
                    }
                } footer: {
                    Text("Deleted notes stay here for 30 days.")
                }
            }
            .navigationTitle("Trash")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } } }
            .task { await load() }
        }
    }

    private func load() async {
        do {
            items = try await store.trash()
        } catch let e as APIError {
            error = e.userMessage
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func restore(_ n: NoteSummary) async {
        do {
            _ = try await store.restore(n.id)
            items?.removeAll { $0.id == n.id }
        } catch let e as APIError {
            error = e.userMessage
        } catch {
            self.error = error.localizedDescription
        }
    }
}
