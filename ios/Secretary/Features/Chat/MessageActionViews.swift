import SwiftUI
import UIKit

/// The chat's message actions as UI state: multi-select, the short toast, the "preparing to share" HUD, the
/// quote-tap scroll and the flash on the original. Owned by ChatView, in the environment of the transcript
/// and the composer.
@Observable
@MainActor
final class ChatActions {
    struct Toast: Equatable {
        let text: String
        let token = UUID()
    }

    struct ScrollRequest: Equatable {
        let id: String
        let token = UUID()
    }

    private(set) var selecting = false
    private(set) var selected: Set<String> = []
    private(set) var toast: Toast?
    /// The share's preparation (downloads): 0...1, nil when nothing is shown. Shown only when it takes a moment.
    private(set) var preparing: Double?
    private(set) var flashId: String?
    private(set) var scrollRequest: ScrollRequest?
    #if DEBUG || SCREENSHOTS
    /// `-SecretaryChatDemo menu`: the message whose menu is drawn for a screenshot.
    var demoMenuItemId: String?
    #endif
    @ObservationIgnored private var prepareTask: Task<Void, Never>?
    @ObservationIgnored private var prepareToken: UUID?
    @ObservationIgnored private var progress: Double = 0
    @ObservationIgnored private var toastTask: Task<Void, Never>?
    @ObservationIgnored private var flashTask: Task<Void, Never>?

    // MARK: Feedback

    func show(_ text: String) {
        toast = Toast(text: text)
        UIAccessibility.post(notification: .announcement, argument: text)
        toastTask?.cancel()
        toastTask = Task { [weak self] in
            try? await Task.sleep(for: .seconds(2.2))
            guard !Task.isCancelled else { return }
            withAnimation(.easeOut(duration: 0.2)) { self?.toast = nil }
        }
    }

    /// The quote was tapped: the transcript scrolls to the original.
    func showOriginal(_ itemId: String) {
        scrollRequest = ScrollRequest(id: itemId)
    }

    /// The original lights up briefly after the scroll.
    func flash(_ itemId: String) {
        flashId = itemId
        flashTask?.cancel()
        flashTask = Task { [weak self] in
            try? await Task.sleep(for: .seconds(1.2))
            guard !Task.isCancelled else { return }
            withAnimation(.easeOut(duration: 0.4)) { self?.flashId = nil }
        }
    }

    // MARK: Selection

    func startSelecting(_ ids: [String]) {
        selecting = true
        selected = Set(ids)
        Haptics.light()
    }

    func toggle(_ itemId: String) {
        if selected.contains(itemId) { selected.remove(itemId) } else { selected.insert(itemId) }
        UISelectionFeedbackGenerator().selectionChanged()
    }

    func endSelecting() {
        selecting = false
        selected = []
    }

    // MARK: Actions

    func perform(_ action: MessageAction, on item: TimelineItem, store: ChatStore, app: AppModel) {
        switch action {
        case .reply:
            guard store.beginReply(item) else {
                show(String(localized: "This message is still being sent."))
                return
            }
            // Brings up the keyboard and keeps the draft.
            store.requestPrefill("")
        case .readAloud:
            Task { await readAloud(item, store: store) }
        case .copy:
            UIPasteboard.general.string = copyText(item, store: store)
            show(String(localized: "Copied"))
        case .share:
            let parts = ChatShareBuilder.parts(for: item, transcript: store.voiceTranscript(of: item))
            prepare(parts, store: store, app: app) { items in
                if !ChatSharePresenter.present(items) { self.show(String(localized: "Couldn't prepare the share.")) }
            }
        case .saveToPhotos:
            let parts = ChatShareBuilder.parts(for: item, transcript: nil).filter {
                if case let .media(media) = $0 { return media.kind == .image }
                return false
            }
            prepare(parts, store: store, app: app, minimumDelay: false) { items in
                var saved = 0
                for file in items.compactMap(\.fileURL) {
                    if await PhotoSaver.save(file: file, image: UIImage(contentsOfFile: file.path)) { saved += 1 }
                }
                self.show(saved > 0 ? String(localized: "Saved to Photos") : String(localized: "Couldn't save to Photos"))
            }
        case .saveToLibrary, .saveAsNote:
            let intent = action.intent ?? "library"
            let parts = ChatShareBuilder.parts(for: item, transcript: store.voiceTranscript(of: item))
            prepare(parts, store: store, app: app, minimumDelay: false) { items in
                let texts = items.compactMap { item -> String? in if case let .text(text) = item.payload { return text } else { return nil } }
                let urls = items.compactMap { item -> String? in if case let .url(url) = item.payload { return url.absoluteString } else { return nil } }
                let files = items.compactMap { item in item.fileURL.map { ShareFile(url: $0, name: $0.lastPathComponent, mime: item.mime) } }
                let ok = await store.forward(intent: intent, texts: texts, urls: urls, files: files)
                if !ok { self.show(String(localized: "Couldn't send this to Lucina.")) }
                else if action == .saveAsNote { self.show(String(localized: "Sent to Lucina as a note")) }
                else { self.show(String(localized: "Sent to Lucina's library")) }
            }
        case .select:
            startSelecting([item.id])
        case .hide:
            store.hide(item.id)
            selected.remove(item.id)
            show(String(localized: "Message hidden"))
        }
    }

