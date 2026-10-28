import SwiftUI

// One agent: header, the timeline (messages in Markdown, tool calls collapsed), pending permissions with
// Allow / Deny, and a composer that sends, steers while the agent runs, and stops it.

struct AgentDetailView: View {
    @Environment(AgentsStore.self) private var store
    let agentId: String
    let fallbackTitle: String
    @State private var model: AgentDetailModel?

    var body: some View {
        Group {
            if let model {
                AgentDetailContent(model: model, store: store, fallbackTitle: fallbackTitle)
            } else {
                Color.clear
            }
        }
        .background(Theme.page.ignoresSafeArea())
        .onAppear { if model == nil { model = AgentDetailModel(agentId: agentId, store: store) } }
    }
}

private struct AgentDetailContent: View {
    @Bindable var model: AgentDetailModel
    let store: AgentsStore
    let fallbackTitle: String
    @State private var visibleCount = 120
    @State private var atBottom = true
    @State private var showDiff = false
    @FocusState private var composerFocused: Bool

    private var title: String {
        if let t = model.snapshot?.title?.trimmingCharacters(in: .whitespacesAndNewlines), !t.isEmpty { return t }
        return fallbackTitle
    }

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 12) {
                    header
                    let all = model.entries
                    if all.count > visibleCount {
                        Button("Show earlier (\(all.count - visibleCount))") { visibleCount += 200 }
                            .font(.footnote)
                            .frame(maxWidth: .infinity)
                    }
                    ForEach(all.suffix(visibleCount)) { entry in
                        AgentEntryView(entry: entry)
                    }
                    if model.loaded, all.isEmpty {
                        Text("Nothing here yet.").font(.footnote).foregroundStyle(.secondary).frame(maxWidth: .infinity)
                    }
                    Color.clear.frame(height: 1).id("bottom")
                        .onAppear { atBottom = true }
                        .onDisappear { atBottom = false }
                }
                .padding(.horizontal, LucinaTokens.Space.gutter)
                .padding(.vertical, 8)
            }
            .scrollDismissesKeyboard(.interactively)
            .onChange(of: model.loaded) { _, loaded in
                if loaded { proxy.scrollTo("bottom", anchor: .bottom) }
            }
            .onChange(of: model.rows.count) { _, _ in
                if atBottom { proxy.scrollTo("bottom", anchor: .bottom) }
            }
            .onChange(of: model.entries.last?.item) { _, _ in
                if atBottom { proxy.scrollTo("bottom", anchor: .bottom) }
            }
        }
        .safeAreaInset(edge: .bottom, spacing: 0) { bottomBar }
        .navigationTitle(Text(verbatim: title))
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) { menu }
        }
        .refreshable { await model.catchUp() }
        .task {
            model.attach()
            await model.load()
        }
        .onDisappear { model.detach() }
        .task {
            // Without the stream, look again every 3 s.
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(3))
                if !store.isLive, model.loaded { await model.catchUp() }
            }
        }
        .onChange(of: store.isLive) { _, live in
            if live { Task { await model.catchUp() } }
        }
        .agentsLive(store)
        .sheet(isPresented: $showDiff) { AgentDiffView(model: model) }
        .accessibilityIdentifier("agentDetail")
    }

    // MARK: Header

    @ViewBuilder private var header: some View {
        if let s = model.snapshot {
            let item = s.item
            Card(spacing: 6) {
                HStack(spacing: 8) {
                    Image(systemName: AgentColors.symbol(item.bucket)).foregroundStyle(AgentColors.bucket(item.bucket))
                    Text(verbatim: item.stateLabel).font(.subheadline.weight(.semibold)).foregroundStyle(AgentColors.bucket(item.bucket))
                    if item.isArchived { Text("Archived").font(.caption).foregroundStyle(.secondary) }
                    Spacer()
                    if model.sending { ProgressView().controlSize(.small) }
                }
                if !item.providerLine.isEmpty {
                    Text(verbatim: item.providerLine).font(.footnote).foregroundStyle(.secondary)
                }
                if !s.cwd.isEmpty {
                    Text(verbatim: Self.shortPath(s.cwd)).font(.caption.monospaced()).foregroundStyle(.tertiary).lineLimit(1).truncationMode(.head)
                }
                if let gate = s.gate {
                    Label(gate.label, systemImage: gate.verdict == "verified" ? "checkmark.seal" : "exclamationmark.shield")
                        .font(.footnote)
                        .foregroundStyle(gate.verdict == "verified" ? Theme.positive : Theme.warning)
                }
                if let error = s.lastError, !error.isEmpty, item.bucket == .failed {
                    Text(verbatim: error).font(.footnote).foregroundStyle(Theme.negative).lineLimit(4)
                }
            }
        } else if let error = model.loadError {
            Card { Text(verbatim: error).font(.footnote).foregroundStyle(.secondary) }
        } else {
            ProgressView().frame(maxWidth: .infinity).padding(.top, 40)
        }
    }

    static func shortPath(_ path: String) -> String {
        let home = "/home/nez/"
        return path.hasPrefix(home) ? "~/" + path.dropFirst(home.count) : path
    }

    // MARK: Menu

    private var menu: some View {
        Menu {
            if model.snapshot?.workspaceId != nil {
                Button { showDiff = true } label: { Label("Changes", systemImage: "plus.forwardslash.minus") }
            }
            if model.isRunning {
                Button(role: .destructive) { Task { await model.stop() } } label: { Label("Stop", systemImage: "stop.circle") }
            }
            if model.snapshot != nil {
                Button { Task { await model.setArchived(!model.isArchived) } } label: {
                    Label(model.isArchived ? "Unarchive" : "Archive", systemImage: model.isArchived ? "tray.and.arrow.up" : "archivebox")
                }
            }
        } label: {
            Image(systemName: "ellipsis.circle")
        }
        .accessibilityLabel(Text("More"))
    }

    // MARK: Bottom bar

    private var bottomBar: some View {
        VStack(spacing: 8) {
            if let error = model.actionError ?? (model.snapshot == nil ? nil : model.loadError) {
                Text(verbatim: error)
                    .font(.footnote)
                    .foregroundStyle(Theme.negative)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            ForEach(model.pending, id: \.id) { permission in
                AgentPermissionCard(permission: permission, approvalId: model.approvalId(for: permission),
                                    busy: model.answering.contains(permission.id)) { allow in
                    Task { await model.answer(permission, allow: allow) }
                }
            }
            composer
        }
        .padding(.horizontal, LucinaTokens.Space.gutter)
        .padding(.top, 8)
        .padding(.bottom, 8)
        .background(.bar)
    }

    private var composer: some View {
        HStack(alignment: .bottom, spacing: 8) {
            TextField(model.isRunning ? "Steer the agent…" : "Message the agent…", text: $model.draft, axis: .vertical)
                .lineLimit(1...5)
                .focused($composerFocused)
                .padding(.horizontal, 12)
                .padding(.vertical, 8)
                .background(Theme.field, in: RoundedRectangle(cornerRadius: 18, style: .continuous))
                .accessibilityIdentifier("agentComposer")
            if model.isRunning {
                Button { Task { await model.stop() } } label: {
                    Image(systemName: "stop.circle.fill").font(.system(size: 30)).foregroundStyle(Theme.negative)
                }
                .accessibilityLabel(Text("Stop"))
            }
            Button {
                Task { if await model.send() { composerFocused = false } }
            } label: {
                Image(systemName: "arrow.up.circle.fill").font(.system(size: 30))
            }
            .disabled(model.draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || model.sending || model.isArchived)
            .accessibilityLabel(Text(model.isRunning ? "Steer" : "Send"))
        }
    }
}

