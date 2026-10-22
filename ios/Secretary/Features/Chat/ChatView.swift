import SwiftUI

/// The Chat tab, built from parts so other surfaces can reuse them:
/// `[stage] + ChatTranscript + ChatComposer`. A future `LucinaStage` (the animated
/// avatar with voice) goes in the `stage` slot above the transcript; the transcript
/// and composer only depend on `ChatStore`. Nothing here plays audio.
struct ChatView<Stage: View>: View {
    @Environment(ChatStore.self) private var store
    @Environment(AppModel.self) private var app
    private let stage: Stage

    init(@ViewBuilder stage: () -> Stage) {
        self.stage = stage()
    }

    var body: some View {
        VStack(spacing: 0) {
            if let notice = app.lucinaNotice {
                ErrorBanner(message: notice, systemImage: "moon.zzz.fill") { app.lucinaNotice = nil }
                    .padding(.horizontal, 12)
                    .padding(.top, 6)
            }
            stage
            ChatTranscript()
            ChatComposer()
        }
        .background(Theme.page)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .principal) { ChatTitle() }
        }
    }
}

extension ChatView where Stage == EmptyView {
    init() { self.init { EmptyView() } }
}

/// "Lucina" with a Live pill while the timeline stream is connected.
struct ChatTitle: View {
    @Environment(ChatStore.self) private var store

    var body: some View {
        HStack(spacing: 8) {
            Text("Lucina").font(.headline)
            if store.mode == .server {
                HStack(spacing: 4) {
                    Circle()
                        .fill(store.isLive ? Color.green : Color.secondary)
                        .frame(width: 6, height: 6)
                    Text(store.isLive ? "Live" : "Offline")
                        .font(.caption2.weight(.semibold))
                        .foregroundStyle(store.isLive ? .primary : .secondary)
                }
                .padding(.horizontal, 7)
                .padding(.vertical, 3)
                .background(.thinMaterial, in: Capsule())
                .accessibilityElement(children: .ignore)
                .accessibilityLabel(store.isLive ? String(localized: "Live: new messages from every channel appear here") : String(localized: "Not connected to live updates"))
                .accessibilityIdentifier("liveIndicator")
            }
        }
    }
}