    func readAloud(_ item: TimelineItem, store: ChatStore) async {
        switch await store.toggleReadAloud(item) {
        case let .refusedRecording(reason): show(reason)
        case .nothingToRead: show(String(localized: "Nothing to read aloud."))
        case let .failed(message): show(message)
        case .started, .stopped: break
        }
    }

    /// "Share conversation": the selected messages as one transcript plus their files.
    func shareSelection(store: ChatStore, app: AppModel) {
        let chosen = store.items.filter { selected.contains($0.id) }
        guard !chosen.isEmpty else { return }
        let parts = ChatShareBuilder.conversationParts(chosen, userName: ChatShareBuilder.ownerName(deviceName: app.deviceName)) {
            store.voiceTranscript(of: $0)
        }
        prepare(parts, store: store, app: app) { items in
            if ChatSharePresenter.present(items, completion: { self.endSelecting() }) == false {
                self.show(String(localized: "Couldn't prepare the share."))
            }
        }
    }

    func copyText(_ item: TimelineItem, store: ChatStore) -> String {
        var parts = [ChatPlainText.body(of: item, includeLinks: true)]
        if let transcript = store.voiceTranscript(of: item) { parts.append(transcript) }
        return parts.filter { !$0.isEmpty }.joined(separator: "\n\n")
    }

    /// Resolves the parts (downloads), showing the HUD after 0.4 s, then hands the items over. The HUD's
    /// Cancel stops the downloads. `minimumDelay` waits for the long-press menu to finish closing before a
    /// sheet is presented.
    private func prepare(_ parts: [ChatSharePart], store: ChatStore, app: AppModel, minimumDelay: Bool = true,
                         then: @escaping @MainActor ([ChatShareItem]) async -> Void) {
        guard !parts.isEmpty else {
            show(String(localized: "Nothing to share."))
            return
        }
        cancelPreparing()
        let token = UUID()
        prepareToken = token
        progress = 0
        let started = Date()
        prepareTask = Task { [weak self] in
            guard let self else { return }
            let hud = Task { @MainActor [weak self] in
                try? await Task.sleep(for: .milliseconds(400))
                guard !Task.isCancelled, let self, self.prepareToken == token else { return }
                withAnimation(.easeOut(duration: 0.15)) { self.preparing = self.progress }
            }
            defer {
                hud.cancel()
                if prepareToken == token {
                    withAnimation(.easeOut(duration: 0.15)) { self.preparing = nil }
                    prepareTask = nil
                    prepareToken = nil
                }
            }
            do {
                let items = try await ChatShareResolver.resolve(parts, store: store, client: app.client) { [weak self] value in
                    guard let self, self.prepareToken == token else { return }
                    self.progress = value
                    if self.preparing != nil { self.preparing = value }
                }
                guard !Task.isCancelled, prepareToken == token else { return }
                guard !items.isEmpty else {
                    show(String(localized: "Nothing to share."))
                    return
                }
                let elapsed = Date().timeIntervalSince(started)
                if minimumDelay, elapsed < 0.45 { try? await Task.sleep(for: .milliseconds(Int((0.45 - elapsed) * 1000))) }
                guard !Task.isCancelled else { return }
                await then(items)
            } catch is CancellationError {
                return
            } catch {
                if !Task.isCancelled {
                    show((error as? LocalizedError)?.errorDescription ?? String(localized: "Couldn't prepare the share."))
                }
            }
        }
    }

    func cancelPreparing() {
        prepareTask?.cancel()
        prepareTask = nil
        prepareToken = nil
        preparing = nil
    }
}

// MARK: - Per message: swipes, long-press menu, selection, VoiceOver