// MARK: - Permissions

struct AgentPermissionCard: View {
    let permission: AgentPermission
    let approvalId: String?
    let busy: Bool
    let answer: (Bool) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 6) {
                Image(systemName: "hand.raised.fill").foregroundStyle(Theme.warning)
                Text(verbatim: permission.headline).font(.subheadline.weight(.semibold)).lineLimit(2)
            }
            if let d = permission.description, !d.isEmpty {
                Text(verbatim: d).font(.footnote).foregroundStyle(.secondary).lineLimit(3)
            }
            if !permission.inputText.isEmpty {
                Text(verbatim: AgentToolDetail.clip(permission.inputText))
                    .font(.caption.monospaced())
                    .lineLimit(6)
                    .padding(8)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(Theme.field, in: RoundedRectangle(cornerRadius: 8, style: .continuous))
            }
            HStack(spacing: 10) {
                Button(role: .destructive) { answer(false) } label: {
                    Text("Deny").frame(maxWidth: .infinity)
                }
                .buttonStyle(.bordered)
                Button { answer(true) } label: {
                    Text("Allow").frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .tint(Theme.accentFill)
            }
            .disabled(busy)
            if approvalId != nil {
                Label("Also in Approvals", systemImage: "checkmark.shield").font(.caption2).foregroundStyle(.secondary)
            }
        }
        .padding(12)
        .background(Theme.card, in: RoundedRectangle(cornerRadius: Theme.cornerRadius, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: Theme.cornerRadius, style: .continuous).strokeBorder(Theme.warning.opacity(0.6), lineWidth: 1))
        .accessibilityIdentifier("agentPermission")
    }
}

