import SwiftUI

/// The Chat tab: a session picker, the conversation, and a composer with the model choice.
/// Answers come only from the notebook's sources and cite them.
struct NotebookChatView: View {
    let store: NotebookStore
    @State private var chatStore: NotebookChatStore?
    @State private var draft = ""
    @State private var renaming: NotebookChat?
    @State private var renameText = ""
    @State private var pendingDelete: NotebookChat?
    @State private var pickedInitial = false
    @AppStorage("secretary.notebooks.model") private var modelKey = NotebookModel.deepseek.rawValue
    @FocusState private var composing: Bool

    private var model: NotebookModel { NotebookModel(rawValue: modelKey) ?? .deepseek }

    private var sortedChats: [NotebookChat] {
        store.chats.sorted { (NoteDates.parse($0.updatedAt) ?? .distantPast) > (NoteDates.parse($1.updatedAt) ?? .distantPast) }
    }

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider()
            if let chatStore {
                NotebookThreadView(chatStore: chatStore, notebook: store)
                    .id(chatStore.chatId)
            } else if !store.loaded {
                ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                emptyThread
            }
            composer
        }
        .onChange(of: store.loaded, initial: true) { _, loaded in
            // Open the most recent chat once the notebook has loaded.
            guard loaded, !pickedInitial else { return }
            pickedInitial = true
            if let first = sortedChats.first { open(first) }
        }
        .alert("Rename chat", isPresented: Binding(get: { renaming != nil }, set: { if !$0 { renaming = nil } })) {
            TextField("Title", text: $renameText)
            Button("Save") {
                if let chat = renaming, !renameText.trimmingCharacters(in: .whitespaces).isEmpty {
                    Task { await store.renameChat(chat, title: renameText.trimmingCharacters(in: .whitespaces)) }
                }
            }
            Button("Cancel", role: .cancel) {}
        }
        .confirmationDialog("Delete this chat?", isPresented: Binding(get: { pendingDelete != nil }, set: { if !$0 { pendingDelete = nil } }),
                            titleVisibility: .visible, presenting: pendingDelete) { chat in
            Button("Delete chat", role: .destructive) {
                Task {
                    await store.deleteChat(chat)
                    if chatStore?.chatId == chat.id { chatStore = nil; if let next = sortedChats.first { open(next) } }
                }
            }
            Button("Cancel", role: .cancel) {}
        }
    }

    // MARK: Pieces

    private var header: some View {
        HStack(spacing: 10) {
            Menu {
                ForEach(sortedChats) { chat in
                    Button {
                        open(chat)
                    } label: {
                        if chat.id == chatStore?.chatId { Label(chat.displayTitle, systemImage: "checkmark") } else { Text(chat.displayTitle) }
                    }
                }
                if !sortedChats.isEmpty { Divider() }
                Button("New chat", systemImage: "square.and.pencil") { Task { await newChat() } }
                if let current = store.chats.first(where: { $0.id == chatStore?.chatId }) {
                    Button("Rename…", systemImage: "pencil") { renameText = current.displayTitle; renaming = current }
                    Button("Delete chat", systemImage: "trash", role: .destructive) { pendingDelete = current }
                }
            } label: {
                HStack(spacing: 6) {
                    Image(systemName: "bubble.left.and.bubble.right")
                    Text(currentTitle).lineLimit(1)
                    Image(systemName: "chevron.up.chevron.down").font(.caption2)
                }
                .font(.subheadline.weight(.semibold))
            }
            .accessibilityIdentifier("chatPicker")
            Spacer(minLength: 0)
            Button { Task { await newChat() } } label: { Image(systemName: "square.and.pencil") }
                .accessibilityLabel("New chat")
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 8)
    }

    private var currentTitle: String {
        if let chatStore, let chat = store.chats.first(where: { $0.id == chatStore.chatId }) { return chat.displayTitle }
        return String(localized: "New chat")
    }

    private var emptyThread: some View {
        VStack(spacing: 12) {
            Spacer()
            Image(systemName: "text.bubble").font(.largeTitle).foregroundStyle(Theme.accent).accessibilityHidden(true)
            Text("Ask your sources").font(.title3.bold())
            Text(readySources == 0
                 ? "Add a source first. Chats answer only from what is in the notebook, and cite the page or minute."
                 : "Answers come only from the \(readySources) sources and notes in this notebook, with citations you can tap.")
                .font(.subheadline).foregroundStyle(.secondary).multilineTextAlignment(.center)
                .padding(.horizontal, 32)
            Spacer()
        }
        .frame(maxWidth: .infinity)
    }

    private var readySources: Int { store.sources.filter { $0.isReady && $0.sourceContext != .off }.count }

    private var composer: some View {
        VStack(spacing: 8) {
            if let error = chatStore?.error {
                ErrorBanner(message: error) { chatStore?.error = nil }
            }
            Picker("Model", selection: $modelKey) {
                ForEach(NotebookModel.allCases) { Text($0.label).tag($0.rawValue) }
            }
            .pickerStyle(.segmented)
            .frame(maxWidth: 220)
            .frame(maxWidth: .infinity, alignment: .leading)
            .accessibilityIdentifier("notebookModel")
            HStack(alignment: .bottom, spacing: 8) {
                TextField("Ask about your sources", text: $draft, axis: .vertical)
                    .lineLimit(1...5)
                    .focused($composing)
                    .padding(.horizontal, 14)
                    .padding(.vertical, 10)
                    .background(Theme.field, in: RoundedRectangle(cornerRadius: 20, style: .continuous))
                    .accessibilityIdentifier("notebookChatField")
                Button { Task { await send() } } label: {
                    Image(systemName: "arrow.up.circle.fill").font(.system(size: 34))
                }
                .tint(Theme.accentFill)
                .disabled(!canSend)
                .accessibilityLabel("Send")
                .accessibilityIdentifier("notebookChatSend")
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 8)
        .background(.bar)
    }

    private var canSend: Bool { !draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && chatStore?.sending != true }

    // MARK: Actions

    private func open(_ chat: NotebookChat) {
        guard chatStore?.chatId != chat.id else { return }
        let next = NotebookChatStore(chat: chat, api: store.api)
        next.onNoteSaved = { store.noteAdded($0) }
        next.onChatChanged = { store.chatChanged($0) }
        chatStore = next
    }

    private func newChat() async {
        guard let chat = await store.createChat() else { return }
        open(chat)
    }

    private func send() async {
        let text = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return }
        if chatStore == nil {
            guard let chat = await store.createChat() else { return }
            open(chat)
        }
        guard let current = chatStore else { return }
        draft = ""
        let firstQuestion = current.messages.isEmpty
        if await current.send(text, model: model) {
            if firstQuestion, let chat = store.chats.first(where: { $0.id == current.chatId }), chat.title.isEmpty || chat.title == "New chat" {
                await store.renameChat(chat, title: String(text.prefix(48)))
            }
        } else {
            draft = text
        }
    }
}

