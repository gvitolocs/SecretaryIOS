import SwiftUI
import UniformTypeIdentifiers

/// The Chat tab, built from parts so other surfaces can reuse them:
/// `[stage] + ChatTranscript + ChatComposer`. The app passes no stage: Lucina's 3D avatar
/// lives in her own tab (Features/Lucina/LucinaTab.swift), so the chat loads no web view or
/// model. The transcript and composer only depend on `ChatStore`. Nothing here plays audio.
struct ChatView<Stage: View>: View {
    @Environment(ChatStore.self) private var store
    @Environment(AppModel.self) private var app
    private let stage: Stage
    @State private var composer = ComposerModel()
    @State private var actions = ChatActions()
    @State private var dropTargeted = false

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
            if actions.selecting {
                ChatSelectionBar()
                    .transition(.move(edge: .bottom).combined(with: .opacity))
            } else {
                ChatComposer(model: composer)
            }
        }
        .animation(.easeOut(duration: 0.2), value: actions.selecting)
        .background(Theme.page)
        // Files dragged onto the chat (iPad, Mac, or another app in Split View) land in the tray.
        .onDrop(of: [UTType.item], isTargeted: $dropTargeted) { providers in
            composer.add(providers: providers)
            return true
        }
        .overlay {
            if dropTargeted {
                RoundedRectangle(cornerRadius: Theme.cornerRadius, style: .continuous)
                    .strokeBorder(Theme.accent, style: StrokeStyle(lineWidth: 3, dash: [10, 6]))
                    .background(Theme.accentSoft.clipShape(RoundedRectangle(cornerRadius: Theme.cornerRadius, style: .continuous)))
                    .padding(8)
                    .allowsHitTesting(false)
            }
        }
        .overlay(alignment: .bottom) { ChatActionToast() }
        .overlay { ChatPrepareHUD() }
        #if DEBUG || SCREENSHOTS
        .overlay { DemoMessageMenu() }
        .task { await ChatDemo.apply(store: store, actions: actions) }
        #endif
        .animation(.easeOut(duration: 0.2), value: actions.toast)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .principal) { ChatTitle() }
        }
        // The transcript, the composer and the overlays all reach the actions.
        .environment(actions)
    }
}

extension ChatView where Stage == EmptyView {
    init() { self.init { EmptyView() } }
}

/// "Lucina". Connection state is shown once, by the subtle status dot in the toolbar.
struct ChatTitle: View {
    var body: some View {
        Text("Lucina").font(.headline)
    }
}

/// The scrolling list: day separators, bubbles with source badges, reminders, job cards,
/// and the thinking indicator. Scrolling to the top loads older messages.
struct ChatTranscript: View {
    @Environment(ChatStore.self) private var store
    @Environment(ChatActions.self) private var actions
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
                        case let .tools(steps):
                            ToolStepsRow(steps: steps).id("tools-" + steps[0].id)
                        case let .message(item):
                            MessageRow(item: item, job: store.jobs[item.id])
                                .modifier(MessageActionsModifier(item: item))
                                .id(item.id)
                                .onAppear {
                                    let source = store.messageSource
                                    let count = store.items.filter { !store.isHidden($0.id) }.count
                                    DispatchQueue.main.async {
                                        PerfLog.firstMessage(source: source, count: count)
                                    }
                                }
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
            .onChange(of: actions.scrollRequest) { _, request in
                guard let request else { return }
                Task { await showOriginal(request.id, proxy) }
            }
            .onAppear { scrollToEnd(proxy, animated: false) }
        }
    }

    enum Row: Identifiable {
        case day(Date)
        case message(TimelineItem)
        /// A run of Lucina's tool calls, shown as one line (ToolSteps).
        case tools([TimelineItem])

        var id: String {
            switch self {
            case let .day(date): return "day-\(date.timeIntervalSince1970)"
            case let .message(item): return item.id
            case let .tools(steps): return "tools-" + (steps.first?.id ?? "")
            }
        }
    }

    /// Items with a separator before each new calendar day; a turn's tool calls fold into one row.
    private var rows: [Row] {
        var out: [Row] = []
        var lastDay: Date?
        let calendar = Calendar.current
        for entry in ToolSteps.entries(store.items.filter { !store.isHidden($0.id) }) {
            let first: TimelineItem
            switch entry {
            case let .item(item): first = item
            case let .tools(steps): first = steps[0]
            }
            let day = calendar.startOfDay(for: first.ts)
            if day != lastDay {
                out.append(.day(day))
                lastDay = day
            }
            switch entry {
            case let .item(item): out.append(.message(item))
            case let .tools(steps): out.append(.tools(steps))
            }
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

    /// A quote was tapped: scrolls to the original (loading older pages if needed) and flashes it.
    private func showOriginal(_ id: String, _ proxy: ScrollViewProxy) async {
        if store.isHidden(id) {
            actions.show(String(localized: "That message is hidden."))
            return
        }
        var pages = 0
        while !store.items.contains(where: { $0.id == id }), store.hasOlder, pages < 5 {
            await store.loadOlder()
            pages += 1
        }
        guard store.items.contains(where: { $0.id == id }) else {
            actions.show(String(localized: "The original message isn't loaded."))
            return
        }
        await Task.yield()
        withAnimation(.easeInOut(duration: 0.3)) { proxy.scrollTo(id, anchor: .center) }
        actions.flash(id)
    }

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
            Image(systemName: item.source.symbol)
                .imageScale(.small)
                .accessibilityHidden(true)
            Text(item.source.label)
            Text("·").accessibilityHidden(true)
            Text(item.ts.formatted(date: .omitted, time: .shortened))
        }
        .font(.caption2)
        .foregroundStyle(.secondary)
        .lineLimit(1)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("From \(item.source.label), \(item.ts.formatted(date: .omitted, time: .shortened))")
    }
}