/// The scrolling list: day separators, bubbles with source badges, reminders, job cards,
/// and the thinking indicator. Scrolling to the top loads older messages.
struct ChatTranscript: View {
    @Environment(ChatStore.self) private var store
    /// The end of the list is on screen; new messages only scroll it when it is.
    @State private var atBottom = true

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(spacing: 10) {
                    if store.hasOlder {
                        HStack { Spacer(); ProgressView(); Spacer() }
                            .padding(.vertical, 8)
                            .onAppear { loadOlder(proxy) }
                            .accessibilityLabel("Loading older messages")
                    }
                    if store.items.isEmpty {
                        emptyState
                    }
                    ForEach(rows) { row in
                        switch row {
                        case let .day(date):
                            DaySeparator(date: date)
                        case let .message(item):
                            MessageRow(item: item, job: store.jobs[item.id])
                                .id(item.id)
                        }
                    }
                    if store.isSending {
                        ThinkingBubble().id("thinking")
                    }
                    Color.clear
                        .frame(height: 1)
                        .id(Self.endId)
                        .onAppear { atBottom = true }
                        .onDisappear { atBottom = false }
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 12)
            }
            .scrollDismissesKeyboard(.interactively)
            .defaultScrollAnchor(.bottom)
            .refreshable { await store.refresh() }
            .onChange(of: store.items.last?.id) { _, _ in
                if atBottom || store.items.last?.isLocal == true { scrollToEnd(proxy) }
            }
            .onChange(of: store.isSending) { _, sending in if sending { scrollToEnd(proxy) } }
            .onChange(of: store.items.last?.rich) { _, rich in if rich != nil, atBottom { scrollToEnd(proxy) } }
            .onAppear { scrollToEnd(proxy, animated: false) }
        }
    }

    enum Row: Identifiable {
        case day(Date)
        case message(TimelineItem)

        var id: String {
            switch self {
            case let .day(date): return "day-\(date.timeIntervalSince1970)"
            case let .message(item): return item.id
            }
        }
    }

    /// Items with a separator before each new calendar day.
    private var rows: [Row] {
        var out: [Row] = []
        var lastDay: Date?
        let calendar = Calendar.current
        for item in store.items {
            let day = calendar.startOfDay(for: item.ts)
            if day != lastDay {
                out.append(.day(day))
                lastDay = day
            }
            out.append(.message(item))
        }
        return out
    }

    private var emptyState: some View {
        VStack(spacing: 12) {
            Image(systemName: "sparkles")
                .font(.system(size: 40))
                .foregroundStyle(Theme.accent)
                .accessibilityHidden(true)
            Text("Ask Lucina anything").font(.title3.bold())
            Text("Reminders, the shopping list, errands. Messages from the web, Telegram, Discord and Alexa show up here too.")
                .font(.subheadline)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
        }
        .padding(.top, 80)
        .padding(.horizontal, 24)
        .accessibilityElement(children: .combine)
    }

    private func loadOlder(_ proxy: ScrollViewProxy) {
        let anchor = store.items.first?.id
        Task {
            await store.loadOlder()
            // Keep the message that was on top in place instead of jumping.
            if let anchor { proxy.scrollTo(anchor, anchor: .top) }
        }
    }

    static let endId = "transcript-end"

    /// Lazy rows have estimated heights, so one `scrollTo` can land short of the end:
    /// scroll again once the new rows are laid out.
    private func scrollToEnd(_ proxy: ScrollViewProxy, animated: Bool = true) {
        Task { @MainActor in
            await Task.yield()
            if animated {
                withAnimation(.easeOut(duration: 0.2)) { proxy.scrollTo(Self.endId, anchor: .bottom) }
            } else {
                proxy.scrollTo(Self.endId, anchor: .bottom)
            }
            try? await Task.sleep(for: .milliseconds(250))
            proxy.scrollTo(Self.endId, anchor: .bottom)
        }
    }
}

private struct DaySeparator: View {
    let date: Date

    var body: some View {
        Text(label)
            .font(.caption.weight(.semibold))
            .foregroundStyle(.secondary)
            .padding(.horizontal, 10)
            .padding(.vertical, 4)
            .background(.thinMaterial, in: Capsule())
            .frame(maxWidth: .infinity)
            .padding(.vertical, 4)
            .accessibilityAddTraits(.isHeader)
    }

    private var label: String {
        let calendar = Calendar.current
        if calendar.isDateInToday(date) { return "Today" }
        if calendar.isDateInYesterday(date) { return "Yesterday" }
        let sameYear = calendar.component(.year, from: date) == calendar.component(.year, from: Date())
        return date.formatted(sameYear ? .dateTime.weekday(.wide).day().month(.wide) : .dateTime.day().month(.wide).year())
    }
}

/// Where a message came from: icon + label, plus the chat or server for Telegram/Discord.
struct SourceBadge: View {
    let item: TimelineItem

    var body: some View {
        HStack(spacing: 4) {
            Image(systemName: item.channel.symbol)
                .imageScale(.small)
                .accessibilityHidden(true)
            Text(text)
            Text("·").accessibilityHidden(true)
            Text(item.ts.formatted(date: .omitted, time: .shortened))
        }
        .font(.caption2)
        .foregroundStyle(.secondary)
        .lineLimit(1)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("From \(text), \(item.ts.formatted(date: .omitted, time: .shortened))")
    }

    private var text: String {
        guard let meta = item.meta else { return item.channel.label }
        if item.channel == .discord {
            if meta.isDm == true { return "Discord DM" }
            let place = [meta.guildName, meta.channelName.map { "#\($0)" }].compactMap { $0 }.joined(separator: " ")
            return place.isEmpty ? "Discord" : String(localized: "Discord · \(place)")
        }
        if item.channel == .telegram, let title = meta.chatTitle, meta.isDm != true {
            return "Telegram · \(title)"
        }
        return item.channel.label
    }
}

private struct MessageRow: View {
    @Environment(ChatStore.self) private var store
    let item: TimelineItem
    let job: ChatJob?

