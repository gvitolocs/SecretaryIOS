import SwiftUI

/// The Notes tab: Keep notes linked to the notebook. They stay in Keep; tapping one opens it there.
struct NotebookNotesView: View {
    let store: NotebookStore
    @State private var writing = false
    @State private var linking = false

    var body: some View {
        List {
            if let error = store.error {
                Section { ErrorBanner(message: error) { store.error = nil } }
                    .listRowInsets(EdgeInsets())
                    .listRowBackground(Color.clear)
            }
            if !store.loaded {
                Section { ProgressView().frame(maxWidth: .infinity) }
            } else if store.notes.isEmpty {
                Section {
                    EmptyCard(icon: "note.text", title: "No notes yet",
                              detail: "Write a note, or link one you already have in Keep. Chats can read the linked notes and cite them.")
                }
                .listRowInsets(EdgeInsets())
                .listRowBackground(Color.clear)
            } else {
                Section {
                    ForEach(store.notes) { note in
                        // A destination link: the notebook is itself pushed, where value links do not resolve.
                        NavigationLink {
                            NoteDetailView(key: note.slug)
                        } label: {
                            NoteLinkRow(note: note)
                        }
                        .swipeActions(edge: .trailing) {
                            Button("Unlink", systemImage: "link.badge.minus", role: .destructive) {
                                Task { await store.unlinkNote(note) }
                            }
                        }
                    }
                } footer: {
                    Text("Swipe to unlink a note from this notebook. The note itself stays in Keep.")
                }
            }
        }
        .listStyle(.insetGrouped)
        .scrollContentBackground(.hidden)
        .refreshable { await store.load() }
        .safeAreaInset(edge: .bottom) {
            HStack(spacing: 10) {
                Button { writing = true } label: {
                    Label("Write note", systemImage: "square.and.pencil")
                        .font(.headline).frame(maxWidth: .infinity).padding(.vertical, 12)
                }
                .buttonStyle(.borderedProminent)
                .buttonBorderShape(.roundedRectangle(radius: 14))
                .tint(Theme.accentFill)
                .accessibilityIdentifier("writeNotebookNote")
                Button { linking = true } label: {
                    Label("Link from Keep", systemImage: "link")
                        .font(.headline).frame(maxWidth: .infinity).padding(.vertical, 12)
                }
                .buttonStyle(.bordered)
                .buttonBorderShape(.roundedRectangle(radius: 14))
            }
            .padding(.horizontal, 16)
            .padding(.bottom, 8)
        }
        .sheet(isPresented: $writing) { WriteNoteSheet(store: store) }
        .sheet(isPresented: $linking) { LinkNoteSheet(store: store) }
    }
}

private struct NoteLinkRow: View {
    let note: NotebookNote

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(note.title).font(.headline).lineLimit(2)
            let excerpt = NoteText.plainExcerpt(note.excerpt, title: note.title)
            if !excerpt.isEmpty {
                Text(excerpt).font(.subheadline).foregroundStyle(.secondary).lineLimit(2)
            }
            HStack(spacing: 8) {
                if let d = note.updated {
                    Text(d.formatted(date: .abbreviated, time: .omitted))
                }
                if !note.indexed {
                    Label("Not read yet", systemImage: "clock").labelStyle(.titleAndIcon)
                }
            }
            .font(.caption).foregroundStyle(.secondary)
        }
        .padding(.vertical, 2)
    }
}

private struct WriteNoteSheet: View {
    @Environment(\.dismiss) private var dismiss
    let store: NotebookStore
    @State private var title = ""
    @State private var text = ""
    @State private var busy = false

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("Title (optional)", text: $title)
                    TextField("Write a note", text: $text, axis: .vertical).lineLimit(8...20)
                        .accessibilityIdentifier("notebookNoteText")
                } footer: {
                    Text("It is saved in Keep with the label “notebook”, and linked here.")
                }
                if let error = store.error {
                    Section { ErrorBanner(message: error) { store.error = nil }.listRowInsets(EdgeInsets()).listRowBackground(Color.clear) }
                }
            }
            .navigationTitle("Write note")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    if busy { ProgressView() } else {
                        Button("Save") {
                            busy = true
                            Task {
                                if await store.writeNote(title: title.trimmingCharacters(in: .whitespacesAndNewlines), text: text) { dismiss() }
                                busy = false
                            }
                        }
                        .disabled(text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                    }
                }
            }
            .onAppear { store.error = nil }
        }
        .presentationDetents([.large])
    }
}

/// Pick a Keep note to link.
private struct LinkNoteSheet: View {
    @Environment(\.dismiss) private var dismiss
    @Environment(NotesStore.self) private var notes
    let store: NotebookStore
    @State private var query = ""
    @State private var busySlug: String?

    private var candidates: [NoteSummary] {
        let linked = Set(store.notes.map(\.slug))
        let q = query.trimmingCharacters(in: .whitespaces).lowercased()
        return notes.notes.filter { !linked.contains($0.slug) && (q.isEmpty || $0.title.lowercased().contains(q) || $0.slug.lowercased().contains(q)) }
    }

    var body: some View {
        NavigationStack {
            List {
                if let error = store.error {
                    Section { ErrorBanner(message: error) { store.error = nil } }
                        .listRowInsets(EdgeInsets())
                        .listRowBackground(Color.clear)
                }
                if !notes.loaded {
                    ProgressView().frame(maxWidth: .infinity)
                } else if candidates.isEmpty {
                    Text(query.isEmpty ? "All your notes are already linked." : "Nothing matches “\(query)”.").foregroundStyle(.secondary)
                }
                ForEach(candidates) { note in
                    Button {
                        busySlug = note.slug
                        Task {
                            _ = await store.linkNote(slug: note.slug)
                            busySlug = nil
                        }
                    } label: {
                        HStack {
                            VStack(alignment: .leading, spacing: 2) {
                                Text(note.title).font(.headline).foregroundStyle(.primary).lineLimit(2)
                                let excerpt = note.plainExcerpt
                                if !excerpt.isEmpty { Text(excerpt).font(.subheadline).foregroundStyle(.secondary).lineLimit(2) }
                            }
                            Spacer()
                            if busySlug == note.slug { ProgressView() } else { Image(systemName: "plus.circle").foregroundStyle(Theme.accent) }
                        }
                    }
                    .disabled(busySlug != nil)
                }
            }
            .searchable(text: $query, prompt: Text("Search notes"))
            .navigationTitle("Link from Keep")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } } }
            .task {
                store.error = nil
                await notes.refresh()
            }
        }
    }
}
