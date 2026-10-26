import SwiftUI
import PhotosUI
import UniformTypeIdentifiers

/// The composer's state: the draft, the attachment tray and the shared location. Owned by ChatView so a
/// drop anywhere on the chat lands in the same tray.
@Observable
@MainActor
final class ComposerModel {
    static let maxAttachments = 20

    var draft = ""
    var attachments: [PendingAttachment] = []
    var location: ShareMeta.Location?
    private(set) var loading = 0
    private(set) var locating = false
    private(set) var sending = false
    var error: String?
    @ObservationIgnored private var transcriptions: [UUID: Task<Void, Never>] = [:]

    var hasAttachments: Bool { !attachments.isEmpty || location != nil }

    // MARK: Adding

    func add(providers: [NSItemProvider]) {
        run {
            let loaded = try await AttachmentLoader.load(providers: providers)
            let extra = (loaded.texts + loaded.urls).joined(separator: "\n")
            if !extra.isEmpty { self.draft += (self.draft.isEmpty ? "" : "\n") + extra }
            return loaded.attachments
        }
    }

    func add(pickerItems: [PhotosPickerItem]) {
        run { try await AttachmentLoader.load(pickerItems: pickerItems) }
    }

    func add(fileURLs: [URL]) {
        run { try await AttachmentLoader.load(fileURLs: fileURLs) }
    }

    func add(cameraImage: UIImage) {
        run { [try await AttachmentLoader.load(cameraImage: cameraImage)] }
    }

    func add(cameraMovie: URL) {
        run { [try await AttachmentLoader.load(cameraMovie: cameraMovie)] }
    }

    func shareLocation() {
        guard !locating else { return }
        locating = true
        error = nil
        Task {
            let found = await OneShotLocation().current()
            locating = false
            if let found { location = found } else { error = String(localized: "Couldn't get your location.") }
        }
    }

    private func run(_ load: @escaping @MainActor () async throws -> [PendingAttachment]) {
        loading += 1
        error = nil
        Task {
            defer { loading -= 1 }
            do {
                let new = try await load()
                append(new)
            } catch let failure as ShareFailure {
                error = failure.localizedDescription
            } catch {
                self.error = String(localized: "Couldn't attach that.")
            }
        }
    }

    private func append(_ new: [PendingAttachment]) {
        var accepted = new
        let room = Self.maxAttachments - attachments.count
        if accepted.count > room {
            for extra in accepted[max(0, room)...] { try? FileManager.default.removeItem(at: extra.url) }
            accepted = Array(accepted.prefix(max(0, room)))
            error = String(localized: "Up to 20 files at a time.")
        }
        for var attachment in accepted {
            if attachment.kind == .audio {
                attachment.isTranscribing = true
                transcribe(attachment)
            }
            attachments.append(attachment)
        }
    }

    /// Audio is transcribed on the iPhone while it waits in the tray; the transcript rides in `meta`.
    private func transcribe(_ attachment: PendingAttachment) {
        let id = attachment.id
        let url = attachment.url
        transcriptions[id] = Task {
            let text = await AudioTranscriber.transcribe(url)
            if let index = attachments.firstIndex(where: { $0.id == id }) {
                attachments[index].transcript = text
                attachments[index].isTranscribing = false
            }
            transcriptions[id] = nil
        }
    }

    // MARK: Sending

    /// Waits for running transcripts (each gives up after 60 s), then hands over the tray.
    func takeAll() async -> (attachments: [PendingAttachment], location: ShareMeta.Location?) {
        sending = true
        defer { sending = false }
        for task in transcriptions.values { await task.value }
        let taken = (attachments, location)
        attachments = []
        location = nil
        return taken
    }

    func restore(_ taken: [PendingAttachment], location: ShareMeta.Location?) {
        attachments = taken + attachments
        self.location = self.location ?? location
    }
}