/// Swipe left → reply, swipe right → read aloud, long-press → the menu with a preview, VoiceOver custom
/// actions, and in selection mode a checkmark and tap-to-select.
struct MessageActionsModifier: ViewModifier {
    @Environment(ChatStore.self) private var store
    @Environment(AppModel.self) private var app
    @Environment(ChatActions.self) private var actions
    let item: TimelineItem
    @State private var track: BubbleSwipe.Track = .undecided
    @State private var armed = false
    @GestureState private var dragging = false

    func body(content: Content) -> some View {
        let speech = store.speechText(for: item)
        let available = MessageAction.available(for: item, speech: speech)
        let canReply = available.contains(.reply) && !actions.selecting
        let canRead = available.contains(.readAloud) && !actions.selecting
        let offset = BubbleSwipe.offset(track, canReply: canReply, canReadAloud: canRead)
        let isUser = item.role == .user
        let selected = actions.selected.contains(item.id)
        return content
            .offset(x: offset)
            .overlay(alignment: isUser ? .topTrailing : .topLeading) { ReadAloudIndicator(itemId: item.id).offset(y: -10) }
            .background(alignment: .trailing) {
                if offset < 0 { swipeIcon("arrowshape.turn.up.left.fill", progress: BubbleSwipe.progress(offset)) }
            }
            .background(alignment: .leading) {
                if offset > 0 { swipeIcon("speaker.wave.2.fill", progress: BubbleSwipe.progress(offset)) }
            }
            .padding(.leading, actions.selecting ? 36 : 0)
            .overlay(alignment: .leading) {
                if actions.selecting {
                    Image(systemName: selected ? "checkmark.circle.fill" : "circle")
                        .font(.title3)
                        .foregroundStyle(selected ? Theme.accent : Theme.textSecondary)
                        .accessibilityHidden(true)
                }
            }
            .overlay {
                if actions.selecting {
                    Color.clear
                        .contentShape(Rectangle())
                        .onTapGesture { actions.toggle(item.id) }
                        .accessibilityElement()
                        .accessibilityLabel(Text(ChatQuote.sender(item) + ": " + ChatQuote.summary(item)))
                        .accessibilityAddTraits(selected ? [.isButton, .isSelected] : .isButton)
                        .accessibilityIdentifier("selectMessage")
                }
            }
            .background {
                if actions.flashId == item.id {
                    RoundedRectangle(cornerRadius: Theme.cornerRadius, style: .continuous)
                        .fill(Theme.accentSoft)
                        .padding(-6)
                        .transition(.opacity)
                }
            }
            .simultaneousGesture(swipe(canReply: canReply, canRead: canRead), including: actions.selecting ? .subviews : .all)
            .onChange(of: dragging) { _, down in
                // The system cancelled the touch (the scroll view took it): back to rest without acting.
                guard !down, track != .undecided else { return }
                withAnimation(.spring(response: 0.3, dampingFraction: 0.8)) { track = .undecided }
                armed = false
            }
            .contextMenu {
                if !actions.selecting {
                    ForEach(available) { action in
                        Button(role: action == .hide ? .destructive : nil) {
                            actions.perform(action, on: item, store: store, app: app)
                        } label: {
                            Label(title(action), systemImage: action == .readAloud && store.readAloud.isActive(item.id) ? "stop.fill" : action.symbol)
                        }
                    }
                }
            } preview: {
                MessagePreview(item: item, client: app.client, transcript: store.voiceTranscript(of: item))
            }
            .accessibilityActions {
                if canReply {
                    Button(String(localized: "Reply")) { actions.perform(.reply, on: item, store: store, app: app) }
                }
                if canRead {
                    Button(store.readAloud.isActive(item.id) ? String(localized: "Stop reading") : String(localized: "Read aloud")) {
                        actions.perform(.readAloud, on: item, store: store, app: app)
                    }
                }
                Button(String(localized: "Share")) { actions.perform(.share, on: item, store: store, app: app) }
            }
    }

    private func title(_ action: MessageAction) -> String {
        if action == .readAloud, store.readAloud.isActive(item.id) { return String(localized: "Stop reading") }
        return MessageActionTitle.text(action)
    }

