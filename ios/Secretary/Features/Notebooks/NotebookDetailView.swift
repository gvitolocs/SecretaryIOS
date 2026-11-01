import SwiftUI

/// One notebook: Sources | Notes | Chat, with the podcasts and the notebook's info behind the toolbar.
struct NotebookDetailView: View {
    @State private var store: NotebookStore
    @State private var tab: Tab = .sources
    @State private var showInfo = false
    @State private var showPodcasts = false
    let onChange: (Notebook) -> Void

    enum Tab: String, CaseIterable, Identifiable {
        case sources, notes, chat
        var id: String { rawValue }
        var title: LocalizedStringKey {
            switch self {
            case .sources: "Sources"
            case .notes: "Notes"
            case .chat: "Chat"
            }
        }
    }

    init(notebook: Notebook, client: APIClient, onChange: @escaping (Notebook) -> Void = { _ in }) {
        _store = State(initialValue: NotebookStore(notebook: notebook, api: { NotebooksAPI(client: client) }))
        self.onChange = onChange
    }

    var body: some View {
        VStack(spacing: 0) {
            Picker("View", selection: $tab) {
                ForEach(Tab.allCases) { Text($0.title).tag($0) }
            }
            .pickerStyle(.segmented)
            .padding(.horizontal, 16)
            .padding(.vertical, 8)
            .accessibilityIdentifier("notebookTabs")

            switch tab {
            case .sources: NotebookSourcesView(store: store)
            case .notes: NotebookNotesView(store: store)
            case .chat: NotebookChatView(store: store)
            }
        }
        .background(Theme.page)
        .navigationTitle(store.notebook.name)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button { showPodcasts = true } label: { Image(systemName: "waveform.circle") }
                    .accessibilityLabel("Podcasts")
                    .accessibilityIdentifier("notebookPodcastsButton")
            }
            ToolbarItem(placement: .topBarTrailing) {
                Button { showInfo = true } label: { Image(systemName: "info.circle") }
                    .accessibilityLabel("About this notebook")
                    .accessibilityIdentifier("notebookInfoButton")
            }
        }
        .sheet(isPresented: $showInfo) { NotebookInfoSheet(store: store) }
        .sheet(isPresented: $showPodcasts) { PodcastsSheet(store: store) }
        .task {
            await store.load()
            onChange(store.notebook)
        }
        // Sources being read and podcasts being made: look again every few seconds while any are.
        .task(id: store.hasPendingWork) {
            guard store.hasPendingWork else { return }
            while !Task.isCancelled, store.hasPendingWork {
                try? await Task.sleep(for: .seconds(3))
                guard !Task.isCancelled else { return }
                await store.load()
            }
            onChange(store.notebook)
        }
        .onChange(of: store.notebook) { _, notebook in onChange(notebook) }
    }
}

/// Name, description, the numbers, and where the idea comes from.
private struct NotebookInfoSheet: View {
    @Environment(\.dismiss) private var dismiss
    let store: NotebookStore
    @State private var name = ""
    @State private var description = ""
    @State private var saving = false

    var body: some View {
        NavigationStack {
            Form {
                Section("Notebook") {
                    TextField("Name", text: $name)
                    TextField("Description", text: $description, axis: .vertical).lineLimit(2...6)
                }
                Section("Contents") {
                    LabeledContent("Sources", value: "\(store.notebook.counts.sources)")
                    LabeledContent("Notes", value: "\(store.notebook.counts.notes)")
                    LabeledContent("Chats", value: "\(store.notebook.counts.chats)")
                    LabeledContent("Podcasts", value: "\(store.notebook.counts.podcasts)")
                    if let d = NoteDates.parse(store.notebook.createdAt) {
                        LabeledContent("Created", value: d.formatted(date: .abbreviated, time: .omitted))
                    }
                }
                if let error = store.error {
                    Section { ErrorBanner(message: error) { store.error = nil }.listRowInsets(EdgeInsets()).listRowBackground(Color.clear) }
                }
                Section {
                    Text("Chats answer only from the sources and notes in this notebook, and cite them. Your Keep notes are linked, never copied.")
                        .font(.footnote).foregroundStyle(.secondary)
                } footer: {
                    NotebookCredit().padding(.top, 4)
                }
            }
            .navigationTitle("About")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Close") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Save") {
                        saving = true
                        Task {
                            if await store.update(name: name.trimmingCharacters(in: .whitespacesAndNewlines),
                                                  description: description.trimmingCharacters(in: .whitespacesAndNewlines)) { dismiss() }
                            saving = false
                        }
                    }
                    .disabled(saving || name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                              || (name == store.notebook.name && description == store.notebook.description))
                }
            }
            .onAppear {
                name = store.notebook.name
                description = store.notebook.description
                store.error = nil
            }
        }
        .presentationDetents([.medium, .large])
    }
}

/// "Based on Open Notebook by Luis Novo (MIT)", with a link to the project.
struct NotebookCredit: View {
    var body: some View {
        Text("Based on [Open Notebook](https://github.com/lfnovo/open-notebook) by Luis Novo (MIT)")
            .font(.caption)
            .foregroundStyle(.secondary)
            .accessibilityIdentifier("notebookCredit")
    }
}
