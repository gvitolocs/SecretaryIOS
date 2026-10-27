// DEBUG, or Release with the SCREENSHOTS condition: Debug apps do not start on iOS 27 from Xcode 26.6.
#if DEBUG || SCREENSHOTS
import Foundation
import OSLog
import AVFoundation
import Photos
import PhotosUI
import SwiftUI
import UIKit
import UniformTypeIdentifiers

/// Debug builds only: an on-device self-test of "Share to Lucina" (XCUITest can't drive the iPhone).
/// `-SecretaryShareSelfTest all` (or a comma list of `photo,pdf,url,text,album,paste,intent,voice,voice-whisper,reply,readaloud,
/// shareitems,selectshare`) feeds real
/// items through the code the share extension, the chat composer and the Shortcuts intent run, and logs
/// every step to `Documents/share-selftest.txt` (and os_log `tech.getsecretary.app` / `ShareSelfTest`).
@MainActor
enum ShareSelfTest {
    static let allCases = ["photo", "pdf", "url", "text", "album", "paste", "intent", "voice", "voice-whisper",
                           "reply", "readaloud", "shareitems", "selectshare"]
    private static var runner: ShareSelfTestRunner?

    static func runIfRequested(app: AppModel, defaults: UserDefaults = .standard) {
        guard runner == nil, let raw = defaults.string(forKey: "SecretaryShareSelfTest")?.trimmingCharacters(in: .whitespaces),
              !raw.isEmpty else { return }
        let wanted = raw.lowercased() == "all" ? allCases
            : raw.split(separator: ",").map { $0.trimmingCharacters(in: .whitespaces).lowercased() }.filter { !$0.isEmpty }
        let runner = ShareSelfTestRunner(app: app, cases: wanted)
        self.runner = runner
        Task { @MainActor in await runner.run() }
    }
}

/// The log: one timestamped line per step, appended (several runs stay in one file).
enum ShareSelfTestLog {
    private static let logger = Logger(subsystem: "tech.getsecretary.app", category: "ShareSelfTest")
    private static let lock = NSLock()
    private static let stamp: ISO8601DateFormatter = {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter
    }()

    static var url: URL {
        let documents = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask).first
            ?? URL(fileURLWithPath: NSTemporaryDirectory())
        return documents.appendingPathComponent("share-selftest.txt")
    }

    static func write(_ line: String) {
        logger.log("\(line, privacy: .public)")
        lock.lock()
        defer { lock.unlock() }
        let text = stamp.string(from: Date()) + " " + line + "\n"
        let url = url
        if !FileManager.default.fileExists(atPath: url.path) {
            FileManager.default.createFile(atPath: url.path, contents: nil)
        }
        guard let handle = try? FileHandle(forWritingTo: url) else { return }
        defer { try? handle.close() }
        _ = try? handle.seekToEnd()
        try? handle.write(contentsOf: Data(text.utf8))
    }
}

/// Sits between ShareUploads and the chat while the test runs: records every upload outcome (with the
/// 202's JSON) and passes everything on, so the chat bubbles behave exactly as without it.
@MainActor
final class ShareSelfTestObserver: ShareUploadObserver {
    weak var forward: ShareUploadObserver?
    private(set) var results: [String: ShareUploadResult] = [:]
    private(set) var order: [String] = []
    private var quartiles: [String: Int] = [:]

    func shareUploadProgressed(_ shareId: String, fraction: Double) {
        logProgress(shareId, fraction: fraction, path: "background session")
        forward?.shareUploadProgressed(shareId, fraction: fraction)
    }

    func shareUploadFinished(_ result: ShareUploadResult) {
        results[result.shareId] = result
        order.append(result.shareId)
        ShareSelfTestLog.write("upload callback: share \(result.shareId) status \(result.status.map(String.init) ?? "none") decision \(result.decision)")
        forward?.shareUploadFinished(result)
    }

    /// Logs 25/50/75/100 % once per share.
    func logProgress(_ shareId: String, fraction: Double, path: String) {
        let step = min(4, Int(fraction * 4))
        guard step > (quartiles[shareId] ?? 0) else { return }
        quartiles[shareId] = step
        ShareSelfTestLog.write("upload progress (\(path)): share \(shareId) \(step * 25)%")
    }
}

@MainActor
final class ShareSelfTestRunner {
    struct Outcome {
        var pass: Bool
        var reason: String
        var reply: String?
    }

    private let app: AppModel
    private let cases: [String]
    private let observer = ShareSelfTestObserver()
    private let uploadTimeout: TimeInterval = 5 * 60
    private let pollTimeout: TimeInterval = 5 * 60

    init(app: AppModel, cases: [String]) {
        self.app = app
        self.cases = cases
    }

    private func log(_ line: String) { ShareSelfTestLog.write(line) }

    // MARK: - Run

    func run() async {
        log("=== share self-test: cases \(cases.joined(separator: ",")) (log \(ShareSelfTestLog.url.path))")
        let known = cases.filter { ShareSelfTest.allCases.contains($0) }
        for unknown in cases where !ShareSelfTest.allCases.contains(unknown) {
            log("unknown case '\(unknown)': skipped (known: \(ShareSelfTest.allCases.joined(separator: ",")))")
        }
        guard await waitForSignIn() else {
            log("SUMMARY 0/\(known.count)")
            return
        }
        observer.forward = ShareUploads.shared.observer
        ShareUploads.shared.observer = observer
        defer { ShareUploads.shared.observer = observer.forward }
        let groupNote = ShareRequestBuilder.hasAppGroup ? "provisioned: extension path enqueues on a background session"
            : "not provisioned: extension path uploads with ShareUploads.sendNow"
        log("App Group \(groupNote); endpoints \(BridgeCredentials.read()?.endpoints.joined(separator: ", ") ?? "?")")
        LucinaDonations.donate { error in
            ShareSelfTestLog.write(error.map { "donation (INSendMessageIntent Lucina): error \($0.localizedDescription)" }
                                   ?? "donation (INSendMessageIntent Lucina): success")
        }

        var passed = 0
        for name in known {
            log("--- case \(name)")
            let outcome: Outcome
            do {
                outcome = try await runCase(name)
            } catch {
                outcome = Outcome(pass: false, reason: "error: \(error.localizedDescription) (\(error))")
            }
            if outcome.pass { passed += 1 }
            let reply = outcome.reply.map { " reply: " + String($0.prefix(300)).replacingOccurrences(of: "\n", with: " ") } ?? ""
            log("RESULT \(name) \(outcome.pass ? "PASS" : "FAIL") \(outcome.reason)\(reply)")
        }
        log("SUMMARY \(passed)/\(known.count)")
        app.showSettings = false
        app.selectedTab = .chat
    }

