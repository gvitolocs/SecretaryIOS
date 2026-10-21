import SwiftUI

struct ChatView: View {
    @Environment(ChatStore.self) private var store
    @Environment(AppModel.self) private var app
    @State private var draft = ""
    @FocusState private var composerFocused: Bool

    var body: some View {
        VStack(spacing: 0) {
            if let notice = app.lucinaNotice {
                ErrorBanner(message: notice, systemImage: "moon.zzz.fill") { app.lucinaNotice = nil }
                    .padding(.horizontal, 12)
                    .padding(.top, 6)
            }
            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(spacing: 10) {
                        if store.messages.isEmpty {
                            emptyState
                        }
                        ForEach(store.messages) { message in
                            MessageRow(message: message)
                                .id(message.id)
                        }
                        if store.isSending {
                            ThinkingBubble().id("thinking")
                        }
                    }
                    .padding(.horizontal, 12)
                    .padding(.vertical, 12)
                }
                .scrollDismissesKeyboard(.interactively)
                .defaultScrollAnchor(.bottom)
                .onChange(of: store.messages.count) { _, _ in scrollToEnd(proxy) }
                .onChange(of: store.isSending) { _, _ in scrollToEnd(proxy) }
                .onAppear { scrollToEnd(proxy, animated: false) }
            }
            composer
        }
        .background(Theme.page)
        .navigationTitle("Lucina")
        .navigationBarTitleDisplayMode(.inline)
        .task { store.resumePolling() }
    }

    private var emptyState: some View {
        VStack(spacing: 12) {
            Image(systemName: "sparkles")
                .font(.system(size: 40))
                .foregroundStyle(Theme.accent)
                .accessibilityHidden(true)
            Text("Ask Lucina anything").font(.title3.bold())
            Text("Reminders, the shopping list, errands. She remembers where you left off.")
                .font(.subheadline)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
        }
        .padding(.top, 80)
        .padding(.horizontal, 24)
        .accessibilityElement(children: .combine)
    }

    private var composer: some View {
        HStack(alignment: .bottom, spacing: 8) {
            TextField("Message Lucina", text: $draft, axis: .vertical)
                .lineLimit(1...6)
                .submitLabel(.send)
                .onSubmit(send)
                .focused($composerFocused)
                .padding(.horizontal, 14)
                .padding(.vertical, 10)
                .background(Theme.card, in: RoundedRectangle(cornerRadius: 20, style: .continuous))
                .accessibilityIdentifier("chatComposer")
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
        composerFocused = true
        Task {
            if !(await store.send(text)) { draft = text }
        }
    }

    private func scrollToEnd(_ proxy: ScrollViewProxy, animated: Bool = true) {
        let target = store.isSending ? "thinking" : store.messages.last?.id
        guard let target else { return }
        if animated {
            withAnimation(.easeOut(duration: 0.2)) { proxy.scrollTo(target, anchor: .bottom) }
        } else {
            proxy.scrollTo(target, anchor: .bottom)
        }
    }
}

private struct MessageRow: View {
    @Environment(ChatStore.self) private var store
    let message: ChatMessage

    var body: some View {
        let isUser = message.role == .user
        HStack {
            if isUser { Spacer(minLength: 48) }
            VStack(alignment: isUser ? .trailing : .leading, spacing: 6) {
                if !message.text.isEmpty {
                    Text(LinkText.attributed(message.text))
                        .textSelection(.enabled)
                        .padding(.horizontal, 14)
                        .padding(.vertical, 10)
                        .foregroundStyle(isUser ? Color.white : Color.primary)
                        .tint(isUser ? Color.white : Theme.accent)
                        .background(isUser ? AnyShapeStyle(Theme.accent) : AnyShapeStyle(Theme.card),
                                    in: RoundedRectangle(cornerRadius: 18, style: .continuous))
                        .opacity(message.status == .sending ? 0.7 : 1)
                        .accessibilityLabel(Text("\(isUser ? "You" : "Lucina"): \(message.text)"))
                        .accessibilityIdentifier(isUser ? "userMessage" : "lucinaMessage")
                }
                if let job = message.job {
                    JobCard(job: job)
                }
                if message.status == .error {
                    HStack(spacing: 6) {
                        Image(systemName: "exclamationmark.circle.fill").foregroundStyle(.red).accessibilityHidden(true)
                        Text(message.error ?? "Not delivered.").font(.caption).foregroundStyle(.secondary)
                        Button("Retry") { Task { await store.retry(message.id) } }
                            .font(.caption.bold())
                            .disabled(store.isSending)
                            .accessibilityIdentifier("retryMessage")
                    }
                } else if message.status == .sending {
                    Text("Sending…").font(.caption2).foregroundStyle(.secondary)
                }
            }
            if !isUser { Spacer(minLength: 48) }
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
                Text(job.error == "interrupted" ? "Lucina restarted while working on this." : (job.error ?? "The task failed."))
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