    private func swipe(canReply: Bool, canRead: Bool) -> some Gesture {
        DragGesture(minimumDistance: BubbleSwipe.activation, coordinateSpace: .global)
            .updating($dragging) { _, state, _ in state = true }
            .onChanged { value in
                let next = BubbleSwipe.track(track, translation: value.translation)
                if next != track { track = next }
                let ready = BubbleSwipe.action(next, canReply: canReply, canReadAloud: canRead) != .none
                if ready != armed {
                    armed = ready
                    if ready { UIImpactFeedbackGenerator(style: .medium).impactOccurred() }
                }
            }
            .onEnded { value in
                let final = BubbleSwipe.track(track, translation: value.translation)
                let action = BubbleSwipe.action(final, canReply: canReply, canReadAloud: canRead)
                withAnimation(.spring(response: 0.3, dampingFraction: 0.8)) { track = .undecided }
                armed = false
                switch action {
                case .reply: actions.perform(.reply, on: item, store: store, app: app)
                case .readAloud: actions.perform(.readAloud, on: item, store: store, app: app)
                case .none: break
                }
            }
    }

    private func swipeIcon(_ symbol: String, progress: Double) -> some View {
        Image(systemName: symbol)
            .font(.system(size: 15, weight: .semibold))
            .foregroundStyle(progress >= 1 ? Theme.onAccent : Theme.accent)
            .frame(width: 32, height: 32)
            .background(progress >= 1 ? AnyShapeStyle(Theme.accentFill) : AnyShapeStyle(Theme.accentSoft), in: Circle())
            .scaleEffect(0.6 + 0.4 * progress)
            .opacity(progress)
            .padding(.horizontal, 6)
            .accessibilityHidden(true)
    }
}

/// The menu's labels (the menu, VoiceOver and the screenshot demo share them).
enum MessageActionTitle {
    static func text(_ action: MessageAction) -> String {
        switch action {
        case .reply: return String(localized: "Reply")
        case .readAloud: return String(localized: "Read aloud")
        case .copy: return String(localized: "Copy text")
        case .share: return String(localized: "Share…")
        case .saveToPhotos: return String(localized: "Save to Photos")
        case .saveToLibrary: return String(localized: "Save to library")
        case .saveAsNote: return String(localized: "Save as note")
        case .select: return String(localized: "Select")
        case .hide: return String(localized: "Hide")
        }
    }
}

/// The speaker on a bubble Lucina is reading aloud (a spinner while her voice loads); tap to stop.
struct ReadAloudIndicator: View {
    @Environment(ChatStore.self) private var store
    let itemId: String

    var body: some View {
        let reader = store.readAloud
        if reader.isActive(itemId) {
            Button { reader.stop() } label: {
                Group {
                    if reader.loadingId == itemId {
                        ProgressView().controlSize(.mini).tint(Theme.onAccent)
                    } else {
                        Image(systemName: "speaker.wave.2.fill")
                            .symbolEffect(.variableColor.iterative, isActive: true)
                    }
                }
                .font(.caption.weight(.semibold))
                .foregroundStyle(Theme.onAccent)
                .frame(width: 30, height: 24)
                .background(Theme.accentFill, in: Capsule())
                .shadow(color: .black.opacity(0.25), radius: 3, y: 1)
            }
            .buttonStyle(.plain)
            .transition(.scale.combined(with: .opacity))
            .accessibilityLabel(Text("Stop reading"))
            .accessibilityIdentifier("readAloudIndicator")
        }
    }
}

// MARK: - Quotes

/// The quote on top of a reply: accent bar, who wrote the original and one or two lines; tap scrolls to it.
struct ReplyQuoteView: View {
    @Environment(ChatStore.self) private var store
    @Environment(ChatActions.self) private var actions
    let replyTo: String

