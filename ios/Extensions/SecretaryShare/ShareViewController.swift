import SwiftUI
import UIKit
import LinkPresentation
import Intents

final class ShareViewController: UIViewController {
    override func viewDidLoad() {
        super.viewDidLoad()
        let model = ShareSheetModel()
        model.context = extensionContext
        // INSendMessageIntent suggestions use the same input-item path as ordinary shares.
        let host = UIHostingController(rootView: ShareSheet(model: model))
        addChild(host)
        view.addSubview(host.view)
        host.view.translatesAutoresizingMaskIntoConstraints = false
        NSLayoutConstraint.activate([host.view.topAnchor.constraint(equalTo: view.topAnchor),
                                     host.view.bottomAnchor.constraint(equalTo: view.bottomAnchor),
                                     host.view.leadingAnchor.constraint(equalTo: view.leadingAnchor),
                                     host.view.trailingAnchor.constraint(equalTo: view.trailingAnchor)])
        host.didMove(toParent: self)
        preferredContentSize = CGSize(width: 420, height: 440)
        Task { await model.load() }
    }
}

@MainActor
final class ShareSheetModel: ObservableObject {
    weak var context: NSExtensionContext?
    @Published var payload = SharePayload()
    /// The files, as the tray shows them (the owner can remove some before sending).
    @Published var attachments: [PendingAttachment] = []
    @Published var note = ""
    @Published var error: String?
    @Published var loaded = false
    @Published var sending = false
    @Published var sent = false
    /// Foreground upload progress (no App Group yet), 0...1.
    @Published var progress: Double?
    /// Big payload and no background upload: the sheet must stay open.
    @Published var keepOpenWarning = false
    private var staging: URL?
    var signedIn: Bool { BridgeCredentials.read() != nil }

    func load() async {
        guard signedIn else { error = ShareFailure.signIn.localizedDescription; return }
        do {
            let directory = try ShareRequestBuilder.directory().appendingPathComponent(UUID().uuidString, isDirectory: true)
            staging = directory
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
            let items = context?.inputItems.compactMap { $0 as? NSExtensionItem } ?? []
            payload = try await ShareItemReader.read(items, directory: directory)
            var read: [PendingAttachment] = []
            for file in payload.files { read.append(await PendingAttachment.make(from: file)) }
            attachments = read
            let size = try payload.files.reduce(Int64(0)) { total, file in
                total + Int64(try file.url.resourceValues(forKeys: [.fileSizeKey]).fileSize ?? 0)
            }
            guard size <= ShareRequestBuilder.maxBytes else { throw ShareFailure.tooLarge }
            keepOpenWarning = !ShareRequestBuilder.hasAppGroup && size > ShareRequestBuilder.foregroundWarningBytes
            loaded = !payload.files.isEmpty || !payload.urls.isEmpty || !payload.texts.isEmpty
            if !loaded { throw ShareFailure.empty }
            DebugEvents.record("share.open", ["files": payload.files.count, "links": payload.urls.count, "snippets": payload.texts.count])
        } catch {
            DebugEvents.record("share.open", ["ok": false, "error": error])
            self.error = error.localizedDescription
        }
    }

    func close() {
        DebugEvents.record("share.close", ["sent": sent])
        DebugEvents.flushSoon()
        if let staging { try? FileManager.default.removeItem(at: staging) }
        context?.completeRequest(returningItems: nil)
    }

    var hasContent: Bool { !attachments.isEmpty || !payload.urls.isEmpty || !payload.texts.isEmpty }

    func send(_ intent: String) {
        guard !sending, loaded, hasContent else { return }
        sending = true
        DebugEvents.record("share.send", ["action": intent, "files": attachments.count, "background": ShareRequestBuilder.hasAppGroup])
        var payload = payload
        payload.files = attachments.map(\.shareFile)
        let meta = PendingAttachment.meta(for: attachments)
        let note = note
        Task {
            do {
                guard let credentials = BridgeCredentials.read() else { throw ShareFailure.signIn }
                // File I/O and multipart streaming run away from the UI thread.
                let record = try await Task.detached {
                    try ShareRequestBuilder.build(payload: payload, intent: intent, note: note, source: "ios-share",
                                                  credentials: credentials, directory: ShareRequestBuilder.directory(), meta: meta)
                }.value
                if ShareRequestBuilder.hasAppGroup {
                    try ShareUploads.shared.enqueue(record, credentials: credentials)
                } else {
                    progress = 0
                    // A little extra time if the owner leaves the sheet while it uploads.
                    let activity = ProcessInfo.processInfo.beginActivity(options: [.userInitiated, .idleSystemSleepDisabled],
                                                                         reason: "Sending a share to Lucina")
                    defer { ProcessInfo.processInfo.endActivity(activity) }
                    try await ShareUploads.shared.sendNow(record, credentials: credentials) { fraction in
                        Task { @MainActor in self.progress = fraction }
                    }
                }
                LucinaDonations.donate()
                DebugEvents.record("share.sent", ["action": intent])
                sent = true
                try? await Task.sleep(for: .milliseconds(600))
                close()
            } catch {
                DebugEvents.record("share.sent", ["action": intent, "ok": false, "error": error])
                self.error = error.localizedDescription; sending = false; progress = nil
            }
        }
    }
}

