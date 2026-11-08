import SwiftUI
import UniformTypeIdentifiers

/// The Sources tab: what the notebook knows, with the status of each source being read.
struct NotebookSourcesView: View {
    let store: NotebookStore
    @State private var adding = false
    @State private var pendingRemove: NotebookSource?

    var body: some View {
        List {
            if let error = store.error {
                Section { ErrorBanner(message: error) { store.error = nil } }
                    .listRowInsets(EdgeInsets())
                    .listRowBackground(Color.clear)
            }
            if !store.loaded {
                Section { ProgressView().frame(maxWidth: .infinity) }
            } else if store.sources.isEmpty {
                Section {
                    EmptyCard(icon: "doc.badge.plus", title: "No sources yet",
                              detail: "Add a PDF, a web page, a YouTube link, some text, or a file from Lucina's computer. Chats answer only from sources.")
                    PrimaryButton(title: "Add source", icon: "plus") { adding = true }
                }
                .listRowInsets(EdgeInsets())
                .listRowBackground(Color.clear)
            } else {
                Section {
                    ForEach(store.sources) { source in
                        NavigationLink {
                            SourceDetailView(source: source, store: store)
                        } label: {
                            SourceRow(source: source)
                        }
                        .swipeActions(edge: .trailing) {
                            Button("Remove", systemImage: "trash", role: .destructive) { pendingRemove = source }
                        }
                        .swipeActions(edge: .leading) {
                            if source.isFailed {
                                Button("Retry", systemImage: "arrow.clockwise") { Task { await store.retry(source) } }.tint(Theme.accentFill)
                            }
                        }
                        .contextMenu {
                            if source.isReady {
                                Section("Chat sees") {
                                    ForEach(SourceContext.allCases) { context in
                                        Button {
                                            Task { await store.setContext(source, context) }
                                        } label: {
                                            if source.sourceContext == context { Label(context.label, systemImage: "checkmark") } else { Text(context.label) }
                                        }
                                    }
                                }
                            }
                            if source.isFailed {
                                Button("Retry", systemImage: "arrow.clockwise") { Task { await store.retry(source) } }
                            }
                            Button("Remove from notebook", systemImage: "trash", role: .destructive) { pendingRemove = source }
                        }
                    }
                } footer: {
                    VStack(alignment: .leading, spacing: 6) {
                        Text("Press and hold a source to choose what the chat sees: full text, a summary, or nothing.")
                        NotebookCredit()
                    }
                    .padding(.top, 6)
                }
            }
        }
        .listStyle(.insetGrouped)
        .scrollContentBackground(.hidden)
        .refreshable { await store.load() }
        .safeAreaInset(edge: .bottom) {
            if !store.sources.isEmpty {
                Button { adding = true } label: {
                    Label("Add source", systemImage: "plus")
                        .font(.headline)
                        .frame(maxWidth: .infinity)
                        .padding(.vertical, 12)
                }
                .buttonStyle(.borderedProminent)
                .buttonBorderShape(.roundedRectangle(radius: 14))
                .tint(Theme.accentFill)
                .padding(.horizontal, 16)
                .padding(.bottom, 8)
                .accessibilityIdentifier("addSourceButton")
            }
        }
        .sheet(isPresented: $adding) { AddSourceSheet(store: store) }
        .confirmationDialog(pendingRemove.map { "Remove “\($0.title)”?" } ?? "", isPresented: Binding(get: { pendingRemove != nil }, set: { if !$0 { pendingRemove = nil } }),
                            titleVisibility: .visible, presenting: pendingRemove) { source in
            Button("Remove source", role: .destructive) { Task { await store.removeSource(source) } }
            Button("Cancel", role: .cancel) {}
        } message: { _ in
            Text("It is deleted for good if no other notebook uses it.")
        }
    }
}