/// The message field with the attach menu, the tray above it and the send button. Return sends.
struct ChatComposer: View {
    @Environment(ChatStore.self) private var store
    @Bindable var model: ComposerModel
    @State private var focused = false
    @State private var showPhotos = false
    @State private var showFiles = false
    @State private var showCamera = false
    @State private var pickerItems: [PhotosPickerItem] = []
    @State private var voice = VoiceNoteController()

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            if !model.attachments.isEmpty {
                AttachmentTray(attachments: $model.attachments)
            }
            if model.locating || model.location != nil { locationChip }
            if model.loading > 0 {
                HStack(spacing: 6) {
                    ProgressView().controlSize(.small)
                    Text("Preparing attachments…").font(.caption).foregroundStyle(Theme.textSecondary)
                }
            }
            if let error = model.error ?? voice.error {
                Text(error).font(.caption).foregroundStyle(Theme.negative)
            }
            HStack(alignment: .bottom, spacing: 8) {
                ZStack {
                    // The field stays in place (and keeps the keyboard) under the recording bar.
                    HStack(alignment: .bottom, spacing: 8) {
                        attachMenu
                        ComposerTextView(text: $model.draft, focused: $focused, placeholder: String(localized: "Message Lucina"),
                                         onSubmit: send, onPaste: { model.add(providers: $0) })
                            .padding(.horizontal, 14)
                            .padding(.vertical, 10)
                            .background(Theme.card, in: RoundedRectangle(cornerRadius: 20, style: .continuous))
                    }
                    .opacity(voice.isRecording ? 0 : 1)
                    .allowsHitTesting(!voice.isRecording)
                    .accessibilityHidden(voice.isRecording)
                    if voice.isRecording {
                        VoiceRecordingBar(voice: voice, recorder: store.noteRecorder)
                            .transition(.opacity)
                    }
                }
                if showsMic {
                    VoiceMicButton(voice: voice)
                } else {
                    Button(action: send) {
                        Image(systemName: "arrow.up.circle.fill")
                            .font(.system(size: 34))
                            .symbolRenderingMode(.hierarchical)
                    }
                    .disabled(!canSend)
                    .accessibilityLabel("Send")
                    .accessibilityIdentifier("chatSend")
                }
            }
            .animation(.easeOut(duration: 0.15), value: voice.isRecording)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(.bar)
        .overlay(alignment: .topTrailing) {
            if model.draft.count > ChatStore.maxCharacters - 200 {
                Text("\(model.draft.count)/\(ChatStore.maxCharacters)")
                    .font(.caption2.monospacedDigit())
                    .foregroundStyle(model.draft.count > ChatStore.maxCharacters ? .red : .secondary)
                    .padding(.trailing, 60)
                    .offset(y: -14)
            }
        }
        .onAppear { voice.attach(store) }
        .onChange(of: store.prefill) { _, request in
            // A button asked for text in the composer: the owner finishes and sends it.
            guard let request else { return }
            // Empty (Ask Lucina widget): just the keyboard, keeping any draft.
            if !request.text.isEmpty {
                model.draft = request.text.hasSuffix(" ") ? request.text : request.text + " "
            }
            focused = true
        }
        .photosPicker(isPresented: $showPhotos, selection: $pickerItems, maxSelectionCount: 10,
                      matching: .any(of: [.images, .videos]), preferredItemEncoding: .current)
        .onChange(of: pickerItems) { _, items in
            guard !items.isEmpty else { return }
            model.add(pickerItems: items)
            pickerItems = []
        }
        .fileImporter(isPresented: $showFiles, allowedContentTypes: [.item], allowsMultipleSelection: true) { result in
            if case let .success(urls) = result, !urls.isEmpty { model.add(fileURLs: urls) }
        }
        .fullScreenCover(isPresented: $showCamera) {
            CameraPicker(onImage: { model.add(cameraImage: $0) }, onMovie: { model.add(cameraMovie: $0) })
                .ignoresSafeArea()
        }
    }

    private var attachMenu: some View {
        Menu {
            Button { showPhotos = true } label: { Label("Photos & videos", systemImage: "photo.on.rectangle") }
            Button { showFiles = true } label: { Label("Files", systemImage: "folder") }
            if UIImagePickerController.isSourceTypeAvailable(.camera) {
                Button { showCamera = true } label: { Label("Camera", systemImage: "camera") }
            }
            Button { model.shareLocation() } label: { Label("Location", systemImage: "location") }
            PasteButton(supportedContentTypes: [.item]) { providers in
                Task { @MainActor in model.add(providers: providers) }
            }
        } label: {
            Image(systemName: "plus.circle.fill")
                .font(.system(size: 30))
                .symbolRenderingMode(.hierarchical)
                .foregroundStyle(Theme.accent)
                .frame(height: 38)
        }
        .accessibilityLabel(Text("Attach"))
        .accessibilityIdentifier("chatAttach")
    }

    private var locationChip: some View {
        HStack(spacing: 6) {
            if model.locating {
                ProgressView().controlSize(.small)
                Text("Finding your location…")
            } else {
                Image(systemName: "location.fill").foregroundStyle(Theme.accent)
                Text(model.location?.name ?? String(localized: "Current location")).lineLimit(1)
                Button { model.location = nil } label: {
                    Image(systemName: "xmark.circle.fill").foregroundStyle(Theme.textSecondary)
                }
                .buttonStyle(.plain)
                .accessibilityLabel(Text("Remove location"))
            }
        }
        .font(.caption)
        .padding(.horizontal, 10)
        .padding(.vertical, 6)
        .background(Theme.card, in: Capsule())
    }

    /// An empty field and an empty tray: the send button is the mic (and stays it while a note records).
    private var showsMic: Bool {
        voice.isRecording || (ChatStore.clean(model.draft).isEmpty && !model.hasAttachments && model.loading == 0 && !model.locating)
    }

    private var canSend: Bool {
        let cleaned = ChatStore.clean(model.draft)
        guard cleaned.count <= ChatStore.maxCharacters, model.loading == 0, !model.sending, !model.locating else { return false }
        if model.hasAttachments { return true }
        return !cleaned.isEmpty && !store.isSending
    }

    private func send() {
        guard canSend else { return }
        let text = model.draft
        if !model.hasAttachments {
            model.draft = ""
            focused = true
            Task {
                if !(await store.send(text)) { model.draft = text }
            }
            return
        }
        model.draft = ""
        Task {
            let taken = await model.takeAll()
            if !store.sendShare(caption: text, attachments: taken.attachments, location: taken.location) {
                model.restore(taken.attachments, location: taken.location)
                if model.draft.isEmpty { model.draft = text }
                model.error = String(localized: "Couldn't send this. Try again.")
            }
        }
    }
}