private struct ShareSheet: View {
    @ObservedObject var model: ShareSheetModel
    private let actions = [("Save to library", "library"), ("Add to shopping", "shopping"), ("Save as note", "note"), ("Remind me", "remind")]
    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                HStack {
                    if let image = LucinaDonations.avatar { Image(uiImage: image).resizable().frame(width: 40, height: 40).clipShape(RoundedRectangle(cornerRadius: 12)) }
                    Text("Lucina").font(.title2.weight(.semibold))
                    Spacer()
                    Button(LocalizedStringKey(model.signedIn ? "Cancel" : "Close")) { model.close() }.disabled(model.sending)
                }
                if let error = model.error { Text(error).foregroundStyle(.secondary) }
                if model.loaded {
                    VStack(alignment: .leading, spacing: 12) {
                        if !model.attachments.isEmpty {
                            AttachmentTray(attachments: $model.attachments)
                        }
                        ForEach(Array(model.payload.urls.enumerated()), id: \.offset) { _, url in
                            ShareLinkPreview(rawURL: url, title: model.payload.title)
                        }
                        ForEach(Array(model.payload.texts.enumerated()), id: \.offset) { _, text in
                            Text(text).font(.subheadline).lineLimit(3)
                        }
                    }.padding().frame(maxWidth: .infinity, alignment: .leading)
                        .background(ShareStyle.card, in: RoundedRectangle(cornerRadius: ShareStyle.radius))
                    TextField("Add a note for Lucina", text: $model.note, axis: .vertical)
                        .lineLimit(2...4).padding().background(ShareStyle.card, in: RoundedRectangle(cornerRadius: ShareStyle.radius))
                        .onChange(of: model.note) { _, value in if value.count > 4000 { model.note = String(value.prefix(4000)) } }
                    if model.keepOpenWarning {
                        Label("This is large: keep this sheet open until it's sent.", systemImage: "exclamationmark.triangle")
                            .font(.footnote).foregroundStyle(.secondary)
                    }
                    if let progress = model.progress, !model.sent {
                        ProgressView(value: progress) { Text("Sending…").font(.footnote) }
                    }
                    Button { model.send("ask") } label: {
                        Text(LocalizedStringKey(model.sent ? "Sent ✓" : "Send to Lucina")).font(.headline).frame(maxWidth: .infinity).padding()
                    }.buttonStyle(.borderedProminent).tint(ShareStyle.button).disabled(!model.hasContent)
                    LazyVGrid(columns: [GridItem(.flexible()), GridItem(.flexible())]) {
                        ForEach(actions, id: \.1) { title, intent in
                            Button { model.send(intent) } label: { Text(LocalizedStringKey(title)).font(.caption).padding(8).frame(maxWidth: .infinity) }
                                .buttonStyle(.bordered).clipShape(Capsule())
                        }
                    }
                } else if model.error == nil { ProgressView() }
            }.padding(22).disabled(model.sending)
        }.background(ShareStyle.page.ignoresSafeArea()).foregroundStyle(ShareStyle.text).tint(ShareStyle.accent)
            .preferredColorScheme(.dark)
    }
}

private struct ShareLinkPreview: View {
    let rawURL: String
    let title: String
    @State private var fetchedTitle: String?
    @State private var provider: LPMetadataProvider?
    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: "link").frame(width: 56)
            VStack(alignment: .leading, spacing: 4) {
                Text(fetchedTitle ?? (title.isEmpty ? URL(string: rawURL)?.host ?? rawURL : title)).lineLimit(2)
                Text(URL(string: rawURL)?.host ?? rawURL).font(.caption).lineLimit(1)
            }
        }.task {
            guard let url = ShareItemReader.webURL(rawURL) else { return }
            let metadataProvider = LPMetadataProvider()
            provider = metadataProvider
            metadataProvider.timeout = 3
            metadataProvider.startFetchingMetadata(for: url) { metadata, _ in
                Task { @MainActor in fetchedTitle = metadata?.title }
            }
        }.onDisappear { provider?.cancel() }
    }
}