private struct SourceRow: View {
    let source: NotebookSource

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            Image(systemName: source.kindIcon)
                .font(.title3)
                .foregroundStyle(Theme.accent)
                .frame(width: 30)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 4) {
                Text(source.title).font(.headline).lineLimit(2)
                statusLine
                if source.isFailed, let error = source.error, !error.isEmpty {
                    Text(error).font(.caption).foregroundStyle(.red).lineLimit(3)
                }
            }
        }
        .padding(.vertical, 2)
        .accessibilityElement(children: .combine)
    }

    @ViewBuilder
    private var statusLine: some View {
        HStack(spacing: 6) {
            if source.isPending {
                ProgressView().controlSize(.mini)
                Text(source.statusLabel).foregroundStyle(.secondary)
            } else if source.isFailed {
                Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.red).font(.caption)
                Text(source.statusLabel).foregroundStyle(.red)
                Text("Swipe right to retry").foregroundStyle(.secondary)
            } else {
                Text(source.summaryLine).foregroundStyle(.secondary)
                if source.sourceContext != .full {
                    Label(source.sourceContext.label, systemImage: source.sourceContext.systemImage)
                        .labelStyle(.titleAndIcon)
                        .padding(.horizontal, 6).padding(.vertical, 1)
                        .background(Theme.field, in: Capsule())
                }
            }
        }
        .font(.caption)
    }
}

// MARK: - Add source

private struct AddSourceSheet: View {
    @Environment(\.dismiss) private var dismiss
    let store: NotebookStore
    @State private var kind: Kind = .link
    @State private var link = ""
    @State private var title = ""
    @State private var text = ""
    @State private var path = "/home/cua/"
    @State private var importing = false
    @State private var busy = false
    @State private var localError: String?

    enum Kind: String, CaseIterable, Identifiable {
        case link, text, file, computer
        var id: String { rawValue }
        var label: LocalizedStringKey {
            switch self {
            case .link: "Link"
            case .text: "Text"
            case .file: "File"
            case .computer: "Lucina's computer"
            }
        }
        var icon: String {
            switch self {
            case .link: "link"
            case .text: "text.alignleft"
            case .file: "folder"
            case .computer: "desktopcomputer"
            }
        }
    }

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    Picker("Kind", selection: $kind) {
                        ForEach(Kind.allCases) { Label($0.label, systemImage: $0.icon).tag($0) }
                    }
                    .pickerStyle(.menu)
                }
                switch kind {
                case .link:
                    Section {
                        TextField("https://…", text: $link)
                            .keyboardType(.URL)
                            .textContentType(.URL)
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                            .accessibilityIdentifier("sourceLinkField")
                    } header: { Text("Web page or YouTube link") } footer: {
                        Text("Articles are read as text. YouTube videos are transcribed, so the answers can cite the minute.")
                    }
                case .text:
                    Section("Text") {
                        TextField("Title (optional)", text: $title)
                        TextField("Paste or write text", text: $text, axis: .vertical).lineLimit(6...16)
                    }
                case .file:
                    Section {
                        Button { importing = true } label: { Label("Choose a file…", systemImage: "folder") }
                            .disabled(busy)
                    } footer: {
                        Text("PDFs, documents, audio and video from Files, up to 200 MB.")
                    }
                case .computer:
                    Section {
                        TextField("/home/cua/Documents/file.pdf", text: $path)
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                            .font(.body.monospaced())
                    } header: { Text("Path on Lucina's computer") } footer: {
                        Text("A file under /home/cua/ on her computer.")
                    }
                }
                if let message = localError ?? store.error {
                    Section { ErrorBanner(message: message) { localError = nil; store.error = nil }.listRowInsets(EdgeInsets()).listRowBackground(Color.clear) }
                }
            }
            .navigationTitle("Add source")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                if kind != .file {
                    ToolbarItem(placement: .confirmationAction) {
                        if busy { ProgressView() } else { Button("Add") { Task { await add() } }.disabled(!canAdd) }
                    }
                } else if busy {
                    ToolbarItem(placement: .confirmationAction) { ProgressView() }
                }
            }
            .fileImporter(isPresented: $importing, allowedContentTypes: [.item]) { result in
                Task { await upload(result) }
            }
            .onAppear { store.error = nil }
        }
        .presentationDetents([.medium, .large])
    }

    private var canAdd: Bool {
        switch kind {
        case .link: URL(string: link.trimmingCharacters(in: .whitespacesAndNewlines))?.host != nil
        case .text: !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        case .computer: path.hasPrefix("/home/cua/") && !path.hasSuffix("/")
        case .file: false
        }
    }

    private func add() async {
        busy = true
        localError = nil
        defer { busy = false }
        let source: NotebooksAPI.NewSource
        switch kind {
        case .link: source = .url(link.trimmingCharacters(in: .whitespacesAndNewlines))
        case .text: source = .text(title: title.trimmingCharacters(in: .whitespacesAndNewlines), text: text)
        case .computer: source = .sandbox(path: path.trimmingCharacters(in: .whitespacesAndNewlines))
        case .file: return
        }
        if await store.addSource(source) { dismiss() }
    }

    private func upload(_ result: Result<URL, Error>) async {
        localError = nil
        guard case let .success(url) = result else {
            if case let .failure(error) = result { localError = error.localizedDescription }
            return
        }
        busy = true
        defer { busy = false }
        let scoped = url.startAccessingSecurityScopedResource()
        defer { if scoped { url.stopAccessingSecurityScopedResource() } }
        do {
            let size = (try url.resourceValues(forKeys: [.fileSizeKey]).fileSize) ?? 0
            guard size <= NotebooksAPI.maxUploadBytes else {
                localError = String(localized: "That file is larger than 200 MB.")
                return
            }
            let data = try Data(contentsOf: url, options: .mappedIfSafe)
            if await store.upload(data: data, filename: url.lastPathComponent) { dismiss() }
        } catch {
            localError = error.localizedDescription
        }
    }
}