    var body: some View {
        let original = store.items.first { $0.id == replyTo }
        let sender = original.map(ChatQuote.sender) ?? String(localized: "Reply")
        let summary = original.map(ChatQuote.summary) ?? String(localized: "Message not loaded")
        Button { actions.showOriginal(replyTo) } label: {
            HStack(spacing: 8) {
                RoundedRectangle(cornerRadius: 2, style: .continuous)
                    .fill(Theme.accent)
                    .frame(width: 3)
                VStack(alignment: .leading, spacing: 2) {
                    Text(sender).font(.caption.weight(.semibold)).foregroundStyle(Theme.accent)
                    Text(summary)
                        .font(.caption)
                        .foregroundStyle(Theme.textSecondary)
                        .lineLimit(2)
                        .multilineTextAlignment(.leading)
                }
                Spacer(minLength: 0)
            }
            .fixedSize(horizontal: false, vertical: true)
            .padding(.horizontal, 10)
            .padding(.vertical, 7)
            .frame(maxWidth: 260, alignment: .leading)
            .background(Theme.accentSoft, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
        }
        .buttonStyle(.plain)
        .accessibilityLabel(Text("Reply to \(sender): \(summary)"))
        .accessibilityHint(Text("Shows the original message"))
        .accessibilityIdentifier("replyQuote")
    }
}

/// Above the composer while replying: the quoted message and ✕ to cancel.
struct ComposerReplyBar: View {
    let item: TimelineItem
    let onCancel: () -> Void

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: "arrowshape.turn.up.left.fill")
                .font(.subheadline)
                .foregroundStyle(Theme.accent)
                .accessibilityHidden(true)
            RoundedRectangle(cornerRadius: 2, style: .continuous)
                .fill(Theme.accent)
                .frame(width: 3)
            VStack(alignment: .leading, spacing: 2) {
                Text("Reply to \(ChatQuote.sender(item))")
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(Theme.accent)
                Text(ChatQuote.summary(item))
                    .font(.caption)
                    .foregroundStyle(Theme.textSecondary)
                    .lineLimit(2)
            }
            Spacer(minLength: 4)
            Button(action: onCancel) {
                Image(systemName: "xmark.circle.fill")
                    .font(.title3)
                    .symbolRenderingMode(.hierarchical)
                    .foregroundStyle(Theme.textSecondary)
            }
            .buttonStyle(.plain)
            .accessibilityLabel(Text("Cancel reply"))
            .accessibilityIdentifier("cancelReply")
        }
        .fixedSize(horizontal: false, vertical: true)
        .padding(.horizontal, 10)
        .padding(.vertical, 6)
        .background(Theme.card, in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("replyPreview")
    }
}

// MARK: - Selection bar, toast, HUD

/// In place of the composer while selecting: Cancel, the count and "Share conversation".
struct ChatSelectionBar: View {
    @Environment(ChatStore.self) private var store
    @Environment(AppModel.self) private var app
    @Environment(ChatActions.self) private var actions

    var body: some View {
        HStack(spacing: 12) {
            Button("Cancel") { actions.endSelecting() }
                .accessibilityIdentifier("selectionCancel")
            Spacer(minLength: 8)
            Text("\(actions.selected.count) selected")
                .font(.subheadline.weight(.semibold))
                .monospacedDigit()
                .accessibilityIdentifier("selectionCount")
            Spacer(minLength: 8)
            Button {
                actions.shareSelection(store: store, app: app)
            } label: {
                Label("Share conversation", systemImage: "square.and.arrow.up")
                    .labelStyle(.titleAndIcon)
                    .font(.subheadline.weight(.semibold))
            }
            .disabled(actions.selected.isEmpty)
            .accessibilityIdentifier("selectionShare")
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 12)
        .frame(maxWidth: .infinity)
        .background(.bar)
        .tint(Theme.accent)
    }
}

struct ChatActionToast: View {
    @Environment(ChatActions.self) private var actions

    var body: some View {
        if let toast = actions.toast {
            Text(toast.text)
                .font(.footnote.weight(.semibold))
                .multilineTextAlignment(.center)
                .padding(.horizontal, 14)
                .padding(.vertical, 8)
                .background(.regularMaterial, in: Capsule())
                .padding(.bottom, 88)
                .padding(.horizontal, 24)
                .transition(.opacity.combined(with: .move(edge: .bottom)))
                .id(toast.token)
                .allowsHitTesting(false)
                .accessibilityIdentifier("chatToast")
        }
    }
}

/// "Preparing…" with the downloads' progress and Cancel, while a share takes a moment.
struct ChatPrepareHUD: View {
    @Environment(ChatActions.self) private var actions

    var body: some View {
        if let progress = actions.preparing {
            VStack(spacing: 10) {
                ProgressView(value: progress)
                    .progressViewStyle(.circular)
                    .controlSize(.large)
                Text("Preparing to share…").font(.subheadline.weight(.semibold))
                Button("Cancel", role: .cancel) { actions.cancelPreparing() }
                    .font(.subheadline)
                    .accessibilityIdentifier("shareCancel")
            }
            .padding(20)
            .frame(minWidth: 180)
            .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 18, style: .continuous))
            .transition(.opacity)
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier("shareHUD")
        }
    }
}

// MARK: - Long-press preview

/// The bubble as the long-press menu lifts it. Self-contained (the preview is hosted outside the chat's
/// environment): the first picture, who wrote it and when, and the words.
struct MessagePreview: View {
    let item: TimelineItem
    let client: APIClient
    let transcript: String?
    @State private var image: UIImage?