// MARK: - Entries

struct AgentEntryView: View {
    let entry: AgentEntry

    var body: some View {
        switch entry.item {
        case let .userMessage(text):
            HStack {
                Spacer(minLength: 40)
                Text(verbatim: text)
                    .padding(.horizontal, 12).padding(.vertical, 8)
                    .foregroundStyle(Theme.onAccent)
                    .background(Theme.accentFill, in: RoundedRectangle(cornerRadius: 16, style: .continuous))
                    .textSelection(.enabled)
            }
        case let .assistantMessage(text, _):
            NoteMarkdownView(blocks: NoteText.parse(text))
                .frame(maxWidth: .infinity, alignment: .leading)
        case let .reasoning(text):
            DisclosureGroup {
                Text(verbatim: text).font(.footnote).foregroundStyle(.secondary).textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
            } label: {
                Label("Thinking", systemImage: "brain").font(.footnote).foregroundStyle(.secondary)
            }
        case let .toolCall(call):
            AgentToolRow(call: call)
        case let .todo(items):
            VStack(alignment: .leading, spacing: 4) {
                ForEach(Array(items.enumerated()), id: \.offset) { _, t in
                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                        Image(systemName: t.completed ? "checkmark.circle.fill" : (t.inProgress ? "circle.dotted" : "circle"))
                            .foregroundStyle(t.completed ? Theme.positive : Theme.accent)
                        Text(verbatim: t.text).strikethrough(t.completed).foregroundStyle(t.completed ? .secondary : .primary)
                    }
                    .font(.footnote)
                }
            }
            .padding(10)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Theme.card, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
        case let .error(message):
            banner(message, color: Theme.negative, symbol: "exclamationmark.triangle.fill")
        case let .notification(level, message):
            banner(message, color: level == "error" ? Theme.negative : (level == "warning" ? Theme.warning : Theme.info),
                   symbol: level == "info" ? "info.circle" : "exclamationmark.circle")
        case let .compaction(done):
            Text(done ? "Context compacted" : "Compacting context…").font(.caption).foregroundStyle(.tertiary).frame(maxWidth: .infinity)
        case .other:
            EmptyView()
        }
    }

    private func banner(_ text: String, color: Color, symbol: String) -> some View {
        HStack(alignment: .top, spacing: 8) {
            Image(systemName: symbol).foregroundStyle(color)
            Text(verbatim: text).font(.footnote).foregroundStyle(color).textSelection(.enabled)
        }
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(color.opacity(0.12), in: RoundedRectangle(cornerRadius: 12, style: .continuous))
    }
}

struct AgentToolRow: View {
    let call: AgentToolCall

    private var symbol: String {
        switch call.detail.type {
        case "shell": "terminal"
        case "read": "doc.text"
        case "edit": "pencil"
        case "write": "square.and.pencil"
        case "search": "magnifyingglass"
        case "fetch": "globe"
        case "sub_agent": "person.2"
        case "plan": "list.bullet.rectangle"
        default: "wrench.and.screwdriver"
        }
    }

    private var statusColor: Color {
        switch call.status {
        case "completed": Theme.positive
        case "failed": Theme.negative
        case "canceled": Theme.textSecondary
        default: Theme.accent
        }
    }

    var body: some View {
        DisclosureGroup {
            let body = call.detail.body
            VStack(alignment: .leading, spacing: 6) {
                if !body.isEmpty {
                    Text(verbatim: body)
                        .font(.caption.monospaced())
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                if let error = call.error {
                    Text(verbatim: error).font(.caption).foregroundStyle(Theme.negative)
                }
                if body.isEmpty, call.error == nil {
                    Text("No details.").font(.caption).foregroundStyle(.secondary)
                }
            }
            .padding(8)
            .background(Theme.field, in: RoundedRectangle(cornerRadius: 8, style: .continuous))
        } label: {
            HStack(spacing: 8) {
                Image(systemName: symbol).foregroundStyle(statusColor).frame(width: 20)
                VStack(alignment: .leading, spacing: 1) {
                    Text(verbatim: call.name).font(.footnote.weight(.semibold))
                    if let s = call.detail.summary, !s.isEmpty {
                        Text(verbatim: s).font(.caption.monospaced()).foregroundStyle(.secondary).lineLimit(1)
                    }
                }
                Spacer(minLength: 0)
                if call.status == "running" { ProgressView().controlSize(.mini) }
                else if call.status == "failed" { Image(systemName: "xmark.circle.fill").foregroundStyle(Theme.negative) }
            }
        }
        .tint(Theme.textSecondary)
        .padding(10)
        .background(Theme.card, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
    }
}