// MARK: - Source detail

/// A source: its details, the insights made from it ("Run transformation"), and its passages.
struct SourceDetailView: View {
    @State private var model: SourceStore
    let notebook: NotebookStore

    init(source: NotebookSource, store: NotebookStore) {
        notebook = store
        let api = store.api
        _model = State(initialValue: SourceStore(source: source, api: api))
    }

    var body: some View {
        List {
            if let error = model.error {
                Section { ErrorBanner(message: error) { model.error = nil } }
                    .listRowInsets(EdgeInsets())
                    .listRowBackground(Color.clear)
            }
            Section {
                VStack(alignment: .leading, spacing: 6) {
                    Label(model.source.title, systemImage: model.source.kindIcon).font(.headline)
                    if !model.source.origin.isEmpty { originView }
                    HStack(spacing: 8) {
                        Text(model.source.statusLabel)
                        if !model.source.summaryLine.isEmpty { Text("· \(model.source.summaryLine)") }
                    }
                    .font(.caption).foregroundStyle(.secondary)
                    if model.source.isFailed, let error = model.source.error, !error.isEmpty {
                        Text(error).font(.caption).foregroundStyle(.red)
                    }
                }
                if model.source.isReady {
                    Picker("Chat sees", selection: Binding(get: { notebook.sources.first(where: { $0.id == model.source.id })?.sourceContext ?? model.source.sourceContext },
                                                         set: { context in Task { await notebook.setContext(model.source, context) } })) {
                        ForEach(SourceContext.allCases) { Text($0.label).tag($0) }
                    }
                }
                if model.source.isFailed {
                    Button("Try again", systemImage: "arrow.clockwise") {
                        Task { await notebook.retry(model.source); await model.load() }
                    }
                }
            }

            if model.source.isReady {
                Section {
                    ForEach(model.insights) { insight in InsightRow(insight: insight, model: model, notebook: notebook) }
                    Menu {
                        ForEach(notebook.transformations) { transformation in
                            Button {
                                Task { await model.run(transformation) }
                            } label: {
                                Text(transformation.name)
                                if !transformation.description.isEmpty { Text(transformation.description) }
                            }
                        }
                    } label: {
                        HStack {
                            Label("Run transformation", systemImage: "wand.and.stars")
                            Spacer()
                            if !model.running.isEmpty { ProgressView() }
                        }
                    }
                    .disabled(notebook.transformations.isEmpty)
                    .accessibilityIdentifier("runTransformation")
                } header: { Text("Insights") } footer: {
                    if notebook.transformations.isEmpty { Text("No transformations are available yet.") }
                }

                Section("Passages") {
                    if model.chunks.isEmpty && model.loaded {
                        Text("No passages.").foregroundStyle(.secondary)
                    }
                    ForEach(model.chunks) { chunk in ChunkRow(chunk: chunk) }
                    if model.moreChunks {
                        Button("Show more") { Task { await model.loadMoreChunks() } }
                    }
                }
            } else if model.source.isPending {
                Section { HStack { ProgressView(); Text("Lucina is reading this source…").foregroundStyle(.secondary) } }
            }
        }
        .listStyle(.insetGrouped)
        .navigationTitle("Source")
        .navigationBarTitleDisplayMode(.inline)
        .task {
            await model.load()
            await notebook.loadTransformations()
        }
        .task(id: model.source.isPending || model.hasPendingInsight) {
            guard model.source.isPending || model.hasPendingInsight else { return }
            while !Task.isCancelled, model.source.isPending || model.hasPendingInsight {
                try? await Task.sleep(for: .seconds(3))
                guard !Task.isCancelled else { return }
                if model.source.isPending { await model.load() } else { await model.refreshInsights() }
            }
        }
        .refreshable { await model.load() }
    }

