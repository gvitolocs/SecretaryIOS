import SwiftUI

// Notebooks (docs/design/notebooks-api.md): sources, linked Keep notes and source-grounded chats.
// The list: pull to refresh, a New notebook sheet, swipe to archive or delete. Based on Open Notebook
// by Luis Novo (MIT); the work is done by Lucina's notebooks service, the app only shows it.

struct NotebooksView: View {
    @Environment(AppModel.self) private var app
    @State private var store: NotebooksStore?
    @State private var creating = false
    @State private var pendingDelete: Notebook?

    var body: some View {
        Group {
            if let store {
                content(store)
            } else {
                ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .background(Theme.page)
        .navigationTitle("Notebooks")
        .onAppear {
            if store == nil {
                let client = app.client
                store = NotebooksStore(api: { NotebooksAPI(client: client) })
            }
        }
    }

    @ViewBuilder
    private func content(_ store: NotebooksStore) -> some View {
        @Bindable var store = store
        List {
            if let error = store.error, store.loaded {
                Section {
                    if store.unavailable && store.notebooks.isEmpty {
                        EmptyCard(icon: "books.vertical", title: "Notebooks aren't available yet",
                                  detail: "The server doesn't have Notebooks yet. Pull down to try again once it has been updated.")
                    } else {
                        ErrorBanner(message: error) { store.error = nil }
                    }
                }
                .listRowInsets(EdgeInsets())
                .listRowBackground(Color.clear)
            }
            if !store.loaded {
                Section { ProgressView().frame(maxWidth: .infinity) }
            } else if store.notebooks.isEmpty && !store.unavailable {
                Section {
                    EmptyCard(icon: "books.vertical",
                              title: store.showArchived ? "No archived notebooks" : "No notebooks yet",
                              detail: store.showArchived
                                  ? "Notebooks you archive end up here."
                                  : "A notebook gathers PDFs, links, videos and notes. Ask it questions and get answers that cite the pages.")
                        .listRowInsets(EdgeInsets())
                        .listRowBackground(Color.clear)
                }
            } else {
                Section {
                    ForEach(store.notebooks) { notebook in
                        NavigationLink {
                            NotebookDetailView(notebook: notebook, client: app.client) { store.replace($0) }
                        } label: {
                            NotebookRow(notebook: notebook)
                        }
                        .swipeActions(edge: .trailing, allowsFullSwipe: false) {
                            Button("Delete", systemImage: "trash", role: .destructive) { pendingDelete = notebook }
                            Button(notebook.archived ? "Restore" : "Archive", systemImage: notebook.archived ? "tray.and.arrow.up" : "archivebox") {
                                Task { await store.setArchived(notebook, !notebook.archived) }
                            }
                            .tint(.orange)
                        }
                    }
                }
            }
        }
        .listStyle(.insetGrouped)
        .refreshable { await store.refresh() }
        .task { await store.refresh() }
        .onChange(of: store.showArchived) { _, _ in Task { await store.refresh() } }
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Menu {
                    Toggle(isOn: $store.showArchived) { Label("Show archived", systemImage: "archivebox") }
                } label: { Image(systemName: "ellipsis.circle") }
                .accessibilityLabel("More")
            }
            ToolbarItem(placement: .topBarTrailing) {
                Button { creating = true } label: { Image(systemName: "plus") }
                    .accessibilityLabel("New notebook")
                    .accessibilityIdentifier("newNotebookButton")
            }
        }
        .sheet(isPresented: $creating) {
            NewNotebookSheet(store: store)
        }
        .confirmationDialog(pendingDelete.map { "Delete “\($0.name)”?" } ?? "", isPresented: Binding(get: { pendingDelete != nil }, set: { if !$0 { pendingDelete = nil } }),
                            titleVisibility: .visible, presenting: pendingDelete) { notebook in
            Button("Delete notebook, keep its sources", role: .destructive) {
                Task { await store.delete(notebook, deleteSources: false) }
            }
            if notebook.counts.sources > 0 {
                Button("Delete notebook and its sources", role: .destructive) {
                    Task { await store.delete(notebook, deleteSources: true) }
                }
            }
            Button("Cancel", role: .cancel) {}
        } message: { _ in
            Text("Your Keep notes stay in Keep.")
        }
    }
}

private struct NotebookRow: View {
    let notebook: Notebook

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                if notebook.archived {
                    Image(systemName: "archivebox.fill").font(.caption).foregroundStyle(.secondary).accessibilityLabel("Archived")
                }
                Text(notebook.name).font(.headline).lineLimit(2)
            }
            if !notebook.description.isEmpty {
                Text(notebook.description).font(.subheadline).foregroundStyle(.secondary).lineLimit(2)
            }
            HStack(spacing: 14) {
                count("doc.on.doc", notebook.counts.sources, "sources")
                count("note.text", notebook.counts.notes, "notes")
                count("bubble.left.and.bubble.right", notebook.counts.chats, "chats")
                if notebook.counts.podcasts > 0 { count("waveform", notebook.counts.podcasts, "podcasts") }
                Spacer(minLength: 0)
                if let d = notebook.updated {
                    Text(d.formatted(date: .abbreviated, time: .omitted)).font(.caption).foregroundStyle(.secondary)
                }
            }
            .font(.caption)
            .foregroundStyle(.secondary)
        }
        .padding(.vertical, 4)
        .accessibilityElement(children: .combine)
    }

    private func count(_ icon: String, _ n: Int, _ label: String) -> some View {
        HStack(spacing: 4) {
            Image(systemName: icon).accessibilityHidden(true)
            Text(verbatim: "\(n)")
        }
        .accessibilityLabel("\(n) \(label)")
    }
}

private struct NewNotebookSheet: View {
    @Environment(\.dismiss) private var dismiss
    let store: NotebooksStore
    @State private var name = ""
    @State private var description = ""
    @State private var busy = false
    @FocusState private var focused: Bool

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("Name", text: $name)
                        .focused($focused)
                        .submitLabel(.done)
                        .accessibilityIdentifier("notebookNameField")
                    TextField("Description (optional)", text: $description, axis: .vertical)
                        .lineLimit(2...5)
                } footer: {
                    Text("Add PDFs, links, videos or text next. Chats only answer from what is in the notebook.")
                }
                if let error = store.error {
                    Section { ErrorBanner(message: error) { store.error = nil }.listRowInsets(EdgeInsets()).listRowBackground(Color.clear) }
                }
            }
            .navigationTitle("New notebook")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Create") {
                        busy = true
                        Task {
                            if await store.create(name: name, description: description) != nil { dismiss() }
                            busy = false
                        }
                    }
                    .disabled(busy || name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }
            }
            .onAppear {
                store.error = nil
                focused = true
            }
        }
        .presentationDetents([.medium, .large])
    }
}