    var body: some View {
        if item.meta?.isReminder == true {
            ReminderCard(item: item)
        } else if item.role == .lucina, let meta = item.meta, meta.isProactive {
            ProactiveCard(item: item, meta: meta)
        } else if item.role == .lucina, let rich = item.rich {
            richRow(rich)
        } else {
            bubbleRow
        }
    }

    /// Lucina message format v1: bubbles, widgets and buttons (LucinaRich.swift).
    private func richRow(_ rich: RichMessage) -> some View {
        HStack {
            VStack(alignment: .leading, spacing: 4) {
                RichReplyView(item: item, rich: rich, inferredChoice: store.inferredChoice(after: item))
                if let job { JobCard(job: job) }
                SourceBadge(item: item).padding(.horizontal, 4)
            }
            Spacer(minLength: 24)
        }
        // `.contain` keeps the buttons' own identifiers (a bare identifier would override them).
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("lucinaRichMessage")
    }

    private var bubbleRow: some View {
        let isUser = item.role == .user
        return HStack {
            if isUser { Spacer(minLength: 48) }
            VStack(alignment: isUser ? .trailing : .leading, spacing: 4) {
                if !item.text.isEmpty {
                    // Her plain replies are markdown too (the Notes renderer): no literal "**".
                    Text(isUser ? LinkText.attributed(item.text) : ChatMarkdown.inline(item.text))
                        .textSelection(.enabled)
                        .padding(.horizontal, 14)
                        .padding(.vertical, 10)
                        .foregroundStyle(isUser ? Color.white : Color.primary)
                        .tint(isUser ? Color.white : Theme.accent)
                        .background(isUser ? AnyShapeStyle(Theme.accentFill) : AnyShapeStyle(Theme.card),
                                    in: RoundedRectangle(cornerRadius: 18, style: .continuous))
                        .opacity(item.localStatus == .sending ? 0.7 : 1)
                        .accessibilityLabel(Text("\(isUser ? "You" : "Lucina"), via \(item.channel.label): \(item.text)"))
                        .accessibilityIdentifier(isUser ? "userMessage" : "lucinaMessage")
                }
                if !item.attachments.isEmpty {
                    ForEach(Array(item.attachments.enumerated()), id: \.offset) { _, attachment in
                        Label(attachment.name ?? attachment.kind.capitalized, systemImage: Self.symbol(for: attachment.kind))
                            .font(.caption)
                            .padding(.horizontal, 10)
                            .padding(.vertical, 6)
                            .background(Theme.field, in: Capsule())
                    }
                }
                if let job {
                    JobCard(job: job)
                }
                if item.localStatus == .error {
                    HStack(spacing: 6) {
                        Image(systemName: "exclamationmark.circle.fill").foregroundStyle(.red).accessibilityHidden(true)
                        Text(item.localError ?? "Not delivered.").font(.caption).foregroundStyle(.secondary)
                        Button("Retry") { Task { await store.retry(item.id) } }
                            .font(.caption.bold())
                            .disabled(store.isSending)
                            .accessibilityIdentifier("retryMessage")
                    }
                } else if item.localStatus == .sending {
                    Text("Sending…").font(.caption2).foregroundStyle(.secondary)
                } else {
                    SourceBadge(item: item)
                        .padding(.horizontal, 4)
                }
            }
            if !isUser { Spacer(minLength: 48) }
        }
    }

    static func symbol(for kind: String) -> String {
        switch kind.lowercased() {
        case "image", "photo": return "photo"
        case "audio", "voice": return "waveform"
        case "video": return "video"
        default: return "paperclip"
        }
    }
}

private struct ReminderCard: View {
    let item: TimelineItem

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: "bell.fill")
                .foregroundStyle(.orange)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 4) {
                Text("Reminder").font(.caption.weight(.semibold)).foregroundStyle(.orange)
                Text(LinkText.attributed(item.text)).font(.subheadline).textSelection(.enabled)
                SourceBadge(item: item)
            }
            Spacer(minLength: 0)
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.orange.opacity(0.10), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 14, style: .continuous).strokeBorder(Color.orange.opacity(0.35)))
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("reminderCard")
    }
}