struct MessageRow: View {
    @Environment(ChatStore.self) private var store
    let item: TimelineItem
    let job: ChatJob?
    var readOnly = false

    var body: some View {
        content.disabled(readOnly).allowsHitTesting(!readOnly)
    }

    @ViewBuilder private var content: some View {
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
                badge.padding(.horizontal, 4)
            }
            Spacer(minLength: 24)
        }
        // `.contain` keeps the buttons' own identifiers (a bare identifier would override them).
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("lucinaRichMessage")
    }

    private var bubbleRow: some View {
        let isUser = item.role == .user
        let speaker = isUser && item.isMcp ? item.source.label : (isUser ? "You" : "Lucina")
        let source = isUser && item.isMcp ? "MCP" : item.source.label
        return HStack {
            if isUser { Spacer(minLength: 48) }
            VStack(alignment: isUser ? .trailing : .leading, spacing: 4) {
                // The message this one answers. Lucina's replies all point at the message before them, so
                // only the owner's replies show the quote.
                if isUser, let replyTo = item.replyTo {
                    ReplyQuoteView(replyTo: replyTo)
                }
                // With photos or videos the caption sits inside the media bubble (AttachmentMessage).
                if !item.displayText.isEmpty && !item.attachments.contains(where: \.isVisual) {
                    // Her plain replies are markdown too (the Notes renderer): no literal "**".
                    Text(isUser ? LinkText.attributed(item.displayText) : ChatMarkdown.inline(item.displayText))
                        .padding(.horizontal, 14)
                        .padding(.vertical, 10)
                        .foregroundStyle(isUser ? Color.white : Color.primary)
                        .tint(isUser ? Color.white : Theme.accent)
                        .background(isUser ? AnyShapeStyle(Theme.accentFill) : AnyShapeStyle(Theme.card),
                                    in: RoundedRectangle(cornerRadius: 18, style: .continuous))
                        .opacity(item.localStatus == .sending ? 0.7 : 1)
                        .accessibilityLabel(Text("\(speaker), via \(source): \(item.displayText)"))
                        .accessibilityIdentifier(isUser ? "userMessage" : "lucinaMessage")
                }
                if !item.attachments.isEmpty {
                    AttachmentMessage(item: item, isUser: isUser)
                }
                if let status = store.readingStatus(for: item) {
                    HStack(spacing: 6) {
                        ProgressView().controlSize(.mini)
                        Text(status.stage == "thinking" ? "Lucina is thinking…" : "Lucina is reading…")
                    }
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .accessibilityElement(children: .combine)
                    .accessibilityIdentifier("shareReading")
                }
                if let job {
                    JobCard(job: job)
                }
                if item.localStatus == .error {
                    HStack(spacing: 6) {
                        Image(systemName: "exclamationmark.circle.fill").foregroundStyle(.red).accessibilityHidden(true)
                        Text(item.localError ?? String(localized: "Not delivered."))
                            .font(.caption).foregroundStyle(.secondary)
                        Button("Retry") { Task { await store.retry(item.id) } }
                            .font(.caption.bold())
                            .disabled(store.isSending && item.meta?.isShare != true)
                            .accessibilityIdentifier("retryMessage")
                    }
                } else if item.localStatus == .sending {
                    HStack(spacing: 6) {
                        if let shareId = item.meta?.shareId, !item.attachments.contains(where: \.isVisual),
                           let progress = store.shareProgress[shareId] {
                            ProgressView(value: progress).progressViewStyle(.circular).controlSize(.mini)
                        }
                        Text("Sending…")
                    }
                    .font(.caption2).foregroundStyle(.secondary)
                } else {
                    badge.padding(.horizontal, 4)
                }
            }
            if !isUser { Spacer(minLength: 48) }
        }
    }

    /// The source line; on a reply Lucina wanted read aloud (a voice note's answer), the speaker to replay it.
    private var badge: some View {
        HStack(spacing: 2) {
            SourceBadge(item: item)
            if item.role == .lucina, item.meta?.voiceReply == true {
                VoiceReplyButton(item: item)
            }
        }
    }

    static func symbol(for kind: String) -> String {
        TimelineAttachment.symbol(for: kind)
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
                Text(LinkText.attributed(item.displayText)).font(.subheadline)
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
            Text(LinkText.attributed(item.displayText)).font(.subheadline)
            if let place = meta.place { PlaceCard(place: place) }
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
                Text(LinkText.attributed(result)).font(.subheadline)
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