// MARK: - Conversation

private struct NotebookThreadView: View {
    let chatStore: NotebookChatStore
    let notebook: NotebookStore
    @State private var citation: NotebookCitation?

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 14) {
                    if !chatStore.loaded {
                        ProgressView().frame(maxWidth: .infinity).padding(.top, 40)
                    } else if chatStore.messages.isEmpty {
                        Text("Ask a question about this notebook's sources.")
                            .font(.subheadline).foregroundStyle(.secondary)
                            .frame(maxWidth: .infinity).padding(.top, 40)
                    }
                    ForEach(chatStore.messages) { message in
                        MessageView(message: message, chatStore: chatStore) { citation = $0 }
                            .id(message.id)
                    }
                    if chatStore.sending {
                        HStack(spacing: 8) {
                            ProgressView()
                            Text("Reading the sources…").font(.subheadline).foregroundStyle(.secondary)
                        }
                        .id("typing")
                    }
                }
                .padding(16)
            }
            .scrollDismissesKeyboard(.interactively)
            .onChange(of: chatStore.messages.count) { _, _ in scrollToEnd(proxy) }
            .onChange(of: chatStore.sending) { _, _ in scrollToEnd(proxy) }
            .onChange(of: chatStore.loaded) { _, _ in scrollToEnd(proxy, animated: false) }
        }
        .task { await chatStore.load() }
        .sheet(item: $citation) { citation in
            CitationSheet(citation: citation, api: notebook.api)
        }
    }

    private func scrollToEnd(_ proxy: ScrollViewProxy, animated: Bool = true) {
        let target: AnyHashable? = chatStore.sending ? "typing" : chatStore.messages.last?.id
        guard let target else { return }
        if animated {
            withAnimation { proxy.scrollTo(target, anchor: .bottom) }
        } else {
            proxy.scrollTo(target, anchor: .bottom)
        }
    }
}