    @ViewBuilder
    private var originView: some View {
        let origin = model.source.origin
        if let url = URL(string: origin), NoteText.isSafe(url) {
            Link(destination: url) {
                Text(origin).font(.caption).lineLimit(2).multilineTextAlignment(.leading)
            }
        } else {
            Text(origin.replacingOccurrences(of: "upload:", with: "")).font(.caption).foregroundStyle(.secondary).lineLimit(2)
        }
    }
}

private struct ChunkRow: View {
    let chunk: SourceChunk

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            if !chunk.position.isEmpty {
                Text(chunk.position).font(.caption.weight(.semibold)).foregroundStyle(Theme.accent)
            }
            Text(chunk.text).font(.subheadline).textSelection(.enabled)
        }
        .padding(.vertical, 2)
    }
}

private struct InsightRow: View {
    let insight: SourceInsight
    let model: SourceStore
    let notebook: NotebookStore
    @State private var expanded = false
    @State private var saved = false

    var body: some View {
        DisclosureGroup(isExpanded: $expanded) {
            switch insight.status {
            case "failed":
                Text("This transformation failed. Run it again.").foregroundStyle(.red).font(.subheadline)
            case "ready":
                NoteMarkdownView(blocks: NoteText.parse(insight.text))
                    .font(.subheadline)
                HStack {
                    Button(saved ? "Saved to notes" : "Save to note", systemImage: saved ? "checkmark" : "square.and.arrow.down") {
                        Task {
                            if let note = await model.saveAsNote(insight) {
                                saved = true
                                notebook.noteAdded(note)
                            }
                        }
                    }
                    .disabled(saved)
                    Spacer()
                    Button("Delete", systemImage: "trash", role: .destructive) { Task { await model.delete(insight) } }
                }
                .font(.footnote)
                .buttonStyle(.borderless)
                .padding(.top, 4)
            default:
                HStack { ProgressView(); Text("Working…").foregroundStyle(.secondary) }
            }
        } label: {
            HStack {
                Label(insight.title.isEmpty ? String(localized: "Insight") : insight.title, systemImage: "sparkles")
                if insight.isPending { ProgressView().controlSize(.mini) }
            }
        }
    }
}