/// "Lucina reached out": a message she sent on her own (crates/lucina-proactive), with its
/// priority, the feedback buttons and why she wrote. In-app links are web routes, so the
/// card offers the feedback and the reason only.
private struct ProactiveCard: View {
    @Environment(ChatStore.self) private var store
    let item: TimelineItem
    let meta: TimelineMeta
    @State private var chosen: String?
    @State private var busy = false
    @State private var failed = false
    @State private var showWhy = false

    private static let fallbackLabels = ["useful": "Useful", "not_now": "Not now", "stop": "Stop these"]
    private static let doneLabels = ["useful": "Thanks, noted.", "not_now": "Got it: not now.", "stop": "Got it: no more of these."]

    private var high: Bool { meta.priority == "high" }
    private var tint: Color { high ? .orange : .purple }
    private var priorityLabel: String {
        switch meta.priority {
        case "high": return "Important"
        case "low": return "FYI"
        default: return "For you"
        }
    }
    private var buttons: [(id: String, label: String)] {
        ProactiveAction.feedbackIds.map { id in
            (id, meta.actions?.first(where: { $0.id == id && $0.isFeedback })?.label ?? Self.fallbackLabels[id] ?? id)
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Label("Lucina reached out", systemImage: "sparkles")
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(tint)
                Spacer(minLength: 8)
                Text(priorityLabel)
                    .font(.caption2.weight(.semibold))
                    .padding(.horizontal, 8)
                    .padding(.vertical, 3)
                    .background(tint.opacity(0.15), in: Capsule())
                    .accessibilityIdentifier("proactivePriority")
            }
            Text(LinkText.attributed(item.text)).font(.subheadline).textSelection(.enabled)
            HStack(spacing: 6) {
                ForEach(buttons, id: \.id) { b in
                    Button(b.label) { tap(b.id) }
                        .font(.caption.weight(.medium))
                        .buttonStyle(.bordered)
                        .tint(chosen == b.id ? tint : .secondary)
                        .disabled(busy || chosen != nil)
                        .accessibilityIdentifier("proactiveFeedback_\(b.id)")
                }
            }
            if let chosen, let done = Self.doneLabels[chosen] {
                Text(done).font(.caption2).foregroundStyle(.secondary)
            } else if failed {
                Text("Couldn't send that. Try again.").font(.caption2).foregroundStyle(.red)
            }
            if let why = meta.why, !why.isEmpty {
                DisclosureGroup("Why?", isExpanded: $showWhy) {
                    Text(why).font(.caption2).foregroundStyle(.secondary).frame(maxWidth: .infinity, alignment: .leading)
                }
                .font(.caption2)
                .tint(.secondary)
            }
            SourceBadge(item: item)
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(tint.opacity(0.08), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 14, style: .continuous).strokeBorder(tint.opacity(0.4)))
        .accessibilityIdentifier("proactiveCard")
    }

    private func tap(_ action: String) {
        guard !busy, chosen == nil, let id = meta.outreachId else { return }
        busy = true
        failed = false
        Task {
            let ok = await store.outreachFeedback(id, action)
            busy = false
            if ok { chosen = action } else { failed = true }
        }
    }
}

/// The message field and send button. Sends on return.
struct ChatComposer: View {
    @Environment(ChatStore.self) private var store
    @State private var draft = ""
    @FocusState private var focused: Bool

