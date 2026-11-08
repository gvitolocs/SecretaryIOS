import SwiftUI

struct AllChatsView: View {
    @Environment(AppModel.self) private var app
    @State private var conversations: [ChatConversation] = []
    @State private var error: String?
    @State private var refreshing = false

    var body: some View {
        List {
            if let error { Text(error).font(.footnote).foregroundStyle(.secondary) }
            if conversations.isEmpty {
                Text("No conversations yet").foregroundStyle(.secondary)
            }
            ForEach(conversations) { conversation in
                if isThisDevice(conversation) {
                    Button {
                        app.showAllChats = false
                        app.selectedTab = .chat
                    } label: { ConversationSummary(conversation: conversation, thisDevice: true) }
                    .buttonStyle(.plain)
                } else {
                    NavigationLink {
                        ConversationView(conversation: conversation)
                    } label: { ConversationSummary(conversation: conversation, thisDevice: false) }
                }
            }
        }
        .scrollContentBackground(.hidden)
        .background(Theme.page)
        .navigationTitle("All chats")
        .refreshable { await refresh() }
        .task {
            conversations = (try? await app.chat.database.cachedConversations()) ?? []
            await refresh()
        }
    }

    private func isThisDevice(_ conversation: ChatConversation) -> Bool {
        guard let id = app.deviceId else { return false }
        return conversation.key == "device:" + id
    }

    private func refresh() async {
        guard !refreshing else { return }
        refreshing = true
        defer { refreshing = false }
        do {
            let updated = try await app.api.conversations()
            try Task.checkCancellation()
            try await app.chat.database.saveConversations(updated)
            try Task.checkCancellation()
            conversations = updated
            error = nil
        } catch is CancellationError {
            return
        } catch { self.error = APIError.wrap(error).userMessage }
    }
}

private struct ConversationSummary: View {
    let conversation: ChatConversation
    let thisDevice: Bool

    private var kindLabel: String {
        switch conversation.kind {
        case "device", "legacy": return String(localized: "Device")
        case "agent": return String(localized: "Agent")
        default: return String(localized: "Bot")
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text(verbatim: conversation.title).font(.headline).lineLimit(1)
                Spacer(minLength: 4)
                Text(kindLabel).font(.caption2.weight(.semibold))
                    .padding(.horizontal, 6).padding(.vertical, 3)
                    .background(Theme.accentSoft, in: Capsule())
            }
            if thisDevice { Text("This iPhone").font(.caption).foregroundStyle(Theme.accent) }
            Text(verbatim: conversation.lastText).font(.subheadline).foregroundStyle(.secondary).lineLimit(2)
            HStack {
                Text(conversation.lastTs, style: .relative)
                Spacer()
                Text("\(conversation.count) messages")
            }
            .font(.caption).foregroundStyle(.secondary)
        }
        .padding(.vertical, 4)
    }
}

struct ConversationView: View {
    @Environment(AppModel.self) private var app
    let conversation: ChatConversation
    @State private var store: ConversationStore?
    @State private var actions = ChatActions()

    var body: some View {
        VStack(spacing: 0) {
            if !conversation.canContinue {
                Text("This conversation is read-only.")
                    .font(.footnote).foregroundStyle(.secondary).lineLimit(1).minimumScaleFactor(0.8).padding(12)
            }
            if let error = store?.error {
                Text(error).font(.footnote).foregroundStyle(.secondary).padding(.horizontal)
            }
            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(spacing: 10) {
                        if store?.hasOlder == true {
                            Button("Load older messages") {
                                let anchor = store?.items.first?.id
                                Task {
                                    await store?.loadOlder()
                                    if let anchor { proxy.scrollTo(anchor, anchor: .top) }
                                }
                            }
                            .disabled(store?.isLoadingOlder == true)
                        }
                        ForEach(ToolSteps.entries(store?.items ?? [])) { entry in
                            switch entry {
                            case let .item(item):
                                MessageRow(item: item, job: nil, readOnly: true).id(item.id)
                            case let .tools(steps):
                                ToolStepsRow(steps: steps).id(entry.id)
                            }
                        }
                    }
                    .padding(12)
                }
                .defaultScrollAnchor(.bottom)
                .refreshable { await store?.refresh() }
            }
            if conversation.canContinue {
                Button("Continue here") {
                    guard let last = store?.items.last, app.chat.beginReply(last) else { return }
                    app.showAllChats = false
                    app.selectedTab = .chat
                }
                .buttonStyle(.borderedProminent)
                .disabled(store?.items.isEmpty != false)
                .padding(12)
            }
        }
        .background(Theme.page)
        .navigationTitle(conversation.title)
        .navigationBarTitleDisplayMode(.inline)
        .environment(actions)
        .task {
            if store == nil {
                store = ConversationStore(key: conversation.key, database: app.chat.database, api: { app.api })
            }
            await store?.open()
        }
    }
}