    private var firstImage: BubbleEntry? { BubbleEntry.entries(item.attachments).first { $0.attachment.isImage } }

    private var words: String {
        let body = ChatPlainText.body(of: item, includeLinks: true)
        if !body.isEmpty { return body }
        if let transcript, !transcript.isEmpty { return transcript }
        return ChatQuote.summary(item)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            if let shown = image ?? firstImage?.attachment.thumbhash.flatMap(ThumbHash.image(base64:)) {
                Image(uiImage: shown)
                    .resizable()
                    .scaledToFit()
                    .frame(maxWidth: .infinity, maxHeight: 320)
                    .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
            }
            HStack {
                Text(ChatQuote.sender(item)).font(.caption.weight(.semibold)).foregroundStyle(Theme.accent)
                Spacer()
                Text(item.ts.formatted(date: .abbreviated, time: .shortened)).font(.caption2).foregroundStyle(Theme.textSecondary)
            }
            Text(words)
                .font(.body)
                .foregroundStyle(Theme.text)
                .lineLimit(14)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
        .padding(14)
        .frame(width: 320, alignment: .leading)
        .background(Theme.card)
        .environment(\.colorScheme, .dark)
        .task { await load() }
    }

    private func load() async {
        guard image == nil, let entry = firstImage else { return }
        if let local = entry.fileIndex.flatMap({ ChatPreviews.image(shareId: item.meta?.shareId, index: $0) }) {
            image = local
            return
        }
        guard let id = entry.attachment.mediaId else { return }
        image = await ChatMedia.image(id, rendition: .grid, info: nil, client: client)
    }
}

// MARK: - Screenshot states

#if DEBUG || SCREENSHOTS
/// `-SecretaryChatDemo replyPreview|menu|selection`: puts the chat in that state for a screenshot once the
/// timeline has server items (the quote bar on Lucina's newest message; the long-press menu's actions drawn
/// over the newest message; multi-select with the two newest selected).
@MainActor
enum ChatDemo {
    static func apply(store: ChatStore, actions: ChatActions, defaults: UserDefaults = .standard) async {
        guard let mode = defaults.string(forKey: "SecretaryChatDemo")?.trimmingCharacters(in: .whitespaces).lowercased(),
              !mode.isEmpty else { return }
        let deadline = Date().addingTimeInterval(20)
        while Date() < deadline, !store.items.contains(where: { !$0.isLocal }) {
            try? await Task.sleep(for: .milliseconds(300))
        }
        try? await Task.sleep(for: .milliseconds(600))
        let visible = store.items.filter { !$0.isLocal && !store.isHidden($0.id) }
        guard let newest = visible.last else { return }
        let lucina = visible.last { $0.role == .lucina } ?? newest
        switch mode {
        case "replypreview":
            store.beginReply(lucina)
        case "selection":
            actions.startSelecting(visible.suffix(2).map(\.id))
        case "menu":
            actions.demoMenuItemId = lucina.id
        default:
            break
        }
    }
}

/// The long-press menu as drawn for the `menu` screenshot (a real context menu can't be opened from code):
/// the same preview and the same actions, over a dimmed chat. Tap anywhere to close.
struct DemoMessageMenu: View {
    @Environment(ChatStore.self) private var store
    @Environment(AppModel.self) private var app
    @Environment(ChatActions.self) private var actions

    var body: some View {
        if let id = actions.demoMenuItemId, let item = store.items.first(where: { $0.id == id }) {
            ZStack {
                Rectangle().fill(.ultraThinMaterial).ignoresSafeArea()
                VStack(alignment: .leading, spacing: 12) {
                    MessagePreview(item: item, client: app.client, transcript: store.voiceTranscript(of: item))
                        .clipShape(RoundedRectangle(cornerRadius: 16, style: .continuous))
                    VStack(spacing: 0) {
                        let available = MessageAction.available(for: item, speech: store.speechText(for: item))
                        ForEach(available) { action in
                            HStack {
                                Text(MessageActionTitle.text(action))
                                Spacer()
                                Image(systemName: action.symbol)
                            }
                            .foregroundStyle(action == .hide ? Theme.negative : Theme.text)
                            .padding(.horizontal, 16)
                            .padding(.vertical, 11)
                            if action != available.last { Divider() }
                        }
                    }
                    .frame(width: 250)
                    .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 14, style: .continuous))
                }
            }
            .onTapGesture { actions.demoMenuItemId = nil }
            .accessibilityIdentifier("demoMessageMenu")
        }
    }
}
#endif