    var body: some View {
        HStack(alignment: .bottom, spacing: 8) {
            TextField("Message Lucina", text: $draft, axis: .vertical)
                .lineLimit(1...6)
                .submitLabel(.send)
                .onSubmit(send)
                .focused($focused)
                .padding(.horizontal, 14)
                .padding(.vertical, 10)
                .background(Theme.card, in: RoundedRectangle(cornerRadius: 20, style: .continuous))
                .accessibilityIdentifier("chatComposer")
                .onChange(of: store.prefill) { _, request in
                    // A button asked for text in the composer: the owner finishes and sends it.
                    guard let request else { return }
                    draft = request.text.isEmpty || request.text.hasSuffix(" ") ? request.text : request.text + " "
                    focused = true
                }
                .onChange(of: draft) { _, value in
                    // A multi-line field inserts a newline on return instead of submitting:
                    // treat a typed trailing newline as "send".
                    if value.hasSuffix("\n") {
                        draft = String(value.dropLast())
                        send()
                    }
                }
            Button(action: send) {
                Image(systemName: "arrow.up.circle.fill")
                    .font(.system(size: 34))
                    .symbolRenderingMode(.hierarchical)
            }
            .disabled(!canSend)
            .accessibilityLabel("Send")
            .accessibilityIdentifier("chatSend")
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(.bar)
        .overlay(alignment: .topTrailing) {
            if draft.count > ChatStore.maxCharacters - 200 {
                Text("\(draft.count)/\(ChatStore.maxCharacters)")
                    .font(.caption2.monospacedDigit())
                    .foregroundStyle(draft.count > ChatStore.maxCharacters ? .red : .secondary)
                    .padding(.trailing, 60)
                    .offset(y: -14)
            }
        }
    }

    private var canSend: Bool {
        let cleaned = ChatStore.clean(draft)
        return !cleaned.isEmpty && cleaned.count <= ChatStore.maxCharacters && !store.isSending
    }

    private func send() {
        guard canSend else { return }
        let text = draft
        draft = ""
        focused = true
        Task {
            if !(await store.send(text)) { draft = text }
        }
    }
}

private struct JobCard: View {
    let job: ChatJob

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                statusIcon
                Text(title).font(.subheadline.weight(.semibold))
                Spacer(minLength: 0)
            }
            if let result = job.result, !result.isEmpty, job.status == "done" {
                Text(LinkText.attributed(result)).font(.subheadline).textSelection(.enabled)
            }
            if job.status == "error" {
                Text(job.error == "interrupted" ? String(localized: "Lucina restarted while working on this.") : (job.error ?? String(localized: "The task failed.")))
                    .font(.subheadline).foregroundStyle(.secondary)
            }
            if let raw = job.imageUrl, let url = URL(string: raw), ["http", "https"].contains(url.scheme?.lowercased() ?? "") {
                Link(destination: url) { Label("Open image", systemImage: "photo") }
                    .font(.subheadline.weight(.semibold))
            }
            if let pollError = job.pollError {
                Text(pollError).font(.caption).foregroundStyle(.orange)
            }
        }
        .padding(12)
        .frame(maxWidth: 300, alignment: .leading)
        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 14, style: .continuous).strokeBorder(Theme.accent.opacity(0.25)))
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("jobCard")
    }

    private var title: String {
        let action = (job.action ?? "task").replacingOccurrences(of: "_task", with: "").replacingOccurrences(of: "_", with: " ")
        switch job.status {
        case "queued": return "Queued: \(action)"
        case "running": return "Working on \(action)…"
        case "done": return "Done: \(action)"
        case "error": return "Failed: \(action)"
        default: return "\(job.status.capitalized): \(action)"
        }
    }

    @ViewBuilder private var statusIcon: some View {
        switch job.status {
        case "done": Image(systemName: "checkmark.circle.fill").foregroundStyle(.green)
        case "error": Image(systemName: "xmark.octagon.fill").foregroundStyle(.red)
        default: ProgressView().controlSize(.small)
        }
    }
}

private struct ThinkingBubble: View {
    var body: some View {
        HStack {
            HStack(spacing: 8) {
                TimelineView(.periodic(from: .now, by: 0.4)) { context in
                    let phase = Int(context.date.timeIntervalSinceReferenceDate / 0.4) % 3
                    HStack(spacing: 4) {
                        ForEach(0..<3) { index in
                            Circle()
                                .frame(width: 7, height: 7)
                                .opacity(index == phase ? 1 : 0.35)
                        }
                    }
                    .foregroundStyle(Theme.accent)
                }
                Text("Lucina is thinking…").font(.subheadline).foregroundStyle(.secondary)
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 10)
            .background(Theme.card, in: RoundedRectangle(cornerRadius: 18, style: .continuous))
            Spacer()
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("Lucina is thinking")
        .accessibilityIdentifier("thinking")
    }
}