private struct MessageView: View {
    let message: NotebookMessage
    let chatStore: NotebookChatStore
    let onCite: (NotebookCitation) -> Void

    var body: some View {
        if message.isUser {
            HStack {
                Spacer(minLength: 48)
                Text(message.text)
                    .padding(.horizontal, 14).padding(.vertical, 10)
                    .foregroundStyle(.white)
                    .background(Theme.accentFill, in: RoundedRectangle(cornerRadius: 18, style: .continuous))
                    .textSelection(.enabled)
            }
        } else {
            let parsed = NotebookCitations.parse(message.text, citations: message.citations)
            VStack(alignment: .leading, spacing: 10) {
                NotebookAnswerView(parsed: parsed, onCite: onCite)
                if !parsed.sources.isEmpty { sourceChips(parsed) }
                actions
            }
            .padding(14)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Theme.card, in: RoundedRectangle(cornerRadius: Theme.cornerRadius, style: .continuous))
        }
    }

    private func sourceChips(_ parsed: NotebookCitations.Parsed) -> some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 6) {
                ForEach(Array(parsed.sources.enumerated()), id: \.offset) { index, source in
                    Button { onCite(source) } label: {
                        HStack(spacing: 5) {
                            Text(verbatim: "\(index + 1)").font(.caption2.weight(.bold))
                                .frame(minWidth: 16, minHeight: 16)
                                .background(Theme.accentFill, in: Circle())
                                .foregroundStyle(.white)
                            Text(verbatim: chipLabel(source)).font(.caption).lineLimit(1)
                        }
                        .padding(.vertical, 4).padding(.leading, 4).padding(.trailing, 10)
                        .background(Theme.field, in: Capsule())
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("Source \(index + 1): \(source.title)")
                }
            }
        }
    }

    private func chipLabel(_ source: NotebookCitation) -> String {
        let title = source.title.isEmpty ? source.ref : source.title
        return source.position.isEmpty ? title : "\(title) · \(source.position)"
    }

    private var actions: some View {
        HStack(spacing: 14) {
            if let note = chatStore.savedNotes[message.id] {
                NavigationLink {
                    NoteDetailView(key: note.slug)
                } label: {
                    Label("Saved in Keep", systemImage: "checkmark.circle.fill")
                }
            } else {
                Button { Task { await chatStore.saveToNote(message) } } label: {
                    if chatStore.saving.contains(message.id) {
                        ProgressView().controlSize(.small)
                    } else {
                        Label("Save to note", systemImage: "square.and.arrow.down")
                    }
                }
                .accessibilityIdentifier("saveAnswerToNote")
            }
            Button {
                UIPasteboard.general.string = NotebookCitations.stripped(message.text)
            } label: { Label("Copy", systemImage: "doc.on.doc") }
            Spacer(minLength: 0)
            if let model = message.model, !model.isEmpty {
                Text(verbatim: model).font(.caption2).foregroundStyle(.tertiary)
            }
        }
        .font(.footnote)
        .buttonStyle(.borderless)
    }
}

/// The answer as markdown, with the citation tokens drawn as small numbered chips.
struct NotebookAnswerView: View {
    let parsed: NotebookCitations.Parsed
    let onCite: (NotebookCitation) -> Void

    var body: some View {
        NoteMarkdownView(blocks: NoteText.parse(parsed.text), inline: { Self.attributed($0) })
            .environment(\.openURL, OpenURLAction { url in
                guard url.scheme == Self.scheme, let n = Int(url.host() ?? ""), parsed.sources.indices.contains(n - 1) else { return .systemAction }
                onCite(parsed.sources[n - 1])
                return .handled
            })
    }

    static let scheme = "secretary-citation"