    private func runCase(_ name: String) async throws -> Outcome {
        switch name {
        case "photo": return try await casePhoto()
        case "pdf": return try await casePDF()
        case "url": return try await caseURL()
        case "text": return try await caseText()
        case "album": return try await caseAlbum()
        case "paste": return try await casePaste()
        case "intent": return try await caseIntent()
        case "voice": return try await caseVoice(whisper: false)
        case "voice-whisper": return try await caseVoice(whisper: true)
        case "reply": return try await caseReply()
        case "readaloud": return await caseReadAloud()
        case "shareitems": return try await caseShareItems()
        case "selectshare": return try await caseSelectShare()
        default: return Outcome(pass: false, reason: "unknown case")
        }
    }

    /// Up to 30 s for the app to be signed in and to have written the bridge credentials the extension reads.
    private func waitForSignIn() async -> Bool {
        let deadline = Date().addingTimeInterval(30)
        while Date() < deadline {
            if app.phase == .signedIn, app.client.accessKey?.isEmpty == false, BridgeCredentials.read() != nil {
                log("signed in: token present, bridge credentials present")
                return true
            }
            try? await Task.sleep(for: .milliseconds(500))
        }
        log("not signed in after 30 s: phase \(app.phase), token \(app.client.accessKey?.isEmpty == false ? "present" : "absent"), "
            + "bridge credentials \(BridgeCredentials.read() == nil ? "absent" : "present"); stopping")
        return false
    }

    // MARK: - Extension path (ShareViewController.load + send, minus the UI)

    private func extensionShare(items: [NSExtensionItem], intent: String, note: String,
                                inspect: ((SharePayload) -> Void)? = nil) async throws -> Outcome {
        guard let credentials = BridgeCredentials.read() else { throw ShareFailure.signIn }
        // ShareSheetModel.load: a staging folder inside the share directory, then ShareItemReader.read.
        let staging = try ShareRequestBuilder.directory().appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: staging, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: staging) }
        var payload = try await ShareItemReader.read(items, directory: staging)
        var attachments: [PendingAttachment] = []
        for file in payload.files { attachments.append(await PendingAttachment.make(from: file)) }
        logRead(payload: payload, attachments: attachments)
        inspect?(payload)
        let size = attachments.reduce(Int64(0)) { $0 + $1.size }
        guard size <= ShareRequestBuilder.maxBytes else { throw ShareFailure.tooLarge }
        guard !attachments.isEmpty || !payload.urls.isEmpty || !payload.texts.isEmpty else { throw ShareFailure.empty }

        // ShareSheetModel.send.
        payload.files = attachments.map(\.shareFile)
        let meta = PendingAttachment.meta(for: attachments)
        let prepared = payload
        let shareId = UUID().uuidString
        log("build: share \(shareId) intent \(intent) note \"\(note)\" source ios-selftest-share meta \((try? meta.json()) ?? "-")")
        let record = try await Task.detached {
            try ShareRequestBuilder.build(payload: prepared, intent: intent, note: note, source: "ios-selftest-share",
                                          credentials: credentials, directory: ShareRequestBuilder.directory(),
                                          shareId: shareId, meta: meta)
        }.value
        let bodySize = (try? ShareRequestBuilder.directory().appendingPathComponent(shareId + ".body")
            .resourceValues(forKeys: [.fileSizeKey]).fileSize) ?? 0
        log("build: body \(bodySize) bytes, endpoints \(record.endpoints.joined(separator: ", "))")
        if ShareRequestBuilder.hasAppGroup {
            try ShareUploads.shared.enqueue(record, credentials: credentials)
            log("upload: ShareUploads.enqueue (background session)")
        } else {
            log("upload: ShareUploads.sendNow (foreground, as the extension without an App Group)")
            let observer = observer
            try await ShareUploads.shared.sendNow(record, credentials: credentials) { fraction in
                Task { @MainActor in observer.logProgress(shareId, fraction: fraction, path: "sendNow") }
            }
            log("upload: sendNow returned")
        }
        LucinaDonations.donate()
        guard let result = await awaitUpload(shareId) else {
            return Outcome(pass: false, reason: "no upload outcome within \(Int(uploadTimeout)) s")
        }
        guard log202(result) else { return Outcome(pass: false, reason: "HTTP \(result.status.map(String.init) ?? "none"), not 202") }
        return await finish(shareId, bubble: false)
    }

    private func logRead(payload: SharePayload, attachments: [PendingAttachment]) {
        log("read: title \"\(payload.title)\", \(payload.texts.count) text, \(payload.urls.count) url, \(attachments.count) file")
        for text in payload.texts { log("read: text \"\(text.prefix(200))\"") }
        for url in payload.urls { log("read: url \(url)") }
        for (index, attachment) in attachments.enumerated() { log("read: " + describe(attachment, index: index)) }
    }

    private func describe(_ attachment: PendingAttachment, index: Int) -> String {
        var line = "part \(index) kind \(attachment.kind.rawValue) name \"\(attachment.name)\" mime \(attachment.mime) bytes \(attachment.size)"
        if let width = attachment.width, let height = attachment.height { line += " \(width)x\(height)" }
        if let takenAt = attachment.takenAt { line += " taken \(ISO8601DateFormatter().string(from: takenAt))" }
        return line
    }

    // MARK: - Composer path (ChatComposer.send, minus the view)

    private func waitForTray(_ model: ComposerModel) async {
        let deadline = Date().addingTimeInterval(120)
        try? await Task.sleep(for: .milliseconds(100))
        while model.loading > 0, Date() < deadline { try? await Task.sleep(for: .milliseconds(200)) }
        if model.loading > 0 { log("tray: still loading after 120 s") }
    }

    private func composerShare(_ model: ComposerModel, caption: String) async -> Outcome {
        await waitForTray(model)
        if let error = model.error { log("tray: error \"\(error)\"") }
        log("tray: \(model.attachments.count) attachment(s)")
        for (index, attachment) in model.attachments.enumerated() { log("tray: " + describe(attachment, index: index)) }
        guard !model.attachments.isEmpty else { return Outcome(pass: false, reason: "the tray is empty") }

        let chat = app.chat
        let before = Set(chat.outgoing.keys)
        model.draft = caption
        let text = model.draft
        model.draft = ""
        let taken = await model.takeAll()
        guard chat.sendShare(caption: text, attachments: taken.attachments, location: taken.location) else {
            model.restore(taken.attachments, location: taken.location)
            return Outcome(pass: false, reason: "ChatStore.sendShare returned false")
        }
        guard let shareId = Set(chat.outgoing.keys).subtracting(before).first else {
            return Outcome(pass: false, reason: "sendShare took it but no new outgoing share appeared")
        }
        log("send: ChatStore.sendShare share \(shareId) caption \"\(text)\" (source ios-chat, background session)")
        return await awaitChatUpload(shareId)
    }

    /// Follows a chat share's bubble until the upload's outcome, then polls its status (as the composer does).
    private func awaitChatUpload(_ shareId: String) async -> Outcome {
        let chat = app.chat
        let localId = TimelineItem.localPrefix + shareId
        var lastBubble = ""
        func bubbleState() -> String {
            if let item = chat.items.first(where: { $0.id == localId }) {
                let progress = chat.shareProgress[shareId].map { String(format: " progress %.0f%%", $0 * 100) } ?? ""
                return "local bubble \(item.localStatus?.rawValue ?? "nil")\(progress)\(item.localError.map { " error \"\($0)\"" } ?? "")"
            }
            if let item = chat.items.first(where: { !$0.isLocal && $0.meta?.shareId == shareId && $0.role == .user }) {
                return "server bubble \(item.id)"
            }
            return "no bubble"
        }
        let deadline = Date().addingTimeInterval(uploadTimeout)
        var result: ShareUploadResult?
        while Date() < deadline {
            let state = bubbleState()
            if state != lastBubble { log("bubble: \(state)"); lastBubble = state }
            if let found = observer.results[shareId] { result = found; break }
            if chat.items.first(where: { $0.id == localId })?.localStatus == .error, chat.shareProgress[shareId] == nil {
                return Outcome(pass: false, reason: "the bubble failed: \(chat.items.first { $0.id == localId }?.localError ?? "?")")
            }
            try? await Task.sleep(for: .milliseconds(300))
        }
        guard let result else { return Outcome(pass: false, reason: "no upload outcome within \(Int(uploadTimeout)) s") }
        guard log202(result) else { return Outcome(pass: false, reason: "HTTP \(result.status.map(String.init) ?? "none"), not 202") }
        try? await Task.sleep(for: .milliseconds(200))
        log("bubble after 202: \(bubbleState()), userItemId \(chat.outgoing[shareId]?.userItemId ?? "nil")")
        return await finish(shareId, bubble: true)
    }

    // MARK: - Upload outcome, status polling, verdict

    private func awaitUpload(_ shareId: String) async -> ShareUploadResult? {
        let deadline = Date().addingTimeInterval(uploadTimeout)
        while Date() < deadline {
            if let result = observer.results[shareId] { return result }
            try? await Task.sleep(for: .milliseconds(300))
        }
        return nil
    }

    /// Logs the answer to `POST /v1/share`; true for a 202.
    private func log202(_ result: ShareUploadResult) -> Bool {
        let body = result.body.map { String(decoding: $0.prefix(4000), as: UTF8.self) } ?? "-"
        log("POST /v1/share: HTTP \(result.status.map(String.init) ?? "none") decision \(result.decision) body \(body)")
        return result.status == 202
    }

    /// Polls `GET /v1/share/{id}` every 3 s (up to 5 min) and decides: PASS = stage `done` with a reply.
    private func finish(_ shareId: String, bubble: Bool) async -> Outcome {
        let deadline = Date().addingTimeInterval(pollTimeout)
        var last: ShareStatus?
        var lastLine = ""
        var lastError = ""
        var readingShown = false
        while Date() < deadline {
            do {
                let status: ShareStatus = try await app.client.get("/v1/share/" + MediaAPI.encode(shareId))
                last = status
                let parts = status.attachments.map { "\($0.index.map(String.init) ?? "?"):\($0.kind ?? "?"):\($0.state ?? "?")" }
                let line = "stage \(status.stage)" + (parts.isEmpty ? "" : " attachments [\(parts.joined(separator: " "))]")
                if line != lastLine { log("GET /v1/share/\(shareId): \(line)"); lastLine = line }
                if bubble, !readingShown, let item = app.chat.items.first(where: { $0.meta?.shareId == shareId && $0.role == .user }),
                   let reading = app.chat.readingStatus(for: item) {
                    readingShown = true
                    log("bubble: \"Lucina is reading…\" shown (stage \(reading.stage))")
                }
                if status.isFinished { break }
            } catch {
                let text = APIError.wrap(error).localizedDescription
                if text != lastError { log("GET /v1/share/\(shareId): error \(text)"); lastError = text }
            }
            try? await Task.sleep(for: .seconds(3))
        }
        guard let status = last else { return Outcome(pass: false, reason: "no status from GET /v1/share (\(lastError))") }
        guard status.isFinished else { return Outcome(pass: false, reason: "still \(status.stage) after \(Int(pollTimeout)) s") }
        let reply = await reply(for: status)
        guard status.stage == "done" else { return Outcome(pass: false, reason: "stage \(status.stage)", reply: reply) }
        guard let reply, !reply.isEmpty else { return Outcome(pass: false, reason: "stage done without a reply") }
        return Outcome(pass: true, reason: "202, stage done", reply: reply)
    }

    /// The status's `reply`, or the text of Lucina's item in the timeline when the status only names it.
    private func reply(for status: ShareStatus) async -> String? {
        if let reply = status.reply?.trimmingCharacters(in: .whitespacesAndNewlines), !reply.isEmpty { return reply }
        guard let id = status.lucinaItemId else { return nil }
        if !app.chat.items.contains(where: { $0.id == id }) { await app.chat.refresh() }
        guard let text = app.chat.items.first(where: { $0.id == id })?.text, !text.isEmpty else { return nil }
        log("reply: the status had none; taken from timeline item \(id)")
        return text
    }

    // MARK: - Cases: extension path

    private func casePhoto() async throws -> Outcome {
        guard let asset = try newestPhotos(1).first else { return Outcome(pass: false, reason: "no photo in the library") }
        let original = try await exportOriginal(asset)
        defer { try? FileManager.default.removeItem(at: original.url.deletingLastPathComponent()) }
        let heic = ["heic", "heif"].contains(original.url.pathExtension.lowercased())
        log("photo: asset \(asset.localIdentifier) created \(asset.creationDate.map { ISO8601DateFormatter().string(from: $0) } ?? "?"), "
            + "original \"\(original.url.lastPathComponent)\" uti \(original.uti) bytes \(original.bytes)")
        guard let provider = NSItemProvider(contentsOf: original.url) else {
            return Outcome(pass: false, reason: "NSItemProvider(contentsOf:) returned nil")
        }
        log("photo: provider types \(provider.registeredTypeIdentifiers)")
        let item = NSExtensionItem()
        item.attachments = [provider]
        return try await extensionShare(items: [item], intent: "ask", note: "[selftest] cosa vedi?") { payload in
            guard let file = payload.files.first else { return }
            if heic {
                let converted = file.mime == "image/jpeg" && file.url.pathExtension.lowercased() == "jpg"
                ShareSelfTestLog.write("photo: HEIC original; reader converted it to JPEG: \(converted ? "yes" : "NO") (\(file.name), \(file.mime))")
            } else {
                ShareSelfTestLog.write("photo: original is not HEIC (\(original.url.pathExtension)); sent as \(file.mime)")
            }
        }
    }

    private func casePDF() async throws -> Outcome {
        let url = try makeBillPDF()
        defer { try? FileManager.default.removeItem(at: url.deletingLastPathComponent()) }
        let bytes = (try? url.resourceValues(forKeys: [.fileSizeKey]).fileSize) ?? 0
        log("pdf: generated \"\(url.lastPathComponent)\" \(bytes) bytes (1 page, text layer)")
        let provider = NSItemProvider()
        provider.suggestedName = url.lastPathComponent
        provider.registerFileRepresentation(forTypeIdentifier: UTType.pdf.identifier, fileOptions: [], visibility: .all) { completion in
            completion(url, false, nil)
            return nil
        }
        let item = NSExtensionItem()
        item.attachments = [provider]
        return try await extensionShare(items: [item], intent: "ask", note: "[selftest] di cosa parla e quanto devo pagare?")
    }

    private func caseURL() async throws -> Outcome {
        guard let url = URL(string: "https://en.wikipedia.org/wiki/Little_Mermaid_(statue)") else {
            return Outcome(pass: false, reason: "bad URL")
        }
        let item = NSExtensionItem()
        item.attributedTitle = NSAttributedString(string: "The Little Mermaid (statue)")
        item.attachments = [NSItemProvider(object: url as NSURL)]
        return try await extensionShare(items: [item], intent: "ask", note: "")
    }

    private func caseText() async throws -> Outcome {
        let item = NSExtensionItem()
        item.attachments = [NSItemProvider(object: "Comprare: latte, uova, pane integrale, 2 limoni" as NSString)]
        return try await extensionShare(items: [item], intent: "shopping", note: "[selftest]")
    }

    // MARK: - Cases: composer path

    private func caseAlbum() async throws -> Outcome {
        let assets = try newestPhotos(3)
        guard assets.count == 3 else { return Outcome(pass: false, reason: "the library has \(assets.count) photo(s), need 3") }
        log("album: assets \(assets.map(\.localIdentifier).joined(separator: ", "))")
        var model = ComposerModel()
        model.add(pickerItems: assets.map { PhotosPickerItem(itemIdentifier: $0.localIdentifier) })
        await waitForTray(model)
        if model.attachments.count == assets.count {
            log("album: loader AttachmentLoader.load(pickerItems:) with PhotosPickerItem(itemIdentifier:) (ComposerModel.add(pickerItems:))")
        } else {
            log("album: the PhotosPicker loader gave \(model.attachments.count)/\(assets.count) (error \(model.error ?? "none")); "
                + "falling back to AttachmentLoader.load(fileURLs:) (ComposerModel.add(fileURLs:)) with the exported originals")
            for attachment in model.attachments { try? FileManager.default.removeItem(at: attachment.url) }
            var exported: [URL] = []
            for asset in assets { exported.append(try await exportOriginal(asset).url) }
            defer { for url in exported { try? FileManager.default.removeItem(at: url.deletingLastPathComponent()) } }
            model = ComposerModel()
            model.add(fileURLs: exported)
            await waitForTray(model)
        }
        return await composerShare(model, caption: "[selftest] tre foto: cosa hanno in comune?")
    }

    private func casePaste() async throws -> Outcome {
        guard let png = makeErrorImage().pngData() else { return Outcome(pass: false, reason: "couldn't render the image") }
        let board = UIPasteboard.general
        // Local only (no Universal Clipboard) and gone in 10 minutes even if the cleanup below doesn't run.
        board.setItems([[UTType.png.identifier: png]], options: [.localOnly: true, .expirationDate: Date().addingTimeInterval(600)])
        let changeCount = board.changeCount
        log("paste: put a 600x300 PNG (\(png.count) bytes) on UIPasteboard.general")
        defer {
            if UIPasteboard.general.changeCount == changeCount {
                UIPasteboard.general.items = []
                ShareSelfTestLog.write("paste: test image removed from the pasteboard (the previous contents are not restored: "
                                       + "reading them would show iOS's paste permission prompt)")
            }
        }
        let model = ComposerModel()
        let view = PastingTextView()
        var routed = false
        // ComposerTextView wires `onPaste: { model.add(providers: $0) }`.
        view.onPaste = { providers in
            routed = true
            ShareSelfTestLog.write("paste: PastingTextView.paste(_:) handed \(providers.count) provider(s) to the tray: "
                                   + providers.map { $0.registeredTypeIdentifiers.joined(separator: "/") }.joined(separator: ", "))
            model.add(providers: providers)
        }
        let canPaste = view.canPerformAction(#selector(UIResponderStandardEditActions.paste(_:)), withSender: nil)
        log("paste: canPerformAction(paste:) \(canPaste); shouldAttach \(AttachmentLoader.shouldAttach(board.itemProviders))")
        view.paste(nil)
        guard routed else {
            return Outcome(pass: false, reason: "paste(_:) pasted as text instead of attaching (text view: \"\(view.text ?? "")\")")
        }
        return await composerShare(model, caption: "[selftest] cosa significa questo errore?")
    }

    // MARK: - Case: voice note (the mic button's send path)

    /// A spoken Italian clip made on the iPhone, sent through `ChatStore.sendVoiceNote` exactly as the mic
    /// button sends a recording (waveform from the file, on-device transcript unless `whisper`).
    private func caseVoice(whisper: Bool) async throws -> Outcome {
        let phrase = "Ciao Lucina, ricordami di comprare il latte domani mattina"
        let clip = try await synthesizeClip(phrase)
        log("voice: synthesized \"\(phrase)\" with AVSpeechSynthesizer.write (voice \(clip.voice)) into "
            + "\"\(clip.url.lastPathComponent)\" ADTS AAC, \(String(format: "%.2f", clip.duration)) s, \(clip.bytes) bytes")
        let chat = app.chat
        let before = Set(chat.outgoing.keys)
        let note = VoiceNote(url: clip.url, duration: clip.duration, levels: [], recordedAt: Date())
        log("send: ChatStore.sendVoiceNote (the mic button's path) transcribe \(!whisper)")
        guard await chat.sendVoiceNote(note, transcribe: !whisper) else {
            try? FileManager.default.removeItem(at: clip.url)
            return Outcome(pass: false, reason: "ChatStore.sendVoiceNote returned false")
        }
        guard let shareId = Set(chat.outgoing.keys).subtracting(before).first, let share = chat.outgoing[shareId] else {
            return Outcome(pass: false, reason: "sendVoiceNote took it but no new outgoing share appeared")
        }
        let file = share.files.first
        log("send: share \(shareId) part \"\(file?.name ?? "?")\" \(file?.mime ?? "?") \(file?.size ?? 0) bytes, "
            + "meta \((try? share.meta.json()) ?? "-")")
        if whisper {
            log("voice: on-device transcript left out on purpose: the server's Whisper transcribes it")
        } else {
            log("voice: on-device transcript \(file?.transcript.map { "\"\($0)\"" } ?? "none (speech recognition not authorized, unavailable on device, or timed out)")")
        }
        var outcome = await awaitChatUpload(shareId)
        // What the bubble shows under the note, and what Lucina's reply carries.
        let user = chat.items.first { $0.meta?.shareId == shareId && $0.role == .user }
        let userItemId = chat.outgoing[shareId]?.userItemId ?? user.flatMap { $0.isLocal ? nil : $0.id }
        if let user, let voice = BubbleEntry.entries(user.attachments).first(where: { $0.attachment.isVoice }) {
            let seen = chat.voiceTranscript(for: user, attachment: voice.attachment, fileIndex: voice.fileIndex)
            log("voice: bubble kind voice, waveform \(voice.attachment.waveform.map { "\(Data(base64Encoded: $0)?.count ?? 0) bytes" } ?? "none"), "
                + "transcript shown \(seen.map { "\"\($0)\"" } ?? "none")")
        } else {
            log("voice: no voice bubble found for share \(shareId) (user item \(user?.id ?? "none"))")
        }
        if let userItemId, let reply = chat.items.first(where: { $0.role == .lucina && $0.replyTo == userItemId }) {
            log("voice: reply \(reply.id) meta.transcript \(reply.meta?.transcript.map { "\"\($0)\"" } ?? "none"), "
                + "meta.voiceReply \(reply.meta?.voiceReply.map(String.init) ?? "absent"), "
                + "spoken by the app \(chat.voiceReplies.playedIds.contains(reply.id) ? "yes" : "no") "
                + "(setting \(LucinaVoiceReplies.isEnabled() ? "on" : "off"))")
        } else {
            log("voice: Lucina's reply item for \(userItemId ?? "?") is not in the timeline")
        }
        if outcome.pass { outcome.reason += whisper ? " (Whisper path)" : " (on-device transcript path)" }
        return outcome
    }

    private struct SpokenClip {
        var url: URL
        var duration: TimeInterval
        var bytes: Int
        var voice: String
    }

    /// Writes the synthesizer's PCM buffers straight into an ADTS AAC file in Caches/ChatNotes (where the
    /// mic button's recordings go), so the clip is the same kind of file a recording is.
    private func synthesizeClip(_ text: String) async throws -> SpokenClip {
        let utterance = AVSpeechUtterance(string: text)
        let voice = AVSpeechSynthesisVoice(language: "it-IT")
        utterance.voice = voice
        let url = VoiceNoteFiles.directory.appendingPathComponent("selftest-\(UUID().uuidString.lowercased()).aac")
        let writer = SpokenClipWriter(url: url)
        let synthesizer = AVSpeechSynthesizer()
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            writer.continuation = continuation
            synthesizer.write(utterance) { buffer in writer.append(buffer) }
            Task {
                // Some iOS versions never send the empty end-of-audio buffer: keep what was written.
                try? await Task.sleep(for: .seconds(30))
                writer.end()
            }
        }
        withExtendedLifetime(synthesizer) {}
        let file = try AVAudioFile(forReading: url)
        let duration = Double(file.length) / file.fileFormat.sampleRate
        let bytes = (try? url.resourceValues(forKeys: [.fileSizeKey]).fileSize) ?? 0
        guard duration > 0.5, bytes > 0 else { throw SelfTestError("the synthesized clip is empty (\(bytes) bytes)") }
        return SpokenClip(url: url, duration: duration, bytes: bytes, voice: voice.map { "\($0.name) \($0.language)" } ?? "default")
    }

    /// Collects the synthesizer's buffers (on its own queue) into the AAC file; resumes once.
    private final class SpokenClipWriter: @unchecked Sendable {
        private let lock = NSLock()
        private let url: URL
        private var file: AVAudioFile?
        var continuation: CheckedContinuation<Void, Error>?

        init(url: URL) { self.url = url }

        func append(_ buffer: AVAudioBuffer) {
            guard let pcm = buffer as? AVAudioPCMBuffer else { return }
            if pcm.frameLength == 0 {
                end()
                return
            }
            lock.lock()
            var failure: Error?
            do {
                if file == nil {
                    let settings: [String: Any] = [AVFormatIDKey: kAudioFormatMPEG4AAC,
                                                   AVSampleRateKey: pcm.format.sampleRate,
                                                   AVNumberOfChannelsKey: pcm.format.channelCount,
                                                   AVEncoderBitRateKey: 64_000]
                    file = try AVAudioFile(forWriting: url, settings: settings, commonFormat: pcm.format.commonFormat,
                                           interleaved: pcm.format.isInterleaved)
                }
                try file?.write(from: pcm)
            } catch {
                file = nil
                failure = error
            }
            lock.unlock()
            if let failure { finish(failure) }
        }

        /// Closes the file and resumes: success when audio was written.
        func end() {
            lock.lock()
            let written = file != nil
            file = nil // closes it
            lock.unlock()
            finish(written ? nil : SelfTestError("the synthesizer produced no audio"))
        }

        func finish(_ error: Error?) {
            lock.lock()
            let pending = continuation
            continuation = nil
            lock.unlock()
            guard let pending else { return }
            if let error { pending.resume(throwing: error) } else { pending.resume() }
        }
    }

    // MARK: - Cases: message actions (reply, read aloud, share items)

    /// A text replying to Lucina's newest message through the swipe's path (`beginReply` → the composer's
    /// `takeReply` → `ask(_:replyTo:)`). PASS: her answer arrives and the server's copy of the user item has
    /// `replyTo` set to that message.
    private func caseReply() async throws -> Outcome {
        let chat = app.chat
        await chat.refresh()
        guard let target = chat.items.last(where: { $0.role == .lucina && !$0.isLocal }) else {
            return Outcome(pass: false, reason: "no Lucina message with a server id in the timeline")
        }
        log("reply: target \(target.id) \"\(ChatQuote.summary(target).prefix(120))\"")
        guard chat.beginReply(target) else { return Outcome(pass: false, reason: "beginReply refused the target") }
        log("reply: quote bar \(chat.replyingTo?.id ?? "nil") sender \(ChatQuote.sender(target))")
        let text = "[selftest] rispondo a questo messaggio: di cosa parlava, in una frase?"
        let reply = chat.takeReply()
        var sent: ChatRequest?
        chat.onChatRequest = { sent = $0 }
        defer { chat.onChatRequest = nil }
        let answer = await chat.ask(text, replyTo: reply?.id)
        if let sent, let body = try? APIClient.encoder.encode(sent) {
            log("reply: POST /v1/chat replyTo \(sent.replyTo ?? "nil") body \(String(decoding: body, as: UTF8.self))")
        } else {
            log("reply: no POST /v1/chat was made")
        }
        guard let answer, !answer.isEmpty else {
            return Outcome(pass: false, reason: answer == nil ? "not sent" : "the send failed (no answer)")
        }
        let cleaned = ChatStore.clean(text)
        guard let user = chat.items.last(where: { $0.role == .user && !$0.isLocal && $0.text == cleaned }) else {
            return Outcome(pass: false, reason: "the user item has no server id", reply: answer)
        }
        let page = try await app.api.timeline(limit: 30)
        let server = page.items.first { $0.id == user.id }
        log("reply: server user item \(server?.id ?? "missing from the newest page") replyTo \(server?.replyTo ?? "nil")")
        guard server?.replyTo == target.id else {
            return Outcome(pass: false, reason: "the server's user item has replyTo \(server?.replyTo ?? "nil"), want \(target.id)", reply: answer)
        }
        return Outcome(pass: true, reason: "answer arrived, server user item replyTo \(target.id)", reply: answer)
    }

    /// The right swipe's action on Lucina's newest message, twice (a TTS fetch, then the cache), muted.
    /// PASS: the first run played more than 1 s and the second came from the cache and played too.
    private func caseReadAloud() async -> Outcome {
        let chat = app.chat
        await chat.refresh()
        guard let target = chat.items.last(where: { $0.role == .lucina && !$0.isLocal && chat.speechText(for: $0) != nil }) else {
            return Outcome(pass: false, reason: "no Lucina message with words in the timeline")
        }
        let reader = chat.readAloud
        reader.muted = true
        defer {
            reader.stop()
            reader.muted = false
        }
        reader.cache.remove(target.id)
        log("readaloud: target \(target.id) speech \"\(chat.speechText(for: target)?.prefix(120) ?? "")\" (muted: volume 0; "
            + "cache file \(TTSCache.fileName(for: target.id)))")
        var played: [Bool] = []
        var hits: [Bool] = []
        for run in 1...2 {
            let outcome = await chat.toggleReadAloud(target)
            let report = reader.lastReport
            log("readaloud run \(run): outcome \(outcome) TTS HTTP \(report?.status.map(String.init) ?? "-") bytes \(report?.bytes ?? 0) "
                + "cache hit \(report?.cacheHit == true ? "yes" : "no") session \(report?.session ?? "-")")
            guard outcome == .started else { return Outcome(pass: false, reason: "run \(run): \(outcome)") }
            try? await Task.sleep(for: .milliseconds(1400))
            let time = reader.currentTime
            log("readaloud run \(run): isPlaying \(reader.isPlaying) currentTime \(String(format: "%.2f", time)) s of "
                + "\(String(format: "%.2f", reader.duration)) s, indicator on \(reader.playingId ?? "none")")
            played.append(time > 1)
            hits.append(report?.cacheHit == true)
            reader.stop()
            try? await Task.sleep(for: .milliseconds(300))
        }
        log("readaloud: session after stop \(ChatAudio.sessionDescription)")
        guard played.allSatisfy({ $0 }) else { return Outcome(pass: false, reason: "audio did not play past 1 s (\(played))") }
        guard hits == [false, true] else { return Outcome(pass: false, reason: "cache hits \(hits), want [false, true]") }
        return Outcome(pass: true, reason: "played > 1 s twice; TTS then cache hit")
    }

    /// The share items of the newest message with a picture and a caption (the `album` case sends one), built
    /// as the Share… action builds them, without presenting the sheet. PASS: an image item and a text item.
    private func caseShareItems() async throws -> Outcome {
        let chat = app.chat
        await chat.refresh()
        let candidates = chat.items.filter { !chat.isHidden($0.id) && $0.attachments.contains(where: \.isImage) }
        guard let target = candidates.last(where: { !$0.displayText.isEmpty }) ?? candidates.last else {
            return Outcome(pass: false, reason: "no message with a picture in the timeline")
        }
        log("shareitems: message \(target.id) \(target.attachments.count) attachment(s) caption \"\(target.displayText.prefix(80))\"")
        let items = try await logShareItems(ChatShareBuilder.parts(for: target, transcript: chat.voiceTranscript(of: target)), label: "shareitems")
        let image = items.contains(where: \.isImage)
        let text = items.contains(where: \.isText)
        guard image, text else { return Outcome(pass: false, reason: "image item \(image), text item \(text)") }
        return Outcome(pass: true, reason: "\(items.count) items: image and text present")
    }

    /// "Share conversation" for the last three visible messages: the transcript and their files.
    private func caseSelectShare() async throws -> Outcome {
        let chat = app.chat
        await chat.refresh()
        let chosen = Array(chat.items.filter { !$0.isLocal && !chat.isHidden($0.id) }.suffix(3))
        guard chosen.count == 3 else { return Outcome(pass: false, reason: "\(chosen.count) message(s) in the timeline, need 3") }
        let userName = ChatShareBuilder.ownerName(deviceName: app.deviceName)
        log("selectshare: messages \(chosen.map(\.id).joined(separator: ", ")), owner name \"\(userName)\"")
        let parts = ChatShareBuilder.conversationParts(chosen, userName: userName) { chat.voiceTranscript(of: $0) }
        let items = try await logShareItems(parts, label: "selectshare")
        guard case let .text(transcript)? = items.first?.payload else { return Outcome(pass: false, reason: "no transcript item first") }
        log("selectshare: transcript \"\(transcript.prefix(600).replacingOccurrences(of: "\n", with: "⏎"))\"")
        let headers = transcript.components(separatedBy: "\n").filter { $0.range(of: #" — \d"#, options: .regularExpression) != nil }.count
        guard headers >= 3 else { return Outcome(pass: false, reason: "the transcript has \(headers) speaker lines, want 3") }
        return Outcome(pass: true, reason: "transcript with \(headers) speaker lines + \(items.count - 1) more item(s)")
    }

    /// Resolves the parts like the share sheet (downloads included) and logs each item's type and size.
    private func logShareItems(_ parts: [ChatSharePart], label: String) async throws -> [ChatShareItem] {
        log("\(label): \(parts.count) part(s): " + parts.map { part in
            switch part {
            case .text: return "text"
            case let .link(url): return "link \(url.host ?? url.absoluteString)"
            case let .media(media): return "\(media.kind.rawValue) \(media.attachment.mediaId ?? "local")"
            }
        }.joined(separator: ", "))
        let started = Date()
        let items = try await ChatShareResolver.resolve(parts, store: app.chat, client: app.client)
        log("\(label): resolved in \(String(format: "%.2f", Date().timeIntervalSince(started))) s")
        for (index, item) in items.enumerated() {
            var line = "\(label): item \(index) UTType \(item.type.identifier) bytes \(item.bytes)"
            if let name = item.name { line += " name \"\(name)\"" }
            if let file = item.fileURL, let provider = NSItemProvider(contentsOf: file) {
                line += " provider types \(provider.registeredTypeIdentifiers)"
            }
            log(line)
        }
        return items
    }

    // MARK: - Case: Shortcuts intent

    private func caseIntent() async throws -> Outcome {
        let before = recordIds().union(observer.results.keys)
        let intent = SendToLucinaIntent()
        intent.text = "[selftest] promemoria: chiamare il dentista domani alle 10"
        intent.action = .remind
        log("intent: SendToLucinaIntent.perform() text \"\(intent.text ?? "")\" action \(intent.action.rawValue)")
        let result = try await intent.perform()
        log("intent: perform returned \(type(of: result)) (dialog \"Sent to Lucina\")")
        // The intent doesn't return its share id: it is the record it just wrote next to the body, or the
        // first upload outcome for a share this test didn't start.
        var shareId = recordIds().subtracting(before).first
        if shareId == nil {
            let deadline = Date().addingTimeInterval(10)
            while shareId == nil, Date() < deadline {
                shareId = observer.order.first { !before.contains($0) }
                if shareId == nil { try? await Task.sleep(for: .milliseconds(300)) }
            }
        }
        guard let shareId else {
            return Outcome(pass: true, reason: "perform succeeded; its share id never appeared (no record, no upload callback), not polled")
        }
        log("intent: share \(shareId) (ShareUploads.enqueue, background session)")
        guard let upload = await awaitUpload(shareId) else {
            return Outcome(pass: false, reason: "no upload outcome within \(Int(uploadTimeout)) s")
        }
        guard log202(upload) else { return Outcome(pass: false, reason: "HTTP \(upload.status.map(String.init) ?? "none"), not 202") }
        return await finish(shareId, bubble: false)
    }

    /// Share ids with a record (`<shareId>.json`) in the share directory: written by build, removed when the upload ends.
    private func recordIds() -> Set<String> {
        guard let directory = try? ShareRequestBuilder.directory(),
              let entries = try? FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: nil) else { return [] }
        return Set(entries.filter { $0.pathExtension == "json" && !$0.lastPathComponent.hasSuffix(".result.json") }
            .map { $0.deletingPathExtension().lastPathComponent })
    }

    // MARK: - Photos

    private struct SelfTestError: LocalizedError {
        let errorDescription: String?
        init(_ text: String) { errorDescription = text }
    }

    private func newestPhotos(_ count: Int) throws -> [PHAsset] {
        let status = PHPhotoLibrary.authorizationStatus(for: .readWrite)
        log("photos: authorization \(status == .authorized ? "full" : status == .limited ? "limited" : "not granted (\(status.rawValue))")")
        guard status == .authorized || status == .limited else { throw SelfTestError("Photos access is not granted") }
        let options = PHFetchOptions()
        options.sortDescriptors = [NSSortDescriptor(key: "creationDate", ascending: false)]
        options.fetchLimit = count
        let result = PHAsset.fetchAssets(with: .image, options: options)
        return result.count == 0 ? [] : result.objects(at: IndexSet(integersIn: 0..<result.count))
    }

    /// The asset's original file (HEIC stays HEIC), in its own temporary folder.
    private func exportOriginal(_ asset: PHAsset) async throws -> (url: URL, uti: String, bytes: Int) {
        let resources = PHAssetResource.assetResources(for: asset)
        guard let resource = resources.first(where: { $0.type == .photo }) ?? resources.first(where: { $0.type == .fullSizePhoto }) ?? resources.first else {
            throw SelfTestError("the asset has no resources")
        }
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent("share-selftest-" + UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        let name = resource.originalFilename.isEmpty ? "photo" : resource.originalFilename
        let url = folder.appendingPathComponent(name)
        let options = PHAssetResourceRequestOptions()
        options.isNetworkAccessAllowed = true
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            PHAssetResourceManager.default().writeData(for: resource, toFile: url, options: options) { error in
                if let error { continuation.resume(throwing: error) } else { continuation.resume() }
            }
        }
        let bytes = (try? url.resourceValues(forKeys: [.fileSizeKey]).fileSize) ?? 0
        return (url, resource.uniformTypeIdentifier, bytes)
    }

    // MARK: - Generated content

    /// A one-page Italian gas bill with a real text layer.
    private func makeBillPDF() throws -> URL {
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent("share-selftest-" + UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        let url = folder.appendingPathComponent("Fattura ENEL 0099-2026.pdf")
        let page = CGRect(x: 0, y: 0, width: 595, height: 842)
        let lines: [(String, UIFont)] = [
            ("ENEL ENERGIA S.p.A.", .boldSystemFont(ofSize: 16)),
            ("FATTURA n. 0099-2026 del 21/10/2026", .systemFont(ofSize: 14)),
            ("Fornitura gas", .systemFont(ofSize: 14)),
            ("Totale da pagare: EUR 57,40", .boldSystemFont(ofSize: 14)),
            ("Scadenza: 18/11/2026", .systemFont(ofSize: 14)),
            ("IBAN IT60X0542811101000000123456", .systemFont(ofSize: 14)),
        ]
        try UIGraphicsPDFRenderer(bounds: page).writePDF(to: url) { context in
            context.beginPage()
            var y: CGFloat = 72
            for (text, font) in lines {
                NSAttributedString(string: text, attributes: [.font: font, .foregroundColor: UIColor.black]).draw(at: CGPoint(x: 72, y: y))
                y += font.lineHeight + 12
            }
        }
        return url
    }

    /// 600x300, white, black text: what a screenshot of a browser error looks like.
    private func makeErrorImage() -> UIImage {
        let format = UIGraphicsImageRendererFormat()
        format.scale = 1
        let size = CGSize(width: 600, height: 300)
        return UIGraphicsImageRenderer(size: size, format: format).image { context in
            UIColor.white.setFill()
            context.fill(CGRect(origin: .zero, size: size))
            let style = NSMutableParagraphStyle()
            style.alignment = .center
            let text = NSAttributedString(string: "ERROR 404 — Pagina non trovata",
                                          attributes: [.font: UIFont.boldSystemFont(ofSize: 34), .foregroundColor: UIColor.black,
                                                       .paragraphStyle: style])
            let height = text.boundingRect(with: CGSize(width: 560, height: 300), options: .usesLineFragmentOrigin, context: nil).height
            text.draw(in: CGRect(x: 20, y: (size.height - height) / 2, width: 560, height: height))
        }
    }
}
#endif