    /// Inline markdown with every citation mark replaced by a tappable chip.
    static func attributed(_ run: String) -> AttributedString {
        let result = NoteText.inline(run)
        let plain = String(result.characters)
        guard plain.contains(NotebookCitations.open),
              let regex = try? NSRegularExpression(pattern: "\(NotebookCitations.open)[0-9,]+\(NotebookCitations.close)") else { return result }
        var out = AttributedString()
        var cursor = result.startIndex
        for match in regex.matches(in: plain, range: NSRange(plain.startIndex..., in: plain)) {
            guard let range = Range(match.range, in: plain),
                  let lower = AttributedString.Index(range.lowerBound, within: result),
                  let upper = AttributedString.Index(range.upperBound, within: result) else { continue }
            out.append(AttributedString(result[cursor..<lower]))
            for piece in NotebookCitations.pieces(of: String(plain[range])) {
                if case let .mark(n) = piece { out.append(chip(n)) }
            }
            cursor = upper
        }
        out.append(AttributedString(result[cursor...]))
        return out
    }

    private static func chip(_ number: Int) -> AttributedString {
        var gap = AttributedString("\u{200A}")
        gap.font = .caption2
        var chip = AttributedString("\u{2009}\(number)\u{2009}")
        chip.font = .caption2.weight(.bold)
        chip.foregroundColor = Theme.accent
        chip.backgroundColor = Theme.accent.opacity(0.18)
        chip.baselineOffset = 3
        chip.link = URL(string: "\(scheme)://\(number)")
        return gap + chip
    }
}

// MARK: - Citation sheet

/// The passage behind a citation, with the pages around it.
private struct CitationSheet: View {
    @Environment(\.dismiss) private var dismiss
    let citation: NotebookCitation
    let api: () -> NotebooksAPI
    @State private var around: [SourceChunk] = []
    @State private var loading = false
    @State private var failed: String?

    var body: some View {
        NavigationStack {
            List {
                Section {
                    VStack(alignment: .leading, spacing: 8) {
                        Label(citation.title.isEmpty ? citation.ref : citation.title, systemImage: icon)
                            .font(.headline)
                        if !citation.position.isEmpty {
                            Text(citation.position).font(.subheadline.weight(.semibold)).foregroundStyle(Theme.accent)
                        }
                        if citation.excerpt.isEmpty {
                            Text("The passage isn't available.").foregroundStyle(.secondary)
                        } else {
                            Text(citation.excerpt).font(.body).textSelection(.enabled)
                        }
                    }
                    .padding(.vertical, 4)
                }
                if citation.kind == "note" {
                    Section {
                        NavigationLink {
                            NoteDetailView(key: String(citation.ref.dropFirst("note:".count)))
                        } label: { Label("Open note", systemImage: "note.text") }
                    }
                }
                if let sourceId = citation.sourceId, citation.chunk != nil {
                    Section {
                        if around.isEmpty {
                            Button {
                                Task { await loadAround(sourceId) }
                            } label: {
                                HStack {
                                    Label("Show the passages around it", systemImage: "text.alignleft")
                                    if loading { Spacer(); ProgressView() }
                                }
                            }
                            .disabled(loading)
                        }
                        ForEach(around) { chunk in
                            VStack(alignment: .leading, spacing: 4) {
                                if !chunk.position.isEmpty {
                                    Text(chunk.position).font(.caption.weight(.semibold)).foregroundStyle(Theme.accent)
                                }
                                Text(chunk.text).font(.subheadline)
                                    .fontWeight(chunk.ordinal == citation.chunk ? .semibold : .regular)
                            }
                            .padding(.vertical, 2)
                        }
                        if let failed { Text(failed).font(.footnote).foregroundStyle(.red) }
                    }
                }
            }
            .navigationTitle("Source")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } } }
        }
        .presentationDetents([.medium, .large])
    }

    private var icon: String {
        switch citation.kind {
        case "note": "note.text"
        case "insight": "sparkles"
        default: "doc.text"
        }
    }

    private func loadAround(_ sourceId: String) async {
        loading = true
        defer { loading = false }
        do {
            around = try await api().chunks(sourceId, limit: 5, around: citation.chunk)
            failed = nil
        } catch {
            failed = NotebooksError.message(error)
        }
    }
}
